//! The checks of serial lines (`verify.check_uart`, `check_rs485`, `check_can`, `check_lin`) and the dispatch to
//! the others.

use crate::cap::{Cap, Ch};
use crate::checks::{Out, V, limit_of, out, si, times, verdicts};
use crate::j::{J, py_eq, py_str};
use crate::jd;
use crate::pyfmt::round_n;
use crate::timing::{judge, percentile};

pub fn run(kind: &str, cap: &Cap, x: &J) -> Out {
    match kind {
        "uart" => check_uart(cap, x),
        "rs485" => check_rs485(cap, x),
        "can" => check_can(cap, x),
        "lin" => check_lin(cap, x),
        k => crate::coded::run(k, cap, x),
    }
}

fn s<'a>(x: &'a J, k: &str) -> &'a str {
    x.get(k).s().unwrap_or("")
}

/// `_seq_diff`: the first item that differs, both sides written out.
pub fn seq_diff(name: &str, want: &[J], got: &[J]) -> (Option<bool>, String) {
    if want.len() == got.len() && want.iter().zip(got).all(|(a, b)| py_eq(a, b)) {
        return (Some(true), String::new());
    }
    let i = want
        .iter()
        .zip(got)
        .position(|(a, b)| !py_eq(a, b))
        .unwrap_or(want.len().min(got.len()));
    let side = |v: &[J]| v.get(i).map(py_str).unwrap_or("-".into());
    (
        Some(false),
        format!(
            "{name}: {} expected, {} read; #{i} want {}, got {}",
            want.len(),
            got.len(),
            side(want),
            side(got)
        ),
    )
}

/// `x.get(k, d)` for an int.
fn geti(x: &J, k: &str, d: i64) -> i64 {
    if x.has(k) { x.get(k).i().unwrap_or(d) } else { d }
}

fn getf(x: &J, k: &str, d: f64) -> f64 {
    if x.has(k) && !x.get(k).is_null() {
        x.get(k).f().unwrap_or(d)
    } else {
        d
    }
}

// ---------------- UART ----------------

pub const LOOKAHEAD: usize = 16;

pub struct Rx {
    /// (start, value, stop_ok, parity_ok)
    pub frames: Vec<(i64, i64, bool, bool)>,
    pub lead_in: i64,
    pub resync_skipped: i64,
    pub cut_at_end: bool,
    pub glitches: Vec<(i64, i64)>,
    pub breaks: Vec<(i64, i64)>,
}

/// A receiver at a given bit time (`verify._uart_rx`).
pub fn uart_rx(ch: &Ch, n: i64, u: f64, idle: u8, bits: usize, parity: &str, stop: f64) -> Rx {
    let e = &ch.edges;
    let act: Vec<i64> = (0..e.len()).filter(|&i| ch.after(i) != idle).map(|i| e[i]).collect();
    let npar = if parity == "none" { 0 } else { 1 };
    let l = 1 + bits + npar + stop.ceil() as usize;
    let first_stop = 1 + bits + npar;
    let lf = l as f64;
    let gap = lf * u;
    let frame = |st: i64| -> (i64, bool, bool) {
        let b: Vec<u8> = (0..l)
            .map(|k| {
                let v = ch.level_at((st as f64 + (k as f64 + 0.5) * u) as i64);
                if idle == 0 { 1 - v } else { v }
            })
            .collect();
        let value = (0..bits).map(|k| (b[1 + k] as i64) << k).sum();
        let stop_ok = b[0] == 0 && b[first_stop] == 1 && (stop < 2.0 || b[first_stop + 1] == 1);
        let mut par_ok = true;
        if npar == 1 {
            let ones = b[1..1 + bits].iter().map(|&v| v as usize).sum::<usize>() + b[1 + bits] as usize;
            par_ok = ones.is_multiple_of(2) == (parity == "even");
        }
        (value, stop_ok, par_ok)
    };
    let next_start = |st: i64| -> usize {
        let t = st as f64 + (first_stop as f64 + 0.5) * u;
        act.partition_point(|&a| (a as f64) < t)
    };
    let in_step = |mut i: usize| -> bool {
        let mut good = 0;
        while i < act.len() && good < LOOKAHEAD {
            let st = act[i];
            if st as f64 + lf * u > n as f64 {
                return good >= 3;
            }
            let (_, so, po) = frame(st);
            if !(so && po) {
                return false;
            }
            good += 1;
            i = next_start(st);
        }
        good >= LOOKAHEAD || good >= 3
    };
    let short_in_step = |mut i: usize| -> bool {
        let mut good = 0;
        while i < act.len() && good < 3 {
            let st = act[i];
            if st as f64 + lf * u > n as f64 {
                return good >= 1;
            }
            let (_, so, po) = frame(st);
            if !(so && po) {
                return false;
            }
            good += 1;
            i = next_start(st);
        }
        good >= 3 || (good >= 1 && i >= act.len())
    };
    let quiet: Vec<i64> = act
        .iter()
        .map(|&a| {
            let j = e.partition_point(|&x| x < a);
            if j > 0 {
                a - e[j - 1]
            } else if ch.initial == idle {
                a
            } else {
                0
            }
        })
        .collect();
    let gap_at: Vec<bool> = quiet.iter().map(|&q| q as f64 >= gap).collect();
    let mut gap_later = gap_at.clone();
    for k in (0..gap_later.len().saturating_sub(1)).rev() {
        gap_later[k] |= gap_later[k + 1];
    }
    let active_until = |st: i64| -> i64 {
        let k = e.partition_point(|&x| x <= st);
        if k < e.len() { e[k] } else { n }
    };
    let mut rx = Rx {
        frames: vec![],
        lead_in: 0,
        resync_skipped: 0,
        cut_at_end: false,
        glitches: vec![],
        breaks: vec![],
    };
    let (mut synced, mut lost, mut i, mut err_at) = (false, false, 0usize, None::<i64>);
    while i < act.len() {
        let st = act[i];
        let mid = (st as f64 + 0.5 * u) as i64;
        if mid < n && ch.level_at(mid) == idle {
            rx.glitches.push((st, active_until(st) - st));
            i += 1;
            continue;
        }
        if !synced {
            let boundary = err_at.is_some_and(|ea| {
                let d = (st - ea) as f64;
                (lf - 0.5) * u <= d && d <= (lf + 2.5) * u
            });
            synced = gap_at[i]
                || (lost && quiet[i] as f64 >= 0.75 * u && (boundary || short_in_step(i)))
                || (!gap_later[i] && in_step(i));
        }
        if st as f64 + lf * u > n as f64 {
            rx.cut_at_end = true;
            break;
        }
        let (value, stop_ok, par_ok) = frame(st);
        let end = active_until(st);
        if synced && value == 0 && !stop_ok && (end - st) as f64 > (lf + 0.5) * u {
            rx.breaks.push((st, end - st));
            i = act.partition_point(|&a| a < end);
            continue;
        }
        if synced {
            rx.frames.push((st, value, stop_ok, par_ok));
            if !stop_ok {
                synced = false;
                lost = true;
                err_at = Some(st);
            }
        } else if lost {
            rx.resync_skipped += 1;
        } else {
            rx.lead_in += 1;
        }
        i = next_start(st);
    }
    rx
}

