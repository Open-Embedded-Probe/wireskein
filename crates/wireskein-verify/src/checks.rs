//! The checks (`verify.check_*`): each takes a capture (or a window of one) and an expectation, and gives its
//! verdict, what it measured and why. Here the dispatch, the helpers they share and the checks of waveforms.

use crate::cap::{self, Cap, Ch};
use crate::j::J;
use crate::jd;
use crate::pyfmt::{g, round};
use crate::timing::{self, median, median_i};
use std::collections::BTreeSet;
use std::rc::Rc;

/// A verdict: ok, ng, unchecked (None in Python), or measured only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V {
    Ok,
    Ng,
    Unchecked,
    Measured,
}

impl V {
    pub fn of(b: Option<bool>) -> V {
        match b {
            Some(true) => V::Ok,
            Some(false) => V::Ng,
            None => V::Unchecked,
        }
    }
}

pub type Out = (V, J, String);

pub fn out(v: V, got: J, why: impl Into<String>) -> Out {
    (v, got, why.into())
}

/// `_si`: seconds in s, ms, us or ns, 4 significant digits.
pub fn si(s: f64) -> String {
    let a = s.abs();
    for (unit, f) in [("s", 1.0), ("ms", 1e3), ("us", 1e6), ("ns", 1e9)] {
        if a >= 1.0 / f || unit == "ns" {
            return format!("{} {unit}", g(s * f, 4));
        }
    }
    unreachable!()
}

/// One verdict from several (`_verdicts`): a broken limit fails, else an undecided one leaves it unchecked.
pub fn verdicts(parts: &[(Option<bool>, String)]) -> (V, String) {
    let bad: Vec<&str> = parts
        .iter()
        .filter(|p| p.0 == Some(false))
        .map(|p| p.1.as_str())
        .collect();
    if !bad.is_empty() {
        return (V::Ng, bad.join("; "));
    }
    let open: Vec<&str> = parts.iter().filter(|p| p.0.is_none()).map(|p| p.1.as_str()).collect();
    if !open.is_empty() {
        return (V::Unchecked, open.join("; "));
    }
    (V::Ok, String::new())
}

/// Every instance of a time against [lo, hi] seconds: the worst one decides (`_limit_of`).
pub fn limit_of(
    name: &str,
    ticks: &[(f64, f64)],
    res: i64,
    lo: Option<f64>,
    hi: Option<f64>,
    rate: f64,
) -> Vec<(Option<bool>, String)> {
    let mut out = vec![];
    if ticks.is_empty() {
        return out;
    }
    let res = res as f64;
    if let Some(lo) = lo {
        let k = argmin(ticks.iter().map(|t| t.1));
        let v = ticks[k].1;
        let j = timing::judge(v, res, Some(lo * rate), None);
        out.push((
            j,
            format!(
                "{name} {} {} {} at {}",
                si(v / rate),
                if j == Some(false) { "<" } else { "near" },
                si(lo),
                si(ticks[k].0 / rate)
            ),
        ));
    }
    if let Some(hi) = hi {
        let k = argmax(ticks.iter().map(|t| t.1));
        let v = ticks[k].1;
        let j = timing::judge(v, res, None, Some(hi * rate));
        out.push((
            j,
            format!(
                "{name} {} {} {} at {}",
                si(v / rate),
                if j == Some(false) { ">" } else { "near" },
                si(hi),
                si(ticks[k].0 / rate)
            ),
        ));
    }
    out
}

/// The index of the first smallest value (`np.argmin`).
pub fn argmin(v: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::INFINITY);
    for (i, x) in v.enumerate() {
        if i == 0 || x < best.1 {
            best = (i, x);
        }
    }
    best.0
}

/// The index of the first largest value (`np.argmax`).
pub fn argmax(v: impl Iterator<Item = f64>) -> usize {
    let mut best = (0, f64::NEG_INFINITY);
    for (i, x) in v.enumerate() {
        if i == 0 || x > best.1 {
            best = (i, x);
        }
    }
    best.0
}

/// `_times`: count, min, max and median of the times in seconds.
pub fn times(ticks: &[(f64, f64)], rate: f64) -> J {
    if ticks.is_empty() {
        return jd! {"count" => 0};
    }
    let v: Vec<f64> = ticks.iter().map(|t| t.1).collect();
    jd! {"count" => v.len(), "min_s" => v.iter().cloned().fold(f64::INFINITY, f64::min) / rate,
    "max_s" => v.iter().cloned().fold(f64::NEG_INFINITY, f64::max) / rate, "median_s" => median(&v) / rate}
}

/// (tick, length) pairs as floats.
pub fn fp(v: &[(i64, i64)]) -> Vec<(f64, f64)> {
    v.iter().map(|&(a, b)| (a as f64, b as f64)).collect()
}

/// A line reduced to any_high / any_low: the level an interval reads when the line was at it anywhere in it.
pub fn any(ch: Option<&Ch>) -> Option<u8> {
    match ch?.reduction.as_deref() {
        Some("any_high") => Some(1),
        Some("any_low") => Some(0),
        _ => None,
    }
}

fn s<'a>(x: &'a J, k: &str) -> &'a str {
    x.get(k).s().unwrap_or("")
}

