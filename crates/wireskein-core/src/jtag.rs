//! JTAG (IEEE 1149.1, Python's `_engine/jtag.py`): TCK, TMS walking the TAP's state machine, TDI and TDO shifting in
//! its Shift-IR / Shift-DR states, all sampled on TCK's rise. Every TMS sequence is a legal walk: the evidence is what
//! the walk explains: TDI's and TDO's changes falling in the shift states, IR scans of one length, and each IR scan's
//! first two bits out on TDO being 1 then 0 (the IR's capture value, which tells TDO from TDI too).

use crate::Candidate;
use crate::clocked::{Group, groups, hex_lsb_first, keep_best};
use crate::view::View;
use serde_json::{Map, Value, json};

pub const TLR: i8 = 0;
pub const RTI: i8 = 1;
pub const SDS: i8 = 2;
pub const CDR: i8 = 3;
pub const SDR: i8 = 4;
pub const E1DR: i8 = 5;
pub const PDR: i8 = 6;
pub const E2DR: i8 = 7;
pub const UDR: i8 = 8;
pub const SIS: i8 = 9;
pub const CIR: i8 = 10;
pub const SIR: i8 = 11;
pub const E1IR: i8 = 12;
pub const PIR: i8 = 13;
pub const E2IR: i8 = 14;
pub const UIR: i8 = 15;

/// The TAP's next state, [state][tms].
pub const NEXT: [[i8; 2]; 16] = [
    [RTI, TLR],
    [RTI, SDS],
    [CDR, SIS],
    [SDR, E1DR],
    [SDR, E1DR],
    [PDR, UDR],
    [PDR, E2DR],
    [SDR, UDR],
    [RTI, SDS],
    [CIR, TLR],
    [SIR, E1IR],
    [SIR, E1IR],
    [PIR, UIR],
    [PIR, E2IR],
    [SIR, UIR],
    [RTI, SDS],
];

pub const LINES_MAX: usize = 6; // data lines a group has at most for JTAG to be tried on it
pub const SCANS: f64 = 4.0; // scans for half the evidence of their count
pub const CAPTURES: usize = 2; // IR scans with TDO's 1 0, at least, for a claim
pub const IN_SHIFT: f64 = 0.9; // TDI's / TDO's changes in the shift states, this share for full weight
pub const EVIDENCE_BITS: f64 = 8.0;

/// The TAP's state before each sampled TMS bit; -1 while more than one state is possible.
pub fn walk(tms: &[u8]) -> Vec<i8> {
    let mut out = vec![-1i8; tms.len()];
    let mut states: u16 = 0xFFFF;
    let mut cur: i8 = -1;
    for (i, &b) in tms.iter().enumerate() {
        if cur < 0 {
            if states.count_ones() == 1 {
                cur = states.trailing_zeros() as i8;
            } else {
                let mut nx = 0u16;
                for (s, nxt) in NEXT.iter().enumerate() {
                    if states >> s & 1 == 1 {
                        nx |= 1 << nxt[b as usize];
                    }
                }
                states = nx;
                continue;
            }
        }
        out[i] = cur;
        cur = NEXT[cur as usize][b as usize];
    }
    out
}

/// One scan: where it starts, IR or DR, the bits shifted in and out (LSB first, as shifted).
#[derive(Debug, Clone)]
pub struct Scan {
    pub s: i64,
    pub ir: bool,
    pub tdi: Vec<u8>,
    pub tdo: Vec<u8>,
}

impl Scan {
    pub fn bits(&self) -> usize {
        if self.tdi.is_empty() {
            self.tdo.len()
        } else {
            self.tdi.len()
        }
    }
}

