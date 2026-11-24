//! What the readers of one line told by its pulses' widths (RGB LED, 1-Wire, IR NEC, DHT) share (Python's
//! `_engine/pulsecode.py` and `analyzers/pulse.py`): a line's whole pulses, the split between two kinds of width,
//! bits to bytes, the views of a line those readers take.

use crate::kernels::{debounce, deglitch};
use crate::view::Channel;

/// (start, width) of every whole pulse at `active` (one cut by the window's ends left out).
pub fn pulses(ch: &Channel, active: u8) -> (Vec<i64>, Vec<i64>) {
    let e = ch.edges;
    let (mut st, mut w) = (vec![], vec![]);
    for i in 0..e.len() {
        let after = ch.initial ^ (((i + 1) & 1) as u8);
        if after == active && i + 1 < e.len() {
            st.push(e[i]);
            w.push(e[i + 1] - e[i]);
        }
    }
    (st, w)
}

/// The width between two kinds of pulse: the largest ratio between neighbouring widths (sorted, distinct), if at
/// least `ratio`.
pub fn split(widths: &[i64], ratio: f64) -> Option<f64> {
    let mut u = widths.to_vec();
    u.sort_unstable();
    u.dedup();
    if u.len() < 2 {
        return None;
    }
    let (mut k, mut best) = (0usize, f64::NEG_INFINITY);
    for i in 0..u.len() - 1 {
        let r = u[i + 1] as f64 / u[i].max(1) as f64;
        if r > best {
            (k, best) = (i, r);
        }
    }
    (best >= ratio).then(|| ((u[k] * u[k + 1]) as f64).sqrt())
}

/// Bits to whole bytes, and the bits left over.
pub fn to_bytes(bits: &[u8], lsb_first: bool) -> (Vec<u8>, usize) {
    let out = bits
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| {
            b.iter()
                .enumerate()
                .fold(0u8, |a, (k, &v)| a | (v << if lsb_first { k } else { 7 - k }))
        })
        .collect();
    (out, bits.len() % 8)
}

/// 1 - exp(-n / scale).
pub fn q(n: f64, scale: f64) -> f64 {
    1.0 - (-n / scale).exp()
}

/// 1 when no value is over half of them, down to 0.05 when one value is all of them.
pub fn degenerate(values: &[u8]) -> f64 {
    if values.len() < 4 {
        return 1.0;
    }
    let mut c = [0usize; 256];
    for &v in values {
        c[v as usize] += 1;
    }
    let top = *c.iter().max().unwrap() as f64 / values.len() as f64;
    if top <= 0.5 {
        1.0
    } else {
        (1.0 - (top - 0.5) / 0.5).max(0.05)
    }
}

pub const BOUNCE_TICKS: i64 = 4; // a bounce: pulses under this many ticks, within as many of an edge
pub const BOUNCE_PAIRED: f64 = 0.5; // this share of the short runs side by side: a bounce's (a gap, the pulse back)

/// Whether a line's short runs (under `short`) are a bounce's: BOUNCE_PAIRED of them side by side. False with none.
pub fn bounces_paired(runs: &[i64], short: i64) -> bool {
    let s: Vec<bool> = runs.iter().map(|&r| r < short).collect();
    let n = s.iter().filter(|&&x| x).count();
    let pairs = s.windows(2).filter(|w| w[0] && w[1]).count();
    n > 0 && (pairs * 2) as f64 >= BOUNCE_PAIRED * n as f64
}

/// A line's views for these readers: as it is, with pulses shorter than `k` taken out (k from the protocol's
/// shortest pulse), and, where its short runs come in pairs (bounces), with each edge's bounces merged at its first
/// change: (changes, deglitch; -4 for bounces merged).
pub fn views(e: &[i64], k: i64, bounces: bool) -> Vec<(Vec<i64>, i64)> {
    let mut out = vec![(e.to_vec(), 0)];
    let d = deglitch(e, k);
    if d.len() != e.len() {
        out.push((d, k));
    }
    if bounces && e.len() >= 16 {
        let r: Vec<i64> = e.windows(2).map(|w| w[1] - w[0]).collect();
        if bounces_paired(&r, BOUNCE_TICKS) {
            let b = debounce(e, BOUNCE_TICKS + 1);
            if b.len() != e.len() {
                out.push((b, -BOUNCE_TICKS));
            }
        }
    }
    out
}

/// The least k of ticks above a duration (Python's `max(2, ceil(s * rate - 1e-9))`).
pub fn ticks_k(s: f64, rate: f64) -> i64 {
    ((s * rate - 1e-9).ceil() as i64).max(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_and_bytes() {
        let e = [10, 13, 20, 30];
        let (st, w) = pulses(&Channel::new("A", 0, &e), 1);
        assert_eq!((st, w), (vec![10, 20], vec![3, 10]));
        assert_eq!(split(&[3, 3, 7, 8], 1.3), Some((21f64).sqrt()));
        assert_eq!(to_bytes(&[1, 0, 0, 0, 0, 0, 0, 0, 1], true), (vec![1], 1));
        assert_eq!(degenerate(&[0, 0, 0, 0]), 0.05);
        assert!(bounces_paired(&[100, 1, 2, 100, 1, 1, 100], 4));
    }
}
