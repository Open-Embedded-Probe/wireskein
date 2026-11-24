//! WCH's RVSWD, two wires (Python's `_engine/rvswd.py` with `decl/rvswd.toml`, read by `declarative.py`): DIO read
//! at SWCLK's rises, frames from START (DIO falling while SWCLK is high) to STOP (DIO rising while SWCLK is high, SWCLK
//! then held high long), or at the clock's gaps in captures that hide them. Each frame a RISC-V DMI access:
//!
//! - short (53 clocks): addr7, R/W (1 = write), header parity, park, ctl4, data32, data parity, park, status2, pad2,
//!   stop; a 54-clock read has one turnaround clock more (data at 15..46, its parity at 47);
//! - a burst (38n + 15, 91 or more): data0's address, then words of 32 bits and 6, one stop bit;
//! - long (85): the host's addr7 data32 op2 and a 0, the target's addr7 (echoed) data32 status2, bit 83 (the op's low
//!   bit), a last clock; a target that does not answer leaves the line high (its bits all 1);
//! - a LinkE's query (80): structure only.
//!
//! The parities and the long form's echoes are the evidence.

use crate::Candidate;
use crate::kernels::median;
use crate::pulse::{candidate, obj};
use crate::sync::{edges_to, spikes_out_where_slow};
use crate::view::{Channel, View};
use serde_json::{Map, Value, json};

pub const RING: i64 = 3; // samples: at a clock slow enough, a data line's pulses this short are a reflection's
const ON_EDGE_SHARE: f64 = 0.1; // a data line's changes on the sampling edge's tick, this share: read before it ...
const ON_EDGE_OVER: f64 = 3.0; // ... and this many times its changes on the other edge's
const JOIN_GAP: usize = 160; // clocks between two pieces of one burst split by a STOP misread
const BURST_HEAD: [u8; 7] = [0, 0, 0, 0, 1, 0, 0]; // a burst reads data0: its header opens with data0's address
const RESTORE_MARGIN: (f64, f64) = (1.5, 4.0);
const CLOCK_HZ: (f64, f64) = (10e3, 1e9);

/// The RISC-V Debug Module's registers by address (and those WCH puts in its address space).
pub fn dm_name(a: u32) -> Option<String> {
    Some(match a {
        0x04..=0x07 => format!("data{}", a - 4),
        0x10 => "dmcontrol".into(),
        0x11 => "dmstatus".into(),
        0x12 => "hartinfo".into(),
        0x13 => "haltsum1".into(),
        0x14 => "hawindowsel".into(),
        0x15 => "hawindow".into(),
        0x16 => "abstractcs".into(),
        0x17 => "command".into(),
        0x18 => "abstractauto".into(),
        0x19 => "confstrptr0".into(),
        0x1D => "nextdm".into(),
        0x30 => "authdata".into(),
        0x38 => "sbcs".into(),
        0x39 => "sbaddress0".into(),
        0x3A => "sbaddress1".into(),
        0x3C => "sbdata0".into(),
        0x3D => "sbdata1".into(),
        0x40 => "haltsum0".into(),
        0x20..=0x2F => format!("progbuf{}", a - 0x20),
        0x7C..=0x7F => format!("wch_{a:02x}"),
        _ => return None,
    })
}

fn msb(b: &[u8]) -> u32 {
    b.iter().fold(0u32, |a, &x| (a << 1) | x as u32)
}

fn parity(b: &[u8]) -> u8 {
    b.iter().fold(0u8, |a, &x| a ^ x)
}

/// A frame: the sampling edges [a, b), those dropped (a glitch's clock) and those restored (at, bit, tick).
#[derive(Debug, Clone, PartialEq)]
struct Frame {
    a: usize,
    b: usize,
    skip: Vec<usize>,
    ins: Vec<(usize, u8, i64)>,
}

impl Frame {
    fn new(a: usize, b: usize) -> Self {
        Frame {
            a,
            b,
            skip: vec![],
            ins: vec![],
        }
    }
    fn plain(&self) -> bool {
        self.skip.is_empty() && self.ins.is_empty()
    }
}

