//! SPI (R2 of bench/notes/2026-11-24-rust-plan.ja.md), after the idea of Python's declared SPI (`decl/spi.toml`) and
//! the bench's light reader: a clock, a select or none (the clock's bursts then), one or two data lines keeping to the
//! clock; each window's bits sampled on the edge away from where the data changes, MSB first. The evidence is windows
//! of whole bytes, the clock's edges inside the windows, the data changing inside them once a bit, data that is no
//! one value, and how many windows.

use crate::Candidate;
use crate::kernels::{deglitch, median};
use crate::sync::{edges_to, keeping};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const CLEAN: f64 = 0.3; // a run under this share of the clock's half period is noise
pub const DATA_KEEPS: f64 = 0.5; // a data line keeps to the clock this well at least
pub const SCALE: f64 = 5.0; // the words of whole-byte windows counted as 1 - exp(-n / SCALE)
pub const CS_FEWER: usize = 8; // a select changes this many times less than the clock at least

struct Clock {
    edges: Vec<i64>,
    rises: Vec<i64>,
    falls: Vec<i64>,
    half: f64,
    initial: u8,
}

/// How a line is made to settle: pulses taken out (spikes, crosstalk) or bounces merged at their first change.
type Clean = fn(&[i64], i64) -> Vec<i64>;

pub const BOUNCE_MERGE_WIDE: i64 = 8;

/// How wide a cleaning is at a clock of this half period: CLEAN of it; bounces merged over 8 samples at least where
/// the half period is long enough to keep its edges (a bounced clock's own median interval is a bounce's)
fn width(how: usize, half: f64) -> i64 {
    match how {
        1 => ((CLEAN * half) as i64).max(2).max(BOUNCE_MERGE_WIDE),
        2 => ((SHORTEST * half).round() as i64).max(2),
        _ => ((CLEAN * half) as i64).max(2),
    }
}

/// The runs shorter than k taken out, the shortest first, each with its own two changes (`views.pulses_out`): a
/// fast clock's bounces, too wide for a pulse filter of a third of its half and too near its edges to merge.
fn shortest(e: &[i64], k: i64) -> Vec<i64> {
    crate::kernels::shortest_first(e, k)
}

pub const SHORTEST: f64 = 0.6;

fn clock_of(ch: &Channel, clean: Clean, how: usize) -> Option<Clock> {
    if ch.edges.len() < 8 {
        return None;
    }
    let d: Vec<f64> = ch.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let half = median(&d);
    let c = clean(ch.edges, width(how, half));
    if c.len() < 8 {
        return None;
    }
    let d: Vec<f64> = c.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let half = median(&d);
    let cc = Channel::new(ch.name, ch.initial, &c);
    let (rises, falls) = (edges_to(&cc, 1), edges_to(&cc, 0));
    Some(Clock {
        edges: c,
        rises,
        falls,
        half,
        initial: ch.initial,
    })
}

/// The windows of the transfers: a select's level holding most of the clock's changes, or the clock's bursts.
/// The windows (start, end), and the select's active level where there is one.
type Windows = (Vec<(i64, i64)>, Option<u8>);

fn windows(ck: &Clock, cs: Option<&Channel>, n: i64) -> Option<Windows> {
    let ce = &ck.edges;
    if let Some(cs) = cs {
        let ke = deglitch(cs.edges, (2.0 * ck.half) as i64);
        if ke.len() < 2 {
            return None;
        }
        let mut bounds = vec![0];
        bounds.extend(&ke);
        bounds.push(n);
        let mut cnt = [0usize; 2];
        for i in 0..bounds.len() - 1 {
            let lv = (cs.initial as usize + i) & 1;
            cnt[lv] += ce.partition_point(|&x| x < bounds[i + 1]) - ce.partition_point(|&x| x < bounds[i]);
        }
        let act = if cnt[1] > cnt[0] { 1 } else { 0 };
        let w = (0..bounds.len() - 1)
            .filter(|&i| (cs.initial as usize + i) & 1 == act)
            .map(|i| (bounds[i], bounds[i + 1]))
            .collect();
        return Some((w, Some(act as u8)));
    }
    let gaps: Vec<f64> = ce.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let m = median(&gaps);
    let mut w = vec![];
    let mut a = ce[0];
    for i in 0..gaps.len() {
        if gaps[i] > 4.0 * m {
            w.push((a, ce[i] + 1));
            a = ce[i + 1];
        }
    }
    w.push((a, ce[ce.len() - 1] + 1));
    Some((w, None))
}

