//! An IR remote's NEC frames (Python's `pulsecode.ir_marks` / `nec` and `analyzers/pulse.NecPlugin`): the marks of
//! the line (a carrier's bursts merged where it is modulated, idle low; a receiver's active lows, idle high); a frame
//! a 9 ms mark, then 4.5 ms and 32 bits (a 562.5 us mark, then 562.5 us for 0 or 1687.5 us for 1, LSB first:
//! address, inverted address or its high byte, command, inverted command) or 2.25 ms (a repeat). The evidence: the
//! commands' inverse bytes.

use crate::Candidate;
use crate::kernels::median;
use crate::pulse::{best_of_line_views, candidate, obj};
use crate::pulsecode::{pulses, q, ticks_k, to_bytes, views};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const LEADER: f64 = 9000e-6;
const SPACE: f64 = 4500e-6;
const REPEAT_SPACE: f64 = 2250e-6;
const SPACE_0: f64 = 562.5e-6;
const SPACE_1: f64 = 1687.5e-6;
const TOL: f64 = 0.25;
const SHORTEST: f64 = 4e-6; // a pulse this short on an IR line is a bounce (a 38 kHz carrier's are 8.8 us)

/// The marks (start, end) and the carrier's frequency where the line is modulated.
fn marks(ch: &Channel, rate: f64) -> (Vec<(i64, i64)>, Option<f64>) {
    let (st, w) = pulses(ch, 1 - ch.initial);
    if st.is_empty() {
        return (vec![], None);
    }
    let wf: Vec<f64> = w.iter().map(|&x| x as f64).collect();
    if median(&wf) < 100e-6 * rate && st.len() > 4 {
        let inside: Vec<bool> = (1..st.len())
            .map(|i| (st[i] - (st[i - 1] + w[i - 1])) as f64 <= 150e-6 * rate)
            .collect();
        let mut carrier = None;
        if inside.iter().any(|&x| x) {
            let per: Vec<f64> = (0..inside.len())
                .filter(|&i| inside[i])
                .map(|i| (st[i + 1] - st[i]) as f64)
                .collect();
            carrier = Some(rate / median(&per));
        }
        let mut b = vec![0usize];
        b.extend((0..inside.len()).filter(|&i| !inside[i]).map(|i| i + 1));
        b.push(st.len());
        let mk = b.windows(2).map(|z| (st[z[0]], st[z[1] - 1] + w[z[1] - 1])).collect();
        (mk, carrier)
    } else {
        (st.iter().zip(&w).map(|(&s, &x)| (s, s + x)).collect(), None)
    }
}

struct Frame {
    repeat: bool,
    cmd_ok: Option<bool>,
    row: Map<String, Value>,
}

fn frames(mk: &[(i64, i64)], rate: f64) -> Vec<Frame> {
    let near = |v: i64, nominal: f64| (v as f64 / rate / nominal - 1.0).abs() <= TOL;
    let mut out = vec![];
    let mut i = 0usize;
    while i < mk.len() {
        let (s, e) = mk[i];
        if !near(e - s, LEADER) || i + 1 >= mk.len() {
            i += 1;
            continue;
        }
        let sp = mk[i + 1].0 - e;
        if near(sp, REPEAT_SPACE) {
            out.push(Frame {
                repeat: true,
                cmd_ok: None,
                row: obj(vec![("s", json!(s)), ("repeat", json!(true))]),
            });
            i += 2;
            continue;
        }
        if !near(sp, SPACE) {
            i += 1;
            continue;
        }
        let (mut bits, mut j) = (vec![], i + 1);
        while j < mk.len() && bits.len() < 32 {
            if j + 1 >= mk.len() {
                break;
            }
            let gap = (mk[j + 1].0 - mk[j].1) as f64 / rate;
            if gap > 2.0 * SPACE_1 {
                break;
            }
            bits.push((gap > (SPACE_0 + SPACE_1) / 2.0) as u8);
            j += 1;
        }
        let (data, _) = to_bytes(&bits, true);
        let mut row = obj(vec![("s", json!(s)), ("repeat", json!(false))]);
        let mut cmd_ok = None;
        if data.len() == 4 {
            let (a, na, c, nc) = (data[0], data[1], data[2], data[3]);
            let ext = a ^ na != 0xFF;
            row.insert(
                "addr".into(),
                json!(if ext { a as u32 | (na as u32) << 8 } else { a as u32 }),
            );
            row.insert("ext".into(), json!(ext));
            row.insert("cmd".into(), json!(c));
            row.insert("cmd_ok".into(), json!(c ^ nc == 0xFF));
            cmd_ok = Some(c ^ nc == 0xFF);
        }
        out.push(Frame {
            repeat: false,
            cmd_ok,
            row,
        });
        i = j + 1;
    }
    out
}

