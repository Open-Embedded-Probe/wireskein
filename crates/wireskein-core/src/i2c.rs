//! I2C on two lines (R2 of bench/notes/2026-11-24-rust-plan.ja.md), after the idea of Python's declared I2C
//! (`decl/i2c.toml`): START / STOP where SDA moves while SCL is high, a bit at each rise of SCL, 9-bit words (8 data bits,
//! MSB first, and the ACK), a frame of 9n or 9n + 1 bits. The evidence is how many frames fit, against chance, with the
//! conditions clear of SCL's edges, SDA moving once or twice a bit, and SCL no faster than I2C allows.

use crate::Candidate;
use crate::kernels::median;
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const CHANCE: f64 = 0.3; // a frame's chance to fit
pub const SOFT: f64 = 0.25; // the side terms weigh this much
pub const MAX_CLOCK_HZ: f64 = 5e6;
pub const FREE_CLOCK: f64 = 0.5; // a free clock's edges inside chance "frames": about half of them (none counts) ...
pub const HELD: f64 = 0.9; // ... a master's clock inside its frames, this share or more (whole)
pub const CLEAR: f64 = 0.15; // a condition this share of the clock's local half from its edges at least is clear

/// P(X >= k) for X ~ Binomial(n, p), summed in logs (thousands of frames underflow the terms themselves).
pub fn tail(n: usize, k: usize, p: f64) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if k > n {
        return 0.0;
    }
    let (lp, lq) = (p.ln(), (1.0 - p).ln());
    // log C(n, k) p^k q^(n - k), then each next term by the ratio
    let mut lt =
        (1..=k).map(|i| ((n - k + i) as f64 / i as f64).ln()).sum::<f64>() + k as f64 * lp + (n - k) as f64 * lq;
    let mut terms = vec![lt];
    for i in k..n {
        lt += ((n - i) as f64 / (i + 1) as f64).ln() + lp - lq;
        terms.push(lt);
    }
    let m = terms.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (m.exp() * terms.iter().map(|t| (t - m).exp()).sum::<f64>()).min(1.0)
}

struct Frame {
    s: i64,
    e: i64,
    bits: Vec<u8>,
    start_cond: &'static str,
    end_cond: Option<&'static str>,
}

fn soft(x: f64) -> f64 {
    1.0 - SOFT * (1.0 - x.clamp(0.0, 1.0))
}

/// The frames on (scl, sda), and the side measures: (frames, conditions clear of SCL's edges, conditions, SCL lows
/// with at most two SDA changes, SCL lows inside frames, SDA's changes while SCL is low inside frames).
type Framed = (Vec<Frame>, usize, usize, usize, usize, usize);
fn frames(scl: &Channel, sda: &Channel) -> Framed {
    let (ce, de) = (scl.edges, sda.edges);
    let (mut i, mut j) = (0usize, 0usize);
    let (mut lc, mut ld) = (scl.initial, sda.initial);
    let mut out: Vec<Frame> = vec![];
    let mut cur: Option<Frame> = None;
    let (mut clear, mut conds, mut quiet, mut lows) = (0usize, 0usize, 0usize, 0usize);
    let mut sda_in_low = 0usize;
    let mut moved = 0usize;
    let near = |t: i64| -> bool {
        // nearer one of SCL's edges than CLEAR of the clock's local half, or than a sample (Python's
        // startstop_clearance: a clock of a few samples a period has its conditions a sample from its edges)
        if ce.len() < 4 {
            let k = ce.partition_point(|&x| x < t - 1);
            return k < ce.len() && ce[k] <= t + 1;
        }
        let j = ce.partition_point(|&x| x < t).clamp(1, ce.len() - 1);
        let d = (t - ce[j - 1]).abs().min((ce[j] - t).abs());
        let jj = j.clamp(2, ce.len() - 2);
        let half = (ce[jj] - ce[jj - 1])
            .min(ce[jj + 1] - ce[jj])
            .min(ce[jj - 1] - ce[jj - 2]);
        (d as f64) < (CLEAR * half as f64).max(1.0)
    };
    while i < ce.len() || j < de.len() {
        // at one tick, SCL's change first: SDA moving as SCL falls is data, not a condition
        let take_scl = j >= de.len() || (i < ce.len() && ce[i] <= de[j]);
        if take_scl {
            let t = ce[i];
            lc ^= 1;
            i += 1;
            if let Some(f) = cur.as_mut() {
                if lc == 1 {
                    f.bits.push(ld); // a bit at each rise
                    lows += 1;
                    if sda_in_low <= 2 {
                        quiet += 1;
                    }
                } else {
                    sda_in_low = 0;
                }
                f.e = t;
            }
        } else {
            let t = de[j];
            ld ^= 1;
            j += 1;
            if lc == 1 {
                // SDA moves while SCL is high: a condition
                conds += 1;
                if !near(t) {
                    clear += 1;
                }
                if ld == 0 {
                    let sr = cur.is_some();
                    if let Some(mut f) = cur.take() {
                        f.end_cond = Some("Sr");
                        f.e = t;
                        out.push(f);
                    }
                    cur = Some(Frame {
                        s: t,
                        e: t,
                        bits: vec![],
                        start_cond: if sr { "Sr" } else { "S" },
                        end_cond: None,
                    });
                    sda_in_low = 0;
                } else if let Some(mut f) = cur.take() {
                    f.end_cond = Some("P");
                    f.e = t;
                    out.push(f);
                }
            } else {
                sda_in_low += 1;
                if cur.is_some() {
                    moved += 1;
                }
            }
        }
    }
    if let Some(f) = cur.take() {
        out.push(f);
    }
    (out, clear, conds, quiet, lows, moved)
}