/// The pins an expectation names (`_pins_of`).
pub fn pins_of(x: &J) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for k in [
        "pin", "scl", "sda", "clk", "mosi", "miso", "cs", "a", "b", "hi", "lo", "bclk", "ws", "sd", "tx", "de",
    ] {
        if let Some(p) = x.get(k).s().filter(|p| !p.is_empty()) {
            out.insert(p.to_string());
        }
    }
    for (k, _) in x.get("levels").items() {
        out.insert(k.clone());
    }
    let kind = s(x, "kind");
    if kind != "only_moving" {
        out.extend(x.get("pins").list().iter().filter_map(|p| p.s().map(String::from)));
    }
    if kind == "protocol" {
        out.extend(
            x.get("roles")
                .items()
                .iter()
                .filter_map(|(_, p)| p.s().map(String::from)),
        );
    }
    if kind == "qspi" {
        out.extend(x.get("io").list().iter().filter_map(|p| p.s().map(String::from)));
    }
    out
}

const LOGIC: [&str; 22] = [
    "protocol",
    "becomes",
    "square",
    "level",
    "ends",
    "starts",
    "pulses",
    "i2c",
    "spi",
    "uart",
    "phase",
    "skew",
    "dead_time",
    "i2c_timing",
    "rgb_led",
    "onewire",
    "ir",
    "lin",
    "i2s",
    "quadrature",
    "sd_spi",
    "spdif",
];
const EVENT_CHECKS: [&str; 5] = ["level", "ends", "starts", "pulses", "only_moving"];
pub const CHECKS: [&str; 27] = [
    "protocol",
    "becomes",
    "square",
    "level",
    "ends",
    "starts",
    "only_moving",
    "pulses",
    "i2c",
    "spi",
    "uart",
    "voltage",
    "phase",
    "skew",
    "dead_time",
    "i2c_timing",
    "rgb_led",
    "onewire",
    "ir",
    "lin",
    "i2s",
    "spdif",
    "quadrature",
    "sd_spi",
    "can",
    "rs485",
    "qspi",
];

/// The notice kinds a check finds (`NOTICES`).
pub fn notice_kinds(kind: &str) -> &'static [&'static str] {
    match kind {
        "rgb_led" => &["latch_short"],
        "i2c" => &["scl_stretch", "bus_glitch", "clock_restored", "slow_edge"],
        "spi" => &["sck_outside_cs", "empty_cs_window", "mode_ambiguous"],
        "uart" => &["baud_near_limit", "glitch", "break"],
        "rs485" => &["baud_near_limit", "glitch", "break", "empty_de_window"],
        "can" => &["bitrate_near_limit", "overload"],
        _ => &[],
    }
}

fn run(kind: &str, cap: &Cap, x: &J) -> Out {
    match kind {
        "level" => check_level(cap, x),
        "starts" => levels(cap, x.get("levels"), false),
        "ends" => levels(cap, x.get("levels"), true),
        "only_moving" => check_only_moving(cap, x),
        "pulses" => check_pulses(cap, x),
        "square" => check_square(cap, x),
        "becomes" => check_becomes(cap, x),
        "phase" => check_phase(cap, x),
        "skew" => check_skew(cap, x),
        "dead_time" => check_dead_time(cap, x),
        "voltage" => check_voltage(cap, x),
        k => crate::bus::run(k, cap, x),
    }
}

/// (verdict, measured, why) of one check on one capture (`_judge`).
pub fn judge(cap: &Rc<Cap>, x: &J, pins: &BTreeSet<String>) -> Out {
    let kind = s(x, "kind");
    if !CHECKS.contains(&kind) {
        return out(V::Ng, J::dict(), format!("no check named {}", x.get("kind").repr()));
    }
    if LOGIC.contains(&kind) {
        let wanted: Vec<&str> = cap
            .analog
            .iter()
            .filter(|a| pins.contains(*a) && cap.ch(a).is_none())
            .map(|a| a.as_str())
            .collect();
        if !wanted.is_empty() {
            if x.get("threshold").is_null() {
                return out(
                    V::Unchecked,
                    J::dict(),
                    format!(
                        "{} analog: give threshold= (volts) to read it as logic",
                        wanted.join(", ")
                    ),
                );
            }
            return out(
                V::Unchecked,
                J::dict(),
                format!(
                    "{} analog: analog channels are not read by the Rust verify yet",
                    wanted.join(", ")
                ),
            );
        }
    }
    let hid: Vec<String> = cap
        .events
        .iter()
        .filter(|t| pins.contains(&t.name) && t.hidden() > 0)
        .map(cap::ev_describe)
        .collect();
    if !hid.is_empty() && !EVENT_CHECKS.contains(&kind) {
        return out(
            V::Unchecked,
            J::dict(),
            format!("{}: {kind} needs every edge", hid.join("; ")),
        );
    }
    let trim_s = x.get("trim_s");
    let trim_p = x.get("trim_periods");
    if trim_s.truthy() || trim_p.truthy() {
        let (mut head, mut tail) = if trim_s.truthy() {
            (trim_s.list()[0].f().unwrap_or(0.0), trim_s.list()[1].f().unwrap_or(0.0))
        } else {
            (0.0, 0.0)
        };
        if trim_p.truthy() {
            let per = if x.get("freq_hz").truthy() {
                Some(1.0 / x.get("freq_hz").f().unwrap())
            } else {
                x.get("period_s").f()
            };
            let Some(per) = per.filter(|&p| p != 0.0) else {
                return out(
                    V::Unchecked,
                    J::dict(),
                    format!("trim by periods: {kind} names no period (freq_hz / period_s)"),
                );
            };
            head += trim_p.list()[0].f().unwrap_or(0.0) * per;
            tail += trim_p.list()[1].f().unwrap_or(0.0) * per;
        }
        let s0 = round(head * cap.rate);
        let s1 = cap.n - round(tail * cap.rate);
        if s1 <= s0 {
            return out(
                V::Unchecked,
                J::dict(),
                format!(
                    "the window is {:.3} ms: trimming {} ms and {} ms leaves nothing",
                    cap.n as f64 / cap.rate * 1e3,
                    g(head * 1e3, 6),
                    g(tail * 1e3, 6)
                ),
            );
        }
        let w = cap::window(cap, s0, s1);
        let (v, mut got, why) = run(kind, &w, x);
        got.set("trimmed", J::from(vec![s0, s1]));
        return (v, got, why);
    }
    run(kind, cap, x)
}

