//! PS/2 (Python's `decl/ps2.toml`): CLK and DATA idling high, CLK in bursts at 10-16.7 kHz, a frame a burst read at
//! CLK's falls: a start 0, 8 data bits LSB first, odd parity, a stop 1 (and the host's ACK 0 after a byte it sent).
//! The start, the stop and the parity are the evidence.

use crate::Candidate;
use crate::kernels::{deglitch, median};
use crate::sync::{bursts, edges_to};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const MIN_HZ: f64 = 5000.0;
pub const MAX_HZ: f64 = 30000.0;
pub const EVIDENCE_BITS: f64 = 8.0;

fn read(view: &View, clk: &Channel, dat: &Channel) -> Option<Candidate> {
    if clk.edges.len() < 22 {
        return None;
    }
    let d: Vec<f64> = clk.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let half = median(&d);
    let hz = view.rate / (2.0 * half);
    if !(MIN_HZ..=MAX_HZ).contains(&hz) {
        return None;
    }
    let k = ((0.3 * half) as i64).max(2);
    let ce = deglitch(clk.edges, k);
    let de = deglitch(dat.edges, k);
    let cc = Channel::new(clk.name, clk.initial, &ce);
    let dc = Channel::new(dat.name, dat.initial, &de);
    let falls = edges_to(&cc, 0);
    let (mut frames, mut n, mut checks, mut fails) = (vec![], 0usize, 0usize, 0usize);
    for (a, b) in bursts(&ce) {
        let lo = falls.partition_point(|&x| x < a);
        let hi = falls.partition_point(|&x| x <= b);
        let t = &falls[lo..hi];
        if t.is_empty() {
            continue;
        }
        n += 1;
        if t.len() != 11 && t.len() != 12 {
            fails += 1;
            continue;
        }
        let bits: Vec<u8> = t.iter().map(|&x| dc.level_at(x)).collect();
        let data = (0..8).fold(0u8, |acc, i| acc | (bits[1 + i] << i));
        let par = (data.count_ones() as u8 + bits[9]) & 1 == 1;
        let good = [bits[0] == 0, bits[10] == 1, par];
        let ok = good.iter().all(|&g| g);
        // the burst's length (11 or 12 clocks) is a check too
        checks += 1 + good.iter().filter(|&&g| g).count();
        fails += good.iter().filter(|&&g| !g).count();
        let mut f = Map::new();
        f.insert("s".into(), json!(t[0]));
        f.insert("e".into(), json!(t[t.len() - 1]));
        f.insert("data".into(), json!(data));
        f.insert("ok".into(), json!(ok));
        if t.len() == 12 {
            f.insert("ack".into(), json!(bits[11] == 0));
        }
        frames.push(Value::Object(f));
    }
    if frames.is_empty() || n < 2 {
        return None;
    }
    let good = frames.iter().filter(|f| f["ok"] == json!(true)).count();
    let fit = good as f64 / n as f64;
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
    // the evidence in bits: a burst of 11 or 12 clocks, a start, a stop and the parity, each one in two by chance
    let info = checks as f64 - fails as f64;
    let score = fit * (1.0 - 0.5f64.powf(info.max(0.0) / EVIDENCE_BITS)) * (0.5 + 0.5 * degenerate);
    let mut params = Map::new();
    params.insert("clock_hz".into(), json!(hz));
    let mut evidence = Map::new();
    evidence.insert("frames".into(), json!(frames.len()));
    evidence.insert("bursts".into(), json!(n));
    evidence.insert("degenerate".into(), json!(degenerate));
    Some(Candidate {
        protocol: "ps2".into(),
        roles: [
            ("clk".to_string(), clk.name.to_string()),
            ("data".to_string(), dat.name.to_string()),
        ]
        .into(),
        score,
        alt: None,
        params,
        evidence,
        layers: crate::claim::raw(serde_json::to_string(&json!({ "frames": frames })).unwrap()),
    })
}

/// The PS/2 readings of a part: two lines idling high, the clock changing more.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    let act: Vec<&Channel> = view
        .channels
        .iter()
        .filter(|c| c.edges.len() >= 4 && c.initial == 1)
        .collect();
    // each line as it is and with its pulses of a tick out (crosstalk, spikes: a clock's median half period and its
    // rate taken from them otherwise), each pair read on both, the better kept
    let quiet: Vec<Vec<i64>> = act.iter().map(|c| deglitch(c.edges, 2)).collect();
    for (i, c) in act.iter().enumerate() {
        for (j, d) in act.iter().enumerate() {
            if c.name == d.name {
                continue;
            }
            // (the clock the line changing more, as it is or as it settles)
            let mut best = if c.edges.len() >= d.edges.len() {
                read(view, c, d)
            } else {
                None
            };
            if (quiet[i].len() != c.edges.len() || quiet[j].len() != d.edges.len()) && quiet[i].len() >= quiet[j].len()
            {
                let (qc, qd) = (
                    Channel::new(c.name, c.initial, &quiet[i]),
                    Channel::new(d.name, d.initial, &quiet[j]),
                );
                if let Some(q) = read(view, &qc, &qd)
                    && best.as_ref().is_none_or(|b| q.score > b.score)
                {
                    best = Some(q);
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

    #[test]
    fn reads_scan_codes() {
        // 12.5 kHz at 1 MS/s: 40 ticks a half period; data set while CLK is high, read on its fall
        let codes = [0x1Cu8, 0xF0, 0x1C, 0x32, 0xF0, 0x32];
        let (mut clk, mut dat, mut t, mut ld) = (vec![], vec![], 100i64, 1u8);
        for &b in &codes {
            let mut bits = vec![0u8];
            bits.extend((0..8).map(|i| (b >> i) & 1));
            bits.push(1 - (b.count_ones() as u8 & 1));
            bits.push(1);
            for x in bits {
                if x != ld {
                    dat.push(t);
                    ld = x;
                }
                clk.push(t + 20);
                clk.push(t + 60);
                t += 80;
            }
            if ld != 1 {
                dat.push(t);
                ld = 1;
            }
            t += 2000;
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![Channel::new("CLK", 1, &clk), Channel::new("DATA", 1, &dat)],
        )
        .unwrap();
        let c = candidates(&v)
            .into_iter()
            .find(|c| c.roles["clk"] == "CLK")
            .expect("a PS/2 reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        let got: Vec<i64> = lay["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["data"].as_i64().unwrap())
            .collect();
        assert_eq!(got, codes.iter().map(|&b| b as i64).collect::<Vec<_>>());
        assert!(c.score > 0.8, "{:?}", c.evidence);
    }
}
