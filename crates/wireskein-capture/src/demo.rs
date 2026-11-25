//! The demo device (`demo:`): a logic analyser with no hardware, sending a known waveform as raw packed samples, so
//! the driver's interface and the shared flow - the conversion of packed samples, the budget, the losses, the files
//! - are tried without a device, and a new driver has an example to follow.
//!
//! Its lines D0..D{n-1} are the bits of a counter of samples: line k is bit k of the sample's index since the start,
//! a square wave of period 2^(k+1) samples; all lines are sent, packed in the fewest whole bytes (1, 2 or 4), as
//! WeAct's logic analysers and most others send them. The address is `[NAME][?OPTION&...]`:
//!
//! - `lines=N` (16): 1 to 32 lines
//! - `type=stream` (the default: it sends samples as it takes them, through its own buffer, dropping what the host
//!   is too late for) or `type=memory` (it keeps them in its memory, then is read out at the host's pace)
//! - `max_rate=HZ` (200M), `link=BYTES_A_SECOND` (400M: what its link carries; streaming, the rate is limited by it
//!   for the lines' packing), `memory=BYTES` (64M), `buffer=BYTES` (1M: a streaming device's own buffer)
//! - `pace=real` (the default: samples come at the rate asked) or `pace=fast` (as fast as the host takes them),
//!   `read=BYTES_A_SECOND` (a memory read out this fast; 0, the default: as fast as the host takes it)
//! - `chunk=BYTES` (64k): what one transfer brings
//! - on purpose: `drop=K[,K...]` the device drops those chunks (a loss), `stall=MS` the spool's first piece of work
//!   held that long (a disk that stood still), `fail=K` the device stops in error after chunk K,
//!   `firmware=FILE` the device needs FILE sent at each open (found as a vendor's file would be: `find_firmware`),
//!   `budget=MIB` the stream's budget (else `WIRESKEIN_CAPTURE_FLOW`'s)

use crate::driver::{
    self, Caps, Ctl, Device, Driver, FirmwareNeed, Found, Hold, LineCap, MemoryCaps, Open, Progress, RateLimit, Stage,
    StreamCaps, Thresholds,
};
use crate::error::{Error, Result};
use crate::flow::{Flow, Shape};
use crate::packed::Packing;
use crate::parts::{Acquired, Loss, Part};
use crate::request::{Mode, Request};
use crate::spool::{Format, Spool, SpoolWorker};
use crate::stream::{self, Chunk, Inlet, Until};
use serde_json::{Map, Value, json};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub struct Demo;

/// How the demo device is set (its address's options).
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    pub name: String,
    pub lines: usize,
    pub memory_type: bool,
    pub max_rate: u64,
    pub link: u64,
    pub memory: u64,
    pub buffer: u64,
    pub real_pace: bool,
    pub read: u64,
    pub chunk: usize,
    pub drop: Vec<u64>,
    pub stall_ms: Option<u64>,
    pub fail: Option<u64>,
    pub firmware: Option<String>,
    /// the stream's budget in MiB (None: WIRESKEIN_CAPTURE_FLOW's or the default)
    pub budget_mib: Option<u64>,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            name: "counter".into(),
            lines: 16,
            memory_type: false,
            max_rate: 200_000_000,
            link: 400_000_000,
            memory: 64 << 20,
            buffer: 1 << 20,
            real_pace: true,
            read: 0,
            chunk: 64 << 10,
            drop: vec![],
            stall_ms: None,
            fail: None,
            firmware: None,
            budget_mib: None,
        }
    }
}

fn count(k: &str, v: &str) -> Result<u64> {
    let (num, mul) = match v.chars().last() {
        Some('k' | 'K') => (&v[..v.len() - 1], 1e3),
        Some('M') => (&v[..v.len() - 1], 1e6),
        Some('G') => (&v[..v.len() - 1], 1e9),
        _ => (v, 1.0),
    };
    num.parse::<f64>()
        .ok()
        .filter(|x| *x >= 0.0)
        .map(|x| (x * mul).round() as u64)
        .ok_or_else(|| Error::Value(format!("demo: {k}={v}: a count (k, M, G)")))
}

