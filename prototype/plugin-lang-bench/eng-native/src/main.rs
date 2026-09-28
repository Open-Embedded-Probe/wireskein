//! Baseline: the same tasks written directly in Rust (what a compiled-in plugin costs).
use std::time::Instant;

use common::*;

struct Tx {
    _addr: u8,
    _rw: bool,
    _ack: bool,
    _bytes: Vec<u8>,
    _start: u32,
    _end: u32,
}

fn i2c(bits: &[u8], t: &[u32], bounds: &[u32]) -> (Vec<Tx>, u64, u64, u64) {
    let (mut recs, mut tx, mut nb, mut x) = (vec![], 0u64, 0u64, 0u64);
    for f in bounds.chunks_exact(2) {
        let (a, b) = (f[0] as usize, f[1] as usize);
        let nw = (b - a) / 9;
        if nw == 0 {
            continue;
        }
        let mut bytes = Vec::with_capacity(nw);
        let mut first_ack = false;
        for w in 0..nw {
            let mut v = 0u32;
            for k in 0..9 {
                v = (v << 1) | bits[a + w * 9 + k] as u32;
            }
            x ^= v as u64;
            if w == 0 {
                first_ack = v & 1 == 0;
            }
            bytes.push((v >> 1) as u8);
        }
        let first = bytes.remove(0);
        tx += 1;
        nb += nw as u64 - 1;
        recs.push(Tx { _addr: first >> 1, _rw: first & 1 == 1, _ack: first_ack, _bytes: bytes, _start: t[a], _end: t[b - 1] });
    }
    (recs, tx, nb, x)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cap = load(&a[1], &a[2]);
    let mut r = Report::new("native", &cap);
    r.text("t1_method", "slice (in process)");
    r.num("t1_zero_copy", 1.0);
    r.num("t1_ms", 0.0);
    let ch = cap.busiest();
    let n = cap.n.min(2_000_000);
    let t = Instant::now();
    let c = scan_ref(&cap, ch, n);
    r.num("t2_samples", n as f64);
    r.num("t2_ms", ms(t));
    r.num("t2_ok", (c == scan_ref(&cap, ch, n)) as u8 as f64);
    let inp = i2c_input(&cap);
    r.num("t3_bits", inp.bits.len() as f64);
    r.num("t3_handoff_ms", 0.0);
    let t = Instant::now();
    let (recs, tx, nb, x) = i2c(&inp.bits, &inp.t, &inp.bounds);
    r.num("t3_ms", ms(t));
    r.num("t3_records", recs.len() as f64);
    r.num("t3_ok", ((tx, nb, x) == i2c_ref(&inp)) as u8 as f64);
    r.num("t4_host_call_us", 0.0);
    r.num("t4_script_call_us", 0.0);
    r.text("t5_error", "(compile-time)");
    r.num("peak_rss_kb", peak_rss_kb() as f64);
    r.print();
}
