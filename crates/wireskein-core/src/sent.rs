//! SENT (SAE J2716; Python's `_engine/sent.py`): the time between falling edges carries the values. A message: a
//! sync of 56 clock ticks (the tick, 3 to 90 us, from it), a status nibble, 1 to 6 data nibbles and a CRC nibble,
//! each 12 + its value ticks. CRC4 x^4 + x^3 + x^2 + 1, seed 5, over the data nibbles; the 2010 recommendation runs a
//! zero nibble in after them (both accepted, the kind kept).

use crate::Candidate;
use crate::kernels::median;
use crate::pulse::{best_of_views, candidate, evidence, level_after, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const SYNC_TICKS: f64 = 56.0;
const TICK_S: (f64, f64) = (2.5e-6, 100e-6);
const NIBBLE: (f64, f64) = (11.5, 27.5);
const TICK_TOL: f64 = 0.05; // a message's tick within this share of the median: the same sensor's
const MIN: usize = 6; // messages checking, at least
const GOOD: f64 = 0.8; // messages of one tick and length whose CRC holds, this share of all at least

fn table() -> [u8; 16] {
    let mut t = [0u8; 16];
    for (i, x) in t.iter_mut().enumerate() {
        let mut v = (i as u32) << 4;
        for b in [7, 6, 5, 4] {
            if v & (1 << b) != 0 {
                v ^= 0x1D << (b - 4);
            }
        }
        *x = (v & 0xF) as u8;
    }
    t
}

pub fn crc4(data: &[u8], recommended: bool) -> u8 {
    let t = table();
    let mut c = 5u8;
    for &x in data {
        c = t[c as usize] ^ x;
    }
    if recommended { t[c as usize] } else { c }
}

struct Msg {
    tick_s: f64,
    data_len: usize,
    ok: bool,
    row: Map<String, Value>,
}

fn messages(ch: &Channel, rate: f64) -> Vec<Msg> {
    let e = ch.edges;
    if e.len() < 20 {
        return vec![];
    }
    let lv = level_after(ch);
    let f: Vec<i64> = e.iter().zip(&lv).filter(|&(_, &l)| l == 0).map(|(&x, _)| x).collect();
    let d: Vec<f64> = f.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.len() < 4 {
        return vec![];
    }
    let in_tick = |t: f64| t >= TICK_S.0 * rate && t <= TICK_S.1 * rate;
    let in_nib = |r: f64| (NIBBLE.0..=NIBBLE.1).contains(&r);
    // where a sync may be: 56 ticks of SENT's range followed by three nibbles
    let syncs: Vec<usize> = (0..d.len() - 3)
        .filter(|&i| {
            let t = d[i] / SYNC_TICKS;
            in_tick(t) && (1..4).all(|k| in_nib(d[i + k] / t))
        })
        .collect();
    let mut out = vec![];
    let Some(&first) = syncs.first() else {
        return out;
    };
    let mut i = first;
    while i + 3 < d.len() {
        let tick = d[i] / SYNC_TICKS;
        if !(in_tick(tick) && (1..4).all(|k| in_nib(d[i + k] / tick))) {
            let k = syncs.partition_point(|&s| s < i + 1);
            if k >= syncs.len() {
                break;
            }
            i = syncs[k];
            continue;
        }
        let (mut j, mut nib) = (i + 1, vec![]);
        while j < d.len() && in_nib(d[j] / tick) && nib.len() < 9 {
            nib.push(((d[j] / tick).round_ties_even() as i64 - 12) as u8);
            j += 1;
        }
        if nib.len() < 3 {
            i = j;
            continue;
        }
        let (status, data, crc) = (nib[0], &nib[1..nib.len() - 1], nib[nib.len() - 1]);
        let kind = if crc4(data, true) == crc {
            Some("2010")
        } else if crc4(data, false) == crc {
            Some("legacy")
        } else {
            None
        };
        let mut row = obj(vec![
            ("s", json!(f[i])),
            ("e", json!(f[j])),
            ("tick_s", json!(tick / rate)),
            ("status", json!(status)),
            ("data", json!(data)),
            ("crc", json!(crc)),
            ("ok", json!(kind.is_some())),
        ]);
        if let Some(k) = kind {
            row.insert("crc_kind".into(), json!(k));
        }
        out.push(Msg {
            tick_s: tick / rate,
            data_len: data.len(),
            ok: kind.is_some(),
            row,
        });
        i = j;
    }
    out
}

/// The most common value (the least of those tied, as Python's max over a set of small ints).
fn mode(x: &[usize]) -> usize {
    let mut best = (0usize, 0usize);
    let mut v = x.to_vec();
    v.sort_unstable();
    v.dedup();
    for u in v {
        let c = x.iter().filter(|&&y| y == u).count();
        if c > best.1 {
            best = (u, c);
        }
    }
    best.0
}

fn read(ch: &Channel, rate: f64, k: i64) -> Option<Candidate> {
    let msgs = messages(ch, rate);
    if msgs.len() < 3 {
        return None;
    }
    // a sensor's messages share its tick and its count of data nibbles: those that do are its
    let tick = median(&msgs.iter().map(|m| m.tick_s).collect::<Vec<_>>());
    let md = mode(&msgs.iter().map(|m| m.data_len).collect::<Vec<_>>());
    let good: Vec<&Msg> = msgs
        .iter()
        .filter(|m| (m.tick_s / tick - 1.0).abs() < TICK_TOL && m.data_len == md && m.ok)
        .collect();
    if good.len() < MIN || (good.len() as f64) < GOOD * msgs.len() as f64 {
        return None;
    }
    // each message's CRC (4) and its sync of 56 ticks with nibbles in range (2)
    let score = good.len() as f64 / msgs.len() as f64 * evidence(6.0 * good.len() as f64);
    let params = obj(vec![
        (
            "tick_s",
            json!(median(&good.iter().map(|m| m.tick_s).collect::<Vec<_>>())),
        ),
        (
            "data_nibbles",
            json!(mode(&good.iter().map(|m| m.data_len).collect::<Vec<_>>())),
        ),
    ]);
    let ev = obj(vec![("messages", json!(msgs.len())), ("ok", json!(good.len()))]);
    let rows: Vec<Map<String, Value>> = msgs.into_iter().map(|m| m.row).collect();
    Some(candidate(
        "sent",
        &[("data", ch.name)],
        score,
        params,
        ev,
        "messages",
        &rows,
        k,
    ))
}

/// The SENT readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 1e6 {
        return vec![];
    }
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 20)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_with_their_crc() {
        let rate = 10e6;
        let tick = 30.0; // 3 us
        let (mut e, mut t) = (vec![], 1000.0f64);
        let mut pulse = |ticks: f64, e: &mut Vec<i64>| {
            e.push(t.round() as i64); // falling
            e.push((t + 5.0 * tick).round() as i64); // rising
            t += ticks * tick;
        };
        let mut sent = vec![];
        for m in 0..10u8 {
            let data = [m & 0xF, 3, 0xA, (m * 7) & 0xF];
            let crc = crc4(&data, true);
            sent.push(data);
            pulse(56.0, &mut e);
            for x in std::iter::once(0u8).chain(data).chain(std::iter::once(crc)) {
                pulse(12.0 + x as f64, &mut e);
            }
        }
        pulse(56.0, &mut e);
        let n = t as i64 + 1000;
        let v = View::new(rate, n, vec![Channel::new("S", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].params["data_nibbles"], 4);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let ms = lay["messages"].as_array().unwrap();
        assert_eq!(ms.len(), 10);
        assert_eq!(ms[3]["data"], json!(sent[3]));
        assert_eq!(ms[3]["crc_kind"], "2010");
    }
}
