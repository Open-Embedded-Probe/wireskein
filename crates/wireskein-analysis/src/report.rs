//! What analyze tells beside a claim (Python's `decode.py`): each frame of a clocked bus given its clock's period
//! and high (`frame_clocks`), the speeds the clock ran at (`clock_speeds`) and those sampled too slowly
//! (`undersampled`), what the bus did not read (`unread`), and how well it read (`reading`: its frames, the noise on
//! its lines, a line stopped or oscillating).

use serde_json::{Map, Value, json};
use wireskein_io::Line;

pub const IDLE_S: f64 = 1e-3; // activity apart by more than this is two bursts (a bus with no bit times of its own)
pub const IDLE_BITS: i64 = 16;
const LEAST_HALF: f64 = 5.0; // samples in the shorter of a clock's high and low, at least, to read it surely
const BEST_HALF: f64 = 10.0;
pub const SAME_SPEED: f64 = 1.25;
const RING: i64 = 3; // samples: a data line's pulses this short, where the clock is slow, a reflection's
pub const STOPPED_AFTER: usize = 16;
const OSCILLATING: usize = 1000;
const NOISY: f64 = 0.01;

/// The protocols a clock samples (their bits and frames are where they were cut, not what was read).
pub const SYNC: [&str; 10] = [
    "i2c",
    "spi",
    "rvswd",
    "swd",
    "mdio",
    "i2s",
    "jtag",
    "sync_unknown",
    "ps2",
    "sdmmc",
];

pub fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

fn r1(x: f64) -> f64 {
    (x * 10.0).round_ties_even() / 10.0
}

fn r4(x: f64) -> f64 {
    (x * 1e4).round_ties_even() / 1e4
}

/// The level after each change of a line.
fn after(l: &Line, i: usize) -> u8 {
    l.initial ^ (((i + 1) & 1) as u8)
}

/// The layer a bus reads frame by frame (Python's `export.LOWER`).
pub fn lower(protocol: &str) -> &'static str {
    match protocol {
        "i2c" | "swim" => "transactions",
        "spi" => "transfers",
        "uart" => "bytes",
        "rvswd" | "swio" => "dmi",
        "swd" | "usb_pd" | "dcc" | "usb" | "dmx512" => "packets",
        "cec" | "sent" => "messages",
        "spdif" => "subframes",
        "jtag" => "scans",
        "dht" => "readings",
        "sync_unknown" => "summary",
        "onewire" => "resets",
        _ => "frames",
    }
}

/// The frames a clocked bus read (RVSWD's and SWIO's DMI accesses, else its lower layer).
pub fn frame_items<'a>(lay: &'a mut Map<String, Value>, protocol: &str) -> Option<&'a mut Vec<Value>> {
    let k = if lay.get("dmi").is_some_and(|v| v.is_array()) {
        "dmi"
    } else {
        lower(protocol)
    };
    lay.get_mut(k).and_then(|v| v.as_array_mut())
}

/// Each frame given its clock: `period_ns` (the median of its clock's periods, kept when the reader gave it) and
/// `high_ns` (the median of its highs), for a frame of 8 clocks or more.
pub fn frame_clocks(items: &mut [Value], clock: &Line, rate: f64) {
    if clock.step != 1 {
        return;
    }
    let e = &clock.edges;
    let (mut t, mut high) = (vec![], vec![]);
    for i in 0..e.len().saturating_sub(1) {
        if after(clock, i) == 1 {
            t.push(e[i]);
            high.push((e[i + 1] - e[i]) as f64);
        }
    }
    let ns = 1e9 / rate;
    for x in items.iter_mut() {
        let (Some(s), Some(en)) = (x.get("s").and_then(|v| v.as_i64()), x.get("e").and_then(|v| v.as_i64())) else {
            continue;
        };
        if en <= s {
            continue;
        }
        let lo = t.partition_point(|&v| v < s);
        let hi = t.partition_point(|&v| v <= en);
        if hi >= lo + 8 {
            let o = x.as_object_mut().unwrap();
            if !o.contains_key("period_ns") {
                let mut d: Vec<f64> = t[lo..hi].windows(2).map(|w| (w[1] - w[0]) as f64).collect();
                o.insert("period_ns".into(), json!(r1(median(&mut d) * ns)));
            }
            let mut h: Vec<f64> = high[lo..hi - 1].to_vec();
            o.insert("high_ns".into(), json!(r1(median(&mut h) * ns)));
        }
    }
}

