//! A clocked bus of unknown framing (Python's `staged.SyncUnknownPlugin`): a clock and data lines keeping to it, in
//! frames of a consistent length that no known framing explains. Reported as a kind of its own ("unknown-sync"),
//! never as a known protocol, and only where no known reading explains its lines better (`crate::candidates`).

use crate::Candidate;
use crate::clocked::Group;
use crate::sampled::Sampled;
use serde_json::{Map, json};

pub const CHANGES: f64 = 0.1; // data changes a bit, the quietest line's, for a whole score
pub const BITS: f64 = 8.0; // the commonest frame this many bits or more for a whole score
pub const PAIR: f64 = 0.6; // data lines keeping to the clock this closely for a whole score
pub const GAP: f64 = 3.0; // an interval of the sampling edges longer than this times both neighbours ends a frame

/// Where a new burst starts in sorted ticks: an interval longer than `ratio` times both neighbours (`burst_split`).
fn burst_split(t: &[i64], ratio: f64) -> Vec<usize> {
    let mut out = vec![0];
    if t.len() < 3 {
        return out;
    }
    let iv: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    for i in 0..iv.len() {
        let prev = if i > 0 { iv[i - 1] } else { f64::INFINITY };
        let next = if i + 1 < iv.len() { iv[i + 1] } else { f64::INFINITY };
        if iv[i] > ratio * prev.min(next) {
            out.push(i + 1);
        }
    }
    out
}

/// The frames' lengths in sampled bits at the clock's bursts.
fn gap_lengths(t: &[i64]) -> Vec<usize> {
    let mut b = burst_split(t, GAP);
    b.push(t.len());
    b.windows(2).map(|w| w[1] - w[0]).collect()
}

/// The frames' lengths between START-like (data falling while the clock is high) and the next START- or STOP-like
/// event (`typed.frames_startstop`).
fn startstop_lengths(clock: &crate::view::Channel, data: &crate::clocked::Line, t: &[i64]) -> Vec<usize> {
    let mut out = vec![];
    let mut open: Option<usize> = None;
    let mut lv = data.initial;
    for &x in &data.edges {
        lv ^= 1;
        if clock.level_at(x) != 1 {
            continue;
        }
        let pos = t.partition_point(|&y| y < x);
        if let Some(p) = open {
            out.push(pos - p);
        }
        open = (lv == 0).then_some(pos);
    }
    out
}

