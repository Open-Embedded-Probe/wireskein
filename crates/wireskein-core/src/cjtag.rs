//! cJTAG (IEEE 1149.7, Python's `_engine/cjtag.py`) on two lines, TCKC and TMSC, in the OScan1 format: JTAG's TAP
//! driven by bits on TMSC, three TCKC cycles a JTAG cycle (nTDI, TMS, TDO, sampled on TCKC's rise). Control is by
//! escapes, TMSC toggling while TCKC stays high (4-5 a deselection, 6-7 a selection, 8 or more a reset to the
//! four-wire mode). A selection is followed by the online activation (OAC, EC, CP, 4 bits each, LSB first); an OAC of
//! OScan1 starts its cycles. The TAP is walked as JTAG's over every JTAG cycle; the evidence is the OAC of OScan1
//! after a selection and, as for JTAG, the IR scans' captures (1 then 0 on TDO) and IR scans of one length.

use crate::Candidate;
use crate::clocked::keep_best;
use crate::jtag::{CAPTURES, EVIDENCE_BITS, SCANS, mode, scan_items, scans, walk};
use crate::spi::Clean;
use crate::sync::edges_to;
use crate::view::{Channel, View};
use serde_json::{Map, json};

pub const ESC_DESELECT: usize = 4;
pub const ESC_SELECT: usize = 6;
pub const ESC_RESET: usize = 8;
pub const OAC_OSCAN1: u32 = 0b1100;
pub const OTHER: f64 = 0.02; // TCKC intervals with 2-3 TMSC changes, this share at most
pub const OAC_BITS: usize = 12;
pub const QUARTER: f64 = 0.25; // pulses under this share of TCKC's half period are noise (an escape's toggles are 0.4 of it)

/// The JTAG cycles of a cJTAG pair.
#[derive(Default)]
struct Cycles {
    t: Vec<i64>,
    tms: Vec<u8>,
    tdi: Vec<u8>,
    tdo: Vec<u8>,
    oscan1: Vec<bool>,
    escapes: usize,
    activations: Vec<u32>,
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Standard,
    Oscan1,
    Other,
    Offline,
}

fn read_cjtag(ckc: &Channel, msc: &Channel, n: i64) -> Option<Cycles> {
    let ke = ckc.edges;
    if ke.len() < 8 {
        return None;
    }
    let rise = edges_to(ckc, 1);
    let fall = edges_to(ckc, 0);
    let me = msc.edges;
    // an escape: ESC_SELECT or more of TMSC's changes with no TCKC edge among them
    let at: Vec<usize> = me.iter().map(|&x| ke.partition_point(|&y| y < x)).collect();
    if at.len() < ESC_SELECT || !(0..=at.len() - ESC_SELECT).any(|j| at[j] == at[j + ESC_SELECT - 1]) {
        return None;
    }
    // outside escapes TMSC changes once between TCKC's edges at most
    let mut per = std::collections::HashMap::new();
    for &a in &at {
        *per.entry(a).or_insert(0usize) += 1;
    }
    let two = per.values().filter(|&&p| (2..ESC_DESELECT).contains(&p)).count() as f64;
    if two > OTHER * per.len() as f64 {
        return None;
    }
    let toggles: Vec<usize> = rise
        .iter()
        .map(|&r| {
            let j = fall.partition_point(|&f| f <= r);
            let end = if j < fall.len() { fall[j] } else { n };
            me.partition_point(|&x| x < end)
                .saturating_sub(me.partition_point(|&x| x <= r))
        })
        .collect();
    if !toggles.iter().any(|&g| (ESC_SELECT..ESC_RESET).contains(&g)) {
        return None;
    }
    let bit = crate::kernels::levels_at_sorted(msc, rise.iter().copied());
    let k = rise.len();
    let mut out = Cycles::default();
    let (mut md, mut i) = (Mode::Standard, 0usize);
    let run_to = |out: &mut Cycles, i: usize, stop: usize, md: Mode| {
        if stop <= i {
            return;
        }
        match md {
            Mode::Standard => {
                for j in i..stop {
                    out.t.push(rise[j]);
                    out.tms.push(bit[j]);
                    out.tdi.push(0);
                    out.tdo.push(0);
                    out.oscan1.push(false);
                }
            }
            Mode::Oscan1 => {
                let m3 = (stop - i) / 3 * 3;
                for j in (i..i + m3).step_by(3) {
                    out.t.push(rise[j]);
                    out.tms.push(bit[j + 1]);
                    out.tdi.push(1 - bit[j]);
                    out.tdo.push(bit[j + 2]);
                    out.oscan1.push(true);
                }
            }
            _ => {}
        }
    };
    let esc: Vec<usize> = (0..k).filter(|&j| toggles[j] >= ESC_DESELECT).chain([k]).collect();
    for e in esc {
        if e < i {
            continue;
        }
        run_to(&mut out, i, e, md);
        if e == k {
            break;
        }
        let tg = toggles[e];
        out.escapes += 1;
        i = e + 1;
        if tg >= ESC_RESET {
            md = Mode::Standard;
        } else if tg >= ESC_SELECT {
            if e + OAC_BITS < k && !toggles[e + 1..e + 1 + OAC_BITS].iter().any(|&g| g >= ESC_DESELECT) {
                let bb = &bit[e + 1..e + 1 + OAC_BITS];
                let oac = bb[..4]
                    .iter()
                    .enumerate()
                    .fold(0u32, |a, (j, &x)| a | ((x as u32) << j));
                out.activations.push(oac);
                md = if oac == OAC_OSCAN1 { Mode::Oscan1 } else { Mode::Other };
                i = e + 1 + OAC_BITS;
            }
        } else {
            md = Mode::Offline;
        }
    }
    Some(out)
}

