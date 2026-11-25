//! oep.fixture.logic and oep.fixture.analog revision 1 (oep-spec interfaces/oep-if-capture: §1.2 analog values, §2
//! segments, §3 operations, §3.7 calibration, §4 groups; oep-client-python's `capture.LogicCapture`, `AnalogCapture`
//! and `CaptureGroup`): configure and its answer, start / stop / status, the segments, read, release, the data pushes
//! of a stream (§3.4) with their generation and segment, an analog track's calibration, and the capture group that
//! starts tracks together (at once, or on its trigger source).

use crate::blocks::{self, Blocks};
use crate::host::Host;
use crate::msg::{self, Error, Reader, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const NAME: &str = "oep.fixture.logic";
pub const ANALOG_NAME: &str = "oep.fixture.analog";
pub const GROUP_NAME: &str = "oep.fixture.capture-group";
pub const OP_CALIBRATION: u8 = 0x19;
pub const OP_CONFIGURE: u8 = 0x10;
pub const OP_START: u8 = 0x11;
pub const OP_STOP: u8 = 0x12;
pub const OP_STATUS: u8 = 0x14;
pub const OP_READ: u8 = 0x15;
pub const OP_SEGMENTS: u8 = 0x16;
pub const OP_RELEASE: u8 = 0x17;
pub const OP_QUERY: u8 = 0x18;

// configure TLVs
const T_MODE: u8 = 0x01;
const T_RATE: u8 = 0x02;
const T_SAMPLES: u8 = 0x03;
const T_SEGMENTS: u8 = 0x04;
pub const T_TRIGGER: u8 = 0x05;
pub const T_PRETRIGGER: u8 = 0x06;
const T_FRONTEND: u8 = 0x07;
pub const T_FORMAT: u8 = 0x08;
const T_CARRIAGE: u8 = 0x09;
pub const T_TRIGGER_OPTS: u8 = 0x0A;
pub const T_INPUT_HYSTERESIS: u8 = 0x0C;
pub const T_GLITCH_FILTER: u8 = 0x13;
// configure's answer
const A_RATE: u8 = 0x01;
const A_LAYOUT: u8 = 0x02;
const A_SAMPLES: u8 = 0x03;
const A_SEGMENTS: u8 = 0x04;
const A_SCALE: u8 = 0x05;
const A_SKEW: u8 = 0x06;
const A_FRONTEND_USED: u8 = 0x07;
const A_REFERENCE: u8 = 0x08;
const A_FORMAT: u8 = 0x09;
const A_BLOCK: u8 = 0x0A;
const A_GLITCH_USED: u8 = 0x10;
const A_HYSTERESIS_USED: u8 = 0x11;
const A_STREAM_BUFFER: u8 = 0x14;
const A_JUDGE_LATENCY: u8 = 0x15;
const A_DATA_RATE: u8 = 0x16;

pub const ONE_SHOT: u8 = 0x01;
pub const REPEAT: u8 = 0x02;
pub const STREAMING: u8 = 0x03;
pub const TRIGGER_IMMEDIATE: u8 = 0x00;
pub const TRIGGER_LEVEL: u8 = 0x01;
pub const TRIGGER_EDGE: u8 = 0x02;
pub const TRIGGER_LINKED: u8 = 0x07;
pub const FORMAT_LOGIC_BLOCKS: u8 = 0x01;

pub const STATE_WAITING: u8 = 0x02;
pub const STATE_DONE: u8 = 0x04;
pub const STATE_ERROR: u8 = 0x06;
pub const SEGMENT_GAP: u8 = 0x01;
pub const SEGMENT_SLIPPED: u8 = 0x04;
pub const STATUS_DROPPED: u8 = 0x01;
const SEGMENT_BYTES: usize = 37;
const READ_GAP: u8 = 0x02;
const READ_HEAD: usize = 11;
const READ_BATCH: usize = 16;

pub const CAPTURE_DROPS: &str = "io.github.open-embedded-probe.capture-drops";
pub const CAPTURE_SPILL: &str = "io.github.open-embedded-probe.capture-spill";

/// state 6's reason.
pub fn error_name(e: u8) -> String {
    match e {
        1 => "peripheral".into(),
        2 => "storage".into(),
        3 => "clock".into(),
        e => format!("error 0x{e:02x}"),
    }
}

/// What configure answered (§3.3), and the format 1 setting as asked.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub rate: (u64, u64),
    pub width: usize,
    pub positions: Vec<usize>,
    pub samples: u64,
    pub segments: u64,
    pub format: u8,
    pub block: usize,
    pub stream_buffer: Option<u64>,
    pub judge_latency_ns: Option<u64>,
    pub data_rate: Option<u64>,
    pub glitch_used: Option<u64>,
    pub hysteresis_used: Option<bool>,
    /// format 1: the raw roles and the reduced ones as asked
    pub raw_roles: Vec<usize>,
    pub reduced: Vec<(usize, (u8, usize))>,
    /// analog (§1.2): the slot's bits, the value's offset and bits in it, the roles in a frame's order
    pub slot: usize,
    pub offset: u32,
    pub bits: u32,
    pub order: Vec<usize>,
    /// analog, per role: zero and nV per value (signed: an inverting frontend), the skew, the frontend taken
    pub zero: BTreeMap<usize, i64>,
    pub scale_nv: BTreeMap<usize, i64>,
    pub skew_ns: BTreeMap<usize, u64>,
    pub frontend: BTreeMap<usize, u8>,
    /// analog: (source, mV, measured) - an unknown source or how as its number, shown as it is (core §2.4)
    pub reference: Option<(String, u32, Value)>,
}