fn level_at(e: &[i64], initial: u8, t: i64) -> u8 {
    initial ^ (e.partition_point(|&x| x <= t) as u8 & 1)
}

/// The SPI reading of (clk, cs, data lines), or None.
fn read(view: &View, clk: &Channel, ck: &Clock, cs: Option<&Channel>, datas: &[&Channel]) -> Option<Candidate> {
    let n = view.n_samples;
    let (w, active) = windows(ck, cs, n)?;
    if w.is_empty() {
        return None;
    }
    let des: Vec<Vec<i64>> = datas
        .iter()
        .map(|d| deglitch(d.edges, (CLEAN * ck.half) as i64))
        .collect();
    // the sampling edge: the one farther from where the data changes
    let dist = |t: &[i64]| -> f64 {
        let mut v = vec![];
        for de in &des {
            if de.is_empty() || t.len() < 2 {
                continue;
            }
            let ds: Vec<f64> = de
                .iter()
                .map(|&x| {
                    let j = t.partition_point(|&y| y < x).clamp(1, t.len() - 1);
                    (x - t[j]).abs().min((x - t[j - 1]).abs()) as f64
                })
                .collect();
            v.push(median(&ds));
        }
        if v.is_empty() {
            f64::NAN
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let (dr, df) = (dist(&ck.rises), dist(&ck.falls));
    if dr.is_nan() {
        return None;
    }
    let rise = dr >= df;
    let take = if rise { &ck.rises } else { &ck.falls };
    let cpol = if w[0].0 > 0 {
        level_at(&ck.edges, ck.initial, w[0].0 - 1)
    } else {
        ck.initial
    };
    let leading_rise = cpol == 0;
    let mode = cpol as i64 * 2 + if rise == leading_rise { 0 } else { 1 };
    let mut transfers = vec![];
    let mut counts = vec![];
    let mut values = vec![];
    for &(a, b) in &w {
        let lo = take.partition_point(|&x| x < a);
        let hi = take.partition_point(|&x| x < b);
        if lo == hi {
            continue;
        }
        let t = &take[lo..hi];
        counts.push(t.len());
        let mut tr = Map::new();
        tr.insert("s".into(), json!(a));
        tr.insert("e".into(), json!(b));
        for (d, de) in datas.iter().zip(&des) {
            let bits: Vec<u8> = t.iter().map(|&x| level_at(de, d.initial, x)).collect();
            let bytes: Vec<u8> = bits
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| c.iter().fold(0u8, |acc, &b| (acc << 1) | b))
                .collect();
            values.extend(bytes.iter().map(|&b| b as i64));
            tr.insert(
                d.name.to_string(),
                json!(bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()),
            );
        }
        transfers.push(Value::Object(tr));
    }
    if counts.len() < 2 {
        return None;
    }
    let whole = counts.iter().filter(|&&c| c % 8 == 0).count() as f64 / counts.len() as f64;
    let covered = counts.iter().sum::<usize>() as f64 / take.len().max(1) as f64;
    // the data's changes inside the windows, and at most one a clock period there
    let mut inside = vec![];
    let mut once = vec![];
    for de in &des {
        if de.len() < 2 {
            continue;
        }
        let mut k = 0usize;
        let mut per_period = std::collections::HashMap::new();
        for &x in de {
            let i = w.partition_point(|&(a, _)| a <= x);
            if i > 0 && x < w[i - 1].1 {
                k += 1;
                let p = take.partition_point(|&y| y <= x);
                *per_period.entry(p).or_insert(0usize) += 1;
            }
        }
        inside.push(k as f64 / de.len() as f64);
        if !per_period.is_empty() {
            once.push(per_period.values().filter(|&&c| c <= 1).count() as f64 / per_period.len() as f64);
        }
    }
    if inside.is_empty() {
        return None;
    }
    let inside = inside.iter().sum::<f64>() / inside.len() as f64;
    let once = if once.is_empty() {
        1.0
    } else {
        once.iter().sum::<f64>() / once.len() as f64
    };
    let degenerate = {
        let mut c = std::collections::HashMap::new();
        for &v in &values {
            *c.entry(v).or_insert(0usize) += 1;
        }
        let top = c.values().max().copied().unwrap_or(0) as f64 / values.len().max(1) as f64;
        if values.len() < 4 || top <= 0.5 {
            1.0
        } else {
            (1.0 - (top - 0.5) / 0.5).max(0.05)
        }
    };
    // as many words as the windows of whole bytes hold (a line's words: Python's q(words, 5))
    let words: usize = counts.iter().filter(|&&c| c % 8 == 0).map(|&c| c / 8).sum();
    let quantity = 1.0 - (-(words as f64) / SCALE).exp();
    // a select frames every burst of the clock (one outside its windows is another target's, or it is no select),
    // its windows hold the clock (a window with none is another target's selection) snugly (the margins before and
    // after the clock's span no longer than the span)
    let (framed, used, snug) = if cs.is_some() {
        let bs = crate::sync::bursts(&ck.edges);
        let inside_w = bs
            .iter()
            .filter(|&&(a, b)| {
                let i = w.partition_point(|&(s, _)| s <= a);
                i > 0 && b < w[i - 1].1
            })
            .count();
        let (mut held, mut tight) = (0usize, 0usize);
        for &(a, b) in &w {
            let lo = ck.edges.partition_point(|&x| x < a);
            let hi = ck.edges.partition_point(|&x| x < b);
            if hi > lo {
                held += 1;
                let (f, l) = (ck.edges[lo], ck.edges[hi - 1]);
                if (f - a) + (b - l) <= (l - f).max(1) + (4.0 * ck.half) as i64 {
                    tight += 1;
                }
            }
        }
        (
            inside_w as f64 / bs.len().max(1) as f64,
            held as f64 / w.len().max(1) as f64,
            tight as f64 / held.max(1) as f64,
        )
    } else {
        (1.0, 1.0, 1.0)
    };
    let score = whole
        * (covered / 0.9).min(1.0)
        * framed
        * framed
        * (0.5 + 0.5 * used)
        * (0.5 + 0.5 * snug)
        * inside
        * (0.75 + 0.25 * once)
        * (0.5 + 0.5 * degenerate)
        * quantity;
    let mut roles: std::collections::BTreeMap<String, String> = Default::default();
    roles.insert("clk".into(), clk.name.into());
    if let Some(cs) = cs {
        roles.insert("cs".into(), cs.name.into());
    }
    for (i, d) in datas.iter().enumerate() {
        roles.insert(format!("data{i}"), d.name.into());
    }
    let mut params = Map::new();
    params.insert("bit_order".into(), json!("msb"));
    params.insert("sample_edge".into(), json!(if rise { "rise" } else { "fall" }));
    if let Some(a) = active {
        params.insert("cs_active".into(), json!(a));
    }
    let mut evidence = Map::new();
    evidence.insert("mode".into(), json!(mode));
    for (k, x) in [
        ("whole_bytes", whole),
        ("covered", covered),
        ("inside", inside),
        ("data_once_a_bit", once),
        ("degenerate", degenerate),
        ("select_frames", framed),
        ("select_used", used),
        ("select_snug", snug),
    ] {
        evidence.insert(k.into(), json!(x));
    }
    evidence.insert("windows".into(), json!(counts.len()));
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "transfers": transfers })).unwrap());
    Some(Candidate {
        protocol: "spi".into(),
        roles,
        score,
        alt: None,
        params,
        evidence,
        layers,
    })
}

