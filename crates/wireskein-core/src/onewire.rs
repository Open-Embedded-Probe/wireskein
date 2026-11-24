//! 1-Wire (Python's `pulsecode.onewire` and `analyzers/pulse.OneWirePlugin`): one line idle high, every pulse low. A
//! low of `reset_detect` or more is a reset, the next low inside the presence window after it a device's presence
//! pulse, every other low a slot (1 when shorter than `split`), LSB first. An Overdrive Skip / Match ROM at standard
//! speed puts the rest at overdrive, until a reset of standard length. The evidence: resets answered by presence
//! pulses, whole bytes after them, slots at the master's pace, bytes of more than one value.

use crate::Candidate;
use crate::kernels::{median, runs};
use crate::pulse::{best_of_line_views, candidate, hex, obj};
use crate::pulsecode::{degenerate, pulses, q, ticks_k, to_bytes, views};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

struct Speed {
    name: &'static str,
    wait_max: f64,
    slot_min: f64,
    low_0_max: f64,
    low_1_min: f64,
    split: f64,
    reset_detect: f64,
}

const STANDARD: Speed = Speed {
    name: "standard",
    wait_max: 60e-6,
    slot_min: 61e-6,
    low_0_max: 120e-6,
    low_1_min: 1e-6,
    split: 15e-6,
    reset_detect: 288e-6,
};
const OVERDRIVE: Speed = Speed {
    name: "overdrive",
    wait_max: 6e-6,
    slot_min: 7e-6,
    low_0_max: 16e-6,
    low_1_min: 1e-6,
    split: 2e-6,
    reset_detect: 48e-6,
};
const OVERDRIVE_ROM: [u8; 2] = [0x3C, 0x69]; // Overdrive Skip ROM, Overdrive Match ROM
const OVERDRIVE_LOOK: usize = 8; // lows after such a command whose pace tells the switch
const PRESENCE: f64 = 2.0; // presence pulses seen: this many count as every reset answered
const PACE: f64 = 0.25; // a slot period within this of the median keeps the master's pace

struct Reset {
    t: i64,
    reset: i64,
    presence: bool,
    speed: Option<&'static str>,
    overdrive_after: bool,
    bits: Vec<u8>,
}

struct Read {
    resets: Vec<Reset>,
    low_0: usize,
    low_1: usize,
    slot: Vec<f64>,
}

fn sp(over: bool) -> &'static Speed {
    if over { &OVERDRIVE } else { &STANDARD }
}

fn decode(ch: &Channel, rate: f64, speed_over: bool) -> Read {
    let (st, w) = pulses(ch, 0);
    let mut r = Read {
        resets: vec![],
        low_0: 0,
        low_1: 0,
        slot: vec![],
    };
    let std_reset = STANDARD.reset_detect * rate;
    let mut mode = speed_over;
    let mut last: Option<(i64, i64)> = None;
    // the lows after the i-th at overdrive's pace (or of overdrive's widths with both bits among them)
    let paced_overdrive = |i: usize| -> bool {
        let (mut gaps, mut lows) = (vec![], vec![]);
        for j in i + 1..st.len().min(i + 1 + OVERDRIVE_LOOK) {
            if w[j] as f64 >= std_reset {
                break;
            }
            gaps.push((st[j] - st[j - 1]) as f64);
            lows.push(w[j] as f64 / rate);
        }
        if gaps.is_empty() {
            return false;
        }
        if median(&gaps) < STANDARD.slot_min * rate {
            return true;
        }
        lows.iter().all(|&l| l <= OVERDRIVE.low_0_max)
            && lows.iter().any(|&l| l < OVERDRIVE.split)
            && lows.iter().any(|&l| l >= OVERDRIVE.split)
    };
    for i in 0..st.len() {
        let (s, x) = (st[i], w[i]);
        let mut p = sp(mode);
        if x as f64 >= p.reset_detect * rate {
            if x as f64 >= std_reset {
                mode = speed_over;
                p = sp(mode);
            }
            r.resets.push(Reset {
                t: s,
                reset: x,
                presence: false,
                speed: (mode != speed_over).then_some(p.name),
                overdrive_after: false,
                bits: vec![],
            });
            last = None;
            continue;
        }
        let Some(cur) = r.resets.last_mut() else {
            continue;
        };
        let rel = cur.t + cur.reset;
        if !cur.presence
            && cur.bits.is_empty()
            && (s - rel) as f64 <= 2.0 * p.wait_max * rate
            && x as f64 >= p.split * rate
        {
            cur.presence = true;
            continue;
        }
        let one = (x as f64) < p.split * rate;
        cur.bits.push(one as u8);
        if mode == speed_over {
            if one {
                r.low_1 += 1;
            } else {
                r.low_0 += 1;
            }
            if let Some((ls, _)) = last
                && ((s - ls) as f64) < 4.0 * p.slot_min * rate
            {
                r.slot.push((s - ls) as f64);
            }
        }
        last = Some((s, x));
        if !mode && cur.bits.len() == 8 && OVERDRIVE_ROM.contains(&to_bytes(&cur.bits, true).0[0]) && paced_overdrive(i)
        {
            mode = true;
            last = None;
            cur.overdrive_after = true;
        }
    }
    r
}

