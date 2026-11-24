//! The states a logic line goes through, and where it changes (Python's `wireskein.states`): each line split into
//! stretches at a level and stretches of a periodic wave, with the pulses between them that belong to neither.
//!
//! Each stretch, as Python gives it (its keys in Python's order): a level `{"t0", "t1", "level"}`, a wave `{"t0",
//! "t1", "period_s", "high_s", "freq_hz", "duty", "periods"}`, an odd pulse `{"odd", "high_s", "period_s", "end"}`,
//! more than `max_odd` odd pulses in a row `{"t0", "t1", "activity"}`.

use wireskein_io::j::J;
use wireskein_io::jd;

/// The options (Python's defaults: tol 0.02, min_periods 3, max_odd 3).
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub tol: f64,
    pub min_periods: usize,
    /// None: each odd pulse kept
    pub max_odd: Option<usize>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            tol: 0.02,
            min_periods: 3,
            max_odd: Some(3),
        }
    }
}

/// numpy's median (the mean of the two middle values of an even count).
pub fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        (v[m - 1] + v[m]) / 2.0
    }
}

/// Every whole pulse of a line: (rise, high, period to the next rise) in ticks (Python's `pulse_rows`).
pub fn pulse_rows(edges: &[i64], initial: u8) -> Vec<[i64; 3]> {
    let (rises, falls): (Vec<i64>, Vec<i64>) = {
        let even: Vec<i64> = edges.iter().step_by(2).copied().collect();
        let odd: Vec<i64> = edges.iter().skip(1).step_by(2).copied().collect();
        if initial == 0 { (even, odd) } else { (odd, even) }
    };
    if rises.len() < 2 {
        return vec![];
    }
    let mut out = vec![];
    for k in 0..rises.len() - 1 {
        let r = rises[k];
        let j = falls.partition_point(|&f| f < r);
        if j < falls.len() {
            out.push([r, falls[j] - r, rises[k + 1] - r]);
        }
    }
    out
}

enum S {
    Level {
        t0: i64,
        t1: i64,
        level: u8,
    },
    Wave {
        t0: i64,
        t1: i64,
        period_s: f64,
        high_s: f64,
        freq_hz: f64,
        duty: f64,
        periods: usize,
    },
    Odd {
        odd: i64,
        high_s: f64,
        period_s: Option<f64>,
        end: i64,
    },
    Activity {
        t0: i64,
        t1: i64,
        activity: usize,
    },
}

impl S {
    fn key(&self) -> i64 {
        match self {
            S::Level { t0, .. } | S::Wave { t0, .. } | S::Activity { t0, .. } => *t0,
            S::Odd { odd, .. } => *odd,
        }
    }

    fn to_j(&self) -> J {
        match *self {
            S::Level { t0, t1, level } => jd! {"t0" => t0, "t1" => t1, "level" => level as i64},
            S::Wave {
                t0,
                t1,
                period_s,
                high_s,
                freq_hz,
                duty,
                periods,
            } => {
                jd! {"t0" => t0, "t1" => t1, "period_s" => period_s, "high_s" => high_s, "freq_hz" => freq_hz,
                "duty" => duty, "periods" => periods}
            }
            S::Odd {
                odd,
                high_s,
                period_s,
                end,
            } => {
                jd! {"odd" => odd, "high_s" => high_s, "period_s" => period_s, "end" => end}
            }
            S::Activity { t0, t1, activity } => jd! {"t0" => t0, "t1" => t1, "activity" => activity},
        }
    }
}

/// The level of a line just after the changes at or before `t` (`side`: right) or before `t` (left).
fn level_by(edges: &[i64], initial: u8, t: i64, right: bool) -> u8 {
    let k = if right {
        edges.partition_point(|&x| x <= t)
    } else {
        edges.partition_point(|&x| x < t)
    };
    initial ^ (k & 1) as u8
}