impl Config {
    pub fn blocks(&self) -> Option<Blocks> {
        (self.format == FORMAT_LOGIC_BLOCKS).then(|| Blocks {
            raw_roles: self.raw_roles.clone(),
            reduced: self.reduced.clone(),
            block: self.block,
            width: self.width,
            positions: self.positions.clone(),
        })
    }
    /// A segment's bytes in the stream (§1.1 rule 4, §1.3).
    pub fn segment_bytes(&self, samples: u64) -> u64 {
        match self.blocks() {
            Some(b) => b.segment_bytes(samples as usize) as u64,
            None if self.width == 0 && !self.order.is_empty() => samples * (self.order.len() * self.slot / 8) as u64,
            None => (samples * self.width as u64).div_ceil(8),
        }
    }
    /// An analog frame's bytes: a value of every channel (§1.2).
    pub fn frame_bytes(&self) -> usize {
        self.slot / 8 * self.order.len()
    }
    /// How long the host may stop reading before the probe drops (§3.3: stream_buffer over data_rate, else over the
    /// format 0 volume).
    pub fn tolerable_delay_s(&self) -> Option<f64> {
        let buf = self.stream_buffer? as f64;
        let rate = match self.data_rate {
            Some(r) if r > 0 => r as f64,
            _ => {
                let per = if self.width > 0 {
                    self.width as f64 / 8.0
                } else {
                    (self.order.len() * self.slot) as f64 / 8.0
                };
                self.rate.0 as f64 / self.rate.1.max(1) as f64 * per
            }
        };
        (rate > 0.0).then(|| buf / rate)
    }
}

fn parse_config(payload: &[u8], analog: bool) -> Result<Config> {
    let mut c = Config::default();
    let mut seen = vec![];
    for (tag, v) in msg::split_tlvs(payload)? {
        if [
            A_RATE,
            A_LAYOUT,
            A_SAMPLES,
            A_SEGMENTS,
            A_FORMAT,
            A_BLOCK,
            A_STREAM_BUFFER,
            A_JUDGE_LATENCY,
            A_DATA_RATE,
        ]
        .contains(&tag)
        {
            if seen.contains(&tag) {
                continue;
            }
            seen.push(tag);
        }
        let u32v = |v: &[u8]| u32::from_le_bytes([v[0], v[1], v[2], v[3]]) as u64;
        match (tag, v.len()) {
            (A_RATE, 8) => {
                let (n, d) = (u32v(&v[..4]), u32v(&v[4..]));
                if d != 0 {
                    c.rate = (n, d);
                }
            }
            (A_LAYOUT, l) if !analog && l >= 2 && l == 2 + v[1] as usize => {
                c.width = v[0] as usize;
                c.positions = v[2..].iter().map(|&p| p as usize).collect();
            }
            (A_LAYOUT, l) if analog && l >= 4 && l == 4 + v[3] as usize => {
                (c.slot, c.offset, c.bits) = (v[0] as usize, v[1] as u32, v[2] as u32);
                c.order = v[4..].iter().map(|&p| p as usize).collect();
            }
            (A_SCALE, 9) if analog => {
                let i = |a: usize| i32::from_le_bytes([v[a], v[a + 1], v[a + 2], v[a + 3]]) as i64;
                c.zero.insert(v[0] as usize, i(1));
                c.scale_nv.insert(v[0] as usize, i(5));
            }
            (A_SKEW, 5) if analog => {
                c.skew_ns.insert(v[0] as usize, u32v(&v[1..]));
            }
            (A_FRONTEND_USED, 2) if analog => {
                c.frontend.insert(v[0] as usize, v[1]);
            }
            (A_REFERENCE, 6) if analog => {
                let source = match v[0] {
                    0 => "supply".to_string(),
                    1 => "internal".to_string(),
                    2 => "external".to_string(),
                    s => s.to_string(),
                };
                let how = match v[5] {
                    0 => Value::Bool(false),
                    1 => Value::Bool(true),
                    h => Value::String(h.to_string()),
                };
                c.reference = Some((source, u32v(&v[1..5]) as u32, how));
            }
            (A_SAMPLES, 4) => c.samples = u32v(&v),
            (A_SEGMENTS, 4) => c.segments = u32v(&v),
            (A_FORMAT, 1) => c.format = v[0],
            (A_BLOCK, 4) => c.block = u32v(&v) as usize,
            (A_STREAM_BUFFER, 4) => c.stream_buffer = Some(u32v(&v)),
            (A_JUDGE_LATENCY, 4) => c.judge_latency_ns = Some(u32v(&v)),
            (A_DATA_RATE, 4) => c.data_rate = Some(u32v(&v)),
            (A_GLITCH_USED, 5) if !analog && v[0] == 0 => c.glitch_used = Some(u32v(&v[1..])),
            (A_HYSTERESIS_USED, 1) if !analog => c.hysteresis_used = Some(v[0] != 0),
            _ => {}
        }
    }
    for k in c.order.clone() {
        c.skew_ns.entry(k).or_insert(0); // a role with no delay may be left out (§3.3)
    }
    Ok(c)
}

