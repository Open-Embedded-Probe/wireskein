//! One capture from a probe, put together (oep-client-python's `acquire`): the request in; the parts with their
//! losses, times and the probe's facts out. Logic channels (oep.fixture.logic): one-shot, repeat and stream, capture
//! format 0 and format 1's reductions, a trigger on a logic line, the glitch filter.

use crate::assemble::{self, AnalogChan, Chan, Feed, Format, Loss, Part, Spool, SpoolWorker};
use crate::blocks;
use crate::capture::{self, Calibration, Config, Group, Logic, Setting};
use crate::host::{self, ClockReading, Host};
use crate::link::Link;
use crate::msg::{self, Error, Result};
use crate::ratio::Ratio;
use crate::transport;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub const REDUCTIONS: [&str; 4] = ["sample", "any_high", "any_low", "change_latch"];
pub const TRIGGERS: [&str; 5] = ["high", "low", "rise", "fall", "both"];
pub const ANALOG_TRIGGERS: [&str; 4] = ["high", "low", "rise", "fall"];
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
    /// analog: (name to record, the probe's channel name, frontend)
    pub analog: Vec<(String, String, Option<u8>)>,
    /// analog samples a second (None: `rate_hz`) and how many (None: as long as the logic capture)
    pub analog_rate_hz: Option<u64>,
    pub analog_samples: Option<u64>,
    /// a trigger on an analog channel: the volts it crosses or stays beyond
    pub trigger_volts: Option<f64>,
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
            analog: vec![],
            analog_rate_hz: None,
            analog_samples: None,
            trigger_volts: None,
        }
    }

    fn analog_names(&self) -> Vec<&str> {
        self.analog.iter().map(|(n, _, _)| n.as_str()).collect()
    }

    /// acquire.Request's checks.
    pub fn check(&self) -> Result<()> {
        let names: Vec<&str> = self.channels.iter().map(|(n, _)| n.as_str()).collect();
        let anames = self.analog_names();
        let mut all: Vec<&str> = names.iter().chain(anames.iter()).copied().collect();
        let given = all.clone();
        all.sort();
        all.dedup();
        if given.is_empty() || all.len() != given.len() {
            return Err(Error::Value(format!(
                "channel names must be given and differ: {given:?}"
            )));
        }
        if let Some((name, kind)) = &self.trigger {
            if anames.contains(&name.as_str()) {
                if !ANALOG_TRIGGERS.contains(&kind.as_str()) || self.trigger_volts.is_none() {
                    return Err(Error::Value(format!(
                        "trigger {name}:{kind}: an analog channel takes {} and the volts it crosses or stays beyond \
                         ({name}:fall:2.9)",
                        ANALOG_TRIGGERS.join("/")
                    )));
                }
            } else if !names.contains(&name.as_str())
                || !TRIGGERS.contains(&kind.as_str())
                || self.trigger_volts.is_some()
            {
                return Err(Error::Value(format!(
                    "trigger {name}:{kind}: the channel must be one captured, the kind one of {} (volts only for an \
                     analog channel)",
                    TRIGGERS.join(", ")
                )));
            }
        }
        if self.mode == Mode::Repeat
            && !self.analog.is_empty()
            && self.segments.is_some_and(|s| s >= 1)
            && self.samples > 0
        {
            return Err(Error::Value("repeat: logic channels only".into()));
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
    /// a serial port (a UART bridge or a CDC port by its path): COBS frames at the boot speed (transports §4)
    Serial {
        path: String,
        unit: Option<String>,
    },
}

impl Address {
    /// The probe itself without its route (host guide §5.3), when its unit_id is known.
    pub fn place(&self) -> Option<String> {
        match self {
            Address::Usb { unit }
            | Address::Tcp { unit: Some(unit), .. }
            | Address::Serial { unit: Some(unit), .. } => Some(format!("oep://{unit}")),
            _ => None,
        }
    }
    pub fn unit(&self) -> Option<&str> {
        match self {
            Address::Usb { unit }
            | Address::Tcp { unit: Some(unit), .. }
            | Address::Serial { unit: Some(unit), .. } => Some(unit),
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
    if text.starts_with("/dev/") {
        return Ok(Some(Address::Serial {
            path: text.to_string(),
            unit: None,
        }));
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
        Some(v) if v.starts_with("tty:") && v.len() > 4 => Some(Address::Serial {
            path: v[4..].to_string(),
            unit: Some(unit),
        }),
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
        Address::Serial { path, .. } => transport::open_serial(path, transport::BASE_BAUD)?,
    };
    let serial = matches!(addr, Address::Serial { .. });
    let quiet = !t.keeps_boundaries && !serial;
    let mut link = if serial {
        let mut lk = Link::with_framing(t, crate::link::Framing::Cobs);
        lk.baud = Some(transport::BASE_BAUD);
        lk
    } else {
        Link::new(t)
    };
    if let Address::Tcp { host, port, .. } = addr {
        // transports §5: a connection whose framing broke is closed and opened again
        let (h, p) = (host.clone(), *port);
        link.reopener = Some(Box::new(move || transport::open_tcp(&h, p)));
    }
    if quiet {
        // pushes of a session before may still come: they stop with its lease (transports §5)
        link.discard_until_quiet(Duration::from_millis(50), Duration::from_secs(61));
    }
    let mut hst = Host::new(link);
    hst.probing = true; // the probing rule: one confirm and its resend, no resync (transports §3)
    if serial {
        // transports §4: confirms at the boot speed for port_speed_idle_ms + host_wait_add_ms - a host that raised
        // the speed and died leaves the probe at that rate until it runs out
        hst.wait_boot_speed().map_err(|e| {
            Error::Runtime(format!(
                "no valid confirm answer ({e}); closed, nothing else sent (transports §3)"
            ))
        })?;
    }
    hst.confirm().map_err(|e| {
        Error::Runtime(format!(
            "no valid confirm answer ({e}); closed, nothing else sent (transports §3)"
        ))
    })?;
    hst.probing = false;
    if matches!(addr, Address::Tcp { .. }) {
        // behind a broker: outlast its own retry towards the probe (oep-client-python's TCP_TIMEOUT)
        hst.link.timeout = Duration::from_secs(15);
    }
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

/// meta.probe.link: how the link went; `oneshot`: a one-shot's (a serial port's rate is said there, as acquire says it).
fn link_info(hst: &Host, open_s: f64, read_bytes: u64, read_s: f64, oneshot: bool) -> Map<String, Value> {
    use std::sync::atomic::Ordering::Relaxed;
    let mut out = Map::new();
    out.insert("open_s".into(), json!(round3(open_s)));
    out.insert("read_bytes".into(), json!(read_bytes));
    out.insert("read_s".into(), json!(round3(read_s)));
    let t = &hst.link.t;
    out.insert("reader".into(), json!(t.reader));
    match hst.link.baud {
        // nothing tried: the boot speed (port_speed is oep-client-python's)
        Some(b) => {
            if oneshot {
                out.insert("rate".into(), json!(b));
                out.insert("raised".into(), json!(false));
            }
            out.insert("broken".into(), json!(hst.link.corrupt));
        }
        None => {
            out.insert("rate".into(), json!(0));
            out.insert("why".into(), json!("the link is not a serial port this host opened"));
        }
    }
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

/// configure's answer, a refusal, or no query on this probe (OEP v1: query is optional).
enum Asked {
    Took(Box<Config>),
    Refused(Error),
    NoQuery,
}

/// configure on `track`; a refusal (unsupported, naming the TLV) in one line (acquire's `_configure`).
fn configure_on(
    hst: &mut Host,
    track: &mut Logic,
    s: &Setting,
    target: &str,
    what_: &str,
    most_said: bool,
) -> Result<Config> {
    match track.configure(hst, s, false) {
        Ok(c) => Ok(c),
        Err(e) if e.is_rejected(msg::UNSUPPORTED) => Err(Error::Runtime(format!(
            "probe {target} cannot capture {what_} with {}{}",
            what(e.unsupported_tag()),
            if most_said {
                said_most(hst, track, s, &e)
            } else {
                String::new()
            }
        ))),
        Err(e @ Error::Rejected(..)) if !s.carriage.is_empty() => Err(Error::Runtime(format!(
            "probe {target} cannot capture {what_} with lines kept as their changes (NAME=ID/D, capture format 1) this \
             way: {e} - the probe has not the room for it here; without /D the lines go as samples"
        ))),
        Err(e) => Err(e),
    }
}

/// configure's query (the same rules, nothing set).
fn query_on(hst: &mut Host, track: &mut Logic, s: &Setting) -> Result<Asked> {
    match track.configure(hst, s, true) {
        Ok(c) => Ok(Asked::Took(Box::new(c))),
        Err(e) if e.is_rejected(msg::UNKNOWN_OPERATION) => Ok(Asked::NoQuery),
        Err(e @ Error::Rejected(..)) => Ok(Asked::Refused(e)),
        Err(e) => Err(e),
    }
}

/// For a refused pretrigger: the most this setting takes, as words; else "".
fn said_most(hst: &mut Host, track: &mut Logic, s: &Setting, e: &Error) -> String {
    if e.unsupported_tag().map(|t| t & 0x7F) != Some(capture::T_PRETRIGGER) || !s.pretrigger.is_some_and(|p| p > 0) {
        return String::new();
    }
    match most_pretrigger_on(hst, track, s) {
        Some(m) => format!(
            " (at most {m} samples before the trigger here, of {})",
            s.samples.unwrap_or(0)
        ),
        None => String::new(),
    }
}

/// The largest pretrigger the probe takes with these settings, by query and bisection.
fn most_pretrigger_on(hst: &mut Host, track: &mut Logic, s: &Setting) -> Option<u64> {
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
    if hi < 1 || !ask(1, track, hst)? {
        return None;
    }
    let mut lo = 1;
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if ask(mid, track, hst)? {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Some(lo)
}

struct Run<'a> {
    hst: Host,
    /// the logic track (a stand-in of fn 0, never used, when only analog lines are captured)
    logic: Logic,
    /// the analog track, its lines' names and channels, its frontends' ranges (describe)
    an: Option<Logic>,
    anames: Vec<String>,
    aids: Vec<u16>,
    req: &'a Request,
    target: String,
    names: Vec<String>,
    ids: Vec<u16>,
    probe: Map<String, Value>,
    open_s: f64,
    clock_before: Option<ClockReading>,
    /// the analog describe's frontends: number -> its measurable range
    frontends: BTreeMap<u8, Value>,
}

impl Run<'_> {
    fn configure(&mut self, s: &Setting, what_: &str) -> Result<Config> {
        let mode_none = s.mode == capture::ONE_SHOT;
        configure_on(&mut self.hst, &mut self.logic, s, &self.target, what_, mode_none)
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
            frontends: vec![],
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
        let mut g = Map::new();
        if let Some(x) = self.logic.generation.filter(|_| !self.names.is_empty()) {
            g.insert("logic".into(), json!(x));
        }
        if let Some(x) = self.an.as_ref().and_then(|a| a.generation) {
            g.insert("analog".into(), json!(x));
        }
        if !g.is_empty() {
            self.probe.insert("generation".into(), Value::Object(g));
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
            Value::Object(link_info(&self.hst, self.open_s, data.len() as u64, read_s, true)),
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
            Value::Object(link_info(&self.hst, self.open_s, received, read_s, false)),
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
        if let Some(r) = cfg.data_rate.filter(|&r| r > 0 && self.an.is_none()) {
            self.probe.insert("data_rate".into(), json!(r));
        }
        // logic and analog: both streamed in one group (capture §4.1)
        let acfg = match self.an.is_some() {
            true => {
                let a = self.analog_setting(capture::STREAMING, None, None);
                let an = self.an.as_mut().expect("analog");
                Some(configure_on(&mut self.hst, an, &a, &self.target, "analog", false)?)
            }
            false => None,
        };
        let base = self.req.spool_dir.clone().unwrap_or_else(std::env::temp_dir);
        let dir = make_temp_dir(&base, ".oep-acquire-stream-")?;
        let out = self.stream_into(&cfg, acfg.as_ref(), &dir);
        if out.is_err() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        out
    }

    /// A stream collected and put together in `dir`; with `acfg`, the analog track streamed beside it in one group
    /// (acquire's `_group_stream_result`).
    fn stream_into(&mut self, cfg: &Config, acfg: Option<&Config>, dir: &std::path::Path) -> Result<Acquired> {
        let req = self.req;
        let dir = dir.to_path_buf();
        let mut spool = SpoolWorker::start(Spool::new(
            self.names.clone(),
            self.ids.clone(),
            cfg.rate,
            Self::format(cfg),
            req.on_loss_stop,
            Some(&dir),
        ));
        let mut col = Collector::default();
        let fn_ = self.logic.fn_;
        let mut ag = Received::default();
        let mut group = match acfg {
            Some(_) => {
                let mut g = Group::new(&mut self.hst)?;
                let afn = self.an.as_ref().expect("analog").fn_;
                g.bind(&mut self.hst, &[fn_, afn], &[])?;
                Some(g)
            }
            None => None,
        };
        let afn = self.an.as_ref().map(|a| a.fn_).unwrap_or(0);
        // the analog calibration read before the start, as acquire does
        let calib = match (group.is_some(), self.an.as_mut()) {
            (true, Some(an)) => Some(an.calibration(&mut self.hst)?),
            _ => None,
        };
        self.hst.subscribe(fn_, STREAM_MIN_BYTES, STREAM_MAX_DELAY_MS)?;
        if let Some(g) = group.as_mut() {
            self.hst.subscribe(afn, STREAM_MIN_BYTES, STREAM_MAX_DELAY_MS)?;
            let an = self.an.as_mut().expect("analog");
            g.start(&mut self.hst, &mut [&mut self.logic, an])?;
        } else {
            self.logic.start(&mut self.hst)?;
        }
        let t_read = Instant::now();
        let deadline = t_read + Duration::from_secs_f64(req.duration_s.unwrap_or(0.0));
        let mut kept = Instant::now();
        let collected: Result<()> = (|| {
            while Instant::now() < deadline {
                let slice = STREAM_POLL.min(deadline.saturating_duration_since(Instant::now()));
                let end = Instant::now() + slice;
                while Instant::now() < end {
                    self.hst.pump(Duration::from_millis(20), true)?;
                    col.take(&mut self.hst.link, fn_, self.logic.generation, &mut spool)?;
                    if let Some(an) = self.an.as_ref().filter(|_| group.is_some()) {
                        ag.collect(&mut self.hst.link, afn, an.generation)?;
                    }
                    if kept.elapsed() >= Duration::from_secs(1) {
                        self.hst.keepalive()?;
                        kept = Instant::now();
                    }
                }
                spool.work()?;
                if req.on_loss_stop && (spool.lost_any() || ag.lost()) {
                    break;
                }
            }
            Ok(())
        })();
        let stopped = match group.as_mut() {
            Some(g) => g.stop(&mut self.hst),
            None => self.logic.stop(&mut self.hst),
        };
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
            self.hst.pump(Duration::from_millis(20), true)?;
            col.take(&mut self.hst.link, fn_, self.logic.generation, &mut spool)?;
        }
        let mut analog_end = None;
        if let Some(g) = group.as_mut() {
            let an = self.an.as_mut().expect("analog");
            finish_received(&mut self.hst, an, &mut ag)?;
            g.bind(&mut self.hst, &[], &[])?;
            analog_end = Some(an.status(&mut self.hst)?);
        }
        let read_s = t_read.elapsed().as_secs_f64();
        let st = self.logic.status(&mut self.hst)?;
        let error = st.state == capture::STATE_ERROR;
        let (parts, mut lost) = spool.finish(if error { Some(st.write_pos) } else { None })?;
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
        let received = col.bytes + ag.data.len() as u64;
        let tail = (!error && col.end.is_some_and(|e| st.write_pos > e)).then(|| st.write_pos - col.end.unwrap_or(0));
        let tail = if !error && col.end.is_none() && st.write_pos > 0 {
            Some(st.write_pos)
        } else {
            tail
        };
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, received, read_s, false)),
        );
        self.clock_after();
        self.generation();
        if let Some(c) = calib.as_ref().filter(|c| !c.factory.is_empty()) {
            self.probe.insert("calibration".into(), calibration_json(c));
        }
        let mut meta = self.base_meta();
        if analog_end.is_some() {
            let pc = meta
                .get_mut("probe_channels")
                .and_then(|v| v.as_object_mut())
                .expect("probe_channels");
            for (n, i) in self.anames.iter().zip(&self.aids) {
                pc.insert(n.clone(), json!(i));
            }
        }
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        meta.insert(
            "stream".into(),
            json!({"duration_s": req.duration_s, "on_loss": if req.on_loss_stop { "stop" } else { "continue" },
                   "frames": col.frames + ag.frames, "bytes": received, "read_s": round3(read_s)}),
        );
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
        let mut parts = parts;
        if let (Some(ast), Some(acfg)) = (analog_end, acfg) {
            let an = self.an.as_ref().expect("analog");
            let calib = calib.unwrap_or_default();
            let ctx = AnalogCtx {
                an,
                acfg,
                req,
                aids: &self.aids,
                frontends: &self.frontends,
                calib: &calib,
            };
            place_analog(&ctx, cfg.rate.into(), &ag, &ast, &mut parts, &mut lost)?;
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

// ---------------- analog lines (oep.fixture.analog) and the capture group ----------------

/// What an analog track's channels are made with (acquire's `_analog_chans` arguments).
struct AnalogCtx<'a> {
    an: &'a Logic,
    acfg: &'a Config,
    req: &'a Request,
    aids: &'a [u16],
    frontends: &'a BTreeMap<u8, Value>,
    calib: &'a Calibration,
}

/// An analog track's data -> its channels: raw values at its own rate, the first sample at tick `t0` (plus each
/// channel's skew), with what was used to take them (pin, frontend, reference, Vrefint, the start's uncertainty).
fn analog_chans(
    ctx: &AnalogCtx,
    data: &[u8],
    samples: u64,
    t0: Ratio,
    tick: Ratio,
    uncertainty_ns: Option<u64>,
    trigger_index: Option<u64>,
) -> Result<Vec<Chan>> {
    let c = ctx.acfg;
    let mut out = vec![];
    for (k, ((name, _, _), ch)) in ctx.req.analog.iter().zip(ctx.aids).enumerate() {
        let values = ctx.an.values(data, k, samples as usize)?;
        let mut acq = Map::new();
        acq.insert("pin".into(), json!(ch));
        if let Some(fe) = c.frontend.get(&k) {
            acq.insert(
                "frontend".into(),
                ctx.frontends
                    .get(fe)
                    .cloned()
                    .unwrap_or_else(|| json!({"frontend": fe})),
            );
        }
        if let Some((source, mv, measured)) = &c.reference {
            let m = match measured {
                Value::Bool(b) => *b,
                Value::String(s) => !s.is_empty(),
                _ => true,
            };
            acq.insert("reference".into(), json!({"source": source, "mv": mv, "measured": m}));
        }
        if let Some((raw, ns)) = ctx.calib.vrefint {
            acq.insert("vrefint_raw".into(), json!(raw));
            acq.insert("vrefint_ns".into(), json!(ns));
        }
        if let Some(mv) = ctx.calib.vrefint_nominal_mv {
            acq.insert("vrefint_nominal_mv".into(), json!(mv));
        }
        acq.insert("start_uncertainty_ns".into(), json!(uncertainty_ns));
        if let Some(t) = trigger_index {
            acq.insert("trigger_index".into(), json!(t));
        }
        let skew = Ratio::int(*c.skew_ns.get(&k).unwrap_or(&0) as i128);
        let n = values.len() as u64 / (c.slot as u64 / 8).max(1);
        out.push(Chan::Analog(Box::new(AnalogChan {
            name: name.clone(),
            data: values,
            n,
            rate: c.rate.into(),
            t0: t0 + skew * tick / Ratio::int(1_000_000_000),
            width: c.slot as u32,
            value_bits: (c.bits > 0).then_some(c.bits),
            zero: c.zero.get(&k).copied(),
            scale_nv: c.scale_nv.get(&k).copied(),
            acquisition: acq,
        })));
    }
    Ok(out)
}

/// meta.probe.calibration: the factory calibration, raw.
fn calibration_json(c: &Calibration) -> Value {
    Value::Array(
        c.factory
            .iter()
            .map(|(fe, scheme, raw)| {
                json!({"frontend": fe, "scheme": scheme,
                       "raw": raw.iter().map(|b| format!("{b:02x}")).collect::<String>()})
            })
            .collect(),
    )
}

/// The analog describe's frontends (capture §3.5): number -> its measurable range (a value of another length than 9
/// bytes is broken and not used).
fn frontends_of(hst: &mut Host, fn_: u16) -> BTreeMap<u8, Value> {
    let mut out = BTreeMap::new();
    for (t, v) in hst.describe(fn_).unwrap_or_default() {
        if t == 0x44 && v.len() == 9 {
            let i = |a: usize| i32::from_le_bytes([v[a], v[a + 1], v[a + 2], v[a + 3]]);
            out.insert(
                v[0],
                json!({"frontend": v[0], "range_min_mv": i(1), "range_max_mv": i(5)}),
            );
        }
    }
    out
}

/// What a track's stream collected (capture.Received): the bytes in arrival order, where the stream skipped, the
/// segments' records; pushes of another generation dropped.
#[derive(Debug, Default)]
struct Received {
    data: Vec<u8>,
    /// stream position of data[0]
    start: Option<u64>,
    /// (index in data where it skipped, bytes skipped)
    gaps: Vec<(usize, i64)>,
    seq_lost: u64,
    frames: u64,
    stale: u64,
    /// a segment's position -> what its frames said (the latest)
    segments: BTreeMap<u64, capture::SegmentInfo>,
    seq: Option<u16>,
    event_seqs: Vec<u16>,
}

impl Received {
    /// The pushes of `fn_` waiting on the link (LogicCapture._collect's rules: seq, generation, gaps, segments).
    fn collect(&mut self, link: &mut Link, fn_: u16, generation: Option<u32>) -> Result<()> {
        for e in link.events.iter().filter(|e| u16::from_le_bytes([e[1], e[2]]) == fn_) {
            self.event_seqs.push(u16::from_le_bytes([e[3], e[4]]));
        }
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
                    if let Some(i) = self.event_seqs.iter().position(|&s| s == want) {
                        self.event_seqs.remove(i);
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
                self.segments.insert(info.position, info);
            }
            match self.start {
                None => self.start = Some(p.position),
                Some(_) => {
                    let skipped = p.position as i64 - self.reached() as i64;
                    if skipped != 0 {
                        self.gaps.push((self.data.len(), skipped));
                    }
                }
            }
            self.data.extend_from_slice(&p.data);
            self.frames += 1;
        }
        link.pushes = rest;
        Ok(())
    }

    /// The stream position just past what arrived.
    fn reached(&self) -> u64 {
        (self.start.unwrap_or(0) as i64 + self.data.len() as i64 + self.gaps.iter().map(|g| g.1).sum::<i64>()) as u64
    }

    fn lost(&self) -> bool {
        !self.gaps.is_empty() || self.start.is_some_and(|s| s > 0)
    }

    /// Every byte at stream position `position` or later dropped (§2.2: after an error stop, status's write_pos).
    fn drop_from(&mut self, position: u64) {
        let Some(start) = self.start else { return };
        let mut gaps = self.gaps.clone();
        gaps.sort();
        let (mut keep, mut skipped) = (self.data.len() as i64, 0i64);
        let mut all = gaps.clone();
        all.push((self.data.len(), 0));
        for (k, (index, n)) in all.iter().enumerate() {
            let seg_from = if k > 0 { gaps[k - 1].0 as i64 } else { 0 };
            if (position as i64) < start as i64 + *index as i64 + skipped {
                keep = seg_from.max(position as i64 - start as i64 - skipped);
                break;
            }
            skipped += n;
        }
        let keep = keep.clamp(0, self.data.len() as i64) as usize;
        self.data.truncate(keep);
        self.gaps.retain(|(i, _)| *i < keep);
    }
}

/// Streaming, after the stop: the pushes still to come, up to the last byte captured (status's write position), or
/// for 5 s; in state 6 the bytes at or past write_pos are dropped (LogicCapture.finish).
fn finish_received(hst: &mut Host, track: &mut Logic, got: &mut Received) -> Result<()> {
    let st = track.status(hst)?;
    let error = st.state == capture::STATE_ERROR;
    let deadline = Instant::now()
        + if error {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        };
    let mut kept = Instant::now();
    while Instant::now() < deadline {
        got.collect(&mut hst.link, track.fn_, track.generation)?;
        if !error && got.start.is_some() && got.reached() == st.write_pos {
            return Ok(());
        }
        if kept.elapsed() >= Duration::from_secs(1) && hst.session.is_some() {
            hst.keepalive()?;
            kept = Instant::now();
        }
        hst.pump(Duration::from_millis(20), true)?;
    }
    got.collect(&mut hst.link, track.fn_, track.generation)?;
    if error {
        got.drop_from(st.write_pos);
    }
    Ok(())
}

/// One gapless stretch of an analog stream: start_ns, its uncertainty, the bytes, the samples.
type Run_ = (u64, u64, Vec<u8>, u64);

/// An analog track's stream -> its gapless runs and the positions it skipped (each a loss: the index of the run
/// after it, or None for the stream's start) (acquire's `_analog_runs`).
fn analog_runs(acfg: &Config, got: &Received) -> (Vec<Run_>, Vec<Option<usize>>) {
    let frame = acfg.frame_bytes().max(1) as i64;
    let segs: Vec<&capture::SegmentInfo> = got.segments.values().collect();
    let (mut runs, mut skips) = (vec![], vec![]);
    let (mut pos, mut at) = (got.start.unwrap_or(0) as i64, 0i64);
    if got.start.is_some_and(|s| s > 0) {
        skips.push(None);
    }
    let mut gaps = got.gaps.clone();
    gaps.sort();
    gaps.push((got.data.len(), 0));
    for (index, skipped) in gaps {
        let n = index as i64 - at;
        let inside = segs
            .iter()
            .find(|x| pos <= x.position as i64 && (x.position as i64) < pos + n);
        let samples = n / frame;
        if samples > 0
            && let Some(x) = inside
        {
            let b = &got.data[at as usize..(at + samples * frame) as usize];
            runs.push((x.start_ns, x.start_uncertainty_ns, b.to_vec(), samples as u64));
        }
        if skipped != 0 {
            skips.push(Some(runs.len()));
        }
        pos += n + skipped;
        at = index as i64;
    }
    (runs, skips)
}

/// A group stream's analog track given to the logic parts: each part the analog samples of its time (the run that
/// covers most of it, placed by the segments' start_ns), and the analog's losses (acquire's `_GroupStreamed`).
fn place_analog(
    ctx: &AnalogCtx,
    tick: Ratio,
    ag: &Received,
    ast: &capture::Status,
    parts: &mut [Part],
    lost: &mut Vec<Loss>,
) -> Result<()> {
    let arate: Ratio = ctx.acfg.rate.into();
    let (runs, skips) = analog_runs(ctx.acfg, ag);
    let frame = ctx.acfg.frame_bytes() as i128;
    let g = Ratio::int(1_000_000_000);
    let start_of = |p: &Part| p.meta.get("start_ns").and_then(|v| v.as_u64());
    let first_ns = parts.iter().filter_map(start_of).min();
    for part in parts.iter_mut() {
        let s_ns = start_of(part);
        let span = Ratio::int(part.channels.iter().map(|c| c.end()).max().unwrap_or(0) as i128) / tick;
        let mut best: Option<(Ratio, usize, Ratio, Ratio)> = None;
        if let Some(s) = s_ns {
            let s = Ratio::int(s as i128) / g;
            for (i, (r0, _, _, n)) in runs.iter().enumerate() {
                let r = Ratio::int(*r0 as i128) / g;
                let a = s.max(r);
                let b = (s + span).min(r + Ratio::int(*n as i128) / arate);
                if b > a && best.as_ref().is_none_or(|x| b - a > x.0) {
                    best = Some((b - a, i, a, b));
                }
            }
        }
        let Some((_, i, a, b)) = best else {
            let empty = analog_chans(ctx, &[], 0, Ratio::int(0), tick, None, None)?;
            part.channels.extend(empty);
            continue;
        };
        let (r0, unc, data, n) = &runs[i];
        let r = Ratio::int(*r0 as i128) / g;
        let k0 = ((a - r) * arate).ceil().max(0);
        let k1 = (((b - r) * arate).floor()).min(*n as i128);
        let s = Ratio::int(s_ns.unwrap_or(0) as i128) / g;
        let t0 = (r + Ratio::int(k0) / arate - s) * tick;
        let cnt = (k1 - k0).max(0);
        let slice = &data[(k0 * frame) as usize..((k0 + cnt) * frame) as usize];
        let chans = analog_chans(ctx, slice, cnt as u64, t0, tick, Some(*unc), None)?;
        part.channels.extend(chans);
    }
    let last = parts.len() as i64 - 1;
    for k in skips {
        let mut at = last;
        if let (Some(k), Some(_)) = (k, first_ns)
            && k < runs.len()
        {
            let r0 = runs[k].0;
            at = parts
                .iter()
                .enumerate()
                .filter(|(_, p)| start_of(p).unwrap_or(0) <= r0)
                .map(|(i, _)| i as i64)
                .max()
                .unwrap_or(0);
        }
        lost.push(Loss::new(
            if k.is_none() { -1 } else { at },
            "analog: the probe dropped a segment (no room to send)",
            false,
        ));
    }
    if ast.state == capture::STATE_ERROR {
        lost.push(Loss::new(
            last,
            format!("analog: the probe stopped in error ({})", ast.error_name()),
            true,
        ));
    }
    let aend = ag.reached();
    if ast.state != capture::STATE_ERROR && ast.write_pos > aend {
        lost.push(Loss::new(
            last,
            format!(
                "analog: the stream's last {} bytes did not arrive after the stop",
                ast.write_pos - aend
            ),
            true,
        ));
    }
    Ok(())
}

impl Run<'_> {
    /// The analog track's setting: its rate (`analog_rate_hz`, else the logic's), samples, frontends; `linked`: the
    /// group's linked trigger with this pretrigger (0: none).
    fn analog_setting(&self, mode: u8, samples: Option<u64>, linked: Option<u64>) -> Setting {
        let req = self.req;
        Setting {
            mode,
            rate: req.analog_rate_hz.unwrap_or(req.rate_hz),
            samples,
            trigger: linked.map(|_| (capture::TRIGGER_LINKED, 0, 0)),
            pretrigger: linked.filter(|&p| p > 0),
            frontends: req
                .analog
                .iter()
                .enumerate()
                .filter_map(|(k, (_, _, fe))| fe.map(|f| (k as u8, f)))
                .collect(),
            ..Setting::default()
        }
    }

    fn analog_ctx<'b>(&'b self, acfg: &'b Config, calib: &'b Calibration) -> AnalogCtx<'b> {
        AnalogCtx {
            an: self.an.as_ref().expect("analog"),
            acfg,
            req: self.req,
            aids: &self.aids,
            frontends: &self.frontends,
            calib,
        }
    }

    /// --trigger as the group's trigger source (capture §4.4): a logic channel's edge or level, or an analog
    /// channel crossing / staying beyond a voltage (in its ADC values by the answer's scale).
    fn trigger_source(&self, acfg: Option<&Config>) -> Result<(capture::Cond, Vec<u8>)> {
        let req = self.req;
        let (name, kind) = req.trigger.as_ref().expect("a trigger");
        let cond = if let Some(role) = self.names.iter().position(|n| n == name) {
            let role = role as u8;
            match kind.as_str() {
                "rise" => capture::Cond::edge(self.logic.fn_, role, 0),
                "fall" => capture::Cond::edge(self.logic.fn_, role, 1),
                "both" => capture::Cond::edge(self.logic.fn_, role, 2),
                k => capture::Cond::level(self.logic.fn_, role, (k == "high") as u8),
            }
        } else {
            let role = self.anames.iter().position(|n| n == name).unwrap_or(0);
            let acfg = acfg.expect("the analog answer");
            let (zero, scale) = (acfg.zero.get(&role).copied(), acfg.scale_nv.get(&role).copied());
            let (Some(zero), Some(scale)) = (zero, scale.filter(|&s| s != 0)) else {
                return Err(Error::Value(format!(
                    "trigger {name}: the probe gives no volt conversion for it"
                )));
            };
            let volts = req.trigger_volts.unwrap_or(0.0);
            let value = (volts * 1e9 / scale as f64 + zero as f64).round_ties_even();
            let top = (acfg.bits > 0).then(|| ((1u64 << acfg.bits) - 1) as f64);
            if value < 0.0 || top.is_some_and(|t| value > t) {
                return Err(Error::Value(format!(
                    "trigger {name}:{kind}:{}: outside the input's range",
                    wireskein_io::pyfmt::repr_f64(volts)
                )));
            }
            let (fn_, role, value) = (self.an.as_ref().expect("analog").fn_, role as u8, value as u32);
            let inverted = scale < 0; // an inverting frontend: up in volts is down in values
            match (kind.as_str(), inverted) {
                ("rise", false) | ("fall", true) => capture::Cond::cross_up(fn_, role, value),
                ("fall", false) | ("rise", true) => capture::Cond::cross_down(fn_, role, value),
                ("high", false) | ("low", true) => capture::Cond::above(fn_, role, value),
                _ => capture::Cond::below(fn_, role, value),
            }
        };
        let tlvs = capture::trigger_source_tlvs(&cond, if req.arm_now { 0 } else { 1 });
        Ok((cond, tlvs))
    }

    /// One-shot with analog lines (alone, or with logic in a group; a trigger with analog is the group's trigger
    /// source): acquire's `_capture` for those.
    fn oneshot_analog(&mut self) -> Result<Acquired> {
        let req = self.req;
        let has_cap = !self.names.is_empty();
        let grouped = req.trigger.is_some();
        let target = self.target.clone();
        let mut cfg: Option<Config> = None;
        let mut lq: Option<Asked> = None;
        let (mut lrate, mut lsamples): (Ratio, u64) = (Ratio::int(req.rate_hz as i128), req.samples);
        let mut lset = Setting::default();
        if has_cap {
            lset = self.setting(capture::ONE_SHOT);
            if grouped {
                lset.trigger = Some((capture::TRIGGER_LINKED, 0, 0));
                lset.trigger_opts = None;
                let mut q = lset.clone();
                q.glitch_filter_ns = None;
                q.input_hysteresis = None;
                let a = query_on(&mut self.hst, &mut self.logic, &q)?;
                if let Asked::Took(c) = &a {
                    if c.rate.0 > 0 {
                        lrate = c.rate.into();
                    }
                    if c.samples > 0 {
                        lsamples = c.samples;
                    }
                }
                lq = Some(a);
            } else {
                let c = self.configure(&lset, "logic")?;
                self.note_used(&c);
                lrate = c.rate.into();
                lsamples = if c.samples > 0 { c.samples } else { req.samples };
                cfg = Some(c);
            }
        }
        // the analog track: as long as the logic capture the probe answered, at the rate it will really use
        let arate = req.analog_rate_hz.unwrap_or(req.rate_hz);
        let probe_rate = |run: &mut Self| -> Result<Option<Ratio>> {
            let q = run.analog_setting(capture::ONE_SHOT, Some(1), None);
            let an = run.an.as_mut().expect("analog");
            match an.configure(&mut run.hst, &q, true) {
                Ok(c) => Ok(Some(c.rate.into()).filter(|r: &Ratio| r.is_positive())),
                Err(e) => Err(e),
            }
        };
        let asamples = match req.analog_samples {
            Some(n) => n,
            None if has_cap => {
                let real = match probe_rate(self) {
                    Ok(r) => r.unwrap_or(Ratio::int(arate as i128)),
                    Err(e) if e.is_rejected(msg::UNKNOWN_OPERATION) => Ratio::int(arate as i128),
                    Err(e) => return Err(e),
                };
                (Ratio::int(lsamples as i128) / lrate * real).round().max(1) as u64
            }
            None => req.samples,
        };
        let mut linked = None;
        if grouped {
            let mut pre = req.pretrigger.unwrap_or(0);
            if has_cap && pre > 0 {
                let real = match probe_rate(self)? {
                    Some(r) => r,
                    None => Ratio::int(0),
                };
                pre = (Ratio::int(pre as i128) * real / lrate).round() as u64;
            }
            linked = Some(pre);
        }
        let mut what_ = "analog".to_string();
        if has_cap && linked.is_some_and(|p| p > 0) {
            what_ = format!(
                "analog (its pretrigger {} of {asamples} samples is the logic's {} at {} Hz, counted at the analog \
                 rate; a longer capture or a lower logic rate gives it more)",
                linked.unwrap_or(0),
                req.pretrigger.unwrap_or(0),
                crate::fmt_g(lrate.to_f64())
            );
        }
        let aset = self.analog_setting(capture::ONE_SHOT, Some(asamples), linked);
        if grouped {
            let aq = {
                let an = self.an.as_mut().expect("analog");
                query_on(&mut self.hst, an, &aset)?
            };
            let mut refused = vec![];
            if let Some(Asked::Refused(e)) = &lq {
                refused.push(("logic".to_string(), e.clone(), false));
            }
            if let Asked::Refused(e) = &aq {
                refused.push((what_.clone(), e.clone(), true));
            }
            if !refused.is_empty() {
                let mut said = vec![];
                for (w, e, is_analog) in refused {
                    if e.is_rejected(msg::UNSUPPORTED) {
                        let most = if is_analog {
                            let an = self.an.as_mut().expect("analog");
                            said_most(&mut self.hst, an, &aset, &e)
                        } else {
                            said_most(&mut self.hst, &mut self.logic, &lset, &e)
                        };
                        said.push(format!("{w} with {}{most}", what(e.unsupported_tag())));
                    } else {
                        said.push(format!("{w}: {e}"));
                    }
                }
                return Err(Error::Runtime(format!(
                    "probe {target} cannot capture these together (each track asked before either was set): {}",
                    said.join("; ")
                )));
            }
            if has_cap {
                let c = self.configure(&lset, "logic")?;
                self.note_used(&c);
                cfg = Some(c);
            }
        }
        let acfg = {
            let an = self.an.as_mut().expect("analog");
            configure_on(&mut self.hst, an, &aset, &target, &what_, true)?
        };
        // started together (a group), or the analog track alone
        let (seg, aseg);
        if has_cap || grouped {
            let mut group = Group::new(&mut self.hst)?;
            let afn = self.an.as_ref().expect("analog").fn_;
            let fns: Vec<u16> = if has_cap { vec![self.logic.fn_, afn] } else { vec![afn] };
            let mut source = vec![];
            let mut latency = None;
            let judge_logic = req.trigger.as_ref().is_some_and(|(n, _)| self.names.contains(n));
            if grouped {
                let (cond, tlvs) = self.trigger_source(Some(&acfg))?;
                source = tlvs;
                latency = if judge_logic {
                    cfg.as_ref().and_then(|c| c.judge_latency_ns)
                } else {
                    acfg.judge_latency_ns
                };
                if let Some(l) = latency {
                    self.probe.insert("judge_latency_ns".into(), json!(l));
                }
                if let Ok(Some(caps)) = group.trigger_caps(&mut self.hst) {
                    self.probe.insert(
                        "trigger_caps".into(),
                        json!({"stages": caps.max_stages, "terms": caps.max_terms, "conds": caps.max_conds,
                               "cross_track": caps.cross_track, "kinds": caps.kinds()}),
                    );
                    if !caps.takes(cond.kind) {
                        let (n, k) = req.trigger.as_ref().expect("a trigger");
                        let v = req
                            .trigger_volts
                            .map(|v| format!(":{}V", wireskein_io::pyfmt::repr_f64(v)))
                            .unwrap_or_default();
                        let kinds = caps.kinds();
                        return Err(Error::Runtime(format!(
                            "probe {target} cannot start its tracks on {n}:{k}{v} together: its capture group \
                             triggers on {} (trigger_caps, capture §4.4)",
                            if kinds.is_empty() {
                                "nothing".to_string()
                            } else {
                                kinds.join(", ")
                            }
                        )));
                    }
                }
            }
            if let Err(e) = group.bind(&mut self.hst, &fns, &source) {
                if source.is_empty() || !matches!(e, Error::Rejected(..)) {
                    return Err(e);
                }
                let mut why = String::new();
                if let Some(l) = latency {
                    let (judge, other, other_rate) = if judge_logic {
                        ("logic", "analog", acfg.rate)
                    } else {
                        ("analog", "logic", cfg.as_ref().map(|c| c.rate).unwrap_or((0, 1)))
                    };
                    let others = if judge_logic || has_cap {
                        format!(
                            "{other} {} Hz",
                            crate::fmt_g(other_rate.0 as f64 / other_rate.1.max(1) as f64)
                        )
                    } else {
                        String::new()
                    };
                    why = format!(
                        "; the {judge} track takes up to {} ms to know the event, and the others ({others}) must hold \
                         that long besides their pretrigger - a lower rate or fewer channels on them, or the trigger \
                         on the other track",
                        wireskein_io::pyfmt::g(l as f64 / 1e6, 3)
                    );
                }
                return Err(Error::Runtime(format!(
                    "probe {target} cannot start its tracks on a trigger together (capture-group trigger source, \
                     capture §4.4: {e}){why}"
                )));
            }
            let started: Result<(u64, capture::GroupStatus, Option<capture::Segment>, capture::Segment)> = (|| {
                let start = {
                    let an = self.an.as_mut().expect("analog");
                    if has_cap {
                        group.start(&mut self.hst, &mut [&mut self.logic, an])?
                    } else {
                        group.start(&mut self.hst, &mut [an])?
                    }
                };
                let st = match group.wait(&mut self.hst, Duration::from_secs_f64(req.timeout_s)) {
                    Ok((st, _)) => st,
                    Err(Error::Timeout(m)) => {
                        if let Some((n, k)) = &req.trigger
                            && group
                                .status(&mut self.hst)
                                .is_ok_and(|s| s.state == capture::STATE_WAITING)
                        {
                            return Err(Error::Timeout(format!(
                                "no trigger ({n}:{k}) within {} s (--timeout)",
                                crate::fmt_g(req.timeout_s)
                            )));
                        }
                        return Err(Error::Timeout(m));
                    }
                    Err(e) => return Err(e),
                };
                let seg = if has_cap {
                    Some(first_segment(&mut self.hst, &mut self.logic)?)
                } else {
                    None
                };
                let an = self.an.as_mut().expect("analog");
                let aseg = first_segment(&mut self.hst, an)?;
                Ok((start, st, seg, aseg))
            })(
            );
            let unbound = group.bind(&mut self.hst, &[], &[]);
            let (start, st, s1, s2) = started?;
            unbound?;
            self.probe.insert("group_start_ns".into(), json!(start));
            if let Some((_, ns, unc)) = st.trigger {
                self.probe.insert("trigger_ns".into(), json!(ns));
                self.probe.insert("trigger_uncertainty_ns".into(), json!(unc));
            }
            (seg, aseg) = (s1, s2);
        } else {
            let an = self.an.as_mut().expect("analog");
            an.start(&mut self.hst)?;
            let segs = an.wait(&mut self.hst, Duration::from_secs_f64(req.timeout_s), None)?;
            aseg = *segs
                .first()
                .ok_or_else(|| Error::Runtime("the probe finished without a segment".into()))?;
            seg = None;
        }
        let t_read = Instant::now();
        let data = match (&seg, &cfg) {
            (Some(s), Some(c)) => {
                let g = if s.generation != 0 {
                    s.generation
                } else {
                    self.logic.generation.unwrap_or(0)
                };
                Some(
                    self.logic
                        .read(&mut self.hst, s.position, c.segment_bytes(s.samples), g)?,
                )
            }
            _ => None,
        };
        let adata = {
            let an = self.an.as_mut().expect("analog");
            let g = if aseg.generation != 0 {
                aseg.generation
            } else {
                an.generation.unwrap_or(0)
            };
            an.read(&mut self.hst, aseg.position, acfg.segment_bytes(aseg.samples), g)?
        };
        let read_s = t_read.elapsed().as_secs_f64();
        let nbytes = data.as_ref().map(|d| d.len()).unwrap_or(0) + adata.len();
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, nbytes as u64, read_s, true)),
        );
        let calib = {
            let an = self.an.as_mut().expect("analog");
            an.calibration(&mut self.hst)?
        };
        self.clock_after();
        let mut meta = Map::new();
        let (tick, mut chans, t_ref): (Ratio, Vec<Chan>, u64);
        match (&seg, &cfg, &data) {
            (Some(seg), Some(cfg), Some(data)) => {
                meta = self.base_meta();
                meta.insert("start_ns".into(), json!(seg.start_ns));
                meta.insert("start_uncertainty_ns".into(), json!(seg.start_uncertainty_ns));
                if seg.flags & capture::SEGMENT_SLIPPED != 0 {
                    meta.insert("time_base_slipped".into(), json!(true));
                }
                if let Some(t) = seg.trigger_index {
                    meta.insert("trigger_tick".into(), json!(t));
                }
                tick = cfg.rate.into();
                let fmt = Self::format(cfg);
                chans = assemble::segment_channels(&fmt, data, seg.samples, &self.names, &self.ids)?;
                if let Format::One(b) = &fmt {
                    self.probe.insert(
                        "data".into(),
                        json!({"format": cfg.format, "block": cfg.block, "bytes": data.len(),
                               "expected_bytes": b.segment_bytes(seg.samples as usize),
                               "format0_bytes": (seg.samples * self.names.len() as u64).div_ceil(8)}),
                    );
                }
                t_ref = seg.start_ns;
            }
            _ => {
                tick = acfg.rate.into();
                chans = vec![];
                meta.insert("start_ns".into(), json!(aseg.start_ns));
                meta.insert("start_uncertainty_ns".into(), json!(aseg.start_uncertainty_ns));
                t_ref = aseg.start_ns;
            }
        }
        self.generation();
        if !calib.factory.is_empty() || calib.vrefint.is_some() {
            self.probe.insert("calibration".into(), calibration_json(&calib));
        }
        let t0 = Ratio::int(aseg.start_ns as i128 - t_ref as i128) * tick / Ratio::int(1_000_000_000);
        let ctx = self.analog_ctx(&acfg, &calib);
        chans.extend(analog_chans(
            &ctx,
            &adata,
            aseg.samples,
            t0,
            tick,
            Some(aseg.start_uncertainty_ns),
            aseg.trigger_index,
        )?);
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        Ok(Acquired {
            tick_hz: (tick.num as u64, tick.den as u64),
            channels: chans,
            meta,
            parts: None,
            lost: vec![],
            layout: "timeline".into(),
            temp_dir: None,
            samples: seg.map(|s| s.samples),
        })
    }

    /// An analog track alone, streamed (capture §2.1 mode 3): its frames joined per gapless stretch, a part each on
    /// one timeline (tick: the analog rate), placed by the segments' start_ns; a skipped position is a loss.
    fn analog_stream(&mut self) -> Result<Acquired> {
        let req = self.req;
        let target = self.target.clone();
        let aset = self.analog_setting(capture::STREAMING, None, None);
        let acfg = {
            let an = self.an.as_mut().expect("analog");
            configure_on(&mut self.hst, an, &aset, &target, "analog", false)?
        };
        if let Some(b) = acfg.stream_buffer {
            self.probe.insert(
                "stream_buffer".into(),
                json!({"bytes": b, "tolerable_delay_s": acfg.tolerable_delay_s()}),
            );
        }
        // read before the stream starts, as acquire does (its Vrefint is the last start's, so none here)
        let calib = self.an.as_mut().expect("analog").calibration(&mut self.hst)?;
        let afn = self.an.as_ref().expect("analog").fn_;
        let mut got = Received::default();
        self.hst.subscribe(afn, STREAM_MIN_BYTES, STREAM_MAX_DELAY_MS)?;
        self.an.as_mut().expect("analog").start(&mut self.hst)?;
        let t_read = Instant::now();
        let deadline = t_read + Duration::from_secs_f64(req.duration_s.unwrap_or(0.0));
        let mut kept = Instant::now();
        let collected: Result<()> = (|| {
            while Instant::now() < deadline {
                let end = Instant::now() + STREAM_POLL.min(deadline.saturating_duration_since(Instant::now()));
                while Instant::now() < end {
                    let g = self.an.as_ref().and_then(|a| a.generation);
                    got.collect(&mut self.hst.link, afn, g)?;
                    if kept.elapsed() >= Duration::from_secs(1) {
                        self.hst.keepalive()?;
                        kept = Instant::now();
                    }
                    self.hst.pump(Duration::from_millis(20), true)?;
                }
                let g = self.an.as_ref().and_then(|a| a.generation);
                got.collect(&mut self.hst.link, afn, g)?;
                if req.on_loss_stop && got.lost() {
                    break;
                }
            }
            Ok(())
        })();
        let stopped = self.an.as_mut().expect("analog").stop(&mut self.hst);
        collected?;
        stopped?;
        {
            let an = self.an.as_mut().expect("analog");
            finish_received(&mut self.hst, an, &mut got)?;
        }
        let read_s = t_read.elapsed().as_secs_f64();
        let st = self.an.as_mut().expect("analog").status(&mut self.hst)?;
        let drops_line = self.note_status(&st);
        let error = st.state == capture::STATE_ERROR;
        let end = got.reached();
        let tail = (!error && st.write_pos > end).then(|| st.write_pos - end);
        self.probe.insert(
            "link".into(),
            Value::Object(link_info(&self.hst, self.open_s, got.data.len() as u64, read_s, false)),
        );
        self.clock_after();
        self.generation();
        if !calib.factory.is_empty() {
            self.probe.insert("calibration".into(), calibration_json(&calib));
        }
        let mut meta = Map::new();
        let pc: Map<String, Value> = self
            .anames
            .iter()
            .zip(&self.aids)
            .map(|(n, i)| (n.clone(), json!(i)))
            .collect();
        meta.insert("probe_channels".into(), Value::Object(pc));
        meta.insert("probe".into(), Value::Object(std::mem::take(&mut self.probe)));
        meta.insert(
            "stream".into(),
            json!({"duration_s": req.duration_s, "on_loss": if req.on_loss_stop { "stop" } else { "continue" },
                   "frames": got.frames, "bytes": got.data.len(), "read_s": round3(read_s)}),
        );
        // the parts: each gapless stretch
        if got.data.is_empty() || got.segments.is_empty() {
            return Err(Error::Runtime(format!(
                "the stream brought no data{}",
                if got.gaps.is_empty() {
                    ""
                } else {
                    " (the probe dropped it all)"
                }
            )));
        }
        let frame = acfg.frame_bytes().max(1) as i64;
        let rate: Ratio = acfg.rate.into();
        let segs: Vec<capture::SegmentInfo> = got.segments.values().copied().collect();
        let first_ns = segs[0].start_ns;
        let (mut parts, mut lost): (Vec<Part>, Vec<Loss>) = (vec![], vec![]);
        let start = got.start.unwrap_or(0);
        if start != 0 {
            if req.on_loss_stop {
                return Err(Error::Runtime(format!(
                    "the probe dropped the stream's first {start} bytes before any arrived"
                )));
            }
            lost.push(Loss::new(
                -1,
                format!("the probe dropped the first {start} bytes (no room to send)"),
                false,
            ));
        }
        let calib2 = calib.clone();
        let ctx = self.analog_ctx(&acfg, &calib2);
        let mut gaps = got.gaps.clone();
        gaps.sort();
        gaps.push((got.data.len(), 0));
        let (mut pos, mut at) = (start as i64, 0i64);
        for (r, (index, skipped)) in gaps.into_iter().enumerate() {
            let n = index as i64 - at;
            if r > 0 && req.on_loss_stop {
                if let Some(l) = lost.last_mut() {
                    l.stopped = true;
                }
                break;
            }
            let x = segs
                .iter()
                .find(|x| pos <= x.position as i64 && (x.position as i64) < pos + n);
            let samples = n / frame;
            if samples > 0 {
                let b = &got.data[at as usize..(at + samples * frame) as usize];
                let chans = analog_chans(
                    &ctx,
                    b,
                    samples as u64,
                    Ratio::int(0),
                    rate,
                    x.map(|x| x.start_uncertainty_ns),
                    None,
                )?;
                let tick0 = match x {
                    Some(x) => (Ratio::int(x.start_ns as i128 - first_ns as i128) / Ratio::int(1_000_000_000) * rate)
                        .round() as i64,
                    None => parts
                        .last()
                        .map(|p: &Part| {
                            p.tick0.unwrap_or(0)
                                + match &p.channels[0] {
                                    Chan::Analog(a) => a.n as i64,
                                    c => c.end() as i64,
                                }
                        })
                        .unwrap_or(0),
                };
                let mut m = Map::new();
                if let Some(x) = x {
                    m.insert("start_ns".into(), json!(x.start_ns));
                    m.insert("start_uncertainty_ns".into(), json!(x.start_uncertainty_ns));
                }
                parts.push(Part {
                    channels: chans,
                    meta: m,
                    tick0: Some(tick0),
                    samples: Some(samples as u64),
                });
            }
            if skipped != 0 {
                lost.push(Loss::new(
                    parts.len() as i64 - 1,
                    "the probe dropped a segment (no room to send)",
                    false,
                ));
            }
            pos += n + skipped;
            at = index as i64;
        }
        if parts.is_empty() {
            return Err(Error::Runtime("the stream brought no whole sample".into()));
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
            tick_hz: acfg.rate,
            channels: parts[0].channels.clone(),
            meta,
            parts: Some(parts),
            lost,
            layout: "timeline".into(),
            temp_dir: None,
            samples: None,
        })
    }

    /// status's named extensions into meta.probe (drops, spill) -> the drops in one line.
    fn note_status(&mut self, st: &capture::Status) -> String {
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
        drops_line
    }
}

