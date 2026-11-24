//! DCC (NMRA S-9.1 / S-9.2, model railway control; Python's `_engine/dcc.py`) on the track signal: each bit two
//! halves of equal length (1: 52-64 us a half, 0: 90 us or more); a packet a preamble of at least 10 ones, bytes each
//! led by a 0, the last the XOR of the others, a closing 1.

use crate::Candidate;
use crate::pulse::{best_of_views, candidate, evidence, hex, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const ONE_HALF: (f64, f64) = (52e-6, 64e-6);
const ZERO_HALF: (f64, f64) = (90e-6, 10e-3);
const PREAMBLE: usize = 10;
const ONES: f64 = 0.3; // halves of a 1's length, this share of the line's at least

/// (bits, tick of each bit's start): a bit where two halves of the same kind follow (-1: an unpaired half).
fn bits_of(ch: &Channel, rate: f64) -> (Vec<i8>, Vec<i64>) {
    let e = ch.edges;
    if e.len() < 40 {
        return (vec![], vec![]);
    }
    let d: Vec<f64> = e.windows(2).map(|w| (w[1] - w[0]) as f64 / rate).collect();
    let one = |x: f64| x >= ONE_HALF.0 && x <= ONE_HALF.1;
    if (d.iter().filter(|&&x| one(x)).count() as f64) < ONES * d.len() as f64 {
        return (vec![], vec![]);
    }
    let kind: Vec<i8> = d
        .iter()
        .map(|&x| {
            if one(x) {
                1
            } else if x >= ZERO_HALF.0 && x <= ZERO_HALF.1 {
                0
            } else {
                -1
            }
        })
        .collect();
    let (mut bits, mut at) = (vec![], vec![]);
    let mut i = 0usize;
    while i + 1 < kind.len() {
        if kind[i] >= 0 && kind[i] == kind[i + 1] {
            bits.push(kind[i]);
            at.push(e[i]);
            i += 2;
        } else {
            bits.push(-1);
            at.push(e[i]);
            i += 1;
        }
    }
    (bits, at)
}

fn address(b: &[u8], m: &mut Map<String, Value>) {
    let a = b[0];
    if a == 0xFF {
        m.insert("address".into(), json!("idle"));
    } else if a == 0 {
        m.insert("address".into(), json!("broadcast"));
    } else if (1..=127).contains(&a) {
        m.insert("address".into(), json!(a));
    } else if (192..=231).contains(&a) && b.len() > 1 {
        m.insert("address".into(), json!(((a as u32 & 0x3F) << 8) | b[1] as u32));
        m.insert("long".into(), json!(true));
    } else {
        m.insert("address".into(), json!(a));
        m.insert("accessory".into(), json!((128..=191).contains(&a)));
    }
}

/// The packets on a line: {s, e, bytes, ok, address, ...}.
pub fn packets(ch: &Channel, rate: f64) -> Vec<Map<String, Value>> {
    let (bits, at) = bits_of(ch, rate);
    let mut out = vec![];
    let mut i = 0usize;
    while i < bits.len() {
        // a preamble: ten 1s at least, then the packet start bit 0
        let mut k = i;
        while k < bits.len() && bits[k] == 1 {
            k += 1;
        }
        if k - i < PREAMBLE || k >= bits.len() || bits[k] != 0 {
            i = if k == i { k + 1 } else { k };
            continue;
        }
        let (mut j, mut data, mut ok) = (k, vec![], true);
        while j + 9 <= bits.len() && bits[j] == 0 {
            let byte = &bits[j + 1..j + 9];
            if byte.contains(&-1) {
                ok = false;
                break;
            }
            data.push(byte.iter().fold(0u8, |a, &b| (a << 1) | b as u8));
            j += 9;
        }
        let end_ok = j < bits.len() && bits[j] == 1;
        if data.len() >= 2 {
            let x = data[..data.len() - 1].iter().fold(0u8, |a, &b| a ^ b);
            let mut row = obj(vec![
                ("s", json!(at[i])),
                ("e", json!(at[j.min(at.len() - 1)])),
                ("bytes", json!(hex(&data))),
                ("ok", json!(ok && end_ok && x == data[data.len() - 1])),
            ]);
            address(&data, &mut row);
            out.push(row);
        }
        i = j.max(k + 1);
    }
    out
}

fn read(ch: &Channel, rate: f64, k: i64) -> Option<Candidate> {
    let pk = packets(ch, rate);
    let good = pk.iter().filter(|p| p["ok"] == json!(true)).count();
    if good < 3 {
        return None;
    }
    // evidence in bits: each packet's preamble (a run of 1s of DCC's halves) and its error detection byte
    let score = good as f64 / pk.len() as f64 * evidence(10.0 * good as f64);
    let ev = obj(vec![("packets", json!(pk.len())), ("ok", json!(good))]);
    Some(candidate(
        "dcc",
        &[("track", ch.name)],
        score,
        Map::new(),
        ev,
        "packets",
        &pk,
        k,
    ))
}

/// The DCC readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 50e3 {
        return vec![];
    }
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 40)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_with_their_check_byte() {
        let rate = 1e6;
        let mut e = vec![];
        let mut t = 100i64;
        let mut half = |w: i64, e: &mut Vec<i64>| {
            e.push(t);
            t += w;
        };
        let pk: [&[u8]; 4] = [&[3, 0x64], &[0x55, 0x70, 0x12], &[0xC1, 0x02, 0x7F], &[0xFF, 0x00]];
        for p in pk {
            let x = p.iter().fold(0u8, |a, &b| a ^ b);
            let mut bits = vec![1u8; 14];
            for &b in p.iter().chain(std::iter::once(&x)) {
                bits.push(0);
                bits.extend((0..8).rev().map(|q| (b >> q) & 1));
            }
            bits.push(1);
            for b in bits {
                let w = if b == 1 { 58 } else { 100 };
                half(w, &mut e);
                half(w, &mut e);
            }
        }
        e.push(t);
        let n = t + 1000;
        let v = View::new(rate, n, vec![Channel::new("T", 0, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let p = lay["packets"].as_array().unwrap();
        assert_eq!(p.len(), 4);
        assert_eq!(p[0]["address"], 3);
        assert_eq!(p[0]["bytes"], "036467");
        assert_eq!(p[2]["address"], (1 << 8) | 2);
        assert_eq!(p[3]["address"], "idle");
        assert!(p.iter().all(|x| x["ok"] == json!(true)));
        assert!(c[0].score > 0.9);
    }
}
