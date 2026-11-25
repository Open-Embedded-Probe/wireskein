//! The byte streams a probe is reached on (transports §1-§3): a TCP connection (a probe's TCP transport, the virtual
//! bench of oep-client-python) and the USB vendor bulk interface (class 0xFF, subclass 'O', protocol 'E').
//!
//! Each has a thread of its own taking what comes in, so a stream is taken while the caller is busy: the USB one keeps
//! many IN transfers queued in the kernel (oep-client-python's usb-async / UsbProcessStream: the probe ends a transfer
//! with a short packet, so a transfer holds one frame and the depth is how many frames the host can hold back), and
//! puts what came in on a queue the reader takes from. It does not wait for the reader, nor for what the reader hands
//! on (the spool), while the stream's backlog in bytes is within its budget (`flow`); past it the transfers are not
//! sent out again, the probe's own buffer fills and it drops whole frames, which the stream's positions show as a
//! loss (memory stays bounded whatever the host keeps up with).

use crate::flow::Flow;
use crate::msg::{Error, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The vendor bulk interface (transports §3).
pub const VENDOR_CLASS: u8 = 0xFF;
pub const VENDOR_SUBCLASS: u8 = 0x4F;
pub const VENDOR_PROTOCOL: u8 = 0x45;
/// An IN transfer's size: a frame's most (the probe ends each transfer with a short packet).
pub const URB_SIZE: usize = 32 * 1024;
/// IN transfers kept queued: 480 x 32 KiB is 15 MiB of usbfs_memory_mb's default 16 (a submit refused leaves the depth
/// at what it took).
pub const DEPTH: usize = 480;
/// The first shape's bound on the queue to the caller, in chunks (§37; a bench compares it, `Flow::counts`).
pub const BACKLOG: usize = 4096;

/// What the IN side did (meta.probe.link's reader statistics).
#[derive(Debug, Default)]
pub struct Stats {
    pub bytes: AtomicU64,
    pub transfers: AtomicU64,
    /// the fewest transfers queued when one came back (near 0: the stream outran the host)
    pub inflight_min: AtomicU64,
    /// the longest between two completions, in microseconds
    pub gap_max_us: AtomicU64,
    /// times a chunk was held back (the stream's backlog at its budget) until there was room
    pub held: AtomicU64,
}

/// What came in, in order, as the reader takes it (the bytes counted out of the stream's backlog).
pub struct Incoming {
    rx: Receiver<Vec<u8>>,
    flow: Arc<Flow>,
    /// whether what comes was counted in (a test's channel is not)
    counted: bool,
}

impl Incoming {
    fn took(&self, c: &[u8]) {
        if self.counted {
            self.flow.unqueue(c.len() as u64);
        }
    }
    pub fn recv_timeout(&self, d: Duration) -> std::result::Result<Vec<u8>, RecvTimeoutError> {
        let r = self.rx.recv_timeout(d);
        if let Ok(c) = &r {
            self.took(c);
        }
        r
    }
    pub fn try_recv(&self) -> std::result::Result<Vec<u8>, TryRecvError> {
        let r = self.rx.try_recv();
        if let Ok(c) = &r {
            self.took(c);
        }
        r
    }
}

/// A chunk to the reader: at once while the stream's backlog has room (whatever the reader and the spool are doing);
/// past its budget it waits there - false: stopped, or nobody reads.
fn deliver(tx: &Sender<Vec<u8>>, chunk: Vec<u8>, flow: &Flow, stats: &Stats, stop: &AtomicBool) -> bool {
    let n = chunk.len() as u64;
    if !flow.room_for(n) {
        stats.held.fetch_add(1, Ordering::Relaxed);
        let t = Instant::now();
        while !flow.room_for(n) {
            if stop.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Flow::longest(&flow.held_max_us, t);
    }
    flow.queue(n);
    if tx.send(chunk).is_err() {
        flow.unqueue(n);
        return false;
    }
    true
}

trait Sink: Send {
    fn send(&mut self, data: &[u8]) -> Result<()>;
}

/// One open stream: what came in (chunks, in order), how to write, and what reads it.
pub struct Transport {
    pub rx: Incoming,
    out: Box<dyn Sink>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    shutdown: Option<Box<dyn FnOnce() + Send>>,
    /// how it reads: "rust-usb-async" or "TcpStream" (meta.probe.link.reader)
    pub reader: &'static str,
    /// a stream that never loses a frame boundary (TCP)
    pub keeps_boundaries: bool,
    pub stats: Arc<Stats>,
    /// the stream's backlog, shared with what the reader hands on (the spool)
    pub flow: Arc<Flow>,
    /// the IN transfers queued (USB)
    pub depth: usize,
    /// a serial port: its path, and how its line speed is set (the host side of port_speed)
    pub port_path: Option<String>,
    set_baud: Option<Box<dyn FnMut(u32) -> Result<()> + Send>>,
}

impl Transport {
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.out.send(data)
    }

    /// A serial port's host side to `baud` (what was read so far is the caller's to drop).
    pub fn set_baud(&mut self, baud: u32) -> Result<()> {
        match self.set_baud.as_mut() {
            Some(f) => f(baud),
            None => Err(Error::Value("not a serial port".into())),
        }
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(f) = self.shutdown.take() {
            f();
        }
        // the reader may wait to hand a chunk over: empty the queue so it sees the stop
        while self.rx.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---------------- TCP ----------------

struct TcpSink(TcpStream);

impl Sink for TcpSink {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.0.write_all(data).map_err(Error::from)
    }
}

/// A TCP connection to HOST:PORT (length-prefixed frames, transports §1).
pub fn open_tcp(host: &str, port: u16) -> Result<Transport> {
    use std::net::ToSocketAddrs;
    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|e| Error::NotFound(format!("tcp://{host}:{port}: {e}")))?
        .next()
        .ok_or_else(|| Error::NotFound(format!("tcp://{host}:{port}: no address")))?;
    let s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))
        .map_err(|e| Error::Io(format!("tcp://{host}:{port}: {e}")))?;
    let _ = s.set_nodelay(true);
    let mut r = s.try_clone()?;
    let closer = s.try_clone()?;
    let (tx, rx) = channel::<Vec<u8>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let flow = Arc::new(Flow::default());
    let (st, stats2, flow2) = (stop.clone(), stats.clone(), flow.clone());
    let thread = std::thread::Builder::new().name("oep-tcp-in".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while !st.load(Ordering::SeqCst) {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stats2.bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if !deliver(&tx, buf[..n].to_vec(), &flow2, &stats2, &st) {
                        break;
                    }
                }
            }
        }
    })?;
    Ok(Transport {
        rx: Incoming {
            rx,
            flow: flow.clone(),
            counted: true,
        },
        flow,
        out: Box::new(TcpSink(s)),
        stop,
        thread: Some(thread),
        shutdown: Some(Box::new(move || {
            let _ = closer.shutdown(std::net::Shutdown::Both);
        })),
        reader: "TcpStream",
        keeps_boundaries: true,
        stats,
        depth: 0,
        port_path: None,
        set_baud: None,
    })
}

