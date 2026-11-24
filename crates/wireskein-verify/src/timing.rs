//! Times between edges of a bus's or a group of outputs' lines (Python's `_engine/timing.py`): the phase of two
//! PWMs, the skew of two patterns, the dead time of a complementary pair, I2C's per-bit times (UM10204), the SPI mode
//! from the lines, a line's shorter half.

use crate::cap::Ch;
use crate::j::J;

pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

pub fn median_i(v: &[i64]) -> f64 {
    median(&v.iter().map(|&x| x as f64).collect::<Vec<_>>())
}

/// numpy's percentile (linear).
pub fn percentile(v: &[f64], q: f64) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    wireskein_core::kernels::quantile(&s, q / 100.0)
}

pub fn res_of(chs: &[&Ch]) -> i64 {
    chs.iter().map(|c| c.step).max().unwrap_or(1)
}

/// True when every value in [v - res, v + res] is in [lo, hi], False when none is, None otherwise.
pub fn judge(v: f64, res: f64, lo: Option<f64>, hi: Option<f64>) -> Option<bool> {
    let (a, b) = (v - res, v + res);
    if lo.is_some_and(|lo| b < lo) || hi.is_some_and(|hi| a > hi) {
        return Some(false);
    }
    if lo.is_some_and(|lo| a < lo) || hi.is_some_and(|hi| b > hi) {
        return None;
    }
    Some(true)
}

/// (starts, levels per line) of the lines' states (`timing.states`).
pub fn states(chs: &[&Ch], n: i64) -> (Vec<i64>, Vec<Vec<u8>>) {
    let mut t: Vec<i64> = chs
        .iter()
        .flat_map(|c| c.edges.iter().copied())
        .filter(|&e| e > 0 && e < n)
        .collect();
    t.sort_unstable();
    t.dedup();
    let mut starts = vec![0];
    starts.extend(t);
    let lv = starts
        .iter()
        .map(|&s| chs.iter().map(|c| c.level_at(s)).collect())
        .collect();
    (starts, lv)
}

fn dirs(ch: &Ch, edge: &str) -> Vec<i64> {
    if edge == "rise" { ch.rises() } else { ch.falls() }
}

/// b's edges after a's of the same direction, as a fraction of a's period (`timing.phase`): the measured dict and
/// each pair's degrees.
pub fn phase(a: &Ch, b: &Ch, rate: f64, edge: &str) -> (J, Vec<f64>) {
    let (ea, eb) = (dirs(a, edge), dirs(b, edge));
    if ea.len() < 3 || eb.len() < 2 {
        return (crate::jd! {"pairs" => 0}, vec![]);
    }
    let diff = |v: &[i64]| v.windows(2).map(|w| (w[1] - w[0]) as f64).collect::<Vec<_>>();
    let per_a = median(&diff(&ea));
    let per_b = median(&diff(&eb));
    let mut d = vec![];
    for &x in &ea {
        let j = eb.partition_point(|&y| y < x);
        if j < eb.len() {
            let v = (eb[j] - x) as f64;
            if v < per_a {
                d.push(v);
            }
        }
    }
    if d.is_empty() {
        return (
            crate::jd! {"pairs" => 0, "freq_hz" => rate / per_a, "freq_b_hz" => rate / per_b},
            vec![],
        );
    }
    let deg: Vec<f64> = d.iter().map(|v| v / per_a * 360.0).collect();
    let res = res_of(&[a, b]) as f64;
    let mn = deg.iter().cloned().fold(f64::INFINITY, f64::min);
    let mx = deg.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    (
        crate::jd! {"pairs" => d.len(), "freq_hz" => rate / per_a, "freq_b_hz" => rate / per_b,
        "phase_deg" => median(&deg), "min_deg" => mn, "max_deg" => mx, "delay_s" => median(&d) / rate,
        "resolution_deg" => res / per_a * 360.0},
        deg,
    )
}

/// x - y in degrees, folded into [-180, 180).
pub fn circular_diff(x: f64, y: f64) -> f64 {
    (x - y + 180.0).rem_euclid(360.0) - 180.0
}

pub struct Skew {
    pub delays: Vec<i64>,
    pub unpaired_a: i64,
    pub unpaired_b: i64,
}

