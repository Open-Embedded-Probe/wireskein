//! How a line keeps to a clock as its data (the idea of Python's `survey.pair_relation`, measured more simply): its
//! changes inside the clock's periods gather at one phase of them (or two: a line both ends drive), and it changes
//! about once a period. A UART line beside a clock changes at any phase.

use crate::view::Channel;

pub const DATA_PER_PERIOD: usize = 4;
pub const DATA_DENSE: f64 = 1.5;
pub const DATA_DENSE_SPREAD: f64 = 0.9;
pub const DATA_FASTER: f64 = 0.25;
pub const DATA_DENSE_PERIODS: usize = 16;

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
