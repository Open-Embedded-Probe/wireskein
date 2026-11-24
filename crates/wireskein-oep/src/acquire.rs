//! One capture from a probe, put together (oep-client-python's `acquire`): the request in; the parts with their
//! losses, times and the probe's facts out. Logic channels (oep.fixture.logic): one-shot, repeat and stream, capture
//! format 0 and format 1's reductions, a trigger on a logic line, the glitch filter.

use crate::assemble::{self, Chan, Format, Loss, Part, Spool};
use crate::blocks;
use crate::capture::{self, Config, Logic, Setting};
use crate::host::{self, ClockReading, Host};
use crate::link::Link;
use crate::msg::{self, Error, Result};
use crate::transport;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const REDUCTIONS: [&str; 4] = ["sample", "any_high", "any_low", "change_latch"];
pub const TRIGGERS: [&str; 5] = ["high", "low", "rise", "fall", "both"];
/// a stream's notifications: at least this much a frame, or after this long (core §10.3)
const STREAM_MIN_BYTES: u16 = 16384;
const STREAM_MAX_DELAY_MS: u32 = 20;
const STREAM_POLL: Duration = Duration::from_millis(200);
const LEASE_MS: u32 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    OneShot,
    Stream,
    Repeat,
}

impl Mode {
    pub fn parse(s: &str) -> Result<Mode> {
        Ok(match s {
            "oneshot" => Mode::OneShot,
            "stream" => Mode::Stream,
            "repeat" => Mode::Repeat,
            _ => return Err(Error::Value(format!("mode {s:?}: oneshot, stream or repeat"))),
        })
    }
}

/// What to capture (acquire.Request's logic part).
#[derive(Debug, Clone)]
pub struct Request {
    /// (name to record, the probe's channel name)
    pub channels: Vec<(String, String)>,
    pub rate_hz: u64,
    pub samples: u64,
    /// (logic channel, one of TRIGGERS)
    pub trigger: Option<(String, String)>,
    pub pretrigger: Option<u64>,
    /// trigger_opts arm 0: the trigger looked for at once
    pub arm_now: bool,
    pub timeout_s: f64,
    /// name -> (reduction, D)
    pub resolution: Vec<(String, (String, u64))>,
    pub mode: Mode,
    pub duration_s: Option<f64>,
    pub segments: Option<u64>,
    /// stream: stop at the first loss (else go on to the end)
    pub on_loss_stop: bool,
    /// stream: where its channels are put together on disk while it runs (None: the system's temporary directory)
    pub spool_dir: Option<PathBuf>,
    pub glitch_filter_ns: Option<u32>,
    pub input_hysteresis: Option<bool>,
}

impl Request {
    pub fn new(channels: Vec<(String, String)>, rate_hz: u64, samples: u64) -> Request {
        Request {
            channels,
            rate_hz,
            samples,
            trigger: None,
            pretrigger: None,
            arm_now: false,
            timeout_s: 10.0,
            resolution: vec![],
            mode: Mode::OneShot,
            duration_s: None,
            segments: None,
            on_loss_stop: true,
            spool_dir: None,
            glitch_filter_ns: None,
            input_hysteresis: None,
        }
    }

