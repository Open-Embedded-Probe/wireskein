//! How the core's analysis grows with a capture's size, natively and as WASM (bench/notes/2026-11-24-rust-plan.ja.md
//! §6): two UART lines of text made here (8N1, 17 samples a bit, a gap now and then), read at each size; one JSON line
//! a size: the changes, the seconds, the memory (the peak resident set natively, the linear memory as WASM).
//!
//!     cargo run --release --example scale -- 10000 100000 1000000
//!     cargo build --release --example scale --target wasm32-wasip1   (run with node's WASI: bench/rust_wasm_scale.mjs)

use std::time::Instant;
use wireskein_core::{Channel, View, analyze};

fn uart(n_chars: usize, bit: i64, seed: u64) -> (Vec<i64>, i64) {
    let text = b"The quick brown fox jumps over the lazy dog. 0123456789 WireSkein\r\n";
    let (mut e, mut lv, mut t, mut x) = (Vec::new(), 1u8, 100i64, seed);
    for i in 0..n_chars {
        let b = text[(i + seed as usize) % text.len()];
        for k in std::iter::once(0).chain((0..8).map(|k| (b >> k) & 1)).chain([1]) {
            if k != lv {
                e.push(t);
                lv = k;
            }
            t += bit;
        }
        x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        t += bit * ((x >> 60) as i64 % 3);
    }
    (e, t + 20 * bit)
}

fn memory() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) * 65536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("VmHWM:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<usize>().ok())
            })
            .map_or(0, |kb| kb * 1024)
    }
}

fn main() {
    let sizes: Vec<usize> = std::env::args().skip(1).filter_map(|a| a.parse().ok()).collect();
    for n in sizes {
        let (a, na) = uart(n, 17, 1);
        let (b, nb) = uart(n, 23, 2);
        let len = na.max(nb);
        let v = View::new(2e6, len, vec![Channel::new("A", 1, &a), Channel::new("B", 1, &b)]).unwrap();
        let t0 = Instant::now();
        let claims = analyze(&v, &serde_json::Value::Null);
        let s = t0.elapsed().as_secs_f64();
        let ok = claims.iter().filter(|c| c.verdict == "confirmed").count();
        println!(
            "{{\"chars\": {n}, \"changes\": {}, \"samples\": {len}, \"seconds\": {s:.4}, \"memory\": {}, \"confirmed\": {ok}}}",
            a.len() + b.len(),
            memory()
        );
    }
}