/// A length a START / STOP frame has: short 53, long 85, the LinkE's query 80, a burst 38n + 15.
fn known_length(n: usize) -> bool {
    n == 53 || n == 85 || n == 80 || burst_length(n)
}

fn burst_length(n: usize) -> bool {
    n >= 91 && (n - 15).is_multiple_of(38)
}

/// The data line's level at each sampling edge (read before the edge where the line changes on it as a driver
/// clocked by it does), and its changes with a reflection's pulses out where the clock is slow.
fn sample(dio: &Channel, t: &[i64], t_other: &[i64]) -> (Vec<u8>, Vec<i64>) {
    let e = if t.len() > 1 {
        spikes_out_where_slow(dio.edges, RING, t)
    } else {
        dio.edges.to_vec()
    };
    let (Some(&t0), Some(&t1)) = (t.first(), t.last()) else {
        return (vec![], e);
    };
    let inside: Vec<i64> = e.iter().copied().filter(|&x| x >= t0 && x <= t1).collect();
    let at = |table: &[i64]| inside.iter().filter(|x| table.binary_search(x).is_ok()).count() as f64;
    let (share, other) = if inside.is_empty() {
        (0.0, 0.0)
    } else {
        (at(t) / inside.len() as f64, at(t_other) / inside.len() as f64)
    };
    let left = share >= ON_EDGE_SHARE && other <= share / ON_EDGE_OVER;
    let bits = t
        .iter()
        .map(|&x| {
            let k = if left {
                e.partition_point(|&y| y < x)
            } else {
                e.partition_point(|&y| y <= x)
            };
            dio.initial ^ (k & 1) as u8
        })
        .collect();
    (bits, e)
}

/// Frames from START to STOP (`rvswd.reframe_startstop`): START is DIO falling while CLK is high outside a frame;
/// STOP is DIO rising while CLK is high, CLK then held high over `HOLD` times its usual high (the median of the 16
/// highs before) and 1.5 us at least, or `FAR` times and 1 us at least. Frames split by a STOP misread inside a burst
/// are joined, a glitch's clock dropped, a clock lost between samples restored.
fn startstop(clk: &Channel, de: &[i64], dinit: u8, rises: &[i64], falls: &[i64], bits: &[u8], rate: f64) -> Vec<Frame> {
    const HOLD: f64 = 1.5;
    const W: usize = 16;
    const HOLD_MIN_S: f64 = 1.5e-6;
    const FAR: f64 = 4.0;
    const FAR_MIN_S: f64 = 1e-6;
    if clk.edges.len() < 4 || de.len() < 2 {
        return vec![];
    }
    let highs: Vec<f64> = rises
        .iter()
        .map(|&r| {
            let j = falls.partition_point(|&f| f <= r);
            if j < falls.len() {
                (falls[j] - r) as f64
            } else {
                f64::INFINITY
            }
        })
        .collect();
    let (mut starts, mut ups) = (vec![], vec![]);
    for (i, &x) in de.iter().enumerate() {
        if clk.level_at(x) != 1 {
            continue;
        }
        let lv = dinit ^ (((i + 1) & 1) as u8);
        if lv == 0 {
            starts.push(x);
        } else {
            ups.push(x);
        }
    }
    if starts.is_empty() {
        return vec![];
    }
    let mut stops = vec![];
    for &u in &ups {
        let k = rises.partition_point(|&r| r <= u);
        if k == 0 {
            continue;
        }
        let k = k - 1;
        let prev: Vec<f64> = highs[k.saturating_sub(W)..k]
            .iter()
            .copied()
            .filter(|h| h.is_finite())
            .collect();
        let usual = if prev.is_empty() { f64::INFINITY } else { median(&prev) };
        let h = highs[k];
        let long = (h > HOLD * usual && h >= HOLD_MIN_S * rate) || (h >= FAR * usual && h >= FAR_MIN_S * rate);
        if !h.is_finite() || long {
            stops.push(u);
        }
    }
    let mut out = vec![];
    let mut after = -1i64;
    for &s in &stops {
        let j = starts.partition_point(|&x| x <= after);
        if j < starts.len() && starts[j] < s {
            let a = rises.partition_point(|&r| r <= starts[j]);
            let b = rises.partition_point(|&r| r <= s);
            if b > a {
                out.push(Frame::new(a, b));
            }
        }
        after = s;
    }
    let out = joined(out, |a| a + 7 <= bits.len() && bits[a..a + 7] == BURST_HEAD);
    let out: Vec<Frame> = out.into_iter().map(|f| repaired(f, &highs)).collect();
    restored(out, rises, dinit, de)
}