/// A track's first segment record (acquire's `_segment`).
fn first_segment(hst: &mut Host, track: &mut Logic) -> Result<capture::Segment> {
    track
        .segments(hst, 0)?
        .first()
        .copied()
        .ok_or_else(|| Error::Runtime("the probe finished without a segment".into()))
}

/// The reference firmware's capture-drops counters (a named extension of status, not an OEP definition; its forms
/// of 50, 42, 34 and 30 bytes as oep-client-python's capture.CaptureDrops reads them).
fn drops(v: &[u8]) -> Option<Map<String, Value>> {
    let n = v.len();
    if ![30, 34, 42, 50].contains(&n) {
        return None;
    }
    let u32at = |i: usize| u32::from_le_bytes(v[i..i + 4].try_into().expect("4")) as u64;
    let mut m = Map::new();
    for (k, name) in ["no_stage_bytes", "unqueued_bytes", "dma_overruns", "queue_overflows"]
        .iter()
        .enumerate()
    {
        m.insert((*name).into(), json!(u32at(4 * k)));
    }
    m.insert("zero_copy".into(), json!(v[16] != 0));
    m.insert("stages".into(), json!(v[17]));
    for (k, name) in [
        "internal_largest",
        "dma_largest",
        "psram_total",
        "buffer_peak",
        "spill_peak",
        "spill_bytes",
    ]
    .iter()
    .enumerate()
    {
        let at = 18 + 4 * k;
        m.insert((*name).into(), if at + 4 <= n { json!(u32at(at)) } else { Value::Null });
    }
    m.insert(
        "dropped_bytes".into(),
        if n == 50 {
            json!(u64::from_le_bytes(v[42..50].try_into().expect("8")))
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
    fn take(&mut self, link: &mut Link, fn_: u16, generation: Option<u32>, spool: &mut dyn Feed) -> Result<()> {
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
            spool.run(p.position, p.data)?;
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
             tcp://HOST:PORT, a serial port /dev/... or oep://UNIT_ID?via=tty:PATH; HID, CDC, DNS-SD and brokers are \
             oep-client-python's)"
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
    let mut hst = open(addr)?;
    // the session kept per probe: a run killed half way does not leave its lock to the next (host guide §5)
    hst.kept = Some(crate::kept::KeptSession::new(None));
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
    let alines: Vec<String> = req.analog.iter().map(|(_, c, _)| c.clone()).collect();
    let ids = if lines.is_empty() {
        vec![]
    } else {
        tri!(channel_ids(&mut hst, &lines))
    };
    let aids = if alines.is_empty() {
        vec![]
    } else {
        tri!(channel_ids(&mut hst, &alines))
    };
    let logic = if ids.is_empty() {
        Logic {
            fn_: 0,
            analog: false,
            config: Config::default(),
            generation: None,
        }
    } else {
        tri!(Logic::new(&mut hst))
    };
    let an = if aids.is_empty() {
        None
    } else {
        Some(tri!(Logic::analog(&mut hst)))
    };
    let mut plan: Vec<(u16, u8, u16)> = ids
        .iter()
        .enumerate()
        .map(|(k, &ch)| (logic.fn_, k as u8, ch))
        .collect();
    if let Some(a) = &an {
        plan.extend(aids.iter().enumerate().map(|(k, &ch)| (a.fn_, k as u8, ch)));
    }
    let frontends = an.as_ref().map(|a| frontends_of(&mut hst, a.fn_)).unwrap_or_default();
    tri!(hst.plan_apply(&plan));
    let mut fns: Vec<u16> = plan.iter().map(|p| p.0).collect();
    fns.sort();
    fns.dedup();
    let mut run = Run {
        hst,
        logic,
        an,
        anames: req.analog.iter().map(|(n, _, _)| n.clone()).collect(),
        aids,
        frontends,
        req,
        target: target.to_string(),
        names: req.channels.iter().map(|(n, _)| n.clone()).collect(),
        ids,
        probe,
        open_s,
        clock_before,
    };
    let out = match (req.mode, run.an.is_some(), run.names.is_empty()) {
        (Mode::OneShot, false, _) => run.oneshot(),
        (Mode::OneShot, true, _) => run.oneshot_analog(),
        (Mode::Repeat, _, _) => run.repeat(),
        (Mode::Stream, true, true) => run.analog_stream(),
        (Mode::Stream, _, _) => run.stream(),
    };
    let released = run.hst.plan_release(&fns);
    let res = out.and_then(|a| released.map(|_| a));
    (run.hst, res)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the reading side takes a second: frames of a 7-line format 0 stream through the link, the collector and
    /// the spool on disk (cargo test --release -p wireskein-oep -- --ignored --nocapture).
    #[test]
    #[ignore]
    fn the_reading_side_keeps_up() {
        let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(transport::BACKLOG);
        let frame = 19_456usize;
        let total = 400usize << 20;
        let producer = std::thread::spawn(move || {
            let mut pos = 0u64;
            let mut seq = 0u16;
            while (pos as usize) < total {
                let mut f = vec![msg::ROLE_DATA];
                f.extend(5u16.to_le_bytes());
                f.extend(seq.to_le_bytes());
                f.extend(pos.to_le_bytes());
                f.extend((frame as u16).to_le_bytes());
                f.extend((0..frame).map(|i| (i * 7) as u8));
                let mut info = (pos / 65536 * 65536).to_le_bytes().to_vec();
                info.extend(65536u32.to_le_bytes());
                info.extend(0u64.to_le_bytes());
                info.extend(5u32.to_le_bytes());
                f.extend(msg::tlv(0x02, &info));
                let mut w = (f.len() as u16).to_le_bytes().to_vec();
                w.extend(f);
                if tx.send(w).is_err() {
                    return;
                }
                pos += frame as u64;
                seq = seq.wrapping_add(1);
            }
        });
        let mut link = Link::new(transport::Transport::from_rx(rx));
        let dir = make_temp_dir(&std::env::temp_dir(), ".oep-bench-").unwrap();
        let names: Vec<String> = (0..7).map(|k| format!("D{k}")).collect();
        let mut spool = SpoolWorker::start(Spool::new(
            names,
            (0..7).collect(),
            (40_000_000, 1),
            Format::Zero {
                width: 8,
                positions: (0..7).collect(),
            },
            true,
            Some(&dir),
        ));
        let mut col = Collector::default();
        let t0 = Instant::now();
        let mut last = Instant::now();
        let (mut tp, mut tt, mut tw) = (0f64, 0f64, 0f64);
        while col.bytes < total as u64 {
            let a = Instant::now();
            link.pump(Duration::from_millis(20), true).unwrap();
            let b = Instant::now();
            col.take(&mut link, 5, None, &mut spool).unwrap();
            let c = Instant::now();
            if last.elapsed() >= STREAM_POLL {
                Feed::work(&mut spool).unwrap();
                last = Instant::now();
            }
            tp += (b - a).as_secs_f64();
            tt += (c - b).as_secs_f64();
            tw += c.elapsed().as_secs_f64();
        }
        println!("reading: pump {tp:.2} s, take {tt:.2} s, handing work {tw:.2} s");
        let (parts, _) = spool.finish(None).unwrap();
        assert_eq!(parts.len(), 1);
        let s = t0.elapsed().as_secs_f64();
        producer.join().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        println!("{:.1} MB/s", total as f64 / s / 1e6);
    }
}
