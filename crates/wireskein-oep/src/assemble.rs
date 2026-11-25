//! A capture put together as it comes (oep-client-python's `acquire._Spool`, `_from_blocks`, `from_interleaved`):
//! the bytes of a stream in order of their positions, cut into parts where a position was skipped (the probe dropped a
//! segment: a loss), each part's channels as bits (on disk for a stream, in memory otherwise) or a reduced line's
//! values; a format 1 segment whose samples are not a multiple of every D starts a new part without a loss (the next
//! segment's grid restarts, capture §1.3).

use crate::blocks::{self, BitBuf, Blocks, Column};
use crate::capture::SegmentInfo;
use crate::msg::{Error, Result};
use crate::ratio::Ratio;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// A channel's bits: in memory, or in a file (a long stream's).
#[derive(Debug, Clone)]
pub enum Data {
    Mem(Vec<u8>),
    File(PathBuf),
}

impl Data {
    pub fn bytes(&self) -> Result<Vec<u8>> {
        match self {
            Data::Mem(b) => Ok(b.clone()),
            Data::File(p) => std::fs::read(p).map_err(|e| Error::Io(format!("{}: {e}", p.display()))),
        }
    }
}

/// A channel as the probe gave it: a line's samples (at step D for format 1's `sample`), or a line reduced over
/// intervals (any_high / any_low / change_latch: `bits` a value).
#[derive(Debug, Clone)]
pub enum Chan {
    Bits {
        name: String,
        data: Data,
        n: u64,
        step: u64,
        pin: u16,
    },
    Reduced {
        name: String,
        kind: u8,
        data: Vec<u8>,
        n: u64,
        step: u64,
        start_level: u8,
        pin: u16,
    },
    /// An analog line's raw values, at its own rate from tick `t0` (acquire.Analog).
    Analog(Box<AnalogChan>),
}

/// An analog line's raw values (`width` bits little endian; volts = (raw - zero) x scale_nv x 1e-9): sample k at tick
/// t0 + k x tick_hz / rate.
#[derive(Debug, Clone)]
pub struct AnalogChan {
    pub name: String,
    pub data: Vec<u8>,
    pub n: u64,
    pub rate: Ratio,
    pub t0: Ratio,
    pub width: u32,
    pub value_bits: Option<u32>,
    pub zero: Option<i64>,
    pub scale_nv: Option<i64>,
    /// pin, frontend, reference, vrefint, start_uncertainty_ns, trigger_index (acquire's `_analog_chans`)
    pub acquisition: Map<String, Value>,
}

impl AnalogChan {
    /// The tick just after its last value: ceil(t0 + n x tick / rate).
    pub fn end_at(&self, tick: Ratio) -> i64 {
        (self.t0 + Ratio::int(self.n as i128) * tick / self.rate).ceil() as i64
    }
}

impl Chan {
    pub fn name(&self) -> &str {
        match self {
            Chan::Bits { name, .. } | Chan::Reduced { name, .. } => name,
            Chan::Analog(a) => &a.name,
        }
    }
    /// The tick just after it (its last sample's or interval's); an analog line's is `end_at`'s.
    pub fn end(&self) -> u64 {
        match self {
            Chan::Bits { n, step, .. } | Chan::Reduced { n, step, .. } => n * step,
            Chan::Analog(_) => 0,
        }
    }
    pub fn step(&self) -> u64 {
        match self {
            Chan::Bits { step, .. } | Chan::Reduced { step, .. } => *step,
            Chan::Analog(_) => 1,
        }
    }
    pub fn is_analog(&self) -> bool {
        matches!(self, Chan::Analog(_))
    }
}

/// A block of ticks with no gap inside.
#[derive(Debug, Clone)]
pub struct Part {
    pub channels: Vec<Chan>,
    pub meta: Map<String, Value>,
    pub tick0: Option<i64>,
    /// its ticks, when known (a part of reduced lines only ends inside its last interval)
    pub samples: Option<u64>,
}