// ---------------- USB vendor bulk ----------------

use nusb::MaybeFuture;
use nusb::transfer::{Buffer, Bulk, In, Out};

struct UsbSink {
    ep: Mutex<nusb::Endpoint<Bulk, Out>>,
}

impl Sink for UsbSink {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        let mut ep = self.ep.lock().map_err(|_| Error::Io("the USB OUT endpoint".into()))?;
        let packet = ep.max_packet_size();
        let c = ep.transfer_blocking(Buffer::from(data.to_vec()), Duration::from_secs(2));
        c.status.map_err(|e| Error::Io(format!("USB write: {e}")))?;
        if !data.is_empty() && data.len().is_multiple_of(packet) {
            // a zero-length packet ends the transfer (transports §1)
            let c = ep.transfer_blocking(Buffer::new(0), Duration::from_secs(2));
            c.status.map_err(|e| Error::Io(format!("USB write: {e}")))?;
        }
        Ok(())
    }
}

/// A USB device by its serial number (= the probe's unit_id, transports §3), whatever its VID:PID: (vid, pid).
pub fn find_usb(unit_id: &str) -> Result<nusb::DeviceInfo> {
    let want = unit_id.to_ascii_lowercase();
    let list = nusb::list_devices()
        .wait()
        .map_err(|e| Error::Io(format!("USB devices: {e}")))?;
    for d in list {
        if d.serial_number().map(|s| s.to_ascii_lowercase()) == Some(want.clone()) {
            return Ok(d);
        }
    }
    Err(Error::NotFound(format!(
        "no USB device with serial number (unit id) {unit_id}"
    )))
}