/// The IR and DR scans from Capture to Update (or to the edge entering Update, with TMS: a capture ending there, the
/// clock stopped, keeps its last scan).
pub fn scans(st: &[i8], t: &[i64], tdi: Option<&[u8]>, tdo: Option<&[u8]>, tms: Option<&[u8]>) -> Vec<Scan> {
    let mut out = vec![];
    let mut cur: Option<Scan> = None;
    for (i, &s) in st.iter().enumerate() {
        let ends = tms.is_some_and(|m| m[i] == 1 && matches!(s, E1DR | E2DR | E1IR | E2IR));
        if ends && cur.is_some() {
            out.push(cur.take().unwrap());
            continue;
        }
        match s {
            CDR | CIR => {
                cur = Some(Scan {
                    s: t[i],
                    ir: s == CIR,
                    tdi: vec![],
                    tdo: vec![],
                })
            }
            SDR | SIR => {
                if let Some(c) = cur.as_mut() {
                    if let Some(d) = tdi {
                        c.tdi.push(d[i]);
                    }
                    if let Some(d) = tdo {
                        c.tdo.push(d[i]);
                    }
                }
            }
            UDR | UIR => {
                if let Some(c) = cur.take() {
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

fn in_shift(s: i8) -> bool {
    s == SDR || s == SIR
}

/// A line's changes (between its bits) where the state is known: (those in or out of a shift state, all).
fn changes_in_shift(row: &[u8], st: &[i8]) -> (usize, usize) {
    let (mut inside, mut tot) = (0, 0);
    for i in 1..row.len() {
        if row[i] != row[i - 1] && st[i] >= 0 {
            tot += 1;
            if in_shift(st[i]) || in_shift(st[i - 1]) {
                inside += 1;
            }
        }
    }
    (inside, tot)
}

/// The most common of the values (the smallest of those tied).
pub fn mode(v: &[usize]) -> Option<usize> {
    let mut c = std::collections::BTreeMap::new();
    for &x in v {
        *c.entry(x).or_insert(0usize) += 1;
    }
    let top = c.values().max().copied()?;
    c.into_iter().find(|&(_, n)| n == top).map(|(x, _)| x)
}

pub fn scan_items(sc: &[Scan], always: bool) -> Vec<Value> {
    sc.iter()
        .map(|s| {
            let mut m = Map::new();
            m.insert("s".into(), json!(s.s));
            m.insert("type".into(), json!(if s.ir { "IR" } else { "DR" }));
            m.insert("bits".into(), json!(s.bits()));
            if always || !s.tdi.is_empty() {
                m.insert("tdi".into(), json!(hex_lsb_first(&s.tdi)));
            }
            if always || !s.tdo.is_empty() {
                m.insert("tdo".into(), json!(hex_lsb_first(&s.tdo)));
            }
            Value::Object(m)
        })
        .collect()
}

fn ir_capture(sc: &[Scan]) -> (usize, usize) {
    let ir: Vec<&Scan> = sc.iter().filter(|s| s.ir && s.tdo.len() >= 2).collect();
    (ir.iter().filter(|s| s.tdo[0] == 1 && s.tdo[1] == 0).count(), ir.len())
}

/// The best JTAG reading of a group: each data line as TMS, TDO the line whose IR scans start 1 0 out.
fn read(g: &Group) -> Option<Candidate> {
    let lines = &g.lines[..g.lines.len().min(LINES_MAX)];
    if lines.len() < 2 {
        return None;
    }
    let bits: Vec<Vec<u8>> = lines.iter().map(|l| g.bits(l, true)).collect();
    let t = &g.rises;
    let mut best: Option<Candidate> = None;
    for mi in 0..lines.len() {
        let st = walk(&bits[mi]);
        if st.iter().all(|&s| s < 0) {
            continue;
        }
        let tms = Some(bits[mi].as_slice());
        let others: Vec<usize> = (0..lines.len()).filter(|&i| i != mi).collect();
        let mut conc = vec![0.0; lines.len()];
        let mut irt = vec![(0usize, 0usize); lines.len()];
        for &i in &others {
            let (inn, tot) = changes_in_shift(&bits[i], &st);
            conc[i] = if tot > 0 { inn as f64 / tot as f64 } else { 0.0 };
            irt[i] = ir_capture(&scans(&st, t, None, Some(&bits[i]), tms));
        }
        let mut tdo = others[0];
        for &i in &others[1..] {
            if (irt[i].0, conc[i]) > (irt[tdo].0, conc[tdo]) {
                tdo = i;
            }
        }
        if irt[tdo].0 < CAPTURES {
            continue;
        }
        let rest: Vec<usize> = others.iter().copied().filter(|&i| i != tdo).collect();
        let tdi = rest.iter().copied().fold(None, |b: Option<usize>, i| match b {
            Some(j) if conc[j] >= conc[i] => Some(j),
            _ => Some(i),
        });
        let sc = scans(&st, t, tdi.map(|i| bits[i].as_slice()), Some(&bits[tdo]), tms);
        if sc.is_empty() {
            continue;
        }
        let ir: Vec<usize> = sc.iter().filter(|s| s.ir).map(|s| s.bits()).collect();
        let ir_len = mode(&ir);
        let same = ir.iter().filter(|&&x| Some(x) == ir_len).count();
        let (ok01, _) = irt[tdo];
        let ls: Vec<f64> = tdi.iter().chain([&tdo]).map(|&i| conc[i]).collect();
        let lmin = ls.iter().copied().fold(f64::INFINITY, f64::min);
        let kept = (lmin / IN_SHIFT).clamp(0.0, 1.0);
        let info = 2.0 * ok01 as f64 + 3.0 * same.saturating_sub(1) as f64 + sc.len() as f64 * kept;
        let known = st.iter().filter(|&&s| s >= 0).count() as f64 / st.len() as f64;
        let score = (1.0 - 0.5f64.powf(info / EVIDENCE_BITS)) * kept * (1.0 - 0.5f64.powf(sc.len() as f64 / SCANS));
        if best.as_ref().is_some_and(|b| b.score >= score) {
            continue;
        }
        let mut roles = std::collections::BTreeMap::new();
        roles.insert("tck".to_string(), g.clock.to_string());
        roles.insert("tms".to_string(), lines[mi].name.to_string());
        if let Some(i) = tdi {
            roles.insert("tdi".to_string(), lines[i].name.to_string());
        }
        roles.insert("tdo".to_string(), lines[tdo].name.to_string());
        let mut params = Map::new();
        params.insert("sample_edge".into(), json!("rise"));
        if let Some(n) = ir_len {
            params.insert("ir_bits".into(), json!(n));
        }
        params.insert("clean".into(), json!(crate::clocked::CLEANS[g.how]));
        let mut evidence = Map::new();
        evidence.insert("scans".into(), json!(sc.len()));
        evidence.insert("ir_scans".into(), json!(ir.len()));
        evidence.insert("ir_bits".into(), json!(ir_len));
        evidence.insert("ir_same".into(), json!(same));
        evidence.insert("ir_capture_ok".into(), json!(ok01));
        evidence.insert("in_shift".into(), json!(lmin));
        evidence.insert("known".into(), json!(known));
        evidence.insert("evidence_bits".into(), json!(info));
        let layers = crate::claim::raw(serde_json::to_string(&json!({ "scans": scan_items(&sc, false) })).unwrap());
        best = Some(Candidate {
            protocol: "jtag".into(),
            roles,
            score,
            alt: None,
            params,
            evidence,
            layers,
        });
    }
    best
}

/// The JTAG readings of a part: each clock's group of data lines read on its rises.
pub fn candidates(view: &View) -> Vec<Candidate> {
    candidates_of(&groups(view))
}

pub fn candidates_of(gs: &[Group]) -> Vec<Candidate> {
    let mut best = Default::default();
    for g in gs {
        if let Some(c) = read(g) {
            keep_best(&mut best, c);
        }
    }
    best.into_values().collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::view::Channel;

    /// TMS, TDI, TDO bit sequences of IR / DR scans (each: IR?, bits, tdi, tdo), from Run-Test/Idle.
    pub(crate) fn session(scans: &[(bool, usize, u64, u64)]) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let (mut tms, mut tdi, mut tdo) = (vec![1, 1, 1, 1, 1, 0], vec![0; 6], vec![0; 6]);
        let mut push = |m: u8, i: u8, o: u8| {
            tms.push(m);
            tdi.push(i);
            tdo.push(o);
        };
        for &(ir, n, vi, vo) in scans {
            push(1, 0, 0); // RTI -> SDS
            if ir {
                push(1, 0, 0); // -> SIS
            }
            push(0, 0, 0); // -> capture
            push(0, 0, 0); // -> shift
            for k in 0..n {
                push((k + 1 == n) as u8, ((vi >> k) & 1) as u8, ((vo >> k) & 1) as u8);
            }
            push(1, 0, 0); // exit1 -> update
            push(0, 0, 0); // -> RTI
            push(0, 0, 0);
        }
        (tms, tdi, tdo)
    }

    /// Lines changing after the falls of a clock of period 10 (rises at 10 k + 5), from bit rows (each line's level
    /// before the part its first bit).
    pub(crate) fn lines_of(rows: &[&[u8]]) -> (Vec<i64>, Vec<Vec<i64>>, i64) {
        let n = rows[0].len();
        let mut clk = vec![];
        let mut out = vec![vec![]; rows.len()];
        for k in 0..n {
            let t = 100 + 10 * k as i64;
            for (r, o) in rows.iter().zip(out.iter_mut()) {
                let prev = if k == 0 { r[0] } else { r[k - 1] };
                if r[k] != prev {
                    o.push(t + 1);
                }
            }
            clk.push(t + 5);
            clk.push(t + 10);
        }
        (clk, out, 100 + 10 * n as i64 + 50)
    }

    #[test]
    fn reads_scans() {
        let sc = [
            (true, 5, 0x1e, 0x01),
            (false, 32, 0, 0x4ba00477),
            (true, 5, 0x0a, 0x05),
            (false, 8, 0x5a, 0xc3),
            (true, 5, 0x1f, 0x11),
        ];
        let (tms, tdi, tdo) = session(&sc);
        let (clk, l, n) = lines_of(&[&tms, &tdi, &tdo]);
        let v = View::new(
            1e6,
            n,
            vec![
                Channel::new("TCK", 0, &clk),
                Channel::new("TMS", 1, &l[0]),
                Channel::new("TDI", 0, &l[1]),
                Channel::new("TDO", 0, &l[2]),
            ],
        )
        .unwrap();
        let c = candidates(&v)
            .into_iter()
            .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
            .expect("a JTAG reading");
        assert_eq!(c.roles["tms"], "TMS");
        assert_eq!(c.roles["tdo"], "TDO");
        assert_eq!(c.roles["tdi"], "TDI");
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["scans"][1]["tdo"], json!("4ba00477"), "{lay}");
        assert_eq!(lay["scans"][3]["tdi"], json!("5a"));
        assert_eq!(c.params["ir_bits"], json!(5));
        assert!(c.score > 0.4, "{:?}", c.evidence); // (5 scans: 0.58 of the count's evidence)
    }
}