/// What was lost (wireskein-format §3.4): after which part, why, whether the capture stopped there, how long.
#[derive(Debug, Clone, PartialEq)]
pub struct Loss {
    pub after_part: i64,
    pub reason: String,
    pub stopped: bool,
    pub ticks: Option<i64>,
    next: Option<u64>,
}

impl Loss {
    pub fn new(after_part: i64, reason: impl Into<String>, stopped: bool) -> Loss {
        Loss {
            after_part,
            reason: reason.into(),
            stopped,
            ticks: None,
            next: None,
        }
    }
    pub fn to_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("after_part".into(), json!(self.after_part));
        m.insert("reason".into(), json!(self.reason));
        if self.stopped {
            m.insert("stopped".into(), json!(true));
        }
        if let Some(t) = self.ticks {
            m.insert("ticks".into(), json!(t));
        }
        Value::Object(m)
    }
}

/// How the bytes hold the samples.
#[derive(Debug, Clone)]
pub enum Format {
    /// format 0: samples of `width` bits, channel k at bit positions[k]
    Zero { width: usize, positions: Vec<usize> },
    /// format 1: the logic blocks
    One(Blocks),
}

/// Python's round of n / d (d > 0): to the nearest integer, ties to even.
pub fn round_div(n: i128, d: i128) -> i128 {
    let (q, r) = (n.div_euclid(d), n.rem_euclid(d));
    match (2 * r).cmp(&d) {
        std::cmp::Ordering::Less => q,
        std::cmp::Ordering::Greater => q + 1,
        std::cmp::Ordering::Equal => q + (q & 1),
    }
}

enum Sink {
    Mem(BitBuf),
    File(BitBuf, std::io::BufWriter<std::fs::File>, PathBuf),
}

impl Sink {
    fn append(&mut self, src: &[u8], n: usize) -> Result<()> {
        match self {
            Sink::Mem(b) => b.append(src, n),
            Sink::File(b, f, _) if b.nbits == 0 && n.is_multiple_of(8) => f.write_all(&src[..n / 8])?,
            Sink::File(b, f, _) => {
                b.append(src, n);
                if b.bytes.len() >= 1 << 16 {
                    f.write_all(&b.take_whole())?;
                }
            }
        }
        Ok(())
    }
    fn nbits(&self) -> usize {
        match self {
            Sink::Mem(b) | Sink::File(b, _, _) => b.nbits,
        }
    }
    /// Its data: a file's written out (the last partial byte too).
    fn close(self) -> Result<Data> {
        match self {
            Sink::Mem(b) => Ok(Data::Mem(b.bytes)),
            Sink::File(b, mut f, p) => {
                f.write_all(&b.bytes)?;
                f.flush()?;
                Ok(Data::File(p))
            }
        }
    }
}

/// A reduced channel of a part: its start level, its intervals, (carriage, D).
type Red = (Option<u8>, u64, Option<(u8, usize)>);

struct Cur {
    tick0: i64,
    meta: Map<String, Value>,
    samples: u64,
    sinks: Vec<Sink>,
    /// per channel: (start level, intervals, (carriage, D)) of a reduced one
    red: Vec<Red>,
}

/// The parts of a capture put together from its bytes in order of position.
pub struct Spool {
    names: Vec<String>,
    ids: Vec<u16>,
    rate: (u64, u64),
    format: Format,
    stop_on_loss: bool,
    dir: Option<PathBuf>,
    pub parts: Vec<Part>,
    lost: Vec<Loss>,
    pending: Vec<u8>,
    at: Option<u64>,
    segs: BTreeMap<u64, SegmentInfo>,
    first_ns: Option<u64>,
    cur: Option<Cur>,
    stopped: bool,
    follows: bool,
    last_samples: u64,
    last_end: Option<u64>,
    /// bytes put together
    pub bytes: u64,
}

impl Spool {
    /// `dir`: where the channels' bits go while it runs (None: memory).
    pub fn new(
        names: Vec<String>,
        ids: Vec<u16>,
        rate: (u64, u64),
        format: Format,
        stop_on_loss: bool,
        dir: Option<&Path>,
    ) -> Spool {
        Spool {
            names,
            ids,
            rate,
            format,
            stop_on_loss,
            dir: dir.map(Path::to_path_buf),
            parts: vec![],
            lost: vec![],
            pending: vec![],
            at: None,
            segs: BTreeMap::new(),
            first_ns: None,
            cur: None,
            stopped: false,
            follows: false,
            last_samples: 0,
            last_end: None,
            bytes: 0,
        }
    }

