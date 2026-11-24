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
pub const SELECT_USED: f64 = 0.5; // a select's windows holding the clock, this share for a whole score
pub const SINGLE_WINDOW: f64 = 0.5; // a select of a single window, standing alone, weighs this much
pub const SELECT_MIN_BITS: usize = 2; // sampling edges a select's window holds at least for a transfer

pub(crate) struct Clock {
    pub(crate) edges: Vec<i64>,
    pub(crate) rises: Vec<i64>,
    pub(crate) falls: Vec<i64>,
    pub(crate) half: f64,
    pub(crate) initial: u8,
}

/// How a line is made to settle: pulses taken out (spikes, crosstalk) or bounces merged at their first change.
pub(crate) type Clean = fn(&[i64], i64) -> Vec<i64>;

pub const BOUNCE_MERGE_WIDE: i64 = 8;

/// How wide a cleaning is at a clock of this half period: CLEAN of it; bounces merged over 8 samples at least where
/// the half period is long enough to keep its edges (a bounced clock's own median interval is a bounce's)
pub(crate) fn width(how: usize, half: f64) -> i64 {
    match how {
        1 => ((CLEAN * half) as i64).max(2).max(BOUNCE_MERGE_WIDE),
        2 => ((SHORTEST * half).round() as i64).max(2),
        3 | 4 => REBUILD_DATA,
        AS_IS => 0,
        _ => ((CLEAN * half) as i64).max(2),
    }
}

/// The runs shorter than k taken out, the shortest first, each with its own two changes (`views.pulses_out`): a
/// fast clock's bounces, too wide for a pulse filter of a third of its half and too near its edges to merge.
pub(crate) fn shortest(e: &[i64], k: i64) -> Vec<i64> {
    crate::kernels::shortest_first(e, k)
}

pub const SHORTEST: f64 = 0.6;
/// The view of the lines as they are (Python's first view of a clocked group): a clock of a few samples a period has
/// runs of a sample of its own, which every cleaning takes out with the clock (`views.own_short_runs`)
pub const AS_IS: usize = 5;
pub const CLEAN_NAMES: [&str; 6] = [
    "pulses",
    "bounces",
    "shortest",
    "rebuilt_refused",
    "rebuilt_followed",
    "as_is",
];
pub const REBUILD_DATA: i64 = 4; // ticks: the data and select lines' bounces merged beside a rebuilt clock
pub const REBUILD_SHORT: f64 = 0.02; // a clock's runs half its median or less, this share or more ...
pub const REBUILD_FEW: usize = 4; // ... and this many at least: views with the clock rebuilt from its period

/// Whether a clock's short runs are a bounce's: enough of them, side by side (a spike's stand alone).
pub(crate) fn bounced(e: &[i64]) -> bool {
    let r: Vec<f64> = e.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    if r.len() < 16 {
        return false;
    }
    let m = median(&r);
    let short: Vec<bool> = r.iter().map(|&x| x <= 0.5 * m).collect();
    let n = short.iter().filter(|&&x| x).count();
    if n < REBUILD_FEW || (n as f64) < REBUILD_SHORT * r.len() as f64 {
        return false;
    }
    let s2: Vec<bool> = r.iter().map(|&x| x < 0.5 * m + 1.0).collect();
    let pairs = s2.windows(2).filter(|w| w[0] && w[1]).count() as f64;
    pairs * 2.0 >= 0.5 * s2.iter().filter(|&&x| x).count() as f64
}