fn read(ch: &Channel, rate: f64, dg: i64, name: &str) -> Option<Candidate> {
    let (mk, carrier) = marks(ch, rate);
    let fr = frames(&mk, rate);
    let data = fr.iter().filter(|f| !f.repeat).count();
    let good = fr.iter().filter(|f| f.cmd_ok == Some(true)).count();
    if good == 0 {
        return None;
    }
    let reps = fr.len() - data;
    let score = good as f64 / data as f64 * q(2.0 * good as f64 + 0.5 * reps as f64, 1.0);
    let ev = obj(vec![
        ("frames", json!(data)),
        ("repeats", json!(reps)),
        ("cmd_ok", json!(good)),
        ("carrier_hz", json!(carrier)),
    ]);
    let params = obj(vec![("carrier_hz", json!(carrier)), ("idle", json!(ch.initial))]);
    let rows: Vec<Map<String, Value>> = fr.into_iter().map(|f| f.row).collect();
    Some(candidate(
        "ir_nec",
        &[("data", name)],
        score,
        params,
        ev,
        "frames",
        &rows,
        dg,
    ))
}

fn read_line(ch: &Channel, rate: f64, outer: i64) -> Option<Candidate> {
    let mut best: Option<Candidate> = None;
    for (v, dg) in views(ch.edges, ticks_k(SHORTEST, rate), false) {
        let c = Channel::new(ch.name, ch.initial, &v);
        if let Some(r) = read(&c, rate, if outer != 0 { outer } else { dg }, ch.name)
            && best.as_ref().is_none_or(|b| r.score > b.score)
        {
            best = Some(r);
        }
    }
    best
}

/// The IR NEC readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 4)
        .filter_map(|ch| best_of_line_views(ch, |c, k| read_line(c, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NEC frames on a receiver's output (idle high, active low) or as a 38 kHz carrier (idle low).
    fn encode(frames: &[(u8, u8)], rate: f64, carrier: bool) -> Vec<i64> {
        let us = rate / 1e6;
        let (mut e, mut t) = (vec![], 1000.0 * us);
        let mark = |len_us: f64, e: &mut Vec<i64>, t: &mut f64| {
            if carrier {
                let per = us * 1e6 / 38e3;
                let mut x = 0.0;
                while x + per / 3.0 < len_us * us {
                    e.extend([(*t + x).round() as i64, (*t + x + per / 3.0).round() as i64]);
                    x += per;
                }
            } else {
                e.extend([t.round() as i64, (*t + len_us * us).round() as i64]);
            }
            *t += len_us * us;
        };
        for &(a, c) in frames {
            mark(9000.0, &mut e, &mut t);
            t += 4500.0 * us;
            for b in [a, !a, c, !c] {
                for k in 0..8 {
                    mark(562.5, &mut e, &mut t);
                    t += if (b >> k) & 1 == 1 { 1687.5 } else { 562.5 } * us;
                }
            }
            mark(562.5, &mut e, &mut t);
            t += 40000.0 * us;
            mark(9000.0, &mut e, &mut t); // a repeat
            t += 2250.0 * us;
            mark(562.5, &mut e, &mut t);
            t += 40000.0 * us;
        }
        e
    }

    #[test]
    fn frames_with_their_inverse() {
        for carrier in [false, true] {
            let rate = 1e6;
            let e = encode(&[(0x04, 0x08), (0x04, 0x11)], rate, carrier);
            let n = e.last().unwrap() + 1000;
            let v = View::new(rate, n, vec![Channel::new("IR", if carrier { 0 } else { 1 }, &e)]).unwrap();
            let c = candidates(&v);
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].params["carrier_hz"].is_null(), !carrier);
            let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
            let f = lay["frames"].as_array().unwrap();
            assert_eq!(f.len(), 4);
            assert_eq!((f[2]["addr"].clone(), f[2]["cmd"].clone()), (json!(4), json!(0x11)));
            assert_eq!(f[3]["repeat"], true);
        }
    }
}