pub fn speed(rows: &[[f64; 4]], rate: f64, frames: Option<usize>) -> Value {
    let p = median(&mut rows.iter().map(|r| r[0]).collect::<Vec<_>>());
    let h = median(&mut rows.iter().map(|r| r[1]).collect::<Vec<_>>());
    let tight = LEAST_HALF * 1e9 / rate;
    let s = rows.iter().map(|r| r[2]).fold(f64::INFINITY, f64::min);
    let e = rows.iter().map(|r| r[3]).fold(f64::NEG_INFINITY, f64::max);
    json!({"period_ns": r1(p), "freq_hz": r1(1e9 / p), "high_ns": r1(h), "low_ns": r1(p - h),
           "frames": frames.unwrap_or(rows.len()), "s": s as i64, "e": e as i64, "rate_ok": h.min(p - h) >= tight})
}

/// The speeds a bus's clock ran at, from its frames' clocks: frames whose periods are within SAME_SPEED of a speed's
/// fastest are that speed, fastest first.
pub fn clock_speeds(rate: f64, items: &[Value]) -> Vec<Value> {
    let mut rows: Vec<[f64; 4]> = items
        .iter()
        .filter_map(|x| {
            let p = x.get("period_ns")?.as_f64().filter(|&p| p != 0.0)?;
            Some([
                p,
                x.get("high_ns")?.as_f64()?,
                x.get("s")?.as_f64()?,
                x.get("e")?.as_f64()?,
            ])
        })
        .collect();
    rows.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mut groups: Vec<Vec<[f64; 4]>> = vec![];
    for r in rows {
        match groups.last_mut() {
            Some(g) if r[0] <= g[0][0] * SAME_SPEED => g.push(r),
            _ => groups.push(vec![r]),
        }
    }
    groups.iter().map(|g| speed(g, rate, None)).collect()
}

/// The speeds sampled too slowly, the worst of them, and the rates that would read it.
pub fn undersampled(rate: f64, speeds: &[Value], clock: &str) -> Option<Value> {
    let bad: Vec<&Value> = speeds.iter().filter(|g| g["rate_ok"] == json!(false)).collect();
    if bad.is_empty() {
        return None;
    }
    let lo = |g: &Value| g["high_ns"].as_f64().unwrap().min(g["low_ns"].as_f64().unwrap());
    let worst = bad.iter().min_by(|a, b| lo(a).partial_cmp(&lo(b)).unwrap()).unwrap();
    let shortest = (1e9 / rate).max(lo(worst)) / 1e9;
    Some(json!({"clock": clock, "rate_hz": rate,
                "frames": bad.iter().map(|g| g["frames"].as_i64().unwrap_or(0)).sum::<i64>(),
                "period_ns": worst["period_ns"], "high_ns": worst["high_ns"], "low_ns": worst["low_ns"],
                "least_rate_hz": (LEAST_HALF / shortest / 1e6).ceil() * 1e6,
                "best_rate_hz": (BEST_HALF / shortest / 1e6).ceil() * 1e6}))
}

/// The clock's period around each tick (`kernels.local_period`): the shorter of the q-th percentiles of the 8
/// intervals before the one it falls in and the 8 after.
fn local_period(ticks: &[i64], at: &[i64], q: f64) -> Vec<f64> {
    if ticks.len() < 2 {
        return vec![0.0; at.len()];
    }
    let n = ticks.len() - 1;
    let m = 8.min(n);
    let pct = |j0: usize| -> f64 {
        let mut x: Vec<f64> = (j0..j0 + m).map(|i| (ticks[i + 1] - ticks[i]) as f64).collect();
        x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let pos = q / 100.0 * (m - 1) as f64;
        let lo = pos.floor() as usize;
        let hi = (lo + 1).min(m - 1);
        let t = pos - lo as f64;
        let (a, b) = (x[lo], x[hi]);
        if t >= 0.5 {
            b - (b - a) * (1.0 - t)
        } else {
            a + (b - a) * t
        }
    };
    at.iter()
        .map(|&a| {
            let j = ticks.partition_point(|&t| t <= a) as i64 - 1;
            let clip = |x: i64| x.clamp(0, (n - m) as i64) as usize;
            pct(clip(j - m as i64)).min(pct(clip(j + 1)))
        })
        .collect()
}

