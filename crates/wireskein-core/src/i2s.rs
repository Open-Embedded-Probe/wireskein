//! I2S and its kin (Python's `_engine/i2s.py`): left- / right-justified, and TDM (a frame sync once a frame of three
//! slots or more). A bit clock, a word select changing once a slot and a serial data line, both keeping to the clock
//! and read on its rise. The word's place in its slot is told by the padding (bits the same in every slot at its end
//! or start); a word filling its slot tells nothing, and I2S is taken (`format_assumed`).

use crate::Candidate;
use crate::clocked::{Group, keep_best};
use crate::view::View;
use serde_json::{Map, Value, json};

pub const SLOT_MIN: usize = 8;
pub const LINES_MAX: usize = 8;
pub const WORD_MAX: usize = 32;
pub const SLOT_MAX: usize = 64;
pub const REGULAR: f64 = 0.9;
pub const SLOTS: f64 = 8.0;
pub const BURSTS: usize = 4;
pub const SLOTS_USUAL: [usize; 5] = [16, 18, 20, 24, 32];
pub const ODD_SLOT: f64 = 0.6;
pub const MIN_WORD: usize = 8;
pub const TDM_SLOTS: [usize; 4] = [32, 24, 16, 8];
pub const TDM_FRAME_MAX: usize = 1024;
pub const TDM_DISTINCT: usize = 4;
pub const TDM_WORD_BITS: [usize; 6] = [8, 12, 16, 20, 24, 32];

/// Where a row of bits changes, and the lengths of its runs between changes.
fn runs(ws: &[u8]) -> (Vec<usize>, Vec<usize>) {
    let ch: Vec<usize> = (1..ws.len()).filter(|&i| ws[i] != ws[i - 1]).collect();
    let r = ch.windows(2).map(|w| w[1] - w[0]).collect();
    (ch, r)
}

/// The most common value and its share (the smallest of those tied).
fn mode(v: &[usize]) -> (usize, f64) {
    let mut c = std::collections::BTreeMap::new();
    for &x in v {
        *c.entry(x).or_insert(0usize) += 1;
    }
    let top = c.values().max().copied().unwrap_or(0);
    let x = c.iter().find(|&(_, &n)| n == top).map(|(&x, _)| x).unwrap_or(0);
    (x, top as f64 / v.len().max(1) as f64)
}

/// (bits always the same at the rows' end, at their start); rows of `w` bits.
fn padding(rows: &[&[u8]], w: usize) -> (usize, usize) {
    let cons: Vec<bool> = (0..w)
        .map(|j| rows.len() > 1 && rows.iter().all(|r| r[j] == rows[0][j]))
        .collect();
    let first = cons.iter().position(|&c| !c);
    let last = cons.iter().rposition(|&c| !c);
    match (first, last) {
        (Some(f), Some(l)) => (w - l - 1, f),
        _ => (w, w),
    }
}

fn word(bits: &[u8]) -> i64 {
    let n = bits.len();
    let v = bits.iter().fold(0i64, |a, &b| (a << 1) | b as i64);
    if n > 0 && v >= 1 << (n - 1) { v - (1 << n) } else { v }
}

/// 1 unless the words are nearly all steps (at most one change of bit inside) of two values or more: down to 0.05
/// as their share goes from 0.9 to 1.
fn square_words(vals: &[i64], bits: usize) -> f64 {
    if vals.len() < 8 || bits <= 1 {
        return 1.0;
    }
    let mask: i64 = if bits >= 63 { -1 } else { (1i64 << bits) - 1 };
    let mut steps = std::collections::BTreeSet::new();
    let mut n_step = 0usize;
    for &x in vals {
        let v = x & mask;
        let c = ((v ^ (v >> 1)) & (mask >> 1)).count_ones();
        if c <= 1 {
            n_step += 1;
            steps.insert(v);
        }
    }
    if steps.len() < 2 {
        return 1.0;
    }
    let share = n_step as f64 / vals.len() as f64;
    if share <= 0.9 {
        1.0
    } else {
        (1.0 - (share - 0.9) / 0.1).max(0.05)
    }
}

