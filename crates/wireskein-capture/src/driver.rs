//! A capture device's driver: how WireSkein finds a device, opens and holds it, asks what it can do, captures from
//! it (one-shot, repeat, stream), stops and closes it. Every kind of device (`oep`, `demo`, ...) is one `Driver`;
//! the built-in kinds are WireSkein's own (nothing is plugged in from outside), listed by wireskein-sources.
//!
//! Two shapes of device are told apart by what they can do (`Caps`): one that sends its samples as it takes them
//! (`Caps::stream`: the host must keep up, its limit a rate in bytes) and one that keeps them in its own memory and
//! is read out once done (`Caps::memory`: the memory bounds how long, a trigger with samples kept before it). A
//! device may need its firmware or FPGA configuration sent each time it is opened (`Caps::firmware`): WireSkein does
//! not ship such files; `find_firmware` looks for them where the user put them (or where the vendor's application
//! installed them) and says plainly where it looked when they are not there.

use crate::error::{Error, Result};
use crate::parts::Acquired;
use crate::request::{Mode, Request};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// A kind of device.
pub trait Driver: Send + Sync {
    /// the kind's name, the source's prefix (`oep` in `oep:oep://30eda0ea068b`)
    fn kind(&self) -> &'static str;
    /// one line: what it is and the address forms it opens
    fn about(&self) -> &'static str;
    /// the devices of this kind that can be seen now (those a host cannot list, e.g. on a network, are not)
    fn list(&self) -> Result<Vec<Found>>;
    /// the device at `address` (the source without `<kind>:`), opened and held for `how.owner`
    fn open(&self, address: &str, how: &Open) -> Result<Box<dyn Device>>;
}

/// An open device, held until closed (or dropped).
pub trait Device: Send {
    /// what it says about itself (model, serial, firmware, ...): the capture's meta.probe
    fn facts(&self) -> Map<String, Value>;
    /// how it is held against other users
    fn hold(&self) -> Hold;
    /// what it can do (a device that must be asked asks it)
    fn caps(&mut self) -> Result<Caps>;
    /// one capture: the request checked against what it can do, set on the device, started, taken, stopped and put
    /// together; `ctl` stops it early and hears how it goes. Data lost on the way is in the result's `lost` (a
    /// failure, what was taken kept), not an error.
    fn capture(&mut self, req: &Request, ctl: &Ctl) -> Result<Acquired>;
    /// let it go (the hold with it)
    fn close(self: Box<Self>) -> Result<()>;
}

/// A device that can be opened: its source (`<kind>:<address>`), a label to show, and what it said.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub source: String,
    pub label: String,
    pub facts: Map<String, Value>,
}

impl Found {
    pub fn to_json(&self) -> Value {
        json!({"source": self.source, "label": self.label, "facts": self.facts})
    }
}

/// How a device is opened.
#[derive(Debug, Clone)]
pub struct Open {
    /// who holds it (an OEP lease's owner, shown to whoever finds it busy)
    pub owner: String,
    /// how long to wait for one who holds it to let it go
    pub wait: Duration,
}

impl Open {
    pub fn new(owner: &str) -> Open {
        Open {
            owner: owner.into(),
            wait: Duration::from_secs(5),
        }
    }
}

/// How a device is held against other users while open.
#[derive(Debug, Clone, PartialEq)]
pub enum Hold {
    /// not at all (a demo)
    None,
    /// a lease the device keeps (OEP: renewed while it runs, lapsing if the host dies), with its owner's name
    Lease { owner: String, ms: u32 },
    /// a lock the host keeps (a claimed USB interface, a tty's TIOCEXCL and flock, a lock file): what it is
    Lock { what: String },
}

impl Hold {
    pub fn to_json(&self) -> Value {
        match self {
            Hold::None => json!({"kind": "none"}),
            Hold::Lease { owner, ms } => json!({"kind": "lease", "owner": owner, "ms": ms}),
            Hold::Lock { what } => json!({"kind": "lock", "what": what}),
        }
    }
}

/// How fast it goes with this many lines on: up to `lines` lines, up to `max_hz` samples a second (a device whose
/// limit is its link or its memory's bandwidth goes faster with fewer lines).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimit {
    pub lines: usize,
    pub max_hz: u64,
}

/// The fastest of `limits` with `lines` lines on (None: more lines than any allows).
pub fn max_rate(limits: &[RateLimit], lines: usize) -> Option<u64> {
    limits.iter().filter(|l| lines <= l.lines).map(|l| l.max_hz).max()
}