/// Two frames next to each other, neither of a known length, at most JOIN_GAP clocks apart, together a burst's
/// length with the first opening as a burst does: one frame (a STOP misread inside a burst at the LinkE's low speed).
fn joined(mut out: Vec<Frame>, opens_burst: impl Fn(usize) -> bool) -> Vec<Frame> {
    let mut i = 0;
    while i + 1 < out.len() {
        let (f1, f2) = (&out[i], &out[i + 1]);
        if !known_length(f1.b - f1.a)
            && !known_length(f2.b - f2.a)
            && f2.a >= f1.b
            && f2.a - f1.b <= JOIN_GAP
            && burst_length(f2.b - f1.a)
            && opens_burst(f1.a)
        {
            let f = Frame::new(f1.a, f2.b);
            out.splice(i..i + 2, [f]);
        }
        i += 1;
    }
    out
}

/// A frame of a length no form has, with as many clocks too many as it has highs of a tick or less (a reflection's
/// glitch on the clock): those dropped.
fn repaired(f: Frame, highs: &[f64]) -> Frame {
    let n = f.b - f.a;
    if known_length(n) {
        return f;
    }
    let short: Vec<usize> = (f.a..f.b).filter(|&i| highs[i] <= 1.0).collect();
    for m in 1..=3usize {
        if n > m && known_length(n - m) && short.len() >= m {
            let mut s = short.clone();
            s.sort_by(|&x, &y| highs[x].partial_cmp(&highs[y]).unwrap());
            let mut drop: Vec<usize> = s[..m].to_vec();
            drop.sort_unstable();
            return Frame { skip: drop, ..f };
        }
    }
    f
}

/// A frame's bits passing its form's checks (short: its two parities; long: its echoes).
fn form_checks(f: &[u8], n: usize) -> bool {
    match n {
        53 => f[8] == parity(&f[0..8]) && f[46] == parity(&f[14..46]),
        85 => f[0..7] == f[42..49] && f[40] == f[83],
        _ => false,
    }
}