/// The level the line rests at, judged by reading (`verify._uart_idle`).
#[allow(clippy::too_many_arguments)]
fn uart_idle(ch: &Ch, n: i64, u: f64, idle: u8, bits: usize, parity: &str, stop: f64, rx: &Rx) -> Option<u8> {
    let (_, length, level) = ch.runs(n);
    let l = 1.0 + bits as f64 + (parity != "none") as i64 as f64 + stop;
    let (mut t1, mut t0) = (0i64, 0i64);
    for (k, &ln) in length.iter().enumerate() {
        if ln as f64 >= 2.0 * l * u {
            if level[k] == 1 { t1 += ln } else { t0 += ln }
        }
    }
    if t0.max(t1) > 0 && t0.min(t1) * 4 <= t0.max(t1) {
        return Some(if t1 > t0 { 1 } else { 0 });
    }
    let good = rx.frames.iter().filter(|f| f.2 && f.3).count();
    let other = uart_rx(ch, n, u, 1 - idle, bits, parity, stop);
    let good_other = other.frames.iter().filter(|f| f.2 && f.3).count();
    if good_other > 2 * good + 2 {
        return Some(1 - idle);
    }
    if good > 0 {
        return Some(idle);
    }
    if length.is_empty() {
        None
    } else {
        Some(level[crate::checks::argmax(length.iter().map(|&v| v as f64))])
    }
}

/// The bit time from edges of the same direction inside good characters (`verify._uart_bit_time`).
fn uart_bit_time(ch: &Ch, frames: &[(i64, i64, bool, bool)], u0: f64, span_bits: usize) -> Option<f64> {
    let e = &ch.edges;
    let good: Vec<&(i64, i64, bool, bool)> = frames.iter().filter(|f| f.2 && f.3).collect();
    let good: Vec<&(i64, i64, bool, bool)> = if good.is_empty() { frames.iter().collect() } else { good };
    let mut d: Vec<f64> = vec![];
    for f in good {
        let st = f.0;
        let a = e.partition_point(|&x| x < st);
        let lim = st as f64 + (span_bits as f64 + 0.5) * u0;
        let b = e.partition_point(|&x| (x as f64) < lim);
        for lv in [0u8, 1] {
            let xs: Vec<i64> = (a..b).filter(|&i| ch.after(i) == lv).map(|i| e[i]).collect();
            if xs.len() > 1 {
                d.extend(xs[1..].iter().map(|&v| (v - xs[0]) as f64));
            }
        }
    }
    if d.is_empty() {
        return None;
    }
    let mut u = u0;
    for kmax in [Some(4.0), None] {
        let (mut num, mut den, mut any) = (0.0, 0.0, false);
        for &v in &d {
            let k = (v / u).round_ties_even();
            if k >= 1.0 && kmax.is_none_or(|m| k <= m) {
                num += v * k;
                den += k * k;
                any = true;
            }
        }
        if any {
            u = num / den;
        }
    }
    Some(u)
}

