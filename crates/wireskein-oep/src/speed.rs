//! port_speed (oep-if-link §3) on a UART bridge this host opened, by oep-client-python's procedure (its README's
//! "UART bridge を速くする", `link.raise_speed` and the in-use judging of `link.SerialLink`), and its record
//! (`speed_record`, the same file, so either client reads what the other learned).
//!
//! The full form, as a capture asks it (acquire's FLOWS: the read-back direction only): the boot speed's baseline per
//! flow (this session's frames at the boot speed when 60 or more, else 60 measured per flow), then per candidate
//! try -> the host to the requested baud -> confirm (100 ms, 3) -> every flow (16 frames; above 500000, the user's
//! choice, in and out with full frames for 1 s each) -> commit. A failed candidate: revert at its rate, the boot speed,
//! confirms there, port_state. Committed: the first period is the rate's probation (32 KiB both ways and 1 s, judged as
//! the verify judges a flow), then the frames of the last 3 s are judged (none under 50: over max(2 x baseline, 10 %)
//! broken or lost steps down); a request whose answer does not come at the rate falls back to the boot speed and the
//! next lower candidate. A rate that broke is not used again in the session, nor any above it. While raised the link
//! sends a keepalive before a request once it has been quiet for 1 s (the probe goes back after 3 s with no good frame).
//!
//! The record (`$XDG_CACHE_HOME/oep-client/link-speed.json`): per "<port>|<unit_id>" the rates that passed (kept 30
//! days) and failed or are unknown (1 day); passed rates go first, failed ones are left out (all failed: the slowest is
//! tried once), at most two tried per capture (acquire's PER_SESSION).

use crate::host::{self, Host};
use crate::link::Framing;
use crate::msg::{self, Error, Reader, Result};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// acquire's default candidates (fastest first) and how many one capture tries
pub const FAST: [u32; 3] = [1_500_000, 921_600, 500_000];
pub const PER_SESSION: usize = 2;
pub const LINK_NAME: &str = "oep.probe.link";
pub const OP_SOURCE: u8 = 0x10;
pub const OP_SINK: u8 = 0x11;
pub const OP_PORT_SPEED: u8 = 0x12;
pub const OP_PORT_STATE: u8 = 0x13;
const STEP_TRY: u8 = 0;
const STEP_COMMIT: u8 = 1;
const STEP_REVERT: u8 = 2;
/// fn 0's describe: transport (index, kind, usb interface); kinds (core §7.5)
pub const TAG_TRANSPORT: u8 = 0x49;
pub const UART_BRIDGE: u8 = 1;
const RELAYING_BROKER: u8 = 0xFF;

pub const DEFAULT_CEILING: u32 = 500_000;
const FAST_VERIFY_S: f64 = 1.0;
const VERIFY_MS: u32 = 2000;
const CONFIRM_TRIES: usize = 3;
const CONFIRM_WAIT: Duration = Duration::from_millis(100);
const FLOW_FRAMES: u64 = 16;
const FLOW_FAIL_MIN: u64 = 3;
const VERIFY_FLOOR: f64 = 0.05;
const BASELINE_FRAMES: u64 = 60;
const BASELINE_MAX: f64 = 0.10;
const SWITCH_SETTLE: Duration = Duration::from_millis(20);
const PROBATION_BYTES: u64 = 32 * 1024;
const PROBATION_S: f64 = 1.0;
const SETTLE_S: f64 = 2.0;
/// raised: a keepalive once the link has been quiet this long (under half of port_speed_idle_ms)
pub const KEEPALIVE: Duration = Duration::from_secs(1);
/// the confirms at the boot speed: port_speed_idle_ms + host_wait_add_ms (transports §4)
pub const OPEN_RETRY: Duration = Duration::from_millis(4000);
const IN_USE_WINDOW_S: f64 = 3.0;
const IN_USE_MIN_FRAMES: usize = 50;
const IN_USE_FLOOR: f64 = 0.10;
const STEP_DOWN_WAIT: Duration = Duration::from_millis(200);
/// the probe's own source answer: max_frame - 7 (oep-if-link §2)
const SOURCE_OVERHEAD: usize = msg::RESULT_HEADER + 2;

/// Python's `{x:.Np%}`.
pub fn pct(x: f64, digits: usize) -> String {
    format!("{:.*}%", digits, x * 100.0)
}

// ---------------- the link's judging (SerialLink's counts) ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Good,
    Broken,
    Lost,
}

/// A committed rate's first period in use.
#[derive(Debug, Clone)]
pub struct Probation {
    pub rate: u32,
    pub bytes: u64,
    pub seconds: f64,
    pub threshold: f64,
    pub started: Instant,
    pub settling: bool,
    pub moved: u64,
    pub frames: u64,
    pub bad: u64,
}

/// How frames go on a serial port while a session holds it: at the boot speed into this session's baseline, at a
/// raised rate in use into the 3 s window and the rate's probation (oep-client-python's `SerialLink._count`).
#[derive(Debug)]
pub struct Judge {
    /// a serial port: its boot speed (None: not a serial port, nothing is counted)
    pub base_baud: Option<u32>,
    /// a session holds the port
    pub held: bool,
    /// a raised rate in use (not raise_speed's own trial)
    pub fallback: bool,
    /// this session's frames at the boot speed: good, broken, lost
    pub base_counts: [u64; 3],
    pub window: VecDeque<(Instant, bool)>,
    pub baseline_ratio: f64,
    pub probation: Option<Probation>,
    /// why the link steps down at the next safe point ("" none)
    pub step_due: String,
    pub step_ratio: Option<f64>,
    /// a probation that passed and the host has not noted yet: (rate, bytes moved)
    pub passed: Option<(u32, u64)>,
}

impl Default for Judge {
    fn default() -> Judge {
        Judge {
            base_baud: None,
            held: false,
            fallback: true,
            base_counts: [0; 3],
            window: VecDeque::new(),
            baseline_ratio: 0.0,
            probation: None,
            step_due: String::new(),
            step_ratio: None,
            passed: None,
        }
    }
}

impl Judge {
    pub fn raised(&self, baud: Option<u32>) -> bool {
        self.base_baud.is_some() && baud.is_some() && baud != self.base_baud
    }
    pub fn in_use(&self, baud: Option<u32>) -> bool {
        self.fallback && self.raised(baud)
    }

    /// Bytes on the line at a raised rate in use (both ways): the probation counts them.
    pub fn moved(&mut self, n: u64, baud: Option<u32>) {
        let in_use = self.in_use(baud);
        if let Some(p) = self.probation.as_mut()
            && in_use
            && Some(p.rate) == baud
        {
            p.moved += n;
        }
    }

    /// One frame the host side saw: `kind` good, broken or lost.
    pub fn count(&mut self, kind: Kind, baud: Option<u32>) {
        if self.base_baud.is_none() || !self.held {
            return;
        }
        if baud == self.base_baud {
            self.base_counts[kind as usize] += 1;
            return;
        }
        if !self.in_use(baud) {
            return;
        }
        let rate = baud.unwrap_or(0);
        let now = Instant::now();
        let bad = kind != Kind::Good;
        let mut passed = None;
        if let Some(p) = self.probation.as_mut()
            && p.rate == rate
        {
            p.frames += 1;
            if bad {
                p.bad += 1;
                let ratio = p.bad as f64 / p.frames as f64;
                if self.step_due.is_empty() && p.bad >= FLOW_FAIL_MIN && ratio > p.threshold {
                    self.step_ratio = Some(ratio);
                    self.step_due = format!(
                        "in probation: {} of {} frames broken or lost at {rate} after {} bytes ({}, over {})",
                        p.bad,
                        p.frames,
                        p.moved,
                        pct(ratio, 1),
                        pct(p.threshold, 0)
                    );
                }
            } else if p.moved >= p.bytes && now.duration_since(p.started).as_secs_f64() >= p.seconds {
                passed = Some((p.rate, p.moved));
            }
        }
        if passed.is_some() {
            self.probation = None;
            self.passed = passed;
        }
        self.window.push_back((now, bad));
        while let Some(&(t, _)) = self.window.front() {
            if now.duration_since(t).as_secs_f64() > IN_USE_WINDOW_S {
                self.window.pop_front();
            } else {
                break;
            }
        }
        if !bad || !self.step_due.is_empty() || self.window.len() < IN_USE_MIN_FRAMES {
            return;
        }
        let n_bad = self.window.iter().filter(|x| x.1).count();
        let ratio = n_bad as f64 / self.window.len() as f64;
        let threshold = (2.0 * self.baseline_ratio).max(IN_USE_FLOOR);
        if ratio > threshold {
            self.step_ratio = Some(ratio);
            self.step_due = format!(
                "{n_bad} of {} frames broken or lost within 3 s at {rate} ({}, over {})",
                self.window.len(),
                pct(ratio, 1),
                pct(threshold, 0)
            );
        }
    }
}