/// The SPI readings of a part: each clock with no select or each line changing far less, and the one or two lines
/// keeping to it best as data (each read alone and the two together).
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut best: std::collections::BTreeMap<Vec<(String, String)>, Candidate> = Default::default();
    let act: Vec<&Channel> = view.channels.iter().filter(|c| c.edges.len() >= 4).collect();
    let cleans: [(Clean, usize); 3] = [(deglitch, 0), (crate::kernels::debounce, 1), (shortest, 2)];
    for (clean, how) in cleans {
        for clk in &act {
            let Some(ck) = clock_of(clk, clean, how) else {
                continue;
            };
            // the other lines as they settle at this clock's pace: what is counted, kept to and read
            let k = width(how, ck.half);
            if how == 1 && k as f64 > 0.6 * ck.half {
                continue; // a clock too fast to merge its bounces without its own edges
            }
            let settled: Vec<Vec<i64>> = act.iter().map(|c| clean(c.edges, k)).collect();
            let lines: Vec<Channel> = act
                .iter()
                .zip(&settled)
                .map(|(c, e)| Channel::new(c.name, c.initial, e))
                .collect();
            let cc = Channel::new(clk.name, clk.initial, &ck.edges);
            let mut keep: Vec<(f64, &Channel)> = lines
                .iter()
                .filter(|d| d.name != clk.name && d.edges.len() as f64 <= 1.5 * ck.edges.len() as f64)
                .map(|d| (keeping(d.edges, &cc), d))
                .filter(|&(k, _)| k >= DATA_KEEPS)
                .collect();
            if keep.is_empty() {
                continue;
            }
            keep.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            let selects: Vec<Option<&Channel>> = std::iter::once(None)
                .chain(
                    lines
                        .iter()
                        .filter(|c| {
                            c.name != clk.name && c.edges.len() >= 2 && c.edges.len() * CS_FEWER <= ck.edges.len()
                        })
                        .map(Some),
                )
                .collect();
            for cs in selects {
                let ds: Vec<&Channel> = keep
                    .iter()
                    .map(|&(_, d)| d)
                    .filter(|d| cs.is_none_or(|c| c.name != d.name))
                    .take(2)
                    .collect();
                let mut sets: Vec<Vec<&Channel>> = ds.iter().map(|d| vec![*d]).collect();
                if ds.len() == 2 {
                    sets.push(ds.clone());
                }
                for set in sets {
                    if let Some(mut c) = read(view, clk, &ck, cs, &set) {
                        c.params
                            .insert("clean".into(), json!(["pulses", "bounces", "shortest"][how]));
                        let key: Vec<(String, String)> = c.roles.clone().into_iter().collect();
                        if best.get(&key).is_none_or(|b| c.score > b.score) {
                            best.insert(key, c);
                        }
                    }
                }
            }
        }
    }
    best.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_mode0_with_a_select() {
        // 4 transfers of 2 bytes, mode 0: data set on falls, sampled on rises; CS low around each
        let (mut clk, mut mosi, mut cs) = (vec![], vec![], vec![]);
        let (mut ld, mut t) = (0u8, 100i64);
        let msgs: [[u8; 2]; 4] = [[0x9f, 0x00], [0x03, 0x10], [0xa5, 0x5a], [0x01, 0x80]];
        for m in msgs {
            cs.push(t);
            t += 10;
            for b in m {
                for k in 0..8 {
                    let x = (b >> (7 - k)) & 1;
                    if x != ld {
                        mosi.push(t);
                        ld = x;
                    }
                    t += 5;
                    clk.push(t);
                    t += 5;
                    clk.push(t);
                }
            }
            t += 10;
            cs.push(t);
            t += 50;
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![
                Channel::new("SCK", 0, &clk),
                Channel::new("MOSI", 0, &mosi),
                Channel::new("CS", 1, &cs),
            ],
        )
        .unwrap();
        let cs_ = candidates(&v);
        let c = cs_
            .iter()
            .filter(|c| c.roles.contains_key("cs"))
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap());
        let c = c.expect("an SPI reading with its select");
        assert_eq!(c.evidence["mode"], json!(0));
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["transfers"][2]["MOSI"], json!("a55a"), "{lay}");
        assert!(c.score > 0.5, "{:?}", c.evidence);
    }
}
