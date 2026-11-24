//! What the pulse-width readers of one line (DALI, DCC, J1850, SENT, S/PDIF, SWIM, USB PD) and of a pair (Wiegand)
//! share: the views of a line they read (Python's `staged._line_views` / `_pair_views`), the score of their checks'
//! evidence, a candidate built from their items.

use crate::Candidate;
use crate::kernels::{debounce, median, runs, shortest_first};
use crate::view::Channel;
use serde_json::{Map, Value, json};

pub const EVIDENCE_BITS: f64 = 8.0;
pub const DEGLITCH_OF_MEDIAN: f64 = 0.25; // a lone line's pulses shorter than this share of its median run taken out ...
pub const DEGLITCH_MAX: f64 = 64.0; // ... up to this many ticks, at least 2

/// A check's bits of evidence to a score (as the declared protocols).
pub fn evidence(info: f64) -> f64 {
    1.0 - 0.5f64.powf(info / EVIDENCE_BITS)
}

/// A lone line as it is and, where it has pulses far shorter than its usual runs (spikes, bounces at its edges), with
/// those taken out and with its bounces merged: (changes, k; k < 0 for bounces merged).
pub fn line_views(e: &[i64]) -> Vec<(Vec<i64>, i64)> {
    let mut out = vec![(e.to_vec(), 0)];
    if e.len() < 8 {
        return out;
    }
    let r: Vec<f64> = e.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let k = (DEGLITCH_OF_MEDIAN * median(&r)).clamp(2.0, DEGLITCH_MAX) as i64;
    if !r.iter().any(|&x| x < k as f64) {
        return out;
    }
    let d = shortest_first(e, k);
    if d.len() != e.len() && d.len() >= 8 {
        out.push((d, k));
    }
    let b = debounce(e, k);
    if b.len() != e.len() && b.len() >= 8 {
        out.push((b, -k));
    }
    out
}

/// The k of a line in a pair's views: of its inner runs, the shorter level's median run's share, or None (too few).
fn pair_k(ch: &Channel, n: i64) -> Option<i64> {
    let (_, length, level) = runs(ch, n);
    if length.len() < 10 {
        return None;
    }
    let (ln, lv) = (&length[1..length.len() - 1], &level[1..level.len() - 1]);
    let med = (0..2u8)
        .filter_map(|l| {
            let x: Vec<f64> = ln
                .iter()
                .zip(lv)
                .filter(|&(_, &v)| v == l)
                .map(|(&a, _)| a as f64)
                .collect();
            (!x.is_empty()).then(|| median(&x))
        })
        .fold(f64::INFINITY, f64::min);
    Some((DEGLITCH_OF_MEDIAN * med).clamp(2.0, DEGLITCH_MAX) as i64)
}

/// Two lines as they are and, where either has pulses far shorter than its shorter level's usual runs, both with
/// those taken out: (a's changes, b's changes, k).
pub fn pair_views(a: &Channel, b: &Channel, n: i64) -> Vec<(Vec<i64>, Vec<i64>, i64)> {
    let mut out = vec![(a.edges.to_vec(), b.edges.to_vec(), 0)];
    let (Some(ka), Some(kb)) = (pair_k(a, n), pair_k(b, n)) else {
        return out;
    };
    let (da, db) = (shortest_first(a.edges, ka), shortest_first(b.edges, kb));
    if da.len() != a.edges.len() || db.len() != b.edges.len() {
        out.push((da, db, ka.max(kb)));
    }
    out
}

/// The level after each change of a line.
pub fn level_after(ch: &Channel) -> Vec<u8> {
    (0..ch.edges.len())
        .map(|i| ch.initial ^ (((i + 1) & 1) as u8))
        .collect()
}

/// A candidate of one reader: its layer's items under `layer`, `deglitch` in its params where read on a view.
#[allow(clippy::too_many_arguments)]
pub fn candidate(
    protocol: &str,
    roles: &[(&str, &str)],
    score: f64,
    mut params: Map<String, Value>,
    evidence: Map<String, Value>,
    layer: &str,
    items: &[Map<String, Value>],
    k: i64,
) -> Candidate {
    if k != 0 {
        params.insert("deglitch".into(), json!(k));
    }
    let mut lay = Map::new();
    lay.insert(layer.into(), json!(items));
    Candidate {
        protocol: protocol.into(),
        roles: roles.iter().map(|&(r, l)| (r.to_string(), l.to_string())).collect(),
        score,
        alt: None,
        params,
        evidence,
        layers: crate::claim::raw(serde_json::to_string(&lay).unwrap()),
    }
}

/// The best of a line's readings over its views.
pub fn best_of_views(ch: &Channel, read: impl Fn(&Channel, i64) -> Option<Candidate>) -> Option<Candidate> {
    let mut best: Option<Candidate> = None;
    for (e, k) in line_views(ch.edges) {
        let c = Channel::new(ch.name, ch.initial, &e);
        if let Some(r) = read(&c, k)
            && best.as_ref().is_none_or(|b| r.score > b.score)
        {
            best = Some(r);
        }
    }
    best
}

/// A JSON object from pairs.
pub fn obj(kv: Vec<(&str, Value)>) -> Map<String, Value> {
    kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// Bits (MSB first) as hex digits, no leading zeros (Python's `format(int(bits, 2), "x")`).
pub fn bits_hex(bits: &[u8]) -> String {
    let pad = (4 - bits.len() % 4) % 4;
    let all: Vec<u8> = std::iter::repeat_n(0u8, pad).chain(bits.iter().copied()).collect();
    let s: String = all
        .chunks(4)
        .map(|c| {
            let v = c.iter().fold(0u32, |a, &b| (a << 1) | b as u32);
            std::char::from_digit(v, 16).unwrap()
        })
        .collect();
    let t = s.trim_start_matches('0');
    if t.is_empty() { "0".into() } else { t.into() }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_of_bits() {
        assert_eq!(bits_hex(&[1, 0, 1, 0, 1]), "15");
        assert_eq!(bits_hex(&[0, 0]), "0");
        assert_eq!(hex(&[0x0a, 0xff]), "0aff");
    }
}