    /// acquire.Request's checks.
    pub fn check(&self) -> Result<()> {
        let names: Vec<&str> = self.channels.iter().map(|(n, _)| n.as_str()).collect();
        let mut u = names.clone();
        u.sort();
        u.dedup();
        if names.is_empty() || u.len() != names.len() {
            return Err(Error::Value(format!(
                "channel names must be given and differ: {names:?}"
            )));
        }
        if let Some((name, kind)) = &self.trigger
            && (!names.contains(&name.as_str()) || !TRIGGERS.contains(&kind.as_str()))
        {
            return Err(Error::Value(format!(
                "trigger {name}:{kind}: the channel must be one captured, the kind one of {}",
                TRIGGERS.join(", ")
            )));
        }
        match self.mode {
            Mode::Repeat if !self.segments.is_some_and(|s| s >= 1) || self.samples == 0 => {
                return Err(Error::Value(
                    "repeat needs samples and segments (how many acquisitions, 1 or more)".into(),
                ));
            }
            Mode::OneShot | Mode::Stream if self.segments.is_some() => {
                return Err(Error::Value("segments is for repeat only".into()));
            }
            Mode::Stream if !self.duration_s.is_some_and(|d| d > 0.0) => {
                return Err(Error::Value("a stream needs duration_s (how long)".into()));
            }
            Mode::Stream if self.trigger.is_some() => {
                return Err(Error::Value("a stream is started at once: no trigger".into()));
            }
            _ => {}
        }
        if self.glitch_filter_ns == Some(0) {
            return Err(Error::Value("glitch_filter_ns 0: 1 or more (None: no filter)".into()));
        }
        for (name, (mode, d)) in &self.resolution {
            if !names.contains(&name.as_str()) || !REDUCTIONS.contains(&mode.as_str()) || *d < 2 {
                return Err(Error::Value(format!(
                    "{name}=({mode:?}, {d}): a resolution is for a logic channel captured, (one of {}, 2 or more \
                     samples)",
                    REDUCTIONS.join(", ")
                )));
            }
        }
        Ok(())
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

/// Where a probe is: an OEP address (oep://UNIT_ID[?via=usb|bulk|tcp:HOST:PORT]) or tcp://HOST:PORT.
#[derive(Debug, Clone, PartialEq)]
pub enum Address {
    Usb {
        unit: String,
    },
    Tcp {
        host: String,
        port: u16,
        unit: Option<String>,
    },
}

impl Address {
    /// The probe itself without its route (host guide §5.3), when its unit_id is known.
    pub fn place(&self) -> Option<String> {
        match self {
            Address::Usb { unit } | Address::Tcp { unit: Some(unit), .. } => Some(format!("oep://{unit}")),
            _ => None,
        }
    }
    pub fn unit(&self) -> Option<&str> {
        match self {
            Address::Usb { unit } | Address::Tcp { unit: Some(unit), .. } => Some(unit),
            _ => None,
        }
    }
}

fn host_port(s: &str, what: &str) -> Result<(String, u16)> {
    let (h, p) = s
        .rsplit_once(':')
        .ok_or_else(|| Error::Value(format!("{what}: HOST:PORT (a port is needed here)")))?;
    let port = p
        .parse::<u16>()
        .map_err(|_| Error::Value(format!("{what}: {p:?} is not a port")))?;
    let h = h.trim_start_matches('[').trim_end_matches(']');
    Ok((
        if h.is_empty() {
            "127.0.0.1".into()
        } else {
            h.to_string()
        },
        port,
    ))
}

/// The addresses this client opens: None for one it does not (a serial port, a HID or CDC route, DNS-SD, a broker:
/// oep-client-python's).
pub fn parse_address(text: &str) -> Result<Option<Address>> {
    if let Some(rest) = text.strip_prefix("tcp://") {
        let (host, port) = host_port(rest.trim_end_matches('/'), text)?;
        return Ok(Some(Address::Tcp { host, port, unit: None }));
    }
    let Some(rest) = text
        .get(..6)
        .filter(|p| p.eq_ignore_ascii_case("oep://"))
        .map(|_| &text[6..])
    else {
        return Ok(None);
    };
    let (place, query) = rest.split_once('?').unwrap_or((rest, ""));
    let unit = place.split('/').next().unwrap_or("").to_string();
    if unit.is_empty() || !unit.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.') {
        return Err(Error::Value(format!(
            "{text}: the place is <probe>[/<port>[/<target>]]"
        )));
    }
    if unit.to_ascii_lowercase().starts_with("x-") {
        return Err(Error::Value(format!(
            "{text}: an x- unit_id names no unit (core §7.5), so no probe is found by it"
        )));
    }
    let via = match query {
        "" => None,
        q => Some(
            q.strip_prefix("via=")
                .ok_or_else(|| Error::Value(format!("{text}: the only query is one via=<route> (host guide §5.3)")))?,
        ),
    };
    Ok(match via {
        None | Some("usb") | Some("bulk") => Some(Address::Usb { unit }),
        Some(v) if v.starts_with("tcp:") => {
            let (host, port) = host_port(&v[4..], text)?;
            Some(Address::Tcp {
                host,
                port,
                unit: Some(unit),
            })
        }
        _ => None,
    })
}

/// Open the link to the probe at `addr` and confirm it (the probing rule, transports §3); an oep:// address's
/// unit_id checked against describe's.
pub fn open(addr: &Address) -> Result<Host> {
    let t = match addr {
        Address::Usb { unit } => {
            let info = transport::find_usb(unit)?;
            transport::open_usb(&info, transport::URB_SIZE, transport::DEPTH)?
        }
        Address::Tcp { host, port, .. } => transport::open_tcp(host, *port)?,
    };
    let quiet = !t.keeps_boundaries;
    let mut link = Link::new(t);
    if quiet {
        // pushes of a session before may still come: they stop with its lease (transports §5)
        link.discard_until_quiet(Duration::from_millis(50), Duration::from_secs(61));
    }
    let mut hst = Host::new(link);
    hst.confirm().map_err(|e| {
        Error::Runtime(format!(
            "no valid confirm answer ({e}); closed, nothing else sent (transports §3)"
        ))
    })?;
    if let Some(unit) = addr.unit() {
        let said = hst
            .describe(host::CORE_FN)?
            .into_iter()
            .find(|(t, _)| *t == host::TAG_UNIT_ID)
            .map(|(_, v)| String::from_utf8_lossy(&v).to_string());
        if said.as_deref().map(str::to_ascii_lowercase) != Some(unit.to_ascii_lowercase()) {
            return Err(Error::Runtime(format!(
                "the probe named as unit_id {unit} says unit_id {said:?} in describe; closed"
            )));
        }
    }
    Ok(hst)
}

/// ASCII case folded (names compare ignoring ASCII case).
fn fold(s: &str) -> String {
    s.to_ascii_lowercase()
}

/// The probe's channel numbers of lines named by the probe's channel names (fn 0's channel_name, core §7.5).
fn channel_ids(hst: &mut Host, lines: &[String]) -> Result<Vec<u16>> {
    let mut names: Vec<(String, u16)> = vec![];
    let mut shown = vec![];
    for (t, v) in hst.describe(host::CORE_FN).unwrap_or_default() {
        if t == host::TAG_CHANNEL_NAME && v.len() >= 2 {
            let name = String::from_utf8_lossy(&v[2..]).to_string();
            names.push((fold(&name), u16::from_le_bytes([v[0], v[1]])));
            shown.push(name);
        }
    }
    let mut out = vec![];
    for line in lines {
        let found: Vec<u16> = names
            .iter()
            .filter(|(n, _)| *n == fold(line))
            .map(|(_, c)| *c)
            .collect();
        if found.len() != 1 {
            let known = shown.iter().take(8).cloned().collect::<Vec<_>>().join(", ")
                + if shown.len() > 8 { ", ..." } else { "" };
            return Err(Error::Value(format!(
                "{line}: {} of that name (the probe's channel names: {}; or a line label S.N)",
                if found.is_empty() {
                    "no channel"
                } else {
                    "several channels"
                },
                if known.is_empty() { "none".to_string() } else { known }
            )));
        }
        out.push(found[0]);
    }
    Ok(out)
}

/// What the probe says about itself (describe of fn 0), its boot_id and address.
fn probe_info(hst: &mut Host, boot_id: Option<u32>, addr: &Address) -> Map<String, Value> {
    let mut info = Map::new();
    for (t, v) in hst.describe(host::CORE_FN).unwrap_or_default() {
        let key = match t {
            host::TAG_CHIP => "chip",
            host::TAG_MODEL => "model",
            host::TAG_FIRMWARE => "firmware",
            host::TAG_UNIT_ID => "unit_id",
            _ => continue,
        };
        if !info.contains_key(key) {
            info.insert(key.into(), json!(String::from_utf8_lossy(&v).trim_end_matches('\0')));
        }
    }
    if let Some(b) = boot_id {
        info.insert("boot_id".into(), json!(b));
    }
    if let Some(p) = addr.place() {
        info.insert("address".into(), json!(p));
    } else if let Some(u) = info.get("unit_id").and_then(|v| v.as_str()).filter(|u| !u.is_empty()) {
        info.insert("address".into(), json!(format!("oep://{u}")));
    }
    info
}

/// meta.probe.clock: each reading, and the probe clock's rate against the host's when both are of one boot.
fn clock_info(before: Option<ClockReading>, after: Option<ClockReading>) -> Map<String, Value> {
    let mut out = Map::new();
    for (name, r) in [("before", before), ("after", after)] {
        if let Some(r) = r {
            out.insert(
                name.into(),
                json!({"host_ns": r.host_ns() as i64, "uptime_ns": r.uptime_ns, "boot_id": r.boot_id,
                       "uncertainty_ns": r.uncertainty_ns() as i64}),
            );
        }
    }
    if let (Some(b), Some(a)) = (before, after)
        && b.boot_id == a.boot_id
    {
        let host_d = (a.host_ns() - b.host_ns()) as f64;
        let probe_d = a.uptime_ns as f64 - b.uptime_ns as f64;
        if host_d > 0.0 {
            out.insert("rate_ppm".into(), json!((probe_d / host_d - 1.0) * 1e6));
            out.insert(
                "rate_ppm_uncertainty".into(),
                json!((b.uncertainty_ns() + a.uncertainty_ns()) as f64 / host_d * 1e6),
            );
        }
    }
    out
}

fn round3(x: f64) -> f64 {
    wireskein_io::pyfmt::round_n(x, 3)
}

/// meta.probe.link: how the link went.
fn link_info(hst: &Host, open_s: f64, read_bytes: u64, read_s: f64) -> Map<String, Value> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = Map::new();
    out.insert("open_s".into(), json!(round3(open_s)));
    out.insert("read_bytes".into(), json!(read_bytes));
    out.insert("read_s".into(), json!(round3(read_s)));
    let t = &hst.link.t;
    out.insert("reader".into(), json!(t.reader));
    out.insert("rate".into(), json!(0));
    out.insert("why".into(), json!("the link is not a serial port this host opened"));
    if t.depth > 0 {
        let s = &t.stats;
        out.insert(
            "usb".into(),
            json!({"urb_size": transport::URB_SIZE, "depth": t.depth, "transfers": s.transfers.load(Relaxed),
                   "inflight_min": s.inflight_min.load(Relaxed),
                   "gap_max_ms": round3(s.gap_max_us.load(Relaxed) as f64 / 1000.0),
                   "held": s.held.load(Relaxed)}),
        );
    }
    out
}