/// Frames a clock short of a fixed form (52 or 84) whose clock lost a high between two samples: the clock restored
/// where the frame's intervals best match the form's (the median, position by position, of the 16 whole frames of
/// that length nearest, 4 at least), where every place about as likely gives the same bits and they pass the checks.
fn restored(mut out: Vec<Frame>, t: &[i64], dinit: u8, de: &[i64]) -> Vec<Frame> {
    const LEAST: usize = 4;
    const NEAR: usize = 16;
    let level = |tick: i64| dinit ^ (de.partition_point(|&x| x <= tick) & 1) as u8;
    for n in [53usize, 85] {
        let whole: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].plain() && out[i].b - out[i].a == n)
            .collect();
        let short: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].plain() && out[i].b - out[i].a == n - 1)
            .collect();
        if whole.len() < LEAST || short.is_empty() {
            continue;
        }
        let starts: Vec<usize> = whole.iter().map(|&i| out[i].a).collect();
        let iv_all: Vec<Vec<f64>> = starts
            .iter()
            .map(|&s| (0..n - 1).map(|j| (t[s + j + 1] - t[s + j]) as f64).collect())
            .collect();
        for &i in &short {
            let (a, b) = (out[i].a, out[i].b);
            let k = starts.partition_point(|&s| s < a);
            let lo = (k as i64 - (NEAR / 2) as i64)
                .min(starts.len() as i64 - NEAR as i64)
                .max(0) as usize;
            let hi = (lo + NEAR).min(starts.len());
            let tt: Vec<f64> = (0..n - 1)
                .map(|j| median(&iv_all[lo..hi].iter().map(|r| r[j]).collect::<Vec<_>>()))
                .collect();
            let iv: Vec<f64> = (a..b - 1).map(|j| (t[j + 1] - t[j]) as f64).collect(); // n - 2
            let mut pre = vec![0.0; n - 1];
            for j in 0..n - 2 {
                pre[j + 1] = pre[j] + (iv[j] - tt[j]).abs();
            }
            let mut post = vec![0.0; n - 1];
            for j in (0..n - 2).rev() {
                post[j] = post[j + 1] + (iv[j] - tt[j + 1]).abs();
            }
            let cost: Vec<f64> = (0..n - 2)
                .map(|j| pre[j] + (iv[j] - (tt[j] + tt[j + 1])).abs() + post[j + 1])
                .collect();
            let lv: Vec<u8> = (a..b).map(|j| level(t[j])).collect();
            let best = cost.iter().copied().fold(f64::INFINITY, f64::min);
            let mut order: Vec<usize> = (0..cost.len()).collect();
            order.sort_by(|&x, &y| cost[x].partial_cmp(&cost[y]).unwrap());
            let lim = RESTORE_MARGIN.0 * best + RESTORE_MARGIN.1 * median(&tt);
            let mut cands: Vec<(usize, u8, i64, Vec<u8>)> = vec![];
            for jb in order {
                if cost[jb] > lim {
                    break;
                }
                let tick = t[a + jb] + tt[jb].round_ties_even() as i64;
                let bit = level(tick);
                let mut f = lv.clone();
                f.insert(jb + 1, bit);
                cands.push((jb, bit, tick, f));
            }
            if let Some(c0) = cands.first()
                && cands.iter().all(|c| c.3 == c0.3)
                && form_checks(&c0.3, n)
            {
                out[i].ins = vec![(c0.0 + 1, c0.1, c0.2)];
            }
        }
    }
    out
}

/// Frames at the clock's gaps (`kernels.burst_split`), each cut where an interval is over 2.5 times its median, a
/// trailing stop clock after an interval over 1.8 times it dropped (`rvswd.reframe`): captures that hide START and
/// STOP.
fn gap_frames(t: &[i64]) -> Vec<Frame> {
    if t.len() < 3 {
        return vec![];
    }
    let iv: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let mut bounds = vec![0usize];
    for i in 0..iv.len() {
        let prev = if i > 0 { iv[i - 1] } else { f64::INFINITY };
        let next = if i + 1 < iv.len() { iv[i + 1] } else { f64::INFINITY };
        if iv[i] > 3.0 * prev.min(next) {
            bounds.push(i + 1);
        }
    }
    bounds.push(t.len());
    let mut out = vec![];
    for z in bounds.windows(2) {
        let (a, b) = (z[0], z[1]);
        if b - a < 8 {
            out.push(Frame::new(a, b));
            continue;
        }
        let med = median(&iv[a..b - 1]);
        let mut cuts = vec![a];
        cuts.extend((a..b - 1).filter(|&j| iv[j] > 2.5 * med).map(|j| j + 1));
        cuts.push(b);
        for c in cuts.windows(2) {
            let (x, mut y) = (c[0], c[1]);
            if y - x >= 3 && (t[y - 1] - t[y - 2]) as f64 > 1.8 * med {
                y -= 1;
            }
            if y > x {
                out.push(Frame::new(x, y));
            }
        }
    }
    out
}

struct Reading {
    recs: Vec<Map<String, Value>>,
    checked: usize,
    passed: usize,
    matched: usize,
    checked_frames: usize,
    explained: usize,
}