/// The probe's vendor bulk interface opened, `depth` IN transfers of `urb` bytes queued on a thread of their own.
pub fn open_usb(info: &nusb::DeviceInfo, urb: usize, depth: usize) -> Result<Transport> {
    let intf = info
        .interfaces()
        .find(|i| (i.class(), i.subclass(), i.protocol()) == (VENDOR_CLASS, VENDOR_SUBCLASS, VENDOR_PROTOCOL))
        .map(|i| i.interface_number())
        .ok_or_else(|| {
            Error::NotFound("no OEP vendor interface (class 0xFF, subclass 0x4F, protocol 0x45) on the device".into())
        })?;
    let dev = info.open().wait().map_err(|e| Error::Io(format!("USB open: {e}")))?;
    let iface = dev
        .claim_interface(intf)
        .wait()
        .map_err(|e| Error::Io(format!("USB interface {intf}: {e}")))?;
    let desc = iface
        .descriptor()
        .ok_or_else(|| Error::Io("USB interface: no descriptor".into()))?;
    let (mut ep_in, mut ep_out) = (None, None);
    for e in desc.endpoints() {
        if e.transfer_type() != nusb::descriptors::TransferType::Bulk {
            continue;
        }
        if e.address() & 0x80 != 0 {
            ep_in.get_or_insert(e.address());
        } else {
            ep_out.get_or_insert(e.address());
        }
    }
    let (Some(ain), Some(aout)) = (ep_in, ep_out) else {
        return Err(Error::NotFound(
            "the OEP vendor interface has no bulk IN / OUT pair".into(),
        ));
    };
    let mut rd = iface
        .endpoint::<Bulk, In>(ain)
        .map_err(|e| Error::Io(format!("USB IN endpoint: {e}")))?;
    let wr = iface
        .endpoint::<Bulk, Out>(aout)
        .map_err(|e| Error::Io(format!("USB OUT endpoint: {e}")))?;
    let packet = rd.max_packet_size().max(1);
    let urb = urb.div_ceil(packet) * packet;
    for _ in 0..depth {
        let b = rd.allocate(urb);
        rd.submit(b);
    }
    let (tx, rx) = channel::<Vec<u8>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let flow = Arc::new(Flow::default());
    stats.inflight_min.store(depth as u64, Ordering::Relaxed);
    let (st, stats2, flow2) = (stop.clone(), stats.clone(), flow.clone());
    let thread = std::thread::Builder::new()
        .name("oep-usb-in".into())
        .spawn(move || usb_in(rd, tx, st, stats2, flow2, urb))?;
    Ok(Transport {
        rx: Incoming {
            rx,
            flow: flow.clone(),
            counted: true,
        },
        flow,
        out: Box::new(UsbSink { ep: Mutex::new(wr) }),
        stop,
        thread: Some(thread),
        shutdown: None,
        reader: "rust-usb-async",
        keeps_boundaries: false,
        stats,
        depth,
        port_path: None,
        set_baud: None,
    })
}

/// The IN thread: every transfer that comes back is copied out and sent out again at once, unless the stream's
/// backlog is at its budget - then it waits there (the kernel takes no more into it meanwhile; the others stay
/// queued).
fn usb_in(
    mut ep: nusb::Endpoint<Bulk, In>,
    tx: Sender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    flow: Arc<Flow>,
    urb: usize,
) {
    let mut last: Option<std::time::Instant> = None;
    let mut failed_at_start = 0usize;
    let mut got_any = false;
    while !stop.load(Ordering::SeqCst) && ep.pending() > 0 {
        let Some(c) = ep.wait_next_complete(Duration::from_millis(100)) else {
            continue;
        };
        let now = std::time::Instant::now();
        if let Some(t) = last {
            let us = now.duration_since(t).as_micros() as u64;
            stats.gap_max_us.fetch_max(us, Ordering::Relaxed);
        }
        last = Some(now);
        stats.inflight_min.fetch_min(ep.pending() as u64, Ordering::Relaxed);
        match c.status {
            Ok(()) => {
                got_any = true;
                let n = c.actual_len.min(c.buffer.len());
                if n > 0 {
                    stats.bytes.fetch_add(n as u64, Ordering::Relaxed);
                    stats.transfers.fetch_add(1, Ordering::Relaxed);
                    if !deliver(&tx, c.buffer[..n].to_vec(), &flow, &stats, &stop) {
                        return;
                    }
                }
                let mut b = c.buffer;
                b.clear();
                b.set_requested_len(urb);
                ep.submit(b);
            }
            Err(nusb::transfer::TransferError::Cancelled) => {}
            Err(_) if !got_any => {
                // a submit usbfs refused (its memory): the depth is what it took, unless none was taken
                failed_at_start += 1;
            }
            Err(_) => {
                std::thread::sleep(Duration::from_millis(10));
                let mut b = c.buffer;
                b.clear();
                b.set_requested_len(urb);
                ep.submit(b);
            }
        }
    }
    let _ = failed_at_start;
    ep.cancel_all();
    while ep.pending() > 0 {
        if ep.wait_next_complete(Duration::from_secs(1)).is_none() {
            break;
        }
    }
}