pub const BUS_FREE_ALLOW: usize = 2; // SCL's changes after a STOP before the next START, at most (a spike)
/// Whether a line's longest run is at its high level.
fn rests_at_high(c: &Channel, n: i64) -> bool {
    let (_, length, level) = crate::kernels::runs(c, n);
    length
        .iter()
        .zip(&level)
        .max_by_key(|(l, _)| **l)
        .is_some_and(|(_, &v)| v == 1)
}

pub const STATIC_SHARE: f64 = 0.95; // one value this share of the words at least ...
pub const STATIC_DATA: f64 = 0.1; // ... all 0s or all 1s: the reading keeps this share of its score
pub const STATIC_BITS: usize = 20; // or SDA changing less than once in this many bits inside the frames
pub const REST: f64 = 8.0; // half periods: a high run this long is a rest (a bounced clock's runs merge to a few)

/// Whether a clock has a high run of REST times its half period or more (the capture's ends included).
fn rests_high(scl: &Channel, n: i64) -> bool {
    let (_, length, level) = crate::kernels::runs(scl, n);
    if length.len() < 3 {
        return true;
    }
    // (its half period from its longer runs: a bounced clock's median run is a bounce's)
    let med = crate::rebuild::half(scl.edges);
    length
        .iter()
        .zip(&level)
        .any(|(&l, &v)| v == 1 && l as f64 >= REST * med)
}

fn fits(n: usize) -> bool {
    n >= 9 && (n.is_multiple_of(9) || n % 9 == 1)
}

