//! Analog channels for the checks (Python's `_engine/analog.py`): levels in volts (`check_voltage`), and analog
//! read as logic at a threshold (with hysteresis) for the logic checks.

use crate::cap::{Cap, Ch};
use crate::checks::{Out, V, out};
use crate::j::J;
use crate::jd;
use crate::pyfmt::g;
pub use wireskein_io::Analog;

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs().max(1) } else { gcd(b, a % b) }
}

/// A fraction reduced, its denominator positive.
pub fn frac(n: i128, d: i128) -> (i128, i128) {
    let g = gcd(n, d);
    let s = if d < 0 { -1 } else { 1 };
    (s * n / g, s * d / g)
}

fn ceil_div(n: i128, d: i128) -> i128 {
    let (n, d) = if d < 0 { (-n, -d) } else { (n, d) };
    n.div_euclid(d) + if n.rem_euclid(d) != 0 { 1 } else { 0 }
}

/// Ticks a sample: tick_hz / rate_hz.
fn per(tick_hz: (i64, i64), a: &Analog) -> (i128, i128) {
    frac(
        tick_hz.0 as i128 * a.rate_hz.1 as i128,
        tick_hz.1 as i128 * a.rate_hz.0 as i128,
    )
}

/// The samples of ticks [s0, s1), tick 0 at s0 (`verify._window`'s analog part).
pub fn window(a: &Analog, tick_hz: (i64, i64), s0: i64, s1: i64) -> Analog {
    let (pn, pd) = per(tick_hz, a);
    let (tn, td) = (a.t0_ticks.0 as i128, a.t0_ticks.1 as i128);
    // ceil((s - t0) / per) = ceil((s * td - tn) * pd / (td * pn))
    let at = |s: i64| ceil_div((s as i128 * td - tn) * pd, td * pn);
    let k0 = at(s0).max(0);
    let k1 = k0.max((a.values.len() as i128).min(at(s1)));
    // t0 + k0 * per - s0
    let (nn, nd) = frac(tn * pd + k0 * pn * td - s0 as i128 * td * pd, td * pd);
    let mut out = a.clone();
    out.values = a.values[k0 as usize..k1 as usize].to_vec();
    out.t0_ticks = (nn as i64, nd as i64);
    out
}

/// (low mask, high mask, low end V, high end V) of a raw channel's codes at an end of its converter (`clipped`).
fn clipped(a: &Analog) -> Option<(Vec<bool>, Vec<bool>, f64, f64)> {
    if a.encoding != "analog" || a.value_bits.is_none_or(|b| b == 0) {
        return None;
    }
    let (zero, scale) = (a.zero?, a.scale_nv?);
    let top = ((1i64 << a.value_bits.unwrap()) - 1) as f64;
    let to_v = |c: f64| (c - zero) * scale * 1e-9;
    let lo: Vec<bool> = a.values.iter().map(|&v| v <= 0.0).collect();
    let hi: Vec<bool> = a.values.iter().map(|&v| v >= top).collect();
    if scale < 0.0 {
        return Some((hi, lo, to_v(top), to_v(0.0)));
    }
    Some((lo, hi, to_v(0.0), to_v(top)))
}

/// A sum with the error of each addition carried (Neumaier): numpy's pairwise sums are as close.
fn fsum(v: &[f64]) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for &x in v {
        let t = s + x;
        c += if s.abs() >= x.abs() { (s - t) + x } else { (x - t) + s };
        s = t;
    }
    s + c
}

/// Mean, min, max, peak-to-peak and spread in volts of the samples inside the converter's range (`levels`).
fn levels(a: &Analog) -> Option<J> {
    let v = a.volts()?;
    let mut ok: Vec<bool> = v.iter().map(|x| x.is_finite()).collect();
    let mut extra = J::dict();
    if let Some((lo, hi, lo_v, hi_v)) = clipped(a) {
        let c = |m: &[bool]| m.iter().zip(&ok).filter(|(a, b)| **a && **b).count();
        extra.set("clipped_low", c(&lo));
        extra.set("clipped_high", c(&hi));
        extra.set("range_v", J::from(vec![lo_v, hi_v]));
        for i in 0..ok.len() {
            ok[i] &= !lo[i] && !hi[i];
        }
    }
    let v: Vec<f64> = v.iter().zip(&ok).filter(|(_, k)| **k).map(|(x, _)| *x).collect();
    let mut got = if v.is_empty() {
        jd! {"samples" => 0}
    } else {
        let n = v.len() as f64;
        let mean = fsum(&v) / n;
        let mn = v.iter().cloned().fold(f64::INFINITY, f64::min);
        let mx = v.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let std = (fsum(&v.iter().map(|x| (x - mean).powi(2)).collect::<Vec<_>>()) / n).sqrt();
        jd! {"samples" => v.len(), "mean_v" => mean, "min_v" => mn, "max_v" => mx, "p2p_v" => mx - mn, "std_v" => std}
    };
    for (k, x) in extra.items() {
        got.set(k, x.clone());
    }
    Some(got)
}