/// Each edge of a with b's nearest edge of the same direction (`timing.skew`).
pub fn skew(a: &Ch, b: &Ch, edge: &str, n: i64) -> Skew {
    let mut out = Skew {
        delays: vec![],
        unpaired_a: 0,
        unpaired_b: 0,
    };
    let edges: &[&str] = if edge == "both" {
        &["rise", "fall"]
    } else {
        std::slice::from_ref(&edge)
    };
    for &e in edges {
        let (ea, eb) = (dirs(a, e), dirs(b, e));
        if ea.is_empty() || eb.is_empty() {
            out.unpaired_a += ea.len() as i64;
            out.unpaired_b += eb.len() as i64;
            continue;
        }
        let gap = if ea.len() > 1 {
            ea.windows(2).map(|w| w[1] - w[0]).min().unwrap() as f64 / 2.0
        } else {
            f64::INFINITY
        };
        let inside = |x: i64| -> bool {
            if !gap.is_finite() {
                return true;
            }
            x as f64 >= gap && x as f64 <= n as f64 - gap
        };
        let m = eb.len();
        let near: Vec<usize> = ea
            .iter()
            .map(|&x| {
                let j = eb.partition_point(|&y| y < x).clamp(1, m) - 1;
                let k = (j + 1).min(m - 1);
                if (eb[k] - x).abs() < (eb[j] - x).abs() { k } else { j }
            })
            .collect();
        let mut taken = vec![false; m];
        for (i, &x) in ea.iter().enumerate() {
            let d = eb[near[i]] - x;
            let ok = ((d.abs() as f64) < gap) && (i == 0 || near[i] != near[i - 1]);
            if ok {
                out.delays.push(d);
                taken[near[i]] = true;
            } else if inside(x) {
                out.unpaired_a += 1;
            }
        }
        out.unpaired_b += eb.iter().enumerate().filter(|&(k, &y)| !taken[k] && inside(y)).count() as i64;
    }
    out
}

pub struct Dead {
    pub hi_to_lo: Vec<(i64, i64)>,
    pub lo_to_hi: Vec<(i64, i64)>,
    pub overlaps: Vec<(i64, i64)>,
}

/// The two outputs of a half bridge (`timing.dead_time`).
pub fn dead_time(hi: &Ch, lo: &Ch, n: i64, active: u8) -> Dead {
    let (starts, lv) = states(&[hi, lo], n);
    let code: Vec<u8> = lv
        .iter()
        .map(|l| (l[0] == active) as u8 + 2 * (l[1] == active) as u8)
        .collect();
    let mut ends: Vec<i64> = starts[1..].to_vec();
    ends.push(n);
    let mut d = Dead {
        hi_to_lo: vec![],
        lo_to_hi: vec![],
        overlaps: vec![],
    };
    for k in 0..code.len().saturating_sub(1) {
        let (c, nx) = (code[k], code[k + 1]);
        if c == 3 && k > 0 {
            d.overlaps.push((starts[k], ends[k] - starts[k]));
        }
        if (c, nx) == (1, 2) {
            d.hi_to_lo.push((starts[k + 1], 0));
        } else if (c, nx) == (2, 1) {
            d.lo_to_hi.push((starts[k + 1], 0));
        } else if c == 0 && k > 0 {
            let p = code[k - 1];
            if (p, nx) == (1, 2) {
                d.hi_to_lo.push((starts[k], ends[k] - starts[k]));
            } else if (p, nx) == (2, 1) {
                d.lo_to_hi.push((starts[k], ends[k] - starts[k]));
            }
        }
    }
    d
}

// ---------------- I2C ----------------

pub const I2C_TIMES: [&str; 9] = [
    "t_low", "t_high", "t_hd_sta", "t_su_sta", "t_hd_dat", "t_su_dat", "t_vd_dat", "t_su_sto", "t_buf",
];

type Lim = (Option<f64>, Option<f64>);