/// A configure item for people (the probe names the TLV it refused with its tag as sent).
fn what(tag: Option<u8>) -> String {
    match tag.map(|t| t & 0x7F) {
        Some(capture::T_TRIGGER) => "a trigger".into(),
        Some(capture::T_PRETRIGGER) => "a pretrigger".into(),
        Some(0x02) => "this rate (outside the probe's rate range for this track: ask a lower --rate)".into(),
        Some(0x01) => "this mode (--mode: the probe does not take it on this track)".into(),
        Some(capture::T_FORMAT) => {
            "this data format (NAME=ID/D: the probe keeps a line as its changes only in some modes)".into()
        }
        Some(capture::T_TRIGGER_OPTS) => "these trigger options (--arm now)".into(),
        Some(t) => format!("configure item 0x{t:02x}"),
        None => "configure item None".into(),
    }
}

struct Run<'a> {
    hst: Host,
    logic: Logic,
    req: &'a Request,
    target: String,
    names: Vec<String>,
    ids: Vec<u16>,
    probe: Map<String, Value>,
    open_s: f64,
    clock_before: Option<ClockReading>,
}

impl Run<'_> {
    fn configure(&mut self, s: &Setting, what_: &str) -> Result<Config> {
        match self.logic.configure(&mut self.hst, s, false) {
            Ok(c) => Ok(c),
            Err(e) if e.is_rejected(msg::UNSUPPORTED) => {
                let tag = e.unsupported_tag();
                let mut most = String::new();
                if tag.map(|t| t & 0x7F) == Some(capture::T_PRETRIGGER)
                    && s.pretrigger.is_some_and(|p| p > 0)
                    && let Some(m) = self.most_pretrigger(s)
                {
                    most = format!(
                        " (at most {m} samples before the trigger here, of {})",
                        s.samples.unwrap_or(0)
                    );
                }
                Err(Error::Runtime(format!(
                    "probe {} cannot capture {what_} with {}{most}",
                    self.target,
                    what(tag)
                )))
            }
            Err(e @ Error::Rejected(..)) if !s.carriage.is_empty() => Err(Error::Runtime(format!(
                "probe {} cannot capture {what_} with lines kept as their changes (NAME=ID/D, capture format 1) this \
                 way: {e} - the probe has not the room for it here; without /D the lines go as samples",
                self.target
            ))),
            Err(e) => Err(e),
        }
    }

    /// The largest pretrigger the probe takes with these settings, by query and bisection.
    fn most_pretrigger(&mut self, s: &Setting) -> Option<u64> {
        let mut q = s.clone();
        let mut ask = |p: u64, l: &mut Logic, h: &mut Host| -> Option<bool> {
            q.pretrigger = Some(p);
            match l.configure(h, &q, true) {
                Ok(_) => Some(true),
                Err(e) if e.is_rejected(msg::UNKNOWN_OPERATION) => None,
                Err(_) => Some(false),
            }
        };
        let mut hi = s.pretrigger? - 1;
        if hi < 1 || !ask(1, &mut self.logic, &mut self.hst)? {
            return None;
        }
        let mut lo = 1;
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            if ask(mid, &mut self.logic, &mut self.hst)? {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        Some(lo)
    }

    fn note_used(&mut self, c: &Config) {
        if let Some(ns) = c.glitch_used {
            self.probe.insert("glitch_filter_ns".into(), json!(ns));
        }
        if let Some(h) = c.hysteresis_used {
            self.probe.insert("input_hysteresis".into(), json!(h));
        }
    }

    fn setting(&self, mode: u8) -> Setting {
        let req = self.req;
        let trigger = req.trigger.as_ref().map(|(name, kind)| {
            let role = self.names.iter().position(|n| n == name).unwrap_or(0) as u8;
            let (ty, v) = match kind.as_str() {
                "high" => (capture::TRIGGER_LEVEL, 1),
                "low" => (capture::TRIGGER_LEVEL, 0),
                "rise" => (capture::TRIGGER_EDGE, 0),
                "fall" => (capture::TRIGGER_EDGE, 1),
                _ => (capture::TRIGGER_EDGE, 2),
            };
            (ty, role, v)
        });
        let carriage = req
            .resolution
            .iter()
            .map(|(n, (m, d))| {
                let role = self.names.iter().position(|x| x == n).unwrap_or(0);
                (role, (blocks::carriage_of(m).unwrap_or(blocks::SAMPLE), *d as usize))
            })
            .collect();
        let trigger_opts = match mode {
            capture::REPEAT if trigger.is_some() => Some((if req.arm_now { 0 } else { 1 }, 1)),
            _ if req.arm_now && trigger.is_some() => Some((0, 0)),
            _ => None,
        };
        Setting {
            mode,
            rate: req.rate_hz,
            samples: (mode != capture::STREAMING).then_some(req.samples),
            segments: (mode == capture::REPEAT).then_some(req.segments.unwrap_or(1)),
            trigger: if mode == capture::STREAMING { None } else { trigger },
            pretrigger: req.pretrigger,
            trigger_opts,
            carriage,
            roles: (0..self.names.len()).collect(),
            glitch_filter_ns: req.glitch_filter_ns,
            input_hysteresis: req.input_hysteresis,
        }
    }

    fn format(c: &Config) -> Format {
        match c.blocks() {
            Some(b) => Format::One(b),
            None => Format::Zero {
                width: c.width,
                positions: c.positions.clone(),
            },
        }
    }

    fn generation(&mut self) {
        if let Some(g) = self.logic.generation {
            self.probe.insert("generation".into(), json!({"logic": g}));
        }
    }

    fn clock_after(&mut self) {
        let after = self.hst.clock_best(4).ok();
        let c = clock_info(self.clock_before, after);
        if !c.is_empty() {
            self.probe.insert("clock".into(), Value::Object(c));
        }
    }

    fn base_meta(&self) -> Map<String, Value> {
        let mut m = Map::new();
        let pc: Map<String, Value> = self
            .names
            .iter()
            .zip(&self.ids)
            .map(|(n, i)| (n.clone(), json!(i)))
            .collect();
        m.insert("probe_channels".into(), Value::Object(pc));
        m
    }

    fn oneshot(&mut self) -> Result<Acquired> {
        let s = self.setting(capture::ONE_SHOT);
        let cfg = self.configure(&s, "logic")?;
        self.note_used(&cfg);
        self.logic.start(&mut self.hst)?;
        let trig = self.req.trigger.as_ref().map(|(n, k)| format!("{n}:{k}"));
        let segs = self.logic.wait(
            &mut self.hst,
            Duration::from_secs_f64(self.req.timeout_s),
            trig.as_deref(),
        )?;
        let seg = *segs
            .first()
            .ok_or_else(|| Error::Runtime("the probe finished without a segment".into()))?;
        let t_read = Instant::now();
        let data = self.logic.read(
            &mut self.hst,
            seg.position,
            cfg.segment_bytes(seg.samples),
            if seg.generation != 0 {
                seg.generation
            } else {
                self.logic.generation.unwrap_or(0)
            },
        )?;
        let read_s = t_read.elapsed().as_secs_f64();
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, data.len() as u64, read_s)),
        );
        self.clock_after();
        let mut meta = self.base_meta();
        meta.insert("start_ns".into(), json!(seg.start_ns));
        meta.insert("start_uncertainty_ns".into(), json!(seg.start_uncertainty_ns));
        if seg.flags & capture::SEGMENT_SLIPPED != 0 {
            meta.insert("time_base_slipped".into(), json!(true));
        }
        if let Some(t) = seg.trigger_index {
            meta.insert("trigger_tick".into(), json!(t));
        }
        let fmt = Self::format(&cfg);
        let chans = assemble::segment_channels(&fmt, &data, seg.samples, &self.names, &self.ids)?;
        if let Format::One(b) = &fmt {
            self.probe.insert(
                "data".into(),
                json!({"format": cfg.format, "block": cfg.block, "bytes": data.len(),
                       "expected_bytes": b.segment_bytes(seg.samples as usize),
                       "format0_bytes": (seg.samples * self.names.len() as u64).div_ceil(8)}),
            );
        }
        self.generation();
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        Ok(Acquired {
            tick_hz: cfg.rate,
            channels: chans,
            meta,
            parts: None,
            lost: vec![],
            layout: "timeline".into(),
            temp_dir: None,
            samples: Some(seg.samples),
        })
    }

    fn repeat(&mut self) -> Result<Acquired> {
        let s = self.setting(capture::REPEAT);
        let cfg = self.configure(&s, "logic in repeat")?;
        self.note_used(&cfg);
        let req = self.req;
        let want = req.segments.unwrap_or(1) as usize;
        let rate = cfg.rate;
        if cfg.segments > 0 {
            self.probe.insert("repeat".into(), json!({"ring": cfg.segments}));
        }
        let fmt = Self::format(&cfg);
        let t0 = Instant::now();
        let (mut parts, mut nxt, mut first_ns, mut received): (Vec<Part>, u32, Option<u64>, u64) = (vec![], 0, None, 0);
        let mut prev: Option<(i64, u64)> = None;
        self.logic.start(&mut self.hst)?;
        let out: Result<()> = (|| {
            let (mut last, mut kept) = (Instant::now(), Instant::now());
            while parts.len() < want {
                if kept.elapsed() >= Duration::from_secs(1) && self.hst.session.is_some() {
                    self.hst.keepalive()?;
                    kept = Instant::now();
                }
                let st = self.logic.status(&mut self.hst)?;
                if st.state == capture::STATE_ERROR {
                    return Err(Error::Failed(format!(
                        "the capture stopped with an error ({})",
                        st.error_name()
                    )));
                }
                let ready: Vec<capture::Segment> = if st.serial_done > nxt {
                    self.logic
                        .segments(&mut self.hst, nxt)?
                        .into_iter()
                        .filter(|x| x.serial < st.serial_done)
                        .collect()
                } else {
                    vec![]
                };
                if ready.is_empty() {
                    if last.elapsed().as_secs_f64() > req.timeout_s {
                        let w = match &req.trigger {
                            Some((n, k)) => format!("no trigger ({n}:{k})"),
                            None => "no segment".into(),
                        };
                        return Err(Error::Timeout(format!(
                            "{w} within {} s (--timeout) after {} of {want} segments",
                            crate::fmt_g(req.timeout_s),
                            parts.len()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let mut ready = ready;
                ready.sort_by_key(|x| x.serial);
                for seg in ready {
                    if parts.len() == want {
                        break;
                    }
                    let g = if seg.generation != 0 {
                        seg.generation
                    } else {
                        self.logic.generation.unwrap_or(0)
                    };
                    let data = self
                        .logic
                        .read(&mut self.hst, seg.position, cfg.segment_bytes(seg.samples), g)?;
                    received += data.len() as u64;
                    self.logic.release(&mut self.hst, seg.serial, g)?;
                    nxt = seg.serial.wrapping_add(1);
                    let chans = assemble::segment_channels(&fmt, &data, seg.samples, &self.names, &self.ids)?;
                    let mut meta = Map::new();
                    meta.insert("start_ns".into(), json!(seg.start_ns));
                    meta.insert("start_uncertainty_ns".into(), json!(seg.start_uncertainty_ns));
                    if let Some(t) = seg.trigger_index {
                        meta.insert("trigger_tick".into(), json!(t));
                    }
                    if seg.flags & capture::SEGMENT_SLIPPED != 0 {
                        meta.insert("time_base_slipped".into(), json!(true));
                    }
                    let f = *first_ns.get_or_insert(seg.start_ns);
                    let tick0 = match prev {
                        Some((t, n)) if req.trigger.is_none() && seg.flags & capture::SEGMENT_GAP == 0 => t + n as i64,
                        _ => assemble::round_div(
                            (seg.start_ns as i128 - f as i128) * rate.0 as i128,
                            1_000_000_000 * rate.1 as i128,
                        ) as i64,
                    };
                    prev = Some((tick0, seg.samples));
                    parts.push(Part {
                        channels: chans,
                        meta,
                        tick0: Some(tick0),
                        samples: Some(seg.samples),
                    });
                }
                last = Instant::now();
            }
            Ok(())
        })();
        let _ = self.logic.stop(&mut self.hst);
        out?;
        let read_s = t0.elapsed().as_secs_f64();
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, received, read_s)),
        );
        self.clock_after();
        self.generation();
        let mut meta = self.base_meta();
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        let mut rep = json!({"segments": parts.len(), "triggered": req.trigger.is_some()});
        if req.trigger.is_some() {
            let without: Vec<usize> = parts
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.meta.contains_key("trigger_tick"))
                .map(|(k, _)| k)
                .collect();
            if !without.is_empty() {
                rep["without_trigger"] = json!(without);
            }
        }
        meta.insert("repeat".into(), rep);
        Ok(Acquired {
            tick_hz: rate,
            channels: parts[0].channels.clone(),
            meta,
            parts: Some(parts),
            lost: vec![],
            layout: if req.trigger.is_some() { "segments" } else { "timeline" }.into(),
            temp_dir: None,
            samples: None,
        })
    }

    fn stream(&mut self) -> Result<Acquired> {
        let s = self.setting(capture::STREAMING);
        let cfg = self.configure(&s, "logic")?;
        self.note_used(&cfg);
        if let Some(b) = cfg.stream_buffer {
            self.probe.insert(
                "stream_buffer".into(),
                json!({"bytes": b, "tolerable_delay_s": cfg.tolerable_delay_s()}),
            );
        }
        if let Some(r) = cfg.data_rate.filter(|&r| r > 0) {
            self.probe.insert("data_rate".into(), json!(r));
        }
        let base = self.req.spool_dir.clone().unwrap_or_else(std::env::temp_dir);
        let dir = make_temp_dir(&base, ".oep-acquire-stream-")?;
        let out = self.stream_into(&cfg, &dir);
        if out.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        out
    }

    /// A stream collected and put together in `dir`.
    fn stream_into(&mut self, cfg: &Config, dir: &std::path::Path) -> Result<Acquired> {
        let req = self.req;
        let dir = dir.to_path_buf();
        let mut spool = Spool::new(
            self.names.clone(),
            self.ids.clone(),
            cfg.rate,
            Self::format(&cfg),
            req.on_loss_stop,
            Some(&dir),
        );
        let mut col = Collector::default();
        let fn_ = self.logic.fn_;
        self.hst.subscribe(fn_, STREAM_MIN_BYTES, STREAM_MAX_DELAY_MS)?;
        self.logic.start(&mut self.hst)?;
        let t_read = Instant::now();
        let deadline = t_read + Duration::from_secs_f64(req.duration_s.unwrap_or(0.0));
        let mut kept = Instant::now();
        let collected: Result<()> = (|| {
            while Instant::now() < deadline {
                let slice = STREAM_POLL.min(deadline.saturating_duration_since(Instant::now()));
                let end = Instant::now() + slice;
                while Instant::now() < end {
                    self.hst.link.pump(Duration::from_millis(20), true)?;
                    col.take(&mut self.hst.link, fn_, self.logic.generation, &mut spool)?;
                    if kept.elapsed() >= Duration::from_secs(1) {
                        self.hst.keepalive()?;
                        kept = Instant::now();
                    }
                }
                spool.work(false, None)?;
                if req.on_loss_stop && spool.lost_any() {
                    break;
                }
            }
            Ok(())
        })();
        let stopped = self.logic.stop(&mut self.hst);
        collected?;
        stopped?;
        // the rest, up to the last byte captured (status's write position)
        let st = self.logic.status(&mut self.hst)?;
        let error = st.state == capture::STATE_ERROR;
        let fin = Instant::now()
            + if error {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(5)
            };
        while Instant::now() < fin {
            if !error && col.end.is_some_and(|e| e >= st.write_pos) {
                break;
            }
            self.hst.link.pump(Duration::from_millis(20), true)?;
            col.take(&mut self.hst.link, fn_, self.logic.generation, &mut spool)?;
        }
        let read_s = t_read.elapsed().as_secs_f64();
        let st = self.logic.status(&mut self.hst)?;
        let error = st.state == capture::STATE_ERROR;
        spool.finish(if error { Some(st.write_pos) } else { None })?;
        let mut drops_line = String::new();
        for (name, v) in &st.named {
            if name == capture::CAPTURE_DROPS {
                if let Some(d) = drops(v) {
                    drops_line = ["no_stage_bytes", "unqueued_bytes", "dma_overruns", "queue_overflows"]
                        .iter()
                        .filter_map(|k| {
                            d.get(*k)
                                .and_then(|x| x.as_u64())
                                .filter(|&x| x > 0)
                                .map(|x| format!("{k} {x}"))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.probe.insert("drops".into(), Value::Object(d));
                }
            } else if name == capture::CAPTURE_SPILL && v.len() == 44 {
                let q = |i: usize| u64::from_le_bytes(v[i..i + 8].try_into().expect("8"));
                let d = |i: usize| u32::from_le_bytes(v[i..i + 4].try_into().expect("4")) as u64;
                let (gap, hz) = (d(36), d(40));
                self.probe.insert(
                    "spill".into(),
                    json!({"in_bytes": q(0), "in_cycles": q(8), "out_bytes": q(16), "out_cycles": q(24),
                           "overruns_with_room": d(32), "harvest_gap_max_cycles": gap, "cpu_hz": hz,
                           "harvest_gap_max_us": if hz > 0 { Some(gap as f64 / hz as f64 * 1e6) } else { None }}),
                );
            }
        }
        let received = col.bytes;
        let tail = (!error && col.end.is_some_and(|e| st.write_pos > e)).then(|| st.write_pos - col.end.unwrap_or(0));
        let tail = if !error && col.end.is_none() && st.write_pos > 0 {
            Some(st.write_pos)
        } else {
            tail
        };
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, received, read_s)),
        );
        self.clock_after();
        self.generation();
        let mut meta = self.base_meta();
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        meta.insert(
            "stream".into(),
            json!({"duration_s": req.duration_s, "on_loss": if req.on_loss_stop { "stop" } else { "continue" },
                   "frames": col.frames, "bytes": received, "read_s": round3(read_s)}),
        );
        let (parts, mut lost) = spool.take();
        if parts.is_empty() {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(Error::Runtime(format!(
                "the stream brought no data{}",
                if lost.is_empty() {
                    ""
                } else {
                    " (the probe dropped it all)"
                }
            )));
        }
        let last = parts.len() as i64 - 1;
        if error {
            lost.push(Loss::new(
                last,
                format!("the probe stopped in error ({})", st.error_name()),
                true,
            ));
        } else if st.dropped() && lost.is_empty() {
            lost.push(Loss::new(last, "the probe dropped data (status flags bit 0)", false));
        }
        if let Some(t) = tail {
            lost.push(Loss::new(
                last,
                format!("the stream's last {t} bytes did not arrive after the stop"),
                true,
            ));
        }
        if !drops_line.is_empty()
            && let Some(l) = lost.last_mut()
        {
            l.reason += &format!(" - the probe's counts since start: {drops_line}");
        }
        Ok(Acquired {
            tick_hz: cfg.rate,
            channels: parts[0].channels.clone(),
            meta,
            parts: Some(parts),
            lost,
            layout: "timeline".into(),
            temp_dir: Some(dir),
            samples: None,
        })
    }
}