/// The I2C reading of (scl, sda), or None where nothing frames.
pub fn read(view: &View, scl: &Channel, sda: &Channel) -> Option<Candidate> {
    let (frs, clear, conds, quiet, lows, moved) = frames(scl, sda);
    // every frame counts, a START and a STOP with no clock between too: SDA moves while SCL is high only for its
    // conditions on I2C, where another line idling beside SCL makes them by the hundred
    let used: Vec<&Frame> = frs.iter().collect();
    if used.iter().all(|f| f.bits.is_empty()) {
        return None;
    }
    // (a transfer the capture ends in counts beside ended ones only: alone, a START and a line held low after it -
    // a select beside a free clock - is no evidence of a length)
    let any_ended = used.iter().any(|f| f.end_cond.is_some());
    let ended: Vec<&&Frame> = used.iter().filter(|f| any_ended || f.end_cond.is_some()).collect();
    let ended: Vec<&&Frame> = if any_ended { ended } else { vec![] };
    let n = ended.len();
    let k = ended.iter().filter(|f| fits(f.bits.len())).count();
    // (no frame fitting: a reading of no weight, kept for what reads a named bus whatever its score - a decode
    // spec's, its lines told stopped or oscillating)
    let fit = if k == 0 { 0.0 } else { 1.0 - tail(n, k, CHANCE) };
    // SCL's rate inside the frames
    let mut per: Vec<f64> = vec![];
    for f in &used {
        let a = scl.edges.partition_point(|&x| x < f.s);
        let b = scl.edges.partition_point(|&x| x <= f.e);
        let rises: Vec<i64> = scl.edges[a..b]
            .iter()
            .enumerate()
            .filter(|&(q, _)| scl.initial ^ (((a + q + 1) & 1) as u8) == 1)
            .map(|(_, &x)| x)
            .collect();
        per.extend(rises.windows(2).map(|w| (w[1] - w[0]) as f64));
    }
    let hz = if per.is_empty() { 0.0 } else { view.rate / median(&per) };
    // (no I2C runs this fast: no weight, the reading kept as above)
    let fit = if hz > MAX_CLOCK_HZ * 1.2 { 0.0 } else { fit };
    let clearance = if conds > 0 { clear as f64 / conds as f64 } else { 0.0 };
    let once = if lows > 0 { quiet as f64 / lows as f64 } else { 0.0 };
    // the transactions
    let mut txs = vec![];
    let mut values = vec![];
    for f in &frs {
        let words = f.bits.len() / 9;
        let bytes: Vec<u8> = (0..words)
            .map(|w| f.bits[w * 9..w * 9 + 8].iter().fold(0u8, |a, &b| (a << 1) | b))
            .collect();
        let acks: Vec<bool> = (0..words).map(|w| f.bits[w * 9 + 8] == 0).collect();
        values.extend(bytes.iter().map(|&b| b as i64));
        let mut t = Map::new();
        t.insert("s".into(), json!(f.s));
        t.insert("e".into(), json!(f.e));
        if let Some(&b0) = bytes.first() {
            t.insert("addr".into(), json!(b0 >> 1));
            t.insert("rw".into(), json!(if b0 & 1 == 1 { "read" } else { "write" }));
            t.insert("addr_ack".into(), json!(acks[0]));
            let rest: String = bytes[1..].iter().map(|b| format!("{b:02x}")).collect();
            t.insert("bytes".into(), json!(rest));
            t.insert("acks".into(), json!(acks[1..]));
        } else {
            t.insert("addr".into(), Value::Null);
            t.insert("rw".into(), Value::Null);
            t.insert("addr_ack".into(), json!(false));
            t.insert("bytes".into(), json!(""));
            t.insert("acks".into(), json!([]));
        }
        t.insert("start_cond".into(), json!(f.start_cond));
        t.insert("end_cond".into(), f.end_cond.map_or(Value::Null, |c| json!(c)));
        if f.end_cond.is_none() {
            // (a transfer the capture ends in: its bits after the last whole word, as they came)
            t.insert("complete".into(), json!(false));
            t.insert("pending_bits".into(), json!(f.bits[words * 9..]));
        }
        txs.push(Value::Object(t));
    }
    // SDA saying one thing again and again is no data
    let varied = {
        let mut v = values.clone();
        v.sort();
        v.dedup();
        if values.len() >= 4 && v.len() == 1 { 0.5 } else { 1.0 }
    };
    // SCL rests high between a STOP and the next START (a master clocks only its transfers): a clock that never rests
    // high (no high run of REST of its median run, the capture's ends included) and runs on between the "frames" is
    // a free clock, its conditions another line's chance changes while it is high (a bounced bus's SDA ending frames
    // early leaves its clock outside them too, but it rests)
    let inside: usize = frs
        .iter()
        .map(|f| scl.edges.partition_point(|&x| x <= f.e) - scl.edges.partition_point(|&x| x < f.s))
        .sum();
    let held = inside as f64 / scl.edges.len().max(1) as f64;
    let rests = if rests_high(scl, view.n_samples) {
        1.0
    } else {
        ((held - FREE_CLOCK) / (HELD - FREE_CLOCK)).clamp(0.0, 1.0)
    };
    // the bus free between a STOP and the next START (Python's bus_free): of the STOPs, the share after which SCL
    // stays still (BUS_FREE_ALLOW changes at most: a spike) until the next START; a clock going on after them is
    // no master's (a free clock, or clocks with SDA high: no START, nothing to read)
    let stops: Vec<(i64, i64)> = frs
        .iter()
        .enumerate()
        .filter(|(_, f)| f.end_cond == Some("P"))
        .map(|(i, f)| (f.e, frs.get(i + 1).map_or(view.n_samples, |g| g.s)))
        // (a STOP the bus stays free after: SDA high until the next START - a bounce of SDA while SCL is high is a
        // "STOP" SDA leaves at once, no bus going free)
        .filter(|&(a, b)| sda.edges.partition_point(|&x| x < b) == sda.edges.partition_point(|&x| x <= a))
        .collect();
    let free = if stops.is_empty() {
        1.0
    } else {
        stops
            .iter()
            .filter(|&&(a, b)| {
                scl.edges.partition_point(|&x| x < b) - scl.edges.partition_point(|&x| x <= a) <= BUS_FREE_ALLOW
            })
            .count() as f64
            / stops.len() as f64
    };
    // every word one value, all 0s or all 1s (Python's static_data): a data line held low or high while a clock
    // runs - a select beside a free clock, one long "frame" of a START and a STOP - reads nothing
    let stat = {
        let mut c = std::collections::HashMap::new();
        for &v in &values {
            *c.entry(v).or_insert(0usize) += 1;
        }
        let top = c.iter().max_by_key(|(_, n)| **n).map(|(&v, &n)| (v, n));
        match top {
            Some((v, n))
                if values.len() >= 4 && n as f64 >= STATIC_SHARE * values.len() as f64 && (v == 0 || v == 0xFF) =>
            {
                STATIC_DATA
            }
            // (a word or a few, each 0x00 or each 0xFF: no address but the general call's or a reserved one, written
            // to with nothing - a select framing a data line's run of a sample or two by chance)
            Some((v, n)) if n == values.len() && (v == 0 || v == 0xFF) => STATIC_DATA,
            // (however the words go: SDA hardly moving while SCL is low inside the frames - less than once in
            // STATIC_BITS bits, the address and the acknowledges with them - is a line held, no data)
            _ if lows >= 9 && moved * STATIC_BITS < lows => STATIC_DATA,
            _ => 1.0,
        }
    };
    let mut score = fit * soft(clearance) * soft(once) * varied * stat * rests * free;
    // few addresses, repeatedly, acknowledged at least sometimes: a real bus's (Python's staged.i2c_addresses), its
    // support raising the score
    let addrs: Vec<(i64, bool)> = txs
        .iter()
        .filter_map(|t| Some((t["addr"].as_i64()?, t["addr_ack"].as_bool().unwrap_or(false))))
        .collect();
    let mut addresses = None;
    if addrs.len() >= 3 {
        let mut cnt = std::collections::HashMap::new();
        for &(a, _) in &addrs {
            *cnt.entry(a).or_insert(0usize) += 1;
        }
        let mut c: Vec<usize> = cnt.into_values().collect();
        c.sort_unstable_by(|a, b| b.cmp(a));
        let top2 = c.iter().take(2).sum::<usize>() as f64 / addrs.len() as f64;
        let acked = addrs.iter().any(|&(_, k)| k);
        let d = top2 * (1.0 - 0.5f64.powf(addrs.len() as f64 / 3.0)) * if acked { 1.0 } else { 0.3 };
        score += 0.5 * (1.0 - score) * d * score;
        addresses = Some(d);
    }
    let mut evidence = Map::new();
    for (k_, x) in [
        ("fit", fit),
        ("clearance", clearance),
        ("data_once_a_bit", once),
        ("scl_hz", hz),
        ("clock_in_frames", held),
        ("bus_free", free),
        ("static_data", stat),
    ] {
        evidence.insert(k_.into(), json!(x));
    }
    evidence.insert("frames".into(), json!(n));
    evidence.insert("frames_fit".into(), json!(k));
    if let Some(d) = addresses {
        evidence.insert("addresses".into(), json!((d * 1e4).round() / 1e4));
    }
    let layers = crate::claim::raw(serde_json::to_string(&json!({ "transactions": txs })).unwrap());
    Some(Candidate {
        protocol: "i2c".into(),
        roles: [
            ("scl".to_string(), scl.name.to_string()),
            ("sda".to_string(), sda.name.to_string()),
        ]
        .into(),
        score,
        alt: None,
        params: Map::new(),
        evidence,
        layers,
    })
}