/// A view's reading at one speed: (score, its reading), or None.
fn score_of(r: &Read) -> Option<f64> {
    let rs = &r.resets;
    // slots of both kinds (a clock's lows are all alike), whole bytes after most resets
    if rs.is_empty() || r.low_0 == 0 || r.low_1 == 0 {
        return None;
    }
    let n_pres = rs.iter().filter(|z| z.presence).count() as f64;
    let pres = (n_pres / rs.len() as f64).max(n_pres / PRESENCE).min(1.0);
    let whole = rs
        .iter()
        .filter(|z| z.bits.len() % 8 == 0 && !z.bits.is_empty())
        .count() as f64
        / rs.len() as f64;
    if whole < 0.5 {
        return None;
    }
    let bits: usize = rs.iter().map(|z| z.bits.len()).sum();
    let paced = paced(&r.slot);
    // a run of one value inside a reset (a blank page read) is one value there
    let mut data = vec![];
    for z in rs {
        let (b, _) = to_bytes(&z.bits, true);
        for (k, &v) in b.iter().enumerate() {
            if k == 0 || b[k - 1] != v {
                data.push(v);
            }
        }
    }
    Some(
        (0.6 + 0.4 * pres)
            * whole
            * q(bits as f64 / 8.0, 2.0)
            * q(rs.len() as f64, 1.0)
            * ((paced - 0.5) / 0.3).clamp(0.0, 1.0)
            * degenerate(&data),
    )
}

fn paced(slot: &[f64]) -> f64 {
    if slot.is_empty() {
        return 0.0;
    }
    let m = median(slot);
    slot.iter().filter(|&&p| (p / m - 1.0).abs() <= PACE).count() as f64 / slot.len() as f64
}

fn read_line(ch: &Channel, n: i64, rate: f64, outer: i64) -> Option<Candidate> {
    // a bus idle high: high most of the time, though a capture triggered on a reset's fall starts low
    if ch.initial != 1 {
        let (_, length, level) = runs(ch, n);
        let high: i64 = length
            .iter()
            .zip(&level)
            .filter(|&(_, &l)| l == 1)
            .map(|(&x, _)| x)
            .sum();
        if (high as f64) < 0.5 * n as f64 {
            return None;
        }
    }
    let low_min = STANDARD.low_1_min.min(OVERDRIVE.low_1_min) * rate;
    let mut best: Option<(f64, bool, Read, i64)> = None;
    for (v, dg) in views(ch.edges, ticks_k(STANDARD.low_1_min, rate), true) {
        let c = Channel::new(ch.name, ch.initial, &v);
        // a view whose lows are mostly shorter than any 1-Wire low is none (an LED string's bits)
        let (_, lw) = pulses(&c, 0);
        if lw.is_empty() || median(&lw.iter().map(|&x| x as f64).collect::<Vec<_>>()) < low_min {
            continue;
        }
        for over in [false, true] {
            let r = decode(&c, rate, over);
            if let Some(s) = score_of(&r)
                && s >= 0.2
                && best.as_ref().is_none_or(|b| s > b.0)
            {
                best = Some((s, over, r, dg));
            }
        }
    }
    let (score, over, r, dg) = best?;
    let rows: Vec<Map<String, Value>> = r
        .resets
        .iter()
        .map(|z| {
            let mut m = obj(vec![
                ("s", json!(z.t)),
                ("reset_ticks", json!(z.reset)),
                ("presence", json!(z.presence)),
                ("data", json!(hex(&to_bytes(&z.bits, true).0))),
            ]);
            if let Some(s) = z.speed {
                m.insert("speed".into(), json!(s));
            }
            if z.overdrive_after {
                m.insert("overdrive_after_bits".into(), json!(8));
            }
            m
        })
        .collect();
    let ev = obj(vec![
        ("resets", json!(r.resets.len())),
        ("presence", json!(r.resets.iter().filter(|z| z.presence).count())),
        ("bits", json!(r.resets.iter().map(|z| z.bits.len()).sum::<usize>())),
        ("paced", json!((paced(&r.slot) * 1e3).round() / 1e3)),
    ]);
    Some(candidate(
        "onewire",
        &[("dq", ch.name)],
        score,
        obj(vec![("speed", json!(sp(over).name))]),
        ev,
        "resets",
        &rows,
        if outer != 0 { outer } else { dg },
    ))
}

/// The 1-Wire readings of a part: each line, the best of its views and speeds.
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 4)
        .filter_map(|ch| best_of_line_views(ch, |c, k| read_line(c, view.n_samples, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resets_presence_and_bytes() {
        let rate = 1e6; // a tick a microsecond
        let (mut e, mut t) = (vec![], 100i64);
        let txs: [&[u8]; 3] = [&[0xCC, 0x44], &[0xCC, 0xBE, 0x50, 0x05], &[0x33, 0x28, 0xFF, 0x4C]];
        for tx in txs {
            e.extend([t, t + 500]); // reset
            t += 500 + 30;
            e.extend([t, t + 120]); // presence
            t += 470;
            for &b in tx {
                for k in 0..8 {
                    let low = if (b >> k) & 1 == 1 { 6 } else { 60 };
                    e.extend([t, t + low]);
                    t += 70;
                }
            }
            t += 1000;
        }
        let v = View::new(rate, t, vec![Channel::new("Q", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].params["speed"], "standard");
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let rs = lay["resets"].as_array().unwrap();
        assert_eq!(rs.len(), 3);
        assert_eq!(rs[1]["data"], "ccbe5005");
        assert!(rs.iter().all(|z| z["presence"] == json!(true)));
        assert!(c[0].score > 0.8, "{}", c[0].score);
    }
}