/// The START / STOP framing's frames over the ticks (Python's `typed.frames_startstop`, whose lengths
/// `startstop_lengths` counts): a frame from each START-like event (data falling while the clock is high) to the next
/// event of either kind, one the capture ends in to its last tick; and every event ("S", "Sr" after an open START,
/// "P"; its tick).
type Startstop = (Vec<(usize, usize)>, Vec<(&'static str, i64)>);
fn startstop_frames(clock: &crate::view::Channel, data: &crate::clocked::Line, t: &[i64]) -> Startstop {
    let (mut frames, mut events) = (vec![], vec![]);
    let mut open: Option<usize> = None;
    let mut lv = data.initial;
    for &x in &data.edges {
        lv ^= 1;
        if clock.level_at(x) != 1 {
            continue;
        }
        let pos = t.partition_point(|&y| y < x);
        events.push((
            match (lv, open.is_some()) {
                (0, true) => "Sr",
                (0, false) => "S",
                _ => "P",
            },
            x,
        ));
        if let Some(p) = open {
            frames.push((p, pos));
        }
        open = (lv == 0).then_some(pos);
    }
    if let Some(p) = open.filter(|&p| p < t.len()) {
        frames.push((p, t.len()));
    }
    (frames, events)
}

/// The mean distance of the data lines' changes to the nearest of the ticks.
fn distance(lines: &[&crate::clocked::Line], t: &[i64]) -> f64 {
    let mut v = vec![];
    for l in lines {
        if l.edges.is_empty() || t.len() < 2 {
            continue;
        }
        let ds: Vec<f64> = l
            .edges
            .iter()
            .map(|&x| {
                let j = t.partition_point(|&y| y < x).clamp(1, t.len() - 1);
                (x - t[j]).abs().min((x - t[j - 1]).abs()) as f64
            })
            .collect();
        v.push(crate::kernels::median(&ds));
    }
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// The share above chance of frames of whole bytes (gap) or of 9n / 9n + 1 bits (start and stop: I2C's).
fn known_fit(lens: &[usize], startstop: bool) -> f64 {
    let ln: Vec<usize> = lens.iter().copied().filter(|&x| x > 0).collect();
    if ln.is_empty() {
        return 0.0;
    }
    let (fit, chance) = if startstop {
        (
            ln.iter().filter(|&&x| x % 9 <= 1).count() as f64 / ln.len() as f64,
            2.0 / 9.0,
        )
    } else {
        (
            ln.iter().filter(|&&x| x % 8 == 0).count() as f64 / ln.len() as f64,
            1.0 / 8.0,
        )
    };
    ((fit - chance) / (1.0 - chance)).max(0.0)
}

/// The readings kept, by their roles, each with the bits it sampled.
type Kept = std::collections::BTreeMap<Vec<(String, String)>, (Candidate, Sampled)>;

fn read(g: &Group, lines: &[&crate::clocked::Line], out: &mut Kept) {
    let rise = distance(lines, &g.rises) >= distance(lines, &g.falls);
    let t = if rise { &g.rises } else { &g.falls };
    if t.len() < 8 {
        return;
    }
    let bits: Vec<Vec<u8>> = lines.iter().map(|l| g.bits(l, rise)).collect();
    let nbits = (t.len() - 1).max(1) as f64;
    let ch_bit = bits
        .iter()
        .map(|r| r.windows(2).filter(|w| w[0] != w[1]).count() as f64 / nbits)
        .fold(f64::INFINITY, f64::min);
    let pair = lines.iter().map(|l| l.keeps).sum::<f64>() / lines.len() as f64;
    let gap = gap_lengths(t);
    let cc = crate::view::Channel::new(g.clock, g.initial, &g.edges);
    let ss = startstop_lengths(&cc, lines[0], t);
    let known = known_fit(&gap, false).max(known_fit(&ss, true));
    for (delim, lens) in [("gap", &gap), ("startstop", &ss)] {
        let lens: Vec<usize> = lens.iter().copied().filter(|&x| x > 2).collect();
        if lens.len() < 4 {
            continue;
        }
        let mut cnt = std::collections::BTreeMap::new();
        for &x in &lens {
            *cnt.entry(x).or_insert(0usize) += 1;
        }
        let mut top: Vec<(usize, usize)> = cnt.into_iter().collect();
        top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        let share = top.iter().take(2).map(|x| x.1).sum::<usize>() as f64 / lens.len() as f64;
        let q = 1.0 - (-(lens.len() as f64) / 4.0).exp();
        let score = 0.9
            * share
            * q
            * (1.0 - known)
            * (pair / PAIR).min(1.0)
            * (top[0].0 as f64 / BITS).min(1.0)
            * (ch_bit / CHANGES).min(1.0);
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("clk".to_string(), g.clock.to_string());
        for (i, l) in lines.iter().enumerate() {
            roles.insert(format!("data{i}"), l.name.to_string());
        }
        let top4: Vec<(usize, usize)> = top.iter().take(4).copied().collect();
        let mut params = Map::new();
        params.insert("sample_edge".into(), json!(if rise { "rise" } else { "fall" }));
        params.insert("delimiter".into(), json!(delim));
        let mut evidence = Map::new();
        evidence.insert("frame_bits".into(), json!(top4));
        evidence.insert("share_top2".into(), json!(share));
        evidence.insert("frames".into(), json!(lens.len()));
        evidence.insert("pair".into(), json!(pair));
        evidence.insert("known_fit".into(), json!(known));
        evidence.insert("changes_a_bit".into(), json!(ch_bit));
        let layers = crate::claim::raw(
            serde_json::to_string(&json!({ "summary": { "frame_bits": top4, "delimiter": delim } })).unwrap(),
        );
        let c = Candidate {
            protocol: "sync_unknown".into(),
            roles,
            score,
            alt: None,
            params,
            evidence,
            layers,
        };
        // (as `clocked::keep_best`: where it reads its roles better than one kept before)
        let key: Vec<(String, String)> = c.roles.clone().into_iter().collect();
        if out.get(&key).is_some_and(|b| c.score <= b.0.score) {
            continue;
        }
        // the bits sampled: every sampling edge of the clock, each line there, the frames of the framing read (gap:
        // every burst; start and stop: its frames and their conditions)
        let (frames, conditions) = if delim == "gap" {
            let mut b = burst_split(t, GAP);
            b.push(t.len());
            (b.windows(2).map(|w| (w[0], w[1])).collect(), vec![])
        } else {
            startstop_frames(&cc, lines[0], t)
        };
        let s = Sampled {
            clock: g.clock.to_string(),
            data: lines.iter().map(|l| l.name.to_string()).collect(),
            sample_edge: if rise { "rise" } else { "fall" },
            deglitch: g.width,
            t: t.clone(),
            values: bits.clone(),
            frames,
            conditions,
            word: 0,
        };
        out.insert(key, (c, s));
    }
}

/// The unknown clocked readings of the groups: each clock with its best data line, and with its best two.
pub fn candidates_of(gs: &[Group]) -> Vec<Candidate> {
    let mut best: Kept = Default::default();
    for g in gs {
        let ls: Vec<&crate::clocked::Line> = g.lines.iter().take(2).collect();
        if ls.is_empty() {
            continue;
        }
        read(g, &ls[..1], &mut best);
        if ls.len() == 2 {
            read(g, &ls, &mut best);
        }
    }
    best.into_values()
        .map(|(mut c, s)| {
            c.layers = crate::sampled::joined(&c.layers, &s.members(), "");
            c
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{Channel, View};

    #[test]
    fn frames_of_twelve_bits() {
        // 10 bursts of 12 clocks, data set on the falls (a pseudo-random pattern), gaps between
        let (mut clk, mut d, mut t, mut lv, mut x) = (vec![], vec![], 100i64, 0u8, 0x5a5u32);
        for _ in 0..10 {
            for _ in 0..12 {
                x = x.wrapping_mul(1103515245).wrapping_add(12345);
                let b = ((x >> 16) & 1) as u8;
                if b != lv {
                    d.push(t + 1);
                    lv = b;
                }
                clk.push(t + 5);
                clk.push(t + 10);
                t += 10;
            }
            t += 200;
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![Channel::new("CLK", 0, &clk), Channel::new("D", 0, &d)],
        )
        .unwrap();
        let c = candidates_of(&crate::clocked::groups(&v))
            .into_iter()
            .find(|c| c.params["delimiter"] == "gap")
            .expect("an unknown reading");
        assert_eq!(c.evidence["frame_bits"][0][0], json!(12), "{:?}", c.evidence);
        assert!(c.score > 0.5, "{:?}", c.evidence);
        // its bits and frames: every sampling edge, the bursts of 12 over them
        let lay: serde_json::Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["bits"]["t"].as_array().unwrap().len(), 120);
        assert_eq!(lay["bits"]["data"], json!(["D"]));
        let fr = lay["frames"].as_array().unwrap();
        assert_eq!(fr.len(), 10);
        assert!(fr.iter().all(|f| f["bits"] == json!(12)), "{fr:?}");
        assert!(lay.get("words").is_none() && lay.get("conditions").is_none());
        assert_eq!(lay["summary"]["delimiter"], json!("gap"));
    }
}
