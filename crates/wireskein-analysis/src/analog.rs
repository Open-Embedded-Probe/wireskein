//! Analog channels read as logic (Python's `wireskein._engine.analog`): a threshold in volts, one value or (low,
//! high) for hysteresis - the level goes high when a sample reaches `high`, low when one reaches `low`, and keeps its
//! level in between. Each edge is placed where the line between the two samples around it crosses the threshold,
//! rounded up to a tick; the channel's step is one analog sample.

use serde_json::{Value, json};
use wireskein_io::{Analog, Line};

/// (low, high) of a threshold; an error when low > high.
pub fn thresholds(low: f64, high: f64) -> Result<(f64, f64), String> {
    if low > high {
        return Err(format!(
            "threshold ({}, {}): low must be <= high",
            wireskein_io::pyfmt::repr_f64(low),
            wireskein_io::pyfmt::repr_f64(high)
        ));
    }
    Ok((low, high))
}

/// The level (0/1) of each sample with hysteresis; a NaN sample keeps the level (Python's `analog.schmitt`).
pub fn schmitt(v: &[f64], low: f64, high: f64) -> Vec<u8> {
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
        // never reached either threshold: the side of the middle it is on
        let fin: Vec<f64> = v.iter().copied().filter(|x| !x.is_nan()).collect();
        let mid = if v.iter().any(|x| x.is_finite()) && !fin.is_empty() {
            fin.iter().sum::<f64>() / fin.len() as f64
        } else {
            0.0
        };
        return vec![u8::from(mid >= (low + high) / 2.0); v.len()];
    };
    let mut out = Vec::with_capacity(v.len());
    let mut cur = state[first].unwrap();
    for s in &state {
        if let Some(x) = s {
            cur = *x;
        }
        out.push(cur);
    }
    // before the first decision: the level it settles to (already: cur started there)
    for o in out.iter_mut().take(first) {
        *o = state[first].unwrap();
    }
    out
}

/// Ticks a sample of the channel: tick_hz / rate_hz.
pub fn ticks_per_sample(tick_hz: (i64, i64), a: &Analog) -> f64 {
    let num = tick_hz.0 as i128 * a.rate_hz.1 as i128;
    let den = tick_hz.1 as i128 * a.rate_hz.0 as i128;
    num as f64 / den as f64
}

fn t0(a: &Analog) -> f64 {
    a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64
}

/// Where a raw channel sat at an end of its converter: (low end V, high end V), or None when the file cannot tell
/// (Python's `analog.clipped`, its ends).
pub fn clip_ends(a: &Analog) -> Option<(f64, f64)> {
    let (Some(bits), Some(z), Some(s)) = (a.value_bits, a.zero, a.scale_nv) else {
        return None;
    };
    if a.encoding != "analog" || bits == 0 {
        return None;
    }
    let top = ((1i64 << bits) - 1) as f64;
    let to_v = |c: f64| (c - z) * s * 1e-9;
    Some(if s < 0.0 {
        (to_v(top), to_v(0.0))
    } else {
        (to_v(0.0), to_v(top))
    })
}

/// Levels per sample, crossing times in ticks, the new level at each.
pub type Crossings = (Vec<u8>, Vec<f64>, Vec<u8>);

/// (levels per sample, crossing times in ticks, the new level at each) (Python's `analog.crossings`).
pub fn crossings(a: &Analog, tick_hz: (i64, i64), low: f64, high: f64) -> Result<Crossings, String> {
    let v = a
        .volts()
        .ok_or_else(|| format!("{}: no conversion to volts, so no threshold in volts applies", a.name))?;
    let (low, high) = thresholds(low, high)?;
    if let Some((lo, hi)) = clip_ends(a)
        && !(lo < low && low <= high && high < hi)
    {
        return Err(format!(
            "{}: threshold {}..{} V is not inside the frontend's range {lo:.3}..{hi:.3} V (the ends clip, so they \
             say only 'at or beyond')",
            a.name,
            wireskein_io::pyfmt::g(low, 6),
            wireskein_io::pyfmt::g(high, 6)
        ));
    }
    let lv = schmitt(&v, low, high);
    let per = ticks_per_sample(tick_hz, a);
    let t0 = t0(a);
    let (mut times, mut new) = (vec![], vec![]);
    for k in 1..lv.len() {
        if lv[k] == lv[k - 1] {
            continue;
        }
        let th = if lv[k] == 1 { high } else { low };
        let (before, after) = (v[k - 1], v[k]);
        let f = (th - before) / (after - before);
        let f = if f.is_nan() { f } else { f.clamp(0.0, 1.0) };
        let f = if f.is_finite() { f } else { 1.0 };
        times.push(t0 + ((k - 1) as f64 + f) * per);
        new.push(lv[k]);
    }
    Ok((lv, times, new))
}

