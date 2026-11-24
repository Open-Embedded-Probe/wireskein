//! oep.fixture.logic revision 1 (oep-spec interfaces/oep-if-capture: §2 segments, §3 operations; oep-client-python's
//! `capture.LogicCapture`): configure and its answer, start / stop / status, the segments, read, release, and the data
//! pushes of a stream (§3.4) with their generation and segment.

use crate::blocks::{self, Blocks};
use crate::host::Host;
use crate::msg::{self, Error, Reader, Result};
use std::time::{Duration, Instant};

pub const NAME: &str = "oep.fixture.logic";
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
            None => (samples * self.width as u64).div_ceil(8),
        }
    }
    /// How long the host may stop reading before the probe drops (§3.3: stream_buffer over data_rate, else over the
    /// format 0 volume).
    pub fn tolerable_delay_s(&self) -> Option<f64> {
        let buf = self.stream_buffer? as f64;
        let rate = match self.data_rate {
            Some(r) if r > 0 => r as f64,
            _ => self.rate.0 as f64 / self.rate.1.max(1) as f64 * self.width as f64 / 8.0,
        };
        (rate > 0.0).then(|| buf / rate)
    }
}

fn parse_config(payload: &[u8]) -> Result<Config> {
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
            (A_LAYOUT, l) if l >= 2 && l == 2 + v[1] as usize => {
                c.width = v[0] as usize;
                c.positions = v[2..].iter().map(|&p| p as usize).collect();
            }
            (A_SAMPLES, 4) => c.samples = u32v(&v),
            (A_SEGMENTS, 4) => c.segments = u32v(&v),
            (A_FORMAT, 1) => c.format = v[0],
            (A_BLOCK, 4) => c.block = u32v(&v) as usize,
            (A_STREAM_BUFFER, 4) => c.stream_buffer = Some(u32v(&v)),
            (A_JUDGE_LATENCY, 4) => c.judge_latency_ns = Some(u32v(&v)),
            (A_DATA_RATE, 4) => c.data_rate = Some(u32v(&v)),
            (A_GLITCH_USED, 5) if v[0] == 0 => c.glitch_used = Some(u32v(&v[1..])),
            (A_HYSTERESIS_USED, 1) => c.hysteresis_used = Some(v[0] != 0),
            _ => {}
        }
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

/// A streaming data frame's segment_info (§3.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentInfo {
    pub position: u64,
    pub samples: u64,
    pub start_ns: u64,
    pub start_uncertainty_ns: u64,
}

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
}

/// The logic track: its fn and what it answered.
pub struct Logic {
    pub fn_: u16,
    pub config: Config,
    pub generation: Option<u32>,
}

impl Logic {
    pub fn new(host: &mut Host) -> Result<Logic> {
        let fn_ = host.find(NAME, 1)?;
        Ok(Logic {
            fn_,
            config: Config::default(),
            generation: None,
        })
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
        let mut c = parse_config(&r.payload)?;
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