/// A segment's record (§2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub serial: u32,
    pub position: u64,
    pub samples: u64,
    pub start_ns: u64,
    pub start_uncertainty_ns: u64,
    pub trigger_index: Option<u64>,
    pub flags: u8,
    pub generation: u32,
}

impl Segment {
    fn parse(b: &[u8]) -> Result<Segment> {
        let mut r = Reader::new(b);
        let serial = r.u32()?;
        let position = r.u64()?;
        let samples = r.u32()? as u64;
        let start_ns = r.u64()?;
        let unc = r.u32()? as u64;
        let trig = r.u32()?;
        let flags = r.u8()?;
        let generation = r.u32()?;
        Ok(Segment {
            serial,
            position,
            samples,
            start_ns,
            start_uncertainty_ns: unc,
            trigger_index: (trig != 0xFFFF_FFFF).then_some(trig as u64),
            flags,
            generation,
        })
    }
}

/// status's answer (§3.2).
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub state: u8,
    pub serial_done: u32,
    pub write_pos: u64,
    pub flags: u8,
    pub generation: u32,
    pub error: Option<u8>,
    pub named: Vec<(String, Vec<u8>)>,
}

impl Status {
    pub fn dropped(&self) -> bool {
        self.flags & STATUS_DROPPED != 0
    }
    pub fn error_name(&self) -> String {
        self.error.map(error_name).unwrap_or_else(|| "unknown".into())
    }
}

/// A streaming data frame's segment_info (§3.4): the spool's segment record.
pub use wireskein_capture::spool::SegmentInfo;

/// A data push (core §10.2): fn seq position data, its generation and segment_info.
#[derive(Debug, Clone)]
pub struct Push {
    pub fn_: u16,
    pub seq: u16,
    pub position: u64,
    pub data: Vec<u8>,
    pub generation: Option<u32>,
    pub info: Option<SegmentInfo>,
}

pub fn parse_push(frame: &[u8]) -> Result<Push> {
    if frame.len() < 13 {
        return Err(Error::Protocol("a data frame shorter than its header".into()));
    }
    let fn_ = u16::from_le_bytes([frame[1], frame[2]]);
    let seq = u16::from_le_bytes([frame[3], frame[4]]);
    let position = u64::from_le_bytes(frame[5..13].try_into().expect("8"));
    let mut rd = Reader::new(&frame[13..]);
    let data = rd.counted16()?.to_vec();
    let tail = rd.tail()?;
    let generation = msg::first(&tail, 0x01)
        .filter(|g| g.len() == 4)
        .map(|g| u32::from_le_bytes([g[0], g[1], g[2], g[3]]));
    let info = msg::first(&tail, 0x02).filter(|v| v.len() == 24).map(|v| SegmentInfo {
        position: u64::from_le_bytes(v[0..8].try_into().expect("8")),
        samples: u32::from_le_bytes(v[8..12].try_into().expect("4")) as u64,
        start_ns: u64::from_le_bytes(v[12..20].try_into().expect("8")),
        start_uncertainty_ns: u32::from_le_bytes(v[20..24].try_into().expect("4")) as u64,
    });
    Ok(Push {
        fn_,
        seq,
        position,
        data,
        generation,
        info,
    })
}