    /// Whether anything was lost so far.
    pub fn lost_any(&self) -> bool {
        !self.lost.is_empty()
    }

    /// A segment's record as the frames say it (the latest kept: samples grow while it is taken).
    pub fn segment(&mut self, info: SegmentInfo) {
        self.segs.insert(info.position, info);
    }

    /// Bytes from stream position `p0` (a stretch with no gap inside).
    pub fn run(&mut self, p0: u64, data: &[u8]) -> Result<()> {
        if self.stopped || data.is_empty() {
            return Ok(());
        }
        self.bytes += data.len() as u64;
        match self.at {
            None if self.parts.is_empty() && self.cur.is_none() => {
                if p0 != 0 {
                    if self.stop_on_loss {
                        return Err(Error::Runtime(format!(
                            "the probe dropped the stream's first {p0} bytes before any arrived: nothing to keep (a \
                             lower --rate or fewer channels, or --on-loss continue)"
                        )));
                    }
                    self.lost.push(Loss::new(
                        -1,
                        format!("the probe dropped the first {p0} bytes (no room to send)"),
                        false,
                    ));
                }
                self.at = Some(p0);
            }
            Some(at) if p0 != at + self.pending.len() as u64 => {
                // a skipped position: the probe dropped a segment
                self.work(true, None)?;
                self.close(false)?;
                let mut l = Loss::new(
                    self.parts.len() as i64 - 1,
                    "the probe dropped a segment (no room to send)",
                    self.stop_on_loss,
                );
                l.next = Some(p0);
                self.lost.push(l);
                if self.stop_on_loss {
                    self.stopped = true;
                    return Ok(());
                }
                self.at = Some(p0);
                self.pending.clear();
            }
            None => self.at = Some(p0),
            Some(_) => {}
        }
        self.pending.extend_from_slice(data);
        Ok(())
    }

    /// Put together what can be: `fin` everything (the end), `write_pos` the end of the data after an error stop.
    pub fn work(&mut self, fin: bool, write_pos: Option<u64>) -> Result<()> {
        if self.at.is_none() || self.pending.is_empty() {
            return Ok(());
        }
        match &self.format {
            Format::Zero { width, .. } if width.is_multiple_of(8) => self.samples(fin, write_pos),
            _ => self.segments(fin, write_pos),
        }
    }

    /// Format 0, whole bytes a sample: the bytes as they come, a whole number of bytes of bits at a time.
    fn samples(&mut self, fin: bool, write_pos: Option<u64>) -> Result<()> {
        let Format::Zero { width, positions } = self.format.clone() else {
            unreachable!()
        };
        let b = width / 8;
        let at = self.at.expect("at");
        let mut n = self.pending.len() / b;
        if let Some(w) = write_pos {
            n = n.min((w.saturating_sub(at) / b as u64) as usize);
        }
        if !fin {
            n = n / 8 * 8;
        }
        if n == 0 {
            if fin {
                self.pending.clear();
            }
            return Ok(());
        }
        self.open(at, None)?;
        let cur = self.cur.as_mut().expect("open");
        let pos: Vec<usize> = (0..cur.sinks.len())
            .map(|k| positions.get(k).copied().unwrap_or(k))
            .collect();
        let cols = blocks::columns(&self.pending[..n * b], width, &pos, n);
        for (s, col) in cur.sinks.iter_mut().zip(cols) {
            s.append(&col, n)?;
        }
        cur.samples += n as u64;
        self.pending.drain(..n * b);
        let at = at + (n * b) as u64;
        self.at = Some(at);
        let first = self.segs.range(..at).map(|(&k, _)| k).collect::<Vec<_>>();
        for q in first {
            self.segs.remove(&q);
        }
        if fin {
            self.pending.clear();
        }
        Ok(())
    }

