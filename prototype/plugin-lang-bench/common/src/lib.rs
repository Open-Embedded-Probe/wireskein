//! Shared by every engine binary: load the capture (mmap'd dense u16 samples,
//! per-channel u32 edges), the Rust kernels that prepare a plugin's input, and
//! result reporting. Each engine binary runs the same tasks:
//!   T1 hand the whole sample buffer to the script (zero-copy if possible)
//!   T2 the script scans raw samples (edges of one channel) -- a hot loop
//!   T3 I2C plugin: bits/t/bounds (from Rust kernels) -> transactions
//!   T4 call overhead host <-> script
//!   T5 a deliberate bug: what the error message looks like

use std::fs::File;
use std::sync::Arc;
use std::time::Instant;

use memmap2::{Mmap, MmapOptions};

pub struct Capture {
    pub samples: Arc<Mmap>,
    pub n: usize,
    pub edges: Vec<Arc<[u32]>>,
    pub initial: Vec<u8>,
    pub scl: usize,
    pub sda: usize,
    pub id: String,
}

impl Capture {
    pub fn samples(&self) -> &[u16] {
        unsafe { std::slice::from_raw_parts(self.samples.as_ptr() as *const u16, self.n) }
    }
    pub fn busiest(&self) -> usize {
        (0..self.edges.len()).max_by_key(|&k| self.edges[k].len()).unwrap()
    }
}

fn json_array(raw: &str, key: &str) -> String {
    let i = raw.find(&format!("\"{key}\":")).unwrap() + key.len() + 3;
    let a = raw[i..].find('[').unwrap() + i + 1;
    let b = raw[a..].find(']').unwrap() + a;
    raw[a..b].to_string()
}

fn role(raw: &str, name: &str) -> Option<usize> {
    let pat = format!("\"{name}\": \"D");
    let i = raw.find(&pat)? + pat.len();
    raw[i..].split('"').next()?.parse().ok()
}

pub fn load(dir: &str, id: &str) -> Capture {
    let raw = std::fs::read_to_string(format!("{dir}/{id}.json")).unwrap();
    let channels = json_array(&raw, "channels").split(',').count();
    let initial = json_array(&raw, "initial").split(',').map(|s| s.trim().parse().unwrap()).collect();
    let f = File::open(format!("{dir}/{id}.u16")).unwrap();
    let m = unsafe { MmapOptions::new().map_copy(&f).unwrap() }.make_read_only().unwrap();
    let mut edges = vec![];
    for k in 0..channels {
        let b = std::fs::read(format!("{dir}/{id}.ch{k}.u32")).unwrap();
        let v: Vec<u32> = b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        edges.push(Arc::from(v.into_boxed_slice()));
    }
    let (scl, sda) = match (role(&raw, "scl"), role(&raw, "sda")) {
        (Some(a), Some(b)) => (a, b),
        _ => (role(&raw, "clk").unwrap_or(0), role(&raw, "dio").unwrap_or(1)),
    };
    let n = m.len() / 2;
    Capture { samples: Arc::new(m), n, edges, initial, scl, sda, id: id.to_string() }
}

/// Levels of `data` sampled at the rising edges of `clk`.
pub fn sync_bits(cap: &Capture, clk: usize, data: usize) -> (Vec<u32>, Vec<u8>) {
    let s = cap.samples();
    let init = cap.initial[clk];
    let (mut t, mut bits) = (vec![], vec![]);
    for (i, &e) in cap.edges[clk].iter().enumerate() {
        if init ^ (((i + 1) & 1) as u8) == 1 {
            t.push(e);
            bits.push(((s[e as usize] >> data) & 1) as u8);
        }
    }
    (t, bits)
}

/// START/STOP frames: data edges while the clock is high; bounds as index pairs into t.
pub fn frames_startstop(cap: &Capture, clk: usize, data: usize, t: &[u32]) -> Vec<u32> {
    let s = cap.samples();
    let init = cap.initial[data];
    let (mut bounds, mut open) = (vec![], None::<u32>);
    for (i, &e) in cap.edges[data].iter().enumerate() {
        if (s[e as usize] >> clk) & 1 == 0 {
            continue;
        }
        let after = init ^ (((i + 1) & 1) as u8);
        let p = t.partition_point(|&x| x < e) as u32;
        if let Some(o) = open {
            bounds.extend_from_slice(&[o, p]);
        }
        open = if after == 0 { Some(p) } else { None };
    }
    bounds
}

pub struct I2cInput {
    pub t: Vec<u32>,
    pub bits: Vec<u8>,
    pub bounds: Vec<u32>,
}

pub fn i2c_input(cap: &Capture) -> I2cInput {
    let (t, bits) = sync_bits(cap, cap.scl, cap.sda);
    let bounds = frames_startstop(cap, cap.scl, cap.sda, &t);
    I2cInput { t, bits, bounds }
}

/// Reference result of T2 (edge count of a channel over all samples).
pub fn scan_ref(cap: &Capture, ch: usize, limit: usize) -> u32 {
    let s = &cap.samples()[..limit.min(cap.n)];
    let mut c = 0;
    let mut p = s[0] >> ch & 1;
    for &v in &s[1..] {
        let v = v >> ch & 1;
        if v != p {
            c += 1;
            p = v;
        }
    }
    c
}

/// Reference result of T3: (transactions, data bytes, xor of all 9-bit words).
pub fn i2c_ref(inp: &I2cInput) -> (u64, u64, u64) {
    let (mut tx, mut nb, mut x) = (0u64, 0u64, 0u64);
    for f in inp.bounds.chunks_exact(2) {
        let (a, b) = (f[0] as usize, f[1] as usize);
        let nw = (b - a) / 9;
        if nw == 0 {
            continue;
        }
        tx += 1;
        nb += nw as u64 - 1;
        for w in 0..nw {
            let mut v = 0u64;
            for k in 0..9 {
                v = (v << 1) | inp.bits[a + w * 9 + k] as u64;
            }
            x ^= v;
        }
    }
    (tx, nb, x)
}

pub fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

/// Peak resident set size of this process (Linux).
pub fn peak_rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| {
        s.lines().find(|l| l.starts_with("VmHWM:")).and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
    }).unwrap_or(0)
}

pub struct Report {
    pub engine: &'static str,
    pub fields: Vec<(String, String)>,
}

impl Report {
    pub fn new(engine: &'static str, cap: &Capture) -> Report {
        Report { engine, fields: vec![("capture".into(), format!("\"{}\"", cap.id))] }
    }
    pub fn num(&mut self, k: &str, v: f64) {
        self.fields.push((k.into(), format!("{v:.4}")));
    }
    pub fn text(&mut self, k: &str, v: &str) {
        self.fields.push((k.into(), format!("{:?}", v)));
    }
    pub fn print(&self) {
        let body: Vec<String> = self.fields.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
        println!("{{\"engine\":\"{}\",{}}}", self.engine, body.join(","));
    }
}