fn uart_edge_offsets(ch: &Ch, frames: &[(i64, i64, bool, bool)], u: f64, span_bits: usize) -> Vec<f64> {
    let e = &ch.edges;
    frames
        .iter()
        .map(|f| {
            let st = f.0;
            let lim = st as f64 + (span_bits as f64 + 0.5) * u;
            let a = e.partition_point(|&x| x <= st);
            let b = e.partition_point(|&x| (x as f64) < lim);
            e[a..b.max(a)]
                .iter()
                .map(|&x| {
                    let d = (x - st) as f64 / u;
                    (d - d.round_ties_even()).abs()
                })
                .fold(0.0, f64::max)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn uart_measure(
    ch: &Ch,
    n: i64,
    runs: &[i64],
    u_hint: Option<f64>,
    idle: u8,
    bits: usize,
    parity: &str,
    stop: f64,
) -> Option<f64> {
    let inner = if runs.len() >= 2 {
        &runs[1..runs.len() - 1]
    } else {
        &[][..]
    };
    let cands = wireskein_core::features::estimate_units(inner, 12.0, 6);
    if cands.is_empty() {
        return None;
    }
    let mut u = match u_hint {
        Some(h) => {
            cands
                .iter()
                .min_by(|a, b| {
                    (a.samples / h)
                        .ln()
                        .abs()
                        .partial_cmp(&(b.samples / h).ln().abs())
                        .unwrap()
                })
                .unwrap()
                .samples
        }
        None => cands[0].samples,
    };
    for _ in 0..3 {
        let rx = uart_rx(ch, n, u, idle, bits, parity, stop);
        u = uart_bit_time(ch, &rx.frames, u, 1 + bits + (parity != "none") as usize).unwrap_or(u);
    }
    Some(u)
}

fn check_uart(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let idle = geti(x, "idle", 1) as u8;
    let bits = geti(x, "bits", 8) as usize;
    let parity = x.get("parity").s().unwrap_or("none").to_string();
    let parity = if x.has("parity") { parity } else { "none".into() };
    let stop = getf(x, "stop", 1.0);
    let baud = x.get("baud").f().filter(|&b| b != 0.0);
    let n = cap.n;
    let (_, length, _) = ch.runs(n);
    let u = uart_measure(ch, n, &length, baud.map(|b| cap.rate / b), idle, bits, &parity, stop);
    if baud.is_none() && u.is_none() {
        return out(
            V::Unchecked,
            jd! {"edges" => ch.edges.len()},
            "measure only: too few edges to find a bit time",
        );
    }
    let u0 = baud.map(|b| cap.rate / b).unwrap_or_else(|| u.unwrap());
    let rx = uart_rx(ch, n, u0, idle, bits, &parity, stop);
    let rest = uart_idle(ch, n, u0, idle, bits, &parity, stop, &rx);
    let frames = &rx.frames;
    if frames.is_empty() {
        let got = jd! {"idle" => rest, "lead_in" => rx.lead_in, "cut_at_end" => rx.cut_at_end,
        "samples_per_bit" => u0 / ch.step as f64};
        if ch.edges.is_empty() {
            return out(V::Ng, got, format!("no activity (constant {})", ch.initial));
        }
        if rest != Some(idle) {
            return out(V::Ng, got, format!("idle level {}, not {idle}", J::from(rest).repr()));
        }
        let why = "no UART character (neither an idle gap of a whole character nor a run of valid characters)";
        return if baud.is_none() {
            out(V::Unchecked, got, format!("measure only: {why}"))
        } else {
            out(V::Ng, got, why)
        };
    }
    let frame_err = frames.iter().filter(|f| !f.2).count();
    let par_err = frames.iter().filter(|f| f.2 && !f.3).count();
    let bad_seq: Vec<bool> = frames.iter().map(|f| !(f.2 && f.3)).collect();
    let error_runs = (0..bad_seq.len())
        .filter(|&k| bad_seq[k] && (k == 0 || !bad_seq[k - 1]))
        .count();
    let data: J = if bits <= 8 {
        J::from(
            frames
                .iter()
                .filter(|f| f.2 && f.3)
                .map(|f| format!("{:02x}", f.1 & 0xFF))
                .collect::<String>(),
        )
    } else {
        J::Null
    };
    let reff = baud.map(|b| cap.rate / b);
    let baud_m = u.map(|u| cap.rate / u);
    let baud_error = match (u, reff) {
        (Some(u), Some(r)) => Some(r / u - 1.0),
        _ => None,
    };
    let mut got = jd! {"baud" => baud_m, "baud_error" => baud_error,
    "samples_per_bit" => u.unwrap_or(u0) / ch.step as f64, "chars" => frames.len(),
    "frame_errors" => frame_err, "parity_errors" => par_err, "error_runs" => error_runs,
    "lead_in" => rx.lead_in, "resync_skipped" => rx.resync_skipped, "cut_at_end" => rx.cut_at_end,
    "data" => data.clone(), "glitches" => rx.glitches.len(), "breaks" => rx.breaks.len()};
    let mut notices: Vec<J> = rx
        .glitches
        .iter()
        .map(|&(t, w)| jd! {"kind" => "glitch", "tick" => t, "width_ticks" => w})
        .collect();
    notices.extend(
        rx.breaks
            .iter()
            .map(|&(t, l)| jd! {"kind" => "break", "tick" => t, "length_ticks" => l}),
    );
    got.set("notices", J::List(notices));
    got.set("idle", rest);
    let span = 1 + bits + (parity != "none") as usize + 1;
    let off = uart_edge_offsets(ch, frames, u.unwrap_or(u0), span);
    let off_max = off.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let p99 = percentile(&off, 99.0);
    got.set("edge_offset_max", off_max);
    got.set("edge_offset_p99", p99);
    let bad: Vec<(i64, f64)> = frames
        .iter()
        .zip(&off)
        .filter(|(f, _)| !(f.2 && f.3))
        .map(|(f, &o)| (f.0, o))
        .collect();
    got.set(
        "errors_at",
        J::List(
            bad.iter()
                .take(8)
                .map(|&(t, o)| jd! {"sample" => t, "edge_offset" => round_n(o, 3)})
                .collect(),
        ),
    );
    let (mut ok, mut why) = (true, vec![]);
    let baud_rel = getf(x, "baud_rel", 0.03);
    if baud.is_some() {
        if u.is_none() {
            ok = false;
            why.push("no edges to measure the bit time".to_string());
        } else {
            let be = baud_error.unwrap();
            if be.abs() > baud_rel / 2.0 && be.abs() <= baud_rel {
                crate::report::push_notice(
                    &mut got,
                    jd! {"kind" => "baud_near_limit", "tick" => ch.edges.first().copied().unwrap_or(0),
                    "value" => round_n(be, 5), "limit" => baud_rel},
                );
            }
        }
    }
    if let (Some(b), Some(be)) = (baud, baud_error)
        && be.abs() > baud_rel
    {
        ok = false;
        why.push(format!("baud {:.0} is {:+.2}% off {b:.0}", baud_m.unwrap(), be * 100.0));
    }
    let mut want = x.get("errors").clone();
    if want.is_null() && baud.is_some() {
        want = J::dict();
    }
    let mut unknown: Vec<&str> = want
        .items()
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| *k != "framing" && *k != "parity")
        .collect();
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty() {
        ok = false;
        why.push(format!("errors: unknown kind {} (framing, parity)", unknown.join(", ")));
    } else if !want.is_null() {
        let (wf, wp) = (geti(&want, "framing", 0), geti(&want, "parity", 0));
        if (frame_err as i64, par_err as i64) != (wf, wp) {
            ok = false;
            let mut msg = format!("{frame_err} framing and {par_err} parity errors, not {wf} and {wp}");
            let skewed: Vec<f64> = bad
                .iter()
                .map(|&(_, o)| o)
                .filter(|&o| o > 0.2f64.max(2.0 * p99))
                .collect();
            if !bad.is_empty() && skewed.len() == bad.len() {
                msg += &format!(
                    " (each in a character whose edges are up to {:.2} bit off the bit grid, elsewhere {p99:.2} at p99: suspect the capture time base)",
                    skewed.iter().cloned().fold(f64::NEG_INFINITY, f64::max)
                );
            }
            why.push(msg);
        }
    }
    if let Some(d) = x.get("data").s()
        && data.s() != Some(d.to_lowercase().as_str())
    {
        ok = false;
        why.push("data differs".into());
    }
    if rest != Some(idle) {
        ok = false;
        why.push(format!("idle level {}", J::from(rest).repr()));
    }
    if baud.is_none() && ok && x.get("data").is_null() && x.get("errors").is_null() {
        let head = match baud_m {
            Some(b) if b != 0.0 => format!("measured {b:.0} baud, "),
            _ => String::new(),
        };
        return out(
            V::Measured,
            got,
            format!(
                "{head}{} chars, {frame_err} framing / {par_err} parity errors",
                frames.len()
            ),
        );
    }
    out(V::of(Some(ok)), got, why.join("; "))
}

fn check_rs485(cap: &Cap, x: &J) -> Out {
    let (Some(tx), Some(de)) = (cap.ch(s(x, "tx")), cap.ch(s(x, "de"))) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let mut ux = x.clone();
    ux.set("kind", "uart");
    ux.set("pin", s(x, "tx"));
    let (ok, mut got, why) = check_uart(cap, &ux);
    let baud = x.get("baud").f().filter(|&b| b != 0.0);
    let gb = got.get("baud").f().filter(|&b| b != 0.0);
    if gb.is_none() && baud.is_none() {
        return (ok, got, why);
    }
    let idle = geti(x, "idle", 1) as u8;
    let bits = geti(x, "bits", 8) as usize;
    let parity = if x.has("parity") {
        s(x, "parity").to_string()
    } else {
        "none".into()
    };
    let stop = getf(x, "stop", 1.0);
    let u = cap.rate / gb.or(baud).unwrap();
    let rx = uart_rx(tx, cap.n, u, idle, bits, &parity, stop);
    let span = (1.0 + bits as f64 + (parity != "none") as i64 as f64 + stop) * u;
    let active = geti(x, "de_active", 1) as u8;
    let (start, length, level) = de.runs(cap.n);
    let win: Vec<(i64, i64)> = (0..start.len())
        .filter(|&k| level[k] == active)
        .map(|k| (start[k], start[k] + length[k]))
        .collect();
    let chars: Vec<(f64, f64)> = rx.frames.iter().map(|f| (f.0 as f64, f.0 as f64 + span)).collect();
    let mut inside: Vec<Vec<(f64, f64)>> = vec![vec![]; win.len()];
    let mut outside = vec![];
    for &(c0, c1) in &chars {
        let k = win.partition_point(|w| (w.0 as f64) <= c0) as isize - 1;
        if k >= 0 && (win[k as usize].0 as f64) <= c0 && c0 < win[k as usize].1 as f64 {
            inside[k as usize].push((c0, c1));
        } else {
            outside.push(c0);
        }
    }
    let n = cap.n;
    type Row = (i64, usize, Option<f64>, Option<(f64, f64)>);
    let mut rows: Vec<Row> = vec![];
    for (k, &(a, b)) in win.iter().enumerate() {
        let cs = &inside[k];
        if cs.is_empty() {
            if a > 0 && b < n {
                crate::report::push_notice(
                    &mut got,
                    jd! {"kind" => "empty_de_window", "tick" => a, "length_ticks" => b - a},
                );
            }
            continue;
        }
        rows.push((
            a,
            cs.len(),
            if a > 0 { Some(cs[0].0 - a as f64) } else { None },
            if b < n {
                Some((cs[cs.len() - 1].1.trunc(), b as f64 - cs[cs.len() - 1].1))
            } else {
                None
            },
        ));
    }
    let leads: Vec<(f64, f64)> = rows.iter().filter_map(|r| r.2.map(|l| (r.0 as f64, l))).collect();
    let tails: Vec<(f64, f64)> = rows.iter().filter_map(|r| r.3).collect();
    got.set(
        "de_windows",
        J::List(
            rows.iter()
                .take(16)
                .map(|r| {
                    jd! {"tick" => r.0, "chars" => r.1, "lead_s" => r.2.map(|l| l / cap.rate),
                    "tail_s" => r.3.map(|t| t.1 / cap.rate)}
                })
                .collect(),
        ),
    );
    got.set("de_lead", times(&leads, cap.rate));
    got.set("de_tail", times(&tails, cap.rate));
    got.set("chars_outside_de", outside.len());
    let mut parts: Vec<(Option<bool>, String)> = vec![];
    if ok != V::Measured {
        parts.push((
            match ok {
                V::Ok => Some(true),
                V::Ng => Some(false),
                _ => None,
            },
            why.clone(),
        ));
    }
    if !outside.is_empty() {
        parts.push((
            Some(false),
            format!(
                "{} characters with DE inactive (the first at {})",
                outside.len(),
                si(outside[0] / cap.rate)
            ),
        ));
    }
    let res = tx.step.max(de.step);
    let lim = |k: &str, d: Option<f64>| if x.has(k) { x.get(k).f() } else { d };
    if !leads.is_empty() {
        parts.extend(limit_of(
            "DE lead",
            &leads,
            res,
            lim("lead_min_s", Some(0.0)),
            lim("lead_max_s", None),
            cap.rate,
        ));
    }
    if !tails.is_empty() {
        parts.extend(limit_of(
            "DE tail",
            &tails,
            res,
            lim("tail_min_s", Some(0.0)),
            lim("tail_max_s", None),
            cap.rate,
        ));
    }
    if ok == V::Measured && parts.iter().all(|p| p.0 != Some(false)) {
        return (V::Measured, got, why);
    }
    let (v, w) = verdicts(&parts);
    (v, got, w)
}

// ---------------- CAN ----------------

const CRC_POLY: u32 = 0x4599;

fn crc15(bits: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &b in bits {
        let nxt = b as u32 ^ ((crc >> 14) & 1);
        crc = (crc << 1) & 0x7FFF;
        if nxt == 1 {
            crc ^= CRC_POLY;
        }
    }
    crc
}

struct Line<'a> {
    e: &'a [i64],
    initial: u8,
    idle: u8,
    falls: Vec<i64>,
    rises: Vec<i64>,
}

impl<'a> Line<'a> {
    fn new(ch: &'a Ch, idle: u8) -> Line<'a> {
        let e = &ch.edges[..];
        let falls = (0..e.len()).filter(|&i| ch.after(i) != idle).map(|i| e[i]).collect();
        let rises = (0..e.len()).filter(|&i| ch.after(i) == idle).map(|i| e[i]).collect();
        Line {
            e,
            initial: ch.initial,
            idle,
            falls,
            rises,
        }
    }

    fn start0(&self) -> f64 {
        if self.initial == self.idle {
            f64::NEG_INFINITY
        } else {
            0.0
        }
    }

    fn idle_falls(&self, min_ticks: f64) -> Vec<i64> {
        self.falls
            .iter()
            .copied()
            .filter(|&f| {
                let k = self.rises.partition_point(|&r| r <= f);
                let since = if k > 0 { self.rises[k - 1] as f64 } else { self.start0() };
                f as f64 - since >= min_ticks
            })
            .collect()
    }

    fn level(&self, t: f64) -> u8 {
        let k = self.e.partition_point(|&x| (x as f64) <= t);
        self.initial ^ (k as u8 & 1)
    }

    fn next_fall(&self, t: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) < t);
        self.falls.get(k).copied()
    }

    fn fall_between(&self, a: f64, b: f64) -> Option<i64> {
        let k = self.falls.partition_point(|&x| (x as f64) <= a);
        self.falls.get(k).copied().filter(|&f| f as f64 <= b)
    }

    fn recessive_since(&self, t: f64) -> f64 {
        let k = self.rises.partition_point(|&x| (x as f64) <= t);
        if k > 0 { self.rises[k - 1] as f64 } else { self.start0() }
    }
}

