//! WCH's SWIO, one wire (Python's `typed.pulse_symbols` and `rvswd.SwioPlugin`): pulses away from the idle level,
//! short for 1 and long for 0, framed at pauses; 41 pulses a DMI access (a 1, addr7, R/W (1 = write), data32), 33 a
//! continuation of reading data0 (a 0, data32). No parity: the evidence is the frames' structure and the Debug
//! Module's register addresses.

use crate::Candidate;
use crate::features::features;
use crate::kernels::median;
use crate::pulse::{candidate, obj};
use crate::rvswd::dm_name;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

const BIN: f64 = 0.08; // log widths a histogram bin
const APART: f64 = 0.47000362924573563; // ln 1.6: clusters nearer are one
const CLUSTERS: usize = 8; // the most populated width clusters tried in pairs
const SHARE: f64 = 0.25; // pulses in 41/33 frames: this share or more for their number to count past it
const MANY: f64 = 16.0; // DMI transfers: this many make the pin's other traffic count for little

struct Symbols {
    lead: Vec<i64>,
    width: Vec<i64>,
    split: f64,
    bounds: Vec<usize>,
}

fn msb(b: &[bool]) -> u32 {
    b.iter().fold(0u32, |a, &x| (a << 1) | x as u32)
}

/// The pin's pulses classified short / long, framed at pauses (the split and the pitch from the two width clusters
/// that frame the most pulses in SWIO's lengths).
fn symbols(ch: &Channel, idle: u8) -> Option<Symbols> {
    let e = ch.edges;
    let (mut lead, mut width) = (vec![], vec![]);
    for i in 0..e.len() {
        let after = ch.initial ^ (((i + 1) & 1) as u8);
        if after != idle && i + 1 < e.len() {
            lead.push(e[i]);
            width.push(e[i + 1] - e[i]);
        }
    }
    if lead.len() < 8 {
        return None;
    }
    let lw: Vec<f64> = width.iter().map(|&w| (w.max(1) as f64).ln()).collect();
    let (lo, hi) = lw
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |a, &x| (a.0.min(x), a.1.max(x)));
    let nb_edges = ((hi + 0.1 - lo) / BIN).ceil() as usize; // np.arange(lo, hi + 0.1, 0.08)
    if nb_edges < 3 {
        return None;
    }
    let nbins = nb_edges - 1;
    let mut raw = vec![0usize; nbins];
    for &x in &lw {
        raw[(((x - lo) / BIN).floor() as usize).min(nbins - 1)] += 1;
    }
    let centers: Vec<f64> = (0..nbins).map(|i| lo + (i as f64 + 0.5) * BIN).collect();
    let hist: Vec<usize> = (0..nbins)
        .map(|i| raw[i] + if i > 0 { raw[i - 1] } else { 0 } + if i + 1 < nbins { raw[i + 1] } else { 0 })
        .collect();
    let mut peaks: Vec<usize> = (0..nbins)
        .filter(|&i| {
            hist[i] >= if i > 0 { hist[i - 1] } else { 0 } && hist[i] > if i + 1 < nbins { hist[i + 1] } else { 0 }
        })
        .collect();
    peaks.sort_by(|&a, &b| hist[b].cmp(&hist[a]).then(b.cmp(&a)));
    let mut order: Vec<usize> = vec![];
    for i in peaks {
        if order.iter().all(|&k| (centers[i] - centers[k]).abs() >= APART) {
            order.push(i);
        }
    }
    let first = *order.first()?;
    let second = order[1..]
        .iter()
        .copied()
        .find(|&i| hist[i] as f64 >= 0.05 * hist[first] as f64)?;
    let iv: Vec<f64> = lead.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let cut_by = |near: Option<&[bool]>| -> Vec<usize> {
        let sel: Vec<f64> = match near {
            None => iv.clone(),
            Some(nr) => (0..iv.len()).filter(|&i| nr[i] && nr[i + 1]).map(|i| iv[i]).collect(),
        };
        let typical = if !sel.is_empty() {
            median(&sel)
        } else if !iv.is_empty() {
            median(&iv)
        } else {
            1.0
        };
        let mut b = vec![0usize];
        b.extend((0..iv.len()).filter(|&i| iv[i] > 3.0 * typical).map(|i| i + 1));
        b.push(lead.len());
        b
    };
    // pulses in frames that read as SWIO's
    let framed = |split: f64, b: &[usize]| -> usize {
        let mut got = 0;
        for z in b.windows(2) {
            let n = z[1] - z[0];
            if n != 41 && n != 33 {
                continue;
            }
            let f: Vec<bool> = width[z[0]..z[1]].iter().map(|&w| (w as f64) < split).collect();
            if f.iter().all(|&x| x) {
                continue;
            }
            if n == 41 && (!f[0] || dm_name(msb(&f[1..8])).is_none()) {
                continue;
            }
            got += n;
        }
        got
    };
    let split = ((centers[first] + centers[second]) / 2.0).exp();
    let b = cut_by(None);
    let mut best = (framed(split, &b), split, b);
    let top: Vec<usize> = order.iter().copied().take(CLUSTERS).filter(|&i| hist[i] > 0).collect();
    for x in 0..top.len() {
        for y in x + 1..top.len() {
            let (i, j) = (top[x], top[y]);
            if (centers[i] - centers[j]).abs() < APART {
                continue;
            }
            let sp = ((centers[i] + centers[j]) / 2.0).exp();
            let near: Vec<bool> = lw
                .iter()
                .map(|&v| (v - centers[i]).abs() < APART || (v - centers[j]).abs() < APART)
                .collect();
            let bb = cut_by(Some(&near));
            let got = framed(sp, &bb);
            if got > best.0 {
                best = (got, sp, bb);
            }
        }
    }
    Some(Symbols {
        lead,
        width,
        split: best.1,
        bounds: best.2,
    })
}