/// What configure is asked.
#[derive(Debug, Clone, Default)]
pub struct Setting {
    pub mode: u8,
    pub rate: u64,
    pub samples: Option<u64>,
    pub segments: Option<u64>,
    /// (type, role, value)
    pub trigger: Option<(u8, u8, u32)>,
    pub pretrigger: Option<u64>,
    /// (arm, rearm)
    pub trigger_opts: Option<(u8, u8)>,
    /// format 1: role -> (carriage, D) of the reduced ones; the roles of every channel
    pub carriage: Vec<(usize, (u8, usize))>,
    pub roles: Vec<usize>,
    pub glitch_filter_ns: Option<u32>,
    pub input_hysteresis: Option<bool>,
    /// analog: role -> the frontend (input range) it is measured with
    pub frontends: Vec<(u8, u8)>,
}

/// A capture track (logic or analog): its fn and what it answered.
pub struct Logic {
    pub fn_: u16,
    pub analog: bool,
    pub config: Config,
    pub generation: Option<u32>,
}

impl Logic {
    pub fn new(host: &mut Host) -> Result<Logic> {
        let fn_ = host.find(NAME, 1)?;
        Ok(Logic {
            fn_,
            analog: false,
            config: Config::default(),
            generation: None,
        })
    }

    /// The analog track (oep.fixture.analog): the same operations; values raw (§1.2).
    pub fn analog(host: &mut Host) -> Result<Logic> {
        let fn_ = host.find(ANALOG_NAME, 1)?;
        Ok(Logic {
            fn_,
            analog: true,
            config: Config::default(),
            generation: None,
        })
    }

    /// The analog calibration (§3.7), raw: the probe applies none of it.
    pub fn calibration(&mut self, host: &mut Host) -> Result<Calibration> {
        let r = host.call(self.fn_, OP_CALIBRATION, &[], false)?;
        let mut out = Calibration::default();
        let mut seen_vrefint = false;
        for (tag, v) in msg::split_tlvs(&r.payload)? {
            if tag == 0x01 {
                if v.len() < 2 || v.len() < 4 + v[1] as usize {
                    continue;
                }
                let n = v[1] as usize;
                let raw_len = u16::from_le_bytes([v[2 + n], v[3 + n]]) as usize;
                if v.len() != 4 + n + raw_len {
                    continue;
                }
                let fe = (v[0] != 0xFF).then_some(v[0]);
                out.factory.push((
                    fe,
                    String::from_utf8_lossy(&v[2..2 + n]).to_string(),
                    v[4 + n..].to_vec(),
                ));
            } else if tag == 0x02 && !seen_vrefint {
                seen_vrefint = true;
                if v.len() == 16 {
                    let raw = u32::from_le_bytes([v[0], v[1], v[2], v[3]]);
                    let ns = u64::from_le_bytes(v[4..12].try_into().expect("8"));
                    out.vrefint = Some((raw, ns));
                    out.vrefint_nominal_mv = Some(u32::from_le_bytes([v[12], v[13], v[14], v[15]]));
                }
            }
        }
        Ok(out)
    }

    /// Channel `k`'s raw values (§1.2: slot bits little endian, the value in bits offset .. offset+bits-1, frames
    /// in `order`) as little-endian integers of the slot's width - 8, 16 or 32 bits (acquire's `_analog_values`).
    pub fn values(&self, data: &[u8], k: usize, samples: usize) -> Result<Vec<u8>> {
        let c = &self.config;
        let w = c.slot / 8;
        if ![1, 2, 4].contains(&w) || !c.slot.is_multiple_of(8) {
            return Err(Error::Protocol(format!(
                "an analog slot of {} bits (8, 16 or 32 are read)",
                c.slot
            )));
        }
        let frame = w * c.order.len();
        let m = c
            .order
            .iter()
            .position(|&r| r == k)
            .ok_or_else(|| Error::Protocol(format!("analog role {k} is not in the layout's order")))?;
        let mask: u64 = if c.bits >= 64 { u64::MAX } else { (1u64 << c.bits) - 1 };
        let n = samples.min(data.len().checked_div(frame).unwrap_or(0));
        let mut out = Vec::with_capacity(n * w);
        for i in 0..n {
            let at = i * frame + m * w;
            let mut x = 0u64;
            for (j, b) in data[at..at + w].iter().enumerate() {
                x |= (*b as u64) << (8 * j);
            }
            let v = (x >> c.offset) & mask;
            out.extend_from_slice(&v.to_le_bytes()[..w]);
        }
        Ok(out)
    }