/// The frames read by their layouts.
fn read_frames(frames: &[Frame], delimited: bool, bits: &[u8], t: &[i64], rate: f64, dio: &Channel) -> Reading {
    let mut r = Reading {
        recs: vec![],
        checked: 0,
        passed: 0,
        matched: 0,
        checked_frames: 0,
        explained: 0,
    };
    for fr in frames {
        let keep: Vec<usize> = (fr.a..fr.b).filter(|i| !fr.skip.contains(i)).collect();
        let mut f: Vec<u8> = keep.iter().map(|&i| bits[i]).collect();
        let mut ft: Vec<i64> = keep.iter().map(|&i| t[i]).collect();
        for &(at, bit, tick) in &fr.ins {
            f.insert(at, bit);
            ft.insert(at, tick);
        }
        let n = f.len();
        let form_ok = matches!(n, 53 | 54 | 85 | 80) || (n == 52 && !delimited) || burst_length(n);
        if !form_ok {
            continue;
        }
        r.matched += 1;
        r.explained += fr.b - fr.a;
        if n == 80 {
            continue; // the LinkE's query: structure only
        }
        let mut rec = obj(vec![("s", json!(t[fr.a])), ("e", json!(t[fr.b - 1]))]);
        let mut goods: Vec<bool> = vec![];
        let mut answered = true;
        let (op, addr, data, status, form): (Value, u32, Value, Value, &str) = match n {
            52 | 53 => {
                goods.push(f[8] == parity(&f[0..8]));
                goods.push(f[46] == parity(&f[14..46]));
                (
                    json!(if f[7] == 1 { "W" } else { "R" }),
                    msb(&f[0..7]),
                    json!(msb(&f[14..46])),
                    json!(msb(&f[48..50])),
                    "short",
                )
            }
            54 => {
                goods.push(f[47] == parity(&f[15..47]));
                (
                    json!("R"),
                    msb(&f[1..8]),
                    json!(msb(&f[15..47])),
                    json!(msb(&f[49..51])),
                    "short",
                )
            }
            85 => {
                let op = match msb(&f[39..41]) {
                    0 => json!("NOP"),
                    1 => json!("R"),
                    2 => json!("W"),
                    x => json!(x),
                };
                if f[49..=82].iter().all(|&x| x == 1) {
                    answered = false;
                    (op, msb(&f[0..7]), Value::Null, Value::Null, "long")
                } else {
                    goods.push(f[0..7] == f[42..49]);
                    goods.push(f[40] == f[83]);
                    (
                        op,
                        msb(&f[0..7]),
                        json!(msb(&f[49..81])),
                        json!(msb(&f[81..83])),
                        "long",
                    )
                }
            }
            _ => {
                // a burst: data0 read again and again
                let k = (n - 14 - 1) / 38;
                let words: Vec<u32> = (0..k).map(|j| msb(&f[14 + 38 * j..14 + 38 * j + 32])).collect();
                (json!("BURST"), msb(&f[0..7]), json!(words), json!(0), "short")
            }
        };
        rec.insert("op".into(), op);
        rec.insert("addr".into(), json!(addr));
        rec.insert("reg".into(), json!(dm_name(addr)));
        rec.insert("data".into(), data);
        let ok = answered && goods.iter().all(|&g| g);
        rec.insert("ok".into(), json!(ok));
        rec.insert("form".into(), json!(form));
        rec.insert("status".into(), status);
        if !answered {
            rec.insert("answered".into(), json!(false));
        } else {
            if !goods.is_empty() {
                r.checked_frames += 1;
            }
            r.checked += goods.len();
            r.passed += goods.iter().filter(|&&g| g).count();
        }
        if ft.len() > 1 {
            let d: Vec<f64> = ft.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            rec.insert(
                "period_ns".into(),
                json!((median(&d) / rate * 1e9 * 10.0).round_ties_even() / 10.0),
            );
        }
        let bs: String = f.iter().map(|&b| if b == 1 { '1' } else { '0' }).collect();
        if let Some(fl) = frame_fields(&bs, form, rec["op"] == json!("BURST")) {
            rec.insert("fields".into(), Value::Object(fl));
        }
        rec.insert("bits".into(), json!(bs));
        if !fr.skip.is_empty() {
            rec.insert(
                "repaired".into(),
                json!(fr.skip.iter().map(|&i| t[i]).collect::<Vec<_>>()),
            );
        }
        if !fr.ins.is_empty() {
            rec.insert("restored".into(), json!(fr.ins.iter().map(|x| x.2).collect::<Vec<_>>()));
        }
        if form == "long" && answered && fr.plain() {
            long_answer(&mut rec, &f, &t[fr.a..t.len().min(fr.b + 1)], dio, rate);
        }
        r.recs.push(rec);
    }
    r
}

