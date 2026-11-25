//! Made-up captures for measuring.
//!
//! - `SynthEdges`: a line whose changes are made on the fly from a hash (none kept in memory): segments of the time
//!   axis each quiet or busy at a period of its own, so a view shows bursts, gaps and dense stretches. 1e9 changes
//!   take no memory but their summary.
//! - `fake_track`: items over a time span (a stand-in for decoded transactions where no decoder ran).
//! - `I2cGen` / `Recorder`: an I2C bus (with a UART beside it) as changes, to write a file the real engine reads.

use crate::Tick;
use crate::ann::{Item, Status, Track};
use crate::digital::EdgeSource;

#[inline]
pub fn mix(mut x: u64) -> u64 {
    // splitmix64
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// A line of made-up changes.
pub struct SynthEdges {
    pub line: u64,
    pub n_ticks: Tick,
    /// the base period of a change (each segment runs at 1x to 4x it)
    pub period: Tick,
    /// the segments' length in ticks
    pub seg_len: Tick,
}

impl SynthEdges {
    fn seg(&self, s: i64) -> Option<Tick> {
        let h = mix(self.line.wrapping_mul(0x1_0000_0001) ^ s as u64);
        // a fifth to a half of the segments quiet, by line
        if h % (2 + self.line % 4) == 0 {
            return None;
        }
        Some(self.period * (1 + ((h >> 8) % 4) as i64))
    }
    #[inline]
    fn edge(&self, s: i64, p: Tick, j: i64) -> Tick {
        let o = (mix(self.line ^ ((s as u64) << 20) ^ j as u64) % (p as u64 / 2).max(1)) as Tick;
        s * self.seg_len + j * p + o
    }
    /// Changes a tick, on average.
    pub fn density(&self) -> f64 {
        let quiet = 1.0 / (2 + self.line % 4) as f64;
        (1.0 - quiet) / (self.period as f64 * 2.5)
    }
}

impl EdgeSource for SynthEdges {
    fn n_ticks(&self) -> Tick {
        self.n_ticks
    }
    fn initial(&self) -> u8 {
        0
    }
    fn edges_in(&self, t0: Tick, t1: Tick, cap: usize, out: &mut Vec<Tick>) -> bool {
        let t0 = t0.max(0);
        let t1 = t1.min(self.n_ticks);
        if t1 <= t0 {
            return true;
        }
        let mut pushed = 0usize;
        let mut s = t0 / self.seg_len;
        while s * self.seg_len < t1 {
            if let Some(p) = self.seg(s) {
                let base = s * self.seg_len;
                let n_in = self.seg_len / p;
                let mut j = ((t0 - base) / p - 1).max(0);
                while j < n_in {
                    let t = self.edge(s, p, j);
                    if t >= t1 {
                        return true;
                    }
                    if t >= t0 {
                        if pushed == cap {
                            return false;
                        }
                        out.push(t);
                        pushed += 1;
                    }
                    j += 1;
                }
            }
            s += 1;
        }
        true
    }
    fn approx_edges(&self) -> u64 {
        (self.n_ticks as f64 * self.density()) as u64
    }
    fn mem_bytes(&self) -> usize {
        0
    }
}

/// `n_lines` made-up lines with about `total` changes together, at 1 GHz ticks.
pub fn synth_lines(total: u64, n_lines: usize) -> (Tick, Vec<SynthEdges>) {
    let periods: Vec<Tick> = (0..n_lines).map(|i| 20 << (i % 5)).collect();
    let mk = |n: Tick| -> Vec<SynthEdges> {
        periods
            .iter()
            .enumerate()
            .map(|(i, &p)| SynthEdges { line: i as u64, n_ticks: n, period: p, seg_len: p * 4096 })
            .collect()
    };
    let probe = mk(1 << 30);
    let per_tick: f64 = probe.iter().map(|l| l.density()).sum();
    let n = ((total as f64) / per_tick).ceil() as Tick;
    (n, mk(n))
}

/// `n` items evenly over [0, n_ticks) (60 % of their slot each), texts like a bus's transactions; a few NACKs and
/// errors.
pub fn fake_track(name: &str, n: usize, n_ticks: Tick) -> Track {
    let slot = (n_ticks / n.max(1) as Tick).max(2);
    let mut items = Vec::with_capacity(n);
    for i in 0..n {
        let h = mix(i as u64 ^ 0xA5A5);
        let s = i as Tick * slot + (h % (slot as u64 / 4).max(1)) as Tick;
        let e = s + slot * 6 / 10;
        let addr = [0x48u8, 0x50, 0x68, 0x3c][(h >> 8) as usize % 4];
        let rw = if (h >> 12) & 1 == 0 { 'W' } else { 'R' };
        let nb = 1 + (h >> 16) as usize % 4;
        let bytes: Vec<String> = (0..nb).map(|k| format!("{:02x}", (h >> (20 + 8 * k)) as u8)).collect();
        let status = match (h >> 56) % 200 {
            0 => Status::Error,
            1..=4 => Status::Note,
            5 => Status::Warn,
            _ => Status::Ok,
        };
        let full = format!("{rw} 0x{addr:02x} {}{}", bytes.join(" "), if status == Status::Note { " NACK" } else { "" });
        let short = format!("{rw}{addr:02x}");
        items.push((Item { s, e, status, id: i as u32 }, full, short));
    }
    Track::new(name, items)
}

/// A made-up analog channel: a slow sine with a fast ripple and noise, `n` samples, `dt` ticks a sample.
pub fn synth_analog(n: usize, dt: f64) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let t = i as f32;
            let noise = (mix(i as u64) % 1000) as f32 / 1000.0 - 0.5;
            1.65 + 1.2 * (t * 2e-5).sin() + 0.2 * (t * 3e-3).sin() + 0.05 * noise + 0.0 * dt as f32
        })
        .collect()
}

