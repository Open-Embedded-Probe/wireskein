//! An RGB LED chain (WS2812 and its kind; Python's `pulsecode.rgb_led` and `analyzers/pulse.RgbLedPlugin`): one line
//! idle low, each bit a high of T0H or T1H then a low, MSB first; frames between lows of more than 5 bit periods.
//! Its evidence: whole bytes in steady bit periods, two kinds of high about twice apart, colours of both bits.

use crate::Candidate;
use crate::kernels::median;
use crate::pulse::{best_of_line_views, candidate, hex, obj};
use crate::pulsecode::{pulses, q, split, ticks_k, to_bytes, views};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const SHORTEST: f64 = 0.15e-6; // no pulse of a WS2812-like line is shorter
const ONE_OVER_ZERO: f64 = 1.5; // a 1's high over a 0's, this many times at least
const MIXED: f64 = 0.1; // the rarer kind of bit's share a string's data has at least
const BIT_MIN_S: f64 = 0.8e-6;
const BIT_MAX_S: f64 = 3e-6;
const T1H_MAX: f64 = 0.9; // a 1's high, this share of the bit at most
const T0H_MIN: f64 = 0.1; // a 0's high, this share of the bit at least
const CUT_HIGH_S: f64 = 2e-6; // a capture begun inside a bit: its first fall within this
const SAME_FRAMES: usize = 4; // frames of one length (a whole number of LEDs) for a dark string's evidence
const WIDTH_SHARE: f64 = 0.02; // a high width taken for the split when this share of the pulses has it

struct Frame {
    t: i64,
    bits: usize,
    data: Vec<u8>,
    leftover: usize,
}

struct Read {
    frames: Vec<Frame>,
    t0h: Vec<f64>,
    t1h: Vec<f64>,
    period: Vec<f64>,
    bit: f64,
}

fn decode(ch: &Channel) -> Option<Read> {
    let (st, w) = pulses(ch, 1);
    if st.len() < 2 {
        return None;
    }
    let per: Vec<i64> = st.windows(2).map(|x| x[1] - x[0]).collect();
    let bit = median(&per.iter().map(|&x| x as f64).collect::<Vec<_>>());
    // the split from the widths most pulses have (a noise's leftovers left out)
    let mut sorted = w.clone();
    sorted.sort_unstable();
    let need = (2.0f64).max(WIDTH_SHARE * w.len() as f64);
    let mut common = vec![];
    let mut i = 0;
    while i < sorted.len() {
        let j = sorted.partition_point(|&x| x <= sorted[i]);
        if (j - i) as f64 >= need {
            common.push(sorted[i]);
        }
        i = j;
    }
    let sp = match split(&common, 1.3) {
        Some(s) if s <= bit => s,
        // every high alike: all one kind of bit, a 1 when the highs are over half the period
        _ => {
            if median(&w.iter().map(|&x| x as f64).collect::<Vec<_>>()) > bit / 2.0 {
                0.0
            } else {
                bit
            }
        }
    };
    let one: Vec<bool> = w.iter().map(|&x| x as f64 > sp).collect();
    let mut bounds = vec![0usize];
    bounds.extend((0..per.len()).filter(|&k| per[k] as f64 > 5.0 * bit).map(|k| k + 1));
    bounds.push(st.len());
    let mut r = Read {
        frames: vec![],
        t0h: vec![],
        t1h: vec![],
        period: vec![],
        bit,
    };
    for k in 0..w.len() {
        if one[k] {
            r.t1h.push(w[k] as f64);
        } else {
            r.t0h.push(w[k] as f64);
        }
    }
    for z in bounds.windows(2) {
        let (a, b) = (z[0], z[1]);
        r.period.extend(per[a..b - 1].iter().map(|&x| x as f64));
        let bits: Vec<u8> = one[a..b].iter().map(|&x| x as u8).collect();
        let (data, leftover) = to_bytes(&bits, false);
        r.frames.push(Frame {
            t: st[a],
            bits: b - a,
            data,
            leftover,
        });
    }
    Some(r)
}

