//! Classic CAN on one line at the bus level (Python's `_engine/canbus.py`, read as a receiving node does, and the
//! main engine's `decl/can.toml`): a frame starts at a recessive-to-dominant edge after the bus was idle (11
//! recessive bits; 3 after a frame's end), each bit sampled at the sample point and resynchronized at each
//! recessive-to-dominant edge; stuffing after five equal bits from the start of frame to the CRC; the CRC-15; the
//! fixed-form bits recessive. Each distinct frame's CRC is the evidence.

use crate::Candidate;
use crate::features::features;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const CRC_POLY: u32 = 0x4599;
pub const SP: f64 = 0.75;
pub const MAX_NOMINAL: f64 = 1.05e6;
pub const FORBIDDEN_RUNS: f64 = 0.05;
pub const EVIDENCE_BITS: f64 = 8.0;

fn crc15(bits: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bits {
        let nxt = b as u32 ^ ((crc >> 14) & 1);
        crc = (crc << 1) & 0x7FFF;
        if nxt == 1 {
            crc ^= CRC_POLY;
        }
    }
    crc
}

struct Line<'a> {
    e: &'a [i64],
    initial: u8,
    idle: u8,
    falls: Vec<i64>,
    rises: Vec<i64>,
}

impl<'a> Line<'a> {
    fn new(ch: &'a Channel, idle: u8) -> Self {
        let (mut falls, mut rises) = (vec![], vec![]);
        for (k, &t) in ch.edges.iter().enumerate() {
            let after = ch.initial ^ (((k + 1) & 1) as u8);
            if after != idle { falls.push(t) } else { rises.push(t) }
        }
        Line {
            e: ch.edges,
            initial: ch.initial,
            idle,
            falls,
            rises,
        }
    }
    fn level(&self, t: f64) -> u8 {
        let k = self.e.partition_point(|&x| (x as f64) <= t);
        self.initial ^ (k as u8 & 1)
    }
    fn fall_between(&self, a: f64, b: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) <= a);
        (k < self.falls.len() && self.falls[k] as f64 <= b).then(|| self.falls[k])
    }
    fn next_fall(&self, t: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) < t);
        self.falls.get(k).copied()
    }
    fn recessive_since(&self, t: f64) -> f64 {
        let k = self.rises.partition_point(|&x| (x as f64) <= t);
        if k > 0 {
            self.rises[k - 1] as f64
        } else if self.initial == self.idle {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }
}

enum Stop {
    Error(f64),
    Cut,
}

/// One frame from its start of frame: (the frame, the tick its end of frame ends).
fn read_frame(line: &Line, sof: i64, u: f64, n: i64) -> Result<(Map<String, Value>, f64), Stop> {
    let mut edge = sof as f64;
    let mut prev = sof as f64;
    let mut k = 0usize;
    let mut raw = || -> Result<(u8, f64), Stop> {
        let nominal = edge + SP * u;
        if k > 0
            && let Some(f) = line.fall_between(prev, nominal)
        {
            edge = f as f64;
        }
        let t = edge + SP * u;
        if t >= n as f64 {
            return Err(Stop::Cut);
        }
        prev = t;
        edge += u;
        k += 1;
        Ok((if line.level(t) != line.idle { 0 } else { 1 }, t))
    };
    let (mut rv, mut rc) = (2u8, 0usize);
    let mut body: Vec<u8> = vec![];
    let mut stuffed = |raw: &mut dyn FnMut() -> Result<(u8, f64), Stop>, body: &mut Vec<u8>| -> Result<u8, Stop> {
        let (mut b, mut t) = raw()?;
        if rc == 5 {
            if b == rv {
                return Err(Stop::Error(t));
            }
            rv = b;
            rc = 1;
            let r = raw()?;
            b = r.0;
            t = r.1;
        }
        let _ = t;
        rc = if b == rv { rc + 1 } else { 1 };
        rv = b;
        body.push(b);
        Ok(b)
    };
    let mut bits =
        |m: usize, raw: &mut dyn FnMut() -> Result<(u8, f64), Stop>, body: &mut Vec<u8>| -> Result<u64, Stop> {
            let mut v = 0u64;
            for _ in 0..m {
                v = (v << 1) | stuffed(raw, body)? as u64;
            }
            Ok(v)
        };
    if bits(1, &mut raw, &mut body)? != 0 {
        return Err(Stop::Error(sof as f64 + SP * u));
    }
    let id_a = bits(11, &mut raw, &mut body)?;
    let rtr_srr = bits(1, &mut raw, &mut body)?;
    let ide = bits(1, &mut raw, &mut body)?;
    let (ident, remote) = if ide == 1 {
        let id_b = bits(18, &mut raw, &mut body)?;
        let rtr = bits(1, &mut raw, &mut body)?;
        bits(1, &mut raw, &mut body)?; // r1
        ((id_a << 18) | id_b, rtr)
    } else {
        (id_a, rtr_srr)
    };
    bits(1, &mut raw, &mut body)?; // r0
    let dlc = bits(4, &mut raw, &mut body)?;
    let nbytes = if remote == 1 { 0 } else { dlc.min(8) };
    let mut data = vec![];
    for _ in 0..nbytes {
        data.push(bits(8, &mut raw, &mut body)? as u8);
    }
    let covered = body.len();
    let crc = bits(15, &mut raw, &mut body)?;
    if rc == 5 {
        let (b, t) = raw()?;
        if b == rv {
            return Err(Stop::Error(t));
        }
    }
    if crc as u32 != crc15(&body[..covered]) {
        let (_, t) = raw()?;
        return Err(Stop::Error(t));
    }
    let fixed = |raw: &mut dyn FnMut() -> Result<(u8, f64), Stop>| -> Result<(), Stop> {
        let (b, t) = raw()?;
        if b != 1 { Err(Stop::Error(t)) } else { Ok(()) }
    };
    fixed(&mut raw)?; // CRC delimiter
    let (ack, _) = raw()?;
    fixed(&mut raw)?; // ACK delimiter
    for _ in 0..6 {
        fixed(&mut raw)?;
    }
    let (last, t) = raw()?;
    let mut f = Map::new();
    f.insert("s".into(), json!(sof));
    f.insert("e".into(), json!(t as i64));
    f.insert("id".into(), json!(ident));
    f.insert("ide".into(), json!(ide == 1));
    f.insert("rtr".into(), json!(remote == 1));
    f.insert("dlc".into(), json!(dlc));
    f.insert(
        "data".into(),
        json!(data.iter().map(|b| format!("{b:02x}")).collect::<String>()),
    );
    f.insert("ack".into(), json!(ack == 0));
    f.insert("ok".into(), json!(true));
    if last == 0 {
        f.insert("overload".into(), json!(t as i64));
    }
    Ok((f, edge))
}

