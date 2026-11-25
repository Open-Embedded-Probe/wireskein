//! The way a device's received bytes go, for a driver whose device sends packed samples at stream positions (a
//! streaming logic analyser, or a memory read out): its IN side puts what came (`Inlet`) on a queue the reader takes
//! from (`Outlet`) within the stream's one budget in bytes (`flow`), the reader hands it to the spool on a thread of
//! its own (`SpoolWorker`), which cuts parts where a position was skipped (a loss) and writes each line's bits out.
//!
//! A streaming device's IN side must never wait for the host: what does not fit its own buffer it drops (`Inlet::
//! try_put`), which the next position shows as a loss. A device read out at the host's pace (a memory) waits instead
//! (`Inlet::put`).

use crate::driver::{Ctl, Progress, Stage};
use crate::error::{Error, Result};
use crate::flow::{self, Flow};
use crate::parts::{Loss, Part};
use crate::spool::{Feed, SpoolWorker};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::time::{Duration, Instant};

/// Bytes at a stream position (the device's count of bytes since the start).
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    pub at: u64,
    pub data: Vec<u8>,
}

/// A device's IN side: where what came in goes.
pub struct Inlet {
    tx: Sender<Chunk>,
    pub flow: Arc<Flow>,
    /// times a chunk was held back (the backlog at its budget)
    pub held: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Inlet {
    /// Whether `n` more bytes fit the stream's budget now.
    pub fn room_for(&self, n: u64) -> bool {
        self.flow.room_for(n)
    }
    /// The chunk to the reader, waiting while the budget is spent (a device read at the host's pace): false when
    /// stopped or nobody reads.
    pub fn put(&self, c: Chunk) -> bool {
        let n = c.data.len() as u64;
        flow::deliver(&self.tx, c, n, &self.flow, &self.held, &self.stop)
    }
    /// The chunk to the reader if it fits the budget now; else it comes back (a streaming device keeps it in its own
    /// buffer, or drops it).
    pub fn try_put(&self, c: Chunk) -> std::result::Result<(), Chunk> {
        let n = c.data.len() as u64;
        if !self.flow.room_for(n) {
            self.held.fetch_add(1, Ordering::Relaxed);
            return Err(c);
        }
        self.flow.queue(n);
        if let Err(e) = self.tx.send(c) {
            self.flow.unqueue(n);
            return Err(e.0);
        }
        Ok(())
    }
    /// Whether the reader asked the IN side to stop.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
}

/// The reader's side.
pub struct Outlet {
    rx: Receiver<Chunk>,
    pub flow: Arc<Flow>,
    stop: Arc<AtomicBool>,
}

impl Outlet {
    pub fn recv_timeout(&self, d: Duration) -> std::result::Result<Chunk, RecvTimeoutError> {
        let r = self.rx.recv_timeout(d);
        if let Ok(c) = &r {
            self.flow.unqueue(c.data.len() as u64);
        }
        r
    }
    /// Tell the IN side to stop (it ends its thread; what it sent before still comes).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// An IN side and its reader, counted in `flow`.
pub fn pipe(flow: Arc<Flow>) -> (Inlet, Outlet) {
    let (tx, rx) = channel();
    let stop = Arc::new(AtomicBool::new(false));
    (
        Inlet {
            tx,
            flow: flow.clone(),
            held: Arc::new(AtomicU64::new(0)),
            stop: stop.clone(),
        },
        Outlet { rx, flow, stop },
    )
}

/// What a stream's reading came to.
#[derive(Debug)]
pub struct Collected {
    pub parts: Vec<Part>,
    pub lost: Vec<Loss>,
    pub chunks: u64,
    pub bytes: u64,
    /// the position just past the last byte that came
    pub end: Option<u64>,
    pub read_s: f64,
    /// stopped by the caller (`Ctl::stop`) before its end
    pub stopped: bool,
}

/// When the reading ends: at a time (a stream's duration), or when the IN side is done (it dropped its Inlet).
pub struct Until {
    pub deadline: Option<Instant>,
    /// the bytes there will be (progress), when known
    pub of_bytes: Option<u64>,
    pub stage: Stage,
    /// stop at the first loss
    pub on_loss_stop: bool,
}

/// Read what `outlet` brings into `spool` until `until` (or the caller stops it, or a loss with on_loss stop); then
/// tell the IN side to stop and take what it still sends (it ends by dropping its Inlet), and put it all together.
/// `write_pos`: the end of the data when the device stopped in error (nothing at or past it is data).
pub fn collect(
    outlet: &Outlet,
    mut spool: SpoolWorker,
    until: &Until,
    ctl: &Ctl,
    write_pos: impl FnOnce() -> Option<u64>,
) -> Result<Collected> {
    let t0 = Instant::now();
    let (mut chunks, mut bytes, mut end) = (0u64, 0u64, None);
    let mut last_work = Instant::now();
    let mut stopped = false;
    let tell = |bytes: u64, losses: usize| {
        ctl.tell(&Progress {
            stage: until.stage,
            elapsed_s: t0.elapsed().as_secs_f64(),
            bytes,
            of_bytes: until.of_bytes,
            backlog: outlet.flow.backlog(),
            losses,
        })
    };
    let mut done = false;
    loop {
        if until.deadline.is_some_and(|d| Instant::now() >= d) {
            break;
        }
        if ctl.stopped() {
            stopped = true;
            break;
        }
        match outlet.recv_timeout(Duration::from_millis(20)) {
            Ok(c) => {
                chunks += 1;
                bytes += c.data.len() as u64;
                end = Some(c.at + c.data.len() as u64);
                spool.run(c.at, c.data)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                done = true;
                break;
            }
        }
        if last_work.elapsed() >= Duration::from_millis(200) {
            spool.work()?;
            last_work = Instant::now();
            tell(bytes, spool.lost_any() as usize);
            if until.on_loss_stop && spool.lost_any() {
                break;
            }
        }
    }
    outlet.stop();
    if !done {
        // what the IN side sent before it stopped (it ends by dropping its Inlet)
        let fin = Instant::now() + Duration::from_secs(5);
        while Instant::now() < fin {
            match outlet.recv_timeout(Duration::from_millis(20)) {
                Ok(c) => {
                    chunks += 1;
                    bytes += c.data.len() as u64;
                    end = Some(c.at + c.data.len() as u64);
                    spool.run(c.at, c.data)?;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }
    let read_s = t0.elapsed().as_secs_f64();
    let (parts, lost) = spool.finish(write_pos())?;
    tell(bytes, lost.len());
    Ok(Collected {
        parts,
        lost,
        chunks,
        bytes,
        end,
        read_s,
        stopped,
    })
}

/// A new directory of its own under `base` (tempfile.mkdtemp's): where a stream's lines are kept while it runs.
pub fn temp_dir(base: &Path, prefix: &str) -> Result<PathBuf> {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    for k in 0..100u64 {
        let p = base.join(format!(
            "{prefix}{}{k}",
            std::process::id() as u64 * 7919 + seed % 1_000_000
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(format!("{}: {e}", p.display()))),
        }
    }
    Err(Error::Io(format!("{}: no new directory", base.display())))
}
