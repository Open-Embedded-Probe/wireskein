//! How a stream's bytes flow from a device's IN side to the files (bench/notes/2026-11-24-rust-plan.ja.md §42, shared
//! by every device since §45): the counts the stages share, so the IN side can see the whole backlog, and where it
//! stood still is measured.
//!
//! The IN side never waits for the stages after it while the backlog - the bytes in the transport's queue and the
//! bytes handed to the spool and not yet put together and written out - is within the budget: a stage that stops
//! for a while (the disk's write-back, a host with every core busy) leaves its bytes in memory and catches up after.
//! Past the budget the IN side holds back (the probe's own buffer fills and it drops whole frames, which the
//! positions show as a loss): memory stays bounded whatever the host keeps up with. (What the reader took into its
//! own buffer and has not yet handed on - Link::recv takes 8 MiB at most at a time - is not counted: the bound is the
//! budget and that.)

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

/// The backlog a stream may build up by default: bytes received and not yet written out. 512 MiB is 13 s at
/// 40 MB/s, more than oep-client-python's UsbProcessStream holds in its two processes and its spool (64 + 64 +
/// 256 MiB).
pub const BUDGET: u64 = 512 << 20;

/// A spool file's buffer: a channel's bits go to the disk this many bytes at a time.
pub const FILE_BUFFER: usize = 1 << 20;

/// The shape of the flow, changed by `WIRESKEIN_CAPTURE_FLOW` (or `WIRESKEIN_OEP_FLOW`, the name an OEP stream has
/// taken it by; words separated by commas): `budget=MIB` the backlog's
/// bound (the host's room is its user's to give), and for a bench comparing shapes `counts` (the first shape of §37),
/// `buffer=KIB` (a spool file's buffer), `nice=N` (the spool's thread that much nicer than the IN side), `writer` (the spool's files written on a
/// thread of their own).
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// the transport's queue bounded at this many chunks and the spool's at that many pieces, each on its own,
    /// instead of the budget in bytes over both ((0, 0): the budget)
    pub counts: (u64, u64),
    pub budget: u64,
    pub file_buffer: usize,
    /// the spool's thread this much nicer (Linux: its own nice value; 0 leaves it)
    pub spool_nice: i32,
    /// the spool's files written on a thread of their own
    pub writer: bool,
}

impl Default for Shape {
    fn default() -> Shape {
        Shape {
            counts: (0, 0),
            budget: BUDGET,
            file_buffer: FILE_BUFFER,
            spool_nice: 0,
            writer: false,
        }
    }
}

impl Shape {
    /// The default, changed by `WIRESKEIN_CAPTURE_FLOW`, else `WIRESKEIN_OEP_FLOW` (the user's room, or a bench's
    /// shape).
    pub fn from_env() -> Shape {
        let v = std::env::var("WIRESKEIN_CAPTURE_FLOW")
            .or_else(|_| std::env::var("WIRESKEIN_OEP_FLOW"))
            .unwrap_or_default();
        Shape::parse(&v)
    }

    pub fn parse(text: &str) -> Shape {
        let mut s = Shape::default();
        for w in text.split(',').map(str::trim).filter(|w| !w.is_empty()) {
            let (k, v) = w.split_once('=').unwrap_or((w, ""));
            let n = v.parse::<u64>().ok();
            match (k, n) {
                ("counts", _) => {
                    s.counts = (4096, 8192);
                    s.budget = u64::MAX;
                    s.file_buffer = 8 << 10;
                }
                ("budget", Some(n)) => s.budget = n << 20,
                ("buffer", Some(n)) => s.file_buffer = (n as usize) << 10,
                ("nice", Some(n)) => s.spool_nice = n as i32,
                ("writer", _) => s.writer = true,
                _ => {}
            }
        }
        s
    }
}

#[derive(Debug)]
pub struct Flow {
    pub shape: Shape,
    /// bytes in the transport's queue (the IN side's, not yet taken by the reader)
    pub queued: AtomicU64,
    /// bytes handed to the spool and not yet taken by it
    pub behind: AtomicU64,
    /// bytes the spool took and has not yet put together (its pending run, set after each piece of its work)
    pub pending: AtomicU64,
    /// bytes handed to the spool's writer and not yet written (`Shape::writer`)
    pub writing: AtomicU64,
    /// the most these came to together
    pub backlog_max: AtomicU64,
    /// the backlog's bound in bytes
    pub budget: AtomicU64,
    /// the longest the IN side held a chunk back (the backlog at its budget), in microseconds
    pub held_max_us: AtomicU64,
    /// the longest one piece of the spool's work took (its writes included), in microseconds
    pub work_max_us: AtomicU64,
    /// the longest one write to a file took, in microseconds
    pub write_max_us: AtomicU64,
    /// the longest the reader waited to hand bytes to the spool, in microseconds
    pub hand_max_us: AtomicU64,
    /// chunks in the transport's queue, and pieces handed to the spool
    pub queued_n: AtomicU64,
    pub behind_n: AtomicU64,
    /// a test's stall: the spool's next piece of work waits this long first (a disk that held it back; the demo
    /// device's `stall=`)
    pub stall: std::sync::Mutex<Option<std::time::Duration>>,
}

