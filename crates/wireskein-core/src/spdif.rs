//! S/PDIF (IEC 60958; Python's `_engine/spdif.py`) on one line: biphase mark coding, subframes of 32 cells: a
//! preamble breaking the coding (a run of one and a half cells: B, M on channel A, W on channel B), 4 aux bits, 20
//! audio bits (LSB first: with the aux bits a 24-bit sample), V, U, C and an even parity over the 28 cells.

use crate::Candidate;
use crate::pulse::{best_of_views, candidate, evidence, obj};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const MIN: usize = 16; // subframes whose parity holds, at least
const CHAIN: f64 = 0.9; // of them, this share in A / B turns a subframe apart
const SHARE: f64 = 0.01; // a run length this share of the runs at least to count as half a cell
const SILENCE: i64 = 64; // half cells: a stretch with no change this long ends a transmission

fn preamble(u: &[i64]) -> Option<&'static str> {
    match u {
        [3, 1, 1, 3] => Some("B"),
        [3, 3, 1, 1] => Some("M"),
        [3, 2, 1, 2] => Some("W"),
        _ => None,
    }
}

/// The lengths a share of the runs have at least, ascending.
fn common(d: &[i64]) -> Vec<i64> {
    let mut v = d.to_vec();
    v.sort_unstable();
    let mut out = vec![];
    let mut i = 0;
    while i < v.len() {
        let j = v.partition_point(|&x| x <= v[i]);
        if (j - i) as f64 >= SHARE * d.len() as f64 {
            out.push(v[i]);
        }
        i = j;
    }
    out
}

struct Sub {
    s: i64,
    e: i64,
    pre: &'static str,
    ok: bool,
    row: Map<String, Value>,
}

/// The subframes on a line, each stretch of transmission (between silences of SILENCE half cells) read at its own
/// half cell.
fn subframes(edges: &[i64]) -> Vec<Sub> {
    if edges.len() < 200 {
        return vec![];
    }
    let d: Vec<i64> = edges.windows(2).map(|w| w[1] - w[0]).collect();
    let c = common(&d);
    let Some(&c0) = c.first() else {
        return vec![];
    };
    let lim = SILENCE * c0.max(1);
    let mut out = vec![];
    let mut a = 0usize;
    for (i, &x) in d.iter().enumerate() {
        if x > lim {
            out.extend(stretch(&edges[a..i + 1]));
            a = i + 1;
        }
    }
    out.extend(stretch(&edges[a..]));
    out
}

fn stretch(edges: &[i64]) -> Vec<Sub> {
    if edges.len() < 200 {
        return vec![];
    }
    let d: Vec<i64> = edges.windows(2).map(|w| w[1] - w[0]).collect();
    let c = common(&d);
    let Some(&c0) = c.first() else {
        return vec![];
    };
    if c0 < 1 {
        return vec![];
    }
    let c0 = c0 as f64;
    // at a few ticks a half cell its runs split over two lengths: the half cell is the length the runs (of up to
    // three halves) are whole multiples of most nearly
    let mut x: Vec<f64> = d.iter().map(|&v| v as f64).filter(|&v| v < 4.0 * c0).collect();
    if x.len() > 4000 {
        let step = x.len() / 4000;
        x = x.into_iter().step_by(step).collect();
    }
    let (mut half, mut best) = (0.0, f64::INFINITY);
    for q in 0..161 {
        let cand = 0.8 * c0 + (1.6 * c0 - 0.8 * c0) * q as f64 / 160.0;
        let err = x
            .iter()
            .map(|&v| (v / cand - (v / cand).round_ties_even()).abs())
            .sum::<f64>()
            / x.len() as f64;
        if err < best {
            (half, best) = (cand, err);
        }
    }
    let u: Vec<i64> = d.iter().map(|&v| (v as f64 / half).round_ties_even() as i64).collect();
    let n = u.len();
    // a subframe starts at a run of one and a half cells (a preamble's coding violation): from those only
    let starts: Vec<usize> = (0..n.saturating_sub(3)).filter(|&i| u[i] == 3).collect();
    if starts.len() < 8 {
        return vec![];
    }
    let mut out = vec![];
    let mut i = starts[0];
    while i + 4 <= n {
        let Some(pre) = preamble(&u[i..i + 4]) else {
            let k = starts.partition_point(|&s| s < i + 1);
            if k >= starts.len() {
                break;
            }
            i = starts[k];
            continue;
        };
        let s0 = edges[i];
        let (mut j, mut bits) = (i + 4, Vec::with_capacity(28));
        while j < n && bits.len() < 28 {
            if u[j] == 2 {
                bits.push(0u32);
                j += 1;
            } else if u[j] == 1 && j + 1 < n && u[j + 1] == 1 {
                bits.push(1);
                j += 2;
            } else {
                break;
            }
        }
        if bits.len() < 28 {
            i = if j > i + 4 { j } else { i + 1 };
            continue;
        }
        let val: u32 = bits[..24].iter().enumerate().map(|(k, &b)| b << k).sum();
        let ok = bits.iter().sum::<u32>() % 2 == 0;
        let row = obj(vec![
            ("s", json!(s0)),
            ("e", json!(edges[j])),
            ("preamble", json!(pre)),
            ("audio", json!(val)),
            ("v", json!(bits[24])),
            ("u", json!(bits[25])),
            ("c", json!(bits[26])),
            ("ok", json!(ok)),
        ]);
        out.push(Sub {
            s: s0,
            e: edges[j],
            pre,
            ok,
            row,
        });
        i = j;
    }
    out
}