pub(crate) fn clock_of(ch: &Channel, clean: Clean, how: usize) -> Option<Clock> {
    if ch.edges.len() < 8 {
        return None;
    }
    let d: Vec<f64> = ch.edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let half = median(&d);
    let c = match how {
        3 => crate::rebuild::refused(ch.edges, ch.initial),
        4 => crate::rebuild::followed(ch.edges, ch.initial),
        AS_IS => ch.edges.to_vec(),
        _ => clean(ch.edges, width(how, half)),
    };
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
        // the active level: the one whose windows hold the clock's bursts one by one, snugly (the margins before and
        // after its edges no longer than their span and a few half periods); where both do as well, the one holding
        // more of the clock (a select of a target among several: the others' transfers fall in its idle level)
        let mut cnt = [0usize; 2];
        let mut snug = [0usize; 2];
        for i in 0..bounds.len() - 1 {
            let lv = (cs.initial as usize + i) & 1;
            let (a, b) = (bounds[i], bounds[i + 1]);
            let lo = ce.partition_point(|&x| x < a);
            let hi = ce.partition_point(|&x| x < b);
            cnt[lv] += hi - lo;
            if hi > lo && i > 0 && i + 2 < bounds.len() {
                let (f, l) = (ce[lo], ce[hi - 1]);
                if (f - a) + (b - l) <= (l - f).max(1) + (4.0 * ck.half) as i64 {
                    snug[lv] += 1;
                }
            }
        }
        let act = if (snug[1], cnt[1]) > (snug[0], cnt[0]) { 1 } else { 0 };
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

pub const TRANSFER_EDGES: usize = 4; // a burst of the clock with this many changes at least is a transfer (two bits)

/// The clock's bursts that can be transfers: a lone pulse (a spike on a clock read as it is) holds no bit.
fn transfers_of(ce: &[i64]) -> Vec<(i64, i64)> {
    crate::sync::bursts(ce)
        .into_iter()
        .filter(|&(a, b)| ce.partition_point(|&x| x <= b) - ce.partition_point(|&x| x < a) >= TRANSFER_EDGES)
        .collect()
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
    let mut single = 0usize; // sampling edges in a select's windows of one
    let mut values = vec![];
    for &(a, b) in &w {
        let lo = take.partition_point(|&x| x < a);
        let hi = take.partition_point(|&x| x < b);
        // (a select's window around a single sampling edge is no transfer: a select toggling with the clock, as
        // lines float while a board powers down; Python's SELECT_MIN_BITS)
        if lo == hi {
            continue;
        }
        if cs.is_some() && hi - lo < SELECT_MIN_BITS {
            single += hi - lo;
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
    // (a select's single window is a target's one transfer: a member of a shared bus, its own reading weighed by
    // the select's framing; without a select, two bursts at least)
    if counts.len() < if cs.is_some() { 1 } else { 2 } {
        return None;
    }
    let whole = counts.iter().filter(|&&c| c % 8 == 0).count() as f64 / counts.len() as f64;
    let covered = counts.iter().sum::<usize>() as f64 / (take.len() - single).max(1) as f64;
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
    // as many words as the windows of whole bytes hold, on each data line (Python's q(words, 5), its words counted per
    // data pin: two lines read together are twice the evidence)
    let words: usize = counts.iter().filter(|&&c| c % 8 == 0).map(|&c| c / 8).sum::<usize>() * datas.len();
    let quantity = 1.0 - (-(words as f64) / SCALE).exp();
    // a select frames every burst of the clock (one outside its windows is another target's, or it is no select),
    // its windows hold the clock (a window with none is another target's selection) snugly (the margins before and
    // after the clock's span no longer than the span)
    let (framed, used, snug) = if cs.is_some() {
        let bs = transfers_of(&ck.edges);
        let inside_w = bs
            .iter()
            .filter(|&&(a, b)| {
                // every edge of the burst inside a window (one, or several of a select toggling with it)
                let lo = ck.edges.partition_point(|&x| x < a);
                let hi = ck.edges.partition_point(|&x| x <= b);
                ck.edges[lo..hi].iter().all(|&x| {
                    let i = w.partition_point(|&(s, _)| s <= x);
                    i > 0 && x < w[i - 1].1
                })
            })
            .count();
        let (mut held, mut tight) = (0usize, 0usize);
        for &(a, b) in &w {
            let lo = ck.edges.partition_point(|&x| x < a);
            let hi = ck.edges.partition_point(|&x| x < b);
            if hi > lo {
                held += 1;
                // (snug at its start: a select is made active just before its clock runs; it may be held on after
                // the transfers a while - a session's - so its end is not asked)
                let (f, l) = (ck.edges[lo], ck.edges[hi - 1]);
                if f - a <= (l - f).max(1) + (4.0 * ck.half) as i64 {
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
    // a select of one window alone tells little (any line changing once before the traffic and once after frames
    // it all): a target of a shared bus, where its window is one of the bus's (shared undoes it)
    let single = if cs.is_some() && counts.len() < 2 {
        SINGLE_WINDOW
    } else {
        1.0
    };
    let score = whole
        * (covered / 0.9).min(1.0)
        * framed
        * framed
        // (a select's windows: whole where half of them or more hold the clock, as Python's SELECT_USED: a
        // selection with no clock in it is a select's as well; snug round it at their start)
        * (used / SELECT_USED).min(1.0)
        * (0.5 + 0.5 * snug)
        * inside
        * (0.75 + 0.25 * once)
        * (0.5 + 0.5 * degenerate)
        * quantity
        * single;
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
        ("single_window", single),
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
    let once: Vec<Channel> = view
        .channels
        .iter()
        .filter(|c| (2..4).contains(&c.edges.len()))
        .copied()
        .collect();
    let debounce: Clean = crate::kernels::debounce;
    let cleans: [(Clean, usize); 6] = [
        (deglitch, 0),
        (debounce, 1),
        (shortest, 2),
        (debounce, 3),
        (debounce, 4),
        (deglitch, AS_IS),
    ];
    // every view of each clock: the clock as it takes it, and the other lines as they settle at the clock's pace (what
    // is counted, kept to and read); each line's keeping to the clock there
    struct Seen<'a> {
        how: usize,
        clk: &'a Channel<'a>,
        ck: Clock,
        settled: Vec<Vec<i64>>,
        keeps: Vec<f64>,
    }
    let mut seen: Vec<Seen> = vec![];
    for (clean, how) in cleans {
        for clk in &act {
            if (how == 3 || how == 4) && !bounced(clk.edges) {
                continue; // the clock rebuilt from its period only where its short runs are a bounce's
            }
            let Some(ck) = clock_of(clk, clean, how) else {
                continue;
            };
            let k = width(how, ck.half);
            if how == 1 && k as f64 > 0.6 * ck.half {
                continue; // a clock too fast to merge its bounces without its own edges
            }
            let cc = Channel::new(clk.name, clk.initial, &ck.edges);
            let settled: Vec<Vec<i64>> = act.iter().map(|c| clean(c.edges, k)).collect();
            let keeps = act
                .iter()
                .zip(&settled)
                .map(|(c, e)| if c.name == clk.name { 0.0 } else { keeping(e, &cc) })
                .collect();
            seen.push(Seen {
                how,
                clk,
                ck,
                settled,
                keeps,
            });
        }
    }
    // a line keeps to a clock line where it does in some view of it: a rebuilt clock's edges, placed by its period,
    // gather a fast bus's data changes less well than the clock as it is, though its reading is the right one
    let mut kmax: std::collections::BTreeMap<(&str, usize), f64> = Default::default();
    for s in &seen {
        for (i, &x) in s.keeps.iter().enumerate() {
            let e = kmax.entry((s.clk.name, i)).or_insert(0.0);
            *e = e.max(x);
        }
    }
    for s in &seen {
        let (how, clk, ck) = (s.how, s.clk, &s.ck);
        // the data lines as this view settles them, and with only their pulses of a tick out: a line beside the
        // clock takes crosstalk's pulses at the clock's very edges, which a wide merging of bounces (a rebuilt
        // clock's) takes out with the data's own changes near them; both read, the better reading kept
        let mut variants: Vec<Vec<Vec<i64>>> = vec![s.settled.clone()];
        if how != 0 {
            let alt: Vec<Vec<i64>> = act
                .iter()
                .zip(&s.settled)
                .map(|(c, own)| {
                    if c.name == clk.name {
                        own.clone()
                    } else {
                        deglitch(c.edges, 2)
                    }
                })
                .collect();
            if alt != s.settled {
                variants.push(alt);
            }
        }
        let cc = Channel::new(clk.name, clk.initial, &ck.edges);
        for (v, settled) in variants.iter().enumerate() {
            let lines: Vec<Channel> = act
                .iter()
                .zip(settled)
                .map(|(c, e)| Channel::new(c.name, c.initial, e))
                .collect();
            // (in the order they keep to the clock in this view: the best two read)
            let mut keep: Vec<(f64, &Channel)> = lines
                .iter()
                .enumerate()
                .filter(|(i, d)| {
                    d.name != clk.name
                        && d.edges.len() as f64 <= 1.5 * ck.edges.len() as f64
                        && kmax[&(clk.name, *i)] >= DATA_KEEPS
                })
                .map(|(i, d)| (if v == 0 { s.keeps[i] } else { keeping(d.edges, &cc) }, d))
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
                // (and a select of a single window: a target selected once, its two changes)
                .chain(once.iter().filter(|c| c.name != clk.name).map(Some))
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
                    if let Some(mut c) = read(view, clk, ck, cs, &set) {
                        c.params.insert("clean".into(), json!(CLEAN_NAMES[how]));
                        let key: Vec<(String, String)> = c.roles.clone().into_iter().collect();
                        if best.get(&key).is_none_or(|b| c.score > b.score) {
                            best.insert(key, c);
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<Candidate> = best.into_values().collect();
    out.extend(shared(view, &out));
    out
}

/// A target of a shared bus: its reading, its selections' spans, its score with the others' transfers not counted.
type Target<'a> = (&'a Candidate, Vec<(i64, i64)>, f64);

pub const SHARED_COVER: f64 = 0.95; // the targets' selections together hold this share of the clock's bursts or more
pub const SHARED_SNUG: f64 = 0.5; // each target's select snug round its bursts this share at least
pub const SHARED_MAX: usize = 8; // select lines on one clock tried in sets for one bus of targets
pub const SHARED_CUT: f64 = 0.1; // a target's select cutting this share of the bursts it reaches at most
pub const SHARED_FREE: usize = 2; // transfers outside every captured select: a target of its own, read without its CS

/// One clocked bus with a target on each select line (Python's `staged.shared_buses`): CLK and the data lines
/// shared, each select's own reading counting the other targets' transfers against it. Selections that never
/// overlap and hold the clock's bursts between them are the bus: one candidate of all its lines, its targets'
/// readings in its layers (`targets`), each a claim of its own when the bus is chosen.
fn shared(view: &View, cands: &[Candidate]) -> Vec<Candidate> {
    let mut by: std::collections::BTreeMap<(String, Vec<String>), Vec<&Candidate>> = Default::default();
    // the lines read without a select: a target whose CS is not captured (Python's `free` member)
    let mut nocs: std::collections::BTreeMap<(String, Vec<String>), &Candidate> = Default::default();
    for c in cands {
        let mut data: Vec<String> = c
            .roles
            .iter()
            .filter(|(k, _)| k.starts_with("data"))
            .map(|(_, v)| v.clone())
            .collect();
        data.sort();
        let key = (c.roles["clk"].clone(), data);
        if c.roles.contains_key("cs") {
            by.entry(key).or_default().push(c);
        } else if nocs.get(&key).is_none_or(|b| c.score > b.score) {
            nocs.insert(key, c);
        }
    }
    let mut out = vec![];
    for ((clk, data), per) in by {
        let free = nocs.get(&(clk.clone(), data.clone())).copied();
        let least = if free.is_some() { 1 } else { 2 };
        // each select line once (its best reading)
        let mut by_cs: std::collections::BTreeMap<&str, &Candidate> = Default::default();
        for c in per {
            let cs = c.roles["cs"].as_str();
            if by_cs.get(cs).is_none_or(|b| c.score > b.score) {
                by_cs.insert(cs, c);
            }
        }
        if by_cs.len() < least {
            continue;
        }
        let Some(ch) = view.channel(&clk) else { continue };
        // (the clock as the targets' readings took it: its pulses of a sample out, or as it is where they read it so -
        // a clock of a few samples a period has runs of a sample of its own; crosstalk's pulses go)
        let as_is = by_cs
            .values()
            .filter(|c| c.params.get("clean").and_then(|v| v.as_str()) == Some(CLEAN_NAMES[AS_IS]))
            .count();
        let ce = if 2 * as_is > by_cs.len() {
            ch.edges.to_vec()
        } else {
            deglitch(ch.edges, 2)
        };
        let mid: Vec<i64> = transfers_of(&ce)
            .iter()
            .filter(|(a, b)| b > a)
            .map(|(a, b)| (a + b) / 2)
            .collect();
        if mid.is_empty() {
            continue;
        }
        // a target's score with the other targets' transfers not counted against it: the clock's bursts outside its
        // windows (select_frames), the sampling edges outside them (covered) and the data's changes outside them
        // (inside) are the other targets' (the set below is held to hold the clock's bursts between them)
        let adjusted = |c: &Candidate| {
            let e = |k: &str| c.evidence.get(k).and_then(|x| x.as_f64()).unwrap_or(1.0);
            let f = e("select_frames");
            let out = (f * f * (e("covered") / 0.9).min(1.0) * e("inside") * e("single_window")).max(1e-12);
            (c.score / out).min(0.99)
        };
        let spans = |c: &Candidate| -> Vec<(i64, i64)> {
            let v: Value = serde_json::from_str(c.layers.get()).unwrap_or(Value::Null);
            let mut s: Vec<(i64, i64)> = v["transfers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|t| Some((t["s"].as_i64()?, t["e"].as_i64()?)))
                        .collect()
                })
                .unwrap_or_default();
            s.sort();
            s
        };
        // a select never cuts a transfer: each burst of the clock its windows reach lies inside one of them (a line
        // toggling on its own cuts them anywhere), SHARED_CUT of them at most (a spike, a bounce)
        let whole_bursts: Vec<(i64, i64)> = transfers_of(&ce);
        let cuts = |sp: &[(i64, i64)]| -> bool {
            let (mut reached, mut cut) = (0usize, 0usize);
            for &(a, b) in &whole_bursts {
                let over: Vec<&(i64, i64)> = sp.iter().filter(|&&(s, e)| s <= b && e >= a).collect();
                if over.is_empty() {
                    continue;
                }
                reached += 1;
                if !over.iter().any(|&&(s, e)| s <= a && b <= e) {
                    cut += 1;
                }
            }
            cut as f64 > SHARED_CUT * reached as f64
        };
        let mut list: Vec<Target> = by_cs
            .values()
            .map(|&c| (c, spans(c), adjusted(c)))
            .filter(|x| !x.1.is_empty() && !cuts(&x.1))
            .collect();
        list.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        list.truncate(SHARED_MAX);
        let holds = |sp: &[(i64, i64)]| -> Vec<bool> {
            mid.iter()
                .map(|&m| {
                    let k = sp.partition_point(|&(a, _)| a <= m);
                    k > 0 && m <= sp[k - 1].1
                })
                .collect()
        };
        let held_by: Vec<Vec<bool>> = list.iter().map(|x| holds(&x.1)).collect();
        let apart =
            |a: &[(i64, i64)], b: &[(i64, i64)]| !b.iter().any(|&(s, e)| a.iter().any(|&(x, y)| x <= e && y >= s));
        // every set of targets whose selections never overlap: the one holding the most bursts, then scoring best
        let mut best: ((f64, f64), Vec<usize>) = ((0.0, -1.0), vec![]);
        let n = list.len();
        for mask in 1u32..(1u32 << n) {
            let set: Vec<usize> = (0..n).filter(|&i| mask & (1 << i) != 0).collect();
            if set.len() < least {
                continue;
            }
            if set
                .iter()
                .enumerate()
                .any(|(a, &i)| set[a + 1..].iter().any(|&j| !apart(&list[i].1, &list[j].1)))
            {
                continue;
            }
            // the bursts held, each weighed by the best score of the targets holding it (a decoy line whose wide
            // windows hold many bursts by chance, its reading poor, weighs little)
            let held: f64 = (0..mid.len())
                .map(|k| {
                    set.iter()
                        .filter(|&&i| held_by[i][k])
                        .map(|&i| list[i].2)
                        .fold(0.0, f64::max)
                })
                .sum();
            let sum: f64 = set.iter().map(|&i| list[i].2).sum();
            if (held, sum) > best.0 {
                best = ((held, sum), set);
            }
        }
        if best.1.len() < least {
            continue;
        }
        let mut cover = (0..mid.len())
            .filter(|&k| best.1.iter().any(|&i| held_by[i][k]))
            .count() as f64
            / mid.len() as f64;
        let snug = best.1.iter().all(|&i| {
            list[i]
                .0
                .evidence
                .get("select_snug")
                .and_then(|x| x.as_f64())
                .unwrap_or(0.0)
                >= SHARED_SNUG
        });
        if !snug {
            continue;
        }
        // the bursts no captured select holds, read by the lines without one: a target whose CS is not on the probe
        // (SHARED_FREE of its transfers or more, all of them framed by that reading)
        let mut members: Vec<(&Candidate, f64)> = best.1.iter().map(|&i| (list[i].0, list[i].2)).collect();
        if cover < SHARED_COVER
            && let Some(f) = free
        {
            let held: Vec<bool> = (0..mid.len()).map(|k| best.1.iter().any(|&i| held_by[i][k])).collect();
            let fh = holds(&spans(f));
            let rest = (0..mid.len()).filter(|&k| !held[k] && fh[k]).count();
            let both = (0..mid.len()).filter(|&k| held[k] || fh[k]).count() as f64 / mid.len() as f64;
            if rest >= SHARED_FREE && both >= SHARED_COVER {
                members.push((f, f.score));
                cover = both;
            }
        }
        if cover < SHARED_COVER || members.len() < 2 {
            continue;
        }
        let w: Vec<f64> = members
            .iter()
            .map(|&(c, _)| {
                c.evidence
                    .get("windows")
                    .and_then(|x| x.as_f64())
                    .unwrap_or(1.0)
                    .max(1.0)
            })
            .collect();
        let total = members.iter().zip(&w).map(|(m, w)| w * m.1).sum::<f64>() / w.iter().sum::<f64>() * cover;
        let mut roles: std::collections::BTreeMap<String, String> = Default::default();
        roles.insert("clk".into(), clk.clone());
        for (i, d) in data.iter().enumerate() {
            roles.insert(format!("data{i}"), d.clone());
        }
        let mut targets = vec![];
        let mut k = 0;
        for &(c, score) in &members {
            if let Some(cs) = c.roles.get("cs") {
                roles.insert(if k == 0 { "cs".to_string() } else { format!("cs{k}") }, cs.clone());
                k += 1;
            }
            let lay: Value = serde_json::from_str(c.layers.get()).unwrap_or(Value::Null);
            targets.push(
                json!({"roles": c.roles, "score": score, "params": c.params, "evidence": c.evidence, "layers": lay}),
            );
        }
        let mut params = Map::new();
        params.insert("bus".into(), json!(format!("spi:{clk}")));
        params.insert("targets".into(), json!(targets.len()));
        params.insert("cover".into(), json!(cover));
        out.push(Candidate {
            protocol: "spi".into(),
            roles,
            score: total,
            alt: None,
            params,
            evidence: Map::new(),
            layers: crate::claim::raw(serde_json::to_string(&json!({ "targets": targets })).unwrap()),
        });
    }
    out
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