fn read(ch: &Channel, rate: f64, dg: i64, name: &str) -> Option<Candidate> {
    let r = decode(ch)?;
    let fr: Vec<&Frame> = r.frames.iter().filter(|f| f.bits >= 8).collect();
    if fr.is_empty() {
        return None;
    }
    let bit_s = r.bit / rate;
    if !(BIT_MIN_S..=BIT_MAX_S).contains(&bit_s) {
        return None;
    }
    let steady = if r.period.is_empty() {
        0.0
    } else {
        r.period.iter().filter(|&&p| (p / r.bit - 1.0).abs() < 0.25).count() as f64 / r.period.len() as f64
    };
    let whole = fr.iter().filter(|f| f.leftover == 0).count() as f64 / fr.len() as f64;
    let bits: usize = fr.iter().map(|f| f.bits).sum();
    let data: Vec<u8> = fr.iter().flat_map(|f| f.data.iter().copied()).collect();
    let ones: u32 = data.iter().map(|b| b.count_ones()).sum();
    let nb = 8 * data.len() as u32;
    let minor = ones.min(nb - ones) as f64 / nb.max(1) as f64;
    // a 1's high about twice a 0's, a 0's high a good part of the bit, a 1's inside its bit
    if !r.t0h.is_empty() && !r.t1h.is_empty() {
        let (m0, m1) = (median(&r.t0h), median(&r.t1h));
        if m1 / m0.max(1e-9) < ONE_OVER_ZERO || m0 < T0H_MIN * r.bit || m1 > T1H_MAX * r.bit {
            return None;
        }
    }
    let mut mixed = 0.3 + 0.7 * (minor / MIXED).clamp(0.0, 1.0);
    // a string all dark: frames of one length, a whole number of LEDs, every time
    let lens: Vec<usize> = fr.iter().map(|f| f.bits).collect();
    let mut u = lens.clone();
    u.sort_unstable();
    u.dedup();
    let (mode, cnt) = u
        .iter()
        .map(|&x| (x, lens.iter().filter(|&&y| y == x).count()))
        .fold((0, 0), |a, b| if b.1 > a.1 { b } else { a });
    if fr.len() >= SAME_FRAMES && (mode % 24 == 0 || mode % 32 == 0) {
        mixed = mixed.max(0.3 + 0.7 * cnt as f64 / fr.len() as f64);
    }
    // under 5 samples a bit, a spike's tick is as wide as T0H and T1H's difference
    let resolvable = (r.bit - 4.0).clamp(0.0, 1.0);
    let score = steady * whole * mixed * q(bits as f64 / 24.0, 2.0) * resolvable;
    let ev = obj(vec![
        ("frames", json!(fr.len())),
        ("bits", json!(bits)),
        ("steady", json!(steady)),
        ("whole_bytes", json!(whole)),
        ("bit_s", json!(bit_s)),
        ("rarer_bits", json!((minor * 1e3).round() / 1e3)),
        ("samples_per_bit", json!((r.bit * 100.0).round() / 100.0)),
    ]);
    let rows: Vec<Map<String, Value>> = fr
        .iter()
        .map(|f| {
            obj(vec![
                ("s", json!(f.t)),
                ("bits", json!(f.bits)),
                ("data", json!(hex(&f.data))),
            ])
        })
        .collect();
    Some(candidate(
        "rgb_led",
        &[("data", name)],
        score,
        obj(vec![("bit_s", json!(bit_s))]),
        ev,
        "frames",
        &rows,
        dg,
    ))
}

/// The reading of one line (as it is given): idle low (a capture begun inside a bit's high read from its first
/// fall), as it is, with pulses shorter than any of an LED line's out, with bounces merged; the best.
fn read_line(ch: &Channel, rate: f64, outer: i64) -> Option<Candidate> {
    let (initial, e) = if ch.initial != 0 {
        if ch.edges.len() < 2 || ch.edges[0] as f64 > CUT_HIGH_S * rate {
            return None;
        }
        (0u8, &ch.edges[1..])
    } else {
        (0u8, ch.edges)
    };
    let mut best: Option<Candidate> = None;
    for (v, dg) in views(e, ticks_k(SHORTEST, rate), true) {
        let c = Channel::new(ch.name, initial, &v);
        if let Some(r) = read(&c, rate, if outer != 0 { outer } else { dg }, ch.name)
            && best.as_ref().is_none_or(|b| r.score > b.score)
        {
            best = Some(r);
        }
    }
    best
}

/// The RGB LED readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 16)
        .filter_map(|ch| best_of_line_views(ch, |c, k| read_line(c, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_of_colours() {
        let rate = 20e6; // 25 ticks a bit of 1.25 us; T0H 8, T1H 16
        let (mut e, mut t) = (vec![], 100i64);
        let mut want = vec![];
        for f in 0..4u32 {
            let mut data = vec![];
            for led in 0..5u32 {
                data.extend([(f * 40 + led * 7) as u8, 0x5A, (led * 51) as u8]);
            }
            for &b in &data {
                for q in (0..8).rev() {
                    let h = if (b >> q) & 1 == 1 { 16 } else { 8 };
                    e.extend([t, t + h]);
                    t += 25;
                }
            }
            want.push(hex(&data));
            t += 1200; // reset
        }
        let v = View::new(rate, t, vec![Channel::new("L", 0, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert!(c[0].score > 0.9, "{}", c[0].score);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let got: Vec<String> = lay["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["data"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(got, want);
    }
}