const LONG_TARGET: usize = 42; // the first clock of the target's phase in the long form

/// When the target answered a long frame (`rvswd.long_answer`), from what it should send: the host's address echoed
/// (and, after a write, the host's data). At each bit where that changes, DIO's change to it after the rising edge
/// before that bit; its delay in that clock period (0 to 1: in time for the bit's rising edge; over 1: after it, the
/// bit sampled late). `answer_delay`: the median and max over those changes, in periods; `answer_delay_ns` the same
/// in ns; `late`: 1 when the median is over 1 (the answer slid a clock, as a CH32V103 does at times while it is
/// reset: its echoed address then reads as another). `r`: the frame's rises and the one after.
fn long_answer(rec: &mut Map<String, Value>, f: &[u8], r: &[i64], dio: &Channel, rate: f64) {
    if f.len() < 83 {
        return;
    }
    let op = f[39] * 2 + f[40];
    let want: &[u8] = if op == 2 { &f[0..39] } else { &f[0..7] };
    if r.len() < LONG_TARGET + want.len() + 1 {
        return;
    }
    let de = dio.edges;
    let level = |j: usize| dio.initial ^ ((j + 1) & 1) as u8; // the line after its change j
    let (mut delays, mut ns) = (vec![], vec![]);
    for i in 1..want.len() {
        if want[i] == want[i - 1] {
            continue;
        }
        let p = LONG_TARGET + i; // the bit that changes; its rising edge r[p], the one before r[p - 1]
        let (r0, r1) = (r[p - 1], r[p]);
        let mut j = de.partition_point(|&x| x <= r0);
        while j < de.len() && level(j) != want[i] {
            j += 1;
        }
        if j >= de.len() || de[j] > r[(p + 1).min(r.len() - 1)] {
            continue; // not changed within two clocks: no answer to time
        }
        delays.push((de[j] - r0) as f64 / (r1 - r0) as f64);
        ns.push((de[j] - r0) as f64 / rate * 1e9);
    }
    if delays.is_empty() {
        return;
    }
    let round = |x: f64, k: f64| (x * k).round_ties_even() / k;
    let most = |v: &[f64]| v.iter().copied().fold(f64::MIN, f64::max);
    let med = median(&delays);
    rec.insert(
        "answer_delay".into(),
        json!({"median": round(med, 1e3), "max": round(most(&delays), 1e3)}),
    );
    rec.insert(
        "answer_delay_ns".into(),
        json!({"median": round(median(&ns), 10.0), "max": round(most(&ns), 10.0)}),
    );
    if med > 1.0 {
        rec.insert("late".into(), json!(1));
    }
}