    /// Format 1 (and format 0 of samples across bytes): segment by segment, as their records say where each begins;
    /// segments of whole blocks read in one go.
    fn segments(&mut self, fin: bool, write_pos: Option<u64>) -> Result<()> {
        let at = self.at.expect("at");
        let end = at + self.pending.len() as u64;
        let starts: Vec<u64> = self.segs.range(at..end).map(|(&k, _)| k).collect();
        let mut ready = vec![];
        for (k, &p) in starts.iter().enumerate() {
            let nxt = starts.get(k + 1).copied().or(if fin { Some(end) } else { None });
            let Some(nxt) = nxt else { break };
            if write_pos.is_some_and(|w| p >= w) {
                break;
            }
            ready.push((p, nxt));
        }
        let (block, ds) = match &self.format {
            Format::One(b) => (
                b.block as u64,
                b.reduced.iter().map(|(_, (_, d))| *d as u64).collect::<Vec<_>>(),
            ),
            Format::Zero { .. } => (0, vec![]),
        };
        let mut k = 0;
        while k < ready.len() {
            let mut j = k;
            while block > 0 && j + 1 < ready.len() && self.segs[&ready[j].0].samples.is_multiple_of(block) {
                j += 1;
            }
            let (p, nxt) = (ready[k].0, ready[j].1);
            let x = self.segs[&p];
            let mut samples: u64 = ready[k..=j]
                .iter()
                .map(|(q, _)| self.segs.remove(q).map(|s| s.samples).unwrap_or(0))
                .sum();
            if let Some(c) = &self.cur
                && ds.iter().any(|d| c.samples % d != 0)
            {
                self.close(true)?; // a grid restart (format 1) is not a loss: the next part follows on
            }
            self.open(p, Some(x))?;
            let data =
                self.pending[(p - self.at.expect("at")) as usize..(nxt - self.at.expect("at")) as usize].to_vec();
            let cols: Vec<Column> = match &self.format {
                Format::One(b) => b
                    .columns(&data, samples as usize)?
                    .into_iter()
                    .map(|(_, c)| c)
                    .collect(),
                Format::Zero { width, positions } => {
                    samples = samples.min(data.len() as u64 * 8 / *width as u64);
                    (0..self.names.len())
                        .map(|k| {
                            Column::Raw(blocks::column(
                                &data,
                                *width,
                                positions.get(k).copied().unwrap_or(k),
                                samples as usize,
                            ))
                        })
                        .collect()
                }
            };
            let cur = self.cur.as_mut().expect("open");
            for (c, col) in cols.into_iter().enumerate() {
                match col {
                    Column::Raw(bits) => cur.sinks[c].append(&bits, samples as usize)?,
                    Column::Reduced(rc) => {
                        let nb = blocks::value_bits(rc.kind);
                        cur.sinks[c].append(&rc.data, rc.n * nb)?;
                        let red = &mut cur.red[c];
                        if red.0.is_none() {
                            red.0 = Some(rc.start_level);
                        }
                        red.1 += rc.n as u64;
                        red.2 = Some((rc.kind, rc.d));
                    }
                }
            }
            cur.samples += samples;
            let cut = (nxt - self.at.expect("at")) as usize;
            self.pending.drain(..cut);
            self.at = Some(nxt);
            k = j + 1;
        }
        if fin {
            self.pending.clear();
        }
        Ok(())
    }

