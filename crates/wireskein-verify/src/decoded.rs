//! The checks that read a bus as a device on it does (`verify.check_i2c`, `check_spi`, `check_protocol`): I2C on
//! the lines as an I2C input sees them (`timing.i2c_input`: its spike filter, its hysteresis, a clock clipped
//! shorter than a sample restored) read START to STOP in 9-bit words, as Python's engine reads them for the check;
//! SPI read at the sampling edges inside CS (`timing.spi_read`), or without CS from the core's SPI reading.

use crate::cap::{Cap, Ch};
use crate::checks::{Out, V, any, out};
use crate::j::{J, py_eq, py_str};
use crate::jd;
use crate::pyfmt::round;
use crate::report::push_notice;
use crate::timing::{self, median, median_i};

pub fn run(kind: &str, cap: &Cap, x: &J) -> Out {
    match kind {
        "i2c" => check_i2c(cap, x),
        "spi" => check_spi(cap, x),
        "protocol" => check_protocol(x),
        _ => out(V::Ng, J::dict(), format!("no check named {}", x.get("kind").repr())),
    }
}

/// A protocol of one's own (`verify.check_protocol`): declared in a definitions directory, which this version does
/// not read. With none named (WIRESKEIN_DEFINITIONS unset) it answers as Python's does without them; a protocol
/// declared in WireSkein itself, read on the wire, is left unchecked.
fn check_protocol(x: &J) -> Out {
    let name = x.get("name").repr();
    if std::env::var("WIRESKEIN_DEFINITIONS").is_ok_and(|v| !v.is_empty()) {
        return out(
            V::Unchecked,
            J::dict(),
            format!(
                "protocol {name}: definitions directories (WIRESKEIN_DEFINITIONS) are not read by the Rust verify yet"
            ),
        );
    }
    if !x.get("bus").is_null() {
        return out(
            V::Ng,
            J::dict(),
            format!(
                "no protocol named {name} (declare it in a definitions directory: wireskein.plugins.add_definitions)"
            ),
        );
    }
    let n = x.get("name").s().unwrap_or("");
    if ["can", "i2c", "mdio", "ps2", "rvswd", "spi", "swd"].contains(&n) {
        return out(
            V::Unchecked,
            J::dict(),
            format!("protocol {name}: WireSkein's declared protocols are not read on the wire by the Rust verify yet"),
        );
    }
    out(
        V::Ng,
        J::dict(),
        format!("nothing decoded as {n} on these pins (is its definition added?)"),
    )
}

fn s<'a>(x: &'a J, k: &str) -> &'a str {
    x.get(k).s().unwrap_or("")
}

// ---------------- the I2C input (timing.py) ----------------

pub const I2C_SPIKE_S: f64 = 50e-9;
pub const I2C_RISE_S: f64 = 1000e-9;
const CLIPPED_LOW: f64 = 0.7;
const CLIPPED_LOW_MAX: f64 = 4.0;
const RESTORE_LOW_MAX: f64 = 2.0 * CLIPPED_LOW_MAX; // a longer SCL low is a wait: no clock restored in it
const SCL_WAIT: f64 = 4.0; // an SCL low longer than this many times the frame's median low is a wait (`typed.SCL_WAIT`)
const I2C_INPUT_PASSES: usize = 3;