/// A frame's fields by name, as bit strings (MSB first), its checks one by one (`rvswd.frame_fields`).
fn frame_fields(b: &str, form: &str, burst: bool) -> Option<Map<String, Value>> {
    let par = |x: &str| x.bytes().filter(|&c| c == b'1').count() & 1;
    let bit = |i: usize| &b[i..i + 1];
    if form == "short" && b.len() == 53 {
        return Some(obj(vec![
            ("addr", json!(&b[0..7])),
            ("rw", json!(bit(7))),
            ("header_parity", json!(bit(8))),
            ("park", json!(bit(9))),
            ("ctl", json!(&b[10..14])),
            ("data", json!(&b[14..46])),
            ("data_parity", json!(bit(46))),
            ("park2", json!(bit(47))),
            ("status", json!(&b[48..50])),
            ("pad", json!(&b[50..52])),
            ("stop", json!(bit(52))),
            ("header_parity_ok", json!(par(&b[0..8]) == (bit(8) == "1") as usize)),
            ("data_parity_ok", json!(par(&b[14..46]) == (bit(46) == "1") as usize)),
        ]));
    }
    if burst && burst_length(b.len()) {
        let n = (b.len() - 15) / 38;
        return Some(obj(vec![
            ("header", json!(&b[0..14])),
            (
                "aux",
                json!((0..n - 1).map(|i| &b[46 + 38 * i..52 + 38 * i]).collect::<Vec<_>>()),
            ),
            ("trailer", json!(&b[14 + 38 * n - 6..])),
        ]));
    }
    if form == "long" && b.len() == 85 {
        return Some(obj(vec![
            ("host_addr", json!(&b[0..7])),
            ("host_data", json!(&b[7..39])),
            ("op", json!(&b[39..41])),
            ("bit41", json!(bit(41))),
            ("target_addr", json!(&b[42..49])),
            ("target_data", json!(&b[49..81])),
            ("status", json!(&b[81..83])),
            ("bit83", json!(bit(83))),
            ("last", json!(bit(84))),
        ]));
    }
    None
}

/// The RVSWD reading of SWCLK and DIO, or None.
fn read(view: &View, clk: &Channel, dio: &Channel) -> Option<Candidate> {
    let rate = view.rate;
    let rises = edges_to(clk, 1);
    let falls = edges_to(clk, 0);
    if rises.len() < 16 {
        return None;
    }
    // SWCLK as a LinkE drives it (80 kHz to 1 MHz and faster)
    let iv: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let hz = rate / median(&iv);
    if !(CLOCK_HZ.0..=CLOCK_HZ.1).contains(&hz) {
        return None;
    }
    let (bits, de) = sample(dio, &rises, &falls);
    let ss = startstop(clk, &de, dio.initial, &rises, &falls, &bits, rate);
    // START and STOP seen: the frames are theirs; else the cuts at the clock's gaps
    let (reading, inside) = if !ss.is_empty() {
        let inside: usize = ss.iter().map(|f| f.b - f.a).sum();
        (read_frames(&ss, true, &bits, &rises, rate, dio), inside)
    } else {
        // no START or STOP: the cuts at the clock's gaps, on a line keeping to the clock as data only (as the survey
        // pairs a clock's data lines; a free clock beside lines that rarely change reads frames whose parities hold
        // over bits all alike)
        if crate::sync::keeping(dio.edges, clk) < crate::clocked::KEEPS_MIN {
            return None;
        }
        let g = gap_frames(&rises);
        let r = read_frames(&g, false, &bits, &rises, rate, dio);
        if r.passed == 0 {
            return None;
        }
        (r, rises.len())
    };
    if reading.matched < 2 {
        return None;
    }
    let par = if reading.checked > 0 {
        reading.passed as f64 / reading.checked as f64
    } else {
        0.0
    };
    let cover = (reading.explained as f64 / inside.max(1) as f64).min(1.0);
    let score = par * par * (0.5 + 0.5 * cover) * (1.0 - 0.5f64.powf(reading.checked_frames as f64 / 2.0));
    let ev = obj(vec![
        (
            "frames",
            json!(if ss.is_empty() {
                gap_frames(&rises).len()
            } else {
                ss.len()
            }),
        ),
        ("matched", json!(reading.matched)),
        ("checked_frames", json!(reading.checked_frames)),
        ("parity_ok", json!(par)),
        ("bit_coverage", json!(cover)),
        ("delimited", json!(!ss.is_empty())),
    ]);
    let mut c = candidate(
        "rvswd",
        &[("clk", clk.name), ("dio", dio.name)],
        score,
        // (the definition Python's engine reads RVSWD by, as its readings name it)
        obj(vec![("sample_edge", json!("rise")), ("declared", json!("rvswd.toml"))]),
        ev,
        "dmi",
        &reading.recs,
        0,
    );
    // the sampling edges and DIO at each (Python's `bits` layer: to time the clock inside a frame)
    let lay = json!({
        "dmi": reading.recs,
        "bits": {"clock": clk.name, "data": [dio.name], "sample_edge": "rise", "t": rises, "values": [bits],
                 "deglitch": 0},
    });
    c.layers = crate::claim::raw(serde_json::to_string(&lay).unwrap());
    Some(c)
}