impl Options {
    pub fn parse(address: &str) -> Result<Options> {
        let mut o = Options::default();
        let (name, query) = address.split_once('?').unwrap_or((address, ""));
        if !name.is_empty() {
            if name != "counter" {
                return Err(Error::Value(format!(
                    "demo:{name}: the demo device's waveform is `counter`"
                )));
            }
            o.name = name.into();
        }
        for w in query.split('&').filter(|w| !w.is_empty()) {
            let (k, v) = w.split_once('=').unwrap_or((w, ""));
            match k {
                "lines" => o.lines = count(k, v)? as usize,
                "type" => {
                    o.memory_type = match v {
                        "stream" => false,
                        "memory" => true,
                        _ => return Err(Error::Value(format!("demo: type={v}: stream or memory"))),
                    }
                }
                "max_rate" => o.max_rate = count(k, v)?,
                "link" => o.link = count(k, v)?,
                "memory" => o.memory = count(k, v)?,
                "buffer" => o.buffer = count(k, v)?,
                "pace" => {
                    o.real_pace = match v {
                        "real" => true,
                        "fast" => false,
                        _ => return Err(Error::Value(format!("demo: pace={v}: real or fast"))),
                    }
                }
                "read" => o.read = count(k, v)?,
                "chunk" => o.chunk = count(k, v)?.max(64) as usize,
                "drop" => o.drop = v.split(',').map(|x| count(k, x)).collect::<Result<Vec<_>>>()?,
                "stall" => o.stall_ms = Some(count(k, v)?),
                "fail" => o.fail = Some(count(k, v)?),
                "firmware" => o.firmware = Some(v.to_string()),
                "budget" => o.budget_mib = Some(count(k, v)?),
                _ => {
                    return Err(Error::Value(format!(
                        "demo: {k}: not an option (lines, type, max_rate, link, memory, buffer, pace, read, chunk, \
                         drop, stall, fail, firmware, budget)"
                    )));
                }
            }
        }
        if !(1..=32).contains(&o.lines) {
            return Err(Error::Value(format!("demo: lines={}: 1 to 32", o.lines)));
        }
        Ok(o)
    }

    pub fn packing(&self) -> Packing {
        Packing::bytes_for(self.lines)
    }

    pub fn caps(&self) -> Caps {
        let lines: Vec<LineCap> = (0..self.lines)
            .map(|k| LineCap {
                id: format!("D{k}"),
                analog: false,
            })
            .collect();
        // all lines are always sent, in the device's packing: the link bounds a stream's rate by it
        let width = self.packing().width as u64;
        let limits = vec![RateLimit {
            lines: self.lines,
            max_hz: (self.link * 8 / width).min(self.max_rate),
        }];
        let memory_limits = vec![RateLimit {
            lines: self.lines,
            max_hz: self.max_rate,
        }];
        Caps {
            lines,
            rates: vec![],
            stream: (!self.memory_type).then_some(StreamCaps {
                limits,
                bytes_per_s: Some(self.link),
                buffer_bytes: Some(self.buffer),
            }),
            memory: self.memory_type.then(|| MemoryCaps {
                limits: memory_limits,
                bytes: self.memory,
                sample_bits: Some(self.packing().width),
                pretrigger: true,
            }),
            modes: if self.memory_type {
                vec![Mode::OneShot, Mode::Repeat]
            } else {
                vec![Mode::OneShot, Mode::Stream, Mode::Repeat]
            },
            triggers: if self.memory_type {
                vec!["rise".into(), "fall".into(), "high".into(), "low".into()]
            } else {
                vec![]
            },
            thresholds: Thresholds::Range {
                min: 0.0,
                max: 5.0,
                step: 0.1,
            },
            reductions: vec![],
            firmware: self
                .firmware
                .iter()
                .map(|f| FirmwareNeed {
                    file: f.clone(),
                    what: "the demo device's firmware".into(),
                    from: "it comes with nothing; any file by that name will do (a test's)".into(),
                    vendor_dirs: vec![],
                })
                .collect(),
            extra: Map::new(),
        }
    }
}

/// Sample `i`'s value: the counter.
#[inline]
fn sample(i: u64) -> u32 {
    i as u32
}

/// Samples [first, first + n) packed in `b` bytes each.
pub fn counter_bytes(first: u64, n: usize, b: usize) -> Vec<u8> {
    let mut out = vec![0u8; n * b];
    match b {
        1 => {
            for (j, o) in out.iter_mut().enumerate() {
                *o = sample(first + j as u64) as u8;
            }
        }
        2 => {
            for (j, o) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                o.copy_from_slice(&(sample(first + j as u64) as u16).to_le_bytes());
            }
        }
        _ => {
            for (j, o) in out.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                o.copy_from_slice(&sample(first + j as u64).to_le_bytes());
            }
        }
    }
    out
}

impl Driver for Demo {
    fn kind(&self) -> &'static str {
        "demo"
    }
    fn about(&self) -> &'static str {
        "a logic analyser with no hardware sending a counter as packed samples: demo:[counter][?lines=16&type=stream|\
         memory&pace=real|fast&drop=K&stall=MS&fail=K&firmware=FILE...]"
    }
    fn list(&self) -> Result<Vec<Found>> {
        let mut facts = Map::new();
        facts.insert("model".into(), json!("demo counter"));
        Ok(vec![Found {
            source: "demo:counter".into(),
            label: "the demo device (a counter, no hardware)".into(),
            facts,
        }])
    }
    fn open(&self, address: &str, how: &Open) -> Result<Box<dyn Device>> {
        let o = Options::parse(address)?;
        let mut firmware = None;
        for need in o.caps().firmware {
            firmware = Some(driver::find_firmware("demo", &need)?);
        }
        Ok(Box::new(DemoDevice {
            o,
            address: address.into(),
            owner: how.owner.clone(),
            firmware,
        }))
    }
}

pub struct DemoDevice {
    o: Options,
    address: String,
    owner: String,
    firmware: Option<PathBuf>,
}

/// A take's parts, losses, what the IN side did, the flow's counts and how many times it held back.
type Taken = (Vec<Part>, Vec<Loss>, Sent, Arc<Flow>, u64);