// ---------------- the checks of waveforms ----------------

fn check_level(cap: &Cap, x: &J) -> Out {
    let pin = s(x, "pin");
    let want = x.get("value").i().unwrap_or(0) as u8;
    if let Some(t) = cap.ev(pin) {
        let ok = !cap::ev_moved(t) && t.start_level == want;
        return out(
            V::of(Some(ok)),
            jd! {"initial" => t.start_level, "changing_quanta" => t.quanta.len(), "step" => t.step},
            if ok {
                String::new()
            } else {
                format!("not constant {want}")
            },
        );
    }
    let Some(ch) = cap.ch(pin) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let stride = 1.max(cap.n / 4096);
    let (mut hits, mut k) = (0usize, 0usize);
    let mut t = 0;
    while t < cap.n {
        hits += ch.level_at(t) as usize;
        k += 1;
        t += stride;
    }
    let got = jd! {"initial" => ch.initial, "edges" => ch.edges.len(),
    "high_fraction" => if k > 0 { hits as f64 / k as f64 } else { f64::NAN }};
    let ok = ch.edges.is_empty() && ch.initial == want;
    if ok && any(Some(ch)) == Some(want) {
        return out(
            V::Unchecked,
            got,
            format!(
                "{} is {} at {} ticks an interval: an interval at {want} may hold a {}",
                ch.name,
                ch.reduction.as_deref().unwrap_or(""),
                ch.step,
                1 - want
            ),
        );
    }
    out(
        V::of(Some(ok)),
        got,
        if ok {
            String::new()
        } else {
            format!("not constant {want}")
        },
    )
}

fn levels(cap: &Cap, want: &J, at_end: bool) -> Out {
    let mut got = J::dict();
    let (mut bad, mut missing, mut unknown) = (vec![], vec![], vec![]);
    for (pin, v) in want.items() {
        let v = v.i().unwrap_or(0) as u8;
        let level;
        if let Some(t) = cap.ev(pin) {
            let l = if at_end {
                Some(cap::ev_end_level(t))
            } else {
                cap::ev_first_level(t)
            };
            got.set(pin, l);
            match l {
                None => {
                    unknown.push(format!(
                        "{}: quantum 0 changed and its {} ticks do not say whether at the first",
                        t.name, t.step
                    ));
                    continue;
                }
                Some(l) => level = l,
            }
        } else if let Some(ch) = cap.ch(pin) {
            level = if at_end { ch.level_at(cap.n - 1) } else { ch.initial };
            got.set(pin, level);
            if any(Some(ch)) == Some(level) {
                unknown.push(format!(
                    "{pin}: {} at {} ticks an interval",
                    ch.reduction.as_deref().unwrap_or(""),
                    ch.step
                ));
                continue;
            }
        } else {
            missing.push(pin.clone());
            continue;
        }
        if level != v {
            bad.push(format!("{pin}={level} (want {v})"));
        }
    }
    if !missing.is_empty() && got.items().is_empty() {
        return out(V::Unchecked, got, format!("pins not captured: {}", missing.join(", ")));
    }
    if !bad.is_empty() {
        return out(V::Ng, got, bad.join("; "));
    }
    if !unknown.is_empty() {
        return out(
            V::Unchecked,
            got,
            format!(
                "{}: the summaries do not tell its level at the {}",
                unknown.join("; "),
                if at_end { "end" } else { "start" }
            ),
        );
    }
    out(V::Ok, got, "")
}

fn check_only_moving(cap: &Cap, x: &J) -> Out {
    let pins: Vec<&str> = x.get("pins").list().iter().filter_map(|p| p.s()).collect();
    let kept: BTreeSet<&str> = cap.events.iter().map(|t| t.name.as_str()).collect();
    let mut moved = J::dict();
    for c in &cap.channels {
        if !c.edges.is_empty() && !pins.contains(&c.name.as_str()) && !kept.contains(c.name.as_str()) {
            moved.set(&c.name, c.edges.len());
        }
    }
    for t in &cap.events {
        if !pins.contains(&t.name.as_str()) && cap::ev_moved(t) {
            moved.set(&t.name, t.counts.iter().map(|&c| c as i64).sum::<i64>());
        }
    }
    if !moved.items().is_empty() {
        let names: Vec<&str> = moved.items().iter().map(|(k, _)| k.as_str()).collect();
        let why = format!("other pins moved: {}", names.join(", "));
        return out(V::Ng, jd! {"unexpected" => moved}, why);
    }
    out(V::Ok, jd! {"unexpected" => moved}, "")
}

/// (start, width) in ticks of the pulses at `level` that start and end inside the window.
fn whole_pulses(ch: &Ch, level: u8) -> Vec<(i64, i64)> {
    let e = &ch.edges;
    (0..e.len())
        .filter(|&i| ch.after(i) == level && i + 1 < e.len())
        .map(|i| (e[i], e[i + 1] - e[i]))
        .collect()
}

fn pyl(v: &J) -> String {
    v.repr()
}

