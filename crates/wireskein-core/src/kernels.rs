//! Small kernels over a line's changes (Python's `_engine/kernels.py`, `views.py`).

use crate::view::Channel;

/// A line's runs over the part: (start, length, level) of each, the first from tick 0 and the last to the part's end.
pub fn runs(ch: &Channel, n: i64) -> (Vec<i64>, Vec<i64>, Vec<u8>) {
    let m = ch.edges.len() + 1;
    let (mut start, mut length, mut level) = (Vec::with_capacity(m), Vec::with_capacity(m), Vec::with_capacity(m));
    let mut at = 0i64;
    let mut lv = ch.initial;
    for &e in ch.edges {
        start.push(at);
        length.push(e - at);
        level.push(lv);
        at = e;
        lv ^= 1;
    }
    start.push(at);
    length.push(n - at);
    level.push(lv);
    (start, length, level)
}

/// Changes each within `w` ticks of the one before are one: an odd number of them a change at the last, an even
/// number none (a pulse). (`views.gated`)
pub fn gated(e: &[i64], w: f64) -> Vec<i64> {
    if e.len() < 2 || w < 2.0 {
        return e.to_vec();
    }
    let mut out = Vec::with_capacity(e.len());
    let mut run = 1usize;
    for i in 1..e.len() {
        if ((e[i] - e[i - 1]) as f64) >= w {
            if run % 2 == 1 {
                out.push(e[i - 1]);
            }
            run = 1;
        } else {
            run += 1;
        }
    }
    if run % 2 == 1 {
        out.push(e[e.len() - 1]);
    }
    out
}

/// The runs shorter than `k` taken out, the shortest first, each with its own two changes; of touching runs of a
/// width, every other from the first. (`kernels.shortest_first`, without `middle`)
pub fn shortest_first(edges: &[i64], k: i64) -> Vec<i64> {
    let mut e = edges.to_vec();
    for w in 1..k {
        loop {
            if e.len() < 2 {
                break;
            }
            let idx: Vec<usize> = (0..e.len() - 1).filter(|&i| e[i + 1] - e[i] == w).collect();
            if idx.is_empty() {
                break;
            }
            let mut drop = vec![false; e.len()];
            let mut pos = 0usize;
            for (j, &i) in idx.iter().enumerate() {
                pos = if j > 0 && idx[j - 1] + 1 == i { pos + 1 } else { 0 };
                if pos.is_multiple_of(2) {
                    drop[i] = true;
                    drop[i + 1] = true;
                }
            }
            e = e.iter().zip(&drop).filter(|&(_, &d)| !d).map(|(&x, _)| x).collect();
        }
    }
    e
}

/// Pulses shorter than `k` dropped: an edge within `k` of the previous kept edge cancels it (`kernels.deglitch`).
pub fn deglitch(e: &[i64], k: i64) -> Vec<i64> {
    deglitch_allow(e, k, None)
}