/// The stretches of one line over ticks [0, n), in order (Python's `states.of_line`).
pub fn of_line(edges: &[i64], initial: u8, n: i64, rate: f64, step: i64, o: &Options) -> J {
    J::List(
        stretches(edges, initial, n, rate, step, o)
            .iter()
            .map(S::to_j)
            .collect(),
    )
}

fn stretches(edges: &[i64], initial: u8, n: i64, rate: f64, step: i64, o: &Options) -> Vec<S> {
    if edges.is_empty() {
        return vec![S::Level {
            t0: 0,
            t1: n,
            level: initial,
        }];
    }
    let tol = o.tol;
    let rows = pulse_rows(edges, initial);
    let slack = (2 * (step + 1)) as f64;
    let alike = |a: &[i64; 3], b: &[i64; 3]| {
        ((a[2] - b[2]).abs() as f64) <= slack.max(tol * b[2] as f64)
            && ((a[1] - b[1]).abs() as f64) <= slack.max(tol * b[1] as f64)
    };
    let mut runs = vec![];
    let mut i = 0;
    while i < rows.len() {
        let mut k = i + 1;
        while k < rows.len() && alike(&rows[k], &rows[i]) {
            k += 1;
        }
        runs.push((i, k));
        i = k;
    }
    let waves: Vec<(usize, usize)> = runs.into_iter().filter(|(a, b)| b - a >= o.min_periods).collect();
    let near = waves
        .iter()
        .map(|&(a, b)| rows[a..b].iter().map(|r| r[2]).max().unwrap())
        .max();
    let mut out: Vec<S> = vec![];

    // ticks [t0, t1) outside the waves: quiet longer than the waves' periods (with no wave, than 10 times the
    // edges' spacing here) is a level; a shorter high is a pulse, odd unless it is the wave before it ending whole
    let between = |out: &mut Vec<S>, t0: i64, t1: i64, before: Option<usize>, t1_is_rise: bool| {
        let a0 = edges.partition_point(|&x| x < t0);
        let b0 = edges.partition_point(|&x| x < t1);
        let e = &edges[a0..b0];
        let mut marks = Vec::with_capacity(e.len() + 2);
        marks.push(t0);
        marks.extend_from_slice(e);
        marks.push(t1);
        let floor: f64 = match near {
            Some(m) => m as f64,
            None if marks.len() - 1 > 2 => {
                let mut g: Vec<f64> = marks.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
                10.0 * median(&mut g)
            }
            None => 0.0,
        };
        let lv0 = level_by(edges, initial, t0, false);
        let at_edge0 = !e.is_empty() && e[0] == t0;
        for i in 0..marks.len() - 1 {
            let (a, b) = (marks[i], marks[i + 1]);
            if b <= a {
                continue;
            }
            let from_edge = i > 0 || at_edge0;
            let here = if from_edge {
                level_by(edges, initial, a, true)
            } else {
                lv0
            };
            if (b - a) as f64 > floor {
                out.push(S::Level {
                    t0: a,
                    t1: b,
                    level: here,
                });
            } else if here == 1 && from_edge {
                let c = marks.get(i + 2).copied();
                // the next rise: an edge here, or the next wave's first rise at t1 (not the end of the capture)
                let real = c.is_some_and(|c| c < t1 || t1_is_rise);
                let per = c.filter(|&c| real && ((c - b) as f64) <= floor).map(|c| c - a);
                if let Some(w) = before
                    && per.is_none()
                    && let S::Wave { t1: wt1, high_s, .. } = &mut out[w]
                    && a == *wt1
                {
                    // (as Python's floats: tol * high_s * rate from the left)
                    if ((b - a) as f64 - *high_s * rate).abs() <= slack.max(tol * *high_s * rate) {
                        *wt1 = b; // the wave's last pulse, whole: the wave ends here
                        continue;
                    }
                }
                let per = per.filter(|&p| p != 0);
                out.push(S::Odd {
                    odd: a,
                    high_s: (b - a) as f64 / rate,
                    period_s: per.map(|p| p as f64 / rate),
                    end: if per.is_some() { c.unwrap() } else { b },
                });
            }
        }
    };

    let (mut t, mut prev) = (0i64, None);
    for &(a, b) in &waves {
        let w0 = rows[a][0];
        between(&mut out, t, w0, prev, true);
        let rs = &rows[a..b];
        let mut per: Vec<f64> = rs.iter().map(|r| r[2] as f64).collect();
        let mut hi: Vec<f64> = rs.iter().map(|r| r[1] as f64).collect();
        let mut duty: Vec<f64> = rs.iter().map(|r| r[1] as f64 / r[2] as f64).collect();
        let (mp, mh) = (median(&mut per), median(&mut hi));
        let t1 = rows[b - 1][0] + rows[b - 1][2];
        out.push(S::Wave {
            t0: w0,
            t1,
            period_s: mp / rate,
            high_s: mh / rate,
            freq_hz: rate / mp,
            duty: median(&mut duty),
            periods: b - a,
        });
        prev = Some(out.len() - 1);
        t = t1;
    }
    between(&mut out, t, n, prev, false);
    out.sort_by_key(S::key); // stable, as Python's sort
    let merged = merge_levels(out);
    match o.max_odd {
        None => merged,
        Some(m) => collapse(merged, m),
    }
}