    fn body(s: &Setting) -> Vec<u8> {
        let mut b = msg::tlv(T_MODE, &[s.mode]);
        b.extend(msg::tlv(T_RATE, &(s.rate as u32).to_le_bytes()));
        if let Some(n) = s.samples {
            b.extend(msg::tlv(T_SAMPLES, &(n as u32).to_le_bytes()));
        }
        if let Some(n) = s.segments {
            b.extend(msg::tlv(T_SEGMENTS, &(n as u32).to_le_bytes()));
        }
        let trigger = s.trigger.map(|t| if t.0 == TRIGGER_IMMEDIATE { (0, 0, 0) } else { t });
        if let Some((ty, role, value)) = trigger {
            let mut v = vec![ty, role];
            v.extend(value.to_le_bytes());
            b.extend(msg::tlv(T_TRIGGER, &v));
        }
        if let Some(p) = s
            .pretrigger
            .filter(|_| trigger.is_some_and(|t| t.0 != TRIGGER_IMMEDIATE))
        {
            b.extend(msg::tlv(T_PRETRIGGER, &(p as u32).to_le_bytes()));
        }
        let mut fes = s.frontends.clone();
        fes.sort();
        for (role, fe) in fes {
            b.extend(msg::tlv(T_FRONTEND, &[role, fe]));
        }
        if !s.carriage.is_empty() {
            b.extend(msg::tlv(T_FORMAT | msg::TAG_CRITICAL, &[FORMAT_LOGIC_BLOCKS]));
        }
        if let Some(ns) = s.glitch_filter_ns {
            let mut v = vec![0u8];
            v.extend(ns.to_le_bytes());
            b.extend(msg::tlv(T_GLITCH_FILTER | msg::TAG_CRITICAL, &v));
        }
        if let Some(h) = s.input_hysteresis {
            b.extend(msg::tlv(T_INPUT_HYSTERESIS | msg::TAG_CRITICAL, &[h as u8]));
        }
        let mut carriage = s.carriage.clone();
        carriage.sort();
        for (role, (kind, d)) in carriage {
            let mut v = vec![role as u8, kind];
            v.extend((d as u32).to_le_bytes());
            b.extend(msg::tlv(T_CARRIAGE | msg::TAG_CRITICAL, &v));
        }
        if let Some((arm, rearm)) = s.trigger_opts {
            let mut v = vec![arm, rearm, 0];
            v.extend(0u32.to_le_bytes());
            b.extend(msg::tlv(T_TRIGGER_OPTS | msg::TAG_CRITICAL, &v));
        }
        b
    }

    /// configure (or its query: the same rules, nothing set) -> the probe's actual values.
    pub fn configure(&mut self, host: &mut Host, s: &Setting, query: bool) -> Result<Config> {
        let op = if query { OP_QUERY } else { OP_CONFIGURE };
        let r = host.call(self.fn_, op, &Self::body(s), !query)?;
        let mut c = parse_config(&r.payload, self.analog)?;
        if !s.carriage.is_empty() {
            c.reduced = s
                .carriage
                .iter()
                .filter(|(_, (k, _))| *k != blocks::RAW)
                .copied()
                .collect();
            let mut raws: Vec<usize> = s
                .roles
                .iter()
                .copied()
                .filter(|r| !c.reduced.iter().any(|(x, _)| x == r))
                .collect();
            raws.sort();
            c.raw_roles = raws;
        }
        if s.mode == ONE_SHOT && c.segments == 0 {
            c.segments = 1;
        }
        if !query {
            self.config = c.clone();
        }
        Ok(c)
    }

    pub fn start(&mut self, host: &mut Host) -> Result<u32> {
        let r = host.call(self.fn_, OP_START, &[], true)?;
        let g = Reader::new(&r.payload).u32()?;
        self.generation = Some(g);
        Ok(g)
    }

    pub fn stop(&mut self, host: &mut Host) -> Result<()> {
        host.call(self.fn_, OP_STOP, &[], true).map(|_| ())
    }

    pub fn status(&mut self, host: &mut Host) -> Result<Status> {
        let r = host.call(self.fn_, OP_STATUS, &[], false)?;
        let mut rd = Reader::new(&r.payload);
        let (state, done, pos, flags, generation) = (rd.u8()?, rd.u32()?, rd.u64()?, rd.u8()?, rd.u32()?);
        let tail = rd.tail()?;
        let error = msg::first(&tail, 0x01).filter(|v| v.len() == 1).map(|v| v[0]);
        self.generation = Some(generation);
        Ok(Status {
            state,
            serial_done: done,
            write_pos: pos,
            flags,
            generation,
            error,
            named: msg::named(&tail),
        })
    }

