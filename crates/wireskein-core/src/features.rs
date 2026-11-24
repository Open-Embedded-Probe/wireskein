//! A line's own statistics (Python's `_engine/features.py`): its idle level, candidate bit units, how clock-like.

use crate::kernels::{median, runs};
use crate::view::Channel;
use std::collections::BTreeMap;

pub const IDLE_ENDS_OF_MEDIAN: f64 = 10.0;

#[derive(Debug, Clone, Copy)]
pub struct Unit {
    pub samples: f64,
    pub fit: f64,
    pub resid: f64,
}

/// Candidate base units from run lengths (`features.estimate_units`).
pub fn estimate_units(runs_in: &[i64], max_mult: f64, top: usize) -> Vec<Unit> {
    let runs: Vec<f64> = runs_in.iter().filter(|&&r| r > 0).map(|&r| r as f64).collect();
    if runs.len() < 4 {
        return vec![];
    }
    // histogram of rounded lengths: the 12 commonest
    let mut hist: BTreeMap<i64, usize> = BTreeMap::new();
    for &r in &runs {
        *hist.entry(r.round_ties_even() as i64).or_default() += 1;
    }
    let mut peaks: Vec<(i64, usize)> = hist.into_iter().collect();
    // np.argsort(counts)[::-1]: by count descending; among equal counts numpy's quicksort order is not stable, take
    // the larger value first (argsort ascending puts the smaller first, reversed: the larger)
    peaks.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    let mut cands: Vec<f64> = Vec::new();
    for &(p, _) in peaks.iter().take(12) {
        for k in [1.0, 2.0, 3.0, 4.0, 5.0, 8.0] {
            let c = p as f64 / k;
            if c >= 1.5 && !cands.contains(&c) {
                cands.push(c);
            }
        }
    }
    cands.sort_by(|a, b| a.partial_cmp(b).unwrap());
    // distinct lengths with counts
    let mut vals: BTreeMap<i64, f64> = BTreeMap::new();
    for &r in runs_in.iter().filter(|&&r| r > 0) {
        *vals.entry(r).or_default() += 1.0;
    }
    let sv_all: Vec<f64> = vals.keys().map(|&v| v as f64).collect();
    let sc_all: Vec<f64> = vals.values().copied().collect();
    let mut cum = Vec::with_capacity(sc_all.len());
    let mut acc = 0.0;
    for &c in &sc_all {
        acc += c;
        cum.push(acc);
    }
    let nr = runs.len() as f64;
    let mut out: Vec<Unit> = Vec::new();
    for &u in &cands {
        let lim = max_mult * u * 1.05;
        let m = sv_all.partition_point(|&v| v <= lim);
        let n_sel = if m > 0 { cum[m - 1] } else { 0.0 };
        if n_sel < (4.0f64).max(0.2 * nr) {
            continue;
        }
        let (sv, sc) = (&sv_all[..m], &sc_all[..m]);
        let k: Vec<f64> = sv.iter().map(|&v| (v / u).round_ties_even().max(1.0)).collect();
        let num: f64 = sv.iter().zip(sc).zip(&k).map(|((&v, &c), &k)| c * v * k).sum();
        let den: f64 = sc.iter().zip(&k).map(|(&c, &k)| c * k * k).sum();
        let u2 = num / den;
        let mut good = 0.0;
        let mut rsum = 0.0;
        for (&v, &c) in sv.iter().zip(sc) {
            let k = (v / u2).round_ties_even().max(1.0);
            let r = (v / u2 - k).abs();
            if r < 0.2 {
                good += c;
            }
            rsum += c * r;
        }
        let fit = good / n_sel * (n_sel / nr / 0.5).min(1.0);
        out.push(Unit {
            samples: u2,
            fit,
            resid: rsum / n_sel,
        });
    }
    if out.is_empty() {
        return out;
    }
    let best = out.iter().map(|c| c.fit).fold(f64::MIN, f64::max);
    let mut good: Vec<Unit> = out.iter().copied().filter(|c| c.fit >= best - 0.03).collect();
    good.sort_by(|a, b| b.samples.partial_cmp(&a.samples).unwrap());
    let mut rest: Vec<Unit> = out.iter().copied().filter(|c| c.fit < best - 0.03).collect();
    rest.sort_by(|a, b| {
        b.fit
            .partial_cmp(&a.fit)
            .unwrap()
            .then(b.samples.partial_cmp(&a.samples).unwrap())
    });
    let mut merged: Vec<Unit> = Vec::new();
    for c in good.into_iter().chain(rest) {
        if let Some(i) = merged
            .iter()
            .position(|k| (c.samples - k.samples).abs() / k.samples <= 0.06)
        {
            if c.fit >= merged[i].fit - 0.02 && c.resid < merged[i].resid {
                merged[i] = c;
            }
        } else {
            merged.push(c);
        }
    }
    merged.truncate(top);
    merged
}

/// What the engine needs of a line's features.
#[derive(Debug, Clone)]
pub struct Features {
    pub n_edges: usize,
    pub idle: u8,
    pub units: Vec<Unit>,
    pub clock: f64,
    pub clock_local: f64,
}