/// `deglitch`, where `allow[i]` says whether edge i may cancel.
pub fn deglitch_allow(e: &[i64], k: i64, allow: Option<&[bool]>) -> Vec<i64> {
    if e.len() < 2 {
        return e.to_vec();
    }
    let mut keep = vec![true; e.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..e.len() {
        if i > 0 && e[i] - e[i - 1] >= k {
            stack.clear();
        }
        match stack.last() {
            Some(&j) if e[i] - e[j] < k && allow.is_none_or(|a| a[i]) => {
                stack.pop();
                keep[j] = false;
                keep[i] = false;
            }
            _ => stack.push(i),
        }
    }
    e.iter().zip(&keep).filter(|&(_, &k)| k).map(|(&x, _)| x).collect()
}

/// A change and the changes within `w` ticks of it (not of each other) are one edge with its bounces: an odd number a
/// change at the first, an even number none. Where bounces ring on close to the next change (a clock of a few samples
/// more than its bounces a half), `debounce`'s chain runs into it; a window from the first change does not.
pub fn settled(e: &[i64], w: i64) -> Vec<i64> {
    let mut out = Vec::with_capacity(e.len());
    let mut i = 0;
    while i < e.len() {
        let j = i + e[i..].partition_point(|&x| x <= e[i] + w);
        if (j - i) % 2 == 1 {
            out.push(e[i]);
        }
        i = j;
    }
    out
}

/// Changes each within `w` of the one before are one edge with its bounces: an odd number a change at the first, an
/// even number none (`kernels.debounce`).
pub fn debounce(e: &[i64], w: i64) -> Vec<i64> {
    if e.len() < 2 || w < 2 {
        return e.to_vec();
    }
    let mut out = Vec::with_capacity(e.len());
    let (mut first, mut run) = (e[0], 1usize);
    for i in 1..e.len() {
        if e[i] - e[i - 1] >= w {
            if run % 2 == 1 {
                out.push(first);
            }
            first = e[i];
            run = 1;
        } else {
            run += 1;
        }
    }
    if run % 2 == 1 {
        out.push(first);
    }
    out
}

/// Index of the last element <= x (`searchsorted(a, x, side="right") - 1`), or -1.
pub fn last_at_or_before(a: &[i64], x: i64) -> isize {
    a.partition_point(|&v| v <= x) as isize - 1
}

/// The level of a line at each of the given ticks (ticks clipped to the part).
pub fn level_at(ch: &Channel, tick: i64) -> u8 {
    ch.level_at(tick)
}

/// Greedy non-overlapping chain: take start i, then the first start >= busy_until[i]. (`kernels.chain`) The starts
/// are sorted; the walk only moves forward.
pub fn chain(starts: &[f64], busy_until: &[f64]) -> Vec<usize> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < starts.len() {
        out.push(i);
        let b = busy_until[i];
        let mut j = i + 1;
        while j < starts.len() && starts[j] < b {
            j += 1;
        }
        i = j;
    }
    out
}

/// The level of a line at each of the sorted ticks, walking its changes once.
pub fn levels_at_sorted(ch: &crate::view::Channel, ticks: impl Iterator<Item = i64>) -> Vec<u8> {
    let e = ch.edges;
    let mut j = 0usize;
    ticks
        .map(|t| {
            while j < e.len() && e[j] <= t {
                j += 1;
            }
            ch.initial ^ (j as u8 & 1)
        })
        .collect()
}

pub fn quantile(sorted: &[f64], q: f64) -> f64 {
    // numpy's default (linear) on sorted data
    if sorted.is_empty() {
        return f64::NAN;
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    sorted[lo] + (sorted[hi] - sorted[lo]) * (pos - lo as f64)
}

pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    quantile(&s, 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gated_and_pulses() {
        assert_eq!(gated(&[0, 10, 11, 20, 21, 22, 40], 4.0), vec![0, 22, 40]);
        assert_eq!(shortest_first(&[0, 10, 11, 20], 3), vec![0, 20]);
        assert_eq!(shortest_first(&[0, 10, 11, 12, 20], 2), vec![0, 12, 20]);
        assert_eq!(deglitch(&[0, 1, 10, 20, 21, 22], 3), vec![10, 22]);
        assert_eq!(debounce(&[0, 1, 2, 10, 20, 21], 4), vec![0, 10]);
        // a change ringing on to its next one 10 ticks on: chained it is lost, from its first change it is kept
        let e = [0, 3, 5, 10, 13, 15];
        assert_eq!(debounce(&e, 7), Vec::<i64>::new());
        assert_eq!(settled(&e, 7), vec![0, 10]);
    }

    #[test]
    fn runs_and_chain() {
        let e = [3, 5];
        let (s, l, v) = runs(&Channel::new("A", 1, &e), 10);
        assert_eq!((s, l, v), (vec![0, 3, 5], vec![3, 2, 5], vec![1, 0, 1]));
        assert_eq!(chain(&[0.0, 1.0, 5.0, 6.0], &[4.0, 5.0, 9.0, 10.0]), vec![0, 2]);
        assert_eq!(quantile(&[1.0, 2.0, 3.0, 4.0], 0.5), 2.5);
    }
}
