//! DALI (IEC 62386, lighting control; Python's `_engine/dali.py`): one line idling high, Manchester at 1200 b/s (a
//! bit two halves of 417 us, low then high for 1), a start bit (1), then 16 bits (a forward frame: the address byte
//! and the command or arc power) or 8 (a backward frame: a gear's answer), MSB first.

use crate::Candidate;
use crate::kernels::runs;
use crate::pulse::{best_of_views, candidate, evidence, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const HALF_S: f64 = 1.0 / 2400.0;
const TOL: f64 = 0.25; // runs within this share of a whole number of halves
const GOOD: f64 = 0.8; // frames of valid Manchester and length, this share of all at least

fn address(a: u32, m: &mut Map<String, Value>) {
    if a >> 7 == 0 {
        m.insert("to".into(), json!("short"));
        m.insert("address".into(), json!((a >> 1) & 0x3F));
    } else if a >> 5 == 0b100 {
        m.insert("to".into(), json!("group"));
        m.insert("address".into(), json!((a >> 1) & 0xF));
    } else if a >> 1 == 0x7F {
        m.insert("to".into(), json!("broadcast"));
    } else {
        m.insert("to".into(), json!("special"));
    }
    m.insert(
        "selector".into(),
        json!(if a & 1 == 1 { "command" } else { "arc_power" }),
    );
}

/// The frames on a line: forward {kind, addr_byte, data, to, address?, selector, ok}, backward {kind, data, ok}, or
/// {ok: false, bits} for a run of Manchester of another length.
pub fn frames(ch: &Channel, n: i64, rate: f64) -> Vec<Map<String, Value>> {
    let (st, length, level) = runs(ch, n);
    let half = HALF_S * rate;
    let k: Vec<f64> = length.iter().map(|&l| l as f64 / half).collect();
    // a DALI line's runs are a half bit or a whole one: a line whose runs mostly are not, is none
    let near = k
        .iter()
        .filter(|&&x| (x - 1.0).abs() < TOL || (x - 2.0).abs() < 2.0 * TOL)
        .count();
    if k.len() < 20 || (near as f64) < 0.5 * k.len() as f64 {
        return vec![];
    }
    let mut out = vec![];
    let mut i = 1usize;
    while i + 1 < st.len() {
        // a frame starts where the idle high falls
        if level[i] != 0 || level[i - 1] != 1 {
            i += 1;
            continue;
        }
        let (mut halves, mut j, mut ok) = (vec![], i, true);
        while j + 1 < st.len() {
            let m = k[j].round_ties_even();
            if (m == 1.0 || m == 2.0) && (k[j] - m).abs() <= TOL {
                halves.extend(std::iter::repeat_n(level[j], m as usize));
                j += 1;
            } else {
                break;
            }
        }
        // the line back at idle (high) after the last bit: a trailing high half belongs to it
        if j < st.len() && level[j] == 1 && halves.len() % 2 == 1 {
            halves.push(1);
        }
        let mut bits = vec![];
        for q in (0..halves.len().saturating_sub(1)).step_by(2) {
            match (halves[q], halves[q + 1]) {
                (0, 1) => bits.push(1u32),
                (1, 0) => bits.push(0),
                _ => {
                    ok = false;
                    break;
                }
            }
        }
        let (s0, e0) = (st[i], if j < st.len() { st[j] } else { n });
        i = j.max(i + 1);
        let nb = bits.len() as i64 - 1;
        if !ok || bits.is_empty() || bits[0] != 1 || (nb != 8 && nb != 16) {
            if bits.len() > 2 {
                out.push(obj(vec![
                    ("s", json!(s0)),
                    ("e", json!(e0)),
                    ("ok", json!(false)),
                    ("bits", json!(nb)),
                ]));
            }
            continue;
        }
        let data = bits[1..].iter().fold(0u32, |a, &b| (a << 1) | b);
        let mut m = obj(vec![("s", json!(s0)), ("e", json!(e0))]);
        if nb == 16 {
            let a = data >> 8;
            m.insert("kind".into(), json!("forward"));
            m.insert("addr_byte".into(), json!(a));
            m.insert("data".into(), json!(data & 0xFF));
            address(a, &mut m);
        } else {
            m.insert("kind".into(), json!("backward"));
            m.insert("data".into(), json!(data));
        }
        m.insert("ok".into(), json!(true));
        out.push(m);
    }
    out
}

fn read(ch: &Channel, n: i64, rate: f64, k: i64) -> Option<Candidate> {
    let fr = frames(ch, n, rate);
    let good: Vec<&Map<String, Value>> = fr.iter().filter(|f| f["ok"] == json!(true)).collect();
    if good.len() < 2 || (good.len() as f64) < GOOD * fr.len() as f64 {
        return None;
    }
    // evidence in bits: each frame's halves all valid Manchester pairs at DALI's own rate, its length 8 or 16
    let info: f64 = good
        .iter()
        .map(|f| 4.0 + if f["kind"] == json!("forward") { 2.0 } else { 1.0 })
        .sum();
    let score = good.len() as f64 / fr.len() as f64 * evidence(info);
    let ev = obj(vec![("frames", json!(fr.len())), ("ok", json!(good.len()))]);
    Some(candidate(
        "dali",
        &[("dali", ch.name)],
        score,
        Map::new(),
        ev,
        "frames",
        &fr,
        k,
    ))
}

/// The DALI readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 10e3 {
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

    /// A DALI line at 100 kHz: frames of (value, bits), each a start bit and the bits MSB first, idle between.
    fn encode(frames: &[(u32, usize)], rate: f64) -> Vec<i64> {
        let half = rate / 2400.0;
        let mut halves: Vec<u8> = vec![1; 40];
        for &(v, nb) in frames {
            let mut bits = vec![1u32];
            bits.extend((0..nb).rev().map(|q| (v >> q) & 1));
            for b in bits {
                halves.extend(if b == 1 { [0, 1] } else { [1, 0] });
            }
            halves.extend([1u8; 30]);
        }
        let mut e = vec![];
        for q in 1..halves.len() {
            if halves[q] != halves[q - 1] {
                e.push((q as f64 * half).round() as i64);
            }
        }
        e
    }

    #[test]
    fn forward_and_backward_frames() {
        let rate = 100e3;
        let e = encode(&[(0x03_90, 16), (0x5a, 8), (0xff_a1, 16)], rate);
        let n = e.last().unwrap() + 2000;
        let v = View::new(rate, n, vec![Channel::new("L", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert!(c[0].score > 0.6, "{}", c[0].score);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let f = &lay["frames"];
        assert_eq!(f[0]["kind"], "forward");
        assert_eq!(f[0]["addr_byte"], 3);
        assert_eq!(f[0]["data"], 0x90);
        assert_eq!(f[0]["to"], "short");
        assert_eq!(f[1]["kind"], "backward");
        assert_eq!(f[1]["data"], 0x5a);
        assert_eq!(f[2]["to"], "broadcast");
    }
}