/// What the device's IN side did.
#[derive(Debug, Default, Clone)]
struct Sent {
    dropped_bytes: u64,
    dropped_chunks: u64,
    failed_at: Option<u64>,
    held_most: usize,
    /// what the reader took, and how long it read
    read_bytes: u64,
    read_s: f64,
}

impl DemoDevice {
    fn positions(&self, req: &Request) -> Vec<usize> {
        req.channels
            .iter()
            .map(|(_, id)| id[1..].parse::<usize>().expect("checked"))
            .collect()
    }

    fn flow(&self) -> Arc<Flow> {
        let mut shape = Shape::from_env();
        if let Some(m) = self.o.budget_mib {
            shape.budget = m << 20;
        }
        let flow = Arc::new(Flow::new(shape));
        if let Some(ms) = self.o.stall_ms {
            *flow.stall.lock().expect("stall") = Some(Duration::from_millis(ms));
        }
        flow
    }

    /// The IN side of a device sending `total` bytes (None: until stopped) from sample `first`, as it takes them:
    /// at the rate asked (pace real) through its own buffer, dropping what does not fit; a memory read out waits for
    /// the host instead.
    fn send(&self, inlet: Inlet, first: u64, total: Option<u64>, bytes_per_s: f64, waits: bool) -> Sent {
        let b = self.o.packing().width / 8;
        let chunk = self.o.chunk / b * b;
        let mut buf: VecDeque<Chunk> = VecDeque::new();
        let mut in_buf = 0u64;
        let mut sent = Sent::default();
        let t0 = Instant::now();
        let mut at = 0u64;
        let mut k = 0u64;
        loop {
            if inlet.stopping() || total.is_some_and(|t| at >= t) {
                break;
            }
            let n = match total {
                Some(t) => (t - at).min(chunk as u64) as usize,
                None => chunk,
            };
            if bytes_per_s > 0.0 {
                let due = t0 + Duration::from_secs_f64((at + n as u64) as f64 / bytes_per_s);
                let now = Instant::now();
                if due > now {
                    std::thread::sleep(due - now);
                }
            }
            let c = Chunk {
                at,
                data: counter_bytes(first + at / b as u64, n / b, b),
            };
            at += n as u64;
            let fail = self.o.fail.is_some_and(|f| k >= f);
            if self.o.drop.contains(&k) {
                sent.dropped_bytes += n as u64;
                sent.dropped_chunks += 1;
            } else if waits {
                if !inlet.put(c) {
                    break;
                }
            } else if in_buf + n as u64 > self.o.buffer {
                // no room on the device: dropped whole (the next position shows the gap)
                sent.dropped_bytes += n as u64;
                sent.dropped_chunks += 1;
            } else {
                in_buf += n as u64;
                buf.push_back(c);
            }
            while let Some(c) = buf.pop_front() {
                let len = c.data.len() as u64;
                match inlet.try_put(c) {
                    Ok(()) => in_buf -= len,
                    Err(c) => {
                        buf.push_front(c);
                        break;
                    }
                }
            }
            sent.held_most = sent.held_most.max(buf.len());
            k += 1;
            if fail {
                sent.failed_at = Some(at);
                break;
            }
        }
        // what its buffer still holds, while the host takes it
        while let Some(c) = buf.pop_front() {
            if !inlet.put(c) {
                break;
            }
        }
        sent
    }

    fn base_meta(&self, req: &Request, flow: &Flow, held: u64, sent: &Sent) -> Map<String, Value> {
        let mut meta = Map::new();
        let pc: Map<String, Value> = req.channels.iter().map(|(n, id)| (n.clone(), json!(id))).collect();
        meta.insert("probe_channels".into(), Value::Object(pc));
        let mut probe = self.facts();
        let mut link = Map::new();
        link.insert("flow".into(), flow.report());
        link.insert("held".into(), json!(held));
        link.insert("open_s".into(), json!(0.0));
        link.insert("read_bytes".into(), json!(sent.read_bytes));
        link.insert("read_s".into(), json!((sent.read_s * 1000.0).round() / 1000.0));
        probe.insert("link".into(), Value::Object(link));
        probe.insert(
            "drops".into(),
            json!({"dropped_bytes": sent.dropped_bytes, "dropped_chunks": sent.dropped_chunks,
                   "buffer_chunks_max": sent.held_most}),
        );
        if let Some(v) = req.threshold_v {
            probe.insert("threshold_v".into(), json!(v));
        }
        meta.insert("probe".into(), Value::Object(probe));
        meta
    }