/// The frames and the errors a node at bit time u (ticks) reads (Python's `canbus.read`, classic CAN).
pub fn read(ch: &Channel, n: i64, u: f64, idle: u8) -> (Vec<Map<String, Value>>, usize) {
    let line = Line::new(ch, idle);
    let (mut frames, mut errors) = (vec![], 0usize);
    let (mut need, mut after, mut pos) = (11.0, f64::NEG_INFINITY, 0.0f64);
    while let Some(f) = line.next_fall(pos) {
        let since = line.recessive_since(f as f64).max(after);
        if (f as f64) - since < (need - 0.5) * u {
            need = 11.0;
            pos = f as f64 + 1.0;
            continue;
        }
        match read_frame(&line, f, u, n) {
            Err(Stop::Cut) => break,
            Err(Stop::Error(t)) => {
                errors += 1;
                need = 11.0;
                after = 0.0;
                pos = t;
                if frames.is_empty() && errors >= 16 {
                    break; // a line of no CAN at this bit time
                }
            }
            Ok((fr, end)) => {
                if fr.contains_key("overload") {
                    need = 11.0;
                    after = 0.0;
                } else {
                    need = 3.0;
                    after = end;
                }
                pos = end;
                frames.push(fr);
            }
        }
    }
    (frames, errors)
}

/// The bit time from the intervals between recessive-to-dominant edges near whole bits of u0, least squares.
fn bit_time(ch: &Channel, u0: f64, idle: u8) -> Option<f64> {
    let line = Line::new(ch, idle);
    let d: Vec<f64> = line.falls.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.is_empty() {
        return None;
    }
    let mut u = u0;
    for tol in [0.3, 0.3, 0.3, 0.15] {
        let (mut num, mut den) = (0.0, 0.0);
        for &x in &d {
            let k = (x / u).round_ties_even();
            if (1.0..=10.0).contains(&k) && (x / u - k).abs() < tol {
                num += x;
                den += k;
            }
        }
        if den == 0.0 {
            return None;
        }
        u = num / den;
    }
    Some(u)
}

