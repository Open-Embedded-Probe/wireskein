//! A DHT11 / DHT22 (AM2302) humidity sensor's one line (Python's `analyzers/pulse.dht_frames` and `DhtPlugin`):
//! the host's long low, the sensor's answer (a low and a high of about 80 us), then 40 bits each a low of 50 us and
//! a high of 26-28 us (0) or 70 us (1); the fifth byte the sum of the four.

use crate::Candidate;
use crate::kernels::runs;
use crate::pulse::{best_of_views, candidate, hex, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const START: f64 = 0.5e-3; // the host's start, a low this long or more
const RESPONSE: (f64, f64) = (40e-6, 110e-6); // the sensor's answer, a low and a high each
const BIT_LOW: (f64, f64) = (30e-6, 80e-6); // each bit's low
const BIT_SPLIT: f64 = 48e-6; // a bit's high under this is 0, over it 1
const BIT_HIGH: f64 = 95e-6; // no bit's high is longer

struct Reading {
    s: i64,
    b: [u8; 5],
    ok: bool,
}

fn readings(ch: &Channel, n: i64, rate: f64) -> Vec<Reading> {
    let (start, length, lv) = runs(ch, n);
    let dur: Vec<f64> = length.iter().map(|&l| l as f64 / rate).collect();
    let resp = |d: f64| (RESPONSE.0..=RESPONSE.1).contains(&d);
    let (mut out, mut i, m) = (vec![], 0usize, dur.len());
    while i < m {
        if !(lv[i] == 0 && dur[i] >= START) {
            i += 1;
            continue;
        }
        let j = i + 2; // past the host's release
        if j + 1 >= m || !(resp(dur[j]) && resp(dur[j + 1])) {
            i += 1;
            continue;
        }
        let (mut k, mut bits) = (j + 2, vec![]);
        while bits.len() < 40
            && k + 1 < m
            && lv[k] == 0
            && (BIT_LOW.0..=BIT_LOW.1).contains(&dur[k])
            && dur[k + 1] <= BIT_HIGH
        {
            bits.push((dur[k + 1] > BIT_SPLIT) as u8);
            k += 2;
        }
        if bits.len() == 40 {
            let mut b = [0u8; 5];
            for (x, v) in b.iter_mut().enumerate() {
                *v = bits[8 * x..8 * x + 8].iter().fold(0u8, |a, &y| (a << 1) | y);
            }
            let ok = b[..4].iter().fold(0u8, |a, &y| a.wrapping_add(y)) == b[4];
            out.push(Reading { s: start[i], b, ok });
        }
        i = k;
    }
    out
}

fn read(ch: &Channel, n: i64, rate: f64, k: i64) -> Option<Candidate> {
    let rd = readings(ch, n, rate);
    let ok = rd.iter().filter(|r| r.ok).count();
    if ok == 0 {
        return None;
    }
    // the model the readings fit: DHT11 (whole humidity and temperature) or DHT22 (tenths)
    let dht11 = rd
        .iter()
        .filter(|r| r.ok)
        .all(|r| r.b[1] == 0 && r.b[0] <= 100 && r.b[3] < 10);
    let rows: Vec<Map<String, Value>> = rd
        .iter()
        .map(|r| {
            let b = r.b;
            let (rh, t) = if dht11 {
                let sign = if b[2] & 0x80 != 0 { -1.0 } else { 1.0 };
                (b[0] as f64, b[2] as f64 + b[3] as f64 / 10.0 * sign)
            } else {
                let raw = (((b[2] & 0x7F) as u32) << 8 | b[3] as u32) as f64;
                (
                    ((b[0] as u32) << 8 | b[1] as u32) as f64 / 10.0,
                    if b[2] & 0x80 != 0 { -raw } else { raw } / 10.0,
                )
            };
            obj(vec![
                ("s", json!(r.s)),
                ("bytes", json!(hex(&b))),
                ("checksum_ok", json!(r.ok)),
                ("rh", json!(rh)),
                ("t", json!(t)),
            ])
        })
        .collect();
    // each reading's checksum held (8) and its start and answer of their lengths (8); the share that holds
    let score = ok as f64 / rd.len() as f64 * (1.0 - 0.5f64.powf(16.0 * ok as f64 / 8.0));
    let ev = obj(vec![("readings", json!(rd.len())), ("checksum_ok", json!(ok))]);
    Some(candidate(
        "dht",
        &[("data", ch.name)],
        score,
        obj(vec![("model", json!(if dht11 { "dht11" } else { "dht22" }))]),
        ev,
        "readings",
        &rows,
        k,
    ))
}

/// The DHT readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    if view.rate < 50e3 {
        return vec![];
    }
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 84)
        .filter_map(|ch| best_of_views(ch, |c, k| read(c, view.n_samples, view.rate, k)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_with_their_checksum() {
        let rate = 1e6;
        let (mut e, mut t) = (vec![], 1000i64);
        let rd: [[u8; 4]; 2] = [[0x02, 0x8C, 0x01, 0x5F], [0x02, 0x90, 0x80, 0x65]];
        for r in rd {
            let sum = r.iter().fold(0u8, |a, &y| a.wrapping_add(y));
            e.extend([t, t + 1000]); // the host's start low, then its release
            t += 1000 + 30;
            e.extend([t, t + 80]); // the answer: low, high
            t += 160;
            for b in r.iter().chain(std::iter::once(&sum)) {
                for q in (0..8).rev() {
                    e.push(t); // the bit's low
                    t += 50;
                    e.push(t);
                    t += if (b >> q) & 1 == 1 { 70 } else { 27 };
                }
            }
            e.push(t); // the last low, then idle
            t += 50;
            e.push(t);
            t += 30_000;
        }
        let v = View::new(rate, t, vec![Channel::new("H", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].params["model"], "dht22");
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let r = lay["readings"].as_array().unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0]["rh"], 65.2);
        assert_eq!(r[0]["t"], 35.1);
        assert_eq!(r[1]["t"], -10.1);
    }
}