/// How a frame's pulses sat against the split (ns): the longest short one, the shortest long one, their medians,
/// the space between pulses and the pitch after a 1 and after a 0, and the R/W bit's own low and gap.
fn widths(ps: &Symbols, short: &[bool], a: usize, b: usize, rw: Option<usize>, ns: f64) -> Map<String, Value> {
    let r1 = |x: f64| json!((x * ns * 10.0).round() / 10.0);
    let pick = |want: bool| -> Vec<f64> {
        (a..b)
            .filter(|&i| short[i] == want)
            .map(|i| ps.width[i] as f64)
            .collect()
    };
    let (sh, lo) = (pick(true), pick(false));
    let opt = |v: &[f64], f: &dyn Fn(&[f64]) -> f64| if v.is_empty() { Value::Null } else { r1(f(v)) };
    let mut m = obj(vec![
        (
            "short_max_ns",
            opt(&sh, &|v| v.iter().copied().fold(f64::MIN, f64::max)),
        ),
        ("long_min_ns", opt(&lo, &|v| v.iter().copied().fold(f64::MAX, f64::min))),
        ("short_median_ns", opt(&sh, &median)),
        ("long_median_ns", opt(&lo, &median)),
    ]);
    if b - a >= 2 {
        let d: Vec<f64> = (a..b - 1).map(|i| (ps.lead[i + 1] - ps.lead[i]) as f64).collect();
        let sp: Vec<f64> = (a..b - 1).map(|i| d[i - a] - ps.width[i] as f64).collect();
        m.insert("space_ns".into(), r1(median(&sp)));
        for (key, want) in [("pitch_1_ns", true), ("pitch_0_ns", false)] {
            let x: Vec<f64> = (a..b - 1).filter(|&i| short[i] == want).map(|i| d[i - a]).collect();
            m.insert(key.into(), opt(&x, &median));
        }
    }
    if let Some(rw) = rw {
        m.insert("rw_low_ns".into(), r1(ps.width[rw] as f64));
        if rw + 1 < b {
            m.insert(
                "rw_gap_ns".into(),
                r1((ps.lead[rw + 1] - ps.lead[rw] - ps.width[rw]) as f64),
            );
        }
    }
    m
}