/// The reference firmware's capture-drops counters (a named extension of status, not an OEP definition).
fn drops(v: &[u8]) -> Option<Map<String, Value>> {
    let u32at = |i: usize| u32::from_le_bytes(v[i..i + 4].try_into().expect("4")) as u64;
    let mut m = Map::new();
    let n = v.len();
    if ![34, 42, 46, 38].contains(&n) {
        return None;
    }
    for (k, name) in ["no_stage_bytes", "unqueued_bytes", "dma_overruns", "queue_overflows"]
        .iter()
        .enumerate()
    {
        m.insert((*name).into(), json!(u32at(4 * k)));
    }
    m.insert("zero_copy".into(), json!(v[16] != 0));
    m.insert("stages".into(), json!(v[17]));
    let mut at = 18;
    for name in ["internal_largest", "dma_largest", "psram_total"] {
        m.insert(name.into(), json!(u32at(at)));
        at += 4;
    }
    for name in ["buffer_peak", "spill_peak", "spill_bytes"] {
        m.insert(name.into(), if at + 4 <= n { json!(u32at(at)) } else { Value::Null });
        at += 4;
    }
    m.insert(
        "dropped_bytes".into(),
        if n == 46 {
            json!(u64::from_le_bytes(v[38..46].try_into().expect("8")))
        } else {
            Value::Null
        },
    );
    Some(m)
}