/// UM10204 (rev. 7) table 10, seconds: (min, max).
pub fn i2c_mode(mode: &str) -> Option<Vec<(&'static str, Lim)>> {
    let v = match mode {
        "standard" => [
            ("f_scl", (None, Some(100e3))),
            ("t_hd_sta", (Some(4.0e-6), None)),
            ("t_low", (Some(4.7e-6), None)),
            ("t_high", (Some(4.0e-6), None)),
            ("t_su_sta", (Some(4.7e-6), None)),
            ("t_hd_dat", (Some(0.0), None)),
            ("t_su_dat", (Some(250e-9), None)),
            ("t_su_sto", (Some(4.0e-6), None)),
            ("t_buf", (Some(4.7e-6), None)),
            ("t_vd_dat", (None, Some(3.45e-6))),
        ],
        "fast" => [
            ("f_scl", (None, Some(400e3))),
            ("t_hd_sta", (Some(0.6e-6), None)),
            ("t_low", (Some(1.3e-6), None)),
            ("t_high", (Some(0.6e-6), None)),
            ("t_su_sta", (Some(0.6e-6), None)),
            ("t_hd_dat", (Some(0.0), None)),
            ("t_su_dat", (Some(100e-9), None)),
            ("t_su_sto", (Some(0.6e-6), None)),
            ("t_buf", (Some(1.3e-6), None)),
            ("t_vd_dat", (None, Some(0.9e-6))),
        ],
        "fast-plus" => [
            ("f_scl", (None, Some(1e6))),
            ("t_hd_sta", (Some(0.26e-6), None)),
            ("t_low", (Some(0.5e-6), None)),
            ("t_high", (Some(0.26e-6), None)),
            ("t_su_sta", (Some(0.26e-6), None)),
            ("t_hd_dat", (Some(0.0), None)),
            ("t_su_dat", (Some(50e-9), None)),
            ("t_su_sto", (Some(0.26e-6), None)),
            ("t_buf", (Some(0.5e-6), None)),
            ("t_vd_dat", (None, Some(0.45e-6))),
        ],
        _ => return None,
    };
    Some(v.to_vec())
}

/// The limits an i2c_timing expectation sets: its mode's, then its own (`"t_low_min_s"` ...), in their order.
pub fn i2c_limits(x: &J) -> Vec<(String, Lim)> {
    let mut lim: Vec<(String, Lim)> = match x.get("mode").s() {
        Some(m) if !m.is_empty() => i2c_mode(m)
            .unwrap_or_default()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        _ => vec![],
    };
    for (k, v) in x.get("limits").items() {
        let mut it = k.rsplitn(3, '_');
        let (_, side, name) = (it.next(), it.next().unwrap_or(""), it.next().unwrap_or(""));
        let v = v.f();
        match lim.iter_mut().find(|(n, _)| n == name) {
            Some(e) => {
                if side == "max" {
                    e.1.1 = v
                } else {
                    e.1.0 = v
                }
            }
            None => lim.push((name.to_string(), if side == "max" { (None, v) } else { (v, None) })),
        }
    }
    lim
}

pub type Times = std::collections::BTreeMap<&'static str, Vec<(i64, i64)>>;

/// Every instance of UM10204's times on the lines (`timing.i2c_times`), and "periods".
pub fn i2c_times(scl: &Ch, sda: &Ch, n: i64) -> Times {
    let (starts, lv) = states(&[scl, sda], n);
    let mut out: Times = I2C_TIMES.iter().map(|&k| (k, vec![])).collect();
    out.insert("periods", vec![]);
    let mut in_tx = false;
    let (mut last_rise, mut last_fall, mut last_stop, mut start_at): (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    ) = (None, None, None, None);
    let mut cond = false;
    let mut vd: Option<(i64, i64)> = None;
    let mut changes: Vec<i64> = vec![];
    for k in 1..starts.len() {
        let t = starts[k];
        let (s0, s1, d0, d1) = (lv[k - 1][0], lv[k][0], lv[k - 1][1], lv[k][1]);
        if d0 != d1 {
            if s0 == 1 && s1 == 1 {
                if d1 == 0 {
                    if in_tx && let Some(r) = last_rise {
                        out.get_mut("t_su_sta").unwrap().push((r, t - r));
                    } else if !in_tx && let Some(s) = last_stop {
                        out.get_mut("t_buf").unwrap().push((s, t - s));
                    }
                    in_tx = true;
                    start_at = Some(t);
                } else {
                    if in_tx && let Some(r) = last_rise {
                        out.get_mut("t_su_sto").unwrap().push((r, t - r));
                    }
                    in_tx = false;
                    last_stop = Some(t);
                    last_fall = None;
                }
                cond = true;
            } else if in_tx && let Some(f) = last_fall {
                if changes.is_empty() {
                    out.get_mut("t_hd_dat").unwrap().push((f, t - f));
                }
                changes.push(t);
            }
        }
        if s0 != s1 && s1 == 1 {
            if in_tx && let Some(f) = last_fall {
                out.get_mut("t_low").unwrap().push((f, t - f));
                if let Some(&c) = changes.last() {
                    out.get_mut("t_su_dat").unwrap().push((c, t - c));
                    vd = Some((f, c - f));
                }
                if let Some(r) = last_rise
                    && !cond
                {
                    out.get_mut("periods").unwrap().push((r, t - r));
                }
            }
            last_rise = Some(t);
            cond = false;
        } else if s0 != s1 {
            if let Some(v) = vd
                && !cond
            {
                out.get_mut("t_vd_dat").unwrap().push(v);
            }
            vd = None;
            if let Some(s) = start_at {
                out.get_mut("t_hd_sta").unwrap().push((s, t - s));
            } else if in_tx
                && !cond
                && let Some(r) = last_rise
            {
                out.get_mut("t_high").unwrap().push((r, t - r));
            }
            start_at = None;
            changes.clear();
            last_fall = if in_tx { Some(t) } else { None };
        }
    }
    out
}

