//! UART on one line (R1 of bench/notes/2026-11-24-rust-plan.ja.md), after the Python main engine's reading
//! (`_engine/typed.py` chars, `survey.char_length`, `staged.UartPlugin`), not a copy of it: judged on the bench against
//! it, equal or better. What it leaves out for now (each a candidate to add when the bench shows the need): rate
//! switches inside a line (one block), views with spikes or bounces taken out, other lines' evidence (keeping to a
//! clock, a select's windows), the LIN / DMX512 / USB / CEC rivals.

use crate::claim::Claim;
use crate::features::{Features, estimate_units, features};
use crate::kernels::{chain, debounce, deglitch, gated, last_at_or_before, median, quantile, runs, shortest_first};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const CLAIM_COST: f64 = 0.3;
pub const ASYNC_MIN_UNIT: f64 = 2.0;
pub const START_PREFIX: usize = 20000;
pub const REFIT_MIN: f64 = 0.002;
pub const UART_CLOCK_LIKE: f64 = 0.2;
pub const UART_FIXED_BITS: f64 = 0.25;
pub const UART_PACED: f64 = 0.5;
pub const GRID_LOW: f64 = 0.88;
pub const GRID_SPAN: f64 = 0.04;
pub const UART_PARITY_HOLDS: f64 = 0.98;
pub const UART_PARITY_MIN: usize = 8;
pub const UART_PARITY_BOTH: f64 = 0.05;
pub const UART_OTHER_IDLE: f64 = 0.9;
pub const UART_REST_BITS: f64 = 11.0;
pub const UART_RESTS: usize = 2;
pub const TWO_VALUES: f64 = 0.6;
pub const UART_SYNC: f64 = 0.2;
pub const SYNC_MIN_PERIOD: f64 = 4.0;
pub const CLOCKS_A_BUS: f64 = 0.8;
pub const CLOCKS_A_BUS_ASYNC: f64 = 0.6;
pub const UART_FRAMED: f64 = 0.35;
pub const DMX_BAUD: f64 = 250000.0;
pub const DMX_RATE_TOL: f64 = 0.03;
pub const DMX_ONE_PACKET: usize = 24;

/// The characters between consecutive breaks: [(values & 0xFF, ok)] (`plugins_uartlike._split_on_breaks`).
fn split_on_breaks(ch: &Chars) -> Vec<(Vec<i64>, Vec<bool>)> {
    let mut b = ch.breaks.clone();
    b.push(i64::MAX);
    b.windows(2)
        .map(|w| {
            let lo = ch.start.partition_point(|&s| s <= w[0]);
            let hi = ch.start.partition_point(|&s| s < w[1]);
            let (lo, hi) = (lo.min(hi), hi);
            (
                ch.values[lo..hi].iter().map(|&x| x & 0xFF).collect(),
                ch.ok[lo..hi].to_vec(),
            )
        })
        .collect()
}

pub fn lin_pid(ident: i64) -> i64 {
    let b: Vec<i64> = (0..6).map(|i| (ident >> i) & 1).collect();
    let p0 = b[0] ^ b[1] ^ b[2] ^ b[4];
    let p1 = 1 - (b[1] ^ b[3] ^ b[4] ^ b[5]);
    ident | (p0 << 6) | (p1 << 7)
}

pub fn lin_checksum(data: &[i64], pid: i64) -> i64 {
    let mut s = pid;
    for &x in data {
        s += x;
        s = (s & 0xFF) + (s >> 8);
    }
    !s & 0xFF
}

/// How far the characters are framed by breaks the way LIN or DMX512 frames them (`UartPlugin`'s framed_by_breaks):
/// the share of LIN's checks that hold, or of DMX512 packets with start code 0 at its rate; 0 with no breaks.
fn framed_by_breaks(ch: &Chars, baud: f64) -> f64 {
    if ch.breaks.is_empty() {
        return 0.0;
    }
    let segs = split_on_breaks(ch);
    let mut framed: f64 = 0.0;
    // LIN: sync 0x55, the PID's parity, the enhanced checksum
    let (mut frames, mut good) = (0usize, 0usize);
    for (v, _) in segs.iter().filter(|(v, _)| v.len() >= 3) {
        frames += 1;
        let (pid, cs) = (v[1], v[v.len() - 1]);
        good += (v[0] == 0x55) as usize + (lin_pid(pid & 0x3F) == pid) as usize;
        good += (lin_checksum(&v[2..v.len() - 1], pid) == cs) as usize;
    }
    if frames >= 2 && ch.breaks.len() >= 2 {
        framed = framed.max(good as f64 / (3 * frames) as f64);
    }
    // DMX512: start code 0, at 250 kbaud
    let (mut packets, mut zero, mut chars) = (0usize, 0usize, 0usize);
    for (v, ok) in segs.iter().filter(|(v, _)| v.len() >= 2) {
        packets += 1;
        zero += (v[0] == 0) as usize;
        chars += ok.len();
    }
    if (baud / DMX_BAUD - 1.0).abs() < DMX_RATE_TOL && (packets >= 2 || (packets == 1 && chars >= DMX_ONE_PACKET)) {
        framed = framed.max(zero as f64 / packets as f64);
    }
    framed
}