/// Lines' changes recorded from levels set in time order.
pub struct Recorder {
    pub level: Vec<u8>,
    pub initial: Vec<u8>,
    pub edges: Vec<Vec<Tick>>,
}

impl Recorder {
    pub fn new(initial: &[u8]) -> Self {
        Recorder { level: initial.to_vec(), initial: initial.to_vec(), edges: vec![vec![]; initial.len()] }
    }
    pub fn set(&mut self, line: usize, t: Tick, v: u8) {
        if self.level[line] != v {
            self.level[line] = v;
            let e = &mut self.edges[line];
            // two changes at one tick cancel
            if e.last() == Some(&t) {
                e.pop();
            } else {
                e.push(t);
            }
        }
    }
}

/// An I2C bus's controller and targets at `bit` ticks a bit: transactions of 1-4 bytes to a few addresses, a few
/// NACKed.
pub struct I2cGen {
    pub bit: Tick,
    pub scl: usize,
    pub sda: usize,
    seed: u64,
}

impl I2cGen {
    pub fn new(bit: Tick, scl: usize, sda: usize, seed: u64) -> Self {
        I2cGen { bit, scl, sda, seed }
    }
    fn byte(&self, r: &mut Recorder, t: &mut Tick, v: u8, ack: u8) {
        let b = self.bit;
        for k in (0..8).rev() {
            r.set(self.sda, *t + b / 4, (v >> k) & 1);
            r.set(self.scl, *t + b / 2, 1);
            r.set(self.scl, *t + b, 0);
            *t += b;
        }
        r.set(self.sda, *t + b / 4, ack);
        r.set(self.scl, *t + b / 2, 1);
        r.set(self.scl, *t + b, 0);
        *t += b;
    }
    /// One transaction from tick t; returns the tick after its STOP.
    pub fn transaction(&mut self, r: &mut Recorder, mut t: Tick, i: u64) -> Tick {
        let h = mix(self.seed ^ i);
        let b = self.bit;
        let addr = [0x48u8, 0x50, 0x68, 0x3c, 0x13][(h % 5) as usize];
        let read = (h >> 8) & 1 == 1;
        let nack = (h >> 16) % 50 == 0;
        let n = 1 + ((h >> 24) % 4) as usize;
        // START
        r.set(self.sda, t, 0);
        r.set(self.scl, t + b / 2, 0);
        t += b / 2;
        self.byte(r, &mut t, (addr << 1) | read as u8, nack as u8);
        if !nack {
            for k in 0..n {
                let v = (h >> (32 + 8 * (k % 4))) as u8;
                let last = k + 1 == n;
                self.byte(r, &mut t, v, (read && last) as u8);
            }
        }
        // STOP
        r.set(self.sda, t + b / 4, 0);
        r.set(self.scl, t + b / 2, 1);
        r.set(self.sda, t + 3 * b / 4, 1);
        t + b
    }
}

/// A UART's 8N1 characters at `bit` ticks a bit, from tick t; returns the tick after the last stop bit.
pub fn uart_bytes(r: &mut Recorder, line: usize, mut t: Tick, bit: f64, data: &[u8]) -> Tick {
    for &c in data {
        let at = |k: usize| t + (k as f64 * bit).round() as Tick;
        r.set(line, at(0), 0);
        for k in 0..8 {
            r.set(line, at(1 + k), (c >> k) & 1);
        }
        r.set(line, at(9), 1);
        t = at(10);
    }
    t
}