    /// Samples [first, first + samples) taken by streaming them (a stream device's one-shot or repeat segment) or
    /// read out of its memory; in memory.
    fn take(&self, req: &Request, ctl: &Ctl, first: u64, samples: u64) -> Result<Taken> {
        let p = self.o.packing();
        let total = p.bytes(samples);
        let flow = self.flow();
        let (inlet, outlet) = stream::pipe(flow.clone());
        let held = inlet.held.clone();
        let spool = SpoolWorker::start_with(
            Spool::new(
                req.channels.iter().map(|(n, _)| n.clone()).collect(),
                vec![],
                (req.rate_hz, 1),
                Format::Packed {
                    width: p.width,
                    positions: self.positions(req),
                },
                req.on_loss_stop,
                None,
            ),
            flow.clone(),
        );
        let (pace, waits, stage) = if self.o.memory_type {
            (self.o.read as f64, true, Stage::Reading)
        } else if self.o.real_pace {
            (req.rate_hz as f64 * (p.width / 8) as f64, false, Stage::Taking)
        } else {
            // as fast as the host takes them: nothing to drop
            (0.0, true, Stage::Taking)
        };
        let me = DemoDevice {
            o: self.o.clone(),
            address: self.address.clone(),
            owner: self.owner.clone(),
            firmware: self.firmware.clone(),
        };
        let th = std::thread::spawn(move || me.send(inlet, first, Some(total), pace, waits));
        let until = Until {
            deadline: None,
            of_bytes: Some(total),
            stage,
            on_loss_stop: req.on_loss_stop,
        };
        let got = stream::collect(&outlet, spool, &until, ctl, || None);
        outlet.stop();
        let mut sent = th
            .join()
            .map_err(|_| Error::Runtime("demo: its IN side panicked".into()))?;
        let got = got?;
        sent.read_bytes = got.bytes;
        sent.read_s = got.read_s;
        let mut lost = got.lost;
        if let Some(at) = sent.failed_at {
            lost.push(Loss::new(
                got.parts.len() as i64 - 1,
                format!("the device stopped in error after {at} bytes (fail=)"),
                true,
            ));
        } else if got.end.is_some_and(|e| e < total) && !got.stopped && !lost.iter().any(|l| l.stopped) {
            lost.push(Loss::new(
                got.parts.len() as i64 - 1,
                format!("the last {} bytes did not arrive", total - got.end.unwrap_or(0)),
                true,
            ));
        }
        Ok((got.parts, lost, sent, flow, held.load(Ordering::Relaxed)))
    }

    /// A memory device's wait while it takes the samples into its memory (the trigger found, the samples after it).
    fn fill(&self, req: &Request, ctl: &Ctl, samples: u64) {
        if !self.o.real_pace || req.rate_hz == 0 {
            return;
        }
        let t0 = Instant::now();
        let secs = samples as f64 / req.rate_hz as f64;
        while t0.elapsed().as_secs_f64() < secs && !ctl.stopped() {
            ctl.tell(&Progress {
                stage: Stage::Taking,
                elapsed_s: t0.elapsed().as_secs_f64(),
                bytes: 0,
                of_bytes: None,
                backlog: 0,
                losses: 0,
            });
            std::thread::sleep(Duration::from_secs_f64(
                (secs - t0.elapsed().as_secs_f64()).clamp(0.0, 0.05),
            ));
        }
    }

    /// Where a memory device's trigger falls: the first sample from `pretrigger` on that meets it (the counter's
    /// line k changes every 2^k samples).
    fn trigger_at(&self, req: &Request, from: u64) -> Result<u64> {
        let Some((name, kind)) = &req.trigger else {
            return Ok(from);
        };
        let id = &req.channels.iter().find(|(n, _)| n == name).expect("checked").1;
        let k: u32 = id[1..].parse().expect("checked");
        let level = |i: u64| (sample(i) >> k) & 1;
        // line k is m & 1 from sample m x 2^k on: it rises at odd m, falls at even m
        let period = 1u64 << k;
        let m0 = from.div_ceil(period);
        let odd = |m: u64| if m.is_multiple_of(2) { m + 1 } else { m };
        let even = |m: u64| {
            if m.is_multiple_of(2) && m > 0 {
                m
            } else if m == 0 {
                2
            } else {
                m + 1
            }
        };
        Ok(match kind.as_str() {
            "high" if level(from) == 1 => from,
            "low" if level(from) == 0 => from,
            "high" | "rise" => odd(m0) * period,
            "low" | "fall" => even(m0) * period,
            _ => return Err(Error::Value(format!("trigger {name}:{kind}"))),
        })
    }

    fn oneshot(&self, req: &Request, ctl: &Ctl) -> Result<Acquired> {
        let pre = req.pretrigger.unwrap_or(0);
        let at = self.trigger_at(req, pre)?;
        let first = at - pre.min(at);
        if self.o.memory_type {
            self.fill(req, ctl, req.samples);
        }
        let (parts, lost, sent, flow, held) = self.take(req, ctl, first, req.samples)?;
        if parts.is_empty() {
            return Err(Error::Runtime("demo: the capture brought no data".into()));
        }
        let mut meta = self.base_meta(req, &flow, held, &sent);
        if req.trigger.is_some() {
            meta.insert("trigger_sample".into(), json!(at - first));
        }
        // the counter's value at the capture's first sample (a test's check)
        meta.insert("counter_start".into(), json!(first));
        if lost.is_empty() && parts.len() == 1 {
            let p = &parts[0];
            return Ok(Acquired {
                tick_hz: (req.rate_hz, 1),
                channels: p.channels.clone(),
                meta,
                parts: None,
                lost,
                layout: "timeline".into(),
                temp_dir: None,
                samples: p.samples,
            });
        }
        Ok(Acquired {
            tick_hz: (req.rate_hz, 1),
            channels: parts[0].channels.clone(),
            meta,
            parts: Some(parts),
            lost,
            layout: "timeline".into(),
            temp_dir: None,
            samples: None,
        })
    }