    /// Every segment record from `from` on (paged by serial, common §1.3).
    pub fn segments(&mut self, host: &mut Host, mut from: u32) -> Result<Vec<Segment>> {
        let mut out = vec![];
        loop {
            let r = host.call(self.fn_, OP_SEGMENTS, &from.to_le_bytes(), false)?;
            let mut rd = Reader::new(&r.payload);
            let (more, count) = (rd.u8()?, rd.u8()?);
            let page: Vec<Segment> = (0..count)
                .map(|_| rd.bytes(SEGMENT_BYTES).and_then(Segment::parse))
                .collect::<Result<_>>()?;
            let last = page.last().map(|s| s.serial);
            out.extend(page);
            match last {
                Some(l) if more != 0 => from = l.wrapping_add(1),
                _ => return Ok(out),
            }
        }
    }

    pub fn release(&mut self, host: &mut Host, serial: u32, generation: u32) -> Result<()> {
        let mut p = generation.to_le_bytes().to_vec();
        p.extend(serial.to_le_bytes());
        host.call(self.fn_, OP_RELEASE, &p, true).map(|_| ())
    }

    /// Bytes [position, position + length) of the stream, pipelined in frame-sized reads; a keepalive between batches.
    pub fn read(&mut self, host: &mut Host, position: u64, length: u64, generation: u32) -> Result<Vec<u8>> {
        let max_frame = host.limits.as_ref().map(|l| l.max_frame).unwrap_or(64);
        let chunk = (max_frame.saturating_sub(msg::RESULT_HEADER + READ_HEAD)).clamp(1, 0xFFFF) as u64;
        let offsets: Vec<u64> = (0..length).step_by(chunk as usize).collect();
        let mut out = Vec::with_capacity(length as usize);
        for (k, batch) in offsets.chunks(READ_BATCH).enumerate() {
            if k > 0 && host.session.is_some() {
                host.keepalive()?;
            }
            let reqs: Vec<(u16, u8, Vec<u8>)> = batch
                .iter()
                .map(|&off| {
                    let mut p = generation.to_le_bytes().to_vec();
                    p.extend((position + off).to_le_bytes());
                    p.extend((chunk.min(length - off) as u16).to_le_bytes());
                    (self.fn_, OP_READ, p)
                })
                .collect();
            let replies = host.pipeline_calls(&reqs, false)?;
            for (&off, r) in batch.iter().zip(replies) {
                let want = chunk.min(length - off) as usize;
                let mut data = read_data(&r.payload, position + off)?;
                while data.len() < want {
                    let at = position + off + data.len() as u64;
                    let mut p = generation.to_le_bytes().to_vec();
                    p.extend(at.to_le_bytes());
                    p.extend(((want - data.len()) as u16).to_le_bytes());
                    let r = host.call(self.fn_, OP_READ, &p, false)?;
                    let more = read_data(&r.payload, at)?;
                    if more.is_empty() {
                        return Err(Error::Protocol(format!("read at {at} returned nothing")));
                    }
                    data.extend(more);
                }
                out.extend(data);
            }
        }
        Ok(out)
    }

