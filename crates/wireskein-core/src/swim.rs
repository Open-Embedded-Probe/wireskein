//! SWIM (STM8's single wire debug; Python's `_engine/swim.py`): each bit a low pulse, short for 1 and long for 0
//! (low speed 2 or 20 of 22 clocks a bit, high speed 2 or 8 of 10), the line idling high. A frame: the start bit (0
//! from the host, 1 from the target), 3 command bits or 8 data bits MSB first, their even parity, the receiver's
//! acknowledge (1). ROTF / WOTF go on with the byte count, a 24-bit address, then the bytes.

use crate::Candidate;
use crate::kernels::{median, runs};
use crate::pulse::{best_of_views, candidate, evidence, hex, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const GAP: f64 = 1.6; // a gap of this many bit times between pulses ends a frame
const SHORT: f64 = 0.5; // a low shorter than this share of a bit: 1
const SHORT_MAX: f64 = 0.35; // a 1's low at most this share of a bit
const LONG_MIN: f64 = 0.65; // a 0's low at least
const PACE: f64 = 0.15; // a frame's pulses within this share of a bit of its pace
const GOOD: f64 = 0.8; // frames whose parity and acknowledge hold, this share of all at least

struct Frame {
    s: i64,
    e: i64,
    target: bool,
    bits: Vec<u8>,
    parity_ok: bool,
    ack: u8,
}

fn frames(ch: &Channel, n: i64) -> Vec<Frame> {
    let (st, length, level) = runs(ch, n);
    let m = st.len();
    let (s, w): (Vec<i64>, Vec<i64>) = (1..m.saturating_sub(1))
        .filter(|&i| level[i] == 0)
        .map(|i| (st[i], length[i]))
        .unzip();
    if s.len() < 12 {
        return vec![];
    }
    let gaps: Vec<f64> = s.windows(2).map(|x| (x[1] - x[0]) as f64).collect();
    let bit = median(&gaps);
    if bit <= 2.0 {
        return vec![];
    }
    let mut groups: Vec<Vec<usize>> = vec![vec![0]];
    for (i, &g) in gaps.iter().enumerate() {
        if g > GAP * bit {
            groups.push(vec![]);
        }
        groups.last_mut().unwrap().push(i + 1);
    }
    // an acknowledge comes from the other side, maybe after a pause: a lone pulse after a frame is its
    let mut merged: Vec<Vec<usize>> = vec![];
    for g in groups {
        if g.len() == 1 && merged.last().is_some_and(|l| l.len() == 5 || l.len() == 10) {
            merged.last_mut().unwrap().extend(g);
        } else {
            merged.push(g);
        }
    }
    let mut out = vec![];
    for g in merged {
        if g.len() != 6 && g.len() != 11 {
            continue;
        }
        // a frame's pulses at its bit's pace, each low clearly short or long
        let paced = g[..g.len() - 1]
            .windows(2)
            .all(|p| (((s[p[1]] - s[p[0]]) as f64 / bit) - 1.0).abs() <= PACE);
        let clear = g.iter().all(|&i| {
            let r = w[i] as f64 / bit;
            !(r > SHORT_MAX && r < LONG_MIN)
        });
        if !paced || !clear {
            continue;
        }
        let b: Vec<u8> = g.iter().map(|&i| ((w[i] as f64) < SHORT * bit) as u8).collect();
        let payload = b[1..b.len() - 2].to_vec();
        let par = payload.iter().map(|&x| x as u32).sum::<u32>() & 1;
        let last = *g.last().unwrap();
        out.push(Frame {
            s: s[g[0]],
            e: s[last] + w[last],
            target: b[0] == 1,
            parity_ok: par == b[b.len() - 2] as u32,
            ack: b[b.len() - 1],
            bits: payload,
        });
    }
    out
}

fn byte(bits: &[u8]) -> u32 {
    bits.iter().fold(0u32, |a, &b| (a << 1) | b as u32)
}

/// The commands and their bytes: (row, ok, has an address).
fn transactions(fr: &[Frame]) -> Vec<(Map<String, Value>, bool, bool)> {
    let mut out = vec![];
    let mut i = 0usize;
    while i < fr.len() {
        let f = &fr[i];
        if f.target || f.bits.len() != 3 {
            i += 1;
            continue;
        }
        let c = byte(&f.bits);
        let cmd = match c {
            0 => "SRST".to_string(),
            1 => "ROTF".to_string(),
            2 => "WOTF".to_string(),
            _ => format!("CMD{c}"),
        };
        let mut row = obj(vec![("s", json!(f.s)), ("e", json!(f.e)), ("cmd", json!(cmd))]);
        let mut ok = f.parity_ok && f.ack == 1;
        let mut addr = false;
        i += 1;
        if (cmd == "ROTF" || cmd == "WOTF") && i + 4 <= fr.len() && fr[i..i + 4].iter().all(|x| x.bits.len() == 8) {
            let h = &fr[i..i + 4];
            let count = byte(&h[0].bits);
            row.insert("count".into(), json!(count));
            row.insert(
                "addr".into(),
                json!((byte(&h[1].bits) << 16) | (byte(&h[2].bits) << 8) | byte(&h[3].bits)),
            );
            addr = true;
            ok &= h.iter().all(|x| x.parity_ok);
            i += 4;
            let mut data = vec![];
            while i < fr.len() && fr[i].bits.len() == 8 && data.len() < count.max(1) as usize {
                data.push(byte(&fr[i].bits) as u8);
                ok &= fr[i].parity_ok;
                row.insert("e".into(), json!(fr[i].e));
                i += 1;
            }
            row.insert("data".into(), json!(hex(&data)));
        }
        row.insert("ok".into(), json!(ok));
        out.push((row, ok, addr));
    }
    out
}

fn read(ch: &Channel, n: i64, k: i64) -> Option<Candidate> {
    let fr = frames(ch, n);
    if fr.is_empty() {
        return None;
    }
    let good = fr.iter().filter(|f| f.parity_ok && f.ack == 1).count();
    // a frame's parity and acknowledge hold by chance one time in four: a line of other pulses has some
    if good < 3 || (good as f64) < GOOD * fr.len() as f64 {
        return None;
    }
    let tr = transactions(&fr);
    // a host reads or writes: a ROTF / WOTF with its count and address
    if !tr.iter().any(|&(_, ok, addr)| ok && addr) {
        return None;
    }
    // each frame's parity and acknowledge (2), the frames' lengths (1)
    let score = good as f64 / fr.len() as f64 * evidence(3.0 * good as f64);
    let ev = obj(vec![
        ("frames", json!(fr.len())),
        ("good", json!(good)),
        ("transactions", json!(tr.len())),
    ]);
    let rows: Vec<Map<String, Value>> = tr.into_iter().map(|t| t.0).collect();
    Some(candidate(
        "swim",
        &[("swim", ch.name)],
        score,
        Map::new(),
        ev,
        "transactions",
        &rows,
        k,
    ))
}

/// The SWIM readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 24)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.n_samples, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame's pulses (low speed: a bit 22 clocks, a 1's low 2, a 0's 20), its parity and acknowledge added.
    fn frame(e: &mut Vec<i64>, t: &mut i64, target: bool, bits: &[u8], clk: i64) {
        let mut all = vec![target as u8];
        all.extend(bits);
        all.push(bits.iter().sum::<u8>() & 1);
        all.push(1);
        for b in all {
            e.push(*t);
            e.push(*t + if b == 1 { 2 } else { 20 } * clk);
            *t += 22 * clk;
        }
        *t += 60 * clk;
    }

    fn bits(v: u32, n: usize) -> Vec<u8> {
        (0..n).rev().map(|q| ((v >> q) & 1) as u8).collect()
    }

    #[test]
    fn a_read_and_a_write() {
        let clk = 3; // ticks a SWIM clock
        let (mut e, mut t) = (vec![], 100i64);
        for (cmd, addr, data) in [(1u32, 0x7F80u32, vec![0x12u32, 0x34]), (2, 0x5005, vec![0xAB])] {
            frame(&mut e, &mut t, false, &bits(cmd, 3), clk);
            for b in [data.len() as u32, addr >> 16, (addr >> 8) & 0xFF, addr & 0xFF] {
                frame(&mut e, &mut t, false, &bits(b, 8), clk);
            }
            for d in data {
                frame(&mut e, &mut t, cmd == 1, &bits(d, 8), clk);
            }
        }
        let v = View::new(1e6, t + 100, vec![Channel::new("W", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let tr = lay["transactions"].as_array().unwrap();
        assert_eq!(tr.len(), 2);
        assert_eq!(tr[0]["cmd"], "ROTF");
        assert_eq!(tr[0]["addr"], 0x7F80);
        assert_eq!(tr[0]["count"], 2);
        assert_eq!(tr[0]["data"], "1234");
        assert_eq!(tr[1]["cmd"], "WOTF");
        assert_eq!(tr[1]["data"], "ab");
    }
}