// ---------------- the report ----------------

#[derive(Debug, Clone)]
pub struct FlowResult {
    pub flow: &'static str,
    pub n: usize,
    pub frames: u64,
    pub broken: u64,
    pub lost: u64,
    pub kb_s: f64,
    pub passed: bool,
    pub gone: bool,
}

impl FlowResult {
    pub fn ratio(&self) -> f64 {
        if self.frames == 0 {
            0.0
        } else {
            (self.broken + self.lost) as f64 / self.frames as f64
        }
    }
    pub fn name(&self) -> String {
        format!("{}@{}", self.flow, self.n)
    }
}

#[derive(Debug, Clone, Default)]
pub struct Trial {
    pub rate: u32,
    pub actual: Option<u32>,
    pub switched: Option<u32>,
    pub flows: Vec<FlowResult>,
    pub committed: bool,
    pub why: String,
    pub n_cap: usize,
    pub probation: String,
    pub probation_bytes: u64,
    pub settling: bool,
}

#[derive(Debug, Clone)]
pub struct StepDown {
    pub rate: u32,
    pub why: String,
    pub ratio: Option<f64>,
    pub to: Option<u32>,
    pub probation: bool,
}

/// raise_speed's answer (the link's `speed`): what was tried and where the link is.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub base: u32,
    pub supported: bool,
    pub rate: u32,
    pub chosen: Option<u32>,
    pub trials: Vec<Trial>,
    pub why: String,
    pub lost: bool,
    pub stepped_down: bool,
    pub down_why: String,
    pub verified: bool,
    pub baseline: Vec<(String, f64)>,
    pub baseline_frames: u64,
    pub baseline_flows: Vec<FlowResult>,
    pub step_downs: Vec<StepDown>,
    pub skipped: Vec<u32>,
    pub retried: Option<u32>,
    pub capped: Vec<u32>,
}

impl Report {
    fn baseline_of(&self, flow: &str) -> f64 {
        self.baseline
            .iter()
            .find(|(f, _)| f == flow)
            .map(|x| x.1)
            .unwrap_or(0.0)
    }

    /// meta.probe.link's part of it (acquire's `_link_info`): the rate in force, each trial with its flows, what the
    /// record skipped, the steps down.
    pub fn link_fields(&self, out: &mut Map<String, Value>) {
        out.insert("rate".into(), json!(self.rate));
        out.insert("raised".into(), json!(self.chosen.is_some()));
        if !self.supported {
            out.insert("why".into(), json!(self.why));
        }
        out.insert(
            "trials".into(),
            Value::Array(
                self.trials
                    .iter()
                    .map(|t| {
                        json!({"rate": t.rate,
                               "result": if t.committed { "committed".to_string() } else { t.why.clone() },
                               "flows": t.flows.iter().map(|f| json!({"flow": f.flow, "n": f.n, "frames": f.frames,
                                   "broken": f.broken, "lost": f.lost, "kb_s": round1(f.kb_s),
                                   "passed": f.passed})).collect::<Vec<_>>()})
                    })
                    .collect(),
            ),
        );
        if !self.skipped.is_empty() {
            out.insert("skipped".into(), json!(self.skipped));
        }
        if let Some(r) = self.retried {
            out.insert("retried".into(), json!(r));
        }
        if !self.capped.is_empty() {
            out.insert("capped".into(), json!(self.capped));
        }
        if !self.step_downs.is_empty() {
            out.insert(
                "step_downs".into(),
                Value::Array(
                    self.step_downs
                        .iter()
                        .map(|d| json!({"from": d.rate, "to": d.to, "why": d.why, "probation": d.probation}))
                        .collect(),
                ),
            );
        }
        if self.stepped_down || self.lost {
            let why = if self.down_why.is_empty() {
                "lost".to_string()
            } else {
                self.down_why.clone()
            };
            out.insert("stepped_down".into(), json!(why));
        }
    }
}

fn round1(x: f64) -> f64 {
    wireskein_io::pyfmt::round_n(x, 1)
}

// ---------------- the record (speed_record) ----------------

const PASS_TTL_S: i64 = 30 * 86400;
const FAIL_TTL_S: i64 = 86400;

/// The port_speed record: one JSON file, {"<port>|<unit_id>": {"port", "unit_id", "rates": {"<rate>": {"result",
/// "passed", "phase", "at"}}}} - oep-client-python's `speed_record.SpeedRecord` (and oep-client-js's), the same file.
#[derive(Debug, Clone)]
pub struct Record {
    pub path: PathBuf,
    pub data: Map<String, Value>,
    pub error: Option<String>,
}

pub fn record_path() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
    let base = env("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from(".cache"));
    base.join("oep-client").join("link-speed.json")
}

/// Seconds since the epoch -> ISO 8601 UTC ("2026-11-25T08:00:00+00:00", Python's isoformat(timespec="seconds")).
pub fn iso_utc(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let s = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant's algorithm)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}+00:00",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// ISO 8601 ("YYYY-MM-DDTHH:MM:SS[.fff][+HH:MM|Z]", no offset: UTC) -> seconds since the epoch.
pub fn parse_iso(t: &str) -> Option<i64> {
    let b = t.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !(b[10] == b'T' || b[10] == b' ') {
        return None;
    }
    let num = |a: usize, n: usize| t.get(a..a + n)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, s) = (
        num(0, 4)?,
        num(5, 2)?,
        num(8, 2)?,
        num(11, 2)?,
        num(14, 2)?,
        num(17, 2)?,
    );
    let mut rest = &t[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "" | "Z" => 0,
        r if r.len() == 6 && (r.starts_with('+') || r.starts_with('-')) => {
            let oh = r[1..3].parse::<i64>().ok()?;
            let om = r[4..6].parse::<i64>().ok()?;
            let o = oh * 3600 + om * 60;
            if r.starts_with('-') { -o } else { o }
        }
        _ => return None,
    };
    // days from civil
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if mo > 2 { mo - 3 } else { mo + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86400 + h * 3600 + mi * 60 + s - offset)
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn result_of(v: &Value) -> Option<&'static str> {
    match v.get("result").and_then(|r| r.as_str()) {
        Some("passed") => return Some("passed"),
        Some("failed") => return Some("failed"),
        Some("unknown") => return Some("unknown"),
        _ => {}
    }
    match v.get("passed")? {
        Value::Null => Some("unknown"),
        Value::Bool(true) => Some("passed"),
        Value::Bool(false) => Some("failed"),
        _ => None,
    }
}