fn check_pulses(cap: &Cap, x: &J) -> Out {
    let pin = s(x, "pin");
    let ch = cap.ch(pin);
    let t = cap.ev(pin);
    let widths = !x.get("width_min_s").is_null() || !x.get("width_max_s").is_null();
    let level_j = if x.has("level") {
        x.get("level").clone()
    } else {
        J::Int(1)
    };
    if let Some(t) = t
        && widths
    {
        return out(
            V::Unchecked,
            J::dict(),
            format!("{}: a pulse's width needs every edge", cap::ev_describe(t)),
        );
    }
    if let (Some(_), true) = (any(ch), widths) {
        let ch = ch.unwrap();
        return out(
            V::Unchecked,
            J::dict(),
            format!(
                "{} is {} at {} ticks an interval: pulses shorter than one merge or vanish, so their widths are not known",
                ch.name,
                ch.reduction.as_deref().unwrap_or(""),
                ch.step
            ),
        );
    }
    if widths {
        let Some(ch) = ch else {
            return out(V::Unchecked, J::dict(), "pin not captured");
        };
        let p = whole_pulses(ch, level_j.i().unwrap_or(1) as u8);
        let ws: Vec<f64> = p.iter().map(|&(_, w)| w as f64 / cap.rate).collect();
        let got = jd! {"pulses" => ws.len(), "widths_s" => ws.clone(),
        "starts_s" => p.iter().map(|&(s, _)| s as f64 / cap.rate).collect::<Vec<_>>()};
        let mut why = vec![];
        if let Some(c) = x.get("count").i()
            && ws.len() as i64 != c
        {
            why.push(format!("{} whole pulses at {}, want {c}", ws.len(), pyl(&level_j)));
        }
        let res = ch.step as f64 / cap.rate;
        let (lo, hi) = (x.get("width_min_s").f(), x.get("width_max_s").f());
        let bad: Vec<f64> = ws
            .iter()
            .copied()
            .filter(|&v| lo.is_some_and(|lo| v < lo - res) || hi.is_some_and(|hi| v > hi + res))
            .collect();
        if !bad.is_empty() {
            why.push(format!(
                "{} of {} pulses outside {}..{} s (first {} s)",
                bad.len(),
                ws.len(),
                x.get("width_min_s").repr(),
                x.get("width_max_s").repr(),
                g(bad[0], 6)
            ));
        }
        if ws.is_empty() && x.get("count").is_null() {
            why.push(format!("no whole pulse at {}", pyl(&level_j)));
        }
        return out(V::of(Some(why.is_empty())), got, why.join("; "));
    }
    if let Some(t) = t
        && (ch.is_none() || x.get("period_s").is_null())
    {
        let (n, exact) = cap::ev_rises(t);
        let got = if exact {
            jd! {"rises" => n}
        } else {
            jd! {"rises_at_least" => n}
        };
        if let Some(c) = x.get("count").i() {
            if (exact && n != c) || (!exact && n > c) {
                return out(
                    V::Ng,
                    got,
                    format!("{}{n} rising edges, want {c}", if exact { "" } else { "at least " }),
                );
            }
            if !exact {
                return out(
                    V::Unchecked,
                    got,
                    format!(
                        "{}: at least {n} rising edges (a quantum's count saturated at 127)",
                        t.name
                    ),
                );
            }
        }
        if !x.get("period_s").is_null() {
            return out(
                V::Unchecked,
                got,
                format!("{}: the period needs every edge", cap::ev_describe(t)),
            );
        }
        return out(V::Ok, got, "");
    }
    let Some(ch) = ch else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let rises = ch.rises();
    let reduced = any(Some(ch)).is_some();
    let mut got = if reduced {
        jd! {"rises_at_least" => rises.len()}
    } else {
        jd! {"rises" => rises.len()}
    };
    let (mut ok, mut why) = (Some(true), vec![]);
    let n = rises.len() as i64;
    if let Some(c) = x.get("count").i() {
        if if reduced { n > c } else { n != c } {
            ok = Some(false);
            why.push(format!(
                "{}{n} rising edges, want {c}",
                if reduced { "at least " } else { "" }
            ));
        } else if reduced && n < c {
            ok = None;
            why.push(format!(
                "at least {n} rising edges ({} at {} ticks an interval), want {c}",
                ch.reduction.as_deref().unwrap_or(""),
                ch.step
            ));
        }
    }
    if let Some(ps) = x.get("period_s").f()
        && rises.len() >= 2
    {
        let d: Vec<i64> = rises.windows(2).map(|w| w[1] - w[0]).collect();
        let per = median_i(&d) / cap.rate;
        got.set("period_s", per);
        if (per / ps - 1.0).abs() > x.get("period_rel").f().unwrap_or(0.05) {
            ok = Some(false);
            why.push(format!("period {:.2} us vs {:.2} us", per * 1e6, ps * 1e6));
        }
    }
    out(V::of(ok), got, why.join("; "))
}

/// `markers._periodic`: frequency / duty of a line toggling regularly, or None.
fn periodic(ed: &[i64], level_before: u8, rate: f64, step: i64) -> Option<J> {
    let o = if level_before == 0 { 0 } else { 1 };
    let rises: Vec<i64> = ed.iter().skip(o).step_by(2).copied().collect();
    let falls: Vec<i64> = ed.iter().skip(1 - o).step_by(2).copied().collect();
    if rises.len() < 4 {
        return None;
    }
    let per: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let med = median(&per);
    let near = per.iter().filter(|&&p| (p - med).abs() <= 0.05 * med).count() as f64 / per.len() as f64;
    if med <= 0.0 || near < 0.9 {
        return None;
    }
    let mut high = vec![];
    for (i, &r) in rises[..rises.len() - 1].iter().enumerate() {
        let j = falls.partition_point(|&f| f < r);
        if j < falls.len() {
            let h = (falls[j] - r) as f64;
            if h < per[i] {
                high.push(h / med);
            }
        }
    }
    let mean = per.iter().sum::<f64>() / per.len() as f64;
    let std = (per.iter().map(|p| (p - mean).powi(2)).sum::<f64>() / per.len() as f64).sqrt();
    Some(
        jd! {"freq_hz" => rate * per.len() as f64 / (rises[rises.len() - 1] - rises[0]) as f64,
        "duty" => if high.is_empty() { J::Null } else { J::Float(median(&high)) },
        "periods" => per.len(), "period_spread" => std / med, "resolution" => step as f64 / med},
    )
}

