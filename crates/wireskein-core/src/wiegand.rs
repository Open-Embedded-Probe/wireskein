//! Wiegand (an access-control reader's output; Python's `_engine/wiegand.py`): two lines idle high, D0 and D1; each
//! bit a short low pulse on one of them (D1: 1, D0: 0), never both at once, at a steady pace; a frame the pulses
//! between gaps many pulse times long. The 26- and 34-bit formats: an even parity over the first half, an odd one
//! over the second (the 26-bit one's facility and card numbers between).

use crate::Candidate;
use crate::kernels::{median, runs};
use crate::pulse::{bits_hex, candidate, evidence, obj, pair_views};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const GAP: f64 = 10.0; // a gap this many pulse intervals long ends a frame
const MIN_BITS: usize = 8; // bits a frame at least
const MAX_WIDTH: f64 = 0.5; // a pulse at most this share of the interval between pulses
const WIDTH_S: (f64, f64) = (10e-6, 500e-6); // a pulse's width
const SAME: f64 = 0.3; // widths and spacings within this share of their medians ...
const REGULAR: f64 = 0.9; // ... this share of them at least
const MIN_FRAME: usize = 24; // bits a frame at least
const PARITY_FORMATS: [usize; 2] = [26, 34];

struct Frame {
    bits: usize,
    ok: Option<bool>,
    row: Map<String, Value>,
}

/// (start, width) of the line's inner low pulses.
fn pulses(ch: &Channel, n: i64) -> Vec<(i64, i64)> {
    let (st, length, level) = runs(ch, n);
    (1..st.len().saturating_sub(1))
        .filter(|&i| level[i] == 0)
        .map(|i| (st[i], length[i]))
        .collect()
}

fn share_near(x: impl Iterator<Item = f64>, m: f64) -> f64 {
    let (mut a, mut k) = (0usize, 0usize);
    for v in x {
        k += 1;
        a += ((v / m - 1.0).abs() < SAME) as usize;
    }
    a as f64 / k as f64
}

fn msb(bits: &[u8]) -> u64 {
    bits.iter().fold(0u64, |a, &b| (a << 1) | b as u64)
}

/// The frames on D0 / D1 as given, or None where they are no Wiegand pair.
fn read_pair(d0: &Channel, d1: &Channel, n: i64, rate: f64) -> Option<Vec<Frame>> {
    if d0.initial != 1 || d1.initial != 1 {
        return None;
    }
    let (p0, p1) = (pulses(d0, n), pulses(d1, n));
    if p0.len() + p1.len() < MIN_BITS || p0.is_empty() || p1.is_empty() {
        return None;
    }
    let mut p: Vec<(i64, i64, u8)> = p0
        .iter()
        .map(|&(s, w)| (s, w, 0))
        .chain(p1.iter().map(|&(s, w)| (s, w, 1)))
        .collect();
    p.sort_by_key(|x| x.0); // stable: D0's first at a tie
    // never both low: each pulse ends before the next starts
    if p.windows(2).any(|x| x[0].0 + x[0].1 > x[1].0) {
        return None;
    }
    let gaps: Vec<f64> = p.windows(2).map(|x| (x[1].0 - x[0].0) as f64).collect();
    let pace = if gaps.is_empty() { 0.0 } else { median(&gaps) };
    let w: Vec<f64> = p.iter().map(|x| x.1 as f64).collect();
    let wm = median(&w);
    if pace <= 0.0 || wm > MAX_WIDTH * pace {
        return None;
    }
    // a reader's pulses are of one width (tens of microseconds) at one pace
    if wm < WIDTH_S.0 * rate || wm > WIDTH_S.1 * rate || share_near(w.iter().copied(), wm) < REGULAR {
        return None;
    }
    let cut: Vec<usize> = (0..gaps.len()).filter(|&i| gaps[i] > GAP * pace).collect();
    let inner: Vec<f64> = (0..gaps.len()).filter(|i| !cut.contains(i)).map(|i| gaps[i]).collect();
    if !inner.is_empty() && share_near(inner.iter().copied(), pace) < REGULAR {
        return None;
    }
    let mut bounds = vec![0usize];
    bounds.extend(cut.iter().map(|&i| i + 1));
    bounds.push(p.len());
    let mut frames = vec![];
    for z in bounds.windows(2) {
        let (a, b) = (z[0], z[1]);
        let bits: Vec<u8> = p[a..b].iter().map(|x| x.2).collect();
        if bits.len() < MIN_BITS {
            continue;
        }
        let mut row = obj(vec![
            ("s", json!(p[a].0)),
            ("e", json!(p[b - 1].0 + p[b - 1].1)),
            ("bits", json!(bits.len())),
            (
                "data",
                json!(bits.iter().map(|b| if *b == 1 { '1' } else { '0' }).collect::<String>()),
            ),
            ("value", json!(bits_hex(&bits))),
        ]);
        let mut ok = None;
        if PARITY_FORMATS.contains(&bits.len()) {
            let h = bits.len() / 2;
            let even = bits[..h].iter().map(|&x| x as u32).sum::<u32>() % 2 == 0;
            let odd = bits[h..].iter().map(|&x| x as u32).sum::<u32>() % 2 == 1;
            ok = Some(even && odd);
            row.insert("ok".into(), json!(even && odd));
            if bits.len() == 26 {
                row.insert("facility".into(), json!(msb(&bits[1..9])));
                row.insert("card".into(), json!(msb(&bits[9..25])));
            }
        }
        frames.push(Frame {
            bits: bits.len(),
            ok,
            row,
        });
    }
    // a reader sends frames of its one format (26 bits and more)
    if frames.is_empty() || frames.iter().any(|f| f.bits != frames[0].bits) || frames[0].bits < MIN_FRAME {
        return None;
    }
    Some(frames)
}

