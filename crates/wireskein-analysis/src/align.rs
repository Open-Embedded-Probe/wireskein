//! Aligning analog channels to the logic ticks, and one capture's ticks onto another's (Python's `wireskein.align`;
//! docs/wireskein-format.ja.md §5.1): the same signal on a logic channel and on an analog one (or on two probes'
//! channels) tells the offset and the time scale by its edges. Time t (ticks, as stored) becomes offset + scale * t.
//! The numbers are numpy's where it matters (its pairwise sums, linspace, arange); the straight line through the
//! matched edges is a least squares fit as np.polyfit's, by its centered form (the same to rounding).

use crate::analog;
use serde_json::{Map, Value, json};
use wireskein_io::Analog;

/// A channel's edges: times in the capture's ticks, the level after each, the resolution in ticks.
#[derive(Debug, Clone)]
pub struct Edges {
    pub t: Vec<f64>,
    pub pol: Vec<u8>,
    pub w: f64,
}

/// A logic channel's edges (each in the middle of the step it happened in).
pub fn logic_edges(edges: &[i64], initial: u8, step: i64) -> Edges {
    Edges {
        t: edges.iter().map(|&e| e as f64 - step as f64 / 2.0).collect(),
        pol: (0..edges.len()).map(|i| initial ^ (((i + 1) & 1) as u8)).collect(),
        w: step as f64,
    }
}

/// An analog channel's edges at a threshold (its crossings); the resolution one analog sample.
pub fn analog_edges(a: &Analog, tick_hz: (i64, i64), low: f64, high: f64) -> Result<Edges, String> {
    let (_, t, pol) = analog::crossings(a, tick_hz, low, high)?;
    Ok(Edges {
        t,
        pol,
        w: analog::ticks_per_sample(tick_hz, a),
    })
}