struct Read {
    frames: Vec<Value>,
    vals: Vec<i64>,
    regular: f64,
    slots: usize,
    bits: usize,
    params: Map<String, Value>,
}

fn read_i2s(t: &[i64], ws: &[u8], sd: &[u8]) -> Option<Read> {
    let (ch, r) = runs(ws);
    if r.len() < 4 {
        return None;
    }
    let (slot, regular) = mode(&r);
    if !(SLOT_MIN..=SLOT_MAX).contains(&slot) || regular < REGULAR {
        return None;
    }
    let mut starts: Vec<usize> = (0..r.len()).filter(|&k| r[k] == slot).map(|k| ch[k]).collect();
    if ws.len() - ch[ch.len() - 1] >= slot {
        starts.push(ch[ch.len() - 1]);
    }
    let rows = |off: usize| -> (Vec<usize>, Vec<&[u8]>) {
        let s: Vec<usize> = starts
            .iter()
            .map(|&x| x + off)
            .filter(|&x| x + slot <= sd.len())
            .collect();
        let rr = s.iter().map(|&x| &sd[x..x + slot]).collect();
        (s, rr)
    };
    let (s1, r1) = rows(1);
    let (s0, r0) = rows(0);
    let (tail1, head1) = padding(&r1, slot);
    let (tail0, head0) = padding(&r0, slot);
    let (fmt, s_, r_, bits) = if head1 > 0 && head1 >= tail1 && head1 >= tail0 {
        ("right", s0, r0, slot as isize - head0 as isize)
    } else if tail0 > tail1 {
        ("left", s0, r0, slot as isize - tail0 as isize)
    } else {
        ("i2s", s1, r1, slot as isize - tail1 as isize)
    };
    let bits = bits.max(1) as usize;
    if bits > WORD_MAX {
        return None;
    }
    let assumed = tail0 == tail1 && tail1 == head1 && head1 == 0;
    let left_level = if fmt == "i2s" { 0 } else { 1 };
    let mut frames: Vec<Map<String, Value>> = vec![];
    let mut vals = vec![];
    let mut open = false;
    for (&st, row) in s_.iter().zip(&r_) {
        let w = if fmt == "right" {
            word(&row[slot - bits..])
        } else {
            word(&row[..bits])
        };
        vals.push(w);
        let lv = ws[(st + bits / 2).min(ws.len() - 1)];
        if lv == left_level {
            let mut m = Map::new();
            m.insert("s".into(), json!(t[st]));
            m.insert("left".into(), json!(w));
            frames.push(m);
            open = true;
        } else if open && !frames.last().unwrap().contains_key("right") {
            frames.last_mut().unwrap().insert("right".into(), json!(w));
        }
    }
    let mut params = Map::new();
    params.insert("format".into(), json!(fmt));
    params.insert("slot_bits".into(), json!(slot));
    params.insert("word_bits".into(), json!(bits));
    if assumed {
        params.insert("format_assumed".into(), json!(true));
    }
    Some(Read {
        frames: frames.into_iter().map(Value::Object).collect(),
        vals,
        regular,
        slots: s_.len(),
        bits,
        params,
    })
}

fn median_usize(v: &[usize]) -> f64 {
    crate::kernels::median(&v.iter().map(|&x| x as f64).collect::<Vec<_>>())
}