/// The RVSWD readings of a part: each line clocking another that changes no more often.
pub fn candidates(view: &View) -> Vec<Candidate> {
    let mut out = vec![];
    for c in view.channels.iter().filter(|c| c.edges.len() >= 64) {
        for d in &view.channels {
            if d.name != c.name && d.edges.len() >= 2 && d.edges.len() <= c.edges.len() {
                out.extend(read(view, c, d));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A short frame's 53 bits.
    fn short(addr: u32, write: bool, data: u32) -> Vec<u8> {
        let mut f: Vec<u8> = (0..7).rev().map(|q| ((addr >> q) & 1) as u8).collect();
        f.push(write as u8);
        f.push(parity(&f));
        f.extend([0, 0, 0, 0, 0]); // park, ctl
        let d: Vec<u8> = (0..32).rev().map(|q| ((data >> q) & 1) as u8).collect();
        f.extend(&d);
        f.push(parity(&d));
        f.extend([0, 0, 0, 0, 0, 1]); // park, status, pad, stop
        f
    }

    /// Frames on SWCLK / DIO: START (DIO falls while CLK is high), each bit set while CLK is low and read at its
    /// rise, STOP (DIO rises while CLK is held high).
    fn encode(frames: &[Vec<u8>]) -> (Vec<i64>, Vec<i64>) {
        let (mut c, mut d) = (vec![], vec![]);
        let (mut t, mut dl) = (1000i64, 1u8);
        let set = |d: &mut Vec<i64>, dl: &mut u8, v: u8, at: i64| {
            if *dl != v {
                d.push(at);
                *dl = v;
            }
        };
        for f in frames {
            set(&mut d, &mut dl, 0, t); // START, CLK high
            t += 20;
            for &b in &f[..f.len() - 1] {
                c.push(t); // CLK falls
                set(&mut d, &mut dl, b, t + 5);
                c.push(t + 10); // CLK rises: the bit read
                t += 20;
            }
            // the last clock is STOP's: DIO low, then rising while CLK is held high
            c.push(t);
            set(&mut d, &mut dl, 0, t + 5);
            c.push(t + 10);
            set(&mut d, &mut dl, 1, t + 30);
            t += 400;
        }
        (c, d)
    }

    #[test]
    fn short_frames_between_start_and_stop() {
        let fr = vec![
            short(0x10, true, 0x8000_0001),
            short(0x11, false, 0x0038_2c82),
            short(0x04, false, 7),
        ];
        let (c, d) = encode(&fr);
        let v = View::new(
            10e6,
            20_000,
            vec![Channel::new("CLK", 1, &c), Channel::new("DIO", 1, &d)],
        )
        .unwrap();
        let cs = candidates(&v);
        let r = cs.iter().find(|x| x.roles["clk"] == "CLK").unwrap();
        assert!(r.score > 0.6, "{}", r.score);
        let lay: Value = serde_json::from_str(r.layers.get()).unwrap();
        let dmi = lay["dmi"].as_array().unwrap();
        assert_eq!(dmi.len(), 3);
        assert_eq!(
            (dmi[0]["op"].clone(), dmi[0]["reg"].clone()),
            (json!("W"), json!("dmcontrol"))
        );
        assert_eq!(dmi[1]["data"], 0x0038_2c82);
        assert!(dmi.iter().all(|x| x["ok"] == json!(true)));
    }
}
