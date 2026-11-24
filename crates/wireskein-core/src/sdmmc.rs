//! SD (and MMC) in SD mode (Python's `_engine/sdmmc.py`): the CMD line read on CLK's rise. A command or a short
//! response is 48 bits: start 0, the transmission bit (1 host to card), the index (6), the argument (32), CRC7
//! (x^7 + x^3 + 1 over the 40 bits before it), end 1, MSB first. R3 (the OCR) has all 1s for index and CRC; R2 (CID /
//! CSD) is 136 bits, its CRC7 over the 120 bits of the register before it. A CRC7 of 7 bits holds by chance once in
//! 128: a reading counts only where commands are answered by the response that follows.

use crate::Candidate;
use crate::clocked::{Group, groups, keep_best};
use crate::view::View;
use serde_json::{Map, Value, json};

pub const POLY: u8 = 0x09;
pub const ANSWERED: f64 = 0.3; // commands answered, this share at least
pub const RESPONSES_ANSWERING: f64 = 0.5; // responses following a command, this share for a whole score
pub const EVIDENCE_BITS: f64 = 8.0;
pub const R2_AFTER: [u64; 3] = [2, 9, 10];
pub const R3_AFTER: [u64; 1] = [41];

pub fn crc7(bits: &[u8]) -> u8 {
    let mut crc = 0u8;
    for &b in bits {
        let fb = ((crc >> 6) & 1) ^ b;
        crc = (crc << 1) & 0x7F;
        if fb == 1 {
            crc ^= POLY;
        }
    }
    crc
}

fn value(bits: &[u8]) -> u64 {
    bits.iter().fold(0u64, |a, &b| (a << 1) | b as u64)
}

#[derive(Debug, Clone)]
struct Frame {
    i: usize,
    n: usize,
    dir: &'static str,
    index: Option<u64>,
    item: Map<String, Value>,
}

/// The CMD line's frames, in order, not overlapping: the 48-bit ones; an R2 where the frame before it is a command it
/// answers (taking the place of chance 48-bit frames inside it); an R3 only after ACMD41.
fn frames(t: &[i64], bits: &[u8]) -> Vec<Frame> {
    let n = bits.len();
    if n < 48 {
        return vec![];
    }
    let mut out: Vec<Frame> = vec![];
    let mut taken_to = 0usize;
    for i in 0..n - 47 {
        if i < taken_to || bits[i] != 0 || bits[i + 47] != 1 {
            continue;
        }
        let got = value(&bits[i + 40..i + 47]) as u8;
        let ok48 = crc7(&bits[i..i + 40]) == got;
        let r3 = bits[i + 1] == 0 && bits[i + 2..i + 8].iter().all(|&b| b == 1) && got == 0x7F;
        if !(ok48 || r3) {
            continue;
        }
        taken_to = i + 48;
        let mut m = Map::new();
        m.insert("start".into(), json!(t[i]));
        m.insert("end".into(), json!(t[i + 47]));
        let (dir, index) = if r3 && !ok48 {
            m.insert("dir".into(), json!("R3"));
            m.insert("ocr".into(), json!(value(&bits[i + 8..i + 40])));
            ("R3", None)
        } else {
            let dir = if bits[i + 1] == 1 { "CMD" } else { "RSP" };
            let idx = value(&bits[i + 2..i + 8]);
            m.insert("dir".into(), json!(dir));
            m.insert("index".into(), json!(idx));
            m.insert("arg".into(), json!(value(&bits[i + 8..i + 40])));
            (dir, Some(idx))
        };
        m.insert("ok".into(), json!(true));
        out.push(Frame {
            i,
            n: 48,
            dir,
            index,
            item: m,
        });
    }
    if n >= 136 {
        for i in 0..n - 135 {
            if bits[i] != 0 || bits[i + 1] != 0 || bits[i + 135] != 1 || bits[i + 2..i + 8].iter().any(|&b| b != 1) {
                continue;
            }
            let reg = &bits[i + 8..i + 128];
            if reg.iter().all(|&b| b == reg[0]) {
                continue; // a register of all 1s (or 0s): an idle line's
            }
            if crc7(reg) != value(&bits[i + 128..i + 135]) as u8 {
                continue;
            }
            let prev = out.iter().filter(|f| f.i + f.n <= i).max_by_key(|f| f.i);
            if !prev.is_some_and(|p| p.dir == "CMD" && p.index.is_some_and(|x| R2_AFTER.contains(&x))) {
                continue;
            }
            out.retain(|f| f.i + f.n <= i || f.i >= i + 136);
            let mut m = Map::new();
            m.insert("start".into(), json!(t[i]));
            m.insert("end".into(), json!(t[i + 135]));
            m.insert("dir".into(), json!("R2"));
            m.insert("reg0".into(), json!(value(&bits[i + 8..i + 40])));
            m.insert("reg1".into(), json!(value(&bits[i + 40..i + 72])));
            m.insert("reg2".into(), json!(value(&bits[i + 72..i + 104])));
            m.insert("reg3".into(), json!(value(&bits[i + 104..i + 128])));
            m.insert("ok".into(), json!(true));
            out.push(Frame {
                i,
                n: 136,
                dir: "R2",
                index: None,
                item: m,
            });
        }
    }
    out.sort_by_key(|f| f.i);
    let mut keep: Vec<Frame> = vec![];
    for f in out {
        if f.dir == "R3"
            && !keep
                .last()
                .is_some_and(|p| p.dir == "CMD" && p.index.is_some_and(|x| R3_AFTER.contains(&x)))
        {
            continue;
        }
        keep.push(f);
    }
    keep
}

