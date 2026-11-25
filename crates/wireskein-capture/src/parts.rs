//! A capture as a device's driver gives it, put together: its parts (blocks of ticks with no gap inside), each part's
//! lines (bits, a reduced line's values, analog values), what was lost (wireskein-format §3.4), its facts.

use crate::error::{Error, Result};
use crate::ratio::Ratio;
use serde_json::{Map, Value, json};
use std::path::PathBuf;

/// How a line is kept over intervals of D samples (a reduction; the numbers are OEP capture format 1's
/// carriages, capture §1.3).
pub const RAW: u8 = 0;
pub const SAMPLE: u8 = 1;
pub const ANY_HIGH: u8 = 2;
pub const ANY_LOW: u8 = 3;
pub const CHANGE_LATCH: u8 = 4;

pub fn carriage_name(kind: u8) -> &'static str {
    match kind {
        SAMPLE => "sample",
        ANY_HIGH => "any_high",
        ANY_LOW => "any_low",
        CHANGE_LATCH => "change_latch",
        _ => "raw",
    }
}

pub fn carriage_of(name: &str) -> Option<u8> {
    Some(match name {
        "sample" => SAMPLE,
        "any_high" => ANY_HIGH,
        "any_low" => ANY_LOW,
        "change_latch" => CHANGE_LATCH,
        _ => return None,
    })
}

/// An interval's bits for a reduction.
pub fn value_bits(kind: u8) -> usize {
    if kind == CHANGE_LATCH { 2 } else { 1 }
}

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
    pub(crate) next: Option<u64>,
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

/// A capture put together: one part (`parts` None: its `channels`, `meta` and `samples`) or several.
#[derive(Debug, Clone)]
pub struct Acquired {
    pub tick_hz: (u64, u64),
    pub channels: Vec<Chan>,
    /// the first part's facts and "probe": the probe's
    pub meta: Map<String, Value>,
    pub parts: Option<Vec<Part>>,
    pub lost: Vec<Loss>,
    pub layout: String,
    /// where a stream's bits are kept on disk: the caller removes it once written
    pub temp_dir: Option<PathBuf>,
    pub samples: Option<u64>,
}

impl Acquired {
    /// The parts, the one part made of `channels` and `meta` when it came in one.
    pub fn all_parts(&self) -> Vec<Part> {
        match &self.parts {
            Some(p) => p.clone(),
            None => {
                let meta = self
                    .meta
                    .iter()
                    .filter(|(k, _)| *k != "probe")
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                vec![Part {
                    channels: self.channels.clone(),
                    meta,
                    tick0: Some(0),
                    samples: self.samples,
                }]
            }
        }
    }
}
