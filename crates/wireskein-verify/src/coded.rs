//! The checks of lines told by pulse widths, of audio, encoders, multi-line SPI and SD (`verify.check_rgb_led`,
//! `check_onewire`, `check_ir`, `check_i2s`, `check_spdif`, `check_quadrature`, `check_qspi`, `check_sd_spi`,
//! `check_i2c_timing`), with what they read the lines with (`_engine/pulsecode.py`, `periph.py`, `spdif.py`).

use crate::bus::{hex, seq_diff};
use crate::cap::{Cap, Ch};
use crate::checks::{Out, V, argmax, argmin, fp, limit_of, out, si, times, verdicts};
use crate::j::J;
use crate::jd;
use crate::pyfmt::{g, round, round_n};
use crate::timing::{self, judge, median, median_i};

pub fn run(kind: &str, cap: &Cap, x: &J) -> Out {
    match kind {
        "rgb_led" => check_rgb_led(cap, x),
        "onewire" => check_onewire(cap, x),
        "ir" => check_ir(cap, x),
        "i2s" => check_i2s(cap, x),
        "spdif" => check_spdif(cap, x),
        "quadrature" => check_quadrature(cap, x),
        "qspi" => check_qspi(cap, x),
        "sd_spi" => check_sd_spi(cap, x),
        "i2c_timing" => check_i2c_timing(cap, x),
        k => crate::decoded::run(k, cap, x),
    }
}

fn s<'a>(x: &'a J, k: &str) -> &'a str {
    x.get(k).s().unwrap_or("")
}

fn getf(x: &J, k: &str, d: f64) -> f64 {
    if x.has(k) && !x.get(k).is_null() {
        x.get(k).f().unwrap_or(d)
    } else {
        d
    }
}

/// `x.get(k, d)`: the default only when the key is missing (a None given stays None).
fn getf_opt(x: &J, k: &str, d: Option<f64>) -> Option<f64> {
    if x.has(k) { x.get(k).f() } else { d }
}

/// (start, width) of every whole pulse at `active` (`pulsecode.pulses`).
pub fn pulses(ch: &Ch, active: u8) -> Vec<(i64, i64)> {
    let e = &ch.edges;
    let lead: Vec<i64> = (0..e.len()).filter(|&i| ch.after(i) == active).map(|i| e[i]).collect();
    let trail: Vec<i64> = (0..e.len()).filter(|&i| ch.after(i) != active).map(|i| e[i]).collect();
    lead.iter()
        .filter_map(|&l| {
            let j = trail.partition_point(|&t| t <= l);
            trail.get(j).map(|&t| (l, t - l))
        })
        .collect()
}

/// The bytes of the bits (whole bytes), and the bits left over.
fn bytes_of(bits: &[u8], lsb_first: bool) -> (Vec<u8>, usize) {
    let mut out = vec![];
    let mut i = 0;
    while i + 8 <= bits.len() {
        out.push(
            bits[i..i + 8]
                .iter()
                .enumerate()
                .map(|(k, &v)| v << if lsb_first { k } else { 7 - k })
                .sum(),
        );
        i += 8;
    }
    (out, bits.len() % 8)
}

fn hexes(v: &J) -> Option<Vec<J>> {
    match v {
        J::Null => None,
        J::Str(s) => Some(vec![J::from(s.to_lowercase())]),
        J::List(l) => Some(l.iter().map(|h| J::from(h.s().unwrap_or("").to_lowercase())).collect()),
        _ => None,
    }
}

fn reprs<T: Into<J> + Clone>(v: &[T]) -> String {
    J::from(v.to_vec()).repr()
}

// ---------------- RGB LEDs ----------------

struct Rgb {
    frames: Vec<(i64, i64, String, usize)>,
    t0h: Vec<(f64, f64)>,
    t1h: Vec<(f64, f64)>,
    period: Vec<(f64, f64)>,
    resets: Vec<(f64, f64)>,
    pauses: Vec<(f64, f64)>,
    tail_low: Option<(f64, f64)>,
}

fn split_of(u: &[i64], ratio: f64) -> Option<f64> {
    if u.len() < 2 {
        return None;
    }
    let r: Vec<f64> = u.windows(2).map(|w| w[1] as f64 / w[0].max(1) as f64).collect();
    let k = argmax(r.iter().copied());
    if r[k] >= ratio {
        Some(((u[k] * u[k + 1]) as f64).sqrt())
    } else {
        None
    }
}

fn rgb_led(ch: &Ch, n: i64) -> Rgb {
    let p = pulses(ch, 1);
    let mut o = Rgb {
        frames: vec![],
        t0h: vec![],
        t1h: vec![],
        period: vec![],
        resets: vec![],
        pauses: vec![],
        tail_low: None,
    };
    if p.len() < 2 {
        return o;
    }
    let st: Vec<i64> = p.iter().map(|v| v.0).collect();
    let w: Vec<i64> = p.iter().map(|v| v.1).collect();
    let per: Vec<i64> = st.windows(2).map(|v| v[1] - v[0]).collect();
    let bit = median_i(&per);
    let mut uw: Vec<(i64, usize)> = vec![];
    let mut ws = w.clone();
    ws.sort_unstable();
    for v in ws {
        match uw.last_mut() {
            Some(l) if l.0 == v => l.1 += 1,
            _ => uw.push((v, 1)),
        }
    }
    let lim = 2f64.max(0.02 * w.len() as f64);
    let common: Vec<i64> = uw.iter().filter(|c| c.1 as f64 >= lim).map(|c| c.0).collect();
    let mut split = split_of(&common, 1.3);
    if split.is_none_or(|sp| sp > bit) {
        split = Some(if !w.is_empty() && median_i(&w) > bit / 2.0 {
            0.0
        } else {
            bit
        });
    }
    let split = split.unwrap();
    let one: Vec<bool> = w.iter().map(|&v| v as f64 > split).collect();
    let mut bounds = vec![0usize];
    bounds.extend((0..per.len()).filter(|&k| per[k] as f64 > 5.0 * bit).map(|k| k + 1));
    bounds.push(st.len());
    for k in 0..st.len() {
        let pair = (st[k] as f64, w[k] as f64);
        if one[k] { o.t1h.push(pair) } else { o.t0h.push(pair) }
    }
    let mut inner = vec![true; st.len() - 1];
    for &b in &bounds[1..bounds.len() - 1] {
        inner[b - 1] = false;
    }
    for k in 0..per.len() {
        if inner[k] {
            o.period.push((st[k] as f64, per[k] as f64));
            if per[k] as f64 > 2.0 * bit {
                o.pauses.push((st[k] as f64, per[k] as f64));
            }
        }
    }
    for wb in bounds.windows(2) {
        let (a, b) = (wb[0], wb[1]);
        let bits: Vec<u8> = one[a..b].iter().map(|&v| v as u8).collect();
        let (data, left) = bytes_of(&bits, false);
        o.frames.push((st[a], (b - a) as i64, hex(&data), left));
        let end = st[b - 1] + w[b - 1];
        if b < st.len() {
            o.resets.push((end as f64, (st[b] - end) as f64));
        } else if ch.level_at(n - 1) == 0 {
            o.tail_low = Some((end as f64, (n - end) as f64));
        }
    }
    o
}

