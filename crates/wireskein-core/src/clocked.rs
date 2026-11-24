//! A clock and the lines keeping to it as data, each line sampled on the clock's edges (Python's survey groups and
//! `typed.sync_bits`, found here as SPI finds its lines): the readers of fixed framings on a clock (JTAG, SD / MMC,
//! I2S) read these groups. Each clock is taken in the ways SPI settles it (pulses out, bounces merged, the shortest
//! runs out, rebuilt from its period where it bounces), the other lines settled alike.

use crate::Candidate;
use crate::spi::{Clean, bounced, clock_of, width};
use crate::sync::{RING, keeping, spikes_out_where_slow};
use crate::view::{Channel, View};

pub const KEEPS_MIN: f64 = 0.3; // a line keeps to the clock this well at least to be one of its data lines
pub const ON_EDGE_SHARE: f64 = 0.1; // a line's changes on the sampling edge's tick, this share: read before it ...
pub const ON_EDGE_OVER: f64 = 3.0; // ... where it meets the other edge this many times less
pub const CLEANS: [&str; 5] = ["pulses", "bounces", "shortest", "rebuilt_refused", "rebuilt_followed"];

/// A data line of a clock, settled as the clock is.
pub struct Line<'a> {
    pub name: &'a str,
    pub initial: u8,
    pub edges: Vec<i64>,
    pub keeps: f64,
}

/// A clock settled one way (`how`: an index of CLEANS) and its data lines, best keeping first.
pub struct Group<'a> {
    pub clock: &'a str,
    pub how: usize,
    pub edges: Vec<i64>,
    pub rises: Vec<i64>,
    pub falls: Vec<i64>,
    pub half: f64,
    pub lines: Vec<Line<'a>>,
}

impl Group<'_> {
    /// A data line's bits at the clock's rises (`rise`) or falls.
    pub fn bits(&self, line: &Line, rise: bool) -> Vec<u8> {
        let (t, other) = if rise {
            (&self.rises, &self.falls)
        } else {
            (&self.falls, &self.rises)
        };
        sample(line.initial, &line.edges, t, other)
    }
}

/// A line's levels at the sampling ticks `t` (sorted): as it settles (ringing at a slow clock's edges taken out),
/// and where its changes fall on the sampling ticks themselves a share of the time (a driver clocked by that edge)
/// the level before them.
pub fn sample(initial: u8, edges: &[i64], t: &[i64], other: &[i64]) -> Vec<u8> {
    if t.is_empty() {
        return vec![];
    }
    let e = spikes_out_where_slow(edges, RING, t);
    let (a, b) = (t[0], t[t.len() - 1]);
    let inside: Vec<i64> = e.iter().copied().filter(|&x| x >= a && x <= b).collect();
    let on = |ts: &[i64]| inside.iter().filter(|&&x| ts.binary_search(&x).is_ok()).count() as f64;
    let left = if inside.is_empty() {
        false
    } else {
        let share = on(t) / inside.len() as f64;
        let oth = on(other) / inside.len() as f64;
        share >= ON_EDGE_SHARE && oth <= share / ON_EDGE_OVER
    };
    let mut j = 0usize;
    t.iter()
        .map(|&x| {
            while j < e.len() && (e[j] < x || (!left && e[j] == x)) {
                j += 1;
            }
            initial ^ (j as u8 & 1)
        })
        .collect()
}

/// The groups of a part: each line of 16 changes or more as a clock, settled each way, with the lines keeping to it
/// (changing no more than 1.5 times as often).
pub fn groups<'a>(view: &View<'a>) -> Vec<Group<'a>> {
    let act: Vec<&Channel<'a>> = view.channels.iter().filter(|c| c.edges.len() >= 4).collect();
    let debounce: Clean = crate::kernels::debounce;
    let cleans: [(Clean, usize); 5] = [
        (crate::kernels::deglitch, 0),
        (debounce, 1),
        (crate::spi::shortest, 2),
        (debounce, 3),
        (debounce, 4),
    ];
    let mut out = vec![];
    for (clean, how) in cleans {
        for clk in &act {
            if clk.edges.len() < 16 || (how >= 3 && !bounced(clk.edges)) {
                continue;
            }
            let Some(ck) = clock_of(clk, clean, how) else {
                continue;
            };
            let k = width(how, ck.half);
            if how == 1 && k as f64 > 0.6 * ck.half {
                continue;
            }
            let cc = Channel::new(clk.name, clk.initial, &ck.edges);
            let mut lines: Vec<Line<'a>> = act
                .iter()
                .filter(|d| d.name != clk.name)
                .filter_map(|d| {
                    let e = clean(d.edges, k);
                    if e.len() < 2 || e.len() as f64 > 1.5 * ck.edges.len() as f64 {
                        return None;
                    }
                    let keeps = keeping(&e, &cc);
                    (keeps >= KEEPS_MIN).then_some(Line {
                        name: d.name,
                        initial: d.initial,
                        edges: e,
                        keeps,
                    })
                })
                .collect();
            if lines.is_empty() {
                continue;
            }
            lines.sort_by(|a, b| b.keeps.partial_cmp(&a.keeps).unwrap());
            out.push(Group {
                clock: clk.name,
                how,
                edges: ck.edges,
                rises: ck.rises,
                falls: ck.falls,
                half: ck.half,
                lines,
            });
        }
    }
    out
}

/// A candidate kept where it reads its roles better than one kept before (the same reading in another view).
pub fn keep_best(best: &mut std::collections::BTreeMap<Vec<(String, String)>, Candidate>, c: Candidate) {
    let key: Vec<(String, String)> = c.roles.clone().into_iter().collect();
    if best.get(&key).is_none_or(|b| c.score > b.score) {
        best.insert(key, c);
    }
}

/// Bits as hex, MSB first, given LSB first as shifted (Python's `format(v, "x")`).
pub fn hex_lsb_first(bits: &[u8]) -> String {
    let mut nib: Vec<u8> = bits
        .chunks(4)
        .map(|c| c.iter().enumerate().fold(0u8, |a, (k, &b)| a | (b << k)))
        .collect();
    while nib.len() > 1 && *nib.last().unwrap() == 0 {
        nib.pop();
    }
    if nib.is_empty() {
        return "0".into();
    }
    nib.iter().rev().map(|n| format!("{n:x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_and_hex() {
        assert_eq!(hex_lsb_first(&[1, 0, 0, 0, 1]), "11");
        assert_eq!(hex_lsb_first(&[0, 0]), "0");
        assert_eq!(hex_lsb_first(&[]), "0");
        // a line changing between the sampling ticks
        assert_eq!(
            sample(0, &[15, 35], &[10, 20, 30, 40], &[5, 15, 25, 35]),
            vec![0, 1, 1, 0]
        );
    }
}