fn merge_levels(out: Vec<S>) -> Vec<S> {
    let mut merged: Vec<S> = vec![];
    for x in out {
        if let (Some(S::Level { t1: pt1, level: pl, .. }), S::Level { t0, t1, level }) = (merged.last_mut(), &x)
            && *pl == *level
            && *pt1 == *t0
        {
            *pt1 = *t1;
            continue;
        }
        merged.push(x);
    }
    merged
}

/// More than max_odd odd pulses in a row (a bus, a burst: not a wave) as one stretch of activity.
fn collapse(out: Vec<S>, max_odd: usize) -> Vec<S> {
    let mut res = vec![];
    let mut it = out.into_iter().peekable();
    while it.peek().is_some() {
        let mut odds = vec![];
        while let Some(S::Odd { .. }) = it.peek() {
            odds.push(it.next().unwrap());
        }
        if odds.len() > max_odd {
            let t0 = odds[0].key();
            let t1 = match odds.last() {
                Some(S::Odd { end, .. }) => *end,
                _ => unreachable!(),
            };
            res.push(S::Activity {
                t0,
                t1,
                activity: odds.len(),
            });
        } else if !odds.is_empty() {
            res.extend(odds);
        } else {
            res.push(it.next().unwrap());
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clock_a_level_and_odd_pulses() {
        // quiet low, a square wave of period 10 (high 5) for 20 periods, quiet, two odd pulses
        let mut e = vec![];
        for k in 0..20 {
            e.push(1000 + 10 * k);
            e.push(1005 + 10 * k);
        }
        e.extend([5000, 5003, 9000, 9001]);
        // as Python's states.of_line gives it
        let want = r#"[{"t0":0,"t1":1000,"level":0},{"t0":1000,"t1":1195,"period_s":1e-05,"high_s":5e-06,"freq_hz":100000.0,"duty":0.5,"periods":19},{"t0":1195,"t1":5000,"level":0},{"odd":5000,"high_s":3e-06,"period_s":null,"end":5003},{"t0":5003,"t1":9000,"level":0},{"odd":9000,"high_s":1e-06,"period_s":null,"end":9001},{"t0":9001,"t1":20000,"level":0}]"#;
        assert_eq!(of_line(&e, 0, 20000, 1e6, 1, &Options::default()).dumps(), want);
        // a line that never moves
        assert_eq!(
            of_line(&[], 1, 50, 1e6, 1, &Options::default()).dumps(),
            r#"[{"t0":0,"t1":50,"level":1}]"#
        );
    }
}