/// The analog channel as a logic line (Python's `analog.to_logic`): its acquisition with `threshold_v`.
pub fn to_logic(a: &Analog, tick_hz: (i64, i64), low: f64, high: f64) -> Result<Line, String> {
    let (lv, times, new) = crossings(a, tick_hz, low, high)?;
    let per = ticks_per_sample(tick_hz, a);
    let t0 = t0(a);
    let mut edges: Vec<i64> = times.iter().map(|t| (t - 1e-9).ceil() as i64).collect();
    if edges.len() > 1 && edges.windows(2).any(|w| w[1] <= w[0]) {
        // an ADC faster than the ticks: the last level at each tick
        let keep: Vec<usize> = (0..edges.len())
            .filter(|&i| i + 1 == edges.len() || edges[i + 1] > edges[i])
            .collect();
        let (e, l): (Vec<i64>, Vec<u8>) = keep.iter().map(|&i| (edges[i], new[i])).unzip();
        edges = (0..e.len())
            .filter(|&i| if i == 0 { l[0] != lv[0] } else { l[i] != l[i - 1] })
            .map(|i| e[i])
            .collect();
    }
    let mut acq = a.acquisition.clone();
    acq.insert("threshold_v".into(), json!([low, high]));
    Ok(Line {
        name: a.name.clone(),
        initial: lv.first().copied().unwrap_or(0),
        edges,
        step: wireskein_io::pyfmt::round(per).max(1),
        phase: (t0.floor() as i64).max(0),
        reduction: None,
        acquisition: acq,
    })
}

/// A threshold as given: "V" or "LOW,HIGH" (the CLI's NAME=V|NAME=LOW,HIGH after the name).
pub fn threshold_of(text: &str) -> Result<(f64, f64), String> {
    let vs: Result<Vec<f64>, _> = text.split(',').map(|x| x.trim().parse::<f64>()).collect();
    match vs.map_err(|e| format!("threshold {text:?}: {e}"))?.as_slice() {
        [v] => Ok((*v, *v)),
        [lo, hi] => thresholds(*lo, *hi),
        _ => Err(format!("threshold {text:?}: V or LOW,HIGH")),
    }
}

/// The thresholds as JSON ({name: V | [LOW, HIGH]}) in their order.
pub fn thresholds_of(v: &Value) -> Result<Vec<(String, f64, f64)>, String> {
    let mut out = vec![];
    for (k, x) in v.as_object().into_iter().flatten() {
        let (lo, hi) = match x {
            Value::Array(a) if a.len() == 2 => (
                a[0].as_f64().ok_or("a threshold in volts")?,
                a[1].as_f64().ok_or("a threshold in volts")?,
            ),
            x => {
                let v = x.as_f64().ok_or("a threshold in volts")?;
                (v, v)
            }
        };
        let (lo, hi) = thresholds(lo, hi)?;
        out.push((k.clone(), lo, hi));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trace(values: Vec<f64>) -> Analog {
        Analog {
            name: "A0".into(),
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
        }
    }

    #[test]
    fn hysteresis_makes_one_edge_of_a_noisy_slow_one() {
        let a = trace(vec![0.0, 0.0, 1.0, 1.6, 1.4, 1.7, 2.5, 3.3, 3.3, 1.0, 0.2, 0.0]);
        let l = to_logic(&a, (10_000_000, 1), 0.8, 2.0).unwrap();
        // 1.7 -> 2.5 crosses 2.0 at 3/8 of a sample (tick 53.75); 1.0 -> 0.2 crosses 0.8 at 1/4 (tick 92.5)
        assert_eq!((l.initial, l.edges.clone(), l.step, l.phase), (0, vec![54, 93], 10, 0));
        assert_eq!(schmitt(&[1.0, 1.0], 0.8, 2.0), vec![0, 0]);
    }
}