pub const SETTLE: i64 = 7; // ticks: a change's bounces within this of it (`kernels::settled`)

/// The views a pair is read on: as captured, pulses under 2 and under 4 samples taken out (spikes, crosstalk), and
/// bounces merged at their first change - each chained to the one before (`debounce`), or within SETTLE ticks of the
/// first (`settled`: a fast clock, its bounces ringing on near its next change, where SCL's half is longer than
/// that); each (k, scl's edges, sda's edges), those that change nothing left out.
fn views(scl: &Channel, sda: &Channel) -> Vec<(i64, Vec<i64>, Vec<i64>)> {
    use crate::kernels::{debounce, deglitch, settled};
    let mut out = vec![(0, scl.edges.to_vec(), sda.edges.to_vec())];
    for (k, a, b) in [
        (2, deglitch(scl.edges, 2), deglitch(sda.edges, 2)),
        (4, deglitch(scl.edges, 4), deglitch(sda.edges, 4)),
        (8, debounce(scl.edges, 8), debounce(sda.edges, 8)),
        (SETTLE, settled(scl.edges, SETTLE), settled(sda.edges, SETTLE)),
    ] {
        // (a window from each change no shorter than SCL's half period takes its own next change with the bounces)
        if k == SETTLE && crate::rebuild::half(scl.edges) <= SETTLE as f64 {
            continue;
        }
        if out.iter().any(|(_, x, y)| *x == a && *y == b) {
            continue;
        }
        out.push((k, a, b));
    }
    out
}