pub fn features(ch: &Channel, n: i64) -> Features {
    let e = ch.edges;
    if e.is_empty() {
        return Features {
            n_edges: 0,
            idle: ch.initial,
            units: vec![],
            clock: 0.0,
            clock_local: 0.0,
        };
    }
    let (_start, length, level) = runs(ch, n);
    let ilen: Vec<i64> = length[1..length.len() - 1].to_vec();
    let ilev: Vec<u8> = level[1..level.len() - 1].to_vec();
    // idle: the level holding most of the time of the 20 longest runs (with long end runs)
    let (mut pool_len, mut pool_lev) = if !ilen.is_empty() {
        (ilen.clone(), ilev.clone())
    } else {
        (length.clone(), level.clone())
    };
    if !ilen.is_empty() && length.len() >= 2 {
        let med0 = median(&ilen.iter().map(|&x| x as f64).collect::<Vec<_>>());
        for i in [0, length.len() - 1] {
            if length[i] as f64 >= IDLE_ENDS_OF_MEDIAN * med0 {
                pool_len.push(length[i]);
                pool_lev.push(level[i]);
            }
        }
    }
    let mut order: Vec<usize> = (0..pool_len.len()).collect();
    order.sort_by_key(|&i| pool_len[i]); // stable: argsort's ties may differ, the sums rarely
    let top = &order[order.len().saturating_sub(20)..];
    let (mut hi, mut lo) = (0i64, 0i64);
    for &i in top {
        if pool_lev[i] == 1 {
            hi += pool_len[i]
        } else {
            lo += pool_len[i]
        }
    }
    let mut idle = if hi >= lo { 1 } else { 0 };
    let fin = ch.initial ^ (e.len() as u8 & 1);
    if ch.initial == fin {
        idle = ch.initial;
    }
    let act: Vec<i64> = ilen
        .iter()
        .zip(&ilev)
        .filter(|&(_, &l)| l != idle)
        .map(|(&r, _)| r)
        .collect();
    let mut units = if act.len() >= 20 {
        estimate_units(&act, 12.0, 6)
    } else {
        estimate_units(&ilen, 12.0, 6)
    };
    if units.is_empty() {
        units = estimate_units(&ilen, 12.0, 6);
    }
    // rising edges: after edge k the level is initial ^ ((k + 1) & 1)
    let rising: Vec<i64> = e
        .iter()
        .enumerate()
        .filter(|&(k, _)| ch.initial ^ (((k + 1) & 1) as u8) == 1)
        .map(|(_, &x)| x)
        .collect();
    let rise_iv: Vec<f64> = rising.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let (mut period, mut conc) = (None, 0.0);
    if rising.len() >= 4 {
        let m = median(&rise_iv);
        let iv: Vec<f64> = rise_iv.iter().copied().filter(|&x| x < 16.0 * m).collect();
        if iv.len() >= 3 {
            let mut h: BTreeMap<i64, usize> = BTreeMap::new();
            for &x in &iv {
                *h.entry(x.round_ties_even() as i64).or_default() += 1;
            }
            // np.argmax over sorted values: the first (smallest) of the most common
            let mx = *h.values().max().unwrap();
            let p = *h.iter().find(|(_, c)| **c == mx).unwrap().0 as f64;
            let near: Vec<f64> = iv
                .iter()
                .copied()
                .filter(|&x| (x - p).abs() <= (1.0f64).max(0.1 * p))
                .collect();
            period = Some(near.iter().sum::<f64>() / near.len() as f64);
            conc = near.len() as f64 / iv.len() as f64;
        }
    }
    let mut clock = 0.0;
    if let Some(p) = period.filter(|&p| p != 0.0) {
        let hi: Vec<f64> = ilen
            .iter()
            .zip(&ilev)
            .filter(|&(&r, &l)| l == 1 && (r as f64) < 2.0 * p)
            .map(|(&r, _)| r as f64)
            .collect();
        let lo: Vec<f64> = ilen
            .iter()
            .zip(&ilev)
            .filter(|&(&r, &l)| l == 0 && (r as f64) < 2.0 * p)
            .map(|(&r, _)| r as f64)
            .collect();
        if hi.len() > 2 && lo.len() > 2 {
            let (mh, ml) = (median(&hi), median(&lo));
            let sh = hi
                .iter()
                .filter(|&&x| (x - mh).abs() <= (1.0f64).max(0.15 * mh))
                .count() as f64
                / hi.len() as f64;
            let sl = lo
                .iter()
                .filter(|&&x| (x - ml).abs() <= (1.0f64).max(0.15 * ml))
                .count() as f64
                / lo.len() as f64;
            clock = conc * (sh + sl) / 2.0;
        }
    }
    let clock_local = local_clock(&rise_iv);
    Features {
        n_edges: e.len(),
        idle,
        units,
        clock,
        clock_local,
    }
}

/// Inside bursts, each rising-to-rising interval close to its neighbour (`features.local_clock`).
fn local_clock(iv: &[f64]) -> f64 {
    if iv.len() < 16 {
        return 0.0;
    }
    let n = iv.len();
    let inb: Vec<bool> = (0..n)
        .map(|i| {
            let prev = if i > 0 { iv[i - 1] } else { f64::INFINITY };
            let nxt = if i + 1 < n { iv[i + 1] } else { f64::INFINITY };
            iv[i] <= 3.0 * prev.min(nxt)
        })
        .collect();
    let mut cnt = 0usize;
    let mut good = 0usize;
    for i in 1..n {
        if inb[i] && inb[i - 1] {
            cnt += 1;
            let r = iv[i] / iv[i - 1];
            if r.ln().abs() < (1.1f64).ln() + 1.0 / iv[i].max(1.0) {
                good += 1;
            }
        }
    }
    if cnt < 8 { 0.0 } else { good as f64 / cnt as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_of_whole_multiples() {
        let runs: Vec<i64> = (0..200).map(|i| [10, 20, 30, 10, 40][i % 5]).collect();
        let u = estimate_units(&runs, 12.0, 6);
        assert!((u[0].samples - 10.0).abs() < 0.01, "{u:?}");
    }

    #[test]
    fn a_clock_is_clock_like() {
        let e: Vec<i64> = (1..200).map(|i| i * 5).collect();
        let f = features(&Channel::new("C", 0, &e), 1000);
        assert!(f.clock > 0.9 && f.clock_local > 0.9, "{f:?}");
    }
}