fn read(ch: &Channel, n: i64, rate: f64) -> Option<Candidate> {
    let idle = features(ch, n).idle;
    let ps = symbols(ch, idle)?;
    let short: Vec<bool> = ps.width.iter().map(|&w| (w as f64) < ps.split).collect();
    let (mut tx, mut lens): (Vec<Map<String, Value>>, Vec<usize>) = (vec![], vec![]);
    let mut last: Option<(bool, u32)> = None; // (a read, its address) of the access before
    let ns = 1e9 / rate;
    for z in ps.bounds.windows(2) {
        let (a, b) = (z[0], z[1]);
        let f = &short[a..b];
        lens.push(b - a);
        let e = ps.lead[b - 1] + ps.width[b - 1];
        if b - a == 41 && f[0] {
            let (write, addr) = (f[8], msb(&f[1..8]));
            tx.push(obj(vec![
                ("s", json!(ps.lead[a])),
                ("e", json!(e)),
                ("op", json!(if write { "W" } else { "R" })),
                ("addr", json!(addr)),
                ("reg", json!(dm_name(addr))),
                ("data", json!(msb(&f[9..]))),
                ("ok", json!(true)),
            ]));
            tx.last_mut()
                .unwrap()
                .extend(widths(&ps, &short, a, b, Some(a + 8), ns));
            last = Some((!write, addr));
        } else if b - a == 33 && !f[0] && last == Some((true, 0x04)) {
            // a continuation of reading data0 (autoexec): only after a read of it
            tx.push(obj(vec![
                ("s", json!(ps.lead[a])),
                ("e", json!(e)),
                ("op", json!("R")),
                ("addr", json!(0x04)),
                ("reg", json!("data0")),
                ("data", json!(msb(&f[1..]))),
                ("ok", json!(true)),
                ("fast", json!(true)),
            ]));
            tx.last_mut().unwrap().extend(widths(&ps, &short, a, b, None, ns));
        }
    }
    if tx.len() < 2 {
        return None;
    }
    let ours = |n: &usize| *n == 41 || *n == 33;
    let structured = lens.iter().filter(|n| ours(n)).count() as f64 / lens.len() as f64;
    let pulses = lens.iter().filter(|n| ours(n)).sum::<usize>() as f64 / lens.iter().sum::<usize>().max(1) as f64;
    let known = tx.iter().filter(|x| x["reg"] != Value::Null).count() as f64 / tx.len() as f64;
    // many reads and writes of known DM registers do not come by chance however much else the pin carries
    let dilute = if pulses >= SHARE {
        pulses.max(1.0 - (-(tx.len() as f64) / MANY).exp())
    } else {
        pulses
    };
    let score = known * (0.5 + 0.5 * dilute) * (1.0 - 0.5f64.powf(tx.len() as f64 / 4.0));
    let split_ns = (ps.split / rate * 1e10).round() / 10.0;
    let ev = obj(vec![
        ("frames", json!(lens.len())),
        ("structured", json!(structured)),
        ("pulse_share", json!(pulses)),
        ("dmi", json!(tx.len())),
        ("dm_addr", json!(known)),
        ("split_ns", json!(split_ns)),
    ]);
    Some(candidate(
        "swio",
        &[("dio", ch.name)],
        score,
        obj(vec![("idle", json!(idle)), ("split_ns", json!(split_ns))]),
        ev,
        "dmi",
        &tx,
        0,
    ))
}

/// The SWIO readings of a part: each line of 82 changes or more (two accesses).
pub fn candidates(view: &View) -> Vec<Candidate> {
    view.channels
        .iter()
        .filter(|ch| ch.edges.len() >= 82)
        .filter_map(|ch| read(ch, view.n_samples, view.rate))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accesses_and_a_data0_continuation() {
        let (mut e, mut t) = (vec![], 1000i64);
        let mut frame = |bits: Vec<bool>, e: &mut Vec<i64>| {
            for b in bits {
                e.extend([t, t + if b { 3 } else { 12 }]); // a low: short 1, long 0
                t += 20;
            }
            t += 300;
        };
        let access = |addr: u32, write: bool, data: u32| -> Vec<bool> {
            let mut f = vec![true];
            f.extend((0..7).rev().map(|q| (addr >> q) & 1 == 1));
            f.push(write);
            f.extend((0..32).rev().map(|q| (data >> q) & 1 == 1));
            f
        };
        frame(access(0x10, true, 0x8000_0001), &mut e);
        frame(access(0x11, false, 0x0038_2c82), &mut e);
        frame(access(0x04, false, 0x1234_5678), &mut e);
        let mut cont = vec![false];
        cont.extend((0..32).rev().map(|q| (0xCAFE_F00Du32 >> q) & 1 == 1));
        frame(cont, &mut e);
        let v = View::new(10e6, t + 100, vec![Channel::new("DIO", 1, &e)]).unwrap();
        let c = candidates(&v);
        assert_eq!(c.len(), 1);
        let lay: Value = serde_json::from_str(c[0].layers.get()).unwrap();
        let d = lay["dmi"].as_array().unwrap();
        assert_eq!(d.len(), 4);
        assert_eq!(d[0]["reg"], "dmcontrol");
        assert_eq!(d[3]["data"], 0xCAFE_F00Du32);
        assert_eq!(d[3]["fast"], true);
    }
}