impl Record {
    pub fn open(path: Option<PathBuf>) -> Record {
        let path = path.unwrap_or_else(record_path);
        let mut r = Record {
            path,
            data: Map::new(),
            error: None,
        };
        match std::fs::read_to_string(&r.path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(m)) => r.data = m,
                Ok(_) => {}
                Err(e) => r.error = Some(format!("{}: {e}", r.path.display())),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => r.error = Some(format!("{}: {e}", r.path.display())),
        }
        r
    }

    pub fn names_a_unit(unit: &str) -> bool {
        !unit.starts_with("x-")
    }

    fn key(port: &str, unit: &str) -> String {
        format!("{port}|{unit}")
    }

    /// The entry's rates within their expiry: rate -> passed / failed / unknown.
    fn fresh(entry: &Value) -> BTreeMap<u32, &'static str> {
        let mut out = BTreeMap::new();
        let now = now_s();
        let Some(rates) = entry.get("rates").and_then(|r| r.as_object()) else {
            return out;
        };
        for (rate, v) in rates {
            let (Ok(r), Some(res), Some(at)) = (
                rate.parse::<u32>(),
                result_of(v),
                v.get("at").and_then(|a| a.as_str()).and_then(parse_iso),
            ) else {
                continue;
            };
            let ttl = if res == "passed" { PASS_TTL_S } else { FAIL_TTL_S };
            if at >= now - ttl {
                out.insert(r, res);
            }
        }
        out
    }

    /// (rates that passed, rates that failed) on this port with this probe, fastest first.
    pub fn lookup(&self, port: &str, unit: &str) -> (Vec<u32>, Vec<u32>) {
        if !Record::names_a_unit(unit) {
            return (vec![], vec![]);
        }
        let rates = self
            .data
            .get(&Record::key(port, unit))
            .map(Record::fresh)
            .unwrap_or_default();
        let pick = |w: &str| rates.iter().rev().filter(|(_, v)| **v == w).map(|(r, _)| *r).collect();
        (pick("passed"), pick("failed"))
    }

    /// `rate` passed (Some(true)), failed (Some(false)) or unknown (None) now, decided at `phase`; saved.
    pub fn note(&mut self, port: &str, unit: &str, rate: u32, passed: Option<bool>, phase: &str) {
        if !Record::names_a_unit(unit) {
            return;
        }
        let entry = self
            .data
            .entry(Record::key(port, unit))
            .or_insert_with(|| json!({"port": port, "unit_id": unit, "rates": {}}));
        if !entry.is_object() {
            *entry = json!({"port": port, "unit_id": unit, "rates": {}});
        }
        let e = entry.as_object_mut().expect("an object");
        e.insert("port".into(), json!(port));
        e.insert("unit_id".into(), json!(unit));
        let rates = e.entry("rates").or_insert_with(|| json!({}));
        if !rates.is_object() {
            *rates = json!({});
        }
        let result = match passed {
            None => "unknown",
            Some(true) => "passed",
            Some(false) => "failed",
        };
        rates.as_object_mut().expect("an object").insert(
            rate.to_string(),
            json!({"result": result, "passed": passed, "phase": phase, "at": iso_utc(now_s())}),
        );
        self.save();
    }

    /// Write the file (expired rates dropped); false when it cannot be written.
    pub fn save(&mut self) -> bool {
        let keys: Vec<String> = self.data.keys().cloned().collect();
        for k in keys {
            let fresh = Record::fresh(&self.data[&k]);
            if fresh.is_empty() {
                self.data.remove(&k);
                continue;
            }
            if let Some(rates) = self.data[&k].get("rates").and_then(|r| r.as_object()).cloned() {
                let kept: Map<String, Value> = rates
                    .into_iter()
                    .filter(|(r, _)| r.parse::<u32>().is_ok_and(|r| fresh.contains_key(&r)))
                    .collect();
                if let Some(e) = self.data.get_mut(&k).and_then(|e| e.as_object_mut()) {
                    e.insert("rates".into(), Value::Object(kept));
                }
            }
        }
        let write = || -> std::io::Result<()> {
            if let Some(d) = self.path.parent() {
                std::fs::create_dir_all(d)?;
            }
            // Python's json.dumps(indent=1, sort_keys=True): serde_json's pretty form with one space a level
            let pretty =
                serde_json::to_string_pretty(&Value::Object(self.data.clone())).map_err(std::io::Error::other)?;
            let mut buf: Vec<u8> = Vec::with_capacity(pretty.len());
            for line in pretty.lines() {
                let lead = line.len() - line.trim_start_matches(' ').len();
                buf.extend(std::iter::repeat_n(b' ', lead / 2));
                buf.extend_from_slice(&line.as_bytes()[lead..]);
                buf.push(b'\n');
            }
            let mut tmp = self.path.clone().into_os_string();
            tmp.push(".tmp");
            std::fs::write(&tmp, &buf)?;
            std::fs::rename(&tmp, &self.path)
        };
        match write() {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("{}: {e}", self.path.display()));
                false
            }
        }
    }
}

// ---------------- the host's side ----------------

/// One raise_speed call's settings, kept for its steps down in use.
#[derive(Debug, Clone)]
struct Run {
    flows: Vec<(&'static str, usize)>,
    frames: u64,
    wait: u32,
    probation_bytes: u64,
    probation_s: f64,
    settle_s: f64,
    size: usize,
    n_max: usize,
}

/// What a session's port_speed holds on the host (SerialLink's fields beside the judging).
#[derive(Debug, Default)]
pub struct Speed {
    pub report: Option<Report>,
    pub fn_: Option<u16>,
    pub port: Option<u8>,
    /// rates that broke in use in this session -> why (none at or above them again)
    pub unusable: BTreeMap<u32, String>,
    /// every rate the line failed in this session
    pub failed: BTreeMap<u32, String>,
    unusable_session: Option<u32>,
    broke_at: Option<Instant>,
    broke_rate: Option<u32>,
    /// the candidates a step down in use may go to, and the session they belong to
    plan: Option<(Vec<u32>, Option<u32>)>,
    run: Option<Run>,
    pub record: Option<(Record, String, String)>,
    /// in flight at the most at the raised rate (0: no cap)
    pub inflight_cap: usize,
    pub lost_count: u64,
    /// the link's own requests are going out (no keepalive or fallback for them)
    pub own: u32,
    /// a keepalive of its own is going out (none inside it)
    pub keeping: bool,
    /// what a capture asked: the rates (acquire's `fast`), whether the record shaped them (`remembered`), and whether
    /// the record was to decide (the default)
    pub asked: Vec<u32>,
    pub remembered: bool,
    pub by_record: bool,
    /// the raise was made for this session
    pub done: Option<u32>,
}

fn pack(rate: u32, step: u8, wait: u16) -> Vec<u8> {
    let mut p = rate.to_le_bytes().to_vec();
    p.push(step);
    p.extend(wait.to_le_bytes());
    p
}

fn type_name(e: &Error) -> &'static str {
    match e {
        Error::Broken(_) => "CorruptFrame",
        Error::Framing(_) => "FramingLost",
        Error::Timeout(_) => "TimeoutError",
        _ => "Exception",
    }
}

/// A link error a flow or a trial reads past (Python's LINK_ERRORS).
fn link_error(e: &Error) -> bool {
    matches!(e, Error::Broken(_) | Error::Timeout(_) | Error::Framing(_))
}

impl Host {
    fn base_baud(&self) -> Option<u32> {
        self.link.judge.base_baud
    }

    /// A raised rate in force.
    pub fn raised(&self) -> bool {
        self.link.judge.raised(self.link.baud)
    }

    /// A raised rate in use outside raise_speed's own trial.
    pub fn speed_in_use(&self) -> bool {
        self.link.judge.in_use(self.link.baud)
    }

    /// The host side of the serial port to `rate` (the probe's `fallback` only when the OS refuses it), what was
    /// read dropped. -> the rate set.
    pub fn set_baud(&mut self, rate: u32, fallback: Option<u32>) -> Result<u32> {
        let mut rate = rate;
        if let Err(e) = self.link.t.set_baud(rate) {
            match fallback {
                Some(f) if f != rate => {
                    self.link.t.set_baud(f)?;
                    rate = f;
                }
                _ => return Err(e),
            }
        }
        self.link.baud = Some(rate);
        std::thread::sleep(SWITCH_SETTLE);
        self.link.discard_input();
        Ok(rate)
    }

