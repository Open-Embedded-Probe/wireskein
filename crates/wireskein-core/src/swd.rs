//! ARM SWD (Python's `decl/swd.toml`): SWCLK and SWDIO, SWDIO read at SWCLK's rises; packets found anywhere in a
//! burst: a request (start 1, APnDP, RnW, A[2:3], even parity, stop 0, park 1), a turnaround, an ACK (OK, WAIT,
//! FAULT), and on OK 32 data bits with their even parity (a turnaround around them as the direction asks).

use crate::Candidate;
use crate::kernels::{deglitch, median};
use crate::sync::{bursts, edges_to};
use crate::view::{Channel, View};
use serde_json::{Value, json};

pub const EVIDENCE_BITS: f64 = 8.0;

fn lsb(b: &[u8]) -> u64 {
    b.iter().enumerate().fold(0u64, |a, (i, &x)| a | ((x as u64) << i))
}

/// The packets in one burst's bits: (packets, bits of evidence their checks hold, bits they cover).
fn packets(bits: &[u8], t: &[i64]) -> (Vec<Value>, f64, usize) {
    let (mut out, mut info, mut covered) = (vec![], 0.0, 0usize);
    let mut i = 0usize;
    while i + 12 <= bits.len() {
        let h = &bits[i..i + 8];
        let par = (h[1] + h[2] + h[3] + h[4]) & 1;
        if !(h[0] == 1 && h[6] == 0 && h[7] == 1 && h[5] == par) {
            i += 1;
            continue;
        }
        let ack = lsb(&bits[i + 9..i + 12]);
        if ![1, 2, 4].contains(&ack) {
            i += 1;
            continue;
        }
        let (apndp, rnw, a) = (h[1], h[2], lsb(&h[3..5]));
        let mut j = i + 12;
        let mut data = Value::Null;
        // the request's start, stop, park and parity, the ACK's code: 4 + 2 bits
        info += 6.0;
        if ack == 1 {
            if rnw == 0 {
                j += 1; // the turnaround back to the host before its data
            }
            if j + 33 > bits.len() {
                break;
            }
            let d = lsb(&bits[j..j + 32]);
            let dp = bits[j + 32];
            if (d.count_ones() as u8 & 1) != dp {
                i += 1;
                info -= 6.0;
                continue;
            }
            info += 1.0;
            data = json!(d);
            j += 33 + rnw as usize;
        } else {
            j += 1;
        }
        out.push(
            json!({"s": t[i], "e": t[(j - 1).min(t.len() - 1)], "apndp": if apndp == 1 { "AP" } else { "DP" },
                        "rnw": if rnw == 1 { "R" } else { "W" }, "a": a,
                        "ack": match ack { 1 => "OK", 2 => "WAIT", _ => "FAULT" }, "data": data}),
        );
        covered += j - i;
        i = j;
    }
    (out, info, covered)
}

/// The bits of a burst that carry something: those outside runs of 8 or more equal bits (the host's idle clocks, a
/// line reset's ones).
fn active(bits: &[u8]) -> usize {
    let mut n = 0usize;
    let mut i = 0usize;
    while i < bits.len() {
        let mut j = i;
        while j < bits.len() && bits[j] == bits[i] {
            j += 1;
        }
        if j - i < 8 {
            n += j - i;
        }
        i = j;
    }
    n
}