/// (commands answered by the frame after them, commands).
fn answered(fr: &[Frame]) -> (usize, usize) {
    let (mut ans, mut cmds) = (0, 0);
    for (i, a) in fr.iter().enumerate() {
        if a.dir != "CMD" {
            continue;
        }
        cmds += 1;
        let Some(b) = fr.get(i + 1) else { continue };
        let (as_, ae) = (a.item["start"].as_i64().unwrap(), a.item["end"].as_i64().unwrap());
        let soon = b.item["start"].as_i64().unwrap() - ae < 2 * (ae - as_).max(1);
        let same = b.dir == "R2" || b.dir == "R3" || (b.dir == "RSP" && b.index == a.index);
        ans += (soon && same) as usize;
    }
    (ans, cmds)
}

fn read(g: &crate::clocked::Group, best: &mut std::collections::BTreeMap<Vec<(String, String)>, Candidate>) {
    for l in &g.lines {
        let bits = g.bits(l, true);
        let f = frames(&g.rises, &bits);
        if f.len() < 2 {
            continue;
        }
        let (ans, ncmd) = answered(&f);
        if ans < 1 || (ans as f64) < ANSWERED * ncmd as f64 {
            continue;
        }
        let info = 7.0 * f.len() as f64 + 6.0 * ans as f64;
        let mut score = (1.0 - 0.5f64.powf(info / EVIDENCE_BITS)) * (0.6 + 0.4 * ans as f64 / ncmd.max(1) as f64);
        let n_rsp = f.iter().filter(|x| x.dir != "CMD").count();
        if n_rsp >= 2 {
            score *= (ans as f64 / n_rsp as f64 / RESPONSES_ANSWERING).min(1.0);
        }
        let mut params = Map::new();
        params.insert("sample_edge".into(), json!("rise"));
        params.insert("clean".into(), json!(crate::clocked::CLEANS[g.how]));
        let mut evidence = Map::new();
        evidence.insert("frames".into(), json!(f.len()));
        evidence.insert("commands".into(), json!(ncmd));
        evidence.insert("answered".into(), json!(ans));
        let items: Vec<&Map<String, Value>> = f.iter().map(|x| &x.item).collect();
        let layers = crate::claim::raw(serde_json::to_string(&json!({ "frames": items })).unwrap());
        keep_best(
            best,
            Candidate {
                protocol: "sdmmc".into(),
                roles: [
                    ("clk".to_string(), g.clock.to_string()),
                    ("cmd".to_string(), l.name.to_string()),
                ]
                .into(),
                score,
                alt: None,
                params,
                evidence,
                layers,
            },
        );
    }
}

/// The SD / MMC readings of a part: each clock's data lines as CMD, read on its rises.
pub fn candidates(view: &View) -> Vec<Candidate> {
    candidates_of(&groups(view))
}

pub fn candidates_of(gs: &[Group]) -> Vec<Candidate> {
    let mut best = Default::default();
    for g in gs {
        read(g, &mut best);
    }
    best.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::Channel;

    fn frame(dir: u8, index: u64, arg: u64) -> Vec<u8> {
        let mut b = vec![0, dir];
        b.extend((0..6).rev().map(|k| ((index >> k) & 1) as u8));
        b.extend((0..32).rev().map(|k| ((arg >> k) & 1) as u8));
        let c = crc7(&b);
        b.extend((0..7).rev().map(|k| (c >> k) & 1));
        b.push(1);
        b
    }

    #[test]
    fn reads_commands_and_responses() {
        let mut bits = vec![1u8; 10];
        for (d, i, a) in [
            (1, 8, 0x1aa),
            (0, 8, 0x1aa),
            (1, 55, 0),
            (0, 55, 0x120),
            (1, 13, 0x10000),
            (0, 13, 0x900),
        ] {
            bits.extend(frame(d, i, a));
            bits.extend([1u8; 6]);
        }
        let (clk, l, n) = crate::jtag::tests::lines_of(&[&bits]);
        let cmd: Vec<i64> = l[0].clone();
        let v = View::new(1e6, n, vec![Channel::new("CLK", 0, &clk), Channel::new("CMD", 1, &cmd)]).unwrap();
        // (the rows start at 0: the line starts high, its first change a fall)
        let c = candidates(&v).into_iter().next().expect("an SD reading");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["frames"].as_array().unwrap().len(), 6, "{lay}");
        assert_eq!(lay["frames"][3]["arg"], json!(0x120));
        assert!(c.score > 0.8, "{:?}", c.evidence);
    }
}
