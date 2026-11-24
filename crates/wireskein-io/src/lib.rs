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

pub mod sr;
pub mod vcd;
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

/// A capture file read.
pub fn read(path: &Path) -> Result<Capture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    read_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))
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