enum Stop {
    Err(&'static str, f64, &'static str),
    Cut,
}

struct Reader<'a, 'b> {
    line: &'b Line<'a>,
    n: i64,
    edge: f64,
    prev: f64,
    k: usize,
    u: f64,
    sp: f64,
    run_v: Option<u8>,
    run_c: u32,
    body: Vec<u8>,
}

impl Reader<'_, '_> {
    fn raw(&mut self) -> Result<(u8, f64), Stop> {
        let nominal = self.edge + self.sp * self.u;
        if let Some(f) = self.line.fall_between(self.prev, nominal)
            && self.k > 0
        {
            self.edge = f as f64;
        }
        let t = self.edge + self.sp * self.u;
        if t >= self.n as f64 {
            return Err(Stop::Cut);
        }
        self.prev = t;
        self.edge += self.u;
        self.k += 1;
        Ok((if self.line.level(t) != self.line.idle { 0 } else { 1 }, t))
    }

    fn stuffed(&mut self, field: &'static str) -> Result<u8, Stop> {
        let (mut b, mut t) = self.raw()?;
        if self.run_c == 5 {
            if Some(b) == self.run_v {
                return Err(Stop::Err("stuff", t, field));
            }
            self.run_v = Some(b);
            self.run_c = 1;
            (b, t) = self.raw()?;
        }
        let _ = t;
        self.run_c = if Some(b) == self.run_v { self.run_c + 1 } else { 1 };
        self.run_v = Some(b);
        self.body.push(b);
        Ok(b)
    }

