//! The byte streams a probe is reached on (transports §1-§3): a TCP connection (a probe's TCP transport, the virtual
//! bench of oep-client-python) and the USB vendor bulk interface (class 0xFF, subclass 'O', protocol 'E').
//!
//! Each has a thread of its own taking what comes in, so a stream is taken while the caller is busy: the USB one keeps
//! many IN transfers queued in the kernel (oep-client-python's usb-async / UsbProcessStream: the probe ends a transfer
//! with a short packet, so a transfer holds one frame and the depth is how many frames the host can hold back), and
//! puts what came in on a bounded queue - past it the transfers are not sent out again, the probe's own buffer fills
//! and it drops whole frames, which the stream's positions show as a loss (memory stays bounded whatever the host
//! keeps up with).

use crate::msg::{Error, Result};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The vendor bulk interface (transports §3).
pub const VENDOR_CLASS: u8 = 0xFF;
pub const VENDOR_SUBCLASS: u8 = 0x4F;
pub const VENDOR_PROTOCOL: u8 = 0x45;
/// An IN transfer's size: a frame's most (the probe ends each transfer with a short packet).
pub const URB_SIZE: usize = 32 * 1024;
/// IN transfers kept queued: 480 x 32 KiB is 15 MiB of usbfs_memory_mb's default 16 (a submit refused leaves the depth
/// at what it took).
pub const DEPTH: usize = 480;
/// Chunks waiting for the caller at most (each one transfer's: at most 32 KiB, so 64 MiB).
pub const BACKLOG: usize = 2048;

/// What the IN side did (meta.probe.link's reader statistics).
#[derive(Debug, Default)]
pub struct Stats {
    pub bytes: AtomicU64,
    pub transfers: AtomicU64,
    /// the fewest transfers queued when one came back (near 0: the stream outran the host)
    pub inflight_min: AtomicU64,
    /// the longest between two completions, in microseconds
    pub gap_max_us: AtomicU64,
    /// times the queue to the caller was full (the transfer held back until there was room)
    pub held: AtomicU64,
}

trait Sink: Send {
    fn send(&mut self, data: &[u8]) -> Result<()>;
}

/// One open stream: what came in (chunks, in order), how to write, and what reads it.
pub struct Transport {
    pub rx: Receiver<Vec<u8>>,
    out: Box<dyn Sink>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    shutdown: Option<Box<dyn FnOnce() + Send>>,
    /// how it reads: "rust-usb-async" or "TcpStream" (meta.probe.link.reader)
    pub reader: &'static str,
    /// a stream that never loses a frame boundary (TCP)
    pub keeps_boundaries: bool,
    pub stats: Arc<Stats>,
    /// the IN transfers queued (USB)
    pub depth: usize,
}

impl Transport {
    pub fn write(&mut self, data: &[u8]) -> Result<()> {
        self.out.send(data)
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
    let (tx, rx) = sync_channel::<Vec<u8>>(BACKLOG);
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    let (st, stats2) = (stop.clone(), stats.clone());
    let thread = std::thread::Builder::new().name("oep-tcp-in".into()).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        while !st.load(Ordering::SeqCst) {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    stats2.bytes.fetch_add(n as u64, Ordering::Relaxed);
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    })?;
    Ok(Transport {
        rx,
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
    let (tx, rx) = sync_channel::<Vec<u8>>(BACKLOG);
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(Stats::default());
    stats.inflight_min.store(depth as u64, Ordering::Relaxed);
    let (st, stats2) = (stop.clone(), stats.clone());
    let thread = std::thread::Builder::new()
        .name("oep-usb-in".into())
        .spawn(move || usb_in(rd, tx, st, stats2, urb))?;
    Ok(Transport {
        rx,
        out: Box::new(UsbSink { ep: Mutex::new(wr) }),
        stop,
        thread: Some(thread),
        shutdown: None,
        reader: "rust-usb-async",
        keeps_boundaries: false,
        stats,
        depth,
    })
}

/// The IN thread: every transfer that comes back is copied out and sent out again at once, unless the caller's
/// queue is full - then it waits there (the kernel takes no more into it meanwhile; the others stay queued).
fn usb_in(
    mut ep: nusb::Endpoint<Bulk, In>,
    tx: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    urb: usize,
) {
    use std::sync::mpsc::TrySendError;
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
                    let mut chunk = c.buffer[..n].to_vec();
                    loop {
                        match tx.try_send(chunk) {
                            Ok(()) => break,
                            Err(TrySendError::Full(back)) => {
                                stats.held.fetch_add(1, Ordering::Relaxed);
                                if stop.load(Ordering::SeqCst) {
                                    return;
                                }
                                std::thread::sleep(Duration::from_millis(1));
                                chunk = back;
                            }
                            Err(TrySendError::Disconnected(_)) => return,
                        }
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