    fn open(&mut self, position: u64, seg: Option<SegmentInfo>) -> Result<()> {
        if self.cur.is_some() {
            return Ok(());
        }
        let seg = seg.or_else(|| self.segs.get(&position).copied());
        if self.first_ns.is_none()
            && let Some(s) = seg
        {
            self.first_ns = Some(s.start_ns);
        }
        let tick0 = if self.follows && !self.parts.is_empty() {
            self.parts.last().and_then(|p| p.tick0).unwrap_or(0) + self.last_samples as i64
        } else if let (Some(s), Some(f)) = (seg, self.first_ns) {
            let dns = s.start_ns as i128 - f as i128;
            round_div(dns * self.rate.0 as i128, 1_000_000_000 * self.rate.1 as i128) as i64
        } else {
            let width = match &self.format {
                Format::Zero { width, .. } => *width as u64,
                Format::One(b) => b.width.max(1) as u64,
            };
            let base = self
                .parts
                .last()
                .map(|p| p.tick0.unwrap_or(0) + self.last_samples as i64)
                .unwrap_or(0);
            base + ((position - self.last_end.unwrap_or(0)) * 8 / width) as i64
        };
        let k = self.parts.len();
        let mut sinks = vec![];
        for j in 0..self.names.len() {
            sinks.push(match &self.dir {
                None => Sink::Mem(BitBuf::default()),
                Some(d) => {
                    let p = d.join(format!("p{k}-{j}.bits"));
                    let f = std::fs::File::create(&p).map_err(|e| Error::Io(format!("{}: {e}", p.display())))?;
                    Sink::File(BitBuf::default(), std::io::BufWriter::new(f), p)
                }
            });
        }
        let mut meta = Map::new();
        if let Some(s) = seg {
            meta.insert("start_ns".into(), json!(s.start_ns));
            meta.insert("start_uncertainty_ns".into(), json!(s.start_uncertainty_ns));
        }
        self.cur = Some(Cur {
            tick0,
            meta,
            samples: 0,
            sinks,
            red: vec![(None, 0, None); self.names.len()],
        });
        self.follows = false;
        Ok(())
    }

    /// The part being put together ends (`follows`: the next one follows on, a grid restart).
    pub fn close(&mut self, follows: bool) -> Result<()> {
        let Some(cur) = self.cur.take() else { return Ok(()) };
        let n = cur.samples;
        let mut chans = vec![];
        for (role, (sink, red)) in cur.sinks.into_iter().zip(cur.red).enumerate() {
            let (name, pin) = (self.names[role].clone(), self.ids.get(role).copied().unwrap_or(0));
            let _ = sink.nbits();
            let data = sink.close()?;
            match red.2 {
                Some((kind, d)) if kind == blocks::SAMPLE => chans.push(Chan::Bits {
                    name,
                    data,
                    n: red.1,
                    step: d as u64,
                    pin,
                }),
                Some((kind, d)) => {
                    let bytes = data.bytes()?;
                    if let Data::File(p) = &data {
                        let _ = std::fs::remove_file(p);
                    }
                    chans.push(Chan::Reduced {
                        name,
                        kind,
                        data: bytes,
                        n: red.1,
                        step: d as u64,
                        start_level: red.0.unwrap_or(0),
                        pin,
                    })
                }
                None => chans.push(Chan::Bits {
                    name,
                    data,
                    n,
                    step: 1,
                    pin,
                }),
            }
        }
        if n > 0 {
            self.parts.push(Part {
                channels: chans,
                meta: cur.meta,
                tick0: Some(cur.tick0),
                samples: Some(n),
            });
        }
        self.last_samples = n;
        self.last_end = self.at;
        self.follows = follows;
        let np = self.parts.len();
        for x in self.lost.iter_mut() {
            if x.next.is_some() && np >= 2 && x.after_part == np as i64 - 2 {
                let (a, b) = (&self.parts[np - 2], &self.parts[np - 1]);
                let gap = b.tick0.unwrap_or(0)
                    - a.tick0.unwrap_or(0)
                    - a.channels.first().map(|c| c.end() as i64).unwrap_or(0);
                if gap > 0 {
                    x.ticks = Some(gap);
                }
                x.next = None;
            }
        }
        Ok(())
    }

    /// The end: everything put together (`write_pos`: an error stop's, nothing at or past it is data).
    pub fn finish(&mut self, write_pos: Option<u64>) -> Result<()> {
        self.work(true, write_pos)?;
        self.close(false)
    }

    /// (parts, losses)
    pub fn take(self) -> (Vec<Part>, Vec<Loss>) {
        (self.parts, self.lost)
    }
}