    /// `segments` acquisitions one after another on a timeline, each `samples` long, a re-arming gap as long between
    /// them (a memory device: all kept, then read out).
    fn repeat(&self, req: &Request, ctl: &Ctl) -> Result<Acquired> {
        let n = req.segments.unwrap_or(1);
        let mut parts = vec![];
        let mut lost = vec![];
        let mut last = None;
        for s in 0..n {
            if ctl.stopped() {
                break;
            }
            let first = s * 2 * req.samples;
            let (ps, ls, sent, flow, held) = self.take(req, ctl, first, req.samples)?;
            let base = parts.len() as i64;
            for mut p in ps {
                p.tick0 = Some(first as i64 + p.tick0.unwrap_or(0));
                parts.push(p);
            }
            for mut l in ls {
                l.after_part += base;
                lost.push(l);
            }
            last = Some((sent, flow, held));
            if req.on_loss_stop && !lost.is_empty() {
                break;
            }
        }
        let (sent, flow, held) = last.ok_or_else(|| Error::Runtime("demo: stopped before any segment".into()))?;
        if parts.is_empty() {
            return Err(Error::Runtime("demo: the capture brought no data".into()));
        }
        let meta = self.base_meta(req, &flow, held, &sent);
        Ok(Acquired {
            tick_hz: (req.rate_hz, 1),
            channels: parts[0].channels.clone(),
            meta,
            parts: Some(parts),
            lost,
            layout: "timeline".into(),
            temp_dir: None,
            samples: None,
        })
    }

    fn stream(&self, req: &Request, ctl: &Ctl) -> Result<Acquired> {
        let p = self.o.packing();
        let base = req.spool_dir.clone().unwrap_or_else(std::env::temp_dir);
        let dir = stream::temp_dir(&base, ".wireskein-demo-stream-")?;
        let out = self.stream_into(req, ctl, &p, &dir);
        if out.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        out
    }

    fn stream_into(&self, req: &Request, ctl: &Ctl, p: &Packing, dir: &std::path::Path) -> Result<Acquired> {
        let flow = self.flow();
        let (inlet, outlet) = stream::pipe(flow.clone());
        let held = inlet.held.clone();
        let spool = SpoolWorker::start_with(
            Spool::new(
                req.channels.iter().map(|(n, _)| n.clone()).collect(),
                vec![],
                (req.rate_hz, 1),
                Format::Packed {
                    width: p.width,
                    positions: self.positions(req),
                },
                req.on_loss_stop,
                Some(dir),
            ),
            flow.clone(),
        );
        let bps = req.rate_hz as f64 * (p.width / 8) as f64;
        let duration = req.duration_s.unwrap_or(0.0);
        // pace fast: as many bytes as the duration holds, as fast as they are taken
        let (total, pace) = if self.o.real_pace {
            (None, bps)
        } else {
            (Some(p.bytes((duration * req.rate_hz as f64) as u64)), 0.0)
        };
        let me = DemoDevice {
            o: self.o.clone(),
            address: self.address.clone(),
            owner: self.owner.clone(),
            firmware: self.firmware.clone(),
        };
        let waits = !self.o.real_pace;
        let th = std::thread::spawn(move || me.send(inlet, 0, total, pace, waits));
        let until = Until {
            deadline: self
                .o
                .real_pace
                .then(|| Instant::now() + Duration::from_secs_f64(duration)),
            of_bytes: Some(p.bytes((duration * req.rate_hz as f64) as u64)),
            stage: Stage::Taking,
            on_loss_stop: req.on_loss_stop,
        };
        let got = stream::collect(&outlet, spool, &until, ctl, || None);
        outlet.stop();
        let mut sent = th
            .join()
            .map_err(|_| Error::Runtime("demo: its IN side panicked".into()))?;
        let got = got?;
        sent.read_bytes = got.bytes;
        sent.read_s = got.read_s;
        let mut lost = got.lost;
        if got.parts.is_empty() {
            return Err(Error::Runtime(format!(
                "the stream brought no data{}",
                if lost.is_empty() {
                    ""
                } else {
                    " (the device dropped it all)"
                }
            )));
        }
        if let Some(at) = sent.failed_at {
            lost.push(Loss::new(
                got.parts.len() as i64 - 1,
                format!("the device stopped in error after {at} bytes (fail=)"),
                true,
            ));
        }
        let mut meta = self.base_meta(req, &flow, held.load(Ordering::Relaxed), &sent);
        meta.insert(
            "stream".into(),
            json!({"duration_s": req.duration_s, "on_loss": if req.on_loss_stop { "stop" } else { "continue" },
                   "frames": got.chunks, "bytes": got.bytes, "read_s": (got.read_s * 1000.0).round() / 1000.0,
                   "stopped": got.stopped}),
        );
        Ok(Acquired {
            tick_hz: (req.rate_hz, 1),
            channels: got.parts[0].channels.clone(),
            meta,
            parts: Some(got.parts),
            lost,
            layout: "timeline".into(),
            temp_dir: Some(dir.to_path_buf()),
            samples: None,
        })
    }
}

