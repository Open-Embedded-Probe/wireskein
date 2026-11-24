//! WireSkein's capture files read in plain Rust (docs/wireskein-format.md; Python's `_fileformat.py`,
//! `_engine/fileio.py`, `_engine/srio.py`, `_engine/vcdio.py`): a WireSkein file (`.wireskein`), a sigrok session
//! (`.sr`) or a VCD, told apart by content (§2.3), read as its parts' logic lines - each line's level before the part
//! and the ticks it changes at, what the core reads.
//!
//! A WireSkein file's channels: `bits`, `events` (a line kept as its changes; at a step of 2 or more read at that
//! step where no quantum hides a change), the reductions `any_high` / `any_low` (each interval a sample at its first
//! tick) and `change_latch` (as events), analog left out (named), an encoding this reader does not know left out and
//! named (never read as another). Parts of a timeline that follow on without a gap are joined into one stretch of
//! time, as `fileio.load_runs` does. Kept apart from the core: the zip reader is this crate's dependency, not the
//! core's.
//!
//! The readers' pair (`write`): a capture written as a WireSkein file, a sigrok session or a VCD, as Python writes
//! it. JSON as Python holds it (`j`: dicts in their keys' order) and numbers as Python writes them (`pyfmt`), shared
//! by the crates that answer as Python does.

pub mod j;
pub mod markers;
pub mod pyfmt;
pub mod sr;
pub mod stream;
pub mod vcd;
pub mod write;
pub mod wsk;

use serde_json::{Map, Value};
use std::path::Path;

/// A logic line of a part: its level before tick 0 (its first sample's), the ticks it changes at, its step.
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub name: String,
    pub initial: u8,
    pub edges: Vec<i64>,
    pub step: i64,
    /// the tick of its first sample
    pub phase: i64,
    /// a reduced line (wireskein-format §4.5): "any_high", "any_low" or "change_latch"
    pub reduction: Option<String>,
    /// how it was taken (pin, ...: wireskein-format §3.1.1)
    pub acquisition: Map<String, Value>,
}

/// A line kept as its changes at a step of 2 or more (an `events` or `change_latch` channel, Python's EventTrace):
/// quantum k is ticks [phase + k * step, phase + (k + 1) * step); in each quantum listed the line changed counts[i]
/// times (127: at least; change_latch: 1 an odd number, 2 an even one) and ended at levels[i].
#[derive(Debug, Clone, PartialEq)]
pub struct Events {
    pub name: String,
    pub step: i64,
    pub phase: i64,
    /// quanta covered
    pub n: i64,
    pub start_level: u8,
    pub quanta: Vec<i64>,
    pub levels: Vec<u8>,
    pub counts: Vec<u8>,
    /// "change_latch" for a reduced line
    pub reduction: Option<String>,
    pub acquisition: Map<String, Value>,
}

/// An analog channel of a WireSkein file (wireskein-format §4.2, §4.3): sample k at tick t0_ticks + k * tick_hz /
/// rate_hz; raw integers ("analog": volts are (raw - zero) * scale_nv * 1e-9 when both are known) or volts
/// ("analog-f32").
#[derive(Debug, Clone, PartialEq)]
pub struct Analog {
    pub name: String,
    pub encoding: String,
    /// samples a second, exact
    pub rate_hz: (i64, i64),
    /// the tick of the first sample, exact
    pub t0_ticks: (i64, i64),
    pub width: i64,
    pub value_bits: Option<i64>,
    pub zero: Option<f64>,
    pub scale_nv: Option<f64>,
    pub unit: String,
    /// the raw integers, or volts
    pub values: Vec<f64>,
    pub acquisition: Map<String, Value>,
}

impl Analog {
    /// The samples in volts, or None without a conversion.
    pub fn volts(&self) -> Option<Vec<f64>> {
        if self.encoding == "analog-f32" {
            return Some(self.values.clone());
        }
        let (z, s) = (self.zero?, self.scale_nv?);
        Some(self.values.iter().map(|v| (v - z) * s * 1e-9).collect())
    }
}

impl Events {
    /// Quanta holding more than one change.
    pub fn hidden(&self) -> usize {
        self.counts.iter().filter(|&&c| c > 1).count()
    }
}

/// A part (or a run of parts read as one): its ticks, where it starts, its lines, what it left out.
#[derive(Debug, Clone)]
pub struct Part {
    /// its number (of a run: its first part's)
    pub part: usize,
    /// a run of parts joined: its first and last part
    pub parts: Option<(usize, usize)>,
    pub ticks: i64,
    pub tick0: Option<i64>,
    /// the capture's and the part's meta together
    pub meta: Map<String, Value>,
    pub lines: Vec<Line>,
    /// the lines kept as changes at a step of 2 or more (also in `lines` where no quantum hides a change); none in
    /// a run of parts joined
    pub events: Vec<Events>,
    /// the analog channels (also named in `skipped`, as channels not read as logic)
    pub analog: Vec<Analog>,
    /// channels not read as logic: [{"name", "encoding"}] (analog, an unknown encoding, events hiding changes)
    pub skipped: Vec<Value>,
}