/// Characters read in a block: L bits each (start, data, any parity, one stop).
#[derive(Debug, Clone)]
pub struct Chars {
    pub bits: usize,
    pub values: Vec<i64>,
    pub ok: Vec<bool>,
    pub start: Vec<i64>,
    pub breaks: Vec<i64>,
}

fn rnd(x: f64) -> f64 {
    x.round_ties_even()
}

/// The bit time, not a fraction of it (`typed._unfold`).
fn unfold(runs: &[i64], mut unit: f64) -> f64 {
    loop {
        let real: Vec<f64> = runs
            .iter()
            .map(|&r| r as f64)
            .filter(|&r| r >= 0.75 * unit)
            .map(|r| r / unit)
            .filter(|&r| (r - rnd(r)).abs() < 0.25 && rnd(r) >= 1.0)
            .collect();
        if real.len() < 12 {
            return unit;
        }
        let even = real.iter().filter(|&&r| rnd(rnd(r)) as i64 % 2 == 0).count() as f64 / real.len() as f64;
        if even < 0.9 {
            return unit;
        }
        unit *= 2.0;
    }
}

/// The bit time of a stretch (`typed.async_segments.unit_of`).
fn unit_of(start: &[i64], length: &[i64], level: &[u8], idle: u8, s0: i64, s1: i64) -> Option<f64> {
    let n = start.len();
    let sel: Vec<usize> = (0..n).filter(|&i| start[i] >= s0 && start[i] < s1).collect();
    let act: Vec<i64> = sel
        .iter()
        .filter(|&&i| level[i] != idle && i != 0 && i != n - 1)
        .map(|&i| length[i])
        .collect();
    let r: Vec<i64> = if act.len() >= 12 {
        act
    } else if sel.len() > 2 {
        sel[1..sel.len() - 1].iter().map(|&i| length[i]).collect()
    } else {
        sel.iter().map(|&i| length[i]).collect()
    };
    let cands = estimate_units(&r, 12.0, 8);
    if cands.is_empty() {
        return None;
    }
    let mut rr: Vec<f64> = r.iter().filter(|&&x| x > 1).map(|&x| x as f64).collect();
    rr.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let shortest = if rr.is_empty() {
        cands[0].samples
    } else {
        quantile(&rr, 0.02)
    };
    let best = cands.iter().map(|c| c.fit).fold(f64::MIN, f64::max);
    let ok = cands
        .iter()
        .filter(|c| c.fit >= best - 0.05 && 0.7 * shortest <= c.samples && c.samples <= 1.15 * shortest)
        .map(|c| c.samples)
        .fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s))));
    Some(unfold(&r, ok.unwrap_or(cands[0].samples)))
}

/// Character lengths by the stop-bit test (`survey.char_length`): [(L, pass rate)] best first, 4 at most.
fn char_length(ch: &Channel, n: i64, idle: u8, unit: f64, s0: i64, s1: i64) -> Vec<(usize, f64)> {
    let e = ch.edges;
    let starts: Vec<f64> = e
        .iter()
        .enumerate()
        .filter(|&(k, &x)| (ch.initial ^ (((k + 1) & 1) as u8)) != idle && x >= s0 && x < s1)
        .map(|(_, &x)| x as f64)
        .collect();
    if starts.len() < 4 {
        return vec![];
    }
    // the level at each start's (k + 0.5) bits, k < 14: the positions rise with the starts, so one walk each
    let grid: Vec<Vec<u8>> = (0..14)
        .map(|k| {
            let lv = crate::kernels::levels_at_sorted(
                ch,
                starts
                    .iter()
                    .map(|&s| ((s + (k as f64 + 0.5) * unit).floor() as i64).min(n - 1)),
            );
            if idle == 0 {
                lv.into_iter().map(|x| 1 - x).collect()
            } else {
                lv
            }
        })
        .collect();
    let mut rates: Vec<(usize, f64)> = (8..14)
        .map(|l| {
            let busy: Vec<f64> = starts.iter().map(|&s| s + (l as f64 - 0.5) * unit).collect();
            let idx = chain(&starts, &busy);
            let ok = idx.iter().filter(|&&i| grid[0][i] == 0 && grid[l - 1][i] == 1).count();
            (
                l,
                if idx.is_empty() {
                    0.0
                } else {
                    ok as f64 / idx.len() as f64
                },
            )
        })
        .collect();
    rates.sort_by(|a, b| {
        let (ra, rb) = ((a.1 * 100.0).round_ties_even(), (b.1 * 100.0).round_ties_even());
        rb.partial_cmp(&ra).unwrap().then(a.0.cmp(&b.0))
    });
    rates.truncate(4);
    rates
}

