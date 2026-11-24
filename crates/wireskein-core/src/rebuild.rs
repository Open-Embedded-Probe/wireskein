//! A clock rebuilt from its period (Python's `views.clock_refused`, `views.clock_followed`): bounces as long as its
//! half period are no width a filter tells from its own runs, so its changes are taken where its period puts them.

pub const REFUSE: f64 = 0.8; // refuse: after a change it keeps, nothing within this share of the half period
pub const TOL: f64 = 0.34; // follow: the next change within this share of a half period of where it is due
pub const BURST: f64 = 3.0; // half periods: a gap this long ends a burst (each burst rebuilt on its own)

/// A clock's half period, from its longer runs (a bounce's are short).
pub fn half(e: &[i64]) -> f64 {
    let mut d: Vec<f64> = e.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.is_empty() {
        return 0.0;
    }
    let big: Vec<f64> = if d.len() > 4 {
        let mut s = d.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let p75 = crate::kernels::quantile(&s, 0.75);
        d.retain(|&x| x >= 0.5 * p75);
        d
    } else {
        d
    };
    crate::kernels::median(&big)
}

fn bursts(e: &[i64], h: f64) -> Vec<&[i64]> {
    let mut out = vec![];
    let mut a = 0;
    for i in 1..e.len() {
        if (e[i] - e[i - 1]) as f64 > BURST * h {
            out.push(&e[a..i]);
            a = i;
        }
    }
    if a < e.len() {
        out.push(&e[a..]);
    }
    out
}

/// Changes in order, two at one tick cancelling.
fn odd(mut out: Vec<i64>) -> Vec<i64> {
    out.sort();
    let mut r: Vec<i64> = vec![];
    let mut i = 0;
    while i < out.len() {
        let mut j = i;
        while j < out.len() && out[j] == out[i] {
            j += 1;
        }
        if (j - i) % 2 == 1 {
            r.push(out[i]);
        }
        i = j;
    }
    r
}

/// A rebuilt burst ends on the level the line holds after it.
fn ends_as_raw(out: &mut Vec<i64>, lv: &mut u8, raw: u8) {
    if *lv != raw && !out.is_empty() {
        out.pop();
        *lv ^= 1;
    }
}

/// In order: a change kept, nothing within REFUSE of a half period after it, a change put back where a gap of 1.5 -
/// 2.5 half periods says the noise cancelled one; each burst ends on the raw level.
pub fn refused(e: &[i64], initial: u8) -> Vec<i64> {
    let h = if e.len() >= 8 { half(e) } else { 0.0 };
    if h < 2.0 {
        return e.to_vec();
    }
    let mut out = vec![];
    let (mut lv, mut raw) = (initial, initial);
    for b in bursts(e, h) {
        raw ^= (b.len() & 1) as u8;
        let mut last = i64::MIN / 2;
        for &t in b {
            let gap = (t - last) as f64;
            if gap >= REFUSE * h {
                if (1.5 * h..2.5 * h).contains(&gap) {
                    out.push(last + h.round() as i64);
                    lv ^= 1;
                }
                out.push(t);
                lv ^= 1;
                last = t;
            }
        }
        ends_as_raw(&mut out, &mut lv, raw);
    }
    odd(out)
}

/// As a PLL: per burst, from its first change, each next the change nearest where a half period puts it (within TOL
/// of one), or that place itself when none is there.
pub fn followed(e: &[i64], initial: u8) -> Vec<i64> {
    let hg = if e.len() >= 8 { half(e) } else { 0.0 };
    if hg < 2.0 {
        return e.to_vec();
    }
    let mut out = vec![];
    let (mut lv, mut raw) = (initial, initial);
    for b in bursts(e, hg) {
        raw ^= (b.len() & 1) as u8;
        if b.len() < 4 {
            for &t in b {
                out.push(t);
                lv ^= 1;
            }
            ends_as_raw(&mut out, &mut lv, raw);
            continue;
        }
        let bd: Vec<f64> = b
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64)
            .filter(|&x| x >= 0.5 * hg)
            .collect();
        let h = if bd.is_empty() { hg } else { crate::kernels::median(&bd) };
        let (mut t, mut i, end, tol) = (b[0], 0usize, b[b.len() - 1], TOL * h);
        out.push(t);
        lv ^= 1;
        let mut steps = 2 * b.len() + 16;
        while (t as f64) + 0.5 * h < end as f64 && steps > 0 {
            steps -= 1;
            let due = t as f64 + h;
            while i < b.len() && (b[i] as f64) < due - tol {
                i += 1;
            }
            let mut j = i;
            let mut best: Option<i64> = None;
            while j < b.len() && (b[j] as f64) <= due + tol {
                if best.is_none_or(|x| (b[j] as f64 - due).abs() < (x as f64 - due).abs()) {
                    best = Some(b[j]);
                }
                j += 1;
            }
            t = best.unwrap_or(due.round() as i64);
            out.push(t);
            lv ^= 1;
        }
        ends_as_raw(&mut out, &mut lv, raw);
    }
    odd(out)
}