    fn bits(&mut self, k: usize, field: &'static str) -> Result<u64, Stop> {
        let mut v = 0u64;
        for _ in 0..k {
            v = (v << 1) | self.stuffed(field)? as u64;
        }
        Ok(v)
    }

    fn fixed(&mut self, field: &'static str) -> Result<(), Stop> {
        let (b, t) = self.raw()?;
        if b != 1 {
            return Err(Stop::Err("form", t, field));
        }
        Ok(())
    }
}

/// One classic frame from its start of frame (`canbus._read_frame`): (the frame, its overload tick, the tick its
/// end of frame ends).
fn read_frame(line: &Line, sof: i64, u: f64, sp: f64, n: i64) -> Result<(J, Option<i64>, f64), Stop> {
    let mut r = Reader {
        line,
        n,
        edge: sof as f64,
        prev: sof as f64,
        k: 0,
        u,
        sp,
        run_v: None,
        run_c: 0,
        body: vec![],
    };
    if r.bits(1, "sof")? != 0 {
        return Err(Stop::Err("form", sof as f64 + sp * u, "sof"));
    }
    let id_a = r.bits(11, "id")?;
    let rtr_srr = r.bits(1, "rtr")?;
    let ide = r.bits(1, "ide")?;
    let (ident, remote) = if ide == 1 {
        let id_b = r.bits(18, "id")?;
        let rtr = r.bits(1, "rtr")?;
        r.bits(1, "r1")?;
        ((id_a << 18) | id_b, rtr)
    } else {
        (id_a, rtr_srr)
    };
    r.bits(1, "r0")?;
    let dlc = r.bits(4, "dlc")?;
    let nb = if remote == 1 { 0 } else { dlc.min(8) };
    let mut data = vec![];
    for _ in 0..nb {
        data.push(r.bits(8, "data")? as u8);
    }
    let covered = r.body.len();
    let crc = r.bits(15, "crc")?;
    if r.run_c == 5 {
        let (b, t) = r.raw()?;
        if Some(b) == r.run_v {
            return Err(Stop::Err("stuff", t, "crc"));
        }
    }
    if crc as u32 != crc15(&r.body[..covered]) {
        let (_, t) = r.raw()?;
        return Err(Stop::Err("crc", t, "crc"));
    }
    r.fixed("crc_delimiter")?;
    let (ack_bit, _) = r.raw()?;
    r.fixed("ack_delimiter")?;
    for _ in 0..6 {
        r.fixed("eof")?;
    }
    let (last, t) = r.raw()?;
    let frame = jd! {"tick" => sof, "id" => ident as i64, "ext" => ide == 1, "rtr" => remote == 1,
    "dlc" => dlc as i64, "data" => hex(&data), "ack" => ack_bit == 0};
    Ok((frame, if last == 0 { Some(t as i64) } else { None }, r.edge))
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02x}")).collect()
}

