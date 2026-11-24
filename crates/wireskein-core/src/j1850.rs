//! SAE J1850 VPW (10.4 kb/s; Python's `_engine/j1850.py`): one line, passive low, active high; SOF (active, 200 us),
//! then symbols of 64 us (short) or 128 us (long) at alternating levels: a passive short or an active long is 0, a
//! passive long or an active short is 1; bytes MSB first, the last a CRC8 (x^8 + x^4 + x^3 + x^2 + 1, preset 0xFF,
//! inverted). Short and long are told apart for each level by its own two lengths (a comparator's offset).

use crate::Candidate;
use crate::kernels::{median, runs};
use crate::pulse::{best_of_views, candidate, evidence, hex, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const GLITCH_S: f64 = 10e-6;
const SOF_S: (f64, f64) = (163e-6, 240e-6);
const SYMBOL_S: (f64, f64) = (34e-6, 163e-6);

pub fn crc8(data: &[u8]) -> u8 {
    let mut c = 0xFFu8;
    for &x in data {
        c ^= x;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x1D } else { c << 1 };
        }
    }
    c ^ 0xFF
}

/// The line's runs (seconds, level) with glitches shorter than GLITCH_S merged into their neighbours.
fn line_runs(ch: &Channel, n: i64, rate: f64) -> (Vec<f64>, Vec<u8>) {
    let (_, length, level) = runs(ch, n);
    let (mut t, mut lv): (Vec<f64>, Vec<u8>) = (vec![], vec![]);
    for (&l, &v) in length.iter().zip(&level) {
        let d = l as f64 / rate;
        if !t.is_empty() && (d < GLITCH_S || *lv.last().unwrap() == v) {
            *t.last_mut().unwrap() += d;
            continue;
        }
        t.push(d);
        lv.push(v);
    }
    // a glitch merged into the run before it: the run after, of the same level, joins it too
    let (mut ot, mut ov): (Vec<f64>, Vec<u8>) = (vec![], vec![]);
    for (d, v) in t.into_iter().zip(lv) {
        if ov.last() == Some(&v) {
            *ot.last_mut().unwrap() += d;
        } else {
            ot.push(d);
            ov.push(v);
        }
    }
    (ot, ov)
}

/// The length between a level's short and long symbols: the midpoint of their two clusters.
fn split(x: &[f64]) -> f64 {
    let x: Vec<f64> = x
        .iter()
        .copied()
        .filter(|&d| d >= SYMBOL_S.0 && d <= SYMBOL_S.1)
        .collect();
    if x.len() < 4 {
        return 96e-6;
    }
    let m = median(&x);
    let lo: Vec<f64> = x.iter().copied().filter(|&d| d < m).collect();
    let hi: Vec<f64> = x.iter().copied().filter(|&d| d >= m).collect();
    if lo.is_empty() || hi.is_empty() || median(&hi) - median(&lo) < 30e-6 {
        return 96e-6;
    }
    (median(&lo) + median(&hi)) / 2.0
}

/// The frames on a line: {s, e, bytes, ok}.
pub fn frames(ch: &Channel, n: i64, rate: f64) -> Vec<Map<String, Value>> {
    let (d, v) = line_runs(ch, n, rate);
    if d.len() < 20 {
        return vec![];
    }
    let sp: Vec<f64> = (0..2u8)
        .map(|l| {
            let x: Vec<f64> = d.iter().zip(&v).filter(|&(_, &w)| w == l).map(|(&a, _)| a).collect();
            split(&x)
        })
        .collect();
    let mut starts = Vec::with_capacity(d.len());
    let mut acc = 0.0;
    for &x in &d {
        starts.push(acc);
        acc += x;
    }
    let mut out = vec![];
    let mut i = 0usize;
    while i < d.len() {
        if !(v[i] == 1 && d[i] >= SOF_S.0 && d[i] <= SOF_S.1) {
            i += 1;
            continue;
        }
        let s0 = starts[i];
        let (mut bits, mut j) = (vec![], i + 1);
        while j < d.len() && d[j] >= SYMBOL_S.0 && d[j] <= SYMBOL_S.1 {
            let short = d[j] < sp[v[j] as usize];
            bits.push(if v[j] == 1 { short } else { !short } as u8);
            j += 1;
        }
        i = j;
        if bits.len() < 16 || !bits.len().is_multiple_of(8) {
            continue;
        }
        let data: Vec<u8> = bits
            .chunks(8)
            .map(|c| c.iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        out.push(obj(vec![
            ("s", json!((s0 * rate).round_ties_even() as i64)),
            ("e", json!((starts[j.min(d.len() - 1)] * rate).round_ties_even() as i64)),
            ("bytes", json!(hex(&data))),
            ("ok", json!(crc8(&data[..data.len() - 1]) == data[data.len() - 1])),
        ]));
    }
    out
}

fn read(ch: &Channel, n: i64, rate: f64, k: i64) -> Option<Candidate> {
    let fr = frames(ch, n, rate);
    let good = fr.iter().filter(|f| f["ok"] == json!(true)).count();
    if good < 2 {
        return None;
    }
    // each frame's CRC8 and its SOF / symbols of VPW's lengths
    let score = good as f64 / fr.len() as f64 * evidence(10.0 * good as f64);
    let ev = obj(vec![("frames", json!(fr.len())), ("ok", json!(good))]);
    Some(candidate(
        "j1850",
        &[("data", ch.name)],
        score,
        obj(vec![("variant", json!("vpw"))]),
        ev,
        "frames",
        &fr,
        k,
    ))
}

/// The J1850 VPW readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 100e3 {
        return vec![];
    }
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 19)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.n_samples, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_with_their_crc_and_an_offset() {
        let rate = 1e6;
        let (mut e, mut t) = (vec![], 500i64);
        // a comparator's offset: the active level 15 us longer, the passive one 15 us shorter
        let frames: [&[u8]; 3] = [&[0x68, 0x6A, 0xF1, 0x01, 0x00], &[0x48, 0x6B], &[0x11, 0x22, 0x33]];
        for f in frames {
            let mut all = f.to_vec();
            all.push(crc8(f));
            e.push(t);
            t += 200 + 15; // SOF, active
            let mut level = 0u8; // the symbol after SOF is passive
            for &b in &all {
                for q in (0..8).rev() {
                    let bit = (b >> q) & 1;
                    let long = if level == 1 { bit == 0 } else { bit == 1 };
                    e.push(t);
                    t += if long { 128 } else { 64 } + if level == 1 { 15 } else { -15 };
                    level ^= 1;
                }
            }
            // (8 symbols a byte: the last is active, its end the line back passive)
            e.push(t);
            t += 2000; // EOF, passive
        }
        let n = t + 100;
        let v = View::new(rate, n, vec![Channel::new("J", 0, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let f = lay["frames"].as_array().unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[1]["bytes"], format!("486b{:02x}", crc8(&[0x48, 0x6B])));
        assert!(f.iter().all(|x| x["ok"] == json!(true)));
    }
}