/// Every whole pulse: (rise, high, period to the next rise) in ticks (`_pulse_rows`).
fn pulse_rows(ch: &Ch) -> Vec<[i64; 3]> {
    let (rises, falls) = (ch.rises(), ch.falls());
    if rises.len() < 2 {
        return vec![];
    }
    let mut out = vec![];
    for i in 0..rises.len() - 1 {
        let j = falls.partition_point(|&f| f < rises[i]);
        if j < falls.len() {
            let r = rises[i];
            out.push([r, falls[j] - r, rises[i + 1] - r]);
        }
    }
    out
}

/// Least squares of y = a * n + b: (a, b), as `np.linalg.lstsq`.
fn fit_line(n: &[f64], y: &[f64]) -> (f64, f64) {
    let m = n.len() as f64;
    let (sx, sy) = (n.iter().sum::<f64>(), y.iter().sum::<f64>());
    let (mx, my) = (sx / m, sy / m);
    let sxx: f64 = n.iter().map(|v| (v - mx) * (v - mx)).sum();
    if sxx == 0.0 {
        // all the same n: the minimum-norm solution
        let nn = n[0];
        let s = my / (nn * nn + 1.0);
        return (nn * s, s);
    }
    let sxy: f64 = n.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let a = sxy / sxx;
    (a, my - a * mx)
}

fn check_square(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let Some(mut p) = periodic(&ch.edges, ch.initial, cap.rate, ch.step) else {
        return out(V::Ng, jd! {"edges" => ch.edges.len()}, "no steady square wave");
    };
    let fx = x.get("freq_hz").f().unwrap();
    let freq_rel = x.get("freq_rel").f().unwrap_or(0.02);
    let pf = p.get("freq_hz").f().unwrap();
    let ferr = pf / fx - 1.0;
    let mut ok = ferr.abs() <= freq_rel;
    let mut why = vec![];
    if !ok {
        why.push(format!("frequency {pf:.2} Hz is {:+.2}% off", ferr * 100.0));
    }
    let duty = x.get("duty").f();
    let duty_abs = x.get("duty_abs").f().unwrap_or(0.01);
    if let Some(d) = duty {
        let pd = p.get("duty").f();
        if pd.is_none_or(|pd| (pd - d).abs() > duty_abs + p.get("resolution").f().unwrap()) {
            ok = false;
            why.push(format!(
                "duty {} vs {d:.4}",
                pd.map(|v| format!("{v:.4}")).unwrap_or("None".into())
            ));
        }
    }
    if let Some(jm) = x.get("jitter_max").f() {
        let sp = p.get("period_spread").f().unwrap();
        if sp > jm {
            ok = false;
            why.push(format!("period spread {sp:.4} > {}", crate::pyfmt::repr_f64(jm)));
        }
    }
    let rows = pulse_rows(ch);
    let per = cap.rate / fx;
    let tol = (ch.step + 1) as f64;
    let bad: Vec<bool> = rows
        .iter()
        .map(|r| {
            let mut b = (r[2] as f64 - per).abs() > freq_rel * per + tol;
            if let Some(d) = duty {
                b |= (r[1] as f64 - d * per).abs() > duty_abs * per + tol;
            }
            b
        })
        .collect();
    let nbad = bad.iter().filter(|&&b| b).count();
    p.set("freq_error", ferr);
    p.set("outliers", nbad);
    let rises: Vec<f64> = ch.rises().iter().map(|&v| v as f64).collect();
    if rises.len() >= 2 {
        let n: Vec<f64> = rises.iter().map(|r| ((r - rises[0]) / per).round_ties_even()).collect();
        let dev: Vec<f64> = rises.iter().zip(&n).map(|(r, k)| r - rises[0] - k * per).collect();
        let k = argmax(dev.iter().map(|d| d.abs()));
        p.set("grid_max_s", dev[k].abs() / cap.rate);
        let (a, b) = fit_line(&n, &rises);
        let fm = rises
            .iter()
            .zip(&n)
            .map(|(r, k)| (r - (a * k + b)).abs())
            .fold(f64::NEG_INFINITY, f64::max);
        p.set("grid_fit_max_s", fm / cap.rate);
        p.set("grid_fit_period_s", a / cap.rate);
        if let Some(ga) = x.get("grid_abs_s").f()
            && dev[k].abs() / cap.rate > ga + 1e-12
        {
            ok = false;
            why.push(format!(
                "rising edge at {:.4} ms is {:+.1} ns off the grid (over {:.1} ns)",
                rises[k] / cap.rate * 1e3,
                dev[k] / cap.rate * 1e9,
                ga * 1e9
            ));
        }
    }
    if nbad as i64 > x.get("outliers_max").i().unwrap_or(0) {
        ok = false;
        let r = rows[bad.iter().position(|&b| b).unwrap()];
        why.push(format!(
            "{nbad} of {} periods out of tolerance (first at {:.3} ms: high {:.1} us, period {:.1} us)",
            rows.len(),
            r[0] as f64 / cap.rate * 1e3,
            r[1] as f64 / cap.rate * 1e6,
            r[2] as f64 / cap.rate * 1e6
        ));
    }
    out(V::of(Some(ok)), p, why.join("; "))
}