fn check_rgb_led(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let rate = cap.rate;
    let r = rgb_led(ch, cap.n);
    let res = ch.step;
    let frames: Vec<J> = r.frames.iter().map(|f| J::from(f.2.clone())).collect();
    let mut got = jd! {"frames" => frames.clone(), "bits" => r.frames.iter().map(|f| f.1).collect::<Vec<_>>(),
    "t0h" => times(&r.t0h, rate), "t1h" => times(&r.t1h, rate), "period" => times(&r.period, rate),
    "resets" => times(&r.resets, rate), "pauses" => times(&r.pauses, rate),
    "resolution_s" => res as f64 / rate};
    if r.frames.is_empty() {
        return out(V::Ng, got, "no pulses");
    }
    let mut parts = vec![];
    if let Some(want) = hexes(x.get("data")) {
        parts.push(seq_diff("frames", &want, &frames));
    }
    let cut: Vec<usize> = (0..r.frames.len()).filter(|&i| r.frames[i].3 > 0).collect();
    if !cut.is_empty() {
        parts.push((
            Some(false),
            format!(
                "frames {} end {} bits after a byte",
                reprs(&cut),
                reprs(&cut.iter().map(|&i| r.frames[i].3).collect::<Vec<_>>())
            ),
        ));
    }
    for (k, v) in [("t0h", &r.t0h), ("t1h", &r.t1h), ("period", &r.period)] {
        let (lo, hi) = (x.get(&format!("{k}_min_s")).f(), x.get(&format!("{k}_max_s")).f());
        if !v.is_empty() && (lo.is_some() || hi.is_some()) {
            let name = if k == "period" {
                "bit period".to_string()
            } else {
                k.to_uppercase()
            };
            parts.extend(limit_of(&name, v, res, lo, hi, rate));
        }
    }
    let reset_min = x.get("reset_min_s").f();
    if let Some(rm) = reset_min {
        if !r.resets.is_empty() {
            parts.extend(limit_of("reset", &r.resets, res, Some(rm), None, rate));
        }
        let tail = r.tail_low;
        if tail.is_none_or(|t| judge(t.1, res as f64, Some(rm * rate), None) != Some(true)) {
            parts.push((
                None,
                format!(
                    "the window ends {} after the last frame, before a reset of {} latched it",
                    si(tail.map_or(0.0, |t| t.1) / rate),
                    si(rm)
                ),
            ));
        }
    }
    let safe = getf_opt(x, "latch_safe_s", Some(280e-6));
    let notices: Vec<J> = match safe {
        Some(sf) if sf != 0.0 => r
            .resets
            .iter()
            .filter(|&&(_, n)| n < sf * rate && reset_min.is_none_or(|rm| n >= rm * rate))
            .map(|&(t, n)| {
                jd! {"kind" => "latch_short", "tick" => t as i64, "length_ticks" => n as i64,
                "safe_ticks" => round(sf * rate)}
            })
            .collect(),
        _ => vec![],
    };
    got.set("notices", J::List(notices));
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

// ---------------- 1-Wire ----------------

struct OwSpeed {
    reset: (Option<f64>, Option<f64>),
    wait: (Option<f64>, Option<f64>),
    presence: (Option<f64>, Option<f64>),
    slot: (Option<f64>, Option<f64>),
    recovery: (Option<f64>, Option<f64>),
    low_0: (Option<f64>, Option<f64>),
    low_1: (Option<f64>, Option<f64>),
    split: f64,
    reset_detect: f64,
}

fn ow(speed: &str) -> OwSpeed {
    if speed == "overdrive" {
        OwSpeed {
            reset: (Some(70e-6), Some(80e-6)),
            wait: (Some(2e-6), Some(6e-6)),
            presence: (Some(8e-6), Some(24e-6)),
            slot: (Some(7e-6), None),
            recovery: (Some(1e-6), None),
            low_0: (None, Some(16e-6)),
            low_1: (Some(1e-6), Some(2e-6)),
            split: 2e-6,
            reset_detect: 48e-6,
        }
    } else {
        OwSpeed {
            reset: (Some(480e-6), None),
            wait: (Some(15e-6), Some(60e-6)),
            presence: (Some(60e-6), Some(240e-6)),
            slot: (Some(61e-6), None),
            recovery: (Some(1e-6), None),
            low_0: (None, Some(120e-6)),
            low_1: (Some(1e-6), Some(15e-6)),
            split: 15e-6,
            reset_detect: 288e-6,
        }
    }
}

const OVERDRIVE_ROM: [u8; 2] = [0x3C, 0x69];
const OVERDRIVE_LOOK: usize = 8;

#[derive(Default)]
struct OwReset {
    t: i64,
    reset: i64,
    presence: bool,
    wait: i64,
    presence_low: i64,
    data: String,
    leftover: usize,
}

#[derive(Default)]
struct Ow {
    resets: Vec<OwReset>,
    low_0: Vec<(f64, f64)>,
    low_1: Vec<(f64, f64)>,
    slot: Vec<(f64, f64)>,
    recovery: Vec<(f64, f64)>,
    before_reset: i64,
}

fn onewire(ch: &Ch, rate: f64, speed: &str) -> Ow {
    let p = pulses(ch, 0);
    let starts: Vec<i64> = p.iter().map(|v| v.0).collect();
    let widths: Vec<i64> = p.iter().map(|v| v.1).collect();
    let mut o = Ow::default();
    let mut cur: Option<usize> = None;
    let mut bits: Vec<u8> = vec![];
    let mut last: Option<(i64, i64)> = None;
    let mut mode = speed.to_string();
    let std_reset = ow("standard").reset_detect * rate;
    let close = |o: &mut Ow, cur: Option<usize>, bits: &[u8]| {
        if let Some(c) = cur {
            let (d, l) = bytes_of(bits, true);
            o.resets[c].data = hex(&d);
            o.resets[c].leftover = l;
        }
    };
    let paced_overdrive = |i: usize| -> bool {
        let od = ow("overdrive");
        let (mut gaps, mut lows) = (vec![], vec![]);
        for j in i + 1..starts.len().min(i + 1 + OVERDRIVE_LOOK) {
            if widths[j] as f64 >= std_reset {
                break;
            }
            gaps.push((starts[j] - starts[j - 1]) as f64);
            lows.push(widths[j] as f64 / rate);
        }
        if gaps.is_empty() {
            return false;
        }
        if median(&gaps) < ow("standard").slot.0.unwrap() * rate {
            return true;
        }
        lows.iter().all(|&l| l <= od.low_0.1.unwrap())
            && lows.iter().any(|&l| l < od.split)
            && lows.iter().any(|&l| l >= od.split)
    };
    for i in 0..starts.len() {
        let (s0, x) = (starts[i], widths[i]);
        let mut sp = ow(&mode);
        if x as f64 >= sp.reset_detect * rate {
            close(&mut o, cur, &bits);
            if x as f64 >= std_reset {
                mode = speed.to_string();
            }
            o.resets.push(OwReset {
                t: s0,
                reset: x,
                ..Default::default()
            });
            cur = Some(o.resets.len() - 1);
            bits.clear();
            last = None;
            continue;
        }
        let Some(c) = cur else {
            o.before_reset += 1;
            continue;
        };
        sp = ow(&mode);
        let rel = o.resets[c].t + o.resets[c].reset;
        if !o.resets[c].presence
            && bits.is_empty()
            && ((s0 - rel) as f64) <= 2.0 * sp.wait.1.unwrap() * rate
            && x as f64 >= sp.split * rate
        {
            let r = &mut o.resets[c];
            r.presence = true;
            r.wait = s0 - rel;
            r.presence_low = x;
            continue;
        }
        let one = (x as f64) < sp.split * rate;
        bits.push(one as u8);
        if mode == speed {
            if one {
                o.low_1.push((s0 as f64, x as f64))
            } else {
                o.low_0.push((s0 as f64, x as f64))
            }
            if let Some((l0, l1)) = last
                && ((s0 - l0) as f64) < 4.0 * sp.slot.0.unwrap() * rate
            {
                o.slot.push((l0 as f64, (s0 - l0) as f64));
                o.recovery.push(((l0 + l1) as f64, (s0 - l0 - l1) as f64));
            }
        }
        last = Some((s0, x));
        if mode == "standard"
            && bits.len() == 8
            && OVERDRIVE_ROM.contains(&bytes_of(&bits, true).0[0])
            && paced_overdrive(i)
        {
            mode = "overdrive".into();
            last = None;
        }
    }
    close(&mut o, cur, &bits);
    o
}

fn check_onewire(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let rate = cap.rate;
    let speed = if x.has("speed") { s(x, "speed") } else { "standard" };
    let sp = ow(speed);
    let r = onewire(ch, rate, speed);
    let res = ch.step;
    let rs = &r.resets;
    let pres: Vec<&OwReset> = rs.iter().filter(|z| z.presence).collect();
    let resets: Vec<(f64, f64)> = rs.iter().map(|z| (z.t as f64, z.reset as f64)).collect();
    let waits: Vec<(f64, f64)> = pres.iter().map(|z| (z.t as f64, z.wait as f64)).collect();
    let plows: Vec<(f64, f64)> = pres.iter().map(|z| (z.t as f64, z.presence_low as f64)).collect();
    let data: Vec<J> = rs.iter().map(|z| J::from(z.data.clone())).collect();
    let got = jd! {"data" => data.clone(), "presence" => rs.iter().map(|z| z.presence).collect::<Vec<_>>(),
    "low_0" => times(&r.low_0, rate), "low_1" => times(&r.low_1, rate), "slot" => times(&r.slot, rate),
    "recovery" => times(&r.recovery, rate), "reset" => times(&resets, rate), "wait" => times(&waits, rate),
    "presence_low" => times(&plows, rate), "slots_before_reset" => r.before_reset,
    "resolution_s" => res as f64 / rate};
    if rs.is_empty() {
        return out(V::Ng, got, "no reset pulse");
    }
    let mut parts = vec![];
    if let Some(want) = hexes(x.get("data")) {
        parts.push(seq_diff("data after each reset", &want, &data));
    }
    let cut: Vec<usize> = (0..rs.len()).filter(|&i| rs[i].leftover > 0).collect();
    if !cut.is_empty() {
        parts.push((
            Some(false),
            format!(
                "after resets {}, {} bits after a byte",
                reprs(&cut),
                reprs(&cut.iter().map(|&i| rs[i].leftover).collect::<Vec<_>>())
            ),
        ));
    }
    if let J::Bool(want) = x.get("presence") {
        let bad: Vec<usize> = (0..rs.len()).filter(|&i| rs[i].presence != *want).collect();
        if !bad.is_empty() {
            parts.push((
                Some(false),
                format!(
                    "resets {} {} a presence pulse",
                    reprs(&bad),
                    if *want { "without" } else { "with" }
                ),
            ));
        }
    }
    parts.extend(limit_of("reset", &resets, res, sp.reset.0, sp.reset.1, rate));
    if !pres.is_empty() {
        parts.extend(limit_of("presence wait", &waits, res, sp.wait.0, sp.wait.1, rate));
        parts.extend(limit_of("presence", &plows, res, sp.presence.0, sp.presence.1, rate));
    }
    for (v, name, lim) in [
        (&r.slot, "slot", sp.slot),
        (&r.recovery, "recovery", sp.recovery),
        (&r.low_0, "0 low", sp.low_0),
        (&r.low_1, "1 low", sp.low_1),
    ] {
        if !v.is_empty() {
            parts.extend(limit_of(name, v, res, lim.0, lim.1, rate));
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

// ---------------- IR (NEC) ----------------

const NEC_KEYS: [&str; 6] = ["leader", "space", "repeat_space", "mark", "space_0", "space_1"];

fn nec_len(k: &str) -> f64 {
    match k {
        "leader" => 9000e-6,
        "space" => 4500e-6,
        "repeat_space" => 2250e-6,
        "mark" => 562.5e-6,
        "space_0" => 562.5e-6,
        _ => 1687.5e-6,
    }
}

struct Nec {
    carrier_hz: Option<f64>,
    carrier_duty: Option<f64>,
    times: std::collections::BTreeMap<&'static str, Vec<(f64, f64)>>,
    /// (repeat, addr, cmd, cmd_ok)
    frames: Vec<(bool, Option<i64>, Option<i64>, bool)>,
}

fn nec(ch: &Ch, rate: f64) -> Nec {
    let idle = ch.initial;
    let p = pulses(ch, 1 - idle);
    let mut o = Nec {
        carrier_hz: None,
        carrier_duty: None,
        times: NEC_KEYS.iter().map(|&k| (k, vec![])).collect(),
        frames: vec![],
    };
    let mut mk: Vec<(i64, i64)> = vec![];
    if !p.is_empty() {
        let st: Vec<i64> = p.iter().map(|v| v.0).collect();
        let w: Vec<i64> = p.iter().map(|v| v.1).collect();
        if median_i(&w) < 100e-6 * rate && st.len() > 4 {
            let inside: Vec<bool> = (0..st.len() - 1)
                .map(|k| ((st[k + 1] - (st[k] + w[k])) as f64) <= 150e-6 * rate)
                .collect();
            let mut b = vec![0usize];
            b.extend((0..inside.len()).filter(|&k| !inside[k]).map(|k| k + 1));
            b.push(st.len());
            if inside.iter().any(|&v| v) {
                let d: Vec<i64> = (0..inside.len())
                    .filter(|&k| inside[k])
                    .map(|k| st[k + 1] - st[k])
                    .collect();
                let per = median_i(&d);
                let ww: Vec<i64> = (0..inside.len()).filter(|&k| inside[k]).map(|k| w[k]).collect();
                o.carrier_hz = Some(rate / per);
                o.carrier_duty = Some(median_i(&ww) / per);
            }
            for wb in b.windows(2) {
                mk.push((st[wb[0]], st[wb[1] - 1] + w[wb[1] - 1]));
            }
        } else {
            mk = p.iter().map(|&(a, ww)| (a, a + ww)).collect();
        }
    }
    let tol = 0.25;
    let near = |v: i64, k: &str| (v as f64 / rate / nec_len(k) - 1.0).abs() <= tol;
    let push = |o: &mut Nec, k: &'static str, a: i64, b: i64| o.times.get_mut(k).unwrap().push((a as f64, b as f64));
    let mut i = 0;
    while i < mk.len() {
        let (s0, e) = mk[i];
        if !near(e - s0, "leader") || i + 1 >= mk.len() {
            i += 1;
            continue;
        }
        let sp = mk[i + 1].0 - e;
        if near(sp, "repeat_space") {
            push(&mut o, "leader", s0, e - s0);
            push(&mut o, "repeat_space", e, sp);
            push(&mut o, "mark", mk[i + 1].0, mk[i + 1].1 - mk[i + 1].0);
            o.frames.push((true, None, None, false));
            i += 2;
            continue;
        }
        if !near(sp, "space") {
            i += 1;
            continue;
        }
        push(&mut o, "leader", s0, e - s0);
        push(&mut o, "space", e, sp);
        let (mut bits, mut j) = (vec![], i + 1);
        while j < mk.len() && bits.len() < 32 {
            let (ms, me) = mk[j];
            push(&mut o, "mark", ms, me - ms);
            if j + 1 >= mk.len() {
                break;
            }
            let gap = mk[j + 1].0 - me;
            if gap as f64 / rate > 2.0 * nec_len("space_1") {
                break;
            }
            let one = gap as f64 / rate > (nec_len("space_0") + nec_len("space_1")) / 2.0;
            push(&mut o, if one { "space_1" } else { "space_0" }, me, gap);
            bits.push(one as u8);
            j += 1;
        }
        if bits.len() == 32 && j < mk.len() {
            push(&mut o, "mark", mk[j].0, mk[j].1 - mk[j].0);
        }
        let (data, _) = bytes_of(&bits, true);
        if data.len() == 4 {
            let (a, na, c, nc) = (data[0] as i64, data[1] as i64, data[2] as i64, data[3] as i64);
            let ext = a ^ na != 0xFF;
            o.frames
                .push((false, Some(if ext { a | na << 8 } else { a }), Some(c), c ^ nc == 0xFF));
        } else {
            o.frames.push((false, None, None, false));
        }
        i = j + 1;
    }
    o
}

fn check_ir(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let proto = if x.has("protocol") {
        x.get("protocol").clone()
    } else {
        J::from("nec")
    };
    if proto.s() != Some("nec") {
        return out(
            V::Unchecked,
            J::dict(),
            format!("IR protocol {} not known (nec)", proto.repr()),
        );
    }
    let rate = cap.rate;
    let r = nec(ch, rate);
    let res = ch.step;
    let frame_j = |f: &(bool, Option<i64>, Option<i64>, bool)| {
        if f.0 {
            jd! {"repeat" => true}
        } else {
            jd! {"addr" => f.1, "cmd" => f.2}
        }
    };
    let frames: Vec<J> = r.frames.iter().map(frame_j).collect();
    let mut got = jd! {"frames" => frames.clone(), "carrier_hz" => r.carrier_hz, "carrier_duty" => r.carrier_duty};
    for k in NEC_KEYS {
        got.set(k, times(&r.times[k], rate));
    }
    got.set("resolution_s", res as f64 / rate);
    if r.frames.is_empty() {
        return out(V::Ng, got, "no NEC frame");
    }
    let mut parts = vec![];
    if !x.get("frames").is_null() {
        let want: Vec<J> = x
            .get("frames")
            .list()
            .iter()
            .map(|f| {
                if f.get("repeat").truthy() {
                    jd! {"repeat" => true}
                } else {
                    jd! {"addr" => f.get("addr"), "cmd" => f.get("cmd")}
                }
            })
            .collect();
        parts.push(seq_diff("frames", &want, &frames));
    }
    let bad: Vec<usize> = (0..r.frames.len())
        .filter(|&i| !r.frames[i].0 && !r.frames[i].3)
        .collect();
    if !bad.is_empty() {
        parts.push((
            Some(false),
            format!(
                "frames {}: the command's inverse does not match (or fewer than 32 bits)",
                reprs(&bad)
            ),
        ));
    }
    if !x.get("carrier_hz").is_null() {
        let want = x.get("carrier_hz").f().unwrap();
        let c = r.carrier_hz;
        parts.push((
            Some(c.is_some_and(|c| (c / want - 1.0).abs() <= x.get("carrier_rel").f().unwrap_or(0.05))),
            match c {
                Some(c) if c != 0.0 => format!("carrier {} Hz vs {}", round(c), x.get("carrier_hz").repr()),
                _ => "no carrier (a demodulated line?)".into(),
            },
        ));
    }
    if let Some(tol) = x.get("timing_rel").f() {
        for k in NEC_KEYS {
            if !r.times[k].is_empty() {
                let nom = nec_len(k);
                parts.extend(limit_of(
                    k,
                    &r.times[k],
                    res,
                    Some(nom * (1.0 - tol)),
                    Some(nom * (1.0 + tol)),
                    rate,
                ));
            }
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

// ---------------- I2S ----------------

fn check_i2s(cap: &Cap, x: &J) -> Out {
    let (Some(bclk), Some(ws), Some(sd)) = (cap.ch(s(x, "bclk")), cap.ch(s(x, "ws")), cap.ch(s(x, "sd"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let rate = cap.rate;
    let fmt = if x.has("format") {
        s(x, "format").to_string()
    } else {
        "i2s".into()
    };
    let r: Vec<i64> = (0..bclk.edges.len())
        .filter(|&i| bclk.after(i) == 1)
        .map(|i| bclk.edges[i])
        .collect();
    let mut got = jd! {"slot_bits" => J::Null, "fs_hz" => J::Null, "bclk_hz" => J::Null,
    "ws_changes_bclk_low" => J::Null, "sd_changes_bclk_low" => J::Null};
    let mut frames: Vec<(i64, i64, i64)> = vec![];
    if r.len() >= 8 {
        let w: Vec<u8> = r.iter().map(|&t| ws.level_at(t)).collect();
        let d: Vec<u8> = r.iter().map(|&t| sd.level_at(t)).collect();
        let ch_at: Vec<usize> = (1..w.len()).filter(|&k| w[k] != w[k - 1]).collect();
        got.set(
            "bclk_hz",
            rate / median_i(&r.windows(2).map(|v| v[1] - v[0]).collect::<Vec<_>>()),
        );
        if ch_at.len() >= 3 {
            let slot = median(&ch_at.windows(2).map(|v| (v[1] - v[0]) as f64).collect::<Vec<_>>()) as i64;
            got.set("slot_bits", slot);
            let we = &ws.edges;
            if we.len() >= 3 {
                let rises: Vec<i64> = (0..we.len()).filter(|&i| ws.after(i) == 1).map(|i| we[i]).collect();
                if rises.len() >= 2 {
                    got.set(
                        "fs_hz",
                        rate / median_i(&rises.windows(2).map(|v| v[1] - v[0]).collect::<Vec<_>>()),
                    );
                }
                got.set(
                    "ws_changes_bclk_low",
                    we.iter().filter(|&&t| bclk.level_at(t) == 0).count() as f64 / we.len() as f64,
                );
            }
            if !sd.edges.is_empty() {
                got.set(
                    "sd_changes_bclk_low",
                    sd.edges.iter().filter(|&&t| bclk.level_at(t) == 0).count() as f64 / sd.edges.len() as f64,
                );
            }
            let nb = x.get("bits").i().filter(|&b| b != 0).unwrap_or(slot) as usize;
            let left = match x.get("left_ws").i() {
                Some(l) => l as u8,
                None => (fmt != "i2s") as u8,
            };
            let mut words: Vec<(u8, i64, i64)> = vec![];
            for p in ch_at.windows(2) {
                let (a, b) = (p[0], p[1]);
                let k0 = match fmt.as_str() {
                    "i2s" => a as i64 + 1,
                    "left" => a as i64,
                    _ => b as i64 - nb as i64,
                };
                if k0 < a as i64
                    || k0 + nb as i64 > d.len() as i64
                    || (fmt != "right" && k0 + nb as i64 > b as i64 + (fmt == "i2s") as i64)
                {
                    continue;
                }
                let mut v: i64 = 0;
                for &bit in &d[k0 as usize..k0 as usize + nb] {
                    v = (v << 1) | bit as i64;
                }
                if nb > 0 && v >= 1 << (nb - 1) {
                    v -= 1 << nb;
                }
                words.push((w[a], r[a], v));
            }
            let mut i = 0;
            while i + 1 < words.len() {
                if words[i].0 == left && words[i + 1].0 != left {
                    frames.push((words[i].1, words[i].2, words[i + 1].2));
                    i += 2;
                } else {
                    i += 1;
                }
            }
        }
    }
    got.set("frames", frames.len());
    got.set(
        "first",
        J::List(frames.iter().take(8).map(|f| J::from(vec![f.1, f.2])).collect()),
    );
    if frames.is_empty() {
        return out(V::Ng, got, "no whole left / right pair");
    }
    let mut parts = vec![];
    if let Some(sb) = x.get("slot_bits").i()
        && got.get("slot_bits").i() != Some(sb)
    {
        parts.push((
            Some(false),
            format!("{} BCLKs a slot, want {sb}", got.get("slot_bits").repr()),
        ));
    }
    if !x.get("fs_hz").is_null() {
        let want = x.get("fs_hz").f().unwrap();
        let fs = got.get("fs_hz").f();
        parts.push((
            Some(fs.is_some_and(|f| (f / want - 1.0).abs() <= x.get("fs_rel").f().unwrap_or(0.01))),
            format!(
                "sample rate {} Hz vs {}",
                match fs {
                    Some(f) if f != 0.0 => J::from(round_n(f, 1)).repr(),
                    Some(f) => J::from(f).repr(),
                    None => "None".into(),
                },
                x.get("fs_hz").repr()
            ),
        ));
    }
    if !x.get("samples").is_null() {
        let want: Vec<(i64, i64)> = x
            .get("samples")
            .list()
            .iter()
            .map(|v| (v.list()[0].i().unwrap_or(0), v.list()[1].i().unwrap_or(0)))
            .collect();
        let seq: Vec<(i64, i64)> = frames.iter().map(|f| (f.1, f.2)).collect();
        let hit = if want.len() <= seq.len() {
            (0..=seq.len() - want.len()).find(|&i| seq[i..i + want.len()] == want[..])
        } else {
            None
        };
        match hit {
            None => {
                let first = J::List(seq.iter().take(4).map(|v| J::from(vec![v.0, v.1])).collect());
                parts.push((
                    Some(false),
                    format!(
                        "the {} samples are not in the {} read (first read {})",
                        want.len(),
                        seq.len(),
                        first.repr()
                    ),
                ));
            }
            Some(h) => got.set("samples_at", h),
        }
    }
    let (v, why) = verdicts(&parts);
    let asked = ["slot_bits", "fs_hz", "samples"].iter().any(|k| !x.get(k).is_null());
    out(if asked || v != V::Ok { v } else { V::Measured }, got, why)
}

// ---------------- S/PDIF ----------------

struct Sub {
    s: i64,
    e: i64,
    pre: char,
    audio: i64,
    c: u8,
    ok: bool,
}

const SPDIF_SILENCE: i64 = 64;
const SPDIF_SHARE: f64 = 0.01;

fn uniq_counts(v: &[i64]) -> Vec<(i64, usize)> {
    let mut s = v.to_vec();
    s.sort_unstable();
    let mut out: Vec<(i64, usize)> = vec![];
    for x in s {
        match out.last_mut() {
            Some(l) if l.0 == x => l.1 += 1,
            _ => out.push((x, 1)),
        }
    }
    out
}

fn subframes(edges: &[i64]) -> Vec<Sub> {
    if edges.len() < 200 {
        return vec![];
    }
    let d: Vec<i64> = edges.windows(2).map(|w| w[1] - w[0]).collect();
    let common: Vec<i64> = uniq_counts(&d)
        .into_iter()
        .filter(|c| c.1 as f64 >= SPDIF_SHARE * d.len() as f64)
        .map(|c| c.0)
        .collect();
    if common.is_empty() {
        return vec![];
    }
    let lim = SPDIF_SILENCE * common[0].max(1);
    let mut out = vec![];
    let mut a = 0;
    for k in 0..d.len() {
        if d[k] > lim {
            out.extend(subframes_part(&edges[a..k + 1]));
            a = k + 1;
        }
    }
    out.extend(subframes_part(&edges[a..]));
    out
}

fn subframes_part(edges: &[i64]) -> Vec<Sub> {
    if edges.len() < 200 {
        return vec![];
    }
    let d: Vec<f64> = edges.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
    let rounded: Vec<i64> = d.iter().map(|v| v.round_ties_even() as i64).collect();
    let common: Vec<i64> = uniq_counts(&rounded)
        .into_iter()
        .filter(|c| c.1 as f64 >= SPDIF_SHARE * d.len() as f64)
        .map(|c| c.0)
        .collect();
    if common.is_empty() || common[0] < 1 {
        return vec![];
    }
    let c0 = common[0] as f64;
    let mut xs: Vec<f64> = d.iter().copied().filter(|&v| v < 4.0 * c0).collect();
    if xs.len() > 4000 {
        let st = xs.len() / 4000;
        xs = xs.iter().copied().step_by(st).collect();
    }
    let (start, stop) = (0.8 * c0, 1.6 * c0);
    let step = (stop - start) / 160.0;
    let mut best = (0usize, f64::INFINITY);
    for i in 0..161 {
        let c = if i == 160 { stop } else { start + i as f64 * step };
        let err = xs
            .iter()
            .map(|&v| {
                let r = v / c;
                (r - r.round_ties_even()).abs()
            })
            .sum::<f64>()
            / xs.len() as f64;
        if i == 0 || err < best.1 {
            best = (i, err);
        }
    }
    let half = if best.0 == 160 {
        stop
    } else {
        start + best.0 as f64 * step
    };
    let u: Vec<i64> = d.iter().map(|v| (v / half).round_ties_even() as i64).collect();
    let n = u.len();
    let starts: Vec<usize> = (0..n.saturating_sub(3)).filter(|&k| u[k] == 3).collect();
    if starts.len() < 8 {
        return vec![];
    }
    let mut out = vec![];
    let mut i = starts[0];
    while i + 4 <= n {
        let pre = match (u[i], u[i + 1], u[i + 2], u[i + 3]) {
            (3, 1, 1, 3) => Some('B'),
            (3, 3, 1, 1) => Some('M'),
            (3, 2, 1, 2) => Some('W'),
            _ => None,
        };
        let Some(pre) = pre else {
            let k = starts.partition_point(|&s| s < i + 1);
            if k >= starts.len() {
                break;
            }
            i = starts[k];
            continue;
        };
        let s0 = edges[i];
        let (mut j, mut bits) = (i + 4, vec![]);
        while j < n && bits.len() < 28 {
            if u[j] == 2 {
                bits.push(0u8);
                j += 1;
            } else if u[j] == 1 && j + 1 < n && u[j + 1] == 1 {
                bits.push(1);
                j += 2;
            } else {
                break;
            }
        }
        if bits.len() < 28 {
            i = if j > i + 4 { j } else { i + 1 };
            continue;
        }
        out.push(Sub {
            s: s0,
            e: edges[j],
            pre,
            audio: (0..24).map(|k| (bits[k] as i64) << k).sum(),
            c: bits[26],
            ok: bits.iter().map(|&b| b as u32).sum::<u32>() % 2 == 0,
        });
        i = j;
    }
    out
}

fn check_spdif(cap: &Cap, x: &J) -> Out {
    let Some(ch) = cap.ch(s(x, "pin")) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    let rate = cap.rate;
    let sf = subframes(&ch.edges);
    let mut got = jd! {"subframes" => sf.len()};
    if sf.len() < 2 {
        return out(V::Ng, got, "no S/PDIF subframes read");
    }
    let span = median(&sf.iter().map(|f| (f.e - f.s) as f64).collect::<Vec<_>>());
    let fs = rate / (2.0 * span);
    got.set("fs_hz", fs);
    let chan: Vec<char> = sf.iter().map(|f| if f.pre == 'W' { 'B' } else { 'A' }).collect();
    let mut errors: Vec<(String, i64)> = vec![];
    for i in 1..sf.len() {
        let gap = (sf[i].s - sf[i - 1].s) as f64 / span;
        if (gap - 1.0).abs() > 0.25 {
            errors.push(("missing".into(), sf[i].s));
        } else if chan[i] == chan[i - 1] {
            errors.push(("out of turn".into(), sf[i].s));
        }
    }
    let starts: Vec<usize> = (0..sf.len()).filter(|&i| sf[i].pre == 'B').collect();
    for p in starts.windows(2) {
        let frames = chan[p[0]..p[1]].iter().filter(|&&c| c == 'A').count();
        if frames != 192 {
            errors.push((format!("a block of {frames} frames"), sf[p[0]].s));
        }
    }
    errors.extend(sf.iter().filter(|f| !f.ok).map(|f| ("parity".to_string(), f.s)));
    got.set("blocks", starts.len());
    got.set("errors", errors.len());
    got.set("parity_errors", errors.iter().filter(|e| e.0 == "parity").count());
    got.set(
        "first_errors",
        J::List(
            errors
                .iter()
                .take(5)
                .map(|(k, t)| jd! {"kind" => k.clone(), "t_s" => *t as f64 / rate})
                .collect(),
        ),
    );
    let mut parts = vec![];
    let emax = if x.has("errors_max") {
        x.get("errors_max").i().unwrap_or(0)
    } else {
        0
    };
    if errors.len() as i64 > emax {
        let (k, t) = &errors[0];
        parts.push((
            Some(false),
            format!(
                "{} subframes out of order or failing parity (at most {emax}): first {k} at {}",
                errors.len(),
                si(*t as f64 / rate)
            ),
        ));
    }
    let vals = |c: char| -> Vec<i64> {
        sf.iter()
            .zip(&chan)
            .filter(|(f, cc)| **cc == c && f.ok)
            .map(|(f, _)| f.audio)
            .collect()
    };
    let (va, vb) = (vals('A'), vals('B'));
    got.set(
        "audio",
        jd! {"A" => va.iter().take(4).copied().collect::<Vec<_>>(), "B" => vb.iter().take(4).copied().collect::<Vec<_>>()},
    );
    for (c, want) in x.get("audio").items() {
        let have = match c.as_str() {
            "A" => &va,
            "B" => &vb,
            _ => &vec![],
        };
        if let J::List(w) = want {
            let w: Vec<i64> = w.iter().map(|v| v.i().unwrap_or(0)).collect();
            let hit = w.len() <= have.len() && (0..=have.len() - w.len()).any(|i| have[i..i + w.len()] == w[..]);
            if !hit {
                parts.push((
                    Some(false),
                    format!(
                        "channel {c}: {}... not in a row among the {} read",
                        reprs(&w[..w.len().min(4)]),
                        have.len()
                    ),
                ));
            }
        } else {
            let w = want.i().unwrap_or(0);
            let bad: Vec<i64> = have.iter().copied().filter(|&v| v != w).collect();
            if have.is_empty() || !bad.is_empty() {
                parts.push((
                    Some(false),
                    format!(
                        "channel {c}: {} of {} samples not {w:#x}{}",
                        bad.len(),
                        have.len(),
                        bad.first().map(|b| format!(" (first {b:#x})")).unwrap_or_default()
                    ),
                ));
            }
        }
    }
    let mut status: [Vec<u64>; 2] = [vec![], vec![]];
    for &a in &starts {
        let mut bits: [Vec<u8>; 2] = [vec![], vec![]];
        for (f, c) in sf[a..].iter().zip(&chan[a..]) {
            let k = (*c == 'B') as usize;
            if bits[k].len() < 32 {
                bits[k].push(f.c);
            }
            if bits[0].len() >= 32 && bits[1].len() >= 32 {
                break;
            }
        }
        for k in 0..2 {
            if bits[k].len() == 32 {
                status[k].push(bits[k].iter().enumerate().map(|(i, &b)| (b as u64) << i).sum());
            }
        }
    }
    let fmt_st = |v: &Vec<u64>| -> Vec<String> {
        let mut u = v.clone();
        u.sort_unstable();
        u.dedup();
        u.iter().map(|v| format!("{v:#010x}")).collect()
    };
    got.set("status", jd! {"A" => fmt_st(&status[0]), "B" => fmt_st(&status[1])});
    let sb = if x.get("status_b").is_null() {
        x.get("status")
    } else {
        x.get("status_b")
    };
    for (k, c, want) in [(0, 'A', x.get("status")), (1, 'B', sb)] {
        let Some(want) = want.i() else { continue };
        let want = want as u64;
        let have = &status[k];
        if have.is_empty() {
            parts.push((
                Some(false),
                format!("channel {c}: no whole block's channel status read"),
            ));
        } else if let Some(bad) = have.iter().find(|&&v| v != want) {
            parts.push((
                Some(false),
                format!("channel {c}: channel status {bad:#010x}, want {want:#010x}"),
            ));
        }
    }
    if !x.get("fs_hz").is_null() {
        let want = x.get("fs_hz").f().unwrap();
        parts.push((
            Some((fs / want - 1.0).abs() <= getf(x, "fs_rel", 0.01)),
            format!("frame rate {fs:.1} Hz vs {}", x.get("fs_hz").repr()),
        ));
    }
    let (v, why) = verdicts(&parts);
    let asked = ["audio", "status", "status_b", "fs_hz"]
        .iter()
        .any(|k| !x.get(k).is_null());
    out(if asked || v != V::Ok { v } else { V::Measured }, got, why)
}

// ---------------- quadrature ----------------

fn check_quadrature(cap: &Cap, x: &J) -> Out {
    let (Some(a), Some(b)) = (cap.ch(s(x, "a")), cap.ch(s(x, "b"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let rate = cap.rate;
    let (s0, _) = timing::states(&[a, b], cap.n);
    let st: Vec<u8> = s0.iter().map(|&t| (a.level_at(t) << 1) | b.level_at(t)).collect();
    let step = |p: u8, q: u8| -> Option<i64> {
        match (p, q) {
            (0b00, 0b10) | (0b10, 0b11) | (0b11, 0b01) | (0b01, 0b00) => Some(1),
            (0b10, 0b00) | (0b11, 0b10) | (0b01, 0b11) | (0b00, 0b01) => Some(-1),
            _ => None,
        }
    };
    let (mut fwd, mut back, mut err, mut rev, mut last) = (0i64, 0i64, 0i64, 0i64, 0i64);
    let mut errs: Vec<i64> = vec![];
    for k in 1..st.len() {
        match step(st[k - 1], st[k]) {
            None => {
                err += 1;
                if errs.len() < 16 {
                    errs.push(s0[k]);
                }
            }
            Some(v) => {
                if v > 0 {
                    fwd += 1
                } else {
                    back += 1
                }
                if last != 0 && v != last {
                    rev += 1;
                }
                last = v;
            }
        }
    }
    let t = &s0[1..];
    let min_step = t.windows(2).map(|w| w[1] - w[0]).min();
    let steps = fwd - back;
    let error_at: Vec<f64> = errs.iter().map(|&t| t as f64 / rate).collect();
    let got = jd! {"steps" => steps, "forward" => fwd, "backward" => back, "errors" => err,
    "error_at" => error_at.clone(), "reversals" => rev, "min_step_ticks" => min_step,
    "min_step_s" => min_step.map(|m| m as f64 / rate)};
    let mut parts = vec![];
    if let Some(m) = x.get("errors_max").i()
        && err > m
    {
        parts.push((
            Some(false),
            format!("{err} times both lines changed at once (first at {})", si(error_at[0])),
        ));
    }
    if let Some(w) = x.get("steps").i()
        && steps != w
    {
        parts.push((Some(false), format!("{steps} steps, want {w}")));
    }
    if let Some(c) = x.get("cycles").f()
        && steps as f64 != 4.0 * c
    {
        parts.push((
            Some(false),
            format!("{} cycles, want {}", g(steps as f64 / 4.0, 6), x.get("cycles").repr()),
        ));
    }
    match x.get("direction").s() {
        Some("forward") if back > 0 || fwd == 0 => {
            parts.push((Some(false), format!("{back} steps backward, {fwd} forward")))
        }
        Some("backward") if fwd > 0 || back == 0 => {
            parts.push((Some(false), format!("{fwd} steps forward, {back} backward")))
        }
        _ => {}
    }
    let (v, why) = verdicts(&parts);
    let asked = ["steps", "cycles", "direction", "errors_max"]
        .iter()
        .any(|k| !x.get(k).is_null());
    out(if asked || v != V::Ok { v } else { V::Measured }, got, why)
}

// ---------------- multi-line SPI ----------------

/// A window's clocks read phase by phase: (each phase's hex or None for a dummy, the clocks left over).
fn qspi_phases(clocks: &[Vec<u8>], phases: &[J]) -> Result<(Vec<Option<String>>, i64), String> {
    let (mut out, mut pos) = (vec![], 0usize);
    for (k, ph) in phases.iter().enumerate() {
        if !ph.get("dummy").is_null() {
            out.push(None);
            pos += ph.get("dummy").i().unwrap_or(0) as usize;
            continue;
        }
        let w = ph.get("width").i().unwrap_or(1) as usize;
        let lines: Vec<usize> = if ph.has("lines") {
            ph.get("lines")
                .list()
                .iter()
                .map(|v| v.i().unwrap_or(0) as usize)
                .collect()
        } else {
            (0..w).rev().collect()
        };
        let n = if !ph.get("bits").is_null() {
            ph.get("bits").i().unwrap_or(0) as usize / lines.len()
        } else if !ph.get("hex").is_null() {
            crate::j::py_str(ph.get("hex")).chars().count() * 4 / lines.len()
        } else if k == phases.len() - 1 {
            clocks.len().saturating_sub(pos)
        } else {
            return Err("a qspi phase needs hex or bits unless it is the last one".into());
        };
        let mut bits = String::new();
        for row in clocks.iter().skip(pos).take(n) {
            for &l in &lines {
                bits.push(if row.get(l).copied().unwrap_or(0) == 1 {
                    '1'
                } else {
                    '0'
                });
            }
        }
        pos += n;
        while !bits.len().is_multiple_of(4) {
            bits.push('0');
        }
        let h: String = bits
            .as_bytes()
            .chunks(4)
            .map(|c| {
                let v = c.iter().fold(0u8, |a, &b| (a << 1) | (b - b'0'));
                format!("{v:x}")
            })
            .collect();
        out.push(Some(h));
    }
    Ok((out, clocks.len() as i64 - pos as i64))
}

fn check_qspi(cap: &Cap, x: &J) -> Out {
    let (clk, cs) = (cap.ch(s(x, "clk")), cap.ch(s(x, "cs")));
    let io: Vec<Option<&Ch>> = x.get("io").list().iter().map(|p| cap.ch(p.s().unwrap_or(""))).collect();
    let (Some(clk), Some(cs)) = (clk, cs) else {
        return out(V::Unchecked, J::dict(), "pin not captured");
    };
    if io.iter().any(|c| c.is_none()) {
        return out(V::Unchecked, J::dict(), "pin not captured");
    }
    let io: Vec<&Ch> = io.into_iter().map(|c| c.unwrap()).collect();
    let rate = cap.rate;
    let mode = x.get("mode").i().unwrap_or(0);
    let active = if x.has("cs_active") {
        x.get("cs_active").i().unwrap_or(0) as u8
    } else {
        0
    };
    let cpol = (mode >> 1) as u8;
    let rise = mode == 0 || mode == 3;
    let t: Vec<i64> = (0..clk.edges.len())
        .filter(|&i| clk.after(i) == if rise { 1 } else { 0 })
        .map(|i| clk.edges[i])
        .collect();
    let mut marks = vec![0];
    marks.extend(&cs.edges);
    marks.push(cap.n);
    let mut windows: Vec<(i64, i64, Vec<i64>)> = vec![];
    let mut lv = cs.initial;
    for m in marks.windows(2) {
        let (a, b) = (m[0], m[1]);
        if lv == active && a > 0 && b < cap.n {
            windows.push((a, b, t.iter().copied().filter(|&v| v > a && v < b).collect()));
        }
        lv ^= 1;
    }
    let mut got = jd! {"windows" => windows.len()};
    if windows.is_empty() {
        return out(V::Ng, got, "no whole window of CS active");
    }
    let gaps: Vec<i64> = windows
        .iter()
        .filter(|w| w.2.len() > 1)
        .flat_map(|w| w.2.windows(2).map(|v| v[1] - v[0]).collect::<Vec<_>>())
        .collect();
    let freq = if gaps.is_empty() {
        None
    } else {
        Some(rate / median_i(&gaps))
    };
    got.set("freq_hz", freq);
    got.set("clocks", windows.iter().map(|w| w.2.len()).collect::<Vec<_>>());
    let idle_bad = windows.iter().filter(|w| clk.level_at(w.0) != cpol).count();
    let mut parts = vec![];
    if idle_bad > 0 {
        parts.push((
            Some(false),
            format!("SCK not at CPOL {cpol} as CS becomes active ({idle_bad} windows; mode {mode})"),
        ));
    }
    let rows: Vec<Vec<Vec<u8>>> = windows
        .iter()
        .map(|w| {
            w.2.iter()
                .map(|&ts| io.iter().map(|c| c.level_at((ts - 1).max(0))).collect())
                .collect()
        })
        .collect();
    let want = x.get("transfers");
    if want.is_null() {
        let w = io.len() as i64;
        let ph = [jd! {"width" => w, "lines" => (0..w).rev().collect::<Vec<_>>()}];
        got.set(
            "data",
            J::List(
                rows.iter()
                    .map(|m| J::from(qspi_phases(m, &ph).map(|r| r.0[0].clone()).unwrap_or_default()))
                    .collect(),
            ),
        );
    } else {
        let want = want.list();
        let mut reads = vec![];
        for (i, m) in rows.iter().take(want.len()).enumerate() {
            let phases = want[i].list();
            let (hexes, left) = match qspi_phases(m, phases) {
                Ok(r) => r,
                Err(e) => return out(V::Ng, got, e),
            };
            reads.push(J::List(hexes.iter().map(|h| J::from(h.clone())).collect()));
            for (k, (ph, h)) in phases.iter().zip(&hexes).enumerate() {
                if !ph.get("hex").is_null() {
                    let w = crate::j::py_str(ph.get("hex")).to_lowercase();
                    if h.as_deref() != Some(w.as_str()) {
                        parts.push((
                            Some(false),
                            format!(
                                "window #{i} phase #{k}: {}, not {w}",
                                h.clone().unwrap_or_else(|| "None".into())
                            ),
                        ));
                    }
                }
            }
            if left > 0 {
                parts.push((Some(false), format!("window #{i}: {left} clocks after its phases")));
            } else if left < 0 {
                parts.push((
                    Some(false),
                    format!("window #{i}: {} clocks short of its phases", -left),
                ));
            }
        }
        got.set("phases", J::List(reads));
        if rows.len() != want.len() {
            parts.push((
                Some(false),
                format!("{} windows of CS active, {} expected", rows.len(), want.len()),
            ));
        }
    }
    if let (Some(fx), Some(f)) = (x.get("freq_hz").f(), freq) {
        let err = f / fx - 1.0;
        if err.abs() > getf(x, "freq_rel", 0.1) {
            parts.push((
                Some(false),
                format!("SCK {f:.0} Hz is {:+.1}% off {fx:.0}", err * 100.0),
            ));
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

// ---------------- SD in SPI mode ----------------

/// (tick, mosi, miso, window) per byte, MSB first (`periph.spi_bytes`).
fn spi_bytes(clk: &Ch, mosi: &Ch, miso: &Ch, n: i64, mode: i64, cs: Option<&Ch>, act: u8) -> Vec<(i64, u8, u8)> {
    let (cpol, cpha) = ((mode >> 1) as u8, mode & 1);
    let ce = &clk.edges;
    let samp: Vec<i64> = (0..ce.len())
        .filter(|&i| {
            let lv = clk.after(i);
            if cpha == 0 { lv != cpol } else { lv == cpol }
        })
        .map(|i| ce[i])
        .collect();
    let wins = match cs {
        Some(cs) => cs_windows(cs, n, act).into_iter().map(|w| (w.0, w.1)).collect(),
        None => vec![(0, n)],
    };
    let mut out = vec![];
    for (a, b) in wins {
        let sm: Vec<i64> = samp.iter().copied().filter(|&v| v > a && v < b).collect();
        let mut k = 0;
        while k + 8 <= sm.len() {
            let (mut vo, mut vi) = (0u8, 0u8);
            for j in 0..8 {
                vo = vo << 1 | mosi.level_at(sm[k + j]);
                vi = vi << 1 | miso.level_at(sm[k + j]);
            }
            out.push((sm[k], vo, vi));
            k += 8;
        }
    }
    out
}

/// CS's windows of `active`: (from, to, whole: it began inside the capture).
pub fn cs_windows(cs: &Ch, n: i64, active: u8) -> Vec<(i64, i64, bool)> {
    let e = &cs.edges;
    let on: Vec<i64> = (0..e.len()).filter(|&i| cs.after(i) == active).map(|i| e[i]).collect();
    let off: Vec<i64> = (0..e.len()).filter(|&i| cs.after(i) != active).map(|i| e[i]).collect();
    let mut w = vec![];
    if cs.initial == active {
        w.push((0, off.first().copied().unwrap_or(n), false));
    }
    for &t in &on {
        let j = off.partition_point(|&o| o <= t);
        w.push((t, off.get(j).copied().unwrap_or(n), true));
    }
    w
}

fn crc7(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        for k in (0..8).rev() {
            let bit = (b >> k) & 1;
            let top = (c >> 6) & 1;
            c = (c << 1) & 0x7F;
            if top ^ bit == 1 {
                c ^= 0x09;
            }
        }
    }
    c
}

fn crc16(data: &[u8]) -> u16 {
    let mut c: u32 = 0;
    for &b in data {
        c ^= (b as u32) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x1021 } else { c << 1 };
            c &= 0xFFFF;
        }
    }
    c as u16
}

const SD_WRITE: [i64; 2] = [24, 25];

/// SD commands in SPI mode on the bytes (`periph.sd_spi`).
fn sd_spi(rows: &[(i64, u8, u8)]) -> Vec<J> {
    let mo: Vec<u8> = rows.iter().map(|r| r.1).collect();
    let mi: Vec<u8> = rows.iter().map(|r| r.2).collect();
    let nn = rows.len();
    let mut cmds = vec![];
    let mut blk: i64 = 512;
    let mut acmd_next = false;
    let mut i = 0usize;
    let read_block = |j: usize, size: usize| -> (Option<J>, usize) {
        let mut k = j;
        while k < nn && mi[k] == 0xFF && k - j < 100000 {
            if mo[k] & 0xC0 == 0x40 {
                return (None, k);
            }
            k += 1;
        }
        if k >= nn {
            return (None, k);
        }
        if mi[k] != 0xFE {
            return (
                Some(jd! {"t" => rows[k].0, "token" => mi[k], "error_token" => true}),
                k + 1,
            );
        }
        let d = &mi[(k + 1).min(nn)..(k + 1 + size).min(nn)];
        let c = &mi[(k + 1 + size).min(nn)..(k + 3 + size).min(nn)];
        let mut b = jd! {"t" => rows[k].0, "token" => 0xFE, "len" => d.len(), "data" => hex(d),
        "nac_bytes" => k - j};
        if d.len() == size && c.len() == 2 {
            b.set("crc_ok", crc16(d) == ((c[0] as u16) << 8 | c[1] as u16));
        } else {
            b.set("cut", true);
        }
        (Some(b), k + 3 + size)
    };
    while i + 6 <= nn {
        let b0 = mo[i];
        if b0 & 0xC0 != 0x40 {
            i += 1;
            continue;
        }
        let cmdb = &mo[i..i + 6];
        let cmd = (b0 & 0x3F) as i64;
        let arg = u32::from_be_bytes([cmdb[1], cmdb[2], cmdb[3], cmdb[4]]) as i64;
        let mut c = jd! {"t" => rows[i].0, "cmd" => cmd, "acmd" => acmd_next, "arg" => arg,
        "crc_ok" => crc7(&cmdb[..5]) == cmdb[5] >> 1, "end_bit" => cmdb[5] & 1 == 1};
        let is_acmd = acmd_next;
        acmd_next = false;
        let j = i + 6;
        let mut k = j;
        while k < nn && k - j < 9 && mi[k] & 0x80 != 0 {
            k += 1;
        }
        if k >= nn || mi[k] & 0x80 != 0 {
            c.set("r1", J::Null);
            cmds.push(c);
            i = j;
            continue;
        }
        c.set("ncr", k - j);
        let r1 = mi[k];
        c.set("r1", r1);
        k += 1;
        let extra = if is_acmd {
            (cmd == 13) as usize
        } else {
            match cmd {
                8 | 58 => 4,
                13 => 1,
                _ => 0,
            }
        };
        if extra > 0 {
            c.set("resp", hex(&mi[k.min(nn)..(k + extra).min(nn)]));
            k += extra;
        }
        c.set(
            "idle_ok",
            mi[i..i + 6].iter().all(|&v| v == 0xFF) && mo[j.min(nn)..k.min(nn)].iter().all(|&v| v == 0xFF),
        );
        if [12, 28, 29, 38].contains(&cmd) && !is_acmd {
            let b = k;
            while k < nn && mi[k] == 0x00 {
                k += 1;
            }
            c.set("busy_bytes", k - b);
        }
        if cmd == 55 && !is_acmd && r1 & 0xFE == 0 {
            acmd_next = true;
        }
        if cmd == 16 && !is_acmd && r1 == 0 {
            blk = arg;
        }
        if r1 == 0 {
            let size = if is_acmd {
                match cmd {
                    13 => Some(64),
                    22 => Some(4),
                    51 => Some(8),
                    _ => None,
                }
            } else {
                match cmd {
                    17 | 18 => Some(blk as usize),
                    9 | 10 => Some(16),
                    _ => None,
                }
            };
            if let Some(size) = size.filter(|&s| s > 0) {
                let mut blocks = vec![];
                loop {
                    let (b, k2) = read_block(k, size);
                    k = k2;
                    let Some(b) = b else { break };
                    let et = b.get("error_token").truthy();
                    blocks.push(b);
                    if cmd != 18 || is_acmd || et {
                        break;
                    }
                }
                c.set("blocks", J::List(blocks));
            } else if SD_WRITE.contains(&cmd) && !is_acmd {
                let mut blocks = vec![];
                let bl = blk as usize;
                while k < nn {
                    while k < nn && mo[k] == 0xFF {
                        k += 1;
                    }
                    if k >= nn || ![0xFE, 0xFC, 0xFD].contains(&mo[k]) {
                        break;
                    }
                    if mo[k] == 0xFD {
                        let b = k + 1;
                        k += 2;
                        while k < nn && mi[k] == 0x00 {
                            k += 1;
                        }
                        c.set("stop_busy_bytes", k as i64 - b as i64);
                        break;
                    }
                    let d = &mo[(k + 1).min(nn)..(k + 1 + bl).min(nn)];
                    let cr = &mo[(k + 1 + bl).min(nn)..(k + 3 + bl).min(nn)];
                    let mut b = jd! {"t" => rows[k].0, "token" => mo[k], "len" => d.len(), "data" => hex(d),
                    "crc_ok" => cr.len() == 2 && crc16(d) == ((cr[0] as u16) << 8 | cr[1] as u16)};
                    k += 3 + bl;
                    while k < nn && mi[k] == 0xFF && mo[k] == 0xFF {
                        k += 1;
                    }
                    if k < nn {
                        b.set("data_response", mi[k]);
                        k += 1;
                        let s0 = k;
                        while k < nn && mi[k] == 0x00 {
                            k += 1;
                        }
                        b.set("busy_bytes", k - s0);
                    }
                    let dr = b.get("data_response").i().unwrap_or(0x05);
                    blocks.push(b);
                    if cmd == 24 || (dr & 0x1F) != 0x05 {
                        break;
                    }
                }
                c.set("blocks", J::List(blocks));
            }
        }
        cmds.push(c);
        i = k.max(j);
    }
    cmds
}

fn sd_row(c: &J) -> J {
    let mut o = jd! {"cmd" => c.get("cmd"), "acmd" => c.get("acmd"), "arg" => c.get("arg"), "r1" => c.get("r1")};
    if c.has("resp") {
        o.set("resp", c.get("resp").clone());
    }
    if c.has("blocks") {
        o.set(
            "data",
            J::List(c.get("blocks").list().iter().map(|b| b.get("data").clone()).collect()),
        );
    }
    o
}

fn check_sd_spi(cap: &Cap, x: &J) -> Out {
    let pins: Vec<Option<&Ch>> = ["clk", "mosi", "miso"]
        .iter()
        .filter(|k| x.get(k).truthy())
        .map(|k| cap.ch(s(x, k)))
        .collect();
    let cs = if x.get("cs").truthy() { cap.ch(s(x, "cs")) } else { None };
    if pins.len() < 3 || pins.iter().any(|p| p.is_none()) || (x.get("cs").truthy() && cs.is_none()) {
        return out(V::Unchecked, J::dict(), "pins not captured");
    }
    let (clk, mosi, miso) = (pins[0].unwrap(), pins[1].unwrap(), pins[2].unwrap());
    let rate = cap.rate;
    let act = if x.has("cs_active") {
        x.get("cs_active").i().unwrap_or(0) as u8
    } else {
        0
    };
    let mode = match x.get("mode").i() {
        Some(m) => m,
        None => {
            let m = timing::spi_mode(clk, &[mosi, miso], cap.n, cs, act);
            let line = m.lines.iter().find_map(|(_, v)| v.cpha);
            match (m.cpol, line) {
                (Some(cp), Some(ph)) => cp as i64 * 2 + ph as i64,
                _ => return out(V::Unchecked, J::dict(), "the SPI mode is not on the lines: give mode="),
            }
        }
    };
    let rows = spi_bytes(clk, mosi, miso, cap.n, mode, cs, act);
    let cmds = sd_spi(&rows);
    let listed: Vec<J> = cmds
        .iter()
        .map(|c| {
            let mut d = sd_row(c);
            d.set("t_s", c.get("t").f().unwrap() / rate);
            for k in ["ncr", "crc_ok", "busy_bytes"] {
                if c.has(k) {
                    d.set(k, c.get(k).clone());
                }
            }
            d
        })
        .collect();
    let got = jd! {"mode" => mode, "bytes" => rows.len(), "commands" => listed};
    if cmds.is_empty() {
        return out(V::Ng, got, "no SD command on MOSI");
    }
    let mut parts = vec![];
    if !x.get("commands").is_null() {
        let want: Vec<J> = x
            .get("commands")
            .list()
            .iter()
            .map(|w| {
                if w.has("cmd") {
                    let mut d = jd! {"acmd" => false};
                    for (k, v) in w.items() {
                        d.set(k, v.clone());
                    }
                    d
                } else {
                    w.clone()
                }
            })
            .collect();
        let rows_: Vec<J> = cmds.iter().map(sd_row).collect();
        let mut have: Vec<J> = want
            .iter()
            .zip(&rows_)
            .map(|(w, g)| {
                let mut d = J::dict();
                for (k, _) in w.items() {
                    d.set(k, g.get(k).clone());
                }
                d
            })
            .collect();
        have.extend(rows_.iter().skip(want.len()).cloned());
        parts.push(seq_diff("commands", &want, &have));
    }
    let ncr_max = if x.has("ncr_max_bytes") {
        x.get("ncr_max_bytes").i().unwrap_or(8)
    } else {
        8
    };
    for c in &cmds {
        let name = format!(
            "{}{}",
            if c.get("acmd").truthy() { "ACMD" } else { "CMD" },
            c.get("cmd").repr()
        );
        let at = format!("{name} at {}", si(c.get("t").f().unwrap() / rate));
        if c.get("r1").is_null() {
            parts.push((Some(false), format!("{at}: no R1 within {ncr_max} bytes")));
        } else if c.get("ncr").i().unwrap_or(0) > ncr_max {
            parts.push((Some(false), format!("{at}: R1 after {} bytes", c.get("ncr").repr())));
        }
        if !c.get("end_bit").truthy() {
            parts.push((Some(false), format!("{at}: the end bit is 0")));
        }
        let cmd = c.get("cmd").i().unwrap_or(0);
        if (cmd == 0 || cmd == 8) && !c.get("acmd").truthy() && !c.get("crc_ok").truthy() {
            parts.push((Some(false), format!("{at}: a wrong CRC7 (CMD0 and CMD8 need it)")));
        }
        for b in c.get("blocks").list() {
            if !SD_WRITE.contains(&cmd) && b.get("crc_ok") == &J::Bool(false) {
                parts.push((
                    Some(false),
                    format!(
                        "{at}: the card's block at {} has a wrong CRC16",
                        si(b.get("t").f().unwrap() / rate)
                    ),
                ));
            }
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}

// ---------------- I2C timing ----------------

fn check_i2c_timing(cap: &Cap, x: &J) -> Out {
    let (Some(scl), Some(sda)) = (cap.ch(s(x, "scl")), cap.ch(s(x, "sda"))) else {
        return out(V::Unchecked, J::dict(), "pins not captured");
    };
    let rate = cap.rate;
    let lim = timing::i2c_limits(x);
    let r = timing::i2c_times(scl, sda, cap.n);
    let res = timing::res_of(&[scl, sda]);
    let mut got = J::dict();
    for k in timing::I2C_TIMES {
        got.set(k, times(&fp(&r[k]), rate));
    }
    got.set("resolution_s", res as f64 / rate);
    let per: Vec<f64> = r["periods"].iter().map(|p| p.1 as f64).collect();
    if !per.is_empty() {
        let mn = per.iter().cloned().fold(f64::INFINITY, f64::min);
        got.set(
            "f_scl",
            jd! {"count" => per.len(), "max_hz" => rate / mn, "median_hz" => rate / median(&per)},
        );
    }
    if r["t_low"].is_empty() {
        return out(V::Unchecked, got, "no clock pulse inside a transaction");
    }
    let mut parts = vec![];
    for (k, (lo, hi)) in &lim {
        if k == "f_scl" {
            if let Some(hi) = hi
                && !per.is_empty()
            {
                let i = argmin(per.iter().copied());
                let at = r["periods"][i].0;
                let j = judge(per[i], res as f64, Some(rate / hi), None);
                parts.push((
                    j,
                    format!(
                        "SCL {} kHz {} {} kHz at {}",
                        g(rate / per[i] / 1e3, 4),
                        if j == Some(false) { ">" } else { "near" },
                        g(hi / 1e3, 4),
                        si(at as f64 / rate)
                    ),
                ));
            }
        } else if let Some(v) = r.iter().find(|(n, _)| **n == k.as_str()).map(|(_, v)| v)
            && !v.is_empty()
        {
            parts.extend(limit_of(k, &fp(v), res, *lo, *hi, rate));
        }
    }
    let (v, why) = verdicts(&parts);
    out(v, got, why)
}