fn read(view: &View, kc: &Channel, kd: &Channel, k: i64) -> Option<Candidate> {
    let r = read_cjtag(kc, kd, view.n_samples)?;
    if !r.oscan1.iter().any(|&x| x) {
        return None;
    }
    let st = walk(&r.tms);
    let sc: Vec<_> = scans(&st, &r.t, Some(&r.tdi), Some(&r.tdo), Some(&r.tms))
        .into_iter()
        .filter(|s| {
            let j = r.t.partition_point(|&x| x < s.s).min(r.t.len() - 1);
            r.oscan1[j]
        })
        .collect();
    if sc.is_empty() {
        return None;
    }
    let ir: Vec<usize> = sc.iter().filter(|s| s.ir).map(|s| s.tdi.len()).collect();
    let ir_len = mode(&ir);
    let same = ir.iter().filter(|&&x| Some(x) == ir_len).count();
    let ok01 = sc
        .iter()
        .filter(|s| s.ir && s.tdo.len() >= 2 && s.tdo[0] == 1 && s.tdo[1] == 0)
        .count();
    let n_osc = r.activations.iter().filter(|&&o| o == OAC_OSCAN1).count();
    if ok01 < CAPTURES {
        return None;
    }
    let info = 4.0 * n_osc as f64 + 2.0 * ok01 as f64 + 3.0 * same.saturating_sub(1) as f64;
    let share = n_osc as f64 / r.activations.len().max(1) as f64;
    let score = (1.0 - 0.5f64.powf(info / EVIDENCE_BITS)) * (1.0 - 0.5f64.powf(sc.len() as f64 / SCANS)) * share;
    let mut params = Map::new();
    params.insert("sample_edge".into(), json!("rise"));
    params.insert("cjtag".into(), json!("oscan1"));
    if let Some(n) = ir_len {
        params.insert("ir_bits".into(), json!(n));
    }
    if k > 0 {
        params.insert("deglitch".into(), json!(k));
    }
    let mut evidence = Map::new();
    evidence.insert("scans".into(), json!(sc.len()));
    evidence.insert("ir_scans".into(), json!(ir.len()));
    evidence.insert("ir_bits".into(), json!(ir_len));
    evidence.insert("ir_same".into(), json!(same));
    evidence.insert("ir_capture_ok".into(), json!(ok01));
    evidence.insert("escapes".into(), json!(r.escapes));
    evidence.insert("activations".into(), json!(r.activations.len()));
    evidence.insert("oscan1".into(), json!(n_osc));
    evidence.insert("evidence_bits".into(), json!(info));
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "scans": scan_items(&sc, true) })).unwrap());
    Some(Candidate {
        protocol: "jtag".into(),
        roles: [
            ("tckc".to_string(), kc.name.to_string()),
            ("tmsc".to_string(), kd.name.to_string()),
        ]
        .into(),
        score,
        alt: None,
        params,
        evidence,
        layers,
    })
}