/// numpy's sum of a float64 array (pairwise, in blocks of 128 with 8 accumulators).
pub fn np_sum(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = -0.0;
        for &x in a {
            r += x;
        }
        r
    } else if n <= 128 {
        let mut r = [0.0; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - (n % 8) {
            for k in 0..8 {
                r[k] += a[i + k];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        np_sum(&a[..n2]) + np_sum(&a[n2..])
    }
}

fn mean(a: &[f64]) -> f64 {
    np_sum(a) / a.len() as f64
}

fn median(a: &[f64]) -> f64 {
    let mut v = a.to_vec();
    v.sort_by(|x, y| x.total_cmp(y));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn rms(a: &[f64]) -> f64 {
    let sq: Vec<f64> = a.iter().map(|x| x * x).collect();
    mean(&sq).sqrt()
}

/// The straight line y = off + scale * x through the points by least squares (np.polyfit(x, y, 1)) -> (scale, off).
pub fn line_fit(x: &[f64], y: &[f64]) -> (f64, f64) {
    let (mx, my) = (mean(x), mean(y));
    let dx: Vec<f64> = x.iter().map(|v| v - mx).collect();
    let sxx = np_sum(&dx.iter().map(|d| d * d).collect::<Vec<_>>());
    let sxy = np_sum(&dx.iter().zip(y).map(|(d, v)| d * (v - my)).collect::<Vec<_>>());
    let scale = sxy / sxx;
    (scale, my - scale * mx)
}

/// np.searchsorted(r, x) (side left).
fn searchsorted(r: &[f64], x: f64) -> usize {
    r.partition_point(|v| *v < x)
}

/// Pairs (index in t, index in ref): each edge of t, moved by the model, to the nearest reference edge of the same
/// direction within tol (one per edge of t) (Python's `align._match`).
fn matched(
    rf: &Edges,
    t: &[f64],
    pol: &[u8],
    off: f64,
    scale: f64,
    tol: &dyn Fn(usize) -> f64,
) -> (Vec<usize>, Vec<usize>) {
    let (mut out_t, mut out_r) = (vec![], vec![]);
    for p in [0u8, 1] {
        let ri: Vec<usize> = (0..rf.t.len()).filter(|&i| rf.pol[i] == p).collect();
        let r: Vec<f64> = ri.iter().map(|&i| rf.t[i]).collect();
        let ti: Vec<usize> = (0..t.len()).filter(|&i| pol[i] == p).collect();
        if r.is_empty() || ti.is_empty() {
            continue;
        }
        for &i in &ti {
            let x = off + scale * t[i];
            let right = searchsorted(&r, x).min(r.len() - 1);
            let left = right.saturating_sub(1);
            let j = if (r[left] - x).abs() <= (r[right] - x).abs() {
                left
            } else {
                right
            };
            if (r[j] - x).abs() <= tol(i) {
                out_t.push(i);
                out_r.push(ri[j]);
            }
        }
    }
    (out_t, out_r)
}

/// np.unique(bins, return_counts=True).
fn unique_counts(mut b: Vec<i64>) -> (Vec<i64>, Vec<i64>) {
    b.sort_unstable();
    let (mut v, mut c) = (vec![], vec![]);
    for x in b {
        if v.last() == Some(&x) {
            *c.last_mut().unwrap() += 1;
        } else {
            v.push(x);
            c.push(1);
        }
    }
    (v, c)
}

fn round_i(x: f64) -> i64 {
    x.round_ties_even() as i64
}

/// numpy's arange(start, stop, step) of floats (its values start + i * (start + step - start)).
fn np_arange(start: f64, stop: f64, step: f64) -> Vec<f64> {
    let n = ((stop - start) / step).ceil().max(0.0) as usize;
    if n == 0 {
        return vec![];
    }
    let delta = (start + step) - start;
    let mut out = vec![start];
    if n > 1 {
        out.push(start + step);
    }
    for i in 2..n {
        out.push(start + i as f64 * delta);
    }
    out
}

/// The alignment of the analog channel `a` (its edges at the threshold) against the logic channel `rf` (Python's
/// `align.find`, its entry for a channel; the caller names the channels it applies to). window_ticks: how far off
/// the start may be (None: 3 uncertainties and 4 analog samples, or a tenth of the capture without one);
/// uncertainty_ticks: the start uncertainties the probe reported, in ticks.
#[allow(clippy::too_many_arguments)]
pub fn find(
    rf: &Edges,
    reference: &str,
    a: &Analog,
    tick_hz: (i64, i64),
    low: f64,
    high: f64,
    window_ticks: Option<f64>,
    uncertainty_ticks: Option<f64>,
    n_samples: i64,
    max_ppm: f64,
) -> Result<Map<String, Value>, String> {
    let via = &a.name;
    let tick = tick_hz.0 as f64 / tick_hz.1 as f64;
    let per = analog::ticks_per_sample(tick_hz, a);
    let (r_t, r_pol) = (&rf.t, &rf.pol);
    let (_, a_t, a_pol) = analog::crossings(a, tick_hz, low, high)?;
    if r_t.len() < 2 || a_t.len() < 2 {
        return Err(format!(
            "too few edges to align: {reference} has {}, {via} has {}",
            r_t.len(),
            a_t.len()
        ));
    }
    let window_ticks = match (window_ticks, uncertainty_ticks) {
        (Some(w), _) => w,
        (None, Some(u)) if u != 0.0 => 3.0 * u + 4.0 * per,
        _ => n_samples as f64 / 10.0,
    };
    // 1. offset and scale together: for each scale on a grid fine enough that the drift over the edges used stays
    //    under half a bin, the most common difference between an analog edge (scaled) and a reference edge of the
    //    same direction within the window; the pair with the most agreeing edges wins
    let width = per.max(rf.w);
    let num = a_t.len().min(512);
    let mut use_: Vec<usize> = if num == 1 {
        vec![0]
    } else {
        let stop = (a_t.len() - 1) as f64;
        let step = stop / (num - 1) as f64;
        (0..num)
            .map(|i| if i + 1 == num { stop } else { i as f64 * step })
            .map(|v| v.round_ties_even() as usize)
            .collect()
    };
    use_.dedup();
    let t_use: Vec<f64> = use_.iter().map(|&i| a_t[i]).collect();
    let p_use: Vec<u8> = use_.iter().map(|&i| a_pol[i]).collect();
    let (tmin, tmax) = t_use
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &x| (a.min(x), b.max(x)));
    let span = (tmax - tmin).max(width);
    let reach = window_ticks + max_ppm * 1e-6 * t_use.iter().fold(0.0f64, |m, x| m.max(x.abs()));
    let (mut pt, mut pr) = (vec![], vec![]);
    for p in [0u8, 1] {
        let r: Vec<f64> = (0..r_t.len()).filter(|&i| r_pol[i] == p).map(|i| r_t[i]).collect();
        for (k, &t) in t_use.iter().enumerate() {
            if p_use[k] != p {
                continue;
            }
            let (lo, hi) = (searchsorted(&r, t - reach), searchsorted(&r, t + reach));
            pr.extend_from_slice(&r[lo..hi]);
            pt.extend(std::iter::repeat_n(t, hi - lo));
        }
    }
    if pt.is_empty() {
        return Err(format!(
            "no edge of {reference} within {:.1} us of an edge of {via}",
            window_ticks / tick * 1e6
        ));
    }
    let step = width / (2.0 * span);
    let t_first = a_t[0];
    let scales: Vec<f64> = np_arange(-max_ppm * 1e-6, max_ppm * 1e-6 + step / 2.0, step)
        .into_iter()
        .map(|x| 1.0 + x)
        .collect();
    let mut best = (0i64, 1.0f64, 0i64, 0i64); // (count, scale, bin, rival count)
    let mut bins = Vec::with_capacity(pt.len());
    for &sc in &scales {
        bins.clear();
        for (x, y) in pt.iter().zip(&pr) {
            let d = y - sc * x;
            if (d + (sc - 1.0) * t_first).abs() <= window_ticks {
                bins.push(round_i(d / width));
            }
        }
        if bins.is_empty() {
            continue;
        }
        let (values, counts) = unique_counts(bins.clone());
        let top = (0..counts.len()).fold(0, |b, i| if counts[i] > counts[b] { i } else { b });
        if counts[top] > best.0 {
            let rival = (0..values.len())
                .filter(|&i| (values[i] - values[top]).abs() > 2)
                .map(|i| counts[i])
                .max()
                .unwrap_or(0);
            best = (counts[top], sc, values[top], rival);
        }
    }
    let (count, mut scale, b, rival) = best;
    if count < 2 {
        return Err(format!("the edges of {via} and {reference} do not agree on any offset"));
    }
    if rival as f64 >= 0.8 * count as f64 {
        return Err(format!(
            "ambiguous: another offset fits about as well ({rival} vs {count} edges; a periodic signal: give a \
             smaller --max-offset, or align on a marker pulse)"
        ));
    }
    let mut off = b as f64 * width;
    // 2. refine: match every edge with the model, fit offset and scale, and match again with the new model
    let mut tol = 1.5 * width;
    for _ in 0..4 {
        let (ti, ri) = matched(rf, &a_t, &a_pol, off, scale, &|_| tol);
        if ti.len() < 2 {
            break;
        }
        let x: Vec<f64> = ti.iter().map(|&i| a_t[i]).collect();
        let y: Vec<f64> = ri.iter().map(|&i| r_t[i]).collect();
        let (lo, hi) = x
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
        if hi - lo > 10.0 * width {
            (scale, off) = line_fit(&x, &y);
        } else {
            off = median(&x.iter().zip(&y).map(|(x, y)| y - scale * x).collect::<Vec<_>>());
        }
        let resid: Vec<f64> = x.iter().zip(&y).map(|(x, y)| y - (off + scale * x)).collect();
        tol = (1.5 * width).max(4.0 * rms(&resid));
    }
    let (ti, ri) = matched(rf, &a_t, &a_pol, off, scale, &|_| tol);
    if ti.len() < 2 {
        return Err(format!("only {} edges of {via} match {reference}", ti.len()));
    }
    let resid: Vec<f64> = ti
        .iter()
        .zip(&ri)
        .map(|(&i, &j)| r_t[j] - (off + scale * a_t[i]))
        .collect();
    // the edges that could match: those that land where the reference has edges
    let (r0, r1) = (r_t[0], r_t[r_t.len() - 1]);
    let overlap = a_t
        .iter()
        .map(|t| off + scale * t)
        .filter(|m| *m >= r0 - tol && *m <= r1 + tol)
        .count();
    let x: Vec<f64> = ti.iter().map(|&i| a_t[i]).collect();
    let mx = mean(&x);
    let spread = np_sum(&x.iter().map(|v| (v - mx) * (v - mx)).collect::<Vec<_>>());
    let dof = (ti.len() as f64 - 2.0).max(1.0);
    let scale_err = if spread > 0.0 {
        (np_sum(&resid.iter().map(|r| r * r).collect::<Vec<_>>()) / dof / spread).sqrt()
    } else {
        f64::INFINITY
    };
    let t0 = a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64;
    let mut e = Map::new();
    for (k, v) in [
        ("offset_ticks", json!(off)),
        ("scale", json!(scale)),
        ("reference", json!(reference)),
        ("via", json!(via)),
        ("method", json!("edges")),
        ("matched", json!(ti.len())),
        ("edges", json!(a_t.len())),
        ("overlap_edges", json!(overlap)),
        ("overlap_s", json!((r1 - r0) / tick)),
        ("residual_ticks", json!(rms(&resid))),
        ("scale_ppm_uncertainty", float(scale_err * 1e6)),
        ("threshold_v", json!([low, high])),
        ("window_ticks", json!(window_ticks)),
        ("offset_us", json!(off / tick * 1e6)),
        ("scale_ppm", json!((scale - 1.0) * 1e6)),
        ("start_shift_us", json!((off + (scale - 1.0) * t0) / tick * 1e6)),
    ] {
        e.insert(k.into(), v);
    }
    Ok(e)
}

/// A float as JSON; infinity as Python's json writes it is not JSON: a string "Infinity" for the caller to read back.
fn float(x: f64) -> Value {
    if x.is_finite() { json!(x) } else { json!("Infinity") }
}

/// How this capture's ticks map onto another capture's (Python's `align.between`): the same signal on `rf` (a logic
/// channel of the reference capture) and `t` (this capture's channel). tick_ref, tick: the two captures' tick rates;
/// ref_n: the reference capture's ticks (the window by default); threshold: the analog `t`'s, said in the entry.
#[allow(clippy::too_many_arguments)]
pub fn between(
    rf: &Edges,
    reference: &str,
    t: &Edges,
    via: &str,
    tick_ref: f64,
    tick: f64,
    ref_n: i64,
    window_ticks: Option<f64>,
    max_ppm: f64,
    threshold: Option<(f64, f64)>,
) -> Result<Map<String, Value>, String> {
    let (r_t, r_pol) = (&rf.t, &rf.pol);
    if r_t.len() < 2 || t.t.len() < 2 {
        return Err(format!(
            "too few edges: {reference} has {}, {via} has {}",
            r_t.len(),
            t.t.len()
        ));
    }
    let s0 = tick_ref / tick; // reference ticks per tick here
    let fine = rf.w.max(t.w * s0);
    let window_ticks = window_ticks.unwrap_or(ref_n as f64);
    // 1. a coarse offset from the first edges: the most common difference to a reference edge of the same
    //    direction, in bins wide enough that the clocks cannot drift out of one over these edges
    let k = 8.max(64.min((20_000_000 / r_t.len().max(1)) as i64)) as usize;
    let head = &t.t[..k.min(t.t.len())];
    let hpol = &t.pol[..head.len()];
    let span = (head[head.len() - 1] - head[0]) * s0;
    let width = fine.max(4.0 * max_ppm * 1e-6 * span);
    let refs: [Vec<f64>; 2] = [0u8, 1].map(|p| (0..r_t.len()).filter(|&i| r_pol[i] == p).map(|i| r_t[i]).collect());
    let mut d = vec![];
    for (x, p) in head.iter().zip(hpol) {
        d.extend(
            refs[*p as usize]
                .iter()
                .map(|r| r - s0 * x)
                .filter(|v| v.abs() <= window_ticks),
        );
    }
    if d.is_empty() {
        return Err(format!("no edges of {reference} within the window"));
    }
    let bins: Vec<i64> = d.iter().map(|v| round_i(v / width)).collect();
    let (values, counts) = unique_counts(bins.clone());
    // the most common bin (numpy's argsort reversed: of equal counts, the last)
    let top = (0..counts.len()).fold(0, |b, i| if counts[i] >= counts[b] { i } else { b });
    let best = values[top];
    let rival = (0..values.len())
        .filter(|&i| (values[i] - best).abs() > 2)
        .map(|i| counts[i])
        .max();
    if counts[top] < 2 || rival.is_some_and(|r| r as f64 >= 0.8 * counts[top] as f64) {
        return Err(format!(
            "ambiguous: the first {} edges of {via} fit several offsets (a periodic signal: give a smaller \
             --max-offset, or align on an irregular marker pulse)",
            head.len()
        ));
    }
    let near: Vec<f64> = d
        .iter()
        .zip(&bins)
        .filter(|(_, b)| (*b - best).abs() <= 1)
        .map(|(v, _)| *v)
        .collect();
    let (mut off, mut scale) = (median(&near), s0);
    // 2. refine over more and more of the edges; the tolerance grows with how uncertain the extrapolation is
    let mut order: Vec<usize> = (0..t.t.len()).collect();
    order.sort_by(|&a, &b| t.t[a].total_cmp(&t.t[b]));
    let tt: Vec<f64> = order.iter().map(|&i| t.t[i]).collect();
    let pp: Vec<u8> = order.iter().map(|&i| t.pol[i]).collect();
    let (mut se, mut rms_, mut center) = (max_ppm * 1e-6 * s0, width / 4.0, tt[0]);
    for frac in [0.02, 0.1, 0.3, 1.0] {
        let n = 16.max((tt.len() as f64 * frac) as usize).min(tt.len());
        let (ts, ps) = (&tt[..n], &pp[..n]);
        let base = (1.5 * fine).max(4.0 * rms_);
        let (ti, ri) = matched(rf, ts, ps, off, scale, &|i| base + 3.0 * se * (ts[i] - center).abs());
        if ti.len() < 3 {
            continue;
        }
        let x: Vec<f64> = ti.iter().map(|&i| ts[i]).collect();
        let y: Vec<f64> = ri.iter().map(|&i| r_t[i]).collect();
        let (lo, hi) = x
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
        if (hi - lo) * s0 > 10.0 * fine {
            (scale, off) = line_fit(&x, &y);
            let resid: Vec<f64> = x.iter().zip(&y).map(|(x, y)| y - (off + scale * x)).collect();
            rms_ = rms(&resid);
            let mx = mean(&x);
            let spread = np_sum(&x.iter().map(|v| (v - mx) * (v - mx)).collect::<Vec<_>>());
            if spread > 0.0 {
                se = (np_sum(&resid.iter().map(|r| r * r).collect::<Vec<_>>())
                    / (x.len() as f64 - 2.0).max(1.0)
                    / spread)
                    .sqrt();
            }
            center = mx;
        } else {
            off = median(&x.iter().zip(&y).map(|(x, y)| y - scale * x).collect::<Vec<_>>());
        }
    }
    let base = (1.5 * fine).max(4.0 * rms_);
    let (ti, ri) = matched(rf, &tt, &pp, off, scale, &|i| base + 3.0 * se * (tt[i] - center).abs());
    if ti.len() < 3 {
        return Err(format!("only {} edges of {via} match {reference}", ti.len()));
    }
    let resid: Vec<f64> = ti
        .iter()
        .zip(&ri)
        .map(|(&i, &j)| r_t[j] - (off + scale * tt[i]))
        .collect();
    let (r0, r1) = (r_t[0], r_t[r_t.len() - 1]);
    let overlap = tt
        .iter()
        .map(|x| off + scale * x)
        .filter(|m| *m >= r0 - fine && *m <= r1 + fine)
        .count();
    let mut e = Map::new();
    for (k, v) in [
        ("offset_ticks", json!(off)),
        ("scale", json!(scale)),
        ("reference", json!(reference)),
        ("via", json!(via)),
        ("method", json!("edges")),
        ("matched", json!(ti.len())),
        ("edges", json!(tt.len())),
        ("overlap_edges", json!(overlap)),
        ("residual_ticks", json!(rms(&resid))),
        ("scale_ppm", json!((scale / s0 - 1.0) * 1e6)),
        ("scale_ppm_uncertainty", json!(se / s0 * 1e6)),
        ("offset_us", json!(off / tick_ref * 1e6)),
    ] {
        e.insert(k.into(), v);
    }
    if let Some((lo, hi)) = threshold {
        e.insert("threshold_v".into(), json!([lo, hi]));
    }
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numpy_sums_and_ranges() {
        let a: Vec<f64> = (0..1000).map(|i| 0.1 * i as f64).collect();
        assert!((np_sum(&a) - 49950.0).abs() < 1e-9);
        assert_eq!(np_arange(-1e-3, 1e-3 + 2.5e-4, 5e-4).len(), 5);
        let (s, o) = line_fit(&[0.0, 1.0, 2.0, 3.0], &[1.0, 3.0, 5.0, 7.0]);
        assert!((s - 2.0).abs() < 1e-12 && (o - 1.0).abs() < 1e-12);
    }

    #[test]
    fn a_shifted_copy_of_the_reference_lines_up() {
        // a reference with irregular edges; its copy on an ADC at 1 MS/s (ticks at 10 MHz), started 1234 ticks late
        let edges: Vec<i64> = (0..60).map(|i| 1000 + i * 997 + (i * i * 37) % 400).collect();
        let rf = logic_edges(&edges, 0, 1);
        let n = 90_000;
        let mut values = vec![0.0; n / 10];
        for (k, v) in values.iter_mut().enumerate() {
            let t = (k * 10) as i64 + 1234;
            let lvl = edges.iter().filter(|&&e| e <= t).count() % 2;
            *v = 3.3 * lvl as f64;
        }
        let a = Analog {
            name: "A".into(),
            encoding: "analog-f32".into(),
            rate_hz: (1_000_000, 1),
            t0_ticks: (0, 1),
            width: 32,
            value_bits: None,
            zero: None,
            scale_nv: None,
            unit: "V".into(),
            values,
            acquisition: Default::default(),
        };
        let e = find(
            &rf,
            "R",
            &a,
            (10_000_000, 1),
            1.65,
            1.65,
            Some(3000.0),
            None,
            n as i64,
            5000.0,
        )
        .unwrap();
        let off = e["offset_ticks"].as_f64().unwrap();
        assert!((off - 1234.0).abs() < 10.0, "{e:?}");
        assert_eq!(e["matched"], json!(edges.len() - 1));
    }
}