/// The data pushes of the logic track, taken off the link into the spool: seq, generation, segment_info.
#[derive(Default)]
struct Collector {
    seq: Option<u16>,
    pub frames: u64,
    pub bytes: u64,
    pub seq_lost: u64,
    pub stale: u64,
    /// the stream position just past what arrived
    pub end: Option<u64>,
}

impl Collector {
    fn take(&mut self, link: &mut Link, fn_: u16, generation: Option<u32>, spool: &mut Spool) -> Result<()> {
        let mut event_seqs: Vec<u16> = link
            .events
            .iter()
            .filter(|e| u16::from_le_bytes([e[1], e[2]]) == fn_)
            .map(|e| u16::from_le_bytes([e[3], e[4]]))
            .collect();
        // this track's events are not used here
        link.events.retain(|e| u16::from_le_bytes([e[1], e[2]]) != fn_);
        let mut rest = std::collections::VecDeque::new();
        while let Some(f) = link.pushes.pop_front() {
            if u16::from_le_bytes([f[1], f[2]]) != fn_ {
                rest.push_back(f);
                continue;
            }
            let p = capture::parse_push(&f)?;
            if let Some(mut want) = self.seq {
                while want != p.seq {
                    if let Some(i) = event_seqs.iter().position(|&s| s == want) {
                        event_seqs.remove(i);
                    } else {
                        self.seq_lost += 1;
                    }
                    want = want.wrapping_add(1);
                }
            }
            self.seq = Some(p.seq.wrapping_add(1));
            if let (Some(g), Some(mine)) = (p.generation, generation)
                && g != mine
            {
                self.stale += 1;
                continue;
            }
            if let Some(info) = p.info {
                spool.segment(info);
            }
            self.frames += 1;
            self.bytes += p.data.len() as u64;
            self.end = Some(p.position + p.data.len() as u64);
            spool.run(p.position, &p.data)?;
        }
        link.pushes = rest;
        Ok(())
    }
}