/// What kind of file it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    WireSkein,
    Sr,
    Vcd,
}

/// A capture: its tick (exact, numerator and denominator), whether it is complete, its parts.
#[derive(Debug, Clone)]
pub struct Capture {
    pub kind: Kind,
    pub tick_hz: (i64, i64),
    pub id: Option<String>,
    pub complete: bool,
    pub lost: Vec<Value>,
    pub layout: String,
    /// the parts in the file
    pub n_parts: usize,
    pub meta: Map<String, Value>,
    /// the parts as read (runs of parts joined)
    pub parts: Vec<Part>,
}

impl Capture {
    /// Ticks a second.
    pub fn rate(&self) -> f64 {
        self.tick_hz.0 as f64 / self.tick_hz.1 as f64
    }
}

/// What a file is, by its content (§2.3).
pub fn sniff(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"PK\x03\x04") {
        let names = wsk::names(bytes).ok()?;
        if names.iter().any(|n| n == wsk::IDENT) {
            let id = wsk::entry(bytes, wsk::IDENT).ok()?;
            let v: Value = serde_json::from_slice(&id).ok()?;
            if v["format"].as_str().is_some_and(|f| f.starts_with("wireskein/")) {
                return Some(Kind::WireSkein);
            }
        }
        return (names.iter().any(|n| n == "version") && names.iter().any(|n| n == "metadata")).then_some(Kind::Sr);
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(4096)]);
    let t = head.trim_start();
    ["$date", "$version", "$timescale", "$comment", "$scope", "$var"]
        .iter()
        .any(|k| t.starts_with(k) && !t[k.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        .then_some(Kind::Vcd)
}

/// A capture file's bytes read.
pub fn read_bytes(bytes: &[u8]) -> Result<Capture, String> {
    match sniff(bytes) {
        Some(Kind::WireSkein) => wsk::read(bytes),
        Some(Kind::Sr) => sr::read(bytes),
        Some(Kind::Vcd) => vcd::read(&String::from_utf8_lossy(bytes)),
        None => Err("not a WireSkein file, a sigrok session (.sr) or a VCD".into()),
    }
}

/// What a file on disk is, by its content (its zip directory read, not the whole file).
pub fn sniff_path(path: &Path) -> Option<Kind> {
    use std::io::Read;
    let mut head = vec![0u8; 4096];
    let mut f = std::fs::File::open(path).ok()?;
    let k = f.read(&mut head).ok()?;
    head.truncate(k);
    if !head.starts_with(b"PK\x03\x04") {
        return sniff(&head);
    }
    let mut z = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let names: Vec<String> = z.file_names().map(String::from).collect();
    if names.iter().any(|n| n == wsk::IDENT) {
        let mut s = String::new();
        z.by_name(wsk::IDENT).ok()?.read_to_string(&mut s).ok()?;
        let v: Value = serde_json::from_str(&s).ok()?;
        if v["format"].as_str().is_some_and(|f| f.starts_with("wireskein/")) {
            return Some(Kind::WireSkein);
        }
    }
    (names.iter().any(|n| n == "version") && names.iter().any(|n| n == "metadata")).then_some(Kind::Sr)
}

/// A capture file read (a zip from the disk as its entries are needed, not loaded whole).
pub fn read(path: &Path) -> Result<Capture, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let file = || std::fs::File::open(path).map_err(|e| at(e.to_string()));
    match sniff_path(path) {
        Some(Kind::WireSkein) => wsk::read_from(file()?).map_err(at),
        Some(Kind::Sr) => sr::read_from(file()?).map_err(at),
        Some(Kind::Vcd) => {
            let t = std::fs::read(path).map_err(|e| at(e.to_string()))?;
            vcd::read(&String::from_utf8_lossy(&t)).map_err(at)
        }
        None => Err(at(
            "not a WireSkein file, a sigrok session (.sr) or a VCD (another format: convert it to a WireSkein file \
             with wireskein.capture)"
                .into(),
        )),
    }
}

/// A capture file read with a WireSkein file's parts each on its own (none joined: Python's `fileio.load_parts`), or
/// only part `only` of it; a .sr or a VCD (one part) as `read` reads it.
pub fn read_parts(path: &Path, only: Option<usize>) -> Result<Capture, String> {
    if sniff_path(path) != Some(Kind::WireSkein) {
        return read(path);
    }
    let at = |e: String| format!("{}: {e}", path.display());
    let f = std::fs::File::open(path).map_err(|e| at(e.to_string()))?;
    wsk::read_parts_from(f, only).map_err(at)
}