/// The CAN readings of a part: each line at its bit time (its units' candidates, refitted), under its idle level.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    for ch in &view.channels {
        if ch.edges.len() < 40 {
            continue;
        }
        let f = features(ch, view.n_samples);
        let (_, length, _) = crate::kernels::runs(ch, view.n_samples);
        let inner: Vec<f64> = if length.len() > 2 {
            length[1..length.len() - 1].iter().map(|&x| x as f64).collect()
        } else {
            vec![]
        };
        let mut best: Option<Candidate> = None;
        let mut tried: Vec<f64> = vec![];
        // the bit time from the shortest runs (a single bit is common on CAN: stuffing makes them) and from the
        // commonest one, beside the line's own units (which fractions of a bit can fit as well)
        let mut sorted = inner.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut bases: Vec<f64> = vec![];
        if !sorted.is_empty() {
            bases.push(crate::kernels::quantile(&sorted, 0.05));
            let mut hist = std::collections::BTreeMap::new();
            for &x in &sorted {
                *hist.entry(x.round() as i64).or_insert(0usize) += 1;
            }
            if let Some((&m, _)) = hist.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))) {
                bases.push(m as f64);
            }
        }
        bases.extend(f.units.iter().take(3).map(|u| u.samples));
        for base in bases {
            let idle = f.idle;
            if base < 2.0 {
                continue;
            }
            let Some(u) = bit_time(ch, base, idle) else {
                continue;
            };
            if u < 2.0 || view.rate / u > MAX_NOMINAL || tried.iter().any(|&v| (u / v - 1.0).abs() < 0.03) {
                continue;
            }
            tried.push(u);
            // stuffing keeps a frame's runs to 5 bits, the bus idle 11 or more: runs between in more than a few
            let forbidden = inner.iter().filter(|&&x| x > 5.5 * u && x < 10.5 * u).count() as f64;
            if !inner.is_empty() && forbidden / inner.len() as f64 > FORBIDDEN_RUNS {
                continue;
            }
            let (frames, errors) = read(ch, view.n_samples, u, idle);
            if frames.is_empty() {
                continue;
            }
            // the evidence: each distinct frame's CRC once (a frame sent again tells nothing new)
            let mut seen = std::collections::BTreeSet::new();
            for fr in &frames {
                seen.insert((fr["id"].to_string(), fr["data"].to_string()));
            }
            let info = 15.0 * seen.len() as f64;
            let datas: Vec<String> = frames.iter().map(|fr| fr["data"].to_string()).collect();
            let degenerate = {
                let mut c = std::collections::HashMap::new();
                for v in &datas {
                    *c.entry(v.clone()).or_insert(0usize) += 1;
                }
                let top = c.values().max().copied().unwrap_or(0) as f64 / datas.len().max(1) as f64;
                if datas.len() < 4 || top <= 0.5 {
                    1.0
                } else {
                    (1.0 - (top - 0.5) / 0.5).max(0.05)
                }
            };
            let score = frames.len() as f64 / (frames.len() + errors) as f64
                * (1.0 - 0.5f64.powf(info / EVIDENCE_BITS))
                * (0.5 + 0.5 * degenerate);
            if best.as_ref().is_some_and(|b| b.score >= score) {
                continue;
            }
            let mut params = Map::new();
            params.insert("bitrate".into(), json!(view.rate / u));
            params.insert("idle".into(), json!(idle));
            let mut evidence = Map::new();
            evidence.insert("frames".into(), json!(frames.len()));
            evidence.insert("errors".into(), json!(errors));
            evidence.insert("distinct".into(), json!(seen.len()));
            best = Some(Candidate {
                protocol: "can".into(),
                roles: [("data".to_string(), ch.name.to_string())].into(),
                score,
                alt: None,
                params,
                evidence,
                layers: crate::claim::raw(serde_json::to_string(&json!({ "frames": frames })).unwrap()),
            });
        }
        out.extend(best);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A classic frame's bits as sent: stuffed from the start of frame through the CRC, then the fixed tail.
    fn frame_bits(id: u32, data: &[u8], ack: bool) -> Vec<u8> {
        let mut b: Vec<u8> = vec![0];
        b.extend((0..11).rev().map(|k| ((id >> k) & 1) as u8));
        b.extend([0, 0, 0]); // RTR, IDE, r0
        b.extend((0..4).rev().map(|k| ((data.len() >> k) & 1) as u8));
        for &x in data {
            b.extend((0..8).rev().map(|k| (x >> k) & 1));
        }
        let crc = crc15(&b);
        b.extend((0..15).rev().map(|k| ((crc >> k) & 1) as u8));
        let mut s = vec![];
        let (mut v, mut c) = (2u8, 0);
        for &x in &b {
            s.push(x);
            c = if x == v { c + 1 } else { 1 };
            v = x;
            if c == 5 {
                s.push(1 - x);
                v = 1 - x;
                c = 1;
            }
        }
        s.push(1);
        s.push(if ack { 0 } else { 1 });
        s.extend([1; 7]);
        s.extend([1; 3]);
        s
    }

    #[test]
    fn reads_frames() {
        let msgs: [(u32, &[u8]); 4] = [
            (0x123, &[0x11, 0x22, 0x33]),
            (0x7FF, &[]),
            (0x055, &[0, 0, 0, 0, 0xFF]),
            (0x321, &[1]),
        ];
        let (mut e, mut lv, mut t) = (vec![], 1u8, 100i64);
        let bit = 8i64; // 250 kbit/s at 2 MS/s
        for _ in 0..3 {
            for (id, d) in msgs {
                for x in frame_bits(id, d, true) {
                    if x != lv {
                        e.push(t);
                        lv = x;
                    }
                    t += bit;
                }
                t += 20 * bit;
            }
        }
        let v = View::new(2e6, t + 100, vec![Channel::new("CAN", 1, &e)]).unwrap();
        let c = candidates(&v).into_iter().next().expect("a CAN reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["frames"].as_array().unwrap().len(), 12, "{:?}", c.evidence);
        assert_eq!(lay["frames"][0]["id"], json!(0x123));
        assert_eq!(lay["frames"][2]["data"], json!("00000000ff"));
        assert!((c.params["bitrate"].as_f64().unwrap() / 250e3 - 1.0).abs() < 0.01);
        assert!(c.score > 0.9, "{:?}", c.evidence);
    }
}