fn read_tdm(t: &[i64], fs: &[u8], sd: &[u8]) -> Option<Read> {
    let (ch, r) = runs(fs);
    if r.len() < 4 {
        return None;
    }
    let lv: Vec<u8> = ch[..ch.len() - 1].iter().map(|&i| fs[i]).collect();
    let hi: Vec<usize> = (0..r.len()).filter(|&k| lv[k] == 1).map(|k| r[k]).collect();
    let lo: Vec<usize> = (0..r.len()).filter(|&k| lv[k] == 0).map(|k| r[k]).collect();
    if hi.is_empty() || lo.is_empty() {
        return None;
    }
    let (mh, ml) = (median_usize(&hi), median_usize(&lo));
    let active = if mh <= ml { 1 } else { 0 };
    let starts: Vec<usize> = ch.iter().copied().filter(|&i| fs[i] == active).collect();
    if starts.len() < 3 {
        return None;
    }
    let period: Vec<usize> = starts.windows(2).map(|w| w[1] - w[0]).collect();
    let (frame, regular) = mode(&period);
    let two_equal = mh == ml && mh <= SLOT_MAX as f64;
    if two_equal || regular < REGULAR || !(3 * SLOT_MIN..=TDM_FRAME_MAX).contains(&frame) {
        return None;
    }
    let at = |off: usize| -> Vec<usize> {
        starts
            .iter()
            .map(|&x| x + off)
            .filter(|&x| x + frame <= sd.len())
            .collect()
    };
    let cells = |s: &[usize], slot: usize| -> Vec<&[u8]> {
        s.iter()
            .flat_map(|&x| (0..frame / slot).map(move |k| x + k * slot))
            .map(|x| &sd[x..x + slot])
            .collect()
    };
    type Key = (usize, bool, bool, isize);
    let mut best: Option<(Key, usize, usize, usize, usize)> = None;
    for off in [1usize, 0] {
        let s = at(off);
        if s.len() < 2 {
            continue;
        }
        for slot in TDM_SLOTS {
            if !frame.is_multiple_of(slot) || frame / slot < 3 {
                continue;
            }
            let (tail, head) = padding(&cells(&s, slot), slot);
            let pad = tail.max(head);
            let key = (pad.min(slot - 1), tail >= head, off == 1, -(slot as isize));
            if best.as_ref().is_none_or(|b| key > b.0) {
                best = Some((key, off, slot, tail, head));
            }
        }
    }
    let (key, mut off, mut slot, mut tail, mut head) = best?;
    let few = {
        let s = at(off);
        let set: std::collections::BTreeSet<&[u8]> = cells(&s, slot).into_iter().collect();
        set.len() < TDM_DISTINCT
    };
    let assumed = key.0 == 0 || few;
    if assumed {
        if frame.is_multiple_of(32) && frame / 32 >= 3 {
            slot = 32;
        }
        off = 1;
        tail = 0;
        head = 0;
    }
    let s = at(off);
    let n_slots = frame / slot;
    let right = head > tail;
    let pad = if right { head } else { tail };
    let bits = TDM_WORD_BITS
        .iter()
        .copied()
        .filter(|&b| b + pad >= slot)
        .chain([slot])
        .min()
        .unwrap();
    let mut frames = vec![];
    let mut vals = vec![];
    for &x in &s {
        let words: Vec<i64> = (0..n_slots)
            .map(|k| {
                let c = &sd[x + k * slot..x + (k + 1) * slot];
                if right {
                    word(&c[slot - bits..])
                } else {
                    word(&c[..bits])
                }
            })
            .collect();
        vals.extend(&words);
        frames.push(json!({ "s": t[x], "slots": words }));
    }
    let mut params = Map::new();
    params.insert("format".into(), json!("tdm"));
    params.insert("slot_bits".into(), json!(slot));
    params.insert("word_bits".into(), json!(bits));
    params.insert("slots_per_frame".into(), json!(n_slots));
    params.insert("offset".into(), json!(off));
    params.insert("fs_active".into(), json!(active));
    if right {
        params.insert("justify".into(), json!("right"));
    }
    if assumed {
        params.insert("format_assumed".into(), json!(true));
    }
    Some(Read {
        frames,
        vals,
        regular,
        slots: s.len() * n_slots,
        bits,
        params,
    })
}