/// numpy's percentile of a short row (its `_lerp`).
fn percentile_row(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let m = s.len();
    let pos = q / 100.0 * (m - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = (lo + 1).min(m - 1);
    let t = pos - lo as f64;
    let (a, b) = (s[lo], s[hi]);
    let d = b - a;
    if t >= 0.5 { b - d * (1.0 - t) } else { a + d * t }
}

/// The clock's period around each tick of `at` (`kernels.local_period`, w 8, q 25).
fn local_period(ticks: &[i64], at: &[i64]) -> Vec<f64> {
    if ticks.len() < 2 || at.is_empty() {
        return vec![0.0; at.len()];
    }
    let n = ticks.len() - 1;
    let m = 8.min(n);
    let iv = |j0: usize| -> f64 {
        let row: Vec<f64> = (j0..j0 + m).map(|k| (ticks[k + 1] - ticks[k]) as f64).collect();
        percentile_row(&row, 25.0)
    };
    let mut memo: std::collections::HashMap<isize, f64> = std::collections::HashMap::new();
    at.iter()
        .map(|&t| {
            let j = ticks.partition_point(|&x| x <= t) as isize - 1;
            *memo.entry(j).or_insert_with(|| {
                let a = (j - m as isize).clamp(0, (n - m) as isize) as usize;
                let b = (j + 1).clamp(0, (n - m) as isize) as usize;
                iv(a).min(iv(b))
            })
        })
        .collect()
}

/// For each i, the upper quartile of d[i - 4 .. i + 4] (`timing.upper_quartiles_around`).
fn upper_quartiles_around(d: &[f64]) -> Vec<f64> {
    (0..d.len())
        .map(|i| {
            let w = &d[i.saturating_sub(4)..(i + 5).min(d.len())];
            if w.len() == 9 {
                let mut s = w.to_vec();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                s[6]
            } else {
                let mut s = w.to_vec();
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                wireskein_core::kernels::quantile(&s, 0.75)
            }
        })
        .collect()
}

/// How far one edge's crossings may spread, per edge of `at` (`timing.i2c_burst_ticks`).
fn i2c_burst_ticks(cap: &Cap, scl: &str, freq: Option<f64>, at: &[i64]) -> Vec<i64> {
    let rate = cap.rate;
    let tr = I2C_RISE_S * rate;
    let zero = vec![0; at.len()];
    if tr < 5.0 {
        return zero;
    }
    if let Some(f) = freq.filter(|&f| f != 0.0) {
        return vec![tr.min(rate / (2.0 * f) / 3.0) as i64; at.len()];
    }
    let Some(ch) = cap.ch(scl) else { return zero };
    let rises = ch.rises();
    if rises.len() < 3 {
        return zero;
    }
    let d: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let m = upper_quartiles_around(&d);
    let keep: Vec<i64> = if d.iter().zip(&m).all(|(a, b)| *a >= b / 4.0) {
        rises.clone()
    } else {
        let mut k = vec![rises[0]];
        for (r, mi) in rises[1..].iter().zip(&m) {
            if (r - k[k.len() - 1]) as f64 >= mi / 4.0 {
                k.push(*r);
            }
        }
        k
    };
    if keep.len() < 3 {
        return zero;
    }
    local_period(&keep, at)
        .iter()
        .map(|p| (p / 6.0).min(tr) as i64)
        .collect()
}

/// Runs of changes each within w of the one before, an odd number of 3 or more: (first, last) (`timing.edge_bursts`).
fn edge_bursts(e: &[i64], w: &[i64]) -> Vec<(usize, usize)> {
    if e.len() < 3 {
        return vec![];
    }
    let close: Vec<bool> = (1..e.len()).map(|k| e[k] - e[k - 1] <= w[k]).collect();
    let mut out = vec![];
    let mut k = 0;
    while k < close.len() {
        if close[k] {
            let i = k;
            while k < close.len() && close[k] {
                k += 1;
            }
            if (k - i) % 2 == 0 {
                out.push((i, k));
            }
        } else {
            k += 1;
        }
    }
    out
}

/// SCL's highs shorter than k that are clocks, not spikes: per edge, whether it may cancel (`timing.clipped_clocks`).
fn clipped_clocks(e: &[i64], initial: u8, k: i64) -> Vec<bool> {
    let mut allow = vec![true; e.len()];
    if e.len() < 6 {
        return allow;
    }
    let lv: Vec<u8> = (0..e.len()).map(|i| initial ^ (((i + 1) & 1) as u8)).collect();
    let d: Vec<i64> = e.windows(2).map(|w| w[1] - w[0]).collect();
    let lows: Vec<f64> = (0..d.len())
        .filter(|&i| lv[i] == 0 && d[i] >= k)
        .map(|i| d[i] as f64)
        .collect();
    if lows.len() < 4 {
        return allow;
    }
    let usual = median(&lows);
    for i in 1..e.len().saturating_sub(2) {
        if lv[i] == 1 && d[i] < k {
            let (a, b) = (d[i - 1] as f64, d[i + 1] as f64);
            if a.min(b) >= CLIPPED_LOW * usual && a.max(b) <= CLIPPED_LOW_MAX * usual {
                allow[i + 1] = false;
            }
        }
    }
    allow
}

/// A clock clipped shorter than a sample restored where one reading is left (`timing.i2c_restore`).
fn i2c_restore(cap: &Cap, scl: &str, sda: &str) -> (Option<Cap>, Vec<i64>) {
    let (Some(s), Some(d)) = (cap.ch(scl), cap.ch(sda)) else {
        return (None, vec![]);
    };
    let (e, de) = (&s.edges, &d.edges);
    if e.len() < 10 || de.len() < 2 || s.step != 1 {
        return (None, vec![]);
    }
    let lv: Vec<u8> = (0..e.len()).map(|i| s.after(i)).collect();
    let gaps: Vec<i64> = e.windows(2).map(|w| w[1] - w[0]).collect();
    let lows: Vec<f64> = (0..gaps.len())
        .filter(|&i| lv[i] == 0)
        .map(|i| gaps[i] as f64)
        .collect();
    let usual = median(&lows);
    let starts: Vec<i64> = de
        .iter()
        .copied()
        .filter(|&t| s.level_at(t) == 1 && d.level_at(t) == 0)
        .collect();
    let stops: Vec<i64> = de
        .iter()
        .copied()
        .filter(|&t| s.level_at(t) == 1 && d.level_at(t) == 1)
        .collect();
    let mut rise_cum = vec![0usize];
    for &l in &lv {
        rise_cum.push(rise_cum[rise_cum.len() - 1] + (l == 1) as usize);
    }
    let long_low: Vec<bool> = (0..gaps.len())
        .map(|i| lv[i] == 0 && gaps[i] as f64 > 1.5 * usual)
        .collect();
    let mut long_cum = vec![0usize];
    for &l in &long_low {
        long_cum.push(long_cum[long_cum.len() - 1] + l as usize);
    }
    let where_: Vec<usize> = (0..long_low.len()).filter(|&i| long_low[i]).collect();
    let mut add = vec![];
    for &st in &starts {
        let ns = stops.partition_point(|&x| x <= st);
        let nt = starts.partition_point(|&x| x <= st);
        let end = stops
            .get(ns)
            .copied()
            .unwrap_or(i64::MAX)
            .min(starts.get(nt).copied().unwrap_or(i64::MAX));
        if end == i64::MAX {
            continue;
        }
        let ia = e.partition_point(|&x| x <= st);
        let ib = e.partition_point(|&x| x < end);
        let rises = rise_cum[ib] - rise_cum[ia.min(ib)];
        let hi = ib.min(e.len() - 1);
        let longs = if ia < hi {
            long_cum[hi] - long_cum[ia.min(hi)]
        } else {
            0
        };
        if !(rises >= 9 && rises.is_multiple_of(9) && longs == 1) {
            continue;
        }
        let i = where_[where_.partition_point(|&w| w < ia)];
        if gaps[i] as f64 > RESTORE_LOW_MAX * usual {
            continue; // (a wait's: a clock lost in it may have been anywhere, i2c_transactions leaves it unread)
        }
        let at = (e[i] as f64 + 0.9 * usual) as i64;
        let lo_t = e[i] as f64 + 0.6 * usual;
        if de.partition_point(|&x| x <= at) == de.partition_point(|&x| (x as f64) <= lo_t) {
            add.push(at);
        }
    }
    if add.is_empty() {
        return (None, vec![]);
    }
    let mut ne = e.clone();
    ne.extend(&add);
    ne.extend(add.iter().map(|a| a + 1));
    ne.sort_unstable();
    let mut out = cap.clone();
    for c in out.channels.iter_mut() {
        if c.name == scl {
            c.edges = ne.clone();
        }
    }
    out.meta.set("i2c_restored", J::from(add.clone()));
    (Some(out), add)
}

/// The two lines as an I2C input sees them (`timing.i2c_input`): (the capture, whether anything changed).
fn i2c_input(cap: &Cap, scl: &str, sda: &str, spike_s: f64, freq: Option<f64>) -> (Cap, bool) {
    let k = (spike_s * cap.rate - 1e-9).ceil() as i64;
    let mut changed = false;
    let mut out = cap.clone();
    for c in out.channels.iter_mut() {
        if (c.name == scl || c.name == sda) && c.step == 1 {
            let mut e = c.edges.clone();
            if k >= 2 {
                let keep = if c.name == scl {
                    Some(clipped_clocks(&e, c.initial, k))
                } else {
                    None
                };
                e = wireskein_core::kernels::deglitch_allow(&e, k, keep.as_deref());
            }
            let w = i2c_burst_ticks(cap, scl, freq, &e);
            let b = edge_bursts(&e, &w);
            if !b.is_empty() {
                let mut keep = vec![true; e.len()];
                for (i, j) in b {
                    keep[i..j].iter_mut().for_each(|v| *v = false);
                }
                e = e.iter().zip(&keep).filter(|(_, k)| **k).map(|(v, _)| *v).collect();
            }
            if e.len() != c.edges.len() {
                changed = true;
                c.edges = e;
            }
        }
    }
    let (restored, add) = i2c_restore(&out, scl, sda);
    let any = changed || !add.is_empty();
    (restored.unwrap_or(out), any)
}

// ---------------- I2C read as the engine does for the check ----------------

/// Pulses shorter than k out where the clock is slow enough that no bit is that short (`kernels.deglitch_slow`).
fn deglitch_slow(e: &[i64], k: i64, clock: &[i64]) -> Vec<i64> {
    if e.len() < 2 {
        return e.to_vec();
    }
    let mut allow = vec![false; e.len()];
    let near: Vec<usize> = (1..e.len()).filter(|&i| e[i] - e[i - 1] < k).collect();
    let at: Vec<i64> = near.iter().map(|&i| e[i]).collect();
    let lp = local_period(clock, &at);
    for (q, &i) in near.iter().enumerate() {
        allow[i] = lp[q] >= 3.0 * k as f64;
    }
    wireskein_core::kernels::deglitch_allow(e, k, Some(&allow))
}

/// SDA's bit at each rise of SCL (`typed.sync_bits`, edge "rise").
fn sync_bits(scl: &Ch, sda: &Ch) -> (Vec<i64>, Vec<u8>) {
    let t = scl.rises();
    let t_other = scl.falls();
    if t.len() > 1 && sda.step == 1 {
        let e = deglitch_slow(&sda.edges, 3, &t);
        let in_sorted = |x: i64, table: &[i64]| table.binary_search(&x).is_ok();
        let at = e.iter().filter(|&&v| in_sorted(v, &t)).count();
        let inside: Vec<i64> = e
            .iter()
            .copied()
            .filter(|&v| v >= t[0] && v <= t[t.len() - 1])
            .collect();
        let share = if inside.is_empty() {
            0.0
        } else {
            at as f64 / inside.len() as f64
        };
        let other = if inside.is_empty() {
            0.0
        } else {
            inside.iter().filter(|&&v| in_sorted(v, &t_other)).count() as f64 / inside.len() as f64
        };
        let left = share >= 0.1 && other <= share / 3.0;
        let bits = t
            .iter()
            .map(|&x| {
                let k = if left {
                    e.partition_point(|&v| v < x)
                } else {
                    e.partition_point(|&v| v <= x)
                };
                (sda.initial as usize + k) as u8 & 1
            })
            .collect();
        return (t, bits);
    }
    let bits = t.iter().map(|&x| sda.level_at(x)).collect();
    (t, bits)
}

/// The first SCL wait among a frame's sampling rises `t` (`typed._scl_waits`: a low longer than SCL_WAIT times the
/// frame's median low, strictly inside its rises): (the word it falls in or before, its start), or None.
fn lost_in_wait(c: &Ch, t: &[i64]) -> Option<(usize, i64)> {
    if t.len() < 2 {
        return None;
    }
    let e = &c.edges;
    let (a, b) = (
        e.partition_point(|&x| x <= t[0]),
        e.partition_point(|&x| x < t[t.len() - 1]),
    );
    let lows: Vec<(i64, i64)> = (a..b)
        .filter(|&q| c.after(q) == 0)
        .map(|q| (e[q], e[(q + 1).min(e.len() - 1)] - e[q]))
        .collect();
    if lows.is_empty() {
        return None;
    }
    let med = median(&lows.iter().map(|&(_, n)| n as f64).collect::<Vec<_>>());
    let &(s, _) = lows.iter().find(|&&(_, n)| n as f64 > SCL_WAIT * med)?;
    Some((t.partition_point(|&x| x < s) / 9, s))
}

/// The transactions on the lines (`typed.frames_startstop`, `words`, `i2c_from_words`), and the ticks restored.
fn i2c_transactions(view: &Cap, scl: &str, sda: &str) -> Vec<J> {
    let (Some(c), Some(d)) = (view.ch(scl), view.ch(sda)) else {
        return vec![];
    };
    let (t, bits) = sync_bits(c, d);
    let de = &d.edges;
    // the conditions: SDA changing while SCL is high
    let mut ev: Vec<(bool, i64)> = vec![];
    for (i, &x) in de.iter().enumerate() {
        if c.level_at(x) == 1 {
            ev.push((d.after(i) == 0, x));
        }
    }
    let pos: Vec<usize> = ev.iter().map(|&(_, x)| t.partition_point(|&v| v < x)).collect();
    let names: Vec<&str> = (0..ev.len())
        .map(|k| {
            if ev[k].0 {
                if k > 0 && ev[k - 1].0 { "Sr" } else { "S" }
            } else {
                "P"
            }
        })
        .collect();
    let mut frames: Vec<(usize, usize, &str, Option<&str>)> = vec![];
    for k in 1..ev.len() {
        if ev[k - 1].0 {
            frames.push((pos[k - 1], pos[k], names[k - 1], Some(if ev[k].0 { "Sr" } else { "P" })));
        }
    }
    let open_tail = !ev.is_empty() && ev[ev.len() - 1].0 && pos[pos.len() - 1] < t.len();
    if open_tail {
        frames.push((pos[pos.len() - 1], t.len(), names[names.len() - 1], None));
    }
    let restored: Vec<i64> = view
        .meta
        .get("i2c_restored")
        .list()
        .iter()
        .filter_map(|v| v.i())
        .collect();
    let mut out = vec![];
    let nf = frames.len();
    for (i, &(a, b, sc, ec)) in frames.iter().enumerate() {
        let last_open = open_tail && i == nf - 1;
        let n = (b.saturating_sub(a)) / 9;
        let words: Vec<u16> = (0..n)
            .map(|w| (0..9).fold(0u16, |acc, k| (acc << 1) | bits[a + w * 9 + k] as u16))
            .collect();
        let mut tx;
        if words.is_empty() {
            if !last_open {
                continue;
            }
            tx = jd! {"addr" => J::Null, "rw" => J::Null, "addr_ack" => false, "bytes" => J::List(vec![]),
            "acks" => J::List(vec![]), "start" => t[a], "end" => t[b - 1], "complete" => false,
            "pending_bits" => bits[a..b].iter().map(|&v| v as i64).collect::<Vec<_>>()};
        } else {
            let data: Vec<i64> = words.iter().map(|&v| (v >> 1) as i64).collect();
            let acks: Vec<bool> = words.iter().map(|&v| v & 1 == 0).collect();
            tx = jd! {"addr" => data[0] >> 1, "rw" => if data[0] & 1 == 1 { "read" } else { "write" },
            "addr_ack" => acks[0], "bytes" => data[1..].to_vec(), "acks" => acks[1..].to_vec(),
            "start" => t[a], "end" => t[a.max(b - 1)]};
        }
        tx.set("start_cond", sc);
        tx.set("end_cond", ec);
        if last_open && !words.is_empty() {
            let left = b - a - n * 9;
            tx.set("complete", false);
            tx.set(
                "pending_bits",
                bits[b - left..b].iter().map(|&v| v as i64).collect::<Vec<_>>(),
            );
        } else if !last_open
            && (b - a).is_multiple_of(9)
            && let Some((k, s)) = lost_in_wait(c, &t[a..b])
        {
            // a clock short with SCL held low in it (`typed._lost_in_wait`): the words from the wait on not read
            tx.set("complete", false);
            tx.set(
                "pending_bits",
                bits[a + 9 * k..b].iter().map(|&v| v as i64).collect::<Vec<_>>(),
            );
            tx.set("clock_lost", s);
            if k == 0 {
                for (key, v) in [
                    ("addr", J::Null),
                    ("rw", J::Null),
                    ("addr_ack", J::Bool(false)),
                    ("bytes", J::List(vec![])),
                    ("acks", J::List(vec![])),
                ] {
                    tx.set(key, v);
                }
            } else {
                let keep = |v: &J| J::List(v.list()[..k - 1].to_vec());
                let (bs, ac) = (keep(tx.get("bytes")), keep(tx.get("acks")));
                tx.set("bytes", bs);
                tx.set("acks", ac);
            }
        }
        let (st, en) = (tx.get("start").i().unwrap(), tx.get("end").i().unwrap());
        let r: Vec<i64> = restored.iter().copied().filter(|&at| st <= at && at <= en).collect();
        if !r.is_empty() {
            tx.set("restored", r);
        }
        out.push(tx);
    }
    out
}

fn burst_hz(ch: &Ch, rate: f64) -> Option<f64> {
    let rises = ch.rises();
    if rises.len() < 3 {
        return None;
    }
    let d: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let med = median(&d);
    let d: Vec<f64> = d.into_iter().filter(|&v| v < 1.5 * med).collect();
    if d.is_empty() { None } else { Some(rate / median(&d)) }
}

fn i2c_tx(t: &J) -> J {
    let addr = t.get("addr");
    let mut o = jd! {"addr" => addr, "rw" => t.get("rw"), "bytes" => t.get("bytes"),
    "ack" => if addr.is_null() { J::Null } else { J::Bool(t.get("addr_ack").truthy()) },
    "acks" => t.get("acks"),
    "complete" => if t.has("complete") { t.get("complete").clone() } else { J::Bool(true) },
    "start" => t.get("start_cond"), "end" => t.get("end_cond")};
    if !o.get("complete").truthy() {
        o.set("pending_bits", t.get("pending_bits").clone());
    }
    if t.has("clock_lost") {
        o.set("clock_lost", t.get("clock_lost").clone());
    }
    o
}

fn i2c_want(t: &J) -> J {
    let addr = t.get("addr");
    let mut o = jd! {"addr" => addr, "rw" => t.get("rw"), "bytes" => t.get("bytes").list().to_vec(),
    "ack" => if addr.is_null() { J::Null } else { J::Bool(!t.has("ack") || t.get("ack").truthy()) },
    "complete" => !t.has("complete") || t.get("complete").truthy()};
    if t.has("pending_bits") {
        o.set("pending_bits", t.get("pending_bits").clone());
    }
    let n = o.get("bytes").list().len();
    let acks: Option<Vec<bool>> = if t.has("acks") {
        Some(t.get("acks").list().iter().map(|a| a.truthy()).collect())
    } else if t.has("nack_at") {
        let at: Vec<i64> = match t.get("nack_at") {
            J::List(v) => v.iter().filter_map(|a| a.i()).collect(),
            v => v.i().into_iter().collect(),
        };
        Some((0..n).map(|i| !at.contains(&(i as i64))).collect())
    } else if !o.get("complete").truthy() || addr.is_null() {
        None
    } else {
        let read = o.get("rw").s() == Some("read");
        Some((0..n).map(|i| !(read && i == n - 1)).collect())
    };
    if let Some(a) = acks {
        o.set("acks", a);
    }
    for k in ["start", "end"] {
        if t.has(k) {
            o.set(k, t.get(k).clone());
        }
    }
    o
}

fn i2c_bytes(t: &J) -> String {
    let acks = t.get("acks");
    t.get("bytes")
        .list()
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let nack = !acks.is_null() && i < acks.list().len() && !acks.list()[i].truthy();
            format!("{:02x}{}", b.i().unwrap_or(0), if nack { ":NACK" } else { "" })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn i2c_fmt(t: &J) -> String {
    let pend = || {
        if t.has("pending_bits") {
            t.get("pending_bits").repr()
        } else {
            "[]".into()
        }
    };
    if t.get("addr").is_null() {
        return format!("(cut in the address, bits {})", pend());
    }
    let mut s = String::new();
    if t.get("start").truthy() {
        s += &format!("{} ", py_str(t.get("start")));
    }
    s += &format!(
        "0x{:02x} {} [{}]",
        t.get("addr").i().unwrap_or(0),
        py_str(t.get("rw")),
        i2c_bytes(t)
    );
    if !t.get("ack").truthy() {
        s += " NACK";
    }
    if t.get("end").truthy() {
        s += &format!(" {}", py_str(t.get("end")));
    }
    if !t.get("complete").truthy() {
        if t.has("clock_lost") {
            s += &format!(
                " not read from a clock lost in the SCL wait at tick {} (bits {})",
                t.get("clock_lost").repr(),
                pend()
            );
        } else {
            s += &format!(" incomplete (pending bits {})", pend());
        }
    }
    s
}

/// What of an expected transaction a reading cut at a clock lost in an SCL wait can be compared with
/// (`verify._i2c_read_part`): the words before the cut, how it began and ended.
fn i2c_read_part(w: &J, g: &J) -> J {
    let mut out = J::dict();
    if g.get("addr").is_null() {
        for k in ["start", "end"] {
            if w.has(k) {
                out.set(k, w.get(k).clone());
            }
        }
        return out;
    }
    let n = g.get("bytes").list().len();
    for (k, v) in w.items() {
        match k.as_str() {
            "complete" | "pending_bits" => {}
            "bytes" | "acks" => out.set(k, J::List(v.list().iter().take(n).cloned().collect())),
            _ => out.set(k, v.clone()),
        }
    }
    out
}

/// The first transaction that differs ("" if equal), and those neither equal nor different (`verify._i2c_diff`).
fn i2c_diff(want: &[J], got: &[J]) -> (String, Vec<String>) {
    let mut open = vec![];
    let differs = |w: &J, g: &J| {
        let mut sub = J::dict();
        for (k, _) in w.items() {
            sub.set(k, g.get(k).clone());
        }
        !py_eq(w, &sub)
    };
    for (i, (w, g)) in want.iter().zip(got).enumerate() {
        let line = || format!("#{i} want {}, got {}", i2c_fmt(w), i2c_fmt(g));
        if g.has("clock_lost") && (!w.has("complete") || w.get("complete").truthy()) {
            if differs(&i2c_read_part(w, g), g) {
                return (line(), open);
            }
            open.push(line());
            continue;
        }
        if differs(w, g) {
            return (line(), open);
        }
    }
    if want.len() != got.len() {
        let k = want.len().min(got.len());
        let extra = if want.len() > got.len() {
            format!("missing {}", i2c_fmt(&want[got.len()]))
        } else {
            format!("extra {}", i2c_fmt(&got[want.len()]))
        };
        return (
            format!("{} expected, {} decoded; #{k} {extra}", want.len(), got.len()),
            open,
        );
    }
    (String::new(), open)
}

/// SCL stretched, SDA changing in the same sample as an SCL edge, spikes, slow edges (`verify._i2c_notices`).
fn i2c_notices(cap: &Cap, scl: &Ch, sda: &Ch, spike_s: f64, stretch: f64, burst: Option<f64>) -> Vec<J> {
    let mut out = vec![];
    let n = cap.n;
    let (start, length, level) = scl.runs(n);
    let inner = 1..length.len().saturating_sub(1).max(1);
    let lows: Vec<f64> = inner
        .clone()
        .filter(|&k| level[k] == 0)
        .map(|k| length[k] as f64)
        .collect();
    if lows.len() >= 4 {
        let usual = median(&lows);
        for k in inner {
            if level[k] == 0 && length[k] as f64 > stretch * usual {
                out.push(
                    jd! {"kind" => "scl_stretch", "tick" => start[k], "length_ticks" => length[k],
                    "usual_ticks" => round(usual)},
                );
            }
        }
    }
    if scl.step == 1 && sda.step == 1 {
        let mut both: Vec<i64> = scl
            .edges
            .iter()
            .copied()
            .filter(|v| sda.edges.binary_search(v).is_ok())
            .collect();
        both.dedup();
        for t in both {
            let k = scl.edges.partition_point(|&v| v < t);
            out.push(
                jd! {"kind" => "same_sample", "tick" => t, "scl" => if scl.after(k) == 1 { "rise" } else { "fall" }},
            );
        }
    }
    let lim = spike_s * cap.rate;
    for (name, ch) in [("scl", scl), ("sda", sda)] {
        let (st, ln, lv) = ch.runs(n);
        for k in 1..ln.len().saturating_sub(1) {
            if (ln[k] as f64) < lim {
                out.push(
                    jd! {"kind" => "bus_glitch", "tick" => st[k], "line" => name, "level" => lv[k],
                    "width_ticks" => ln[k]},
                );
            }
        }
        let e = &ch.edges;
        let w = i2c_burst_ticks(cap, &scl.name, burst, e);
        for (i, j) in edge_bursts(e, &w) {
            out.push(
                jd! {"kind" => "slow_edge", "tick" => e[i], "line" => name, "changes" => j - i + 1,
                "span_ticks" => e[j] - e[i], "to" => ((ch.initial as usize + j + 1) & 1) as i64},
            );
        }
    }
    out.sort_by_key(|m| m.get("tick").i().unwrap_or(0));
    out
}

const BOUNCE_TICKS: i64 = 4; // staged's: a clock's typical run also with its pulses under this taken out
const GLITCH_SHARE: f64 = 0.6; // a clock's runs this short a share or less: views with them taken out
const GLITCH_SHARE_CLEANED: f64 = 0.9; // ... where its typical run is its cleaned one's

/// The glitch views of an I2C pair (Python's `staged.glitch_views` for a group of SCL and SDA): where SCL has short
/// pulses, rare beside its own runs (or SDA has, SCL clean), the two lines with their pulses under k taken out, for
/// a few k under half SCL's typical run (2, 3 and 5 samples, and one from the pulses seen); those that take out
/// nothing more than a smaller k left out. Lines of a step of their own are left as they are.
fn i2c_glitch_views(cap: &Cap, scl: &str, sda: &str) -> Vec<Cap> {
    let (Some(c), Some(d)) = (cap.ch(scl), cap.ch(sda)) else {
        return vec![];
    };
    if c.step != 1 || d.step != 1 {
        return vec![];
    }
    let (_, length, _) = c.runs(cap.n);
    if length.len() < 18 {
        return vec![];
    }
    let inner: Vec<i64> = length[1..length.len() - 1].to_vec();
    let mut med = median_i(&inner);
    let clean: Vec<i64> = wireskein_core::kernels::deglitch(&c.edges, BOUNCE_TICKS)
        .windows(2)
        .map(|w| w[1] - w[0])
        .collect();
    let mut cleaned = false;
    if clean.len() >= 16 && 4 * clean.len() >= inner.len() {
        let m = median_i(&clean);
        cleaned = m > med;
        med = med.max(m);
    }
    let share_max = if cleaned { GLITCH_SHARE_CLEANED } else { GLITCH_SHARE };
    let lim = (0.25 * med).max(1.0);
    let short = inner.iter().filter(|&&r| r as f64 <= lim).count();
    let mut seen: Vec<f64> = inner.iter().filter(|&&r| r as f64 <= lim).map(|&r| r as f64).collect();
    if short == 0 && d.edges.len() > 2 {
        // a clean clock, SDA with short pulses (crosstalk: a pulse while SCL is high reads as a START or STOP)
        let r: Vec<f64> = d.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
        let s: Vec<f64> = r.iter().copied().filter(|&x| x <= lim).collect();
        let share = s.len() as f64 / r.len() as f64;
        if share > 0.0 && share < GLITCH_SHARE {
            seen = s;
        }
    }
    if med < 3.0 || seen.is_empty() || short as f64 >= share_max * inner.len() as f64 {
        return vec![];
    }
    seen.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = wireskein_core::kernels::quantile(&seen, 0.95);
    let k_rel = (0.3 * med).min(q + 1.0).max(2.0) as i64;
    let mut ks = vec![2, 3, 5, k_rel];
    ks.sort();
    ks.dedup();
    let mut made: Vec<(Vec<i64>, Vec<i64>)> = vec![(c.edges.clone(), d.edges.clone())];
    let mut out = vec![];
    for k in ks {
        if k as f64 > 0.5 * med {
            continue;
        }
        let lines = (
            wireskein_core::kernels::deglitch(&c.edges, k),
            wireskein_core::kernels::deglitch(&d.edges, k),
        );
        if made.contains(&lines) {
            continue;
        }
        let mut v = cap.clone();
        for ch in v.channels.iter_mut() {
            if ch.name == scl {
                ch.edges = lines.0.clone();
            } else if ch.name == sda {
                ch.edges = lines.1.clone();
            }
        }
        made.push(lines);
        out.push(v);
    }
    out
}

/// The view of an I2C pair read best: the input view (`input`), the lines as they are and their glitch views, each
/// scored by the Rust core's I2C reading of the pair (the first of the best kept: the input view where they tie).
fn i2c_best_view(cap: &Cap, input: Cap, scl: &str, sda: &str) -> Cap {
    let score = |v: &Cap| -> f64 {
        let (Some(c), Some(d)) = (v.ch(scl), v.ch(sda)) else {
            return f64::NEG_INFINITY;
        };
        let Ok(view) = wireskein_core::View::new(v.rate, v.n, vec![c.core(), d.core()]) else {
            return f64::NEG_INFINITY;
        };
        wireskein_core::i2c::read(&view, &view.channels[0], &view.channels[1]).map_or(f64::NEG_INFINITY, |r| r.score)
    };
    let mut best = score(&input);
    let mut keep = input;
    for v in std::iter::once(cap.clone()).chain(i2c_glitch_views(cap, scl, sda)) {
        let s = score(&v);
        if s > best {
            best = s;
            keep = v;
        }
    }
    keep
}

fn check_i2c(cap: &Cap, x: &J) -> Out {
    let (Some(scl), Some(sda)) = (cap.ch(s(x, "scl")), cap.ch(s(x, "sda"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let spike = if x.has("spike_max_s") {
        x.get("spike_max_s").f().unwrap_or(I2C_SPIKE_S)
    } else {
        I2C_SPIKE_S
    };
    let w = x.get("freq_hz").f().filter(|&f| f != 0.0);
    let (read, _) = i2c_input(cap, &scl.name, &sda.name, spike, w);
    // the reading: the input view, again on its own output while that changes something
    let spike_e = if x.get("spike_max_s").truthy() {
        spike
    } else {
        I2C_SPIKE_S
    };
    let (mut view, mut changed) = i2c_input(cap, &scl.name, &sda.name, spike_e, w);
    for _ in 0..I2C_INPUT_PASSES - 1 {
        if !changed {
            break;
        }
        let (v2, more) = i2c_input(&view, &scl.name, &sda.name, spike_e, w);
        if !more {
            break;
        }
        view = v2;
        changed = more;
    }
    // at a rate that does not show t_SP (under 2 samples in it) the lines as they are and their glitch views are
    // read beside the input view, the best kept (Python's engine reads the group so: staged's views, i2c_only)
    if spike_e * cap.rate < 2.0 {
        view = i2c_best_view(cap, view, &scl.name, &sda.name);
    }
    let items = i2c_transactions(&view, &scl.name, &sda.name);
    let txs: Vec<J> = items.iter().map(i2c_tx).collect();
    let mut got = jd! {"transactions" => txs.clone()};
    let (mut ok, mut why) = (Some(true), vec![]);
    if !x.get("transactions").is_null() {
        let want: Vec<J> = x.get("transactions").list().iter().map(i2c_want).collect();
        let (diff, open) = i2c_diff(&want, &txs);
        if !diff.is_empty() {
            ok = Some(false);
            why.push(format!("transactions differ: {diff}"));
        } else if !open.is_empty() {
            // a clock lost in a wait: neither a match nor a difference
            ok = None;
            why.push(format!(
                "bytes not read where a clock was lost in an SCL wait (a controller that gave up waiting for a \
                 target stretching the clock): {}",
                open.join("; ")
            ));
        }
    }
    let hz = read.ch(&scl.name).and_then(|c| burst_hz(c, cap.rate));
    got.set("scl_hz", hz);
    let stretch = if x.has("stretch_min") {
        x.get("stretch_min").f().unwrap_or(2.0)
    } else {
        2.0
    };
    let mut notices = i2c_notices(cap, scl, sda, spike, stretch, w);
    for t in &items {
        for at in t.get("restored").list() {
            notices.push(jd! {"kind" => "clock_restored", "tick" => at});
        }
    }
    for t in &txs {
        if t.has("clock_lost") {
            notices.push(jd! {"kind" => "clock_lost", "tick" => t.get("clock_lost").clone(),
            "bits" => t.get("pending_bits").list().len()});
        }
    }
    notices.sort_by_key(|m| m.get("tick").i().unwrap_or(0));
    let count = |k: &str| notices.iter().filter(|m| m.get("kind").s() == Some(k)).count();
    let (stretches, spikes) = (count("scl_stretch"), count("bus_glitch"));
    let same: Vec<&J> = notices
        .iter()
        .filter(|m| m.get("kind").s() == Some("same_sample"))
        .collect();
    let n_same = same.len();
    if n_same > 0 && !why.is_empty() {
        let k = same[0];
        let rise = k.get("scl").s() == Some("rise");
        why.push(format!(
            "SDA changed in the same sample as SCL {} {n_same} times (first at tick {}): its {} time is under a \
             sample ({} ns), so the change may read as SDA moving while SCL is high (a START or STOP that is not \
             there): the controller's waveform, or a rate too low for it",
            if rise { "rose" } else { "fell" },
            k.get("tick").repr(),
            if rise { "setup" } else { "hold" },
            crate::pyfmt::g(1e9 / cap.rate, 3)
        ));
    }
    got.set("notices", J::List(notices.clone()));
    got.set("stretches", stretches);
    got.set("spikes", spikes);
    got.set("same_sample", n_same);
    if let Some(f) = x.get("freq_hz").f()
        && hz.is_none_or(|h| (h / f - 1.0).abs() > x.get("freq_rel").f().unwrap_or(0.1))
    {
        ok = Some(false);
        why.push(format!(
            "SCL {} Hz vs {}",
            match hz {
                Some(h) if h != 0.0 => round(h).to_string(),
                Some(h) => J::from(h).repr(),
                None => "None".into(),
            },
            x.get("freq_hz").repr()
        ));
    }
    if !x.has("released") || x.get("released").truthy() {
        let (es, ed) = (scl.level_at(cap.n - 1), sda.level_at(cap.n - 1));
        got.set("end_levels", jd! {"scl" => es, "sda" => ed});
        if (es, ed) != (1, 1) {
            ok = Some(false);
            why.push(format!("bus not released at the end (SCL={es}, SDA={ed})"));
        }
    }
    out(V::of(ok), got, why.join("; "))
}

// ---------------- SPI ----------------

/// A data line's setup and hold around the sampling edges (`timing.sampling_margins`).
fn sampling_margins(clk: &Ch, data: &[&Ch], edge: &str, rate: f64, cs: Option<&Ch>, act: u8) -> Vec<(String, J)> {
    let want = if edge == "rise" { 1 } else { 0 };
    let mut t: Vec<i64> = (0..clk.edges.len())
        .filter(|&i| clk.after(i) == want)
        .map(|i| clk.edges[i])
        .collect();
    if let Some(cs) = cs {
        t.retain(|&v| cs.level_at(v) == act);
    }
    data.iter()
        .map(|d| {
            let de = &d.edges;
            if t.is_empty() || de.is_empty() {
                return (d.name.clone(), jd! {"setup_s" => J::Null, "hold_s" => J::Null});
            }
            let (mut before, mut after) = (i64::MAX, i64::MAX);
            for &x in &t {
                let j = de.partition_point(|&v| v < x);
                if j > 0 {
                    before = before.min(x - de[j - 1]);
                }
                if j < de.len() {
                    after = after.min(de[j] - x);
                }
            }
            let f = |v: i64| {
                if v == i64::MAX {
                    J::Null
                } else {
                    J::Float(v as f64 / rate)
                }
            };
            (d.name.clone(), jd! {"setup_s" => f(before), "hold_s" => f(after)})
        })
        .collect()
}

/// Each data line's bytes read just before each sampling edge, per window of CS active (`timing.spi_read`).
fn spi_read(clk: &Ch, data: &[&Ch], edge: &str, n: i64, cs: Option<&Ch>, act: u8, lsb: bool) -> J {
    let want = if edge == "rise" { 1 } else { 0 };
    let t: Vec<i64> = (0..clk.edges.len())
        .filter(|&i| clk.after(i) == want)
        .map(|i| clk.edges[i])
        .collect();
    let (keep, win): (Vec<bool>, Vec<usize>) = match cs {
        Some(cs) => {
            let mut marks = vec![0];
            marks.extend(&cs.edges);
            marks.push(n);
            (
                t.iter().map(|&v| cs.level_at(v) == act).collect(),
                t.iter().map(|&v| marks.partition_point(|&m| m <= v)).collect(),
            )
        }
        None => (vec![true; t.len()], vec![0; t.len()]),
    };
    let mut ws: Vec<usize> = (0..t.len()).filter(|&i| keep[i]).map(|i| win[i]).collect();
    ws.sort_unstable();
    ws.dedup();
    let mut o = J::dict();
    for d in data {
        let mut bytes = vec![];
        for &w in &ws {
            let bits: Vec<u8> = (0..t.len())
                .filter(|&i| keep[i] && win[i] == w)
                .map(|i| d.level_at((t[i] - 1).max(0)))
                .collect();
            for k in 0..bits.len() / 8 {
                let b = &bits[8 * k..8 * k + 8];
                bytes.push(if lsb {
                    b.iter().rev().fold(0u8, |a, &v| (a << 1) | v)
                } else {
                    b.iter().fold(0u8, |a, &v| (a << 1) | v)
                });
            }
        }
        o.set(&d.name, crate::bus::hex(&bytes));
    }
    o
}

/// The two modes of SCK's idle level, each readable or not and the bytes it reads (`timing.spi_modes`).
#[allow(clippy::too_many_arguments)]
fn spi_modes(
    clk: &Ch,
    data: &[&Ch],
    cpol: u8,
    rate: f64,
    n: i64,
    cs: Option<&Ch>,
    act: u8,
    lsb: bool,
    edge_sample: Option<&str>,
) -> Vec<J> {
    let hold_min = if edge_sample == Some("before") { 0.0 } else { 1.0 / rate };
    (0..2u8)
        .map(|h| {
            let mode = cpol * 2 + h;
            let edge = if cpol == h { "rise" } else { "fall" };
            let mg = sampling_margins(clk, data, edge, rate, cs, act);
            let ok = mg.iter().all(|(_, v)| {
                v.get("setup_s").f().is_none_or(|s| s >= 1.0 / rate)
                    && v.get("hold_s").f().is_none_or(|h| h >= hold_min)
            });
            jd! {"mode" => mode, "readable" => ok, "bytes" => spi_read(clk, data, edge, n, cs, act, lsb)}
        })
        .collect()
}

type Clocks = (Vec<i64>, i64, Option<i64>, Option<f64>, Vec<J>);

/// Per window of CS active its clocks, SCK's edges outside, the shortest and median SCK level inside, the notices
/// (`verify._spi_clocks`).
fn spi_clocks(sck: &Ch, cs: &Ch, n: i64, active: u8) -> Clocks {
    let mut marks = vec![0];
    marks.extend(&cs.edges);
    marks.push(n);
    let mut lv = cs.initial;
    let (mut frames, mut outside, mut levels, mut found) = (vec![], 0i64, vec![], vec![]);
    for m in marks.windows(2) {
        let (a, b) = (m[0], m[1]);
        let e: Vec<i64> = sck.edges.iter().copied().filter(|&v| v > a && v < b).collect();
        if lv == active {
            frames.push(e.len() as i64 / 2);
            if e.len() > 1 {
                levels.extend(e.windows(2).map(|w| w[1] - w[0]));
            }
            if e.is_empty() && a > 0 {
                found.push(jd! {"kind" => "empty_cs_window", "tick" => a, "length_ticks" => b - a});
            }
        } else if !e.is_empty() {
            outside += e.len() as i64;
            found.push(jd! {"kind" => "sck_outside_cs", "tick" => e[0], "count" => e.len()});
        }
        lv ^= 1;
    }
    if levels.is_empty() {
        return (frames, outside, None, None, found);
    }
    let shortest = *levels.iter().min().unwrap();
    (frames, outside, Some(shortest), Some(median_i(&levels)), found)
}

fn mode_edge(mode: i64) -> &'static str {
    if (mode >> 1) == (mode & 1) { "rise" } else { "fall" }
}

fn check_spi(cap0: &Cap, x: &J) -> Out {
    let mut roles: Vec<(&str, String)> = vec![("clk", s(x, "clk").to_string())];
    for k in ["mosi", "miso", "cs"] {
        if x.get(k).truthy() {
            roles.push((k, s(x, k).to_string()));
        }
    }
    if roles.iter().any(|(_, p)| cap0.ch(p).is_none()) {
        return out(V::Unchecked, J::dict(), "pins not captured");
    }
    let act = if x.has("cs_active") {
        x.get("cs_active").i().unwrap_or(0) as u8
    } else {
        0
    };
    let has_cs = x.get("cs").truthy();
    // CS sampled coarser than the clock: its change to active taken as early as it could have been
    let mut cs_sample_s = None;
    let mut fine = None;
    if has_cs {
        let ch = cap0.ch(s(x, "cs")).unwrap();
        if ch.step <= 1 || ch.edges.is_empty() || any(Some(ch)) == Some(act) {
            if any(Some(ch)) == Some(act) {
                cs_sample_s = Some(ch.step as f64 / cap0.rate);
            }
        } else {
            let mut e = ch.edges.clone();
            for (i, v) in e.iter_mut().enumerate() {
                if ch.after(i) == act {
                    let prev = if i == 0 { 0 } else { ch.edges[i - 1] + 1 };
                    *v = (*v - (ch.step - 1)).max(prev);
                }
            }
            let mut c2 = cap0.clone();
            for c in c2.channels.iter_mut() {
                if c.name == ch.name {
                    c.edges = e.clone();
                    c.step = 1;
                    c.phase = 0;
                }
            }
            fine = Some(c2);
            cs_sample_s = Some(ch.step as f64 / cap0.rate);
        }
    }
    let cap = fine.as_ref().unwrap_or(cap0);
    let rate = cap.rate;
    let clk = cap.ch(s(x, "clk")).unwrap();
    let cs = if has_cs { cap.ch(s(x, "cs")) } else { None };
    let lines: Vec<&Ch> = ["mosi", "miso"]
        .iter()
        .filter(|k| x.get(k).truthy())
        .map(|k| cap.ch(s(x, k)).unwrap())
        .collect();
    let lsb = x.get("bit_order").s() == Some("lsb");
    // the core's SPI readings on these pins
    let chans: Vec<wireskein_core::Channel> = cap.channels.iter().map(|c| c.core()).collect();
    let readings = match wireskein_core::View::new(rate, cap.n, chans) {
        Ok(v) => {
            let mut pins = serde_json::Map::new();
            for (r, p) in &roles {
                pins.insert(p.clone(), serde_json::json!({"protocol": "spi", "role": r}));
            }
            wireskein_core::readings(&v, &serde_json::json!({"protocols": ["spi"], "pins": pins}))
        }
        Err(_) => vec![],
    };
    let need: Vec<(&str, String)> = roles
        .iter()
        .filter(|(r, _)| *r == "clk" || *r == "cs")
        .map(|(r, p)| (*r, p.clone()))
        .collect();
    let best = |edge: Option<&str>| -> Option<&wireskein_core::Candidate> {
        readings
            .iter()
            .filter(|c| c.protocol == "spi" && need.iter().all(|(r, p)| c.roles.get(*r) == Some(p)))
            .filter(|c| edge.is_none_or(|e| c.params.get("sample_edge").and_then(|v| v.as_str()) == Some(e)))
            .fold(None, |a: Option<&wireskein_core::Candidate>, c| match a {
                Some(a) if a.score >= c.score => Some(a),
                _ => Some(c),
            })
    };
    let m = timing::spi_mode(clk, &lines, cap.n, cs, act);
    let line = lines.iter().find_map(|l| {
        m.lines
            .iter()
            .find(|(n, v)| *n == l.name && v.cpha.is_some())
            .map(|(_, v)| v.clone())
    });
    let mut n = best(None);
    let mut edge: Option<&str> = None;
    if let (Some(lv), Some(cp)) = (&line, m.cpol) {
        let e = if cp == lv.cpha.unwrap() { "rise" } else { "fall" };
        edge = Some(e);
        n = best(Some(e)).or(n);
    }
    let edge_sample = x.get("edge_sample").s();
    if has_cs
        && edge.is_none()
        && let Some(cp) = m.cpol
    {
        let opts = spi_modes(clk, &lines, cp, rate, cap.n, cs, act, false, edge_sample);
        let modes: Vec<i64> = opts.iter().map(|o| o.get("mode").i().unwrap()).collect();
        let pick = match x.get("mode").i() {
            Some(md) if modes.contains(&md) => Some(md),
            _ => opts
                .iter()
                .find(|o| o.get("readable").truthy())
                .map(|o| o.get("mode").i().unwrap()),
        };
        if let Some(p) = pick {
            edge = Some(mode_edge(p));
        }
    }
    if has_cs && let Some(md) = x.get("mode").i() {
        edge = Some(mode_edge(md));
    }
    let mut margins: Vec<(String, J)> = vec![];
    let hexes: J;
    if has_cs && let Some(e) = edge {
        hexes = spi_read(clk, &lines, e, cap.n, cs, act, lsb);
        let other = if e == "rise" { "fall" } else { "rise" };
        let here = sampling_margins(clk, &lines, e, rate, cs, act);
        let there = sampling_margins(clk, &lines, other, rate, cs, act);
        for ((p, h), (_, t)) in here.into_iter().zip(there) {
            let mut d = h.clone();
            d.set("other_setup_s", t.get("setup_s").clone());
            d.set("other_hold_s", t.get("hold_s").clone());
            margins.push((p, d));
        }
    } else if let Some(c) = n {
        let v: serde_json::Value = serde_json::from_str(c.layers.get()).unwrap_or_default();
        let tr = v["transfers"].as_array().cloned().unwrap_or_default();
        let mut data_roles: Vec<(&String, &String)> = c.roles.iter().filter(|(k, _)| k.starts_with("data")).collect();
        data_roles.sort();
        let mut h = J::dict();
        for (_, pin) in data_roles {
            let mut s = String::new();
            for t in &tr {
                let hx = t[pin.as_str()].as_str().unwrap_or("");
                for k in 0..hx.len() / 2 {
                    let b = u8::from_str_radix(&hx[2 * k..2 * k + 2], 16).unwrap_or(0);
                    s += &format!("{:02x}", if lsb { b.reverse_bits() } else { b });
                }
            }
            h.set(pin, s);
        }
        // a line the test names that the decoder's reading left out (a data line of few changes, such as one held
        // high after a few bytes): read at the sampling edge the lines say, else the asked mode's, as given
        let e = edge.or_else(|| x.get("mode").i().map(mode_edge));
        let missing: Vec<&Ch> = lines.iter().copied().filter(|l| !h.has(&l.name)).collect();
        if let Some(e) = e
            && !missing.is_empty()
        {
            for (k, v) in spi_read(clk, &missing, e, cap.n, None, act, lsb).items() {
                h.set(k, v.clone());
            }
        }
        hexes = h;
    } else {
        return out(V::Ng, J::dict(), "nothing decoded as SPI on these pins");
    }
    let decoded_mode = n.and_then(|c| c.evidence.get("mode").and_then(|v| v.as_i64()));
    let mut got = jd! {"mode" => decoded_mode, "bytes" => hexes.clone(), "sck_hz" => burst_hz(clk, rate)};
    if let Some(cs_s) = cs_sample_s {
        got.set("cs_sample_s", cs_s);
    }
    if !margins.is_empty() {
        let cpol = m
            .cpol
            .map(|c| c as i64)
            .unwrap_or_else(|| x.get("mode").i().unwrap_or(0) >> 1) as u8;
        let modes = spi_modes(clk, &lines, cpol, rate, cap.n, cs, act, lsb, edge_sample);
        for (_, mg) in margins.iter_mut() {
            if let Some(h) = mg.get("hold_s").f()
                && h < 1.0 / rate
            {
                mg.set("hold_under_sample", true);
                mg.set("sample_s", 1.0 / rate);
            }
        }
        got.set("margins", J::Dict(margins.clone()));
        got.set("modes", J::List(modes.clone()));
        got.set(
            "modes_ok",
            modes
                .iter()
                .filter(|m| m.get("readable").truthy())
                .map(|m| m.get("mode").clone())
                .collect::<Vec<_>>(),
        );
    }
    let (mut ok, mut why) = (true, vec![]);
    if let (Some(lv), Some(cp)) = (&line, m.cpol) {
        let cpha = lv.cpha.unwrap();
        got.set("decoded_mode", got.get("mode").clone());
        got.set("mode", cp as i64 * 2 + cpha as i64);
        got.set("cpol", cp);
        got.set("cpha", cpha);
        let mut votes = J::dict();
        for (k, v) in &m.lines {
            votes.set(
                k,
                jd! {"after_leading" => v.after_leading, "after_trailing" => v.after_trailing,
                "before_first" => v.before_first},
            );
        }
        got.set("cpha_votes", votes);
        let zeros = m.cpol_votes.iter().filter(|&&v| v == 0).count();
        if zeros > 0 && zeros < m.cpol_votes.len() {
            ok = false;
            why.push(format!(
                "SCK's level when CS became active was {zeros} times 0 and {} times 1",
                m.cpol_votes.len() - zeros
            ));
        }
    }
    for role in ["mosi", "miso"] {
        if let Some(want) = x.get(&format!("{role}_bytes")).s() {
            let have = if x.get(role).truthy() {
                hexes.get(s(x, role))
            } else {
                &crate::j::NULL
            };
            if have.s() != Some(want.to_lowercase().as_str()) {
                ok = false;
                why.push(format!("{role} {} vs {want}", py_str(have)));
            }
        }
    }
    for (pin, mg) in &margins {
        for k in ["setup", "hold"] {
            if let (Some(lim), Some(v)) = (x.get(&format!("{k}_min_s")).f(), mg.get(&format!("{k}_s")).f())
                && v < lim
            {
                ok = false;
                why.push(format!(
                    "{pin} {k} {:.0} ns at the sampling edges, want {:.0} ns",
                    v * 1e9,
                    lim * 1e9
                ));
            }
        }
    }
    if got.has("modes") {
        let want: Vec<(String, Option<String>)> = ["mosi", "miso"]
            .iter()
            .filter(|r| x.get(r).truthy())
            .map(|r| {
                (
                    s(x, r).to_string(),
                    x.get(&format!("{r}_bytes")).s().map(|w| w.to_lowercase()),
                )
            })
            .collect();
        let cands: Vec<i64> = got
            .get("modes")
            .list()
            .iter()
            .filter(|mm| {
                mm.get("readable").truthy()
                    && want
                        .iter()
                        .all(|(p, w)| w.as_ref().is_none_or(|w| mm.get("bytes").get(p).s() == Some(w)))
            })
            .map(|mm| mm.get("mode").i().unwrap())
            .collect();
        got.set("mode_candidates", cands.clone());
        if cands.len() > 1 {
            let first = cs.and_then(|c| c.edges.first().copied()).unwrap_or(0);
            push_notice(
                &mut got,
                jd! {"kind" => "mode_ambiguous", "tick" => first, "candidates" => cands.clone()},
            );
        }
        if let Some(md) = x.get("mode").i()
            && !cands.contains(&md)
        {
            ok = false;
            why.push(format!(
                "mode {md} does not read it (it reads in {}; its changes say mode {})",
                if cands.is_empty() {
                    "no mode".to_string()
                } else {
                    J::from(cands.clone()).repr()
                },
                got.get("mode").repr()
            ));
        }
    } else if let Some(md) = x.get("mode").i()
        && got.get("mode").i() != Some(md)
    {
        ok = false;
        why.push(format!("mode {} vs {md}", got.get("mode").repr()));
    }
    if let Some(f) = x.get("freq_hz").f() {
        let hz = got.get("sck_hz").f();
        if hz.is_none_or(|h| (h / f - 1.0).abs() > x.get("freq_rel").f().unwrap_or(0.1)) {
            ok = false;
            why.push(format!(
                "SCK {} Hz vs {}",
                match hz {
                    Some(h) if h != 0.0 => round(h).to_string(),
                    Some(h) => J::from(h).repr(),
                    None => "None".into(),
                },
                x.get("freq_hz").repr()
            ));
        }
    }
    if let Some(cs) = cs {
        let cs_end = cs.level_at(cap.n - 1);
        got.set("cs_end", cs_end);
        if cs_end != 1 {
            ok = false;
            why.push("CS not high at the end".into());
        }
        let (frames, outside, shortest, half, found) = spi_clocks(clk, cs, cap.n, act);
        for f in found {
            push_notice(&mut got, f);
        }
        got.set("frame_bits", frames.clone());
        got.set("sck_edges_outside_cs", outside);
        got.set(
            "sck_shortest_s",
            match shortest {
                Some(0) => J::Int(0),
                Some(v) => J::Float(v as f64 / rate),
                None => J::Null,
            },
        );
        got.set(
            "sck_half_period_s",
            match half {
                Some(h) => J::Float(h / rate),
                None => J::Null,
            },
        );
        let want = x.get("bits");
        if !want.is_null() {
            let want: Vec<i64> = match want {
                J::List(v) => v.iter().filter_map(|b| b.i()).collect(),
                v => vec![v.i().unwrap_or(0); frames.len()],
            };
            if frames != want {
                ok = false;
                why.push(format!(
                    "clocks while CS is active {}, want {}",
                    J::from(frames.clone()).repr(),
                    J::from(want).repr()
                ));
            }
        }
        if let (Some(sh), Some(h)) = (shortest, half)
            && h != 0.0
            && (sh as f64) < 0.5 * h
        {
            ok = false;
            why.push(format!(
                "SCK held a level {:.0} ns while CS was active, its half period {:.0} ns: a glitch",
                sh as f64 / rate * 1e9,
                h / rate * 1e9
            ));
        }
    }
    out(V::of(Some(ok)), got, why.join("; "))
}