fn check_phase(cap: &Cap, x: &J) -> Out {
    let (Some(a), Some(b)) = (cap.ch(s(x, "a")), cap.ch(s(x, "b"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let edge = x.get("edge").s().unwrap_or("rise");
    let (p, deg) = timing::phase(a, b, cap.rate, edge);
    if p.get("pairs").i() == Some(0) {
        let why = format!("no {} edge within a period after {}'s", s(x, "b"), s(x, "a"));
        return out(V::Ng, p, why);
    }
    let mut parts = vec![];
    if let Some(want) = x.get("phase_deg").f() {
        let dev: Vec<f64> = deg.iter().map(|&d| timing::circular_diff(d, want).abs()).collect();
        let k = argmax(dev.iter().copied());
        let rd = p.get("resolution_deg").f().unwrap();
        let j = timing::judge(dev[k], rd, None, x.get("phase_abs_deg").f());
        parts.push((
            j,
            format!(
                "phase {:.2} deg vs {} +- {} (resolution {rd:.2} deg)",
                deg[k],
                x.get("phase_deg").repr(),
                x.get("phase_abs_deg").repr()
            ),
        ));
    }
    if let Some(fx) = x.get("freq_hz").f() {
        for k in ["freq_hz", "freq_b_hz"] {
            let f = p.get(k).f().unwrap();
            let err = f / fx - 1.0;
            parts.push((
                Some(err.abs() <= x.get("freq_rel").f().unwrap_or(0.02)),
                format!(
                    "{} {f:.2} Hz is {:+.2}% off",
                    s(x, if k == "freq_hz" { "a" } else { "b" }),
                    err * 100.0
                ),
            ));
        }
    }
    if parts.is_empty() {
        return out(V::Measured, p, "");
    }
    let (v, why) = verdicts(&parts);
    out(v, p, why)
}

fn check_skew(cap: &Cap, x: &J) -> Out {
    let (Some(a), Some(b)) = (cap.ch(s(x, "a")), cap.ch(s(x, "b"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let r = timing::skew(a, b, x.get("edge").s().unwrap_or("both"), cap.n);
    let d: Vec<f64> = r.delays.iter().map(|&v| v as f64).collect();
    let res = timing::res_of(&[a, b]);
    let mut got = jd! {"pairs" => d.len(), "unpaired_a" => r.unpaired_a, "unpaired_b" => r.unpaired_b,
    "resolution_s" => res as f64 / cap.rate};
    if d.is_empty() {
        return out(V::Ng, got, format!("no edge of {} near {}'s", s(x, "b"), s(x, "a")));
    }
    got.set("min_s", d.iter().cloned().fold(f64::INFINITY, f64::min) / cap.rate);
    got.set("max_s", d.iter().cloned().fold(f64::NEG_INFINITY, f64::max) / cap.rate);
    got.set("median_s", median(&d) / cap.rate);
    let mut parts = vec![];
    if r.unpaired_a > 0 || r.unpaired_b > 0 {
        parts.push((
            Some(false),
            format!(
                "{} edges of {} and {} of {} without a partner",
                r.unpaired_a,
                s(x, "a"),
                r.unpaired_b,
                s(x, "b")
            ),
        ));
    }
    let delay = x.get("delay_s").f().unwrap_or(0.0);
    let want = delay * cap.rate;
    let k = argmax(d.iter().map(|v| (v - want).abs()));
    let abs = x.get("delay_abs_s").f().unwrap();
    parts.push((
        timing::judge((d[k] - want).abs(), res as f64, None, Some(abs * cap.rate)),
        format!(
            "{} {} after {} vs {} +- {}",
            s(x, "b"),
            si(d[k] / cap.rate),
            s(x, "a"),
            si(delay),
            si(abs)
        ),
    ));
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

fn check_dead_time(cap: &Cap, x: &J) -> Out {
    let (Some(hi), Some(lo)) = (cap.ch(s(x, "hi")), cap.ch(s(x, "lo"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let r = timing::dead_time(hi, lo, cap.n, x.get("active").i().unwrap_or(1) as u8);
    let res = timing::res_of(&[hi, lo]);
    let got = jd! {"hi_to_lo" => times(&fp(&r.hi_to_lo), cap.rate), "lo_to_hi" => times(&fp(&r.lo_to_hi), cap.rate),
    "overlaps" => times(&fp(&r.overlaps), cap.rate), "resolution_s" => res as f64 / cap.rate};
    if r.hi_to_lo.is_empty() && r.lo_to_hi.is_empty() && r.overlaps.is_empty() {
        return out(V::Ng, got, "the outputs never switched from one to the other");
    }
    let mut parts = vec![];
    if !r.overlaps.is_empty() && !x.get("allow_overlap").truthy() {
        let (t, ln) = r.overlaps[argmax(r.overlaps.iter().map(|o| o.1 as f64))];
        parts.push((
            Some(false),
            format!(
                "both on {} times, longest {} at {}",
                r.overlaps.len(),
                si(ln as f64 / cap.rate),
                si(t as f64 / cap.rate)
            ),
        ));
    }
    for (side, v) in [("hi_to_lo", &r.hi_to_lo), ("lo_to_hi", &r.lo_to_hi)] {
        if !v.is_empty() {
            let v = &fp(v);
            parts.extend(limit_of(
                side,
                v,
                res,
                x.get("dead_min_s").f(),
                x.get("dead_max_s").f(),
                cap.rate,
            ));
            parts.extend(limit_of(
                side,
                v,
                res,
                x.get(&format!("{side}_min_s")).f(),
                x.get(&format!("{side}_max_s")).f(),
                cap.rate,
            ));
        }
    }
    if parts.is_empty() {
        return out(V::Measured, got, "");
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

fn check_voltage(cap: &Cap, x: &J) -> Out {
    let pin = s(x, "pin");
    if !cap.analog.iter().any(|a| a == pin) {
        return out(
            V::Unchecked,
            J::dict(),
            if cap.ch(pin).is_some() {
                "not an analog channel in this capture"
            } else {
                "pin not captured"
            },
        );
    }
    out(
        V::Unchecked,
        J::dict(),
        "analog channels are not read by the Rust verify yet",
    )
}

// ---------------- becomes ----------------

fn state_text(prior: &Option<Prior>, rate: f64) -> J {
    match prior {
        None => J::Null,
        Some(Prior::Level(v)) => jd! {"level" => *v},
        Some(Prior::Wave(r)) => jd! {"period_s" => r[2] as f64 / rate, "high_s" => r[1] as f64 / rate},
    }
}

#[derive(Clone, Copy)]
enum Prior {
    Level(u8),
    Wave([i64; 3]),
}

fn check_becomes(cap: &Cap, x: &J) -> Out {
    let pin = s(x, "pin");
    if cap.ev(pin).is_some() {
        return out(
            V::Unchecked,
            J::dict(),
            format!("{pin} kept as changes: the states need every edge"),
        );
    }
    let Some(ch) = cap.ch(pin) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let (rate, q, n) = (cap.rate, (ch.step + 1) as f64, cap.n);
    let (parent, s0) = match &cap.parent {
        Some((p, s0)) => (Some(p.clone()), *s0),
        None => (None, 0),
    };
    let pch = parent.as_ref().and_then(|p| p.ch(pin).cloned());
    let rows_all: Vec<[i64; 3]> = match &pch {
        Some(pc) => pulse_rows(pc).into_iter().map(|r| [r[0] - s0, r[1], r[2]]).collect(),
        None => pulse_rows(ch),
    };
    let rows: Vec<[i64; 3]> = rows_all
        .iter()
        .copied()
        .filter(|r| r[0] >= 0 && r[0] + r[2] <= n)
        .collect();
    let e = &ch.edges;
    let mut got = J::dict();
    let similar = |r: &[i64; 3], rf: &[i64; 3]| -> bool {
        ((r[2] - rf[2]).abs() as f64) <= (2.0 * q).max(0.02 * rf[2] as f64)
            && ((r[1] - rf[1]).abs() as f64) <= (2.0 * q).max(0.02 * rf[1] as f64)
    };
    let mut prior: Option<Prior> = None;
    if let Some(pc) = &pch {
        let pre_all: Vec<[i64; 3]> = rows_all.iter().copied().filter(|r| r[0] + r[2] <= 0).collect();
        let pre = &pre_all[pre_all.len().saturating_sub(3)..];
        let running = pre.len() >= 2 && {
            let last = pre[pre.len() - 1];
            pre.iter().all(|r| similar(r, &last))
                && (last[0] + 2 * last[2]) as f64 + (2.0 * q).max(0.02 * last[2] as f64) > 0.0
        };
        prior = Some(if running {
            Prior::Wave(pre[pre.len() - 1])
        } else {
            Prior::Level(pc.level_at(s0))
        });
    }
    let before: Vec<[i64; 3]>;
    let last_high: Option<i64>;
    let irregular_max = x.get("irregular_max").i().unwrap_or(1);
    if !x.get("level").is_null() {
        let v = x.get("level").i().unwrap() as u8;
        let end = ch.initial ^ (e.len() as u8 & 1);
        let t = e.last().copied().unwrap_or(0);
        got.set("level_after", end);
        got.set("hold_s", (n - t) as f64 / rate);
        if end != v {
            return out(V::Ng, got, format!("ends at {end}, not {v}"));
        }
        if let Some(Prior::Wave(r)) = prior
            && !e.is_empty()
            && n - t < 2 * r[2]
        {
            let why = format!(
                "at {v} for the last {:.1} us only: not told from the wave (period {:.1} us) going on",
                (n - t) as f64 / rate * 1e6,
                r[2] as f64 / rate * 1e6
            );
            return out(V::Ng, got, why);
        }
        before = rows.iter().copied().filter(|r| r[0] + r[2] <= t).collect();
        got.set(
            "change_s",
            if e.is_empty() {
                J::Null
            } else {
                J::Float(t as f64 / rate)
            },
        );
        if e.is_empty()
            && let (Some(Prior::Wave(r)), Some(pc)) = (prior, &pch)
        {
            let rises: Vec<i64> = pc.rises().into_iter().filter(|&x| x < s0).collect();
            if let Some(&last) = rises.last() {
                let due = last + r[2] - s0;
                if 0 <= due && due < n {
                    got.set("change_s", due as f64 / rate);
                    got.set("missed", true);
                }
            }
        }
        last_high = if v == 0 && e.len() >= 2 && (before.is_empty() || e[e.len() - 2] > before[before.len() - 1][0]) {
            Some(e[e.len() - 1] - e[e.len() - 2])
        } else {
            None
        };
    } else {
        let per = if x.get("freq_hz").truthy() {
            rate / x.get("freq_hz").f().unwrap()
        } else {
            x.get("period_s").f().unwrap() * rate
        };
        let high = if let Some(d) = x.get("duty").f() {
            Some(d * per)
        } else {
            x.get("high_s").f().map(|h| h * rate)
        };
        let ptol = x.get("freq_rel").f().unwrap_or(0.02) * per + q;
        let a = if !x.get("duty").is_null() {
            x.get("duty_abs").f().unwrap_or(0.01) * per
        } else {
            x.get("high_abs_s").f().unwrap_or(0.0) * rate
        };
        let htol = a.max(x.get("high_rel").f().unwrap_or(0.0) * high.unwrap_or(0.0)) + q;
        if rows.is_empty() {
            return out(V::Ng, jd! {"edges" => e.len()}, "no whole period");
        }
        let m: Vec<bool> = rows
            .iter()
            .map(|r| (r[2] as f64 - per).abs() <= ptol && high.is_none_or(|h| (r[1] as f64 - h).abs() <= htol))
            .collect();
        let mut k = m.len();
        while k > 0 && m[k - 1] {
            k -= 1;
        }
        let tail = &rows[k..];
        got.set("periods", tail.len());
        if !tail.is_empty() {
            let p2: Vec<f64> = tail.iter().map(|r| r[2] as f64).collect();
            let p1: Vec<f64> = tail.iter().map(|r| r[1] as f64).collect();
            let mp = median(&p2);
            got.set("period_s", mp / rate);
            got.set("freq_hz", rate / mp);
            got.set("high_s", median(&p1) / rate);
            got.set(
                "duty",
                median(&tail.iter().map(|r| r[1] as f64 / r[2] as f64).collect::<Vec<_>>()),
            );
            got.set(
                "worst_period_rel",
                p2.iter().map(|v| (v - per).abs()).fold(f64::NEG_INFINITY, f64::max) / per,
            );
            got.set(
                "worst_high_s",
                match high {
                    Some(h) => J::Float(p1.iter().map(|v| (v - h).abs()).fold(f64::NEG_INFINITY, f64::max) / rate),
                    None => J::Null,
                },
            );
        }
        let min_periods = x.get("min_periods").i().unwrap_or(3);
        if (tail.len() as i64) < min_periods {
            let mut why = format!(
                "{} whole periods in the state at the end, want {min_periods}",
                tail.len()
            );
            if k > 0 {
                let b = rows[k - 1];
                why += &format!(
                    " (last other pulse: high {:.1} us, period {:.1} us)",
                    b[1] as f64 / rate * 1e6,
                    b[2] as f64 / rate * 1e6
                );
            }
            return out(V::Ng, got, why);
        }
        let t = tail[0][0];
        before = rows[..k].to_vec();
        last_high = None;
        let changed = if prior.is_some() {
            let was = matches!(prior, Some(Prior::Wave(r)) if similar(&tail[0], &r));
            k > 0 || !was
        } else {
            k > 0 || t as f64 > per + ptol
        };
        got.set("change_s", if changed { J::Float(t as f64 / rate) } else { J::Null });
    }
    // before the change: the prior state, then at most irregular_max pulses of neither
    if prior.is_none() && !before.is_empty() {
        let all = before.iter().all(|r| similar(r, &before[0]));
        let lead = if all {
            before.len()
        } else {
            before.iter().position(|r| !similar(r, &before[0])).unwrap()
        };
        if lead >= 2 {
            prior = Some(Prior::Wave(before[0]));
        }
    }
    let rest: Vec<[i64; 3]> = match prior {
        Some(Prior::Wave(r)) if !before.is_empty() => {
            let lead = before.iter().position(|b| !similar(b, &r)).unwrap_or(before.len());
            before[lead..].to_vec()
        }
        _ => before.clone(),
    };
    if rest.len() >= 2 && rest.iter().filter(|r| similar(r, &rest[0])).count() >= 2 && rest.len() as i64 > irregular_max
    {
        got.set("before", state_text(&prior, rate));
        return out(V::Ng, got, "more than one change in the window");
    }
    let mut odd: Vec<J> = rest
        .iter()
        .map(|r| jd! {"high_s" => r[1] as f64 / rate, "period_s" => r[2] as f64 / rate})
        .collect();
    if let (Some(lh), Some(Prior::Wave(r))) = (last_high, prior)
        && rest.is_empty()
        && ((lh - r[1]).abs() as f64) > (2.0 * q).max(0.02 * r[1] as f64)
    {
        odd.push(jd! {"high_s" => lh as f64 / rate, "period_s" => J::Null});
    }
    let irregular = odd.len() as i64;
    got.set("before", state_text(&prior, rate));
    got.set("irregular", irregular);
    got.set("irregular_pulses", J::List(odd));
    if let Some(c) = got.get("change_s").f() {
        let base = cap.window_base();
        got.set("change_tick", round(c * rate) + base);
    }
    if irregular > irregular_max {
        return out(
            V::Ng,
            got,
            format!("{irregular} pulses of neither state at the change (at most {irregular_max})"),
        );
    }
    let imin = x.get("irregular_min").i().unwrap_or(0);
    if irregular < imin {
        return out(
            V::Ng,
            got,
            format!("{irregular} pulses of neither state at the change (at least {imin})"),
        );
    }
    if let (Some(w), Some(c)) = (x.get("within_s").f(), got.get("change_s").f())
        && c > w
    {
        let why = format!(
            "changed {:.3} ms into the window, want within {} ms",
            c * 1e3,
            g(w * 1e3, 6)
        );
        return out(V::Ng, got, why);
    }
    out(V::Ok, got, "")
}