impl Default for Flow {
    fn default() -> Flow {
        Flow::new(Shape::from_env())
    }
}

impl Flow {
    pub fn new(shape: Shape) -> Flow {
        Flow {
            budget: AtomicU64::new(shape.budget),
            shape,
            queued: AtomicU64::new(0),
            behind: AtomicU64::new(0),
            pending: AtomicU64::new(0),
            writing: AtomicU64::new(0),
            backlog_max: AtomicU64::new(0),
            held_max_us: AtomicU64::new(0),
            work_max_us: AtomicU64::new(0),
            write_max_us: AtomicU64::new(0),
            hand_max_us: AtomicU64::new(0),
            queued_n: AtomicU64::new(0),
            behind_n: AtomicU64::new(0),
            stall: std::sync::Mutex::new(None),
        }
    }

    /// The bytes received and not yet written out.
    pub fn backlog(&self) -> u64 {
        self.queued.load(Ordering::Relaxed)
            + self.behind.load(Ordering::Relaxed)
            + self.pending.load(Ordering::Relaxed)
            + self.writing.load(Ordering::Relaxed)
    }

    /// Whether `n` more bytes fit (a chunk always fits an empty backlog).
    pub fn room_for(&self, n: u64) -> bool {
        if self.shape.counts.0 > 0 {
            return self.queued_n.load(Ordering::Relaxed) < self.shape.counts.0;
        }
        let b = self.backlog();
        b == 0 || b.saturating_add(n) <= self.budget.load(Ordering::Relaxed)
    }

    /// Whether the spool takes another piece (always, but in the first shape).
    pub fn spool_room(&self) -> bool {
        self.shape.counts.1 == 0 || self.behind_n.load(Ordering::Relaxed) < self.shape.counts.1
    }

    /// A chunk into the transport's queue, and out of it.
    pub fn queue(&self, n: u64) {
        self.queued.fetch_add(n, Ordering::Relaxed);
        self.queued_n.fetch_add(1, Ordering::Relaxed);
        self.note_backlog();
    }
    pub fn unqueue(&self, n: u64) {
        self.queued.fetch_sub(n, Ordering::Relaxed);
        self.queued_n.fetch_sub(1, Ordering::Relaxed);
    }
    /// A piece handed to the spool, and taken by it.
    pub fn hand(&self, n: u64) {
        self.behind.fetch_add(n, Ordering::Relaxed);
        self.behind_n.fetch_add(1, Ordering::Relaxed);
        self.note_backlog();
    }
    pub fn taken(&self, n: u64) {
        self.behind.fetch_sub(n, Ordering::Relaxed);
        self.behind_n.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn note_backlog(&self) {
        self.backlog_max.fetch_max(self.backlog(), Ordering::Relaxed);
    }

    /// `since` to now as the longest in `which`.
    pub fn longest(which: &AtomicU64, since: Instant) {
        which.fetch_max(since.elapsed().as_micros() as u64, Ordering::Relaxed);
    }

    /// The counts for meta.probe.link (bytes and milliseconds).
    pub fn report(&self) -> serde_json::Value {
        let ms = |a: &AtomicU64| a.load(Ordering::Relaxed) as f64 / 1000.0;
        let mut out = serde_json::json!({
            "budget": self.budget.load(Ordering::Relaxed),
            "backlog_max": self.backlog_max.load(Ordering::Relaxed),
            "held_max_ms": ms(&self.held_max_us),
            "work_max_ms": ms(&self.work_max_us),
            "write_max_ms": ms(&self.write_max_us),
            "hand_max_ms": ms(&self.hand_max_us),
        });
        if self.shape != Shape::default() {
            out["shape"] = serde_json::json!(format!("{:?}", self.shape));
        }
        out
    }
}

/// An item of `n` bytes to the reader: at once while the stream's backlog has room (whatever the reader and the spool
/// are doing); past its budget it waits there, `held` counting the times - false: stopped, or nobody reads. (A
/// device whose own buffer can wait uses `Flow::room_for` and keeps the item instead.)
pub fn deliver<T>(tx: &Sender<T>, item: T, n: u64, flow: &Flow, held: &AtomicU64, stop: &AtomicBool) -> bool {
    if !flow.room_for(n) {
        held.fetch_add(1, Ordering::Relaxed);
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
    if tx.send(item).is_err() {
        flow.unqueue(n);
        return false;
    }
    true
}

/// The calling thread `n` nicer (Linux: a thread has its own nice value; elsewhere nothing).
pub fn nicer(n: i32) {
    #[cfg(target_os = "linux")]
    if n > 0 {
        let now = rustix::process::getpriority_process(None).unwrap_or(0);
        let _ = rustix::process::setpriority_process(None, (now + n).min(19));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = n;
}