    /// A confirm straight on the link: true when its answer came within `timeout` (broken leftovers read past);
    /// `floor`: the last before the host decides none came - it waits at least core §4.4's floor.
    pub fn confirm_raw(&mut self, timeout: Duration, floor: bool) -> Result<bool> {
        let corr = self.next_corr();
        let m = msg::request(corr, host::CORE_FN, host::OP_CONFIRM, 0, host::CONFIRM_BODY);
        self.link.write(&[m])?;
        let timeout = if floor {
            timeout.max(self.wait_for(host::CORE_FN, host::OP_CONFIRM))
        } else {
            timeout
        };
        let deadline = Instant::now() + timeout;
        loop {
            match self.link.recv_for(corr, deadline) {
                Ok(Some(r)) => {
                    self.confirmed(&r);
                    self.failed_transport.clear();
                    return Ok(true);
                }
                Ok(None) | Err(Error::Framing(_)) => return Ok(false),
                Err(Error::Broken(_)) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn confirm_tries(&mut self, tries: usize, each: Duration) -> Result<bool> {
        for i in 0..tries {
            if self.confirm_raw(each, i == tries - 1)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn confirm_within(&mut self, wait: Duration, each: Duration) -> Result<bool> {
        let deadline = Instant::now() + wait;
        loop {
            let last = Instant::now() + each >= deadline;
            if self.confirm_raw(each, last)? {
                return Ok(true);
            }
            if last {
                return Ok(false);
            }
        }
    }

    /// The host at the boot speed again and the probe confirmed there (confirms every 0.25 s up to `wait`).
    pub fn back_to_base(&mut self, wait: Duration) -> Result<bool> {
        self.speed.inflight_cap = 0;
        let Some(base) = self.base_baud() else {
            return Ok(false);
        };
        self.set_baud(base, None)?;
        self.confirm_within(wait, Duration::from_millis(250))
    }

    /// Whether fn's describe declares op in its ops tag (core §7.4; true when it declares none).
    pub fn offers(&mut self, fn_: u16, op: u8) -> Result<bool> {
        let d = self.describe(fn_)?;
        let Some(v) = msg::first(&d, 0x07) else {
            return Ok(true);
        };
        if v.len() < 2 || v[0] as usize + 8 * (v.len() - 1) > 256 {
            return Ok(false);
        }
        let base = v[0] as usize;
        Ok(v[1..]
            .iter()
            .enumerate()
            .any(|(i, b)| (0..8).any(|k| b >> k & 1 == 1 && base + i * 8 + k == op as usize)))
    }

    /// oep.probe.link port_state: (baud, phase) of this host's port; None when not offered. A new request always.
    pub fn port_state(&mut self, fn_: u16) -> Result<Option<(u32, u8)>> {
        if !self.offers(fn_, OP_PORT_STATE)? {
            return Ok(None);
        }
        let r = self.call(fn_, OP_PORT_STATE, &[], false)?;
        let mut rd = Reader::new(&r.payload);
        Ok(Some((rd.u32()?, rd.u8()?)))
    }

    /// How many requests the link keeps in flight: the probe's max_inflight, the raised rate's cap, and on a serial
    /// port the answer-burst bound (6144 bytes over a whole COBS frame of max_frame).
    pub fn inflight_for(&self, limits: &host::Limits) -> usize {
        let cap = if self.speed.inflight_cap > 0 {
            self.speed.inflight_cap
        } else {
            255
        };
        let mut n = limits.max_inflight.min(cap);
        if self.link.framing == Framing::Cobs && limits.max_frame > 0 {
            n = n.min((6144 / crate::cobs::frame_max(limits.max_frame)).max(1));
        }
        n.max(1)
    }

    /// While a raised rate is in force and a session holds the port: a keepalive once the link has been quiet for
    /// 1 s (the probe goes back after 3 s with no good frame); a failure is the next request's to find.
    pub fn keep_raised(&mut self) {
        if !self.raised() || self.session.is_none() || self.speed.keeping {
            return;
        }
        if self.link.last_write.is_some_and(|t| t.elapsed() < KEEPALIVE) {
            return;
        }
        self.speed.keeping = true;
        self.speed.own += 1;
        let _ = self.request(host::CORE_FN, host::OP_KEEPALIVE, &[], true);
        self.speed.own -= 1;
        self.speed.keeping = false;
    }

    /// A probation that passed: noted in the report and the record.
    pub fn speed_events(&mut self) {
        if let Some((rate, moved)) = self.link.judge.passed.take() {
            if let Some(t) = self.speed_trial(rate) {
                t.probation = "passed".into();
                t.probation_bytes = moved;
            }
            self.note(rate, Some(true), "probation");
        }
    }

    fn speed_trial(&mut self, rate: u32) -> Option<&mut Trial> {
        self.speed
            .report
            .as_mut()?
            .trials
            .iter_mut()
            .rev()
            .find(|t| t.committed && t.rate == rate)
    }

    fn note(&mut self, rate: u32, passed: Option<bool>, phase: &str) {
        if let Some((rec, port, unit)) = self.speed.record.as_mut() {
            rec.note(port, unit, rate, passed, phase);
        }
    }

    /// After a request, a pump: a step down that came due (in use) is made now.
    pub fn step_down_if_due(&mut self) -> Result<()> {
        self.speed_events();
        if !self.link.judge.step_due.is_empty() && self.speed_in_use() {
            let why = self.link.judge.step_due.clone();
            self.step_down(&why)?;
        }
        Ok(())
    }

    /// Leave the raised rate: the revert at it (200 ms, never again), the boot speed, a confirm there, then the next
    /// lower candidate.
    fn step_down(&mut self, why: &str) -> Result<()> {
        let rate = self.link.baud.unwrap_or(0);
        let base = self.base_baud().unwrap_or(0);
        self.link.judge.step_due.clear();
        self.link.judge.window.clear();
        let mut lost = false;
        if let (Some(sid), Some(_), Some(fn_)) = (self.session, self.speed.port, self.speed.fn_) {
            let (timeout, resend, fallback) = (self.link.timeout, self.link.resend, self.link.judge.fallback);
            self.link.timeout = STEP_DOWN_WAIT;
            self.link.resend = false;
            self.link.judge.fallback = false;
            self.speed.own += 1;
            let corr = self.next_corr();
            let m = msg::request(corr, fn_, OP_PORT_SPEED, sid, &pack(0, STEP_REVERT, 0));
            let wait = self.wait_for(fn_, OP_PORT_SPEED);
            if let Err(e) = self.send_once(&m, corr, wait) {
                if !link_error(&e) {
                    self.speed.own -= 1;
                    (self.link.timeout, self.link.resend, self.link.judge.fallback) = (timeout, resend, fallback);
                    return Err(e);
                }
                lost = true;
            }
            self.speed.own -= 1;
            (self.link.timeout, self.link.resend, self.link.judge.fallback) = (timeout, resend, fallback);
        }
        if !self.back_to_base(OPEN_RETRY)? {
            return Err(Error::Connection(format!(
                "the probe answers neither at {rate} nor at the boot speed {base}"
            )));
        }
        if lost
            && let Some(fn_) = self.speed.fn_
            && let Some((b, phase)) = self.port_state(fn_)?
            && (b, phase) != (base, 0)
        {
            return Err(Error::Connection(format!(
                "after the revert from {rate}: the port reports {b} in phase {phase}, not the boot speed"
            )));
        }
        self.stepped(rate, why.to_string());
        self.step_lower()
    }

    /// `rate` broke (in its probation or later in use): not used again in this session, nor anything above it.
    fn stepped(&mut self, rate: u32, why: String) {
        let p = self.link.judge.probation.take();
        let in_probation = p.as_ref().is_some_and(|p| p.rate == rate);
        let why = if in_probation && !why.starts_with("in probation") {
            format!("in probation: {why}")
        } else {
            why
        };
        self.speed.unusable.insert(rate, why.clone());
        self.speed.failed.insert(rate, why.clone());
        let base = self.base_baud();
        let ratio = self.link.judge.step_ratio.take();
        if let Some(r) = self.speed.report.as_mut() {
            r.rate = base.unwrap_or(0);
            r.chosen = None;
            r.stepped_down = true;
            r.down_why = why.clone();
            r.step_downs.push(StepDown {
                rate,
                why,
                ratio,
                to: base,
                probation: in_probation,
            });
        }
        if in_probation && let Some(t) = self.speed_trial(rate) {
            t.probation = "failed".into();
        }
        let settling = p.as_ref().is_some_and(|p| p.settling);
        self.note(
            rate,
            if in_probation && settling { None } else { Some(false) },
            if in_probation { "probation" } else { "in_use" },
        );
        self.speed.broke_at = Some(Instant::now());
        self.speed.broke_rate = Some(rate);
    }

    /// After a breakdown in use, at the boot speed: the next lower candidate of the plan that has not failed in this
    /// session gets a fresh try; none left: the boot speed for the rest of the session.
    fn step_lower(&mut self) -> Result<()> {
        let Some((rates, session)) = self.speed.plan.clone() else {
            return Ok(());
        };
        if session != self.speed.unusable_session || self.speed.unusable.is_empty() || session != self.session {
            return Ok(());
        }
        let ceiling = self
            .speed
            .unusable
            .keys()
            .chain(self.speed.failed.keys())
            .min()
            .copied()
            .unwrap_or(0);
        let mut lower: Vec<u32> = rates.into_iter().filter(|r| *r < ceiling).collect();
        lower.sort_unstable_by(|a, b| b.cmp(a));
        lower.dedup();
        if !lower.is_empty() {
            let saved = self.link.judge.fallback;
            self.link.judge.fallback = false;
            self.speed.own += 1;
            let r = self.try_rates(&lower);
            self.speed.own -= 1;
            self.link.judge.fallback = saved;
            r?;
        }
        let baud = self.link.baud;
        if let Some(r) = self.speed.report.as_mut()
            && let Some(last) = r.step_downs.last_mut()
        {
            last.to = baud;
        }
        Ok(())
    }

    /// A request failed (its resend at the raised rate too) while a raised rate was in use: broken frames (or a step
    /// down already due) step down; no answer at all: back to the boot speed, confirmed, then the next lower
    /// candidate. true: send the request again there.
    pub fn speed_fallback(&mut self, e: &Error) -> Result<bool> {
        if !self.speed_in_use() {
            return Ok(false);
        }
        let rate = self.link.baud.unwrap_or(0);
        let base = self.base_baud().unwrap_or(0);
        let due = self.link.judge.step_due.clone();
        if !due.is_empty() || !matches!(e, Error::Timeout(_)) {
            let why = if due.is_empty() {
                format!("frames kept breaking at {rate} ({})", type_name(e))
            } else {
                due
            };
            self.step_down(&why)?;
            return Ok(true);
        }
        if !self.back_to_base(OPEN_RETRY)? {
            return Err(Error::Connection(format!(
                "the probe answers neither at {rate} nor at the boot speed {base}"
            )));
        }
        self.speed.lost_count += 1;
        self.link.judge.step_due.clear();
        self.link.judge.window.clear();
        if let Some(r) = self.speed.report.as_mut() {
            r.lost = true;
        }
        self.stepped(rate, format!("no answer at {rate} (the probe went back by itself)"));
        self.step_lower()?;
        Ok(true)
    }

    /// After an answer: a completed end, or port_speed's revert, puts the probe back at its boot speed at once.
    pub fn after_answer(&mut self, message: &[u8], reply: &[u8]) -> Result<()> {
        if !self.raised() || message.len() < msg::REQUEST_HEADER || reply.len() < 4 || reply[3] != msg::COMPLETED {
            return Ok(());
        }
        let fn_ = u16::from_le_bytes([message[3], message[4]]);
        let op = message[5];
        let at = msg::REQUEST_HEADER + 4;
        let reverts = (fn_ == host::CORE_FN && op == host::OP_END)
            || (Some(fn_) == self.speed.fn_ && op == OP_PORT_SPEED && message.len() > at && message[at] == STEP_REVERT);
        if reverts && let Some(base) = self.base_baud() {
            self.set_baud(base, None)?;
            if let Some(r) = self.speed.report.as_mut() {
                r.rate = base;
                r.chosen = None;
            }
        }
        Ok(())
    }

    /// (oep.probe.link's fn, the port this host's requests come in on) when the probe offers port_speed and that
    /// port is a UART bridge; else why not.
    fn speed_port(&mut self) -> Result<std::result::Result<(u16, u8), String>> {
        let Ok(fn_) = self.find(LINK_NAME, 1) else {
            return Ok(Err("the probe does not offer oep.probe.link".into()));
        };
        if !self.offers(fn_, OP_PORT_SPEED)? {
            return Ok(Err(
                "the probe's oep.probe.link does not offer port_speed (its ops)".into()
            ));
        }
        let index = match &self.limits {
            Some(l) => l.transport,
            None => self.confirm()?.transport,
        };
        let Some(index) = index else {
            return Ok(Err(
                "the probe's confirm names no transport (core §7.1 requires it)".into()
            ));
        };
        if index == RELAYING_BROKER {
            return Ok(Err(
                "a relaying broker answers the confirm (transport 0xFF): no port of this probe to raise".into(),
            ));
        }
        let kind = self
            .describe(host::CORE_FN)?
            .into_iter()
            .find(|(t, v)| *t == TAG_TRANSPORT && v.len() == 3 && v[0] == index)
            .map(|(_, v)| v[1]);
        if kind != Some(UART_BRIDGE) {
            return Ok(Err(format!(
                "this host's transport (index {index}) is not a UART bridge"
            )));
        }
        Ok(Ok((fn_, index)))
    }

    /// port_speed by the full form (oep-client-python's `raise_speed(hst, candidates, flows=, record=, max_tries=)`):
    /// try `candidates` in order and commit the first that passes. The session must be open. `record`: the record of
    /// this port and probe orders and filters them, and this run's outcomes are written.
    pub fn raise_speed(
        &mut self,
        candidates: &[u32],
        flows: &[(&'static str, usize)],
        record: bool,
        max_tries: Option<usize>,
    ) -> Result<()> {
        let base = self.base_baud();
        let mut report = Report {
            base: base.unwrap_or(0),
            supported: false,
            rate: self.link.baud.unwrap_or(0),
            ..Default::default()
        };
        if self.link.framing != Framing::Cobs || self.link.t.port_path.is_none() || base.is_none() {
            report.why = "the link is not a serial port this host opened".into();
            self.speed.report = Some(report);
            return Ok(());
        }
        let (fn_, port) = match self.speed_port()? {
            Ok(x) => x,
            Err(why) => {
                report.why = why;
                self.speed.report = Some(report);
                return Ok(());
            }
        };
        self.speed.fn_ = Some(fn_);
        let Some(session) = self.session else {
            self.speed.report = Some(report);
            return Err(Error::Runtime(
                "raise_speed needs an open session (the rate lasts as long as the session)".into(),
            ));
        };
        report.supported = true;
        report.verified = true;
        let lease = self.lease_ms.unwrap_or(0);
        let wait = if lease > 0 {
            VERIFY_MS.min(lease.saturating_sub(1000).max(500))
        } else {
            VERIFY_MS
        }
        .min(65535);
        if self.speed.unusable_session != Some(session) {
            self.speed.unusable.clear();
            self.speed.failed.clear();
            self.speed.unusable_session = Some(session);
            self.speed.broke_at = None;
            self.speed.broke_rate = None;
        }
        self.speed.record = None;
        if record {
            let unit = self
                .describe(host::CORE_FN)?
                .into_iter()
                .find(|(t, _)| *t == host::TAG_UNIT_ID)
                .map(|(_, v)| String::from_utf8_lossy(&v).to_string())
                .unwrap_or_else(|| "?".into());
            if Record::names_a_unit(&unit) {
                let port_path = self.link.t.port_path.clone().unwrap_or_else(|| "<stream>".into());
                self.speed.record = Some((Record::open(None), port_path, unit));
            } else {
                report.why = format!("unit_id {unit} names no unit (core §7.5): no speed record");
            }
        }
        self.speed.port = Some(port);
        self.speed.report = Some(report);
        self.speed.run = Some(Run {
            flows: flows.to_vec(),
            frames: FLOW_FRAMES,
            wait,
            probation_bytes: PROBATION_BYTES,
            probation_s: PROBATION_S,
            settle_s: SETTLE_S,
            size: 0,
            n_max: 1,
        });
        self.link.judge.fallback = false;
        let r = self.raise(candidates.to_vec(), max_tries);
        self.link.judge.fallback = true;
        r
    }

    fn report(&mut self) -> &mut Report {
        self.speed.report.get_or_insert_with(Report::default)
    }

    fn barred(&self, rate: u32) -> String {
        if let Some(why) = self.speed.unusable.get(&rate) {
            return format!("broke in use earlier in this session ({why})");
        }
        if let Some(&ceiling) = self.speed.unusable.keys().min()
            && rate > ceiling
        {
            return format!("above {ceiling}, which broke in use in this session");
        }
        String::new()
    }

    fn raise(&mut self, mut candidates: Vec<u32>, max_tries: Option<usize>) -> Result<()> {
        let limits = match self.limits.clone() {
            Some(l) => l,
            None => self.confirm()?,
        };
        let n_max = self.inflight_for(&limits);
        if let Some(run) = self.speed.run.as_mut() {
            run.size = limits.max_frame.saturating_sub(SOURCE_OVERHEAD).max(1);
            run.n_max = n_max;
        }
        if let Some((rec, port, unit)) = self.speed.record.as_ref() {
            let (passed, failed) = rec.lookup(port, unit);
            let report = self.speed.report.as_mut().expect("a report");
            if !candidates.is_empty() && candidates.iter().all(|r| failed.contains(r)) {
                let slowest = *candidates.iter().min().expect("candidates");
                report.retried = Some(slowest);
                report.skipped = candidates.iter().copied().filter(|r| *r != slowest).collect();
                candidates = vec![slowest];
            } else {
                report.skipped = candidates.iter().copied().filter(|r| failed.contains(r)).collect();
                let mut c: Vec<u32> = candidates.iter().copied().filter(|r| passed.contains(r)).collect();
                c.extend(
                    candidates
                        .iter()
                        .copied()
                        .filter(|r| !passed.contains(r) && !failed.contains(r)),
                );
                candidates = c;
            }
        }
        let mut rates = vec![];
        for rate in candidates {
            let why = self.barred(rate);
            if why.is_empty() {
                rates.push(rate);
            } else {
                self.report().trials.push(Trial {
                    rate,
                    why,
                    ..Default::default()
                });
            }
        }
        if let Some(mt) = max_tries
            && rates.len() > mt
        {
            self.report().capped = rates.split_off(mt);
        }
        if rates.is_empty() {
            return Ok(());
        }
        // the full form: the flows at their n, the boot speed's baseline
        let mut run = self.speed.run.clone().expect("a run");
        run.flows = run
            .flows
            .iter()
            .map(|&(f, n)| (f, if n == 0 { n_max } else { n.min(n_max) }.max(1)))
            .collect();
        let why = self.baseline(&mut run)?;
        self.speed.run = Some(run);
        if !why.is_empty() {
            self.report().why = why;
            return Ok(());
        }
        self.speed.plan = Some((rates.clone(), self.session));
        self.try_rates(&rates)
    }

    /// The boot speed's ratio per flow: this session's frames at the boot speed when 60 or more (and under 10 %
    /// broken or lost), else 60 measured per flow at its n (over 10 %: again at n = 1, which then caps it). -> "" or
    /// why the port is not raised at all.
    fn baseline(&mut self, run: &mut Run) -> Result<String> {
        let c = self.link.judge.base_counts;
        let total = c.iter().sum::<u64>();
        if total >= BASELINE_FRAMES && (c[1] + c[2]) as f64 / total as f64 <= BASELINE_MAX {
            let ratio = (c[1] + c[2]) as f64 / total as f64;
            let r = self.report();
            r.baseline_frames = total;
            r.baseline = run.flows.iter().map(|(f, _)| (f.to_string(), ratio)).collect();
            return Ok(String::new());
        }
        let base = self.base_baud().unwrap_or(0);
        for i in 0..run.flows.len() {
            let (flow, n) = run.flows[i];
            let mut res = self.flow_run(flow, n, run.size, BASELINE_FRAMES, base, 0.0)?;
            self.report().baseline_flows.push(res.clone());
            if res.ratio() > BASELINE_MAX && n > 1 {
                res = self.flow_run(flow, 1, run.size, BASELINE_FRAMES, base, 0.0)?;
                self.report().baseline_flows.push(res.clone());
                if res.ratio() <= BASELINE_MAX {
                    run.flows[i] = (flow, 1);
                }
            }
            if res.ratio() > BASELINE_MAX {
                return Ok(format!(
                    "the boot speed {base} itself loses {} of {} frames (over {}): not raised",
                    pct(res.ratio(), 0),
                    res.name(),
                    pct(BASELINE_MAX, 0)
                ));
            }
            self.report().baseline.push((flow.to_string(), res.ratio()));
        }
        Ok(String::new())
    }

    /// `frames` of one flow (and on until `min_s` has passed) at the rate in force, `n` in flight, `size` bytes
    /// each: broken = an answer that came wrong, lost = none came (a broken COBS frame is read as one). After lost
    /// frames a confirm puts the link in step; none answered: the probe went back, the flow stops (`gone`).
    fn flow_run(
        &mut self,
        flow: &'static str,
        n: usize,
        size: usize,
        frames: u64,
        rate: u32,
        min_s: f64,
    ) -> Result<FlowResult> {
        let mut res = FlowResult {
            flow,
            n,
            frames: 0,
            broken: 0,
            lost: 0,
            kb_s: 0.0,
            passed: false,
            gone: false,
        };
        let limits = match self.limits.clone() {
            Some(l) => l,
            None => self.confirm()?,
        };
        let fn_ = self.speed.fn_.unwrap_or(0);
        let data: Vec<u8> = (0..size).map(|k| (k & 0xFF) as u8).collect();
        let sink_n = data
            .len()
            .min(limits.max_frame.saturating_sub(msg::REQUEST_HEADER + 2).max(1));
        let (timeout, resend) = (self.link.timeout, self.link.resend);
        let answer = (4.0 * (size + 24) as f64 * 10.0 / rate.max(1) as f64 * n as f64 + 0.1).max(0.3);
        self.link.timeout = Duration::from_secs_f64(answer);
        self.link.resend = false;
        self.keep_once();
        let (mut moved, t0) = (0u64, Instant::now());
        let out = (|| -> Result<()> {
            while res.frames < frames || t0.elapsed().as_secs_f64() < min_s {
                let batch = if res.frames < frames {
                    (2 * n as u64).min(frames - res.frames)
                } else {
                    2 * n as u64
                };
                let kinds: Vec<&str> = (0..batch)
                    .map(|k| match flow {
                        "duplex" => {
                            if (res.frames + k).is_multiple_of(2) {
                                "in"
                            } else {
                                "out"
                            }
                        }
                        f => f,
                    })
                    .collect();
                let msgs: Vec<Vec<u8>> = kinds
                    .iter()
                    .map(|k| {
                        let corr = self.next_corr();
                        if *k == "in" {
                            msg::request(corr, fn_, OP_SOURCE, 0, &(size as u32).to_le_bytes())
                        } else {
                            let mut p = (sink_n as u16).to_le_bytes().to_vec();
                            p.extend_from_slice(&data[..sink_n]);
                            msg::request(corr, fn_, OP_SINK, 0, &p)
                        }
                    })
                    .collect();
                let mut replies = vec![];
                let wait = self.wait_for(fn_, OP_SOURCE);
                if let Err(e) = self.link.exchange_once(&msgs, n, limits.window, wait, &mut replies)
                    && !link_error(&e)
                {
                    return Err(e);
                }
                for (kind, raw) in kinds.iter().zip(&replies) {
                    if answer_good(kind, raw, &data) {
                        moved += size as u64;
                    } else {
                        res.broken += 1;
                    }
                }
                res.lost += (msgs.len() - replies.len()) as u64;
                res.frames += msgs.len() as u64;
                if replies.len() < msgs.len() && !self.confirm_tries(CONFIRM_TRIES, CONFIRM_WAIT)? {
                    res.lost += frames.saturating_sub(res.frames);
                    res.frames = res.frames.max(frames);
                    res.gone = true;
                    break;
                }
            }
            Ok(())
        })();
        (self.link.timeout, self.link.resend) = (timeout, resend);
        out?;
        let s = t0.elapsed().as_secs_f64();
        res.kb_s = if s > 0.0 { moved as f64 / s / 1000.0 } else { 0.0 };
        Ok(res)
    }

    /// A keepalive at the rate in force, no resend (the lease outlasts a flow); a failure is the flow's to find.
    fn keep_once(&mut self) {
        let Some(sid) = self.session else { return };
        let resend = self.link.resend;
        self.link.resend = false;
        self.speed.own += 1;
        let corr = self.next_corr();
        let m = msg::request(corr, host::CORE_FN, host::OP_KEEPALIVE, sid, &[]);
        let wait = self.wait_for(host::CORE_FN, host::OP_KEEPALIVE);
        let _ = self.send_once(&m, corr, wait);
        self.speed.own -= 1;
        self.link.resend = resend;
    }

    fn verify_flows(&mut self, rate: u32, trial: &mut Trial, run: &Run) -> Result<bool> {
        for &(flow, n) in &run.flows {
            let threshold = (2.0 * self.report().baseline_of(flow)).max(VERIFY_FLOOR);
            let ks: Vec<usize> = if n > 1 { vec![n, 1] } else { vec![n] };
            let mut last = None;
            for k in ks {
                let mut res = self.flow_run(flow, k, run.size, run.frames, rate, 0.0)?;
                res.passed = !res.gone && !(res.broken + res.lost >= FLOW_FAIL_MIN && res.ratio() > threshold);
                trial.flows.push(res.clone());
                let stop = res.passed || res.gone;
                last = Some((k, res));
                if stop {
                    break;
                }
            }
            let (k, res) = last.expect("a flow ran");
            if res.gone {
                trial.why = format!("{}: no answer at {rate} any more (the probe went back)", res.name());
                return Ok(false);
            }
            if !res.passed {
                trial.why = format!(
                    "{}: {} of {} frames broken or lost ({}, over {})",
                    res.name(),
                    res.broken + res.lost,
                    res.frames,
                    pct(res.ratio(), 0),
                    pct(threshold, 0)
                );
                return Ok(false);
            }
            if k < n {
                trial.n_cap = 1;
            }
        }
        Ok(true)
    }

    fn verify_fast(
        &mut self,
        rate: u32,
        trial: &mut Trial,
        flows: &[(&'static str, usize)],
        size: usize,
    ) -> Result<bool> {
        for &(flow, n) in flows {
            let threshold = (2.0 * self.report().baseline_of(flow)).max(VERIFY_FLOOR);
            let mut res = self.flow_run(flow, n, size, FLOW_FRAMES, rate, FAST_VERIFY_S)?;
            res.passed = !res.gone && !(res.broken + res.lost >= FLOW_FAIL_MIN && res.ratio() > threshold);
            trial.flows.push(res.clone());
            if res.gone {
                trial.why = format!("{}: no answer at {rate} any more (the probe went back)", res.name());
                return Ok(false);
            }
            if !res.passed {
                trial.why = format!(
                    "{}: {} of {} full frames in 1 s broken or lost ({}, over {})",
                    res.name(),
                    res.broken + res.lost,
                    res.frames,
                    pct(res.ratio(), 0),
                    pct(threshold, 0)
                );
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A candidate that did not pass: revert at its rate (its answer need not come), the boot speed, confirmed.
    fn back(&mut self, rate: u32, revert: bool, wait_ms: u32) -> Result<()> {
        let base = self.base_baud().unwrap_or(0);
        let fn_ = self.speed.fn_.unwrap_or(0);
        if revert {
            let (timeout, resend) = (self.link.timeout, self.link.resend);
            self.link.timeout = Duration::from_millis(300);
            self.link.resend = false;
            self.speed.own += 1;
            let r = self.call(fn_, OP_PORT_SPEED, &pack(rate, STEP_REVERT, 0), true);
            self.speed.own -= 1;
            (self.link.timeout, self.link.resend) = (timeout, resend);
            match r {
                Ok(_) | Err(Error::Rejected(..)) => {}
                Err(e) if link_error(&e) => {}
                Err(e) => return Err(e),
            }
        }
        let wait = OPEN_RETRY.max(Duration::from_secs_f64(wait_ms as f64 / 1000.0 + 1.0));
        if !self.back_to_base(wait)? {
            return Err(Error::Connection(format!(
                "after trying {rate}: no answer at the boot speed {base}"
            )));
        }
        if let Some((b, phase)) = self.port_state(fn_)?
            && (b, phase) != (base, 0)
        {
            return Err(Error::Connection(format!(
                "after trying {rate}: the port reports {b} in phase {phase}, not the boot speed"
            )));
        }
        self.report().rate = base;
        Ok(())
    }

    fn trial_note(&mut self, trial: &Trial, passed: bool, phase: &str) {
        self.note(
            trial.rate,
            if trial.settling && !passed { None } else { Some(passed) },
            phase,
        );
    }

    fn trial_failed(&mut self, trial: &Trial, phase: &str) {
        self.speed.failed.insert(trial.rate, trial.why.clone());
        self.trial_note(trial, false, phase);
        self.speed.broke_at = Some(Instant::now());
        self.speed.broke_rate = Some(trial.rate);
    }

    /// Each of `rates` in order until one is committed (raise_speed's own loop, also a step down's in use).
    fn try_rates(&mut self, rates: &[u32]) -> Result<()> {
        let run = self.speed.run.clone().expect("a run");
        let fn_ = self.speed.fn_.unwrap_or(0);
        for &rate in rates {
            let mut trial = Trial {
                rate,
                ..Default::default()
            };
            trial.why = self.barred(rate);
            if !trial.why.is_empty() {
                self.report().trials.push(trial);
                continue;
            }
            trial.settling = self
                .speed
                .broke_at
                .is_some_and(|t| t.elapsed().as_secs_f64() < run.settle_s)
                && self.speed.broke_rate != Some(rate);
            let fast = rate > DEFAULT_CEILING;
            let fast_flows: Vec<(&'static str, usize)> = if fast {
                let given = |f: &str| run.flows.iter().find(|x| x.0 == f).map(|x| x.1);
                let mut v = vec![
                    ("in", given("in").unwrap_or(run.n_max)),
                    ("out", given("out").unwrap_or(run.n_max)),
                ];
                if let Some(n) = given("duplex") {
                    v.push(("duplex", n));
                }
                v
            } else {
                vec![]
            };
            let wait = if fast {
                run.wait
                    .max((2.0 * FAST_VERIFY_S * 1000.0) as u32 * fast_flows.len() as u32)
                    .min(65535)
            } else {
                run.wait
            };
            if fast
                && let Some(lease) = self.lease_ms.filter(|l| *l > 0)
                && lease.saturating_sub(1000) < wait
            {
                trial.why = format!(
                    "above {DEFAULT_CEILING}: its 1 s verify each way needs verify_ms {wait} and a lease of {} ms or \
                     more (this session's: {lease} ms)",
                    wait + 1000
                );
                self.report().trials.push(trial);
                continue;
            }
            let at = self.report().trials.len();
            self.report().trials.push(trial.clone());
            let r = self.call(fn_, OP_PORT_SPEED, &pack(rate, STEP_TRY, wait as u16), true);
            let reply = match r {
                Ok(r) => r,
                Err(Error::Timeout(_)) => {
                    trial.why = "no answer to the try".into();
                    self.report().trials[at] = trial;
                    self.back(rate, false, wait)?;
                    continue;
                }
                Err(Error::Rejected(msg::UNKNOWN_OPERATION, _)) => {
                    let r = self.report();
                    r.supported = false;
                    r.why = "the probe does not take port_speed (unknown_operation)".into();
                    r.trials.truncate(at);
                    return Ok(());
                }
                Err(e @ Error::Rejected(..)) if !e.is_rejected(msg::UNSUPPORTED) => {
                    trial.why = e.to_string();
                    self.report().trials[at] = trial;
                    break;
                }
                Err(Error::Rejected(..)) => {
                    trial.why = "unsupported: the probe's UART cannot make it".into();
                    self.trial_note(&trial, false, "try");
                    self.report().trials[at] = trial;
                    continue;
                }
                Err(e) => return Err(e),
            };
            let actual = Reader::new(&reply.payload).u32()?;
            trial.actual = Some(actual);
            match self.set_baud(rate, Some(actual)) {
                Ok(b) => trial.switched = Some(b),
                Err(e) => {
                    trial.why = format!("the OS refuses {rate} and {actual}: {e}");
                    self.link.baud = Some(rate);
                    std::thread::sleep(Duration::from_millis(wait as u64));
                    self.report().trials[at] = trial.clone();
                    self.back(rate, false, wait)?;
                    self.trial_note(&trial, false, "try");
                    continue;
                }
            }
            if !self.confirm_tries(CONFIRM_TRIES, CONFIRM_WAIT)? {
                trial.why = "no confirm at the new rate".into();
                self.report().trials[at] = trial.clone();
                self.back(rate, true, wait)?;
                self.trial_failed(&trial, "confirm");
                continue;
            }
            let verified = if fast {
                self.verify_fast(rate, &mut trial, &fast_flows, run.size)?
            } else {
                self.verify_flows(rate, &mut trial, &run)?
            };
            self.report().trials[at] = trial.clone();
            if !verified {
                self.back(rate, true, wait)?;
                self.trial_failed(&trial, "verify");
                continue;
            }
            match self.call(fn_, OP_PORT_SPEED, &pack(rate, STEP_COMMIT, 0), true) {
                Ok(_) => {}
                Err(e @ (Error::Timeout(_) | Error::Rejected(..))) => {
                    trial.why = format!("the commit failed: {e}");
                    self.report().trials[at] = trial;
                    self.back(rate, false, wait)?;
                    continue;
                }
                Err(e) => return Err(e),
            }
            trial.committed = true;
            let baseline = self.report().baseline.iter().map(|x| x.1).fold(0.0, f64::max);
            {
                let r = self.report();
                r.rate = rate;
                r.chosen = Some(rate);
            }
            self.speed.inflight_cap = trial.n_cap;
            let j = &mut self.link.judge;
            j.baseline_ratio = baseline;
            j.window.clear();
            j.step_due.clear();
            self.trial_note(&trial, true, "verify");
            if run.probation_bytes > 0 || run.probation_s > 0.0 {
                trial.probation = "running".into();
                self.link.judge.probation = Some(Probation {
                    rate,
                    bytes: run.probation_bytes,
                    seconds: run.probation_s,
                    threshold: (2.0 * baseline).max(VERIFY_FLOOR),
                    started: Instant::now(),
                    settling: trial.settling,
                    moved: 0,
                    frames: 0,
                    bad: 0,
                });
            } else {
                trial.probation = "off".into();
                self.link.judge.probation = None;
            }
            self.report().trials[at] = trial;
            return Ok(());
        }
        Ok(())
    }
}

impl Host {
    /// A capture's raise on a session it opened (acquire's `_capture`): `fast` None - the default, left to the
    /// record of this port and probe (FAST, at most PER_SESSION tried, the read-back flow verified); empty - the boot
    /// speed; rates - those, verified the same way. Once per session.
    pub fn raise_for_capture(&mut self, fast: Option<&[u32]>) -> Result<()> {
        if self.session.is_none() || self.speed.done == self.session {
            return Ok(());
        }
        self.speed.done = self.session;
        const FLOWS: [(&str, usize); 1] = [("in", 0)];
        match fast {
            None => {
                self.speed.by_record = true;
                if self.link.t.port_path.is_none() {
                    return Ok(());
                }
                self.speed.asked = FAST.to_vec();
                self.raise_speed(&FAST, &FLOWS, true, Some(PER_SESSION))?;
                self.speed.remembered = self
                    .speed
                    .report
                    .as_ref()
                    .is_some_and(|r| !r.skipped.is_empty() || r.retried.is_some());
            }
            Some(rates) => {
                self.speed.asked = rates.to_vec();
                if !rates.is_empty() {
                    self.raise_speed(rates, &FLOWS, false, None)?;
                }
            }
        }
        Ok(())
    }
}

/// A source answer that is completed success with `data` exactly, or a sink answer that is completed success.
fn answer_good(kind: &str, raw: &[u8], data: &[u8]) -> bool {
    let Ok(r) = msg::Reply::parse(raw) else {
        return false;
    };
    if !r.succeeded() {
        return false;
    }
    if kind != "in" {
        return true;
    }
    let mut rd = Reader::new(&r.payload);
    match rd.counted16() {
        Ok(got) => got == data && rd.tail().is_ok(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_go_both_ways() {
        for s in [0i64, 1_764_057_600, 1_764_057_600 + 3661, 951_782_400] {
            assert_eq!(parse_iso(&iso_utc(s)), Some(s));
        }
        assert_eq!(iso_utc(1_764_057_600), "2025-11-25T08:00:00+00:00");
        assert_eq!(parse_iso("2025-11-25T09:00:00+01:00"), Some(1_764_057_600));
        assert_eq!(parse_iso("2025-11-25T08:00:00"), Some(1_764_057_600));
        assert_eq!(parse_iso("2025-11-25T08:00:00.123456+00:00"), Some(1_764_057_600));
    }

    #[test]
    fn the_record_orders_and_expires() {
        let dir = std::env::temp_dir().join(format!("ws-speed-{}", std::process::id()));
        let path = dir.join("link-speed.json");
        let _ = std::fs::remove_file(&path);
        let mut r = Record::open(Some(path.clone()));
        r.note("/dev/x", "abc", 921_600, Some(true), "verify");
        r.note("/dev/x", "abc", 1_500_000, Some(false), "verify");
        r.note("/dev/x", "abc", 500_000, None, "probation");
        r.note("/dev/x", "x-1", 500_000, Some(true), "verify");
        let again = Record::open(Some(path.clone()));
        assert_eq!(again.lookup("/dev/x", "abc"), (vec![921_600], vec![1_500_000]));
        assert!(again.data.get("/dev/x|x-1").is_none());
        // an old failure is gone, an old pass of 2 days stays
        let mut old = again.clone();
        let rates = old.data["/dev/x|abc"]["rates"]
            .as_object_mut()
            .map(|m| m.clone())
            .unwrap();
        let mut rates = rates;
        rates.get_mut("1500000").unwrap()["at"] = json!(iso_utc(now_s() - 2 * 86400));
        rates.get_mut("921600").unwrap()["at"] = json!(iso_utc(now_s() - 2 * 86400));
        old.data.get_mut("/dev/x|abc").unwrap()["rates"] = Value::Object(rates);
        assert_eq!(old.lookup("/dev/x", "abc"), (vec![921_600], vec![]));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("{\n \"/dev/x|abc\": {\n  \"port\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_judge_steps_down_in_probation_and_in_use() {
        let mut j = Judge {
            base_baud: Some(115_200),
            held: true,
            ..Default::default()
        };
        let b = Some(921_600);
        j.count(Kind::Good, Some(115_200));
        assert_eq!(j.base_counts, [1, 0, 0]);
        j.probation = Some(Probation {
            rate: 921_600,
            bytes: 10,
            seconds: 0.0,
            threshold: 0.05,
            started: Instant::now(),
            settling: false,
            moved: 0,
            frames: 0,
            bad: 0,
        });
        for _ in 0..10 {
            j.count(Kind::Good, b);
        }
        for _ in 0..3 {
            j.count(Kind::Broken, b);
        }
        assert!(
            j.step_due
                .starts_with("in probation: 3 of 13 frames broken or lost at 921600 after 0 bytes (23.1%")
        );
        let mut j = Judge {
            base_baud: Some(115_200),
            held: true,
            ..Default::default()
        };
        for k in 0..60 {
            j.count(if k % 5 == 0 { Kind::Lost } else { Kind::Good }, b);
        }
        assert_eq!(
            j.step_due,
            "11 of 51 frames broken or lost within 3 s at 921600 (21.6%, over 10%)"
        );
    }
}