pub const OTHER_CLOCK: f64 = 0.1; // a share off for each line whose changes meet another clock's edges, not this one's
pub const COINCIDE_TICKS: i64 = 1;

/// The share of a line's changes within COINCIDE_TICKS of one of a clock's (Python's survey `coincide`).
fn coincide(line: &[i64], clock: &[i64]) -> f64 {
    if line.is_empty() || clock.is_empty() {
        return 0.0;
    }
    let near = line
        .iter()
        .filter(|&&x| {
            let j = clock.partition_point(|&y| y < x - COINCIDE_TICKS);
            j < clock.len() && clock[j] <= x + COINCIDE_TICKS
        })
        .count();
    near as f64 / line.len() as f64
}

/// 1, less OTHER_CLOCK for each line meeting another line's edges (another clock's) more than this clock's by over
/// a half (`i2s._own_clock`: two audio buses whose bit clocks are 1 : 2 each read the other's WS in step).
fn own_clock(view: &View, clock: &str, lines: &[&str]) -> f64 {
    let Some(ck) = view.channel(clock) else {
        return 1.0;
    };
    let mut f = 1.0;
    for &l in lines {
        let Some(lc) = view.channel(l) else { continue };
        let mine = coincide(lc.edges, ck.edges);
        let other = view
            .channels
            .iter()
            .filter(|c| c.name != clock && c.name != l && c.edges.len() >= 16)
            .map(|c| coincide(lc.edges, c.edges))
            .fold(0.0, f64::max);
        if other - mine > 0.5 {
            f *= 1.0 - OTHER_CLOCK;
        }
    }
    f
}

/// The best I2S (or TDM) reading of a group: each data line as WS beside each other as SD.
fn read(g: &Group, view: &View) -> Option<Candidate> {
    let lines = &g.lines[..g.lines.len().min(LINES_MAX)];
    if lines.len() < 2 {
        return None;
    }
    let bits: Vec<Vec<u8>> = lines.iter().map(|l| g.bits(l, true)).collect();
    let t = &g.rises;
    let nb = crate::sync::bursts(t).len();
    let mut best: Option<Candidate> = None;
    for wi in 0..lines.len() {
        for si in 0..lines.len() {
            if si == wi {
                continue;
            }
            let Some(r) = read_i2s(t, &bits[wi], &bits[si]).or_else(|| read_tdm(t, &bits[wi], &bits[si])) else {
                continue;
            };
            if r.bits < MIN_WORD {
                continue;
            }
            let pair = lines[si].keeps;
            let q = 1.0 - 0.5f64.powf(r.slots as f64 / SLOTS);
            let tv = square_words(&r.vals, r.bits);
            let runs_on = if nb <= BURSTS.max(r.frames.len()) {
                1.0
            } else {
                r.frames.len() as f64 / nb as f64
            };
            let tdm = r.params["format"] == "tdm";
            let slot = r.params["slot_bits"].as_u64().unwrap() as usize;
            let usual = if tdm || SLOTS_USUAL.contains(&slot) {
                1.0
            } else {
                ODD_SLOT
            };
            let own = own_clock(view, g.clock, &[lines[wi].name, lines[si].name]);
            let score = r.regular * q * (0.5 + 0.5 * pair) * tv * runs_on * usual * own;
            if best.as_ref().is_some_and(|b| b.score >= score) {
                continue;
            }
            let mut params = r.params;
            params.insert("sample_edge".into(), json!("rise"));
            params.insert("clean".into(), json!(crate::clocked::CLEANS[g.how]));
            let mut evidence = Map::new();
            evidence.insert("regular".into(), json!(r.regular));
            evidence.insert("slots".into(), json!(r.slots));
            evidence.insert("bits".into(), json!(r.bits));
            evidence.insert("pair".into(), json!(pair));
            evidence.insert("square_words".into(), json!(tv));
            evidence.insert("runs_on".into(), json!(runs_on));
            evidence.insert("usual_slot".into(), json!(usual));
            evidence.insert("own_clock".into(), json!(own));
            let layers = crate::claim::raw(serde_json::to_string(&json!({ "frames": r.frames })).unwrap());
            best = Some(Candidate {
                protocol: "i2s".into(),
                roles: [
                    ("sck".to_string(), g.clock.to_string()),
                    ("ws".to_string(), lines[wi].name.to_string()),
                    ("sd".to_string(), lines[si].name.to_string()),
                ]
                .into(),
                score,
                alt: None,
                params,
                evidence,
                layers,
            });
        }
    }
    best
}