/// Signs of noise on a clocked bus in ticks [lo, hi): the clock's glitches (pulses of a sample where its period is
/// 8 samples or more), each data line's ringing (pulses under RING where the clock is slow enough).
pub fn noise(lines: &[&Line], clock: &Line, data: &[&Line], lo: i64, hi: i64) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("glitches".into(), json!(0));
    out.insert("clock_edges".into(), json!(0));
    out.insert("ringing".into(), json!({}));
    out.insert("data_edges".into(), json!({}));
    let _ = lines;
    if clock.step != 1 {
        return out;
    }
    let e = &clock.edges;
    out.insert(
        "clock_edges".into(),
        json!(e.iter().filter(|&&x| x >= lo && x < hi).count()),
    );
    let rises: Vec<i64> = (0..e.len()).filter(|&i| after(clock, i) == 1).map(|i| e[i]).collect();
    let near: Vec<usize> = (1..e.len())
        .filter(|&i| e[i] - e[i - 1] <= 1 && e[i] >= lo && e[i] < hi)
        .collect();
    if !near.is_empty() && rises.len() > 2 {
        let lp = local_period(&rises, &near.iter().map(|&i| e[i]).collect::<Vec<_>>(), 50.0);
        let kept: Vec<usize> = near
            .iter()
            .zip(&lp)
            .filter(|&(_, &p)| p >= 8.0)
            .map(|(&i, _)| i)
            .collect();
        out.insert("glitches".into(), json!(kept.len()));
        let u: std::collections::BTreeSet<usize> = kept.iter().flat_map(|&i| [i - 1, i]).collect();
        out.insert("glitch_edges".into(), json!(u.len()));
    }
    let (mut ringing, mut ring_e, mut data_e) = (Map::new(), Map::new(), Map::new());
    for d in data {
        let de = &d.edges;
        if d.step != 1 || de.len() < 2 || rises.len() < 3 {
            continue;
        }
        let k: Vec<usize> = (1..de.len())
            .filter(|&i| de[i] - de[i - 1] < RING && de[i] >= lo && de[i] < hi)
            .collect();
        let k: Vec<usize> = if k.is_empty() {
            k
        } else {
            let lp = local_period(&rises, &k.iter().map(|&i| de[i]).collect::<Vec<_>>(), 25.0);
            k.into_iter()
                .zip(lp)
                .filter(|&(_, p)| p >= (3 * RING) as f64)
                .map(|(i, _)| i)
                .collect()
        };
        ringing.insert(d.name.clone(), json!(k.len()));
        let u: std::collections::BTreeSet<usize> = k.iter().flat_map(|&i| [i - 1, i]).collect();
        ring_e.insert(d.name.clone(), json!(u.len()));
        data_e.insert(d.name.clone(), json!(de.iter().filter(|&&x| x >= lo && x < hi).count()));
    }
    out.insert("ringing".into(), Value::Object(ringing));
    if !ring_e.is_empty() {
        out.insert("ringing_edges".into(), Value::Object(ring_e));
    }
    out.insert("data_edges".into(), Value::Object(data_e));
    out
}

/// The noise as shares of the changes; `noisy` when one is over NOISY.
pub fn shares(r: &mut Map<String, Value>) {
    let ce = r.get("clock_edges").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let ge = r.get("glitch_edges").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let gs = if ce > 0.0 { r4(ge / ce) } else { 0.0 };
    let mut rs = Map::new();
    if let Some(ring) = r.get("ringing").and_then(|v| v.as_object()) {
        for k in ring.keys() {
            let de = r["data_edges"].get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
            let re = r
                .get("ringing_edges")
                .and_then(|v| v.get(k))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            rs.insert(k.clone(), json!(if de > 0.0 { r4(re / de) } else { 0.0 }));
        }
    }
    let noisy = gs > NOISY || rs.values().any(|v| v.as_f64().unwrap_or(0.0) > NOISY);
    r.insert("glitch_share".into(), json!(gs));
    r.insert("ringing_share".into(), Value::Object(rs));
    r.insert("noisy".into(), json!(noisy));
}

