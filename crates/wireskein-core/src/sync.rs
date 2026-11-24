//! How a line keeps to a clock as its data (the idea of Python's `survey.pair_relation`, measured more simply): its
//! changes inside the clock's periods gather at one phase of them (or two: a line both ends drive), and it changes
//! about once a period. A UART line beside a clock changes at any phase.

use crate::view::Channel;

pub const DATA_PER_PERIOD: usize = 4;
pub const DATA_DENSE: f64 = 1.5;
pub const DATA_DENSE_SPREAD: f64 = 0.9;
pub const DATA_FASTER: f64 = 0.25;
pub const DATA_DENSE_PERIODS: usize = 16;
pub const RING: i64 = 3;

/// The clock's period around a tick: the shorter of the lower quartiles of the 8 intervals of `ticks` before the one
/// it falls in and the 8 after (`kernels.local_period`). `iv`: the intervals of `ticks`.
fn local_period(ticks: &[i64], iv: &[f64], at: i64) -> f64 {
    if iv.is_empty() {
        return 0.0;
    }
    let j = ticks.partition_point(|&t| t <= at) as isize - 1;
    let q = |a: isize, b: isize| -> f64 {
        let (a, b) = (a.max(0) as usize, (b.max(0) as usize).min(iv.len()));
        if a >= b {
            return f64::INFINITY;
        }
        let mut v = iv[a..b].to_vec();
        v.sort_by(|x, y| x.partial_cmp(y).unwrap());
        crate::kernels::quantile(&v, 0.25)
    };
    let p = q(j - 8, j.max(0)).min(q(j + 1, j + 9));
    if p.is_finite() {
        p
    } else {
        iv.iter().copied().fold(f64::INFINITY, f64::min)
    }
}

/// Pulses shorter than `k` taken out where the clock's period around is 3 k or more (`kernels.deglitch_slow`): a
/// reflection ringing at the clock's edges, crosstalk from it, where no bit is that short.
pub fn spikes_out_where_slow(e: &[i64], k: i64, clock_rises: &[i64]) -> Vec<i64> {
    if e.len() < 2 {
        return e.to_vec();
    }
    let iv: Vec<f64> = clock_rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let mut allow = vec![false; e.len()];
    for i in 1..e.len() {
        if e[i] - e[i - 1] < k {
            allow[i] = local_period(clock_rises, &iv, e[i]) >= 3.0 * k as f64;
        }
    }
    crate::kernels::deglitch_allow(e, k, Some(&allow))
}

/// The edges of a clock after which it is at `level`.
pub fn edges_to(ch: &Channel, level: u8) -> Vec<i64> {
    ch.edges
        .iter()
        .enumerate()
        .filter(|&(k, _)| ch.initial ^ (((k + 1) & 1) as u8) == level)
        .map(|(_, &x)| x)
        .collect()
}

/// The phase of each x in its surrounding ref interval, gathered (`kernels.relative_phase_concentration`).
pub fn phase_concentration(x: &[i64], r: &[i64]) -> f64 {
    if r.len() < 3 || x.len() < 2 {
        return 0.0;
    }
    let iv: Vec<f64> = r.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let (mut c1, mut s1, mut c2, mut s2, mut n) = (0.0, 0.0, 0.0, 0.0, 0usize);
    for &v in x {
        let j = r.partition_point(|&e| e <= v);
        if j == 0 || j >= r.len() {
            continue;
        }
        let k = j - 1;
        let prev = if k > 0 { iv[k - 1] } else { f64::INFINITY };
        let nxt = if k + 1 < iv.len() { iv[k + 1] } else { f64::INFINITY };
        if iv[k] > 3.0 * prev.min(nxt) {
            continue; // a gap
        }
        let ph = 2.0 * std::f64::consts::PI * (v - r[k]) as f64 / iv[k];
        let (s, c) = ph.sin_cos();
        c1 += c;
        s1 += s;
        c2 += c * c - s * s;
        s2 += 2.0 * c * s;
        n += 1;
    }
    if n < 2 {
        return 0.0;
    }
    let n = n as f64;
    ((c1 / n).hypot(s1 / n)).max((c2 / n).hypot(s2 / n))
}

/// How the line keeps to the clock as data, 0..1: the gathering of its changes at the clock's rises or falls, none
/// for a line crowding a period with its changes (another, faster line) or changing in most periods more than once
/// or twice (a pulse train of its own).
pub fn keeping(line: &[i64], clock: &Channel) -> f64 {
    let rise = edges_to(clock, 1);
    let fall = edges_to(clock, 0);
    // as the line settles: ringing and crosstalk at the clock's edges taken out where the clock is slow
    let settled = spikes_out_where_slow(&spikes_out_where_slow(line, RING, &rise), 2, &rise);
    let line: &[i64] = &settled;
    if rise.len() < 3 || line.len() < 2 {
        return 0.0;
    }
    let per_len: Vec<f64> = rise.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let med = crate::kernels::median(&per_len);
    // changes in each period of the clock's own length
    let mut per = vec![0usize; rise.len() + 1];
    let mut inside = 0usize;
    for &v in line {
        let j = rise.partition_point(|&e| e < v);
        if j > 0 && j < rise.len() && per_len[j - 1] <= 2.0 * med {
            per[j] += 1;
            inside += 1;
        }
    }
    if inside == 0 {
        return 0.0;
    }
    let crowded = per.iter().filter(|&&p| p > DATA_PER_PERIOD).sum::<usize>() as f64 / line.len() as f64;
    let active: Vec<usize> = per[..rise.len()].iter().copied().filter(|&p| p > 0).collect();
    let periods = per_len.iter().filter(|&&p| p <= 2.0 * med).count();
    if active.len() >= DATA_DENSE_PERIODS {
        let mean = active.iter().sum::<usize>() as f64 / active.len() as f64;
        let faster = active.iter().filter(|&&p| p >= 3).count() as f64 / active.len() as f64;
        if (mean > DATA_DENSE && active.len() as f64 >= DATA_DENSE_SPREAD * periods as f64) || faster >= DATA_FASTER {
            return 0.0;
        }
    }
    // only the changes inside the clock's periods count, weighed by their share of the line's (a line changing
    // mostly where the clock rests is no data of it) and by how many they are (a few gather by chance)
    let within: Vec<i64> = line
        .iter()
        .copied()
        .filter(|&v| {
            let j = rise.partition_point(|&e| e < v);
            j > 0 && j < rise.len() && per_len[j - 1] <= 2.0 * med
        })
        .collect();
    let co = inside as f64 / line.len() as f64;
    let quantity = 1.0 - (-(inside as f64) / 8.0).exp();
    let pr = phase_concentration(&within, &rise);
    let pf = phase_concentration(&within, &fall);
    co * quantity * pr.max(pf) * (1.0 - crowded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_keeps_and_a_uart_does_not() {
        // a clock of period 10, a data line changing at some of its falls, a line at its own pace
        let clk: Vec<i64> = (1..400).map(|i| i * 5).collect();
        let data: Vec<i64> = (1..195)
            .filter(|i| i % 4 == 1 || i % 7 == 2)
            .map(|i| i * 10 + 5)
            .collect();
        let lone: Vec<i64> = (1..120).map(|i| i * 17 + (i * i) % 13).collect();
        let c = Channel::new("C", 0, &clk);
        assert!(keeping(&data, &c) > 0.9, "{}", keeping(&data, &c));
        assert!(keeping(&lone, &c) < 0.3, "{}", keeping(&lone, &c));
    }
}