/// A device that sends its samples as it takes them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StreamCaps {
    pub limits: Vec<RateLimit>,
    /// what its link carries at most, bytes a second (None: not known)
    pub bytes_per_s: Option<u64>,
    /// bytes it holds back itself while the host is late (None: not known)
    pub buffer_bytes: Option<u64>,
}

/// A device that keeps its samples in its own memory and is read out once done.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryCaps {
    pub limits: Vec<RateLimit>,
    /// its memory for samples, in bytes
    pub bytes: u64,
    /// how a sample of `lines` lines is packed in it: bits a sample, the fewest whole bytes of 1, 2 or 4 (None)
    /// or a width of its own
    pub sample_bits: Option<usize>,
    /// whether samples before a trigger are kept (how many: the request's pretrigger)
    pub pretrigger: bool,
}

impl MemoryCaps {
    /// The most samples it keeps with `lines` lines on.
    pub fn samples_for(&self, lines: usize) -> u64 {
        let bits = self
            .sample_bits
            .unwrap_or_else(|| crate::packed::Packing::bytes_for(lines.max(1)).width) as u64;
        self.bytes * 8 / bits.max(1)
    }
}

/// The logic lines' threshold a device sets.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Thresholds {
    /// none to set (fixed by its hardware)
    #[default]
    Fixed,
    /// one of these, volts
    Steps(Vec<f64>),
    /// any from `min` to `max` volts in steps of `step`
    Range { min: f64, max: f64, step: f64 },
}

/// A file a device needs sent each time it is opened (its firmware, an FPGA configuration), which WireSkein does not
/// ship: the user's copy (often installed with the vendor's application).
#[derive(Debug, Clone, PartialEq)]
pub struct FirmwareNeed {
    /// the file's name
    pub file: String,
    /// what it is ("the FPGA configuration")
    pub what: String,
    /// where a user gets it ("installed with the vendor's application ...")
    pub from: String,
    /// where the vendor's application puts it, looked in last
    pub vendor_dirs: Vec<PathBuf>,
}

/// One logic or analog line of a device.
#[derive(Debug, Clone, PartialEq)]
pub struct LineCap {
    /// the id a request names it by
    pub id: String,
    pub analog: bool,
}

/// What a device can do. A capture asked beyond it is refused with the reason (`Caps::check`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Caps {
    pub lines: Vec<LineCap>,
    /// the rates it takes (empty: any up to its limits, the nearest it keeps taken)
    pub rates: Vec<u64>,
    pub stream: Option<StreamCaps>,
    pub memory: Option<MemoryCaps>,
    pub modes: Vec<Mode>,
    /// trigger kinds on a logic line (empty: none)
    pub triggers: Vec<String>,
    pub thresholds: Thresholds,
    /// lines kept reduced over intervals (wireskein.sources.REDUCTIONS: empty for none)
    pub reductions: Vec<String>,
    /// files sent at each open
    pub firmware: Vec<FirmwareNeed>,
    /// anything else it says
    pub extra: Map<String, Value>,
}

impl Caps {
    /// The fastest it takes with `lines` lines on, streaming or into its memory.
    pub fn max_rate(&self, lines: usize, stream: bool) -> Option<u64> {
        let limits = if stream {
            self.stream.as_ref().map(|s| &s.limits[..])
        } else {
            self.memory
                .as_ref()
                .map(|m| &m.limits[..])
                .or(self.stream.as_ref().map(|s| &s.limits[..]))
        };
        max_rate(limits.unwrap_or(&[]), lines)
    }