struct CanRx {
    frames: Vec<J>,
    errors: Vec<J>,
    lead_in: i64,
    cut: bool,
    flags: i64,
    overloads: Vec<J>,
}

/// The frames and errors a node at bit time u reads (`canbus.read`, classic CAN).
fn can_read(ch: &Ch, n: i64, u: f64, idle: u8, sp: f64) -> CanRx {
    let line = Line::new(ch, idle);
    let (mut frames, mut errors, mut overloads) = (vec![], vec![], vec![]);
    let (mut need, mut after, mut pos) = (11.0, f64::NEG_INFINITY, 0.0f64);
    let (mut lead_in, mut cut) = (None, false);
    let idle_falls = line.idle_falls((11.0 - 0.5) * u);
    loop {
        if need == 11.0 && after <= 0.0 {
            let k = idle_falls.partition_point(|&f| (f as f64) < pos);
            if k == idle_falls.len() {
                break;
            }
            pos = pos.max(idle_falls[k] as f64);
        }
        let Some(f) = line.next_fall(pos) else { break };
        let since = line.recessive_since(f as f64).max(after);
        let ff = f as f64;
        if ff - since < (need - 0.5) * u {
            if need == 3.0 && ff - since >= 0.5 * u {
                overloads.push(jd! {"tick" => f});
            }
            need = 11.0;
            pos = ff + 1.0;
            continue;
        }
        if lead_in.is_none() {
            lead_in = Some(f);
        }
        match read_frame(&line, f, u, sp, n) {
            Err(Stop::Cut) => {
                cut = true;
                break;
            }
            Err(Stop::Err(kind, tick, field)) => {
                errors.push(jd! {"kind" => kind, "tick" => tick as i64, "field" => field});
                need = 11.0;
                after = 0.0;
                pos = tick;
                continue;
            }
            Ok((fr, ov, end)) => {
                if let Some(o) = ov {
                    overloads.push(jd! {"tick" => o});
                    need = 11.0;
                    after = 0.0;
                    pos = end;
                } else {
                    need = 3.0;
                    after = end;
                    pos = end;
                }
                frames.push(fr);
            }
        }
    }
    let (_, length, level) = ch.runs(n);
    let flags = (0..length.len())
        .filter(|&k| level[k] != idle && length[k] as f64 >= 5.5 * u)
        .count() as i64;
    CanRx {
        frames,
        errors,
        lead_in: lead_in.unwrap_or(0),
        cut,
        flags,
        overloads,
    }
}

/// The bit time from the intervals between recessive-to-dominant edges (`canbus.bit_time`).
fn can_bit_time(ch: &Ch, u0: f64, idle: u8) -> Option<f64> {
    let line = Line::new(ch, idle);
    let d: Vec<f64> = line.falls.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if d.is_empty() {
        return None;
    }
    let mut u = u0;
    for tol in [0.3, 0.3, 0.3, 0.15] {
        let (mut sd, mut sk, mut any) = (0.0, 0.0, false);
        for &v in &d {
            let k = (v / u).round_ties_even();
            if (1.0..=10.0).contains(&k) && (v / u - k).abs() < tol {
                sd += v;
                sk += k;
                any = true;
            }
        }
        if !any {
            return None;
        }
        u = sd / sk;
    }
    Some(u)
}

const CAN_ERRORS: [&str; 3] = ["stuff", "form", "crc"];