struct NullSink;

impl Sink for NullSink {
    fn send(&mut self, _: &[u8]) -> Result<()> {
        Ok(())
    }
}

impl Transport {
    /// A stream that only brings what `rx` gives (writes go nowhere): for a test of what reads it.
    pub fn from_rx(rx: Receiver<Vec<u8>>) -> Transport {
        let flow = Arc::new(Flow::default());
        Transport {
            rx: Incoming {
                rx,
                flow: flow.clone(),
                counted: false,
            },
            flow,
            out: Box::new(NullSink),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            shutdown: None,
            reader: "test",
            keeps_boundaries: false,
            stats: Arc::new(Stats::default()),
            depth: 0,
            port_path: None,
            set_baud: None,
        }
    }

    /// A stream whose IN side takes what `kernel` gives as the USB one takes its transfers back (a test's or a bench's
    /// probe: what it cannot put in `kernel` it drops), counted in `flow`; writes go nowhere.
    pub fn in_side_of(kernel: Receiver<Vec<u8>>, flow: Arc<Flow>) -> Transport {
        let (tx, rx) = channel::<Vec<u8>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let (st, stats2, flow2) = (stop.clone(), stats.clone(), flow.clone());
        let thread = std::thread::Builder::new()
            .name("oep-test-in".into())
            .spawn(move || {
                let mut last: Option<Instant> = None;
                while !st.load(Ordering::SeqCst) {
                    let c = match kernel.recv_timeout(Duration::from_millis(100)) {
                        Ok(c) => c,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let now = Instant::now();
                    if let Some(t) = last {
                        stats2
                            .gap_max_us
                            .fetch_max(now.duration_since(t).as_micros() as u64, Ordering::Relaxed);
                    }
                    last = Some(now);
                    stats2.bytes.fetch_add(c.len() as u64, Ordering::Relaxed);
                    stats2.transfers.fetch_add(1, Ordering::Relaxed);
                    if !deliver(&tx, c, &flow2, &stats2, &st) {
                        break;
                    }
                }
            })
            .expect("a thread");
        Transport {
            rx: Incoming {
                rx,
                flow: flow.clone(),
                counted: true,
            },
            flow,
            out: Box::new(NullSink),
            stop,
            thread: Some(thread),
            shutdown: None,
            reader: "test",
            keeps_boundaries: false,
            stats,
            depth: 0,
            port_path: None,
            set_baud: None,
        }
    }

    /// A stream whose writes go to `answer` and whose reads are what it gives back (a scripted probe for tests);
    /// `keeps_boundaries` as TCP's.
    pub fn scripted<F: FnMut(&[u8]) -> Vec<Vec<u8>> + Send + 'static>(answer: F, keeps_boundaries: bool) -> Transport {
        let (tx, rx) = channel::<Vec<u8>>();
        let flow = Arc::new(Flow::default());
        Transport {
            rx: Incoming {
                rx,
                flow: flow.clone(),
                counted: false,
            },
            flow,
            out: Box::new(ScriptSink { answer, tx }),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            shutdown: None,
            reader: "test",
            keeps_boundaries,
            stats: Arc::new(Stats::default()),
            depth: 0,
            port_path: None,
            set_baud: None,
        }
    }
}

/// A stand-in probe for tests: each write handed to `answer`, whose chunks come back as what was read.
struct ScriptSink<F: FnMut(&[u8]) -> Vec<Vec<u8>> + Send> {
    answer: F,
    tx: Sender<Vec<u8>>,
}

impl<F: FnMut(&[u8]) -> Vec<Vec<u8>> + Send> Sink for ScriptSink<F> {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        for c in (self.answer)(data) {
            self.tx
                .send(c)
                .map_err(|_| Error::Io("the scripted probe's reader went".into()))?;
        }
        Ok(())
    }
}