impl Device for DemoDevice {
    fn facts(&self) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("kind".into(), json!("demo"));
        m.insert("model".into(), json!(format!("demo {}", self.o.name)));
        m.insert("address".into(), json!(format!("demo:{}", self.address)));
        m.insert("lines".into(), json!(self.o.lines));
        m.insert(
            "type".into(),
            json!(if self.o.memory_type { "memory" } else { "stream" }),
        );
        if let Some(f) = &self.firmware {
            m.insert(
                "firmware".into(),
                json!(f.file_name().map(|n| n.to_string_lossy().to_string())),
            );
        }
        m
    }
    fn hold(&self) -> Hold {
        Hold::None
    }
    fn caps(&mut self) -> Result<Caps> {
        Ok(self.o.caps())
    }
    fn capture(&mut self, req: &Request, ctl: &Ctl) -> Result<Acquired> {
        self.o.caps().check(req)?;
        if req.rate_hz == 0 {
            return Err(Error::Value("demo: a rate is needed".into()));
        }
        match req.mode {
            Mode::OneShot => self.oneshot(req, ctl),
            Mode::Repeat => self.repeat(req, ctl),
            Mode::Stream => self.stream(req, ctl),
        }
    }
    fn close(self: Box<Self>) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packed::bit;
    use crate::parts::{Chan, Data};
    use crate::source::Registry;
    use std::sync::Mutex;

    fn reg() -> Registry {
        Registry::new().with(Box::new(Demo))
    }

    fn req(lines: &[usize], rate: u64, samples: u64) -> Request {
        Request::new(
            lines.iter().map(|k| (format!("L{k}"), format!("D{k}"))).collect(),
            rate,
            samples,
        )
    }

    fn cap(source: &str, r: &Request) -> Result<Acquired> {
        reg().capture(source, r, &Open::new("test"), &Ctl::new())
    }

    /// Every part's lines are the counter's bits from its tick0 on (line Dk: bit k of the sample's index).
    fn check_counter(a: &Acquired, lines: &[usize]) -> u64 {
        let mut total = 0;
        let start = a.meta.get("counter_start").and_then(|v| v.as_u64()).unwrap_or(0);
        for p in a.all_parts() {
            let t0 = start + p.tick0.unwrap_or(0) as u64;
            for (c, &k) in p.channels.iter().zip(lines) {
                let Chan::Bits { data, n, .. } = c else { panic!("bits") };
                let bytes = match data {
                    Data::Mem(b) => b.clone(),
                    Data::File(f) => std::fs::read(f).unwrap(),
                };
                for i in 0..*n as usize {
                    let want = ((t0 + i as u64) >> k) as u8 & 1;
                    assert_eq!(bit(&bytes, i), want, "line D{k} at {}", t0 + i as u64);
                }
            }
            total += p.channels[0].end();
        }
        total
    }

    fn drops(a: &Acquired) -> u64 {
        a.meta["probe"]["drops"]["dropped_bytes"].as_u64().unwrap()
    }

    #[test]
    fn a_one_shot_is_the_counter_in_every_packing() {
        for (n, lines) in [(8usize, vec![0, 7, 3]), (16, vec![15, 0, 9]), (32, vec![31, 16, 2])] {
            let r = req(&lines, 1_000_000, 100_003);
            let a = cap(&format!("demo:counter?lines={n}&pace=fast&chunk=4096"), &r).unwrap();
            assert!(a.lost.is_empty() && a.parts.is_none());
            assert_eq!(check_counter(&a, &lines), 100_003);
            assert_eq!(a.meta["probe"]["type"], "stream");
        }
    }

    #[test]
    fn a_memory_device_keeps_then_is_read_out_with_its_trigger_and_pretrigger() {
        let heard = Arc::new(Mutex::new(vec![]));
        let h = heard.clone();
        let ctl = Ctl::new().hearing(move |p| h.lock().unwrap().push(p.stage));
        let mut r = req(&[4, 0], 1_000_000, 50_000);
        r.trigger = Some(("L4".into(), "fall".into()));
        r.pretrigger = Some(10);
        let a = reg()
            .capture(
                "demo:?type=memory&lines=8&read=20M&chunk=8192",
                &r,
                &Open::new("t"),
                &ctl,
            )
            .unwrap();
        assert!(a.lost.is_empty());
        assert_eq!(a.meta["trigger_sample"], 10);
        check_counter(&a, &[4, 0]);
        // sample 10 is the trigger's: D4 falls there (from 1 to 0)
        let Chan::Bits { data: Data::Mem(b), .. } = &a.channels[0] else {
            panic!()
        };
        assert_eq!((bit(b, 9), bit(b, 10)), (1, 0));
        let stages = heard.lock().unwrap().clone();
        assert!(
            stages.contains(&Stage::Taking) && stages.contains(&Stage::Reading),
            "{stages:?}"
        );
        // what a memory device cannot do
        let mut s = req(&[0], 1_000_000, 0);
        s.mode = Mode::Stream;
        s.duration_s = Some(1.0);
        let e = cap("demo:?type=memory", &s).unwrap_err();
        assert!(e.to_string().contains("mode stream"), "{e}");
        let big = req(&[0], 1_000_000, 70_000_000);
        let e = cap("demo:?type=memory&lines=8", &big).unwrap_err();
        assert!(e.to_string().contains("keeps 67108864 at most"), "{e}");
    }

    #[test]
    fn caps_say_both_shapes_and_their_limits() {
        let mut d = Demo.open("?lines=16", &Open::new("t")).unwrap();
        let c = d.caps().unwrap();
        assert_eq!(c.max_rate(16, true), Some(200_000_000));
        assert!(c.memory.is_none() && c.firmware.is_empty());
        let mut d = Demo.open("?lines=32&link=400M", &Open::new("t")).unwrap();
        assert_eq!(d.caps().unwrap().max_rate(3, true), Some(100_000_000)); // 4 bytes a sample
        let e = cap("demo:?lines=32", &req(&[0], 150_000_000, 10)).unwrap_err();
        assert!(e.to_string().ends_with("takes 100000000 Hz at most"), "{e}");
        let mut d = Demo.open("?type=memory&memory=1M&lines=16", &Open::new("t")).unwrap();
        let c = d.caps().unwrap();
        assert_eq!(c.memory.as_ref().unwrap().samples_for(16), 500_000);
        assert!(c.to_json()["memory"]["pretrigger"].as_bool().unwrap());
        let e = cap("demo:?lines=4", &req(&[5], 1000, 10)).unwrap_err();
        assert!(e.to_string().contains("no such line"), "{e}");
    }

    #[test]
    fn a_firmware_not_there_says_where_it_looked_and_one_there_is_used() {
        let dir = std::env::temp_dir().join(format!("wsk-demo-fw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: the only test reading this variable
        unsafe { std::env::set_var("WIRESKEIN_FIRMWARE_DEMO", &dir) };
        let Err(e) = Demo.open("?firmware=demo-fpga.bin", &Open::new("t")) else {
            panic!()
        };
        let text = e.to_string();
        assert!(matches!(e, Error::NotFound(_)), "{text}");
        assert!(
            text.contains("demo-fpga.bin") && text.contains(&dir.display().to_string()),
            "{text}"
        );
        assert!(text.contains("WIRESKEIN_FIRMWARE_DEMO"), "{text}");
        std::fs::write(dir.join("demo-fpga.bin"), b"x").unwrap();
        let d = Demo.open("?firmware=demo-fpga.bin", &Open::new("t")).unwrap();
        assert_eq!(d.facts()["firmware"], "demo-fpga.bin");
        unsafe { std::env::remove_var("WIRESKEIN_FIRMWARE_DEMO") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dropped_chunk_is_a_loss_stop_or_continue() {
        let r = req(&[0, 5], 1_000_000, 64 * 1024);
        let mut stop = r.clone();
        stop.on_loss_stop = true;
        let a = cap("demo:?lines=8&pace=fast&chunk=4096&drop=3", &stop).unwrap();
        assert_eq!(a.lost.len(), 1);
        assert!(a.lost[0].stopped);
        assert_eq!(check_counter(&a, &[0, 5]), 3 * 4096);
        let mut go = r;
        go.on_loss_stop = false;
        let a = cap("demo:?lines=8&pace=fast&chunk=4096&drop=3,7", &go).unwrap();
        assert_eq!(a.lost.len(), 2);
        assert!(a.lost.iter().all(|l| !l.stopped));
        assert_eq!(a.lost[0].ticks, Some(4096));
        let parts = a.parts.as_ref().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[1].tick0, Some(4 * 4096));
        assert_eq!(check_counter(&a, &[0, 5]), 64 * 1024 - 2 * 4096);
        assert_eq!(drops(&a), 2 * 4096);
    }

    #[test]
    fn a_device_that_stops_in_error_keeps_what_came() {
        let mut r = req(&[1], 1_000_000, 64 * 1024);
        r.on_loss_stop = false;
        let a = cap("demo:?lines=8&pace=fast&chunk=4096&fail=4", &r).unwrap();
        assert_eq!(a.lost.len(), 1);
        assert!(a.lost[0].stopped && a.lost[0].reason.contains("stopped in error"));
        assert_eq!(check_counter(&a, &[1]), 5 * 4096);
    }

    fn stream_req(lines: &[usize], rate: u64, secs: f64, dir: &std::path::Path) -> Request {
        let mut r = req(lines, rate, 0);
        r.mode = Mode::Stream;
        r.duration_s = Some(secs);
        r.on_loss_stop = false;
        r.spool_dir = Some(dir.to_path_buf());
        r
    }

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("wsk-demo-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_stalled_spool_does_not_stop_the_in_side() {
        // 8 MB/s for 1.5 s; the spool's first piece of work held 0.8 s: within the budget nothing is dropped
        let dir = scratch("stall");
        let r = stream_req(&[0, 3, 7], 8_000_000, 1.5, &dir);
        let a = cap("demo:?lines=8&stall=800&budget=64", &r).unwrap();
        assert!(a.lost.is_empty(), "{:?}", a.lost);
        assert_eq!(drops(&a), 0);
        let flow = &a.meta["probe"]["link"]["flow"];
        assert!(flow["backlog_max"].as_u64().unwrap() > 4 << 20, "{flow}");
        assert!(flow["work_max_ms"].as_f64().unwrap() >= 800.0, "{flow}");
        let n = check_counter(&a, &[0, 3, 7]);
        assert!(n > 10_000_000, "{n}");
        // written out and read back: the counter's changes
        let out = dir.join("s.wireskein");
        crate::save::save(&a, &out, &Map::new()).unwrap();
        let back = wireskein_io::read(&out).unwrap();
        let l = &back.parts[0].lines[1];
        assert_eq!(l.name, "L3");
        assert_eq!(&l.edges[..3], &[8, 16, 24]);
        assert!(a.temp_dir.as_ref().is_none_or(|d| !d.exists()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stall_past_the_budget_is_a_loss_and_memory_stays_bounded() {
        let dir = scratch("past");
        let r = stream_req(&[2], 8_000_000, 2.0, &dir);
        let a = cap("demo:?lines=8&stall=1200&budget=2&buffer=1M", &r).unwrap();
        assert!(!a.lost.is_empty());
        assert!(drops(&a) > 0);
        assert!(a.meta["probe"]["link"]["held"].as_u64().unwrap() > 0);
        assert!(a.meta["probe"]["link"]["flow"]["backlog_max"].as_u64().unwrap() <= 3 << 20);
        // what was kept is right where it was put
        check_counter(&a, &[2]);
        if let Some(d) = &a.temp_dir {
            let _ = std::fs::remove_dir_all(d);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stream_stopped_by_its_caller_ends_complete() {
        let dir = scratch("stop");
        let r = stream_req(&[0], 1_000_000, 30.0, &dir);
        let ctl = Ctl::new();
        let c2 = ctl.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.stop();
        });
        let t = Instant::now();
        let a = reg().capture("demo:?lines=8", &r, &Open::new("t"), &ctl).unwrap();
        assert!(t.elapsed() < Duration::from_secs(10));
        assert!(a.lost.is_empty());
        assert_eq!(a.meta["stream"]["stopped"], true);
        check_counter(&a, &[0]);
        if let Some(d) = &a.temp_dir {
            let _ = std::fs::remove_dir_all(d);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repeat_is_parts_on_a_timeline() {
        let mut r = req(&[0, 9], 1_000_000, 5000);
        r.mode = Mode::Repeat;
        r.segments = Some(3);
        for src in ["demo:?pace=fast", "demo:?type=memory"] {
            let a = cap(src, &r).unwrap();
            let parts = a.parts.as_ref().unwrap();
            assert_eq!(
                parts.iter().map(|p| p.tick0.unwrap()).collect::<Vec<_>>(),
                [0, 10_000, 20_000]
            );
            check_counter(&a, &[0, 9]);
        }
    }

    #[test]
    fn a_source_names_its_kind() {
        let e = reg()
            .capture("nope:x", &req(&[0], 1, 1), &Open::new("t"), &Ctl::new())
            .unwrap_err();
        assert_eq!(e.to_string(), "unknown source 'nope' (known: demo)");
        assert!(crate::source::split("oep").is_err());
        assert_eq!(
            crate::source::split("oep:oep://ab?via=tcp:h:1").unwrap(),
            ("oep", "oep://ab?via=tcp:h:1")
        );
        assert_eq!(reg().list(None).unwrap().0[0].source, "demo:counter");
    }

    /// The whole flow's speed: 16 lines at 200 MHz (2 bytes a sample, 400 MB/s) as fast as the host takes it,
    /// through the budget, the spool and its files (cargo test --release -p wireskein-capture -- --ignored
    /// --nocapture flow_speed; WIRESKEIN_BENCH_DIR: where the files go, not a memory /tmp).
    #[test]
    #[ignore]
    fn flow_speed_of_16_lines_at_200_mhz() {
        let base = std::env::var_os("WIRESKEIN_BENCH_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let dir = base.join(format!("wsk-demo-speed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (lines, secs) in [(16usize, 2.0), (8, 2.0)] {
            let all: Vec<usize> = (0..lines).collect();
            let r = stream_req(&all, 200_000_000, secs, &dir);
            let t = Instant::now();
            let a = cap(&format!("demo:?lines={lines}&pace=fast&chunk=1M"), &r).unwrap();
            let s = t.elapsed().as_secs_f64();
            let bytes = a.meta["stream"]["bytes"].as_f64().unwrap();
            println!(
                "{lines} lines at 200 MHz: {:.0} MB in {s:.2} s = {:.0} MB/s through the spool to disk; flow {}",
                bytes / 1e6,
                bytes / s / 1e6,
                a.meta["probe"]["link"]["flow"]
            );
            assert!(a.lost.is_empty());
            if let Some(d) = &a.temp_dir {
                let _ = std::fs::remove_dir_all(d);
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