/// What takes a stream's pushes: a spool here, or one on a thread of its own.
pub trait Feed {
    fn segment(&mut self, info: SegmentInfo);
    fn run(&mut self, p0: u64, data: Vec<u8>) -> Result<()>;
    /// put together what can be (a poll's worth)
    fn work(&mut self) -> Result<()>;
    fn lost_any(&self) -> bool;
}

impl Feed for Spool {
    fn segment(&mut self, info: SegmentInfo) {
        Spool::segment(self, info)
    }
    fn run(&mut self, p0: u64, data: Vec<u8>) -> Result<()> {
        Spool::run(self, p0, &data)
    }
    fn work(&mut self) -> Result<()> {
        Spool::work(self, false, None)
    }
    fn lost_any(&self) -> bool {
        Spool::lost_any(self)
    }
}

enum Job {
    Segment(SegmentInfo),
    Run(u64, Vec<u8>),
    Work,
}

/// A spool on a thread of its own (acquire._Spool's worker): the reader hands the pushes over and goes on reading; a
/// queue of QUEUE jobs at most (past it the reader waits, the transport's queue fills, the probe drops whole frames,
/// which the positions show).
pub struct SpoolWorker {
    tx: Option<std::sync::mpsc::SyncSender<Job>>,
    thread: Option<std::thread::JoinHandle<Result<Spool>>>,
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SpoolWorker {
    const QUEUE: usize = 8192;

    pub fn start(mut spool: Spool) -> SpoolWorker {
        use std::sync::atomic::Ordering;
        let (tx, rx) = std::sync::mpsc::sync_channel::<Job>(Self::QUEUE);
        let lost = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = lost.clone();
        let thread = std::thread::Builder::new()
            .name("oep-spool".into())
            .spawn(move || {
                for job in rx {
                    match job {
                        Job::Segment(i) => spool.segment(i),
                        Job::Run(p, d) => spool.run(p, &d)?,
                        Job::Work => spool.work(false, None)?,
                    }
                    if spool.lost_any() {
                        flag.store(true, Ordering::SeqCst);
                    }
                }
                Ok(spool)
            })
            .expect("a thread");
        SpoolWorker {
            tx: Some(tx),
            thread: Some(thread),
            lost,
        }
    }

    fn send(&mut self, job: Job) -> Result<()> {
        match &self.tx {
            Some(tx) if tx.send(job).is_ok() => Ok(()),
            _ => Err(self.stop_err()),
        }
    }

    fn stop_err(&mut self) -> Error {
        self.tx = None;
        match self.thread.take().map(|t| t.join()) {
            Some(Ok(Err(e))) => e,
            _ => Error::Runtime("the stream's spool stopped".into()),
        }
    }

    /// The end: everything handed over put together (`write_pos`: an error stop's). -> (parts, losses)
    pub fn finish(mut self, write_pos: Option<u64>) -> Result<(Vec<Part>, Vec<Loss>)> {
        self.tx = None;
        let mut spool = match self.thread.take().map(|t| t.join()) {
            Some(Ok(Ok(s))) => s,
            Some(Ok(Err(e))) => return Err(e),
            _ => return Err(Error::Runtime("the stream's spool stopped".into())),
        };
        spool.finish(write_pos)?;
        Ok(spool.take())
    }
}

impl Feed for SpoolWorker {
    fn segment(&mut self, info: SegmentInfo) {
        let _ = self.send(Job::Segment(info));
    }
    fn run(&mut self, p0: u64, data: Vec<u8>) -> Result<()> {
        self.send(Job::Run(p0, data))
    }
    fn work(&mut self) -> Result<()> {
        self.send(Job::Work)
    }
    fn lost_any(&self) -> bool {
        self.lost.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One segment's bytes (a one-shot's or a repeat's) as its channels, in the order asked.
pub fn segment_channels(
    format: &Format,
    data: &[u8],
    samples: u64,
    names: &[String],
    ids: &[u16],
) -> Result<Vec<Chan>> {
    let pin = |k: usize| ids.get(k).copied().unwrap_or(0);
    match format {
        Format::Zero { width, positions } => {
            let total = (data.len() * 8 / (*width).max(1)) as u64;
            if samples > total {
                return Err(Error::Value(format!(
                    "{samples} samples of {width} bits need {} bytes, got {}",
                    (samples * *width as u64).div_ceil(8),
                    data.len()
                )));
            }
            Ok(names
                .iter()
                .enumerate()
                .map(|(k, name)| Chan::Bits {
                    name: name.clone(),
                    data: Data::Mem(blocks::column(
                        data,
                        *width,
                        positions.get(k).copied().unwrap_or(k),
                        samples as usize,
                    )),
                    n: samples,
                    step: 1,
                    pin: pin(k),
                })
                .collect())
        }
        Format::One(b) => {
            let cols = b.columns(data, samples as usize)?;
            Ok(names
                .iter()
                .zip(cols)
                .enumerate()
                .map(|(k, (name, (_, col)))| match col {
                    Column::Raw(mut bits) => {
                        bits.truncate(samples.div_ceil(8) as usize);
                        Chan::Bits {
                            name: name.clone(),
                            data: Data::Mem(bits),
                            n: samples,
                            step: 1,
                            pin: pin(k),
                        }
                    }
                    Column::Reduced(rc) if rc.kind == blocks::SAMPLE => Chan::Bits {
                        name: name.clone(),
                        data: Data::Mem(rc.data),
                        n: rc.n as u64,
                        step: rc.d as u64,
                        pin: pin(k),
                    },
                    Column::Reduced(rc) => Chan::Reduced {
                        name: name.clone(),
                        kind: rc.kind,
                        data: rc.data,
                        n: rc.n as u64,
                        step: rc.d as u64,
                        start_level: rc.start_level,
                        pin: pin(k),
                    },
                })
                .collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(position: u64, samples: u64, start_ns: u64) -> SegmentInfo {
        SegmentInfo {
            position,
            samples,
            start_ns,
            start_uncertainty_ns: 5,
        }
    }

    fn spool(stop: bool) -> Spool {
        Spool::new(
            vec!["A".into(), "B".into()],
            vec![10, 11],
            (1_000_000, 1),
            Format::Zero {
                width: 8,
                positions: vec![0, 1],
            },
            stop,
            None,
        )
    }

    #[test]
    fn a_stream_without_a_gap_is_one_part() {
        let mut s = spool(true);
        let data: Vec<u8> = (0..100u8).collect();
        s.segment(info(0, 50, 1000));
        s.run(0, &data[..50]).unwrap();
        s.work(false, None).unwrap();
        s.segment(info(50, 50, 51_000));
        s.run(50, &data[50..]).unwrap();
        s.finish(None).unwrap();
        let (parts, lost) = s.take();
        assert!(lost.is_empty());
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].samples, Some(100));
        let Chan::Bits {
            data: Data::Mem(a), n, ..
        } = &parts[0].channels[0]
        else {
            panic!()
        };
        assert_eq!(*n, 100);
        assert_eq!(a.len(), 13);
        assert_eq!(
            (0..100).map(|i| blocks::bit(a, i)).collect::<Vec<_>>(),
            (0..100).map(|i| (i & 1) as u8).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_skipped_position_is_a_loss_and_ends_the_part() {
        for stop in [true, false] {
            let mut s = spool(stop);
            s.segment(info(0, 40, 0));
            s.run(0, &[1u8; 40]).unwrap();
            s.segment(info(80, 40, 80_000));
            s.run(80, &[0u8; 40]).unwrap();
            s.finish(None).unwrap();
            let (parts, lost) = s.take();
            assert_eq!(lost.len(), 1);
            assert_eq!(lost[0].after_part, 0);
            assert_eq!(lost[0].stopped, stop);
            if stop {
                assert_eq!(parts.len(), 1);
            } else {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[1].tick0, Some(80));
                assert_eq!(lost[0].ticks, Some(40));
            }
        }
    }

    #[test]
    fn rounds_as_python_does() {
        assert_eq!(round_div(5, 2), 2);
        assert_eq!(round_div(7, 2), 4);
        assert_eq!(round_div(-5, 2), -2);
        assert_eq!(round_div(10, 3), 3);
    }
}