// ---------------- a serial port (transports §4) ----------------

/// The UART bridge's boot speed (transports §4: 115200 8N1).
pub const BASE_BAUD: u32 = 115_200;

#[cfg(unix)]
fn termios_at(f: &std::fs::File, baud: u32) -> Result<()> {
    use rustix::termios::{self, ControlModes, OptionalActions, SpecialCodeIndex};
    let mut t = termios::tcgetattr(f).map_err(|e| Error::Io(format!("serial port: {e}")))?;
    t.make_raw();
    t.control_modes |= ControlModes::CLOCAL | ControlModes::CREAD;
    t.control_modes -= ControlModes::CRTSCTS | ControlModes::CSTOPB | ControlModes::PARENB;
    t.special_codes[SpecialCodeIndex::VMIN] = 0;
    t.special_codes[SpecialCodeIndex::VTIME] = 1; // a read returns after 100 ms with nothing
    t.set_speed(baud)
        .map_err(|e| Error::Value(format!("serial port: {baud} baud: {e}")))?;
    termios::tcsetattr(f, OptionalActions::Now, &t)
        .map_err(|e| Error::Value(format!("serial port: {baud} baud: {e}")))?;
    let _ = termios::tcflush(f, termios::QueueSelector::IFlush);
    Ok(())
}

struct FileSink(std::fs::File);

impl Sink for FileSink {
    fn send(&mut self, data: &[u8]) -> Result<()> {
        self.0
            .write_all(data)
            .map_err(|e| Error::Io(format!("serial port write: {e}")))
    }
}

/// A serial port opened exclusively (flock and TIOCEXCL: a second open fails), 8N1, no flow control, at `baud`,
/// read on a thread of its own (oep-client-python's `open_serial`).
#[cfg(unix)]
pub fn open_serial(path: &str, baud: u32) -> Result<Transport> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(rustix::fs::OFlags::NOCTTY.bits() as i32 | rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Error::NotFound(format!("{path}: {e}")),
            _ => Error::Io(format!("{path}: {e}")),
        })?;
    if rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_err() {
        return Err(Error::Io(format!(
            "{path}: the port is in use (another process holds it)"
        )));
    }
    let _ = rustix::termios::ioctl_tiocexcl(&f);
    // blocking again: reads wait VTIME (100 ms) at the most
    let flags = rustix::fs::fcntl_getfl(&f).map_err(|e| Error::Io(format!("{path}: {e}")))?;
    rustix::fs::fcntl_setfl(&f, flags - rustix::fs::OFlags::NONBLOCK).map_err(|e| Error::Io(format!("{path}: {e}")))?;
    termios_at(&f, baud)?;
    let mut r = f.try_clone()?;
    let w = f.try_clone()?;
    let (tx, rx) = channel::<Vec<u8>>();
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let flow = Arc::new(Flow::default());
    let (st, stats2, flow2) = (stop.clone(), stats.clone(), flow.clone());
    let thread = std::thread::Builder::new()
        .name("oep-serial-in".into())
        .spawn(move || {
            let mut buf = vec![0u8; 16 * 1024];
            while !st.load(Ordering::SeqCst) {
                match r.read(&mut buf) {
                    Ok(0) => continue,
                    Ok(n) => {
                        stats2.bytes.fetch_add(n as u64, Ordering::Relaxed);
                        if !deliver(&tx, buf[..n].to_vec(), &flow2, &stats2, &st) {
                            break;
                        }
                    }
                    Err(e)
                        if e.kind() == std::io::ErrorKind::Interrupted
                            || e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        })?;
    let setter = f;
    Ok(Transport {
        rx: Incoming {
            rx,
            flow: flow.clone(),
            counted: true,
        },
        flow,
        out: Box::new(FileSink(w)),
        stop,
        thread: Some(thread),
        shutdown: None,
        reader: "rust-serial",
        keeps_boundaries: false,
        stats,
        depth: 0,
        port_path: Some(path.to_string()),
        set_baud: Some(Box::new(move |b| termios_at(&setter, b))),
    })
}

#[cfg(not(unix))]
pub fn open_serial(path: &str, _baud: u32) -> Result<Transport> {
    Err(Error::Value(format!(
        "{path}: serial ports are oep-client-python's on this system"
    )))
}