fn char_starts(lv: &[u8], l: usize, i0: usize) -> Vec<usize> {
    let mut at = Vec::new();
    let mut i = i0;
    while i < lv.len() {
        if lv[i] == 0 && i + l <= lv.len() {
            at.push(i);
            i += l;
        } else {
            i += 1;
        }
    }
    at
}

fn first_start(lv: &[u8], l: usize) -> usize {
    let Some(first) = lv.iter().position(|&x| x == 0) else {
        return 0;
    };
    let mut cands = vec![first];
    for f in 1..lv.len() {
        if lv[f] == 0 && lv[f - 1] == 1 && first < f && f < first + 2 * l {
            cands.push(f);
        }
    }
    if cands.len() == 1 {
        return first;
    }
    // the first idle run of l bits or more after the first 0
    let mut end = lv.len();
    let mut i = first;
    while i < lv.len() {
        if lv[i] == 1 {
            let j = lv[i..].iter().position(|&x| x == 0).map_or(lv.len(), |p| i + p);
            if j - i >= l {
                end = j;
                break;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let end = end.min(first + START_PREFIX);
    let head = &lv[..end];
    let (mut best, mut best_ok) = (first, -1i64);
    for c in cands {
        let at = char_starts(head, l, c);
        let n_ok = at.iter().filter(|&&a| head[a] == 0 && head[a + l - 1] == 1).count() as i64;
        if n_ok > best_ok {
            best = c;
            best_ok = n_ok;
        }
    }
    best
}

/// Characters from the runs of a block, each run a whole number of bits (`typed.chars`).
#[allow(clippy::too_many_arguments)]
fn chars(start: &[i64], length: &[i64], level: &[u8], idle: u8, s0: i64, s1: i64, u: f64, l: usize) -> Chars {
    let mut lv: Vec<u8> = Vec::new();
    let mut tb: Vec<i64> = Vec::new();
    let mut breaks = Vec::new();
    for i in 0..start.len() {
        if start[i] < s0 || start[i] >= s1 {
            continue;
        }
        let units = rnd(length[i] as f64 / u).clamp(0.0, 32767.0) as usize;
        let mut v = level[i];
        if v != idle && units >= l {
            breaks.push(start[i]);
            v = idle;
        }
        let bit = if idle == 0 { 1 - v } else { v };
        let k = units.min(l + 2);
        lv.extend(std::iter::repeat_n(bit, k));
        tb.extend(std::iter::repeat_n(start[i], k));
    }
    let at = char_starts(&lv, l, first_start(&lv, l));
    let mut out = Chars {
        bits: l,
        values: vec![],
        ok: vec![],
        start: vec![],
        breaks,
    };
    for a in at {
        let fr = &lv[a..a + l];
        out.ok.push(fr[0] == 0 && fr[l - 1] == 1);
        out.values
            .push(fr[1..l - 1].iter().enumerate().map(|(b, &x)| (x as i64) << b).sum());
        out.start.push(tb[a]);
    }
    out
}

/// The bit time fitted over the well-formed characters' edges (`typed.refit_unit`).
fn refit_unit(edges: &[i64], ch: &Chars, u: f64) -> Option<f64> {
    let mut starts: Vec<i64> = ch
        .start
        .iter()
        .zip(&ch.ok)
        .filter(|&(_, &o)| o)
        .map(|(&s, _)| s)
        .collect();
    starts.sort();
    if starts.len() < 2 || edges.is_empty() {
        return None;
    }
    let l = ch.bits as f64;
    let lo = edges.partition_point(|&x| x < starts[0]);
    let e = &edges[lo..];
    let ci: Vec<usize> = e.iter().map(|&x| last_at_or_before(&starts, x) as usize).collect();
    let mut u = u;
    for _ in 0..2 {
        let m = starts.len();
        let (mut n, mut sk, mut sd, mut skd, mut skk) =
            (vec![0.0; m], vec![0.0; m], vec![0.0; m], vec![0.0; m], vec![0.0; m]);
        for (&x, &i) in e.iter().zip(&ci) {
            let d = (x - starts[i]) as f64;
            if d >= (l - 0.5) * u {
                continue;
            }
            let k = rnd(d / u);
            if (d / u - k).abs() >= 0.3 {
                continue;
            }
            n[i] += 1.0;
            sk[i] += k;
            sd[i] += d;
            skd[i] += k * d;
            skk[i] += k * k;
        }
        let (mut num, mut den) = (0.0, 0.0);
        let mut any = false;
        for i in 0..m {
            if n[i] >= 2.0 {
                any = true;
                num += skd[i] - sk[i] * sd[i] / n[i];
                den += skk[i] - sk[i] * sk[i] / n[i];
            }
        }
        if !any || den <= 0.0 {
            return None;
        }
        u = num / den;
    }
    Some(u)
}

/// ("even" | "odd", data bits) where a character's last bit is the parity of the ones before (`staged.uart_parity`).
fn parity(v: &[i64], nbits: usize) -> Option<(&'static str, usize)> {
    if nbits < 6 || v.len() < UART_PARITY_MIN {
        return None;
    }
    let d = nbits - 1;
    let last: Vec<i64> = v.iter().map(|&x| (x >> d) & 1).collect();
    let mean = last.iter().sum::<i64>() as f64 / v.len() as f64;
    if !(UART_PARITY_BOTH..=1.0 - UART_PARITY_BOTH).contains(&mean) {
        return None;
    }
    let even = v
        .iter()
        .zip(&last)
        .filter(|&(&x, &p)| (0..d).fold(0, |a, b| a ^ ((x >> b) & 1)) == p)
        .count() as f64
        / v.len() as f64;
    if even >= UART_PARITY_HOLDS {
        Some(("even", d))
    } else if 1.0 - even >= UART_PARITY_HOLDS {
        Some(("odd", d))
    } else {
        None
    }
}

fn counts(values: &[i64]) -> Vec<usize> {
    let mut c = std::collections::HashMap::new();
    for &v in values {
        *c.entry(v).or_insert(0usize) += 1;
    }
    let mut c: Vec<usize> = c.into_values().collect();
    c.sort_unstable_by(|a, b| b.cmp(a));
    c
}

/// 1 when no value is over half of them, down to 0.05 when one is all (`scoring.degenerate`).
fn degenerate(values: &[i64]) -> f64 {
    if values.len() < 4 {
        return 1.0;
    }
    let top = counts(values)[0] as f64 / values.len() as f64;
    if top <= 0.5 {
        1.0
    } else {
        (1.0 - (top - 0.5) / 0.5).max(0.05)
    }
}

/// 1 when the two commonest values are TWO_VALUES of them or less (`scoring.two_values`).
fn two_values(values: &[i64]) -> f64 {
    if values.len() < 4 {
        return 1.0;
    }
    let c = counts(values);
    let top = c.iter().take(2).sum::<usize>() as f64 / values.len() as f64;
    if top <= TWO_VALUES {
        1.0
    } else {
        (1.0 - (top - TWO_VALUES) / (1.0 - TWO_VALUES)).max(0.05)
    }
}

/// Of the changes inside the well-formed characters, the share on the bit grid (`staged._on_grid`).
fn on_grid(edges: &[i64], blocks: &[(i64, i64, f64, Chars)]) -> Option<f64> {
    let (mut num, mut den) = (0usize, 0usize);
    for (s0, s1, u, ch) in blocks {
        let mut e: Vec<i64> = edges.iter().copied().filter(|&x| x >= *s0 && x < *s1).collect();
        let kp = if *u >= 3.5 { 2i64.max((*u / 3.0) as i64) } else { 0 };
        if kp > 0 && e.len() > 2 {
            e = shortest_first(&e, kp);
        }
        let e = gated(&e, u / 4.0);
        let mut st: Vec<i64> = ch
            .start
            .iter()
            .zip(&ch.ok)
            .filter(|&(_, &o)| o)
            .map(|(&s, _)| s)
            .collect();
        st.sort();
        if st.is_empty() || e.is_empty() {
            continue;
        }
        for &x in &e {
            let i = last_at_or_before(&st, x);
            if i < 0 {
                continue;
            }
            let d = (x - st[i as usize]) as f64 / u;
            if d < ch.bits as f64 - 0.5 {
                den += 1;
                if (d - rnd(d)).abs() < 0.25 {
                    num += 1;
                }
            }
        }
    }
    if den > 0 { Some(num as f64 / den as f64) } else { None }
}

fn q(n: f64) -> f64 {
    1.0 - (-n / 8.0).exp()
}

/// A line read as a UART under an idle level: the claim's parts, or None where nothing frames.
struct Reading {
    score: f64,
    params: Map<String, Value>,
    evidence: Map<String, Value>,
    framing: f64,
    // what the layers are written from, for the reading claimed only
    unit: f64,
    n: i64,
    chars: Chars,
    bytes: Vec<i64>,
    starts: Vec<i64>,
}

/// The layers of a UART reading as JSON (blocks, chars, breaks, bytes), written straight to text.
fn layers(r: &Reading) -> Box<serde_json::value::RawValue> {
    use std::fmt::Write;
    let c = &r.chars;
    let mut s = String::with_capacity(64 + c.start.len() * 40 + r.bytes.len() * 12);
    let _ = write!(
        s,
        r#"{{"blocks":[{{"s":0,"e":{},"unit_samples":{}}}],"chars":["#,
        r.n,
        json!(r.unit)
    );
    for (i, ((&st, &x), &o)) in c.start.iter().zip(&c.values).zip(&c.ok).enumerate() {
        let _ = write!(
            s,
            r#"{}{{"s":{st},"value":{x},"ok":{o}}}"#,
            if i > 0 { "," } else { "" }
        );
    }
    s.push_str(r#"],"breaks":["#);
    for (i, b) in c.breaks.iter().enumerate() {
        let _ = write!(s, "{}{b}", if i > 0 { "," } else { "" });
    }
    s.push_str(r#"],"bytes":{"s":["#);
    for (i, st) in r.starts.iter().enumerate() {
        let _ = write!(s, "{}{st}", if i > 0 { "," } else { "" });
    }
    s.push_str(r#"],"hex":""#);
    for &x in &r.bytes {
        let _ = write!(s, "{:02x}", x & 0xFF);
    }
    s.push_str("\"}}");
    crate::claim::raw(s)
}

#[allow(clippy::too_many_arguments)]
fn read(view: &View, ch: &Channel, f: &Features, idle: u8, k: i64, clock_like: f64, sync: f64) -> Option<Reading> {
    let n = view.n_samples;
    let (start, length, level) = runs(ch, n);
    let inner = length.len().saturating_sub(2);
    let u0 = if inner < 40 {
        f.units.first()?.samples
    } else {
        unit_of(&start, &length, &level, idle, 0, n)?
    };
    if u0 < ASYNC_MIN_UNIT {
        return None;
    }
    // one block (rate switches are not read yet)
    let cands = char_length(ch, n, idle, u0, 0, n);
    if cands.is_empty() {
        return None;
    }
    let best = cands[0].1;
    let l = cands.iter().filter(|c| c.1 >= best - 0.02).map(|c| c.0).min()?;
    let mut u = u0;
    let mut chs = chars(&start, &length, &level, idle, 0, n, u, l);
    if let Some(u2) = refit_unit(ch.edges, &chs, u)
        && (u2 / u - 1.0).abs() > REFIT_MIN
    {
        let ch2 = chars(&start, &length, &level, idle, 0, n, u2, l);
        if ch2.ok.iter().filter(|&&o| o).count() >= chs.ok.iter().filter(|&&o| o).count() {
            u = u2;
            chs = ch2;
        }
    }
    let total = chs.ok.len();
    if total < 2 {
        return None;
    }
    let ok_n = chs.ok.iter().filter(|&&o| o).count();
    let mut v: Vec<i64> = chs
        .values
        .iter()
        .zip(&chs.ok)
        .filter(|&(_, &o)| o)
        .map(|(&x, _)| x)
        .collect();
    let starts: Vec<i64> = chs
        .start
        .iter()
        .zip(&chs.ok)
        .filter(|&(_, &o)| o)
        .map(|(&s, _)| s)
        .collect();
    let par = parity(&v, l - 2);
    if let Some((_, d)) = par {
        v.iter_mut().for_each(|x| *x &= (1 << d) - 1);
    }
    let framing = ok_n as f64 / total as f64;
    // edges explained: inside a well-formed character
    let mut wins: Vec<(i64, i64)> = starts.iter().map(|&s| (s, s + (l as f64 * u) as i64)).collect();
    wins.sort();
    let ws: Vec<i64> = wins.iter().map(|w| w.0).collect();
    let inside = ch
        .edges
        .iter()
        .filter(|&&x| {
            let k = last_at_or_before(&ws, x);
            k >= 0 && x < wins[k as usize].1
        })
        .count();
    let coverage = inside as f64 / ch.edges.len().max(1) as f64;
    let resolvable = (u - 2.0).clamp(0.0, 1.0);
    let blocks = vec![(0i64, n, u, chs)];
    let grid = on_grid(ch.edges, &blocks);
    let gridded = grid.map_or(1.0, |g| ((g - GRID_LOW) / GRID_SPAN).clamp(0.0, 1.0));
    let v8: Vec<i64> = v.iter().map(|&x| x & 0xFF).collect();
    let tv = two_values(&v8);
    let nb = v
        .iter()
        .max()
        .map_or(1, |&m| (64 - (m as u64).leading_zeros()).max(1) as usize);
    let fixed = if v.is_empty() {
        0.0
    } else {
        (0..nb)
            .filter(|&b| {
                let m = v.iter().map(|&x| (x >> b) & 1).sum::<i64>() as f64 / v.len() as f64;
                m.max(1.0 - m) >= 0.95
            })
            .count() as f64
            / nb as f64
    };
    let paced = l % 3 == 0 && fixed >= UART_FIXED_BITS;
    let framed_raw = framed_by_breaks(&blocks[0].3, view.rate / u);
    let framed = ((framed_raw - UART_FRAMED) / (1.0 - UART_FRAMED)).max(0.0);
    let clock_off = 0.5 * ((clock_like - UART_CLOCK_LIKE) / (0.7 - UART_CLOCK_LIKE)).clamp(0.0, 1.0);
    let sync_off = ((sync - UART_SYNC) / 0.3).clamp(0.0, 1.0);
    let score = framing
        * coverage
        * q(ok_n as f64)
        * degenerate(&v8)
        * tv
        * (1.0 - clock_off)
        * (1.0 - sync_off)
        * resolvable
        * gridded
        * (1.0 - framed)
        * if paced { UART_PACED } else { 1.0 };
    let baud = view.rate / u;
    let mut params = Map::new();
    params.insert("baud".into(), json!(baud));
    params.insert("idle".into(), json!(idle));
    params.insert("L".into(), json!(l));
    params.insert("blocks".into(), json!(1));
    if let Some((p, d)) = par {
        params.insert("parity".into(), json!(p));
        params.insert("data_bits".into(), json!(d));
    }
    params.insert("deglitch".into(), json!(k));
    let mut evidence = Map::new();
    for (k, x) in [
        ("framing_rate", framing),
        ("coverage", coverage),
        ("clock_like", clock_like),
        ("samples_per_bit", u),
        ("two_values", tv),
        ("fixed_bits", fixed),
        ("framed_by_breaks", framed_raw),
        ("sync_with_clock", sync),
    ] {
        evidence.insert(k.into(), json!(x));
    }
    evidence.insert("chars".into(), json!(ok_n));
    evidence.insert("on_grid".into(), grid.map_or(Value::Null, |g| json!(g)));
    let (_, _, _, chs) = blocks.into_iter().next().expect("one block");
    Some(Reading {
        score,
        params,
        evidence,
        framing,
        unit: u,
        n,
        chars: chs,
        bytes: v,
        starts,
    })
}

/// The line rests at `level` between characters now and then (`staged._rests_at`).
fn rests_at(ch: &Channel, n: i64, level: u8, u: f64) -> bool {
    let (_, length, lv) = runs(ch, n);
    if length.len() < 3 {
        return false;
    }
    (1..length.len() - 1)
        .filter(|&i| lv[i] == level && length[i] as f64 >= UART_REST_BITS * u)
        .count()
        >= UART_RESTS
}

pub const BOUNCE_TICKS: i64 = 4;
pub const BOUNCE_MERGE_WIDE: i64 = 2 * BOUNCE_TICKS;
pub const BOUNCE_PAIRED: f64 = 0.5;
pub const UART_VIEW_CLOCK: f64 = 0.4;
pub const UART_RAW_CLOCK: f64 = 0.9;
pub const EQUIVALENT_BAUD: f64 = 0.03;

/// The line as it is, and views of it with spikes or bounces taken out (`staged.single_views`): (edges, k).
fn views(ch: &Channel, f: &Features, n: i64) -> Vec<(Vec<i64>, i64)> {
    let mut out = vec![(ch.edges.to_vec(), 0)];
    let (_, length, _) = runs(ch, n);
    let inner: &[i64] = if length.len() >= 2 {
        &length[1..length.len() - 1]
    } else {
        &[]
    };
    let mut ks: Vec<i64> = vec![];
    if let Some(u) = f.units.first()
        && inner.len() >= 16
        && u.samples >= 4.0
    {
        let spikes = inner.iter().filter(|&&r| r <= 1).count() as f64 / inner.len() as f64;
        if 0.0 < spikes && spikes < 0.1 {
            ks.push((2.0f64).max((0.3 * u.samples).min(3.0)) as i64);
        }
    }
    let kb = BOUNCE_TICKS;
    let mut bounced = None;
    if inner.len() >= 16 && !ks.contains(&kb) && inner.iter().any(|&r| r < kb) {
        let e = deglitch(ch.edges, kb);
        let mut left: Vec<f64> = e.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
        left.sort_by(|a, b| a.partial_cmp(b).unwrap());
        if left.len() >= 16 && quantile(&left, 0.1) >= 2.0 * kb as f64 {
            ks = vec![kb];
            let short: Vec<bool> = inner.iter().map(|&r| r < kb).collect();
            let pairs = short.windows(2).filter(|w| w[0] && w[1]).count() as f64;
            let n_short = short.iter().filter(|&&x| x).count() as f64;
            if n_short > 0.0 && pairs * 2.0 >= BOUNCE_PAIRED * n_short {
                let d = debounce(ch.edges, BOUNCE_MERGE_WIDE);
                let dl: Vec<f64> = d.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
                let med = median(&dl);
                let steady = if dl.is_empty() {
                    1.0
                } else {
                    dl.iter().filter(|&&x| (x / med - 1.0).abs() <= 0.15).count() as f64 / dl.len() as f64
                };
                let mut sl = dl.clone();
                sl.sort_by(|a, b| a.partial_cmp(b).unwrap());
                if dl.len() >= 16 && quantile(&sl, 0.1) >= 3.0 * kb as f64 && steady < 0.7 {
                    bounced = Some(d);
                }
            }
        }
    }
    ks.sort();
    for k in ks {
        let e = match &bounced {
            Some(d) if k == kb => d.clone(),
            _ => deglitch(ch.edges, k),
        };
        out.push((e, k));
    }
    out
}

/// The UART claims of a part. Each line is read on each of its views under its idle level (and the other, where
/// few characters frame and the line rests there); the best reading is claimed where its score is past the claim's
/// cost, its margin over the best reading that is no same one (another idle level, or a rate 3 % off or more).
pub fn claims(view: &View) -> Vec<Claim> {
    let mut out = Vec::new();
    let n = view.n_samples;
    let feats: Vec<Features> = view.channels.iter().map(|c| features(c, n)).collect();
    // the clocks: lines with a rising period of 4 samples or more, by how clock-like they are
    let clocks: Vec<(usize, f64)> = view
        .channels
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let r = crate::sync::edges_to(c, 1);
            if r.len() < 4 {
                return None;
            }
            let iv: Vec<f64> = r.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            let score = feats[i].clock.max(feats[i].clock_local);
            (crate::kernels::median(&iv) >= SYNC_MIN_PERIOD && score > 0.0).then_some((i, score))
        })
        .collect();
    for (ci, ch) in view.channels.iter().enumerate() {
        if ch.edges.len() < 8 {
            continue;
        }
        let f = feats[ci].clone();
        let raw_clock = f.clock.max(f.clock_local);
        // a bus's clock (another line keeps to it as its data) is no UART line
        if raw_clock >= CLOCKS_A_BUS
            && view
                .channels
                .iter()
                .enumerate()
                .any(|(j, o)| j != ci && crate::sync::keeping(o.edges, ch) >= CLOCKS_A_BUS_ASYNC)
        {
            continue;
        }
        let sync = clocks
            .iter()
            .filter(|&&(k, _)| k != ci)
            .map(|&(k, s)| crate::sync::keeping(ch.edges, &view.channels[k]) * s)
            .fold(0.0, f64::max);
        let mut readings: Vec<Reading> = vec![];
        for (edges, k) in views(ch, &f, n) {
            let vch = Channel::new(ch.name, ch.initial, &edges);
            let mut clock_like = raw_clock;
            if edges.len() != f.n_edges && UART_VIEW_CLOCK < clock_like && clock_like < UART_RAW_CLOCK {
                let fv = features(&vch, n);
                clock_like = clock_like.min(fv.clock.max(fv.clock_local));
            }
            let Some(r) = read(view, &vch, &f, f.idle, k, clock_like, sync) else {
                continue;
            };
            if r.framing < UART_OTHER_IDLE {
                let u = r.params["baud"].as_f64().map_or(0.0, |b| view.rate / b);
                if rests_at(&vch, n, 1 - f.idle, u)
                    && let Some(r2) = read(view, &vch, &f, 1 - f.idle, k, clock_like, sync)
                {
                    readings.push(r2);
                }
            }
            readings.push(r);
        }
        let Some(best) = readings.iter().max_by(|a, b| a.score.partial_cmp(&b.score).unwrap()) else {
            continue;
        };
        if best.score <= CLAIM_COST {
            continue;
        }
        let same = |a: &Reading, b: &Reading| {
            a.params["idle"] == b.params["idle"]
                && (a.params["baud"].as_f64().unwrap() / b.params["baud"].as_f64().unwrap() - 1.0).abs()
                    < EQUIVALENT_BAUD
        };
        let rival = readings
            .iter()
            .filter(|r| !same(r, best) && r.score > CLAIM_COST)
            .map(|r| r.score)
            .fold(None, |m: Option<f64>, s| Some(m.map_or(s, |m| m.max(s))));
        let margin = best.score - rival.unwrap_or(CLAIM_COST);
        let confirmed = (best.score >= 0.85 && margin >= 0.15)
            || (rival.is_some() && ((best.score >= 0.80 && margin >= 0.25) || (best.score >= 0.60 && margin >= 0.50)));
        let verdict = if confirmed {
            "confirmed"
        } else if best.score >= 0.6 && margin >= 0.05 {
            "likely"
        } else {
            "ambiguous"
        };
        out.push(Claim {
            protocol: "uart".into(),
            roles: [("data".to_string(), ch.name.to_string())].into(),
            verdict: verdict.into(),
            score: (best.score * 1e4).round() / 1e4,
            margin: (margin * 1e4).round() / 1e4,
            params: best.params.clone(),
            evidence: best.evidence.clone(),
            layers: layers(best),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A UART line (8N1, idle high) sending `data` at `bit` ticks a bit, starting at `t0`, `gap` bits between.
    fn uart(data: &[u8], bit: i64, t0: i64, gap: i64) -> (Vec<i64>, i64) {
        let (mut e, mut lv, mut t) = (vec![], 1u8, t0);
        for &b in data {
            let bits: Vec<u8> = std::iter::once(0)
                .chain((0..8).map(|k| (b >> k) & 1))
                .chain([1])
                .collect();
            for x in bits {
                if x != lv {
                    e.push(t);
                    lv = x;
                }
                t += bit;
            }
            t += gap * bit;
        }
        (e, t + 20 * bit)
    }

    #[test]
    fn reads_text() {
        let text = b"Hello, WireSkein! The quick brown fox jumps over the lazy dog.\r\n".repeat(3);
        let (e, n) = uart(&text, 17, 100, 1);
        let v = View::new(2e6, n, vec![Channel::new("TX", 1, &e)]).unwrap();
        let cs = claims(&v);
        assert_eq!(cs.len(), 1, "{cs:?}");
        let c = &cs[0];
        assert_eq!(c.verdict, "confirmed", "{:?}", c.evidence);
        let hex: String = text.iter().map(|b| format!("{b:02x}")).collect();
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["bytes"]["hex"], json!(hex));
        let baud = c.params["baud"].as_f64().unwrap();
        assert!((baud / (2e6 / 17.0) - 1.0).abs() < 0.01, "{baud}");
    }

    #[test]
    fn a_clock_is_no_uart() {
        let e: Vec<i64> = (1..2000).map(|i| i * 7).collect();
        let v = View::new(1e6, 15000, vec![Channel::new("C", 0, &e)]).unwrap();
        assert!(claims(&v).iter().all(|c| c.verdict != "confirmed"));
    }
}