/// Each line's changes, a bus line stopped while the others go on, a line oscillating.
fn line_faults(lines: &[&Line], n_samples: i64) -> Map<String, Value> {
    let mut out = Map::new();
    if lines.is_empty() {
        return out;
    }
    let edges: Vec<&Line> = lines.iter().copied().filter(|l| l.step == 1).collect();
    let mut lc = Map::new();
    for l in &edges {
        lc.insert(
            l.name.clone(),
            json!({"changes": l.edges.len(), "last": l.edges.last()}),
        );
    }
    out.insert("line_changes".into(), Value::Object(lc));
    let (mut stopped, mut osc) = (vec![], vec![]);
    for l in &edges {
        let last = l.edges.last().copied().unwrap_or(0);
        let after: usize = edges
            .iter()
            .filter(|o| o.name != l.name)
            .map(|o| o.edges.len() - o.edges.partition_point(|&x| x <= last))
            .sum();
        if after >= STOPPED_AFTER && (l.edges.is_empty() || last < n_samples - 1) {
            stopped.push(json!({"line": l.name, "tick": last, "others_after": after}));
        }
        if l.edges.len() > OSCILLATING {
            let e = &l.edges;
            let close: Vec<bool> = e.windows(2).map(|w| w[1] - w[0] <= 2).collect();
            let mut i = 0;
            while i < close.len() {
                if close[i] {
                    let mut j = i;
                    while j < close.len() && close[j] {
                        j += 1;
                    }
                    if j - i >= OSCILLATING {
                        osc.push(json!({"line": l.name, "s": e[i], "e": e[j], "changes": j - i + 1}));
                    }
                    i = j;
                } else {
                    i += 1;
                }
            }
        }
    }
    if !stopped.is_empty() {
        out.insert("stopped".into(), json!(stopped));
    }
    if !osc.is_empty() {
        out.insert("oscillating".into(), json!(osc));
    }
    out
}

/// (s, e) of every item with a time in the layers but `skip`.
fn items_of(lay: &Map<String, Value>, skip: &[&str]) -> Vec<(i64, i64)> {
    let mut out = vec![];
    for (k, v) in lay {
        if skip.contains(&k.as_str()) {
            continue;
        }
        match v {
            Value::Array(a) => {
                for x in a {
                    if let Some(s) = x.get("s").and_then(|v| v.as_i64()) {
                        out.push((s, x.get("e").and_then(|v| v.as_i64()).unwrap_or(s)));
                    }
                }
            }
            Value::Object(o) => {
                if let Some(Value::Array(ss)) = o.get("s") {
                    out.extend(ss.iter().filter_map(|v| v.as_i64()).map(|s| (s, s)));
                }
            }
            _ => {}
        }
    }
    out
}

/// The changes outside every span [s - margin, e + margin] (spans sorted by start).
pub fn uncovered(e: &[i64], spans: &[(i64, i64)], margin: i64) -> Vec<i64> {
    if spans.is_empty() {
        return e.to_vec();
    }
    let lo: Vec<i64> = spans.iter().map(|s| s.0 - margin).collect();
    let mut hi = Vec::with_capacity(spans.len());
    let mut m = i64::MIN;
    for s in spans {
        m = m.max(s.1);
        hi.push(m + margin);
    }
    e.iter()
        .copied()
        .filter(|&x| {
            let k = lo.partition_point(|&v| v <= x) as i64 - 1;
            !(k >= 0 && x <= hi[k as usize])
        })
        .collect()
}

pub fn split_at_gaps(e: &[i64], idle: i64) -> Vec<Vec<i64>> {
    let mut out: Vec<Vec<i64>> = vec![];
    for (i, &x) in e.iter().enumerate() {
        if i == 0 || x - e[i - 1] > idle {
            out.push(vec![]);
        }
        out.last_mut().unwrap().push(x);
    }
    out
}

/// A select line's changes that bound a window holding an item, and the empty windows.
fn selections(sel: &Line, others: &[&Line], items: &[(i64, i64)], n: i64) -> (Vec<bool>, Vec<Value>) {
    let e = &sel.edges;
    if e.is_empty() || items.is_empty() {
        return (vec![false; e.len()], vec![]);
    }
    let mut starts: Vec<i64> = items.iter().map(|x| x.0).collect();
    starts.sort_unstable();
    let level_at = |t: i64| sel.initial ^ ((e.partition_point(|&x| x <= t) & 1) as u8);
    let ones = starts.iter().filter(|&&s| level_at(s) == 1).count();
    let active = (ones * 2 > starts.len()) as u8;
    let mut other: Vec<i64> = others.iter().flat_map(|l| l.edges.iter().copied()).collect();
    other.sort_unstable();
    let (mut seen, mut empty) = (vec![false; e.len()], vec![]);
    for i in (0..e.len()).filter(|&i| after(sel, i) == active) {
        let j = (i + 1 < e.len()).then_some(i + 1);
        let (a, b) = (e[i], j.map_or(n, |j| e[j]));
        let k = starts.partition_point(|&s| s < a);
        if k < starts.len() && starts[k] <= b {
            seen[i] = true;
            if let Some(j) = j {
                seen[j] = true;
            }
        } else if let Some(j) = j
            && other.partition_point(|&x| x <= a) == other.partition_point(|&x| x < b)
        {
            seen[i] = true;
            seen[j] = true;
            empty.push(json!({"s": a, "e": b}));
        }
    }
    (seen, empty)
}