fn oks(fr: &[Frame]) -> usize {
    fr.iter().filter(|f| f.ok == Some(true)).count()
}

fn read(a: &Channel, b: &Channel, n: i64, rate: f64, k: i64) -> Option<Candidate> {
    let (mut fr, mut d0, mut d1) = if let Some(f) = read_pair(a, b, n, rate) {
        (f, a, b)
    } else {
        (read_pair(b, a, n, rate)?, b, a)
    };
    // which line is D1 is not on the wire save through a format's parity
    if fr.iter().any(|f| PARITY_FORMATS.contains(&f.bits)) {
        let alt = read_pair(d1, d0, n, rate).unwrap_or_default();
        if oks(&alt) > oks(&fr) {
            (fr, d0, d1) = (alt, d1, d0);
        }
    }
    let nbits: usize = fr.iter().map(|f| f.bits).sum();
    let checked = fr.iter().filter(|f| f.ok.is_some()).count();
    let good = oks(&fr);
    // pulses taking turns at a steady pace on two lines, frames of one length again, parities that hold
    let same = fr.len(); // (read_pair keeps frames of one length only)
    let info = nbits.min(64) as f64 / 8.0 + 4.0 * same.saturating_sub(1) as f64 + 2.0 * good as f64;
    let share = if checked > 0 { good as f64 / checked as f64 } else { 0.9 };
    let score = evidence(info) * share;
    let ev = obj(vec![
        ("frames", json!(fr.len())),
        ("bits", json!(nbits)),
        ("checked", json!(checked)),
        ("parity_ok", json!(good)),
    ]);
    let params = obj(vec![("bits", json!(fr[0].bits))]);
    let rows: Vec<Map<String, Value>> = fr.into_iter().map(|f| f.row).collect();
    Some(candidate(
        "wiegand",
        &[("d0", d0.name), ("d1", d1.name)],
        score,
        params,
        ev,
        "frames",
        &rows,
        k,
    ))
}

/// The Wiegand readings of a part: each pair of lines idling high, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let chans: Vec<&Channel> = view
        .channels
        .iter()
        .filter(|c| c.edges.len() >= 16 && c.initial == 1)
        .collect();
    let mut out = vec![];
    for (i, a) in chans.iter().enumerate() {
        for b in &chans[i + 1..] {
            let mut best: Option<Candidate> = None;
            for (ea, eb, k) in pair_views(a, b, view.n_samples) {
                let (ca, cb) = (
                    Channel::new(a.name, a.initial, &ea),
                    Channel::new(b.name, b.initial, &eb),
                );
                if let Some(r) = read(&ca, &cb, view.n_samples, view.rate, k)
                    && best.as_ref().is_none_or(|x| r.score > x.score)
                {
                    best = Some(r);
                }
            }
            out.extend(best);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w26(fac: u32, card: u32) -> Vec<u8> {
        let mid: Vec<u8> = (0..8)
            .rev()
            .map(|q| ((fac >> q) & 1) as u8)
            .chain((0..16).rev().map(|q| ((card >> q) & 1) as u8))
            .collect();
        let even = mid[..12].iter().sum::<u8>() & 1;
        let odd = 1 - (mid[12..].iter().sum::<u8>() & 1);
        std::iter::once(even).chain(mid).chain(std::iter::once(odd)).collect()
    }

    #[test]
    fn cards_on_two_lines() {
        let rate = 1e6;
        let (mut e0, mut e1, mut t) = (vec![], vec![], 1000i64);
        let cards = [(12u32, 3456u32), (200, 65000), (7, 1)];
        for &(f, c) in &cards {
            for b in w26(f, c) {
                let e = if b == 1 { &mut e1 } else { &mut e0 };
                e.extend([t, t + 50]);
                t += 1000;
            }
            t += 50_000;
        }
        let v = View::new(rate, t, vec![Channel::new("X", 1, &e1), Channel::new("Y", 1, &e0)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].roles["d0"], "Y");
        assert_eq!(c[0].roles["d1"], "X");
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let f = lay["frames"].as_array().unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(
            (f[1]["facility"].clone(), f[1]["card"].clone()),
            (json!(200), json!(65000))
        );
        assert!(c[0].score > 0.8);
    }
}