pub fn candidates_of(gs: &[Group], view: &View) -> Vec<Candidate> {
    let mut best = Default::default();
    for g in gs {
        if let Some(c) = read(g, view) {
            keep_best(&mut best, c);
        }
    }
    best.into_values().collect()
}

/// The I2S readings of a part.
pub fn candidates(view: &View) -> Vec<Candidate> {
    candidates_of(&crate::clocked::groups(view), view)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Channel;

    #[test]
    fn reads_i2s_and_tdm() {
        // I2S: 32-bit slots of 16-bit words, WS a bit before the MSB, left at WS low
        let frames: Vec<(i64, i64)> = (0..20).map(|k| (k * 997 % 30000 - 15000, -(k * 331 % 20000))).collect();
        let (mut ws, mut sd) = (vec![], vec![]);
        for &(l, r) in &frames {
            for (c, v) in [(0u8, l), (1, r)] {
                let v = (v & 0xFFFF) as u64;
                for j in 0..32 {
                    ws.push(c);
                    sd.push(if j < 16 { ((v >> (15 - j)) & 1) as u8 } else { 0 });
                }
            }
        }
        let ws: Vec<u8> = ws[1..].iter().copied().chain([1]).collect();
        let (clk, l, n) = crate::jtag::tests::lines_of(&[&ws, &sd]);
        let v = View::new(
            1e6,
            n,
            vec![
                Channel::new("BCLK", 0, &clk),
                Channel::new("WS", ws[0], &l[0]),
                Channel::new("SD", sd[0], &l[1]),
            ],
        )
        .unwrap();
        let c = candidates(&v)
            .into_iter()
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
            .expect("an I2S reading");
        assert_eq!(c.roles["ws"], "WS");
        assert_eq!(c.params["format"], json!("i2s"));
        assert_eq!(c.params["word_bits"], json!(16));
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        // (the first frame has no change of WS before it)
        assert_eq!(lay["frames"][2]["left"], json!(frames[3].0), "{lay}");
        assert_eq!(lay["frames"][2]["right"], json!(frames[3].1));
        assert!(c.score > 0.6, "{:?}", c.evidence);

        // TDM: 4 slots of 32 bits, 24-bit words, FS a bit wide a bit before the frame
        let rows: Vec<Vec<i64>> = (0..12)
            .map(|k| {
                (0..4)
                    .map(|s| (k * 7919 + s * 104729) % 8_000_000 - 4_000_000)
                    .collect()
            })
            .collect();
        let (mut fs, mut sd) = (vec![], vec![]);
        for row in &rows {
            for (s, &x) in row.iter().enumerate() {
                let v = (x & 0xFF_FFFF) as u64;
                for j in 0..32 {
                    fs.push((s == 0 && j == 0) as u8);
                    sd.push(if j < 24 { ((v >> (23 - j)) & 1) as u8 } else { 0 });
                }
            }
        }
        let fs: Vec<u8> = fs[1..].iter().copied().chain([0]).collect();
        let r = read_tdm(&(0..fs.len() as i64).collect::<Vec<_>>(), &fs, &sd).expect("a TDM reading");
        assert_eq!(r.params["slots_per_frame"], json!(4));
        assert_eq!(r.params["word_bits"], json!(24));
        assert_eq!(r.frames[2]["slots"][1], json!(rows[3][1])); // (the first frame has no FS before it)
    }
}