/// A new directory of its own under `base` (tempfile.mkdtemp's).
fn make_temp_dir(base: &std::path::Path, prefix: &str) -> Result<PathBuf> {
    for k in 0..100u32 {
        let p = base.join(format!(
            "{prefix}{}{k}",
            std::process::id() as u64 * 7919 + host::monotonic_ns() as u64 % 1_000_000
        ));
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(format!("{}: {e}", p.display()))),
        }
    }
    Err(Error::Io(format!("{}: no new directory", base.display())))
}

/// Open the probe at `address`, take its lease, run one capture and let it all go (acquire.capture_at).
pub fn capture_at(address: &str, req: &Request, owner: &str) -> Result<Acquired> {
    req.check()?;
    let addr = parse_address(address)?.ok_or_else(|| {
        Error::Value(format!(
            "{address}: not an address this client opens (oep://UNIT_ID[?via=usb|bulk|tcp:HOST:PORT] or \
             tcp://HOST:PORT; a serial port, HID, CDC, DNS-SD and brokers are oep-client-python's)"
        ))
    })?;
    let wrap = |e: Error| match e {
        Error::Rejected(..) | Error::Failed(_) | Error::Protocol(_) => {
            let s = e.to_string();
            let hint = if s.contains("(storage)") {
                " - the probe lost data inside a segment: try a lower rate or fewer channels"
            } else {
                ""
            };
            Error::Runtime(format!("probe {address}: {s}{hint}"))
        }
        e => e,
    };
    capture_on(&addr, address, req, owner).map_err(wrap)
}