    /// Whether `req` is within what it can do: Error::Value with the reason when not.
    pub fn check(&self, req: &Request) -> Result<()> {
        req.check()?;
        let known: Vec<&str> = self.lines.iter().filter(|l| !l.analog).map(|l| l.id.as_str()).collect();
        for (name, id) in &req.channels {
            if !known.contains(&id.as_str()) {
                return Err(Error::Value(format!(
                    "{name}={id}: no such line on this device (its lines: {})",
                    known.join(", ")
                )));
            }
        }
        let aknown: Vec<&str> = self.lines.iter().filter(|l| l.analog).map(|l| l.id.as_str()).collect();
        for (name, id, _) in &req.analog {
            if !aknown.contains(&id.as_str()) {
                return Err(Error::Value(format!(
                    "{name}={id}: no such analog line on this device{}",
                    if aknown.is_empty() {
                        " (it has none)".to_string()
                    } else {
                        format!(" (its analog lines: {})", aknown.join(", "))
                    }
                )));
            }
        }
        if !self.modes.contains(&req.mode) {
            let can: Vec<&str> = self.modes.iter().map(|m| m.name()).collect();
            return Err(Error::Value(format!(
                "mode {}: this device does {}",
                req.mode.name(),
                can.join(", ")
            )));
        }
        let stream = req.mode == Mode::Stream;
        let n = req.channels.len();
        if n > 0 {
            match self.max_rate(n, stream) {
                None => {
                    return Err(Error::Value(format!(
                        "{n} lines: more than this device takes{}",
                        if stream { " streaming" } else { "" }
                    )));
                }
                Some(max) if req.rate_hz > max => {
                    return Err(Error::Value(format!(
                        "{} Hz with {n} lines: this device takes {max} Hz at most{}",
                        req.rate_hz,
                        if stream { " streaming" } else { "" }
                    )));
                }
                _ => {}
            }
            if !self.rates.is_empty() && !self.rates.contains(&req.rate_hz) {
                return Err(Error::Value(format!(
                    "{} Hz: this device takes {}",
                    req.rate_hz,
                    self.rates.iter().map(|r| r.to_string()).collect::<Vec<_>>().join(", ")
                )));
            }
        }
        if !stream && let Some(m) = &self.memory {
            let most = m.samples_for(n);
            let want = req.samples * req.segments.unwrap_or(1);
            if want > most {
                return Err(Error::Value(format!(
                    "{want} samples of {n} lines: this device keeps {most} at most ({} bytes of memory)",
                    m.bytes
                )));
            }
            if req.pretrigger.is_some_and(|p| p > 0) && !m.pretrigger {
                return Err(Error::Value(
                    "a pretrigger: this device keeps no samples before its trigger".into(),
                ));
            }
        }
        if let Some((_, kind)) = &req.trigger
            && req.trigger_volts.is_none()
            && !self.triggers.contains(kind)
        {
            return Err(Error::Value(format!(
                "trigger {kind}: this device {}",
                if self.triggers.is_empty() {
                    "has no trigger".to_string()
                } else {
                    format!("takes {}", self.triggers.join(", "))
                }
            )));
        }
        for (name, (mode, _)) in &req.resolution {
            if !self.reductions.contains(mode) {
                return Err(Error::Value(format!(
                    "{name}: this device does not reduce a line ({mode})"
                )));
            }
        }
        if let Some(v) = req.threshold_v {
            let ok = match &self.thresholds {
                Thresholds::Fixed => false,
                Thresholds::Steps(s) => s.iter().any(|x| (x - v).abs() < 1e-9),
                Thresholds::Range { min, max, .. } => *min <= v && v <= *max,
            };
            if !ok {
                return Err(Error::Value(format!(
                    "threshold {v} V: this device {}",
                    match &self.thresholds {
                        Thresholds::Fixed => "sets none".to_string(),
                        Thresholds::Steps(s) => format!("takes {s:?} V"),
                        Thresholds::Range { min, max, .. } => format!("takes {min} to {max} V"),
                    }
                )));
            }
        }
        Ok(())
    }

    pub fn to_json(&self) -> Value {
        let lim = |l: &[RateLimit]| -> Value {
            l.iter()
                .map(|x| json!({"lines": x.lines, "max_hz": x.max_hz}))
                .collect()
        };
        json!({
            "lines": self.lines.iter().map(|l| json!({"id": l.id, "analog": l.analog})).collect::<Vec<_>>(),
            "rates": self.rates,
            "stream": self.stream.as_ref().map(|s| json!({"limits": lim(&s.limits), "bytes_per_s": s.bytes_per_s,
                                                           "buffer_bytes": s.buffer_bytes})),
            "memory": self.memory.as_ref().map(|m| json!({"limits": lim(&m.limits), "bytes": m.bytes,
                                                           "sample_bits": m.sample_bits, "pretrigger": m.pretrigger})),
            "modes": self.modes.iter().map(|m| m.name()).collect::<Vec<_>>(),
            "triggers": self.triggers,
            "thresholds": match &self.thresholds {
                Thresholds::Fixed => Value::Null,
                Thresholds::Steps(s) => json!(s),
                Thresholds::Range { min, max, step } => json!({"min": min, "max": max, "step": step}),
            },
            "reductions": self.reductions,
            "firmware": self.firmware.iter().map(|f| json!({"file": f.file, "what": f.what, "from": f.from}))
                .collect::<Vec<_>>(),
            "extra": self.extra,
        })
    }
}