/// The stretches of activity on a bus's lines that no item it read covers ({s, e, edges}).
pub fn unread(
    protocol: &str,
    lines: &[&Line],
    lay: &Map<String, Value>,
    idle: i64,
    select: Option<&Line>,
    n: i64,
    empty: &mut Vec<Value>,
) -> Vec<Value> {
    let mut skip: Vec<&str> = vec![];
    if SYNC.contains(&protocol) {
        skip.extend(["bits", "frames"]);
    } else if protocol == "swio" {
        skip.push("pulses");
    }
    if protocol == "rvswd" || protocol == "swio" {
        skip.push("dm");
    }
    let items = items_of(lay, &skip);
    let mut starts: Vec<i64> = items.iter().map(|x| x.0).collect();
    starts.sort_unstable();
    let mut spans: Vec<(i64, i64)> = items.iter().copied().filter(|x| x.1 > x.0).collect();
    spans.sort_unstable();
    let margin = (idle / IDLE_BITS * 2).max(1);
    let mut all: Vec<i64> = lines.iter().flat_map(|l| l.edges.iter().copied()).collect();
    all.sort_unstable();
    if let Some(sel) = select {
        let with_end: Vec<(i64, i64)> = items.iter().copied().filter(|x| x.1 > x.0).collect();
        let others: Vec<&Line> = lines.iter().copied().filter(|l| l.name != sel.name).collect();
        let (seen, emp) = selections(sel, &others, if with_end.is_empty() { &items } else { &with_end }, n);
        empty.extend(emp);
        let drop: std::collections::BTreeSet<i64> = sel
            .edges
            .iter()
            .zip(&seen)
            .filter(|(_, s)| **s)
            .map(|(&x, _)| x)
            .collect();
        if !drop.is_empty() {
            all.retain(|x| !drop.contains(x));
        }
    }
    let mut un = vec![];
    for x in split_at_gaps(&all, idle) {
        let (s, e) = (x[0], *x.last().unwrap());
        if spans.iter().any(|sp| sp.0 <= e && sp.1 >= s) {
            let left = uncovered(&x, &spans, margin);
            for y in split_at_gaps(&left, idle) {
                un.push(json!({"s": y[0], "e": y[y.len() - 1], "edges": y.len()}));
            }
            continue;
        }
        let j = starts.partition_point(|&v| v < s - idle);
        if j < starts.len() && starts[j] <= e {
            continue;
        }
        if items.iter().any(|&(a, z)| a <= s && e <= z) {
            continue;
        }
        un.push(json!({"s": s, "e": e, "edges": x.len()}));
    }
    if protocol == "rvswd" || protocol == "swio" {
        un.retain(|u| u["edges"].as_u64().unwrap_or(0) >= 3);
    }
    un
}

/// How well a bus read: its frames, ok, unanswered, restored, repaired, what it did not read, every speed sampled
/// well enough, the noise on its lines, a line stopped or oscillating.
pub fn reading(
    frames: &[Value],
    uninterpreted: Option<&[Value]>,
    speeds: &[Value],
    lines: &[&Line],
    clock: Option<&Line>,
    data: &[&Line],
    n: i64,
) -> Map<String, Value> {
    let fr: Vec<&Value> = frames.iter().filter(|x| x.is_object()).collect();
    let count = |f: &dyn Fn(&Value) -> bool| fr.iter().filter(|x| f(x)).count();
    let mut r = Map::new();
    r.insert("frames".into(), json!(fr.len()));
    r.insert("ok".into(), json!(count(&|x| x.get("ok") != Some(&json!(false)))));
    r.insert(
        "unanswered".into(),
        json!(count(&|x| x.get("answered") == Some(&json!(false)))),
    );
    r.insert("restored".into(), json!(count(&|x| x.get("restored").is_some())));
    r.insert("repaired".into(), json!(count(&|x| x.get("repaired").is_some())));
    r.insert(
        "rate_ok".into(),
        json!(speeds.iter().all(|g| g["rate_ok"] == json!(true))),
    );
    if let Some(un) = uninterpreted {
        r.insert("unread".into(), json!(un.len()));
        r.insert(
            "unread_edges".into(),
            json!(un.iter().map(|u| u["edges"].as_i64().unwrap_or(0)).sum::<i64>()),
        );
    }
    if let Some(c) = clock {
        r.extend(noise(lines, c, data, 0, n));
        shares(&mut r);
    }
    r.extend(line_faults(lines, n));
    r
}
