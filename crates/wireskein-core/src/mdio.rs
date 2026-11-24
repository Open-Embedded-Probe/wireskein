//! MDIO (IEEE 802.3 clause 22 and 45 management frames, Python's `decl/mdio.toml`): MDC a clock in bursts, MDIO
//! sampled at its rises; each frame 32 ones (the preamble), a start (01, or 00 for clause 45), an operation, the
//! PHY (port) address, the register (device), the turnaround, 16 bits of data, MSB first. The preamble and the start
//! are 34 bits a line of other data rarely shows: the frames found are the evidence.

use crate::Candidate;
use crate::kernels::{deglitch, median};
use crate::sync::{bursts, edges_to};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const DATA_KEEPS: f64 = 0.5; // MDIO keeps to MDC this well at least
pub const SCALE: f64 = 2.0; // frames counted as 1 - exp(-n / SCALE): each holds 34 fixed bits

fn bits_msb(b: &[u8]) -> u32 {
    b.iter().fold(0u32, |a, &x| (a << 1) | x as u32)
}

/// The frames in one burst's bits: (index of the frame's first bit, the frame as a JSON object).
fn frames_in(bits: &[u8], ticks: &[i64]) -> Vec<Value> {
    let mut out = vec![];
    let mut i = 0usize;
    while i + 64 <= bits.len() {
        if bits[i..i + 32].iter().all(|&b| b == 1) {
            // the preamble may be longer than 32 ones: the start is the first 0 after it
            let mut j = i + 32;
            while j < bits.len() && bits[j] == 1 {
                j += 1;
            }
            // the start's first bit is the first 0 after the ones (ST = 01 or 00)
            let st0 = j;
            if st0 + 32 > bits.len() {
                break;
            }
            let f = &bits[st0..st0 + 32];
            let st = bits_msb(&f[0..2]);
            let op = bits_msb(&f[2..4]);
            let phy = bits_msb(&f[4..9]);
            let reg = bits_msb(&f[9..14]);
            let ta2 = f[15];
            let data = bits_msb(&f[16..32]);
            // after a frame the line is released (the next preamble's ones, or the burst's end): a frame of another
            // bus that starts alike (an SD command's 01 after an idle line) goes on with its own bits
            let after = &bits[st0 + 32..(st0 + 40).min(bits.len())];
            let released = after.iter().all(|&b| b == 1);
            let ok22 = st == 1 && (op == 1 || op == 2) && ta2 == 0 && released;
            let ok45 = st == 0 && ta2 == 0 && released;
            if ok22 || ok45 {
                let name = if st == 1 {
                    ["?", "W", "R", "?"][op as usize]
                } else {
                    ["ADDR", "W", "RINC", "R"][op as usize]
                };
                let mut m = Map::new();
                m.insert("s".into(), json!(ticks[i]));
                m.insert("e".into(), json!(ticks[st0 + 31]));
                m.insert("clause".into(), json!(if st == 1 { 22 } else { 45 }));
                m.insert("op".into(), json!(name));
                m.insert("phy".into(), json!(phy));
                m.insert(if st == 1 { "reg" } else { "dev" }.into(), json!(reg));
                m.insert("data".into(), json!(data));
                out.push(Value::Object(m));
                i = st0 + 32;
                continue;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

/// The MDIO reading of (mdc, mdio), or None.
fn read(view: &View, mdc: &Channel, mdio: &Channel) -> Option<Candidate> {
    let d: Vec<f64> = mdc.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.len() < 64 {
        return None;
    }
    let half = median(&d);
    let k = ((0.3 * half) as i64).max(2);
    let ce = deglitch(mdc.edges, k);
    let de = deglitch(mdio.edges, k);
    let cc = Channel::new(mdc.name, mdc.initial, &ce);
    let dc = Channel::new(mdio.name, mdio.initial, &de);
    // MDIO keeps to MDC as its data: a line mostly idling high meets the preamble's ones by itself
    let keeps = crate::sync::keeping(&de, &cc);
    if keeps < DATA_KEEPS {
        return None;
    }
    let rises = edges_to(&cc, 1);
    let mut frames = vec![];
    let mut windows = 0usize;
    for (a, b) in bursts(&ce) {
        let lo = rises.partition_point(|&x| x < a);
        let hi = rises.partition_point(|&x| x <= b);
        if hi - lo < 64 {
            continue;
        }
        windows += 1;
        let t = &rises[lo..hi];
        let bits: Vec<u8> = t.iter().map(|&x| dc.level_at(x)).collect();
        frames.extend(frames_in(&bits, t));
    }
    if frames.is_empty() {
        return None;
    }
    let fit = (frames.len() as f64 / windows.max(1) as f64).min(1.0);
    let quantity = 1.0 - (-(frames.len() as f64) / SCALE).exp();
    let datas: Vec<i64> = frames.iter().filter_map(|f| f["data"].as_i64()).collect();
    let degenerate = {
        let mut c = std::collections::HashMap::new();
        for &v in &datas {
            *c.entry(v).or_insert(0usize) += 1;
        }
        let top = c.values().max().copied().unwrap_or(0) as f64 / datas.len().max(1) as f64;
        if datas.len() < 4 || top <= 0.5 {
            1.0
        } else {
            (1.0 - (top - 0.5) / 0.5).max(0.05)
        }
    };
    let score = fit * quantity * (0.5 + 0.5 * degenerate);
    let mut evidence = Map::new();
    evidence.insert("frames".into(), json!(frames.len()));
    evidence.insert("bursts".into(), json!(windows));
    evidence.insert("degenerate".into(), json!(degenerate));
    evidence.insert("keeps_to_mdc".into(), json!(keeps));
    let mut params = Map::new();
    params.insert("clock_hz".into(), json!(view.rate / (2.0 * half)));
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "frames": frames })).unwrap());
    Some(Candidate {
        protocol: "mdio".into(),
        roles: [
            ("mdc".to_string(), mdc.name.to_string()),
            ("mdio".to_string(), mdio.name.to_string()),
        ]
        .into(),
        score,
        alt: None,
        params,
        evidence,
        layers,
    })
}

