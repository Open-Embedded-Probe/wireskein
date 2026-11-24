//! HDMI CEC on one line idling high (Python's `_engine/cec.py`): a start bit (3.7 ms low of 4.5 ms), then blocks of
//! 10 bits, each a low pulse in 2.4 ms (0.6 ms: 1, 1.5 ms: 0): 8 data bits MSB first, EOM, ACK. Its timing is its
//! own: a UART reading of the same line (each bit a 6-bit character) is its rival, and these tell the two apart.

use crate::Candidate;
use crate::kernels::runs;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const START_LOW: (f64, f64) = (3.5e-3, 3.9e-3);
const LOW_1: (f64, f64) = (0.4e-3, 0.8e-3);
const LOW_0: (f64, f64) = (1.3e-3, 1.7e-3);
const BIT_GAP: (f64, f64) = (2.05e-3, 2.75e-3);
const START_GAP: (f64, f64) = (4.3e-3, 4.7e-3);
const EVIDENCE_BITS: f64 = 8.0;

fn within(x: f64, r: (f64, f64)) -> bool {
    r.0 <= x && x <= r.1
}

/// The messages on a line: [{s, e, initiator, destination, opcode?, data?, acked, complete}].
pub fn messages(ch: &Channel, n: i64, rate: f64) -> Vec<Map<String, Value>> {
    let (st, length, level) = runs(ch, n);
    let k = st.len();
    let lows: Vec<(f64, f64)> = (1..k.saturating_sub(1))
        .filter(|&i| level[i] == 0)
        .map(|i| (st[i] as f64 / rate, length[i] as f64 / rate))
        .collect();
    let mut out = vec![];
    let mut i = 0usize;
    while i < lows.len() {
        if !within(lows[i].1, START_LOW) {
            i += 1;
            continue;
        }
        let s0 = lows[i].0;
        i += 1;
        let (mut bits, mut blocks, mut acks): (Vec<u8>, Vec<u8>, Vec<u8>) = (vec![], vec![], vec![]);
        let (mut first, mut eom, mut e) = (true, false, 0i64);
        while i < lows.len() {
            let gap = lows[i].0 - lows[i - 1].0;
            if !within(gap, if first { START_GAP } else { BIT_GAP }) {
                break;
            }
            let w = lows[i].1;
            if within(w, LOW_1) {
                bits.push(1);
            } else if within(w, LOW_0) {
                bits.push(0);
            } else {
                break;
            }
            first = false;
            i += 1;
            if bits.len() == 10 {
                blocks.push(bits[..8].iter().fold(0u8, |a, &b| (a << 1) | b));
                acks.push(bits[9]);
                e = ((lows[i - 1].0 + lows[i - 1].1) * rate).round() as i64;
                eom = bits[8] == 1;
                bits.clear();
                if eom {
                    break;
                }
            }
        }
        if blocks.is_empty() {
            continue;
        }
        let dest = blocks[0] & 0xF;
        let broadcast = dest == 0xF;
        let mut m = Map::new();
        m.insert("s".into(), json!((s0 * rate).round() as i64));
        m.insert("e".into(), json!(e));
        m.insert("initiator".into(), json!(blocks[0] >> 4));
        m.insert("destination".into(), json!(dest));
        if blocks.len() > 1 {
            m.insert("opcode".into(), json!(blocks[1]));
            let d: String = blocks[2..].iter().map(|b| format!("{b:02x}")).collect();
            m.insert("data".into(), json!(d));
        }
        m.insert(
            "acked".into(),
            json!(acks.iter().all(|&a| if broadcast { a == 1 } else { a == 0 })),
        );
        m.insert("complete".into(), json!(eom));
        m.insert("blocks".into(), json!(blocks.len()));
        out.push(m);
    }
    out
}

/// The share of the line's low pulses inside complete CEC messages (0 with no start bit's low).
pub fn covered(ch: &Channel, n: i64, rate: f64) -> f64 {
    let (st, length, level) = runs(ch, n);
    let lows: Vec<(i64, f64)> = (0..st.len())
        .filter(|&i| level[i] == 0)
        .map(|i| (st[i], length[i] as f64 / rate))
        .collect();
    if !lows.iter().any(|&(_, w)| within(w, START_LOW)) {
        return 0.0;
    }
    let msgs: Vec<(i64, i64)> = messages(ch, n, rate)
        .iter()
        .filter(|m| m["complete"] == json!(true))
        .map(|m| (m["s"].as_i64().unwrap(), m["e"].as_i64().unwrap()))
        .collect();
    if msgs.is_empty() || lows.is_empty() {
        return 0.0;
    }
    let inside = lows
        .iter()
        .filter(|&&(s, _)| msgs.iter().any(|&(a, b)| s >= a && s < b))
        .count();
    inside as f64 / lows.len() as f64
}

/// The CEC reading of one line as it is given, or None.
fn read(ch: &Channel, n: i64, rate: f64, k: i64) -> Option<Candidate> {
    let mut msgs = messages(ch, n, rate);
    let done = msgs.iter().filter(|m| m["complete"] == json!(true)).count();
    if done < 2 {
        return None;
    }
    let info: f64 = msgs
        .iter()
        .filter(|m| m["complete"] == json!(true))
        .map(|m| {
            let b = m["blocks"].as_u64().unwrap_or(1) as f64;
            4.0 + if m["acked"] == json!(true) { 3.0 } else { 2.0 } * b
        })
        .sum();
    let score = done as f64 / msgs.len() as f64 * (1.0 - 0.5f64.powf(info / EVIDENCE_BITS));
    for m in &mut msgs {
        m.remove("blocks");
    }
    let mut evidence = Map::new();
    evidence.insert("messages".into(), json!(msgs.len()));
    evidence.insert("complete".into(), json!(done));
    let mut params = Map::new();
    params.insert("deglitch".into(), json!(k));
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "messages": msgs })).unwrap());
    Some(Candidate {
        protocol: "cec".into(),
        roles: [("cec".to_string(), ch.name.to_string())].into(),
        score,
        alt: None,
        params,
        evidence,
        layers,
    })
}

pub const NOISE_S: f64 = 50e-6; // pulses shorter than this are no CEC bit's (its lows are 0.4 ms or more)

/// The CEC readings of a part: each line with complete messages, read as it is, with pulses under NOISE_S taken out,
/// and with bounces merged; the best of the three.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    if view.rate < 20e3 {
        return out;
    }
    let k = ((NOISE_S * view.rate) as i64).max(2);
    for ch in &view.channels {
        if ch.edges.len() < 20 {
            continue;
        }
        let views = [
            (ch.edges.to_vec(), 0),
            (crate::kernels::deglitch(ch.edges, k), k),
            (crate::kernels::debounce(ch.edges, k), -k),
        ];
        let mut best: Option<Candidate> = None;
        for (e, kk) in views {
            let c = Channel::new(ch.name, ch.initial, &e);
            if let Some(r) = read(&c, view.n_samples, view.rate, kk)
                && best.as_ref().is_none_or(|b| r.score > b.score)
            {
                best = Some(r);
            }
        }
        out.extend(best);
    }
    out
}