/// (low, high) of a threshold: one value or a pair.
fn thresholds(t: &J) -> Result<(f64, f64), String> {
    let (lo, hi) = match t {
        J::List(v) if v.len() == 2 => (v[0].f().unwrap_or(0.0), v[1].f().unwrap_or(0.0)),
        v => (v.f().unwrap_or(0.0), v.f().unwrap_or(0.0)),
    };
    if lo > hi {
        return Err(format!("threshold {}: low must be <= high", t.repr()));
    }
    Ok((lo, hi))
}

/// The level of each sample with hysteresis (`schmitt`).
fn schmitt(v: &[f64], low: f64, high: f64) -> Vec<u8> {
    let state: Vec<Option<u8>> = v
        .iter()
        .map(|&x| {
            if x >= high {
                Some(1)
            } else if x <= low {
                Some(0)
            } else {
                None
            }
        })
        .collect();
    let Some(first) = state.iter().position(|s| s.is_some()) else {
        let fin: Vec<f64> = v.iter().copied().filter(|x| !x.is_nan()).collect();
        let mid = if v.iter().any(|x| x.is_finite()) {
            fin.iter().sum::<f64>() / fin.len() as f64
        } else {
            0.0
        };
        return vec![(mid >= (low + high) / 2.0) as u8; v.len()];
    };
    let mut cur = state[first].unwrap();
    state
        .iter()
        .map(|s| {
            if let Some(x) = s {
                cur = *x;
            }
            cur
        })
        .collect()
}

/// The analog channel as a logic line at the threshold (`to_logic`).
pub fn to_logic(a: &Analog, tick_hz: (i64, i64), threshold: &J) -> Result<Ch, String> {
    let v = a
        .volts()
        .ok_or_else(|| format!("{}: no conversion to volts, so no threshold in volts applies", a.name))?;
    let (low, high) = thresholds(threshold)?;
    if let Some((_, _, lo_v, hi_v)) = clipped(a)
        && !(lo_v < low && low <= high && high < hi_v)
    {
        return Err(format!(
            "{}: threshold {}..{} V is not inside the frontend's range {lo_v:.3}..{hi_v:.3} V (the ends clip, so \
             they say only 'at or beyond')",
            a.name,
            g(low, 6),
            g(high, 6)
        ));
    }
    let lv = schmitt(&v, low, high);
    let (pn, pd) = per(tick_hz, a);
    let per = pn as f64 / pd as f64;
    let t0 = a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64;
    let mut edges: Vec<i64> = vec![];
    let mut new: Vec<u8> = vec![];
    for k in 1..lv.len() {
        if lv[k] != lv[k - 1] {
            let th = if lv[k] == 1 { high } else { low };
            let (b, af) = (v[k - 1], v[k]);
            let mut f = ((th - b) / (af - b)).clamp(0.0, 1.0);
            if !f.is_finite() {
                f = 1.0;
            }
            let t = t0 + (k as f64 - 1.0 + f) * per;
            edges.push((t - 1e-9).ceil() as i64);
            new.push(lv[k]);
        }
    }
    if edges.len() > 1 {
        let keep: Vec<bool> = (0..edges.len())
            .map(|i| i + 1 == edges.len() || edges[i + 1] > edges[i])
            .collect();
        if !keep.iter().all(|&k| k) {
            let e2: Vec<i64> = edges.iter().zip(&keep).filter(|(_, k)| **k).map(|(e, _)| *e).collect();
            let l2: Vec<u8> = new.iter().zip(&keep).filter(|(_, k)| **k).map(|(e, _)| *e).collect();
            let mut out = vec![];
            for i in 0..e2.len() {
                let flip = if i == 0 { l2[0] != lv[0] } else { l2[i] != l2[i - 1] };
                if flip {
                    out.push(e2[i]);
                }
            }
            edges = out;
        }
    }
    Ok(Ch {
        name: a.name.clone(),
        initial: lv.first().copied().unwrap_or(0),
        edges,
        step: 1.max(per.round_ties_even() as i64),
        phase: 0.max(t0.floor() as i64),
        reduction: None,
    })
}