/// (level at the first sample, the ticks of the changes) of samples at ticks phase + k * step.
pub fn dense_edges(samples: impl IntoIterator<Item = u8>, step: i64, phase: i64) -> (u8, Vec<i64>) {
    let mut it = samples.into_iter();
    let Some(first) = it.next() else {
        return (0, vec![]);
    };
    let (mut last, mut out) = (first, vec![]);
    for (k, v) in it.enumerate() {
        if v != last {
            out.push(phase + (k as i64 + 1) * step);
            last = v;
        }
    }
    (first, out)
}

/// `dense_edges` of n samples packed a bit each (sample k at bit k % 8 of byte k / 8; bytes past the data read as
/// 0), a byte the line holds its level through passed over whole (no sample list made).
pub fn packed_edges(data: &[u8], n: usize, step: i64, phase: i64) -> (u8, Vec<i64>) {
    if n == 0 {
        return (0, vec![]);
    }
    let byte = |b: usize| data.get(b).copied().unwrap_or(0);
    let first = byte(0) & 1;
    let (mut level, mut out) = (first, vec![]);
    for b in 0..n.div_ceil(8) {
        let v = byte(b);
        let k0 = b * 8;
        if k0 + 8 <= n && v == if level == 1 { 0xFF } else { 0 } {
            continue;
        }
        for i in 0..8.min(n - k0) {
            let s = (v >> i) & 1;
            if s != level {
                out.push(phase + (k0 + i) as i64 * step);
                level = s;
            }
        }
    }
    (first, out)
}

/// A fraction reduced.
pub fn fraction(n: i64, d: i64) -> (i64, i64) {
    fn gcd(a: i64, b: i64) -> i64 {
        if b == 0 { a.abs() } else { gcd(b, a % b) }
    }
    let g = gcd(n, d).max(1);
    let s = if d < 0 { -1 } else { 1 };
    (s * n / g, s * d / g)
}

/// A float as a fraction of denominator at most 10^9 (Python's `Fraction(x).limit_denominator(10**9)`, on the
/// float's exact value).
pub fn limit_denominator(x: f64) -> (i64, i64) {
    const MAX: i128 = 1_000_000_000;
    // the float's exact value: mantissa * 2^exp
    let bits = x.to_bits();
    let (sign, exp, man) = (
        (bits >> 63) as i128,
        ((bits >> 52) & 0x7FF) as i32,
        (bits & ((1 << 52) - 1)) as i128,
    );
    let (man, exp) = if exp == 0 {
        (man, -1074)
    } else {
        (man | (1 << 52), exp - 1075)
    };
    let s = if sign == 1 { -1 } else { 1 };
    if exp >= 0 {
        return ((s * (man << exp.min(10))) as i64, 1);
    }
    if exp < -120 {
        return (0, 1);
    }
    let (mut n, mut d) = (man, 1i128 << (-exp));
    let g = {
        let (mut a, mut b) = (n, d);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        a
    };
    (n, d) = (n / g, d / g);
    if d <= MAX {
        return ((s * n) as i64, d as i64);
    }
    let den = d;
    let (mut p0, mut q0, mut p1, mut q1) = (0i128, 1i128, 1i128, 0i128);
    loop {
        let a = n / d;
        let q2 = q0 + a * q1;
        if q2 > MAX {
            break;
        }
        (p0, q0, p1, q1) = (p1, q1, p0 + a * p1, q2);
        (n, d) = (d, n - a * d);
    }
    let k = (MAX - q0) / q1;
    let (pn, pd) = if 2 * d * (q0 + k * q1) <= den {
        (p1, q1)
    } else {
        (p0 + k * p1, q0 + k * q1)
    };
    fraction((s * pn) as i64, pd as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_and_fractions() {
        assert_eq!(dense_edges([1, 1, 0, 0, 1], 2, 3), (1, vec![7, 11]));
        // packed: the same as the samples unpacked, over runs of whole bytes, a short last byte, missing bytes
        let mut x = 12345u64;
        let data: Vec<u8> = (0..400)
            .map(|i| {
                x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                if i % 7 < 3 { [0u8, 0xFF][i % 2] } else { (x >> 33) as u8 }
            })
            .collect();
        for n in [0, 1, 7, 8, 9, 100, 3199, 3200, 3205] {
            let unpacked = (0..n).map(|k| (data.get(k / 8).copied().unwrap_or(0) >> (k % 8)) & 1);
            assert_eq!(packed_edges(&data, n, 3, 2), dense_edges(unpacked, 3, 2), "n {n}");
        }
        assert_eq!(fraction(200_000_000, 2), (100_000_000, 1));
        assert_eq!(limit_denominator(1e8), (100_000_000, 1));
        // as Python's Fraction(x).limit_denominator(10**9)
        assert_eq!(limit_denominator(160e6 / 3.0), (7158278826666667, 134217728));
        assert_eq!(
            (limit_denominator(0.1), limit_denominator(1.0 / 3.0 * 1e-3)),
            ((1, 10), (1, 3000))
        );
        assert_eq!(sniff(b"$timescale 1 ns $end"), Some(Kind::Vcd));
        assert_eq!(sniff(b"hello"), None);
    }
}