/// The I2C readings of a part: every two lines both idling high, SCL changing no less than half as often as SDA (a
/// clock stopped part way: a glitch filter locked), each read on its views, the best kept.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    let act: Vec<&Channel> = view.channels.iter().filter(|c| c.edges.len() >= 4).collect();
    // each line as it settles (pulses under 4 samples out: spikes, crosstalk) and its idle level, for the pairing
    let settled: Vec<(usize, u8)> = act
        .iter()
        .map(|c| {
            (
                crate::kernels::deglitch(c.edges, 4).len(),
                crate::features::features(c, view.n_samples).idle,
            )
        })
        .collect();
    // each line rebuilt as a clock from its period where its short runs are a bounce's (each way, those that differ),
    // and each with its bounces merged as beside a rebuilt clock
    let rebuilt: Vec<Vec<(&str, Vec<i64>)>> = act
        .iter()
        .map(|c| {
            let mut v: Vec<(&str, Vec<i64>)> = vec![];
            if crate::spi::bounced(c.edges) {
                for (name, e) in [
                    ("rebuilt_refused", crate::rebuild::refused(c.edges, c.initial)),
                    ("rebuilt_followed", crate::rebuild::followed(c.edges, c.initial)),
                    ("rebuilt_gridded", crate::rebuild::gridded(c.edges, c.initial)),
                ] {
                    if e.len() >= 16 && !v.iter().any(|(_, x)| *x == e) {
                        v.push((name, e));
                    }
                }
            }
            v
        })
        .collect();
    let merged: Vec<Vec<i64>> = act
        .iter()
        .map(|c| crate::kernels::debounce(c.edges, crate::spi::REBUILD_DATA))
        .collect();
    for (ia, a) in act.iter().enumerate() {
        for (ib, b) in act.iter().enumerate() {
            // (counted as the lines settle and as they are: a clock of a few samples a half loses its own runs to
            // the settling)
            let fewer = 2 * settled[ia].0 < settled[ib].0 && 2 * a.edges.len() < b.edges.len();
            // (idling high, or high as the capture starts - a bus left held low at its end: a target stopped mid
            // byte - or resting high at its longest run - a piece of a busy bus, SDA low more than high)
            let high = |i: usize, c: &Channel| settled[i].1 == 1 || c.initial == 1 || rests_at_high(c, view.n_samples);
            if ia == ib || fewer || !high(ia, a) || !high(ib, b) {
                continue;
            }
            let mut best: Option<Candidate> = None;
            for (k, ea, eb) in views(a, b) {
                let (ca, cb) = (
                    Channel::new(a.name, a.initial, &ea),
                    Channel::new(b.name, b.initial, &eb),
                );
                if let Some(mut c) = read(view, &ca, &cb)
                    && best.as_ref().is_none_or(|x| c.score > x.score)
                {
                    c.params.insert("deglitch".into(), json!(k));
                    best = Some(c);
                }
            }
            // SCL rebuilt from its period, SDA's bounces merged (Python's rebuilt views of a group, SPI's here): a
            // clock of a few samples a half bouncing through it has no width a filter tells from its own runs
            for (name, ea) in &rebuilt[ia] {
                let (ca, cb) = (
                    Channel::new(a.name, a.initial, ea),
                    Channel::new(b.name, b.initial, &merged[ib]),
                );
                if let Some(mut c) = read(view, &ca, &cb)
                    && best.as_ref().is_none_or(|x| c.score > x.score)
                {
                    c.params.insert("deglitch".into(), json!(crate::spi::REBUILD_DATA));
                    c.params.insert("clean".into(), json!(name));
                    best = Some(c);
                }
            }
            out.extend(best);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tails() {
        assert!((tail(1, 1, 0.3) - 0.3).abs() < 1e-12);
        assert!((tail(2, 2, 0.3) - 0.09).abs() < 1e-12);
        assert!((tail(3, 1, 0.3) - (1.0 - 0.343)).abs() < 1e-12);
        assert!(tail(4113, 49, 0.3) > 0.999_999);
        assert!(tail(4113, 4000, 0.3) < 1e-300);
    }

    /// I2C writes: (addr, bytes), SCL period 2 * h ticks.
    fn i2c(txs: &[(u8, &[u8])], h: i64) -> (Vec<i64>, Vec<i64>, i64) {
        let (mut scl, mut sda, mut t) = (vec![], vec![], 100i64);
        let (mut lc, mut ld) = (1u8, 1u8);
        let set = |v: &mut Vec<i64>, l: &mut u8, x: u8, t: i64| {
            if *l != x {
                v.push(t);
                *l = x;
            }
        };
        for (addr, data) in txs {
            set(&mut sda, &mut ld, 0, t); // START
            t += h;
            set(&mut scl, &mut lc, 0, t);
            let bytes: Vec<u8> = std::iter::once(addr << 1).chain(data.iter().copied()).collect();
            for b in bytes {
                for k in (0..8).map(|k| (b >> (7 - k)) & 1).chain([0]) {
                    t += h / 2;
                    set(&mut sda, &mut ld, k, t);
                    t += h / 2;
                    set(&mut scl, &mut lc, 1, t);
                    t += h;
                    set(&mut scl, &mut lc, 0, t);
                }
            }
            t += h / 2;
            set(&mut sda, &mut ld, 0, t);
            t += h / 2;
            set(&mut scl, &mut lc, 1, t);
            t += h;
            set(&mut sda, &mut ld, 1, t); // STOP
            t += 4 * h;
        }
        (scl, sda, t + 10 * h)
    }

    #[test]
    fn reads_writes() {
        let (scl, sda, n) = i2c(&[(0x50, &[1, 2, 3]), (0x50, &[4]), (0x1d, &[0xaa, 0x55])], 10);
        let v = View::new(2e6, n, vec![Channel::new("SCL", 1, &scl), Channel::new("SDA", 1, &sda)]).unwrap();
        let cs = candidates(&v);
        let c = cs.iter().find(|c| c.roles["scl"] == "SCL").expect("an I2C reading");
        assert!(c.score > 0.9, "{:?}", c.evidence);
        let lay: Value = serde_json::from_str(c.layers.get()).unwrap();
        assert_eq!(lay["transactions"][0]["addr"], json!(0x50));
        assert_eq!(lay["transactions"][2]["bytes"], json!("aa55"));
    }
}