/// Where a capture is: taking samples (`Taking`) or reading a device's memory out (`Reading`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Taking,
    Reading,
}

/// How a capture goes, as it goes.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub stage: Stage,
    pub elapsed_s: f64,
    /// bytes received so far, and how many there will be when known (a memory read out, a stream's duration)
    pub bytes: u64,
    pub of_bytes: Option<u64>,
    /// bytes received and not yet put together and written out
    pub backlog: u64,
    pub losses: usize,
}

type Hear = Arc<dyn Fn(&Progress) + Send + Sync>;

/// The caller's hold on a capture as it runs: stop it (from another thread: Ctrl-C, a window's button) and hear how
/// it goes.
#[derive(Clone, Default)]
pub struct Ctl {
    stop: Arc<AtomicBool>,
    hear: Option<Hear>,
}

impl Ctl {
    pub fn new() -> Ctl {
        Ctl::default()
    }
    pub fn hearing(mut self, f: impl Fn(&Progress) + Send + Sync + 'static) -> Ctl {
        self.hear = Some(Arc::new(f));
        self
    }
    /// Stop the capture (what was taken so far is kept: a stream ends as at its duration, a memory still read out).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }
    pub fn tell(&self, p: &Progress) {
        if let Some(h) = &self.hear {
            h(p)
        }
    }
}

/// Where a file a device needs is looked for, in this order: `WIRESKEIN_FIRMWARE_<KIND>` (the file, or a directory
/// with it), `WIRESKEIN_FIRMWARE_DIR`, WireSkein's firmware directory for the kind (`<config>/wireskein/firmware/
/// <kind>`: on Linux $XDG_CONFIG_HOME or ~/.config, on macOS ~/Library/Application Support, on Windows %APPDATA%),
/// then the vendor's own places.
pub fn firmware_places(kind: &str, need: &FirmwareNeed) -> Vec<PathBuf> {
    let mut out = vec![];
    let var = format!("WIRESKEIN_FIRMWARE_{}", kind.to_ascii_uppercase().replace('-', "_"));
    if let Some(v) = std::env::var_os(&var) {
        out.push(PathBuf::from(v));
    }
    if let Some(v) = std::env::var_os("WIRESKEIN_FIRMWARE_DIR") {
        out.push(PathBuf::from(v));
    }
    if let Some(c) = config_dir() {
        out.push(c.join("wireskein").join("firmware").join(kind));
    }
    out.extend(need.vendor_dirs.iter().cloned());
    out
}

fn config_dir() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA")
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library").join("Application Support"))
    } else {
        env("XDG_CONFIG_HOME").or_else(|| env("HOME").map(|h| h.join(".config")))
    }
}

/// The file a device of `kind` needs: the first of `firmware_places` that is it or holds it; Error::NotFound naming
/// what it is, where to get it and every place looked in, when none does.
pub fn find_firmware(kind: &str, need: &FirmwareNeed) -> Result<PathBuf> {
    let places = firmware_places(kind, need);
    for p in &places {
        let f = if p.is_dir() { p.join(&need.file) } else { p.clone() };
        if f.is_file() && f.file_name().is_some_and(|n| n == Path::new(&need.file).as_os_str()) {
            return Ok(f);
        }
    }
    let shown: Vec<String> = places.iter().map(|p| format!("  {}", p.display())).collect();
    Err(Error::NotFound(format!(
        "{kind}: this device needs {} ({}) sent each time it is opened, which WireSkein does not ship: {}. Looked \
         in:\n{}\nPut the file in one of these, or point WIRESKEIN_FIRMWARE_{} at it (or at its directory)",
        need.what,
        need.file,
        need.from,
        shown.join("\n"),
        kind.to_ascii_uppercase().replace('-', "_")
    )))
}

/// One capture from `driver`'s device at `address`: opened, captured, closed (closed whatever the capture ended in).
pub fn capture_with(driver: &dyn Driver, address: &str, req: &Request, how: &Open, ctl: &Ctl) -> Result<Acquired> {
    req.check()?;
    let mut dev = driver.open(address, how)?;
    let got = dev.capture(req, ctl);
    let closed = dev.close();
    let got = got?;
    closed?;
    Ok(got)
}