/// Mean, range and ripple in volts (`verify.check_voltage`).
pub fn check_voltage(cap: &Cap, x: &J) -> Out {
    let pin = x.get("pin").s().unwrap_or("");
    let Some(a) = cap.analog.iter().find(|a| a.name == pin) else {
        return out(
            V::Unchecked,
            J::dict(),
            if cap.ch(pin).is_some() {
                "not an analog channel in this capture"
            } else {
                "pin not captured"
            },
        );
    };
    let Some(got) = levels(a) else {
        return out(
            V::Unchecked,
            jd! {"samples" => a.values.len()},
            "no conversion to volts in the file",
        );
    };
    let n = |k: &str| got.get(k).i().unwrap_or(0);
    let (c_lo, c_hi) = (n("clipped_low"), n("clipped_high"));
    let range = got.get("range_v").list();
    let lo_v = range.first().and_then(|v| v.f()).unwrap_or(f64::NAN);
    let hi_v = range.get(1).and_then(|v| v.f()).unwrap_or(f64::NAN);
    let samples = n("samples");
    if samples == 0 && c_lo == 0 && c_hi == 0 {
        return out(V::Ng, got, "no samples");
    }
    let f = |k: &str| got.get(k).f().unwrap_or(f64::NAN);
    let (mut ok, mut why, mut open) = (true, vec![], vec![]);
    if let Some(m) = x.get("level_min_v").f() {
        if c_lo > 0 && lo_v < m {
            ok = false;
            why.push(format!(
                "{c_lo} samples clipped at the low end (<= {lo_v:.3} V) < {} V",
                g(m, 6)
            ));
        } else if c_lo > 0 {
            open.push(format!(
                "{c_lo} samples clipped at <= {lo_v:.3} V: level_min_v {} V cannot be told",
                g(m, 6)
            ));
        } else if samples > 0 && f("min_v") < m {
            ok = false;
            why.push(format!("min {:.4} V < {} V", f("min_v"), g(m, 6)));
        }
        if c_hi > 0 && hi_v < m {
            open.push(format!(
                "{c_hi} samples clipped at >= {hi_v:.3} V: level_min_v {} V cannot be told",
                g(m, 6)
            ));
        }
    }
    if let Some(m) = x.get("level_max_v").f() {
        if c_hi > 0 && hi_v > m {
            ok = false;
            why.push(format!(
                "{c_hi} samples clipped at the high end (>= {hi_v:.3} V) > {} V",
                g(m, 6)
            ));
        } else if c_hi > 0 {
            open.push(format!(
                "{c_hi} samples clipped at >= {hi_v:.3} V: level_max_v {} V cannot be told",
                g(m, 6)
            ));
        } else if samples > 0 && f("max_v") > m {
            ok = false;
            why.push(format!("max {:.4} V > {} V", f("max_v"), g(m, 6)));
        }
        if c_lo > 0 && lo_v > m {
            open.push(format!(
                "{c_lo} samples clipped at <= {lo_v:.3} V: level_max_v {} V cannot be told",
                g(m, 6)
            ));
        }
    }
    if !x.get("level_v").is_null() || !x.get("ripple_max_v").is_null() {
        if c_lo > 0 || c_hi > 0 {
            open.push(format!(
                "{} samples clipped at an end of {lo_v:.3}..{hi_v:.3} V: mean and ripple cannot be told",
                c_lo + c_hi
            ));
        } else {
            if let Some(lv) = x.get("level_v").f() {
                let abs = x.get("level_abs_v").f().unwrap_or(0.05);
                if (f("mean_v") - lv).abs() > abs {
                    ok = false;
                    why.push(format!("mean {:.4} V vs {lv:.4} +- {} V", f("mean_v"), g(abs, 6)));
                }
            }
            if let Some(r) = x.get("ripple_max_v").f()
                && f("p2p_v") > r
            {
                ok = false;
                why.push(format!("peak-to-peak {:.4} V > {} V", f("p2p_v"), g(r, 6)));
            }
        }
    }
    if !ok {
        why.extend(open);
        return out(V::Ng, got, why.join("; "));
    }
    if !open.is_empty() {
        return out(V::Unchecked, got, open.join("; "));
    }
    out(V::Ok, got, "")
}