pub const GRID: f64 = 0.6; // grid: a change within this many ticks of a point of the grid is the change there

/// As a grid (Python's `views.clock_gridded`): per burst, from its first change, points every h - the fractional half
/// period within 0.6-1.4 of the burst's that puts the most changes within GRID ticks of a point (less half the points
/// with none) - each moved to the change there if any; an error at one point does not carry to the next (a clock of
/// a few samples a half, 3 and 4 for 3.2, has no integer half to follow).
pub fn gridded(e: &[i64], initial: u8) -> Vec<i64> {
    let hg = if e.len() >= 8 { half(e) } else { 0.0 };
    if hg < 2.0 {
        return e.to_vec();
    }
    let mut out = vec![];
    let (mut lv, mut raw) = (initial, initial);
    for b in bursts(e, hg) {
        raw ^= (b.len() & 1) as u8;
        if b.len() < 6 {
            for &t in b {
                out.push(t);
                lv ^= 1;
            }
            ends_as_raw(&mut out, &mut lv, raw);
            continue;
        }
        let bf: Vec<f64> = b.iter().map(|&x| x as f64).collect();
        let bd: Vec<f64> = bf.windows(2).map(|w| w[1] - w[0]).filter(|&x| x >= 0.5 * hg).collect();
        let h0 = if bd.is_empty() { hg } else { crate::kernels::median(&bd) };
        let (first, last) = (bf[0], bf[bf.len() - 1]);
        // the point k of a grid of half h: the change nearest it, and how far
        let nearest = |g: f64| -> (f64, f64) {
            let i = bf.partition_point(|&x| x < g).clamp(1, bf.len() - 1);
            let (a, c) = (bf[i - 1], bf[i]);
            if (c - g).abs() <= (a - g).abs() {
                (c, (c - g).abs())
            } else {
                (a, (a - g).abs())
            }
        };
        let step = (0.004 * h0).max(0.01);
        let (mut best_h, mut best_s) = (h0, f64::NEG_INFINITY);
        let mut h = 0.6 * h0;
        while h < 1.4 * h0 {
            let n = ((last - first) / h + 0.5).floor() as usize + 1;
            let mut s = 0.0;
            // (the points in order: the change at or after each found walking on)
            let mut i = 1usize;
            for k in 0..n {
                let g = first + h * k as f64;
                while i < bf.len() - 1 && bf[i] < g {
                    i += 1;
                }
                let d = (bf[i] - g).abs().min((bf[i - 1] - g).abs());
                s += if d <= GRID { 1.0 } else { -0.5 };
            }
            if s > best_s {
                (best_h, best_s) = (h, s);
            }
            h += step;
        }
        let n = ((last - first) / best_h + 0.5).floor() as usize + 1;
        let mut pts: Vec<i64> = (0..n)
            .map(|k| {
                let g = first + best_h * k as f64;
                let (t, d) = nearest(g);
                if d <= GRID { t as i64 } else { g.round() as i64 }
            })
            .collect();
        pts.sort_unstable();
        pts.dedup();
        for t in pts {
            out.push(t);
            lv ^= 1;
        }
        ends_as_raw(&mut out, &mut lv, raw);
    }
    odd(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bounced_clock_comes_back() {
        // a clock of half period 6, a bounce (back and forth within 3 ticks) after some of its edges
        let clean: Vec<i64> = (0..64).map(|k| 100 + 6 * k).collect();
        let mut e = vec![];
        for (k, &t) in clean.iter().enumerate() {
            e.push(t);
            if k % 5 == 2 {
                e.push(t + 2);
                e.push(t + 3);
            }
        }
        assert_eq!(refused(&e, 0), clean);
        assert_eq!(followed(&e, 0), clean);
        assert_eq!(gridded(&e, 0), clean);
    }
}