fn read(view: &View, clk: &Channel, dio: &Channel) -> Option<Candidate> {
    if clk.edges.len() < 24 {
        return None;
    }
    let d: Vec<f64> = clk.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let half = median(&d);
    let k = ((0.3 * half) as i64).max(2);
    let ce = deglitch(clk.edges, k);
    let de = deglitch(dio.edges, k);
    let cc = Channel::new(clk.name, clk.initial, &ce);
    let dc = Channel::new(dio.name, dio.initial, &de);
    if crate::sync::keeping(&de, &cc) < 0.5 {
        return None;
    }
    let rises = edges_to(&cc, 1);
    // SWDIO at each rise as the host latches it: where the target drives its bits on the rise itself (its changes
    // on the rise's own sample, a share of them), the level before (`clocked::sample`, Python's sync_bits)
    let sampled = crate::clocked::sample(dc.initial, &de, &rises, &edges_to(&cc, 0));
    let (mut all, mut info, mut clocked, mut covered, mut busy) = (vec![], 0.0, 0usize, 0usize, 0usize);
    for (a, b) in bursts(&ce) {
        let lo = rises.partition_point(|&x| x < a);
        let hi = rises.partition_point(|&x| x <= b);
        let t = &rises[lo..hi];
        clocked += t.len();
        let bits: Vec<u8> = sampled[lo..hi].to_vec();
        let (p, i, c) = packets(&bits, t);
        all.extend(p);
        info += i;
        covered += c;
        busy += active(&bits);
    }
    if all.len() < 2 {
        return None;
    }
    let datas: Vec<u64> = all.iter().filter_map(|p| p["data"].as_u64()).collect();
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
    // a bus of SWD is its packets (and idle clocks, line resets): a long burst of other bits holds a few packets by
    // chance anywhere it is searched, and leaves most of its bits to no packet
    let coverage = (covered as f64 / busy.max(1) as f64).min(1.0);
    let score = (1.0 - 0.5f64.powf(info / EVIDENCE_BITS)) * (0.5 + 0.5 * degenerate) * coverage;
    let mut params = serde_json::Map::new();
    params.insert("clock_hz".into(), json!(view.rate / (2.0 * half)));
    let mut evidence = serde_json::Map::new();
    evidence.insert("packets".into(), json!(all.len()));
    evidence.insert("clocks".into(), json!(clocked));
    evidence.insert("coverage".into(), json!(coverage));
    evidence.insert("degenerate".into(), json!(degenerate));
    Some(Candidate {
        protocol: "swd".into(),
        roles: [
            ("swclk".to_string(), clk.name.to_string()),
            ("swdio".to_string(), dio.name.to_string()),
        ]
        .into(),
        score,
        alt: None,
        params,
        evidence,
        layers: crate::claim::raw(serde_json::to_string(&json!({ "packets": all })).unwrap()),
    })
}

/// The SWD readings of a part: each clock of 24 changes or more with each other line keeping to it.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    // each line as it is and with its pulses of a tick out (crosstalk, spikes: a clock's median half period taken
    // from them otherwise), each pair read on both, the better kept
    let quiet: Vec<Vec<i64>> = view.channels.iter().map(|c| deglitch(c.edges, 2)).collect();
    for (i, c) in view.channels.iter().enumerate() {
        for (j, d) in view.channels.iter().enumerate() {
            if c.name == d.name {
                continue;
            }
            // (the clock the line changing more, as it is or as it settles)
            let mut best = if d.edges.len() >= 4 && d.edges.len() <= c.edges.len() {
                read(view, c, d)
            } else {
                None
            };
            if (quiet[i].len() != c.edges.len() || quiet[j].len() != d.edges.len())
                && quiet[j].len() >= 4
                && quiet[j].len() <= quiet[i].len()
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
    fn reads_a_read_and_a_write() {
        // bits change after SWCLK's fall, read at its rise; 20 ticks a half period
        let mut bits: Vec<u8> = vec![];
        for (apndp, rnw, a, data) in [(0u8, 1u8, 0u8, 0x2BA0_1477u32), (1, 0, 1, 0x1234_5678)] {
            let par = (apndp + rnw + (a & 1) + ((a >> 1) & 1)) & 1;
            bits.extend([1, apndp, rnw, a & 1, (a >> 1) & 1, par, 0, 1]);
            bits.push(1); // turnaround
            bits.extend([1, 0, 0]); // ACK OK
            if rnw == 0 {
                bits.push(1);
            }
            bits.extend((0..32).map(|i| ((data >> i) & 1) as u8));
            bits.push((data.count_ones() & 1) as u8);
            if rnw == 1 {
                bits.push(1);
            }
            bits.extend([0; 8]);
        }
        let (mut clk, mut dio, mut t, mut ld) = (vec![], vec![], 100i64, 0u8);
        for &x in &bits {
            if x != ld {
                dio.push(t + 2);
                ld = x;
            }
            clk.push(t + 20);
            clk.push(t + 40);
            t += 40;
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![Channel::new("SWCLK", 0, &clk), Channel::new("SWDIO", 0, &dio)],
        )
        .unwrap();
        let c = candidates(&v)
            .into_iter()
            .find(|c| c.roles["swclk"] == "SWCLK")
            .expect("an SWD reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["packets"][0]["data"], json!(0x2BA0_1477u32));
        assert_eq!(lay["packets"][1]["apndp"], json!("AP"));
        assert_eq!(lay["packets"][1]["data"], json!(0x1234_5678u32));
    }
}