/// The MDIO readings of a part: each line of 64 changes or more as MDC, each other line as MDIO.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    let act: Vec<&Channel> = view.channels.iter().filter(|c| c.edges.len() >= 2).collect();
    for c in &act {
        if c.edges.len() < 64 {
            continue;
        }
        for d in &act {
            if d.name != c.name && d.edges.len() <= c.edges.len() {
                out.extend(read(view, c, d));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_clause_22() {
        // two frames: write phy 1 reg 0 data 0x1140, read phy 1 reg 2 data 0x0141; MDC period 20, MDIO set on falls
        let mut bits: Vec<Vec<u8>> = vec![];
        for (op, phy, reg, data) in [(1u32, 1u32, 0u32, 0x1140u32), (2, 1, 2, 0x0141)] {
            let mut b = vec![1u8; 32];
            b.extend([0, 1]);
            b.extend((0..2).rev().map(|k| ((op >> k) & 1) as u8));
            b.extend((0..5).rev().map(|k| ((phy >> k) & 1) as u8));
            b.extend((0..5).rev().map(|k| ((reg >> k) & 1) as u8));
            b.extend([1, 0]);
            b.extend((0..16).rev().map(|k| ((data >> k) & 1) as u8));
            bits.push(b);
        }
        let (mut mdc, mut mdio, mut t, mut ld) = (vec![], vec![], 100i64, 1u8);
        for b in &bits {
            for &x in b {
                if x != ld {
                    mdio.push(t);
                    ld = x;
                }
                t += 10;
                mdc.push(t);
                t += 10;
                mdc.push(t);
            }
            t += 500;
        }
        if ld != 1 {
            mdio.push(t);
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![Channel::new("MDC", 0, &mdc), Channel::new("MDIO", 1, &mdio)],
        )
        .unwrap();
        let c = candidates(&v)
            .into_iter()
            .find(|c| c.roles["mdc"] == "MDC")
            .expect("an MDIO reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["frames"][0]["op"], json!("W"));
        assert_eq!(lay["frames"][1]["data"], json!(0x0141));
        assert!(c.score > 0.6, "{:?}", c.evidence);
    }
}