/// The shorter of a line's median high and median low, in ticks (its first and last runs left out).
pub fn short_half(ch: &Ch, n: i64) -> Option<f64> {
    if ch.edges.len() < 5 {
        return None;
    }
    let (_, length, level) = ch.runs(n);
    let k = length.len();
    let (mut hi, mut lo) = (vec![], vec![]);
    for i in 1..k - 1 {
        if level[i] == 1 {
            hi.push(length[i] as f64)
        } else {
            lo.push(length[i] as f64)
        }
    }
    if hi.is_empty() || lo.is_empty() {
        return None;
    }
    Some(median(&hi).min(median(&lo)))
}

// ---------------- SPI ----------------

/// A data line's votes for CPHA (`timing.spi_mode`).
#[derive(Debug, Clone)]
pub struct Votes {
    pub after_leading: i64,
    pub after_trailing: i64,
    pub before_first: i64,
    pub cpha: Option<u8>,
}

#[derive(Debug, Clone)]
pub struct SpiMode {
    pub windows: usize,
    pub cpol: Option<u8>,
    pub cpol_votes: Vec<u8>,
    pub lines: Vec<(String, Votes)>,
}

/// CPOL from SCK's level as CS becomes active (without CS: over its longest pause), CPHA per data line from the SCK
/// edge each change follows (`timing.spi_mode`).
pub fn spi_mode(clk: &Ch, data: &[&Ch], n: i64, cs: Option<&Ch>, cs_active: u8) -> SpiMode {
    let none = |w| SpiMode {
        windows: w,
        cpol: None,
        cpol_votes: vec![],
        lines: vec![],
    };
    let (wins, cpols): (Vec<(i64, i64)>, Vec<u8>) = match cs {
        Some(cs) => {
            let w = crate::coded::cs_windows(cs, n, cs_active);
            let cpols = w.iter().filter(|w| w.2).map(|w| clk.level_at(w.0)).collect();
            (w.into_iter().map(|w| (w.0, w.1)).collect(), cpols)
        }
        None => {
            let (_, ln, lvl) = clk.runs(n);
            if ln.len() < 2 {
                return none(0);
            }
            let (a, b) = if ln.len() > 2 { (1, ln.len() - 1) } else { (0, ln.len()) };
            let k = crate::checks::argmax(ln[a..b].iter().map(|&v| v as f64)) + a;
            (vec![(0, n)], vec![lvl[k]])
        }
    };
    if cpols.is_empty() {
        return none(wins.len());
    }
    // the commonest (max over the set {0, 1}: on a tie the one the set gives first, 0)
    let c0 = cpols.iter().filter(|&&v| v == 0).count();
    let c1 = cpols.len() - c0;
    let cpol = if c0 > 0 && c0 >= c1 { 0 } else { 1 };
    let ce = &clk.edges;
    let leading: Vec<bool> = (0..ce.len()).map(|i| clk.after(i) != cpol).collect();
    let mut lines = vec![];
    for ch in data {
        let (mut lead, mut trail, mut before) = (0i64, 0i64, 0i64);
        for &(a, b) in &wins {
            for &x in ch.edges.iter().filter(|&&x| x > a && x < b) {
                let j = ce.partition_point(|&c| c <= x) as isize - 1;
                if j >= 0 && ce[j as usize] > a {
                    if leading[j as usize] { lead += 1 } else { trail += 1 }
                } else {
                    before += 1;
                }
            }
        }
        let cpha = if lead != trail {
            Some(if lead > trail { 1 } else { 0 })
        } else if before > 0 && lead == 0 {
            Some(0)
        } else {
            None
        };
        lines.push((
            ch.name.clone(),
            Votes {
                after_leading: lead,
                after_trailing: trail,
                before_first: before,
                cpha,
            },
        ));
    }
    SpiMode {
        windows: wins.len(),
        cpol: Some(cpol),
        cpol_votes: cpols,
        lines,
    }
}