fn read(ch: &Channel, k: i64) -> Option<Candidate> {
    let sf = subframes(ch.edges);
    let good: Vec<&Sub> = sf.iter().filter(|x| x.ok).collect();
    if good.len() < MIN {
        return None;
    }
    // subframes in their order: channel A (B / M) and B (W) by turns, one subframe's time apart
    let chain = good
        .windows(2)
        .filter(|w| {
            let (a, b) = (w[0], w[1]);
            ((a.pre == "W") != (b.pre == "W")) && (((b.s - a.s) as f64 / (a.e - a.s).max(1) as f64) - 1.0).abs() < 0.1
        })
        .count();
    if (chain as f64) < CHAIN * (good.len() - 1) as f64 {
        return None;
    }
    // each subframe's preamble (a coding violation of its own pattern: 3) and parity (1)
    let score = good.len() as f64 / sf.len() as f64 * evidence(4.0 * good.len() as f64);
    let blocks = sf.iter().filter(|x| x.pre == "B").count();
    let ev = obj(vec![
        ("subframes", json!(sf.len())),
        ("ok", json!(good.len())),
        ("blocks", json!(blocks)),
    ]);
    let rows: Vec<Map<String, Value>> = sf.into_iter().map(|x| x.row).collect();
    Some(candidate(
        "spdif",
        &[("data", ch.name)],
        score,
        Map::new(),
        ev,
        "subframes",
        &rows,
        k,
    ))
}

/// The S/PDIF readings of a part: each line, the best of its views.
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 200)
        .filter_map(|ch| best_of_views(ch, read))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Half cells of a subframe: the preamble's and two per bit (biphase mark), its parity even.
    fn subframe(pre: [i64; 4], audio: u32) -> Vec<i64> {
        let mut bits: Vec<u32> = (0..24).map(|k| (audio >> k) & 1).collect();
        bits.extend([0, 0, 1]);
        bits.push(bits.iter().sum::<u32>() % 2);
        let mut runs = pre.to_vec();
        for b in bits {
            if b == 1 {
                runs.extend([1, 1]);
            } else {
                runs.push(2);
            }
        }
        runs
    }

    #[test]
    fn subframes_in_turns() {
        let half = 7.3f64; // ticks a half cell (not whole)
        let mut runs = vec![];
        let mut want = vec![];
        for f in 0..40u32 {
            let (a, b) = ((f * 99991) & 0xFFFFFF, f * 7919 + 5);
            let pre = if f == 0 { [3, 1, 1, 3] } else { [3, 3, 1, 1] };
            runs.extend(subframe(pre, a));
            runs.extend(subframe([3, 2, 1, 2], b));
            want.push(a);
            want.push(b);
        }
        let (mut e, mut t) = (vec![], 100.0f64);
        for r in runs {
            e.push(t.round() as i64);
            t += r as f64 * half;
        }
        e.push(t.round() as i64);
        let v = View::new(1e6, t as i64 + 100, vec![Channel::new("D", 0, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let sf = lay["subframes"].as_array().unwrap();
        let got: Vec<u64> = sf.iter().map(|x| x["audio"].as_u64().unwrap()).collect();
        // the last subframe has no change after it to end its last cell: all but it
        assert_eq!(got[..], want.iter().map(|&x| x as u64).collect::<Vec<_>>()[..got.len()]);
        assert!(got.len() >= 79);
        assert_eq!(sf[0]["preamble"], "B");
        assert_eq!(c[0].evidence["blocks"], 1);
    }
}