fn capture_on(addr: &Address, target: &str, req: &Request, owner: &str) -> Result<Acquired> {
    let t_open = Instant::now();
    let hst = open(addr)?;
    let (mut hst, res) = session(hst, t_open, addr, target, req, owner);
    // let the session go (its plan and subscriptions with it); the link closes when the host is dropped
    let _ = hst.end();
    res
}

/// The capture on an open link: the lease taken, the plan applied and released. -> the host back, and the capture.
fn session(
    mut hst: Host,
    t_open: Instant,
    addr: &Address,
    target: &str,
    req: &Request,
    owner: &str,
) -> (Host, Result<Acquired>) {
    macro_rules! tri {
        ($e:expr) => {
            match $e {
                Ok(v) => v,
                Err(e) => return (hst, Err(e)),
            }
        };
    }
    let (_, boot_id) = tri!(
        hst.take(LEASE_MS, Some(owner), Duration::from_secs(5))
            .map_err(|e| match e {
                Error::Runtime(s) => Error::Failed(s),
                e => e,
            })
    );
    let open_s = t_open.elapsed().as_secs_f64();
    let clock_before = hst.clock_best(4).ok();
    let probe = probe_info(&mut hst, Some(boot_id), addr);
    let lines: Vec<String> = req.channels.iter().map(|(_, c)| c.clone()).collect();
    let ids = tri!(channel_ids(&mut hst, &lines));
    let logic = tri!(Logic::new(&mut hst));
    let plan: Vec<(u16, u8, u16)> = ids
        .iter()
        .enumerate()
        .map(|(k, &ch)| (logic.fn_, k as u8, ch))
        .collect();
    tri!(hst.plan_apply(&plan));
    let fn_ = logic.fn_;
    let mut run = Run {
        hst,
        logic,
        req,
        target: target.to_string(),
        names: req.channels.iter().map(|(n, _)| n.clone()).collect(),
        ids,
        probe,
        open_s,
        clock_before,
    };
    let out = match req.mode {
        Mode::OneShot => run.oneshot(),
        Mode::Repeat => run.repeat(),
        Mode::Stream => run.stream(),
    };
    let released = run.hst.plan_release(&[fn_]);
    let res = out.and_then(|a| released.map(|_| a));
    (run.hst, res)
}