    /// Poll status until the one-shot is done (keepalive every second) -> its segments. A timeout while still
    /// waiting for a trigger names it.
    pub fn wait(&mut self, host: &mut Host, timeout: Duration, trigger: Option<&str>) -> Result<Vec<Segment>> {
        let deadline = Instant::now() + timeout;
        let mut kept = Instant::now();
        let mut state = 0;
        while Instant::now() < deadline {
            if host.session.is_some() && kept.elapsed() >= Duration::from_secs(1) {
                host.keepalive()?;
                kept = Instant::now();
            }
            let st = self.status(host)?;
            state = st.state;
            if st.state == STATE_DONE {
                return self.segments(host, 0);
            }
            if st.state == STATE_ERROR {
                return Err(Error::Failed(format!(
                    "the capture stopped with an error ({})",
                    st.error_name()
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        match trigger {
            Some(t) if state == STATE_WAITING => Err(Error::Timeout(format!(
                "no trigger ({t}) within {} s (--timeout)",
                crate::fmt_g(timeout.as_secs_f64())
            ))),
            _ => Err(Error::Timeout("capture did not finish".into())),
        }
    }
}

/// read's answer (position flags len data [TLV], §3.2) -> the data, which must start at `asked`.
fn read_data(payload: &[u8], asked: u64) -> Result<Vec<u8>> {
    let mut rd = Reader::new(payload);
    let (position, flags) = (rd.u64()?, rd.u8()?);
    let data = rd.counted16()?.to_vec();
    if flags & READ_GAP != 0 {
        return Err(Error::Protocol(format!(
            "capture read at {asked}: those bytes are gone, readable from {position} (oep-if-capture §3.2)"
        )));
    }
    if position != asked && !data.is_empty() {
        return Err(Error::Protocol(format!(
            "capture read at {asked} answered position {position} without gap"
        )));
    }
    Ok(data)
}

/// What the probe knows for turning an analog value into a voltage (§3.7), raw.
#[derive(Debug, Clone, Default)]
pub struct Calibration {
    /// (frontend or None, scheme, raw bytes)
    pub factory: Vec<(Option<u8>, String, Vec<u8>)>,
    /// the internal reference measured after the last start: (raw, ns), and its nominal voltage
    pub vrefint: Option<(u32, u64)>,
    pub vrefint_nominal_mv: Option<u32>,
}

// ---------------- the capture group (§4) ----------------

pub const OP_GROUP_BIND: u8 = 0x1A;
const T_TRIGGER_SOURCE: u8 = 0x01;
const T_STAGE: u8 = 0x02;
const T_COND: u8 = 0x03;
const TAG_TRIGGER_CAPS: u8 = 0x41;
const COND_SRC_TRACK: u8 = 1;
/// trigger_cond (§4.4), by number
pub const CONDS: [(&str, u8); 11] = [
    ("level", 1),
    ("edge", 2),
    ("cross_up", 3),
    ("cross_down", 4),
    ("pulse", 5),
    ("pattern", 6),
    ("above", 7),
    ("below", 8),
    ("analog_pulse", 9),
    ("timer", 10),
    ("serial", 11),
];

/// One condition of the group's trigger source (§4.4): the bound track's fn, its role, the type, its arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct Cond {
    pub fn_: u16,
    pub role: u8,
    pub kind: u8,
    pub args: Vec<u8>,
}

impl Cond {
    pub fn level(fn_: u16, role: u8, level: u8) -> Cond {
        Cond {
            fn_,
            role,
            kind: 1,
            args: vec![level, 0],
        }
    }
    /// edge 0 rising, 1 falling, 2 both
    pub fn edge(fn_: u16, role: u8, edge: u8) -> Cond {
        Cond {
            fn_,
            role,
            kind: 2,
            args: vec![edge],
        }
    }
    fn crossing(fn_: u16, role: u8, kind: u8, value: u32) -> Cond {
        let mut args = value.to_le_bytes().to_vec();
        args.extend(0u32.to_le_bytes());
        if kind == 7 || kind == 8 {
            args.push(0); // when
        }
        Cond { fn_, role, kind, args }
    }
    pub fn cross_up(fn_: u16, role: u8, value: u32) -> Cond {
        Cond::crossing(fn_, role, 3, value)
    }
    pub fn cross_down(fn_: u16, role: u8, value: u32) -> Cond {
        Cond::crossing(fn_, role, 4, value)
    }
    pub fn above(fn_: u16, role: u8, value: u32) -> Cond {
        Cond::crossing(fn_, role, 7, value)
    }
    pub fn below(fn_: u16, role: u8, value: u32) -> Cond {
        Cond::crossing(fn_, role, 8, value)
    }
}

/// The bind TLVs of a trigger source of one stage of one condition (§4.4), every one critical; `arm` 0 at once, 1
/// once the pretrigger is there.
pub fn trigger_source_tlvs(cond: &Cond, arm: u8) -> Vec<u8> {
    let mut v = vec![1u8];
    v.extend(0u64.to_le_bytes());
    v.extend([arm, 0]);
    let mut out = msg::tlv(T_TRIGGER_SOURCE | msg::TAG_CRITICAL, &v);
    let mut st = vec![0u8];
    st.extend(1u32.to_le_bytes());
    st.extend(0u64.to_le_bytes());
    st.push(0);
    out.extend(msg::tlv(T_STAGE | msg::TAG_CRITICAL, &st));
    let mut c = vec![0u8, 0, 0, COND_SRC_TRACK];
    c.extend(cond.fn_.to_le_bytes());
    c.extend([cond.role, cond.kind]);
    c.extend_from_slice(&cond.args);
    out.extend(msg::tlv(T_COND | msg::TAG_CRITICAL, &c));
    out
}

/// describe's trigger_caps (§4.4): what the group's trigger source takes (a declaration; bind has the last word).
#[derive(Debug, Clone, PartialEq)]
pub struct TriggerCaps {
    pub max_stages: u8,
    pub max_terms: u8,
    pub max_conds: u8,
    pub cross_track: bool,
    pub types: u32,
}

impl TriggerCaps {
    pub fn takes(&self, kind: u8) -> bool {
        kind < 32 && (self.types >> kind) & 1 == 1
    }
    /// The declared condition types by name, in number order.
    pub fn kinds(&self) -> Vec<&'static str> {
        CONDS.iter().filter(|(_, k)| self.takes(*k)).map(|(n, _)| *n).collect()
    }
}

/// The capture group's status (§4).
#[derive(Debug, Clone, Default)]
pub struct GroupStatus {
    pub state: u8,
    pub start_ns: Option<u64>,
    pub generation: u32,
    /// once the event happened: (fn judging, ns, +-ns)
    pub trigger: Option<(u16, u64, u32)>,
}

/// oep.fixture.capture-group: tracks started together by one request.
pub struct Group {
    pub fn_: u16,
    pub generation: Option<u32>,
}

impl Group {
    pub fn new(host: &mut Host) -> Result<Group> {
        Ok(Group {
            fn_: host.find(GROUP_NAME, 1)?,
            generation: None,
        })
    }

    pub fn trigger_caps(&self, host: &mut Host) -> Result<Option<TriggerCaps>> {
        for (t, v) in host.describe(self.fn_)? {
            if t == TAG_TRIGGER_CAPS {
                if v.len() != 13 {
                    return Ok(None);
                }
                return Ok(Some(TriggerCaps {
                    max_stages: v[0],
                    max_terms: v[1],
                    max_conds: v[2],
                    cross_track: v[3] != 0,
                    types: u32::from_le_bytes([v[4], v[5], v[6], v[7]]),
                }));
            }
        }
        Ok(None)
    }

    /// Bind these configured tracks (their fns); none unbinds. `source`: the trigger source's TLVs.
    pub fn bind(&mut self, host: &mut Host, fns: &[u16], source: &[u8]) -> Result<()> {
        let mut p = vec![fns.len() as u8];
        for f in fns {
            p.extend(f.to_le_bytes());
        }
        p.extend_from_slice(source);
        host.call(self.fn_, OP_GROUP_BIND, &p, true).map(|_| ())
    }

    /// -> the group's start_ns; each track's new generation set (in `tracks`).
    pub fn start(&mut self, host: &mut Host, tracks: &mut [&mut Logic]) -> Result<u64> {
        let r = host.call(self.fn_, OP_START, &[], true)?;
        let mut rd = Reader::new(&r.payload);
        let (start_ns, g, n) = (rd.u64()?, rd.u32()?, rd.u8()?);
        self.generation = Some(g);
        for _ in 0..n {
            let (f, tg) = (rd.u16()?, rd.u32()?);
            for t in tracks.iter_mut() {
                if t.fn_ == f {
                    t.generation = Some(tg);
                }
            }
        }
        Ok(start_ns)
    }

    pub fn stop(&mut self, host: &mut Host) -> Result<()> {
        host.call(self.fn_, OP_STOP, &[], true).map(|_| ())
    }

    pub fn status(&mut self, host: &mut Host) -> Result<GroupStatus> {
        let r = host.call(self.fn_, OP_STATUS, &[], false)?;
        let mut rd = Reader::new(&r.payload);
        let (state, start, generation) = (rd.u8()?, rd.u64()?, rd.u32()?);
        let tail = rd.tail()?;
        self.generation = Some(generation);
        let trigger = msg::first(&tail, 0x02).filter(|v| v.len() == 14).map(|v| {
            (
                u16::from_le_bytes([v[0], v[1]]),
                u64::from_le_bytes(v[2..10].try_into().expect("8")),
                u32::from_le_bytes(v[10..14].try_into().expect("4")),
            )
        });
        Ok(GroupStatus {
            state,
            start_ns: (start != u64::MAX).then_some(start),
            generation,
            trigger,
        })
    }

    /// Poll until every track is done (one-shot), keepalive every second.
    pub fn wait(&mut self, host: &mut Host, timeout: Duration) -> Result<(GroupStatus, u8)> {
        let deadline = Instant::now() + timeout;
        let mut kept = Instant::now();
        let mut last = 0;
        while Instant::now() < deadline {
            if host.session.is_some() && kept.elapsed() >= Duration::from_secs(1) {
                host.keepalive()?;
                kept = Instant::now();
            }
            let st = self.status(host)?;
            last = st.state;
            if st.state == STATE_DONE {
                return Ok((st, last));
            }
            if st.state == STATE_ERROR {
                return Err(Error::Failed("the group's capture stopped with an error".into()));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = last;
        Err(Error::Timeout("the group's capture did not finish".into()))
    }
}