fn check_can(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let idle = geti(x, "idle", 1) as u8;
    let want_rate = x.get("bitrate").f().filter(|&b| b != 0.0);
    let u0 = match want_rate {
        Some(r) => cap.rate / r,
        None => {
            let (_, length, _) = ch.runs(cap.n);
            let inner = if length.len() >= 2 {
                &length[1..length.len() - 1]
            } else {
                &[][..]
            };
            let c = wireskein_core::features::estimate_units(inner, 12.0, 6);
            if c.is_empty() {
                return out(
                    V::Unchecked,
                    jd! {"edges" => ch.edges.len()},
                    "measure only: too few edges to find a bit time",
                );
            }
            c[0].samples
        }
    };
    let u = can_bit_time(ch, u0, idle);
    let rx = can_read(ch, cap.n, u.unwrap_or(u0), idle, getf(x, "sample_point", 0.75));
    let count = |k: &str| rx.errors.iter().filter(|e| e.get("kind").s() == Some(k)).count() as i64;
    let unacked = rx.frames.iter().filter(|f| !f.get("ack").truthy()).count();
    let bitrate = u.map(|u| cap.rate / u);
    let bitrate_error = match (u, want_rate) {
        (Some(u), Some(_)) => Some(u0 / u - 1.0),
        _ => None,
    };
    let frames: Vec<J> = rx
        .frames
        .iter()
        .map(|f| {
            let mut d = J::dict();
            for k in ["id", "ext", "rtr", "dlc", "data", "ack"] {
                d.set(k, f.get(k).clone());
            }
            d
        })
        .collect();
    let mut got = jd! {"bitrate" => bitrate, "bitrate_error" => bitrate_error,
    "samples_per_bit" => u.unwrap_or(u0) / ch.step as f64, "frames" => frames.clone(),
    "frame_count" => rx.frames.len(), "unacked" => unacked, "stuff_errors" => count("stuff"),
    "form_errors" => count("form"), "crc_errors" => count("crc"),
    "errors_at" => rx.errors.iter().take(8).cloned().collect::<Vec<_>>(), "error_flags" => rx.flags,
    "overloads" => rx.overloads.len(), "lead_in" => rx.lead_in, "cut_at_end" => rx.cut};
    got.set(
        "notices",
        J::List(
            rx.overloads
                .iter()
                .map(|o| jd! {"kind" => "overload", "tick" => o.get("tick")})
                .collect(),
        ),
    );
    if ch.edges.is_empty() {
        return out(V::Ng, got, format!("no activity (constant {})", ch.initial));
    }
    let mut parts = vec![];
    let rel = getf(x, "bitrate_rel", 0.01);
    if let Some(wr) = want_rate {
        match bitrate_error {
            None => parts.push((Some(false), "no edges to measure the bit time".to_string())),
            Some(be) if be.abs() > rel => parts.push((
                Some(false),
                format!("bitrate {:.0} is {:+.2}% off {wr:.0}", bitrate.unwrap(), be * 100.0),
            )),
            Some(be) if be.abs() > rel / 2.0 => crate::report::push_notice(
                &mut got,
                jd! {"kind" => "bitrate_near_limit", "tick" => ch.edges[0], "value" => round_n(be, 5),
                "limit" => rel},
            ),
            _ => {}
        }
    }
    let mut want = x.get("errors").clone();
    if want.is_null() && want_rate.is_some() {
        want = J::dict();
    }
    let mut unknown: Vec<&str> = want
        .items()
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !CAN_ERRORS.contains(k))
        .collect();
    unknown.sort();
    if !unknown.is_empty() {
        parts.push((
            Some(false),
            format!(
                "errors: unknown kind {} ({})",
                unknown.join(", "),
                CAN_ERRORS.join(", ")
            ),
        ));
    } else if !want.is_null() && CAN_ERRORS.iter().any(|k| count(k) != geti(&want, k, 0)) {
        parts.push((
            Some(false),
            format!(
                "{} errors, not {}",
                CAN_ERRORS
                    .iter()
                    .map(|k| format!("{} {k}", count(k)))
                    .collect::<Vec<_>>()
                    .join(", "),
                CAN_ERRORS
                    .iter()
                    .map(|k| geti(&want, k, 0).to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    if !x.get("frames").is_null() {
        let norm = |f: &J| {
            let id = f.get("id").i().unwrap_or(0);
            jd! {"id" => id, "ext" => if f.has("ext") { f.get("ext").truthy() } else { id > 0x7FF },
            "rtr" => f.get("rtr").truthy(),
            "data" => if f.has("data") { py_str(f.get("data")).to_lowercase() } else { String::new() }}
        };
        let want: Vec<J> = x.get("frames").list().iter().map(norm).collect();
        let have: Vec<J> = frames
            .iter()
            .map(|f| jd! {"id" => f.get("id"), "ext" => f.get("ext"), "rtr" => f.get("rtr"), "data" => f.get("data")})
            .collect();
        parts.push(seq_diff("frames", &want, &have));
    }
    let ack = if x.has("ack") {
        x.get("ack").clone()
    } else {
        J::Bool(true)
    };
    let nf = rx.frames.len();
    if ack == J::Bool(true) && unacked > 0 {
        parts.push((
            Some(false),
            format!("{unacked} of {nf} frames not acknowledged (a recessive ACK slot)"),
        ));
    } else if ack == J::Bool(false) && nf - unacked > 0 {
        parts.push((Some(false), format!("{} of {nf} frames acknowledged", nf - unacked)));
    }
    if rx.frames.is_empty() && rx.errors.is_empty() {
        parts.push((
            if want_rate.is_some() { Some(false) } else { None },
            "no CAN frame (a start of frame after 11 recessive bits)".into(),
        ));
    }
    let (v, why) = verdicts(&parts);
    if want_rate.is_none() && v != V::Ng && x.get("frames").is_null() && x.get("errors").is_null() {
        let head = match bitrate {
            Some(b) if b != 0.0 => format!("measured {b:.0} bit/s, "),
            _ => String::new(),
        };
        return out(
            V::Measured,
            got,
            format!(
                "{head}{nf} frames, {} errors",
                CAN_ERRORS
                    .iter()
                    .map(|k| format!("{} {k}", count(k)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    out(v, got, why)
}

// ---------------- LIN ----------------

fn lin_pid(ident: i64) -> i64 {
    let b: Vec<i64> = (0..6).map(|i| (ident >> i) & 1).collect();
    let p0 = b[0] ^ b[1] ^ b[2] ^ b[4];
    let p1 = 1 - (b[1] ^ b[3] ^ b[4] ^ b[5]);
    ident | (p0 << 6) | (p1 << 7)
}

fn lin_checksum(data: &[i64], pid: Option<i64>) -> i64 {
    let mut s = pid.unwrap_or(0);
    for &x in data {
        s += x;
        s = (s & 0xFF) + (s >> 8);
    }
    !s & 0xFF
}

const LIN_CLASSIC_IDS: [i64; 2] = [0x3C, 0x3D];

fn check_lin(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let (rate, n) = (cap.rate, cap.n);
    let baud = x.get("baud").f().unwrap();
    let bmin = x.get("break_min_bits").f().unwrap_or(13.0);
    let u0 = rate / baud;
    let (st, ln, lv) = ch.runs(n);
    let brk: Vec<usize> = (0..st.len())
        .filter(|&k| lv[k] == 0 && ln[k] as f64 >= bmin.min(11.0) * u0)
        .collect();
    let falls: Vec<i64> = (0..ch.edges.len())
        .filter(|&i| ch.after(i) == 0)
        .map(|i| ch.edges[i])
        .collect();
    let mut fr: Vec<J> = vec![];
    for (k, &i) in brk.iter().enumerate() {
        if i == 0 || i + 1 >= st.len() {
            continue;
        }
        let b_end = st[i] + ln[i];
        let nxt = if k + 1 < brk.len() { st[brk[k + 1]] } else { n };
        let mut f = jd! {"t" => st[i], "break" => ln[i], "delimiter" => ln[i + 1]};
        let fl: Vec<i64> = falls.iter().copied().filter(|&v| v > b_end && v < nxt).collect();
        let u = if fl.len() >= 5 && ((fl[4] - fl[0]) as f64) < 12.0 * u0 {
            (fl[4] - fl[0]) as f64 / 8.0
        } else {
            u0
        };
        f.set("bit_ticks", u);
        let mut chars: Vec<(i64, i64, bool)> = vec![];
        let mut t = b_end as f64;
        loop {
            let j = fl.partition_point(|&v| (v as f64) < t);
            if j >= fl.len() || (!chars.is_empty() && fl[j] as f64 - t > 20.0 * u) {
                break;
            }
            let s0 = fl[j];
            if s0 as f64 + 10.0 * u > nxt as f64 {
                break;
            }
            let b: Vec<i64> = (0..10)
                .map(|k| ch.level_at((s0 as f64 + (k as f64 + 0.5) * u) as i64) as i64)
                .collect();
            chars.push((s0, (1..9).map(|k| b[k] << (k - 1)).sum(), b[0] == 0 && b[9] == 1));
            t = s0 as f64 + 9.5 * u;
        }
        if chars.len() >= 2 {
            let v: Vec<i64> = chars.iter().map(|c| c.1).collect();
            f.set("sync", v[0]);
            f.set("pid", v[1]);
            f.set("id", v[1] & 0x3F);
            f.set("pid_ok", lin_pid(v[1] & 0x3F) == v[1]);
            if v.len() >= 3 {
                let data = &v[2..v.len() - 1];
                let cs = v[v.len() - 1];
                f.set("data", data.iter().map(|b| format!("{b:02x}")).collect::<String>());
                f.set("checksum", cs);
                f.set(
                    "checksum_model",
                    if lin_checksum(data, Some(v[1])) == cs {
                        J::from("enhanced")
                    } else if lin_checksum(data, None) == cs {
                        J::from("classic")
                    } else {
                        J::Null
                    },
                );
            }
            f.set("framing_ok", chars.iter().all(|c| c.2));
            fr.push(f);
        }
    }
    let got = jd! {
    "frames" => fr.iter().map(|f| jd! {"id" => f.get("id"), "data" => f.get("data")}).collect::<Vec<_>>(),
    "baud" => fr.iter().map(|f| J::from(rate / f.get("bit_ticks").f().unwrap())).collect::<Vec<_>>(),
    "break_bits" => fr.iter().map(|f| J::from(f.get("break").f().unwrap() / u0)).collect::<Vec<_>>(),
    "delimiter_bits" => fr.iter().map(|f| J::from(f.get("delimiter").f().unwrap() / u0)).collect::<Vec<_>>(),
    "checksum_model" => fr.iter().map(|f| f.get("checksum_model").clone()).collect::<Vec<_>>()};
    if fr.is_empty() {
        return out(V::Ng, got, "no LIN frame (a break and two characters)");
    }
    let mut parts = vec![];
    if !x.get("frames").is_null() {
        let want: Vec<J> = x
            .get("frames")
            .list()
            .iter()
            .map(|f| jd! {"id" => f.get("id"), "data" => s(f, "data").to_lowercase()})
            .collect();
        parts.push(seq_diff("frames", &want, got.get("frames").list()));
    }
    for (i, f) in fr.iter().enumerate() {
        let mut bad = vec![];
        let sync = f.get("sync").i().unwrap();
        if sync != 0x55 {
            bad.push(format!("sync 0x{sync:02x}"));
        }
        if !f.get("pid_ok").truthy() {
            bad.push(format!(
                "identifier 0x{:02x} with wrong parity",
                f.get("pid").i().unwrap()
            ));
        }
        if !f.get("framing_ok").truthy() {
            bad.push("a framing error".into());
        }
        if f.has("checksum") {
            let mut model = if x.has("checksum") {
                s(x, "checksum").to_string()
            } else {
                "auto".into()
            };
            if model == "auto" {
                model = if LIN_CLASSIC_IDS.contains(&f.get("id").i().unwrap()) {
                    "classic"
                } else {
                    "enhanced"
                }
                .into();
            }
            if f.get("checksum_model").s() != Some(model.as_str()) {
                bad.push(format!(
                    "checksum 0x{:02x} is not the {model} one",
                    f.get("checksum").i().unwrap()
                ));
            }
        }
        if !bad.is_empty() {
            parts.push((
                Some(false),
                format!("frame #{i} (id 0x{:02x}): {}", f.get("id").i().unwrap(), bad.join(", ")),
            ));
        }
    }
    let res = ch.step;
    let pairs = |k: &str| -> Vec<(f64, f64)> {
        fr.iter()
            .map(|f| (f.get("t").f().unwrap(), f.get(k).f().unwrap()))
            .collect()
    };
    parts.extend(limit_of("break", &pairs("break"), res, Some(bmin / baud), None, rate));
    parts.extend(limit_of(
        "break delimiter",
        &pairs("delimiter"),
        res,
        Some(1.0 / baud),
        None,
        rate,
    ));
    if let Some(br) = x.get("baud_rel").f() {
        for f in &fr {
            let bt = f.get("bit_ticks").f().unwrap();
            let err = bt / u0 - 1.0;
            let j = judge(err.abs(), res as f64 / (8.0 * bt), None, Some(br));
            if j != Some(true) {
                parts.push((
                    j,
                    format!(
                        "frame at {}: {:.1} baud ({:+.2}%) from its sync field",
                        si(f.get("t").f().unwrap() / rate),
                        rate / bt,
                        err * 100.0
                    ),
                ));
            }
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}