pub const MERGE: f64 = 0.15; // changes closer than this share of TCKC's half period are one change's bounces

/// The cJTAG readings of a part: each pair of lines as TCKC and TMSC, as they are, with pulses under a quarter of
/// TCKC's half period taken out of both (a spike on TMSC while TCKC is high is an escape's toggle, a bounce on TCKC a
/// cycle more), and with changes closer than MERGE of it merged at the first (a bounce beside an escape's toggle,
/// which a pulse filter takes out with the next toggle); the half period from TCKC's longer runs (a bounced clock's
/// median run is a bounce's).
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut best = Default::default();
    let act: Vec<&Channel> = view.channels.iter().filter(|c| c.edges.len() >= 8).collect();
    for kc in &act {
        let half = crate::rebuild::half(kc.edges);
        let k = ((QUARTER * half) as i64).clamp(2, 64);
        let w = ((MERGE * half).round() as i64).max(2);
        let cleans: [(Clean, i64); 2] = [(crate::kernels::shortest_first, k), (crate::kernels::debounce, w)];
        let kes: Vec<Vec<i64>> = cleans.iter().map(|(f, x)| f(kc.edges, *x)).collect();
        for kd in &act {
            if kd.name == kc.name || kd.edges.len() < ESC_SELECT {
                continue;
            }
            if let Some(c) = read(view, kc, kd, 0) {
                keep_best(&mut best, c);
            }
            for ((f, x), ke) in cleans.iter().zip(&kes) {
                let de = f(kd.edges, *x);
                if ke.len() == kc.edges.len() && de.len() == kd.edges.len() {
                    continue;
                }
                let (c2, d2) = (
                    Channel::new(kc.name, kc.initial, ke),
                    Channel::new(kd.name, kd.initial, &de),
                );
                if let Some(c) = read(view, &c2, &d2, *x) {
                    keep_best(&mut best, c);
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
    fn reads_oscan1() {
        // a selection escape (6 toggles in a TCKC high), OAC = OScan1, EC, CP; then OScan1 cycles of a JTAG session
        let (tms, tdi, tdo) = crate::jtag::tests::session(&[
            (true, 4, 0x3, 0x1),
            (false, 8, 0xa5, 0x3c),
            (true, 4, 0xe, 0x5),
            (false, 8, 0x11, 0x22),
        ]);
        let mut cyc: Vec<u8> = vec![0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]; // OAC 1100 LSB first, EC, CP
        for j in 0..tms.len() {
            cyc.extend([1 - tdi[j], tms[j], tdo[j]]);
        }
        let (mut ck, mut ms, mut t, mut lv) = (vec![], vec![], 100i64, 1u8);
        // the escape while TCKC is high after a rise, then cycles (low half then high half, the rise at the middle)
        ck.push(t);
        for _ in 0..6 {
            ms.push(t + 2 + ms.len() as i64 % 6);
            lv ^= 1;
        }
        t += 20;
        for &b in &cyc {
            ck.push(t); // fall
            if b != lv {
                ms.push(t + 1);
                lv = b;
            }
            ck.push(t + 5); // rise
            t += 10;
        }
        let v = View::new(
            1e6,
            t + 100,
            vec![Channel::new("TCKC", 0, &ck), Channel::new("TMSC", 1, &ms)],
        )
        .unwrap();
        let c = candidates(&v).into_iter().next().expect("a cJTAG reading");
        assert_eq!(c.roles["tckc"], "TCKC");
        let lay: serde_json::Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["scans"][1]["tdi"], json!("a5"), "{lay}");
        assert_eq!(lay["scans"][1]["tdo"], json!("3c"));
        assert!(c.score > 0.3, "{:?}", c.evidence);
    }
}
