//! Capture files written (the readers' pair; Python's `_fileformat.write` through `fileio.save`, `srio.write_sr`,
//! `vcdio.write_vcd`): a capture's parts with their logic lines, the lines kept as changes at a coarser step, the
//! analog channels, written as a WireSkein file, a sigrok session or a VCD, the same that Python writes as its readers
//! read it back.
//!
//! A WireSkein file (docs/wireskein-format.md): a line of one tick a sample as its changes (`events`) when that is
//! smaller than its bits, else as its samples (`bits`; a reduced one, `any_high` / `any_low`, as its intervals); a
//! line kept as changes at a coarser step as `events` (`change_latch` as its intervals); analog as raw integers or
//! float32 volts. The channel data deflated fast (level 1) unless its first MiB does not shrink by a tenth (stored);
//! entries carried from another file (attachments, notes) written as they are.

use crate::j::J;
use crate::{Analog, Capture, Events, Line, Part, jd};
use serde_json::{Map, Value};
use std::io::Write;
use std::path::Path;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// A part's own facts (wireskein-format §3.3): in a part's meta, not the capture's.
pub const PART_META: [&str; 4] = ["start_ns", "start_uncertainty_ns", "trigger_tick", "time_base_slipped"];
/// a fraction's numerator and denominator stay exact as JavaScript numbers (§3)
const MAX_TERM: i128 = (1 << 53) - 1;
/// a channel's data is deflated unless its first MiB does not shrink by a tenth
const TRY_BYTES: usize = 1 << 20;
/// ticks per logic-1-N / analog-1-K-N file of a .sr
const SR_CHUNK: i64 = 4 << 20;

fn gcd(a: i128, b: i128) -> i128 {
    if b == 0 { a.abs() } else { gcd(b, a % b) }
}

/// ceil(n / d), d > 0.
fn ceil_div(n: i128, d: i128) -> i128 {
    n.div_euclid(d) + (n.rem_euclid(d) != 0) as i128
}

/// Python's round of a fraction n / d (d > 0): to the nearest integer, ties to even.
fn round_frac(n: i128, d: i128) -> i128 {
    let (q, r) = (n.div_euclid(d), n.rem_euclid(d));
    if 2 * r > d || (2 * r == d && q % 2 != 0) {
        q + 1
    } else {
        q
    }
}

fn terms(f: (i64, i64), what: &str) -> Result<J, String> {
    if (f.0 as i128).abs() > MAX_TERM || f.1 as i128 > MAX_TERM {
        return Err(format!(
            "{what} {}/{}: numerator and denominator must be at most 2^53 - 1",
            f.0, f.1
        ));
    }
    Ok(J::List(vec![J::Int(f.0), J::Int(f.1)]))
}

/// A JSON value in the order its keys come (serde_json's map is sorted).
pub fn j_of(v: &Value) -> J {
    match v {
        Value::Null => J::Null,
        Value::Bool(b) => J::Bool(*b),
        Value::Number(n) => n
            .as_i64()
            .map(J::Int)
            .unwrap_or_else(|| J::Float(n.as_f64().unwrap_or(f64::NAN))),
        Value::String(s) => J::Str(s.clone()),
        Value::Array(a) => J::List(a.iter().map(j_of).collect()),
        Value::Object(o) => J::Dict(o.iter().map(|(k, x)| (k.clone(), j_of(x))).collect()),
    }
}

fn obj(m: &Map<String, Value>) -> J {
    j_of(&Value::Object(m.clone()))
}

/// A new capture id (wireskein-format §3): 128 bits as 32 lowercase hex digits, from the standard library's
/// randomly keyed hasher (the time and a counter mixed in).
pub fn new_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let k = COUNT.fetch_add(1, Ordering::Relaxed);
    let half = |salt: u64| {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(now);
        h.write_u64(k);
        h.write_u64(salt);
        h.write_u32(std::process::id());
        h.finish()
    };
    format!("{:016x}{:016x}", half(1), half(2))
}

/// Bits packed one a sample (sample k at bit k % 8 of byte k / 8).
fn pack(levels: impl Iterator<Item = u8>, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n.div_ceil(8)];
    for (k, v) in levels.enumerate().take(n) {
        out[k / 8] |= (v & 1) << (k % 8);
    }
    out
}

/// The level of a line at each of n samples at phase + k * step (the changes at or before each counted).
fn sample_levels(l: &Line, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n.div_ceil(8)];
    let mut j = 0usize;
    let mut k = 0usize;
    while k < n {
        let t = l.phase + k as i64 * l.step;
        while j < l.edges.len() && l.edges[j] <= t {
            j += 1;
        }
        let lv = l.initial ^ (j & 1) as u8;
        // a whole byte of samples before the next change: filled at once
        if k.is_multiple_of(8) && k + 8 <= n {
            let last = l.phase + (k as i64 + 7) * l.step;
            if j >= l.edges.len() || l.edges[j] > last {
                out[k / 8] = if lv == 1 { 0xFF } else { 0 };
                k += 8;
                continue;
            }
        }
        out[k / 8] |= lv << (k % 8);
        k += 1;
    }
    out
}

/// Events (quantum, level after, count) as the format keeps them: per event a varint of the gap from the one before,
/// then the level (bit 7) and the count (bits 0-6, 127: at least 127).
fn events_data(
    name: &str,
    quanta: &[i64],
    levels: &[u8],
    counts: &[u8],
    n: i64,
    step: i64,
    start: u8,
) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    let (mut prev, mut level) = (-1i64, start);
    for ((&k, &lv), &c) in quanta.iter().zip(levels).zip(counts) {
        if !(prev < k && k < n) || lv > 1 || c < 1 || (step == 1 && c != 1) {
            return Err(format!(
                "{name}: event (quantum {k}, level {lv}, count {c}) out of order or impossible"
            ));
        }
        let c = c.min(127);
        if c < 127 && lv != level ^ (c & 1) {
            return Err(format!(
                "{name}: quantum {k} ends at {lv} after {c} changes from {level}"
            ));
        }
        let mut gap = (k - prev - 1) as u64;
        loop {
            out.push((gap & 0x7F) as u8 | if gap > 0x7F { 0x80 } else { 0 });
            gap >>= 7;
            if gap == 0 {
                break;
            }
        }
        out.push((lv << 7) | c);
        prev = k;
        level = lv;
    }
    Ok(out)
}

/// One stored channel: its header entry and its data.
struct Entry {
    head: J,
    data: Vec<u8>,
    /// the tick just after it (None: analog, its end from the tick)
    end: i64,
    step: i64,
    logic: bool,
}

fn with_acq(mut e: J, acq: &Map<String, Value>) -> J {
    if !acq.is_empty() {
        e.set("acquisition", obj(acq));
    }
    e
}

/// A line of one tick a sample as its changes when that is smaller than its bits (lossless either way); a slower
/// one, or a reduced one, as its samples (Python's `fileio.to_logic`).
fn logic_entry(l: &Line, n_ticks: i64, prefix: &str) -> Result<Entry, String> {
    if l.step == 1 && l.reduction.is_none() {
        let n = (n_ticks - l.phase).max(0);
        let a = l.edges.partition_point(|&x| x <= l.phase);
        let b = l.edges.partition_point(|&x| x < l.phase + n);
        let ks: Vec<i64> = l.edges[a..b.max(a)].iter().map(|&x| x - l.phase).collect();
        if ((ks.len() * 3) as i64) < n / 8 {
            let start = if n > 0 { l.initial ^ (a & 1) as u8 } else { l.initial };
            let levels: Vec<u8> = (1..=ks.len()).map(|i| start ^ (i & 1) as u8).collect();
            let data = events_data(&l.name, &ks, &levels, &vec![1; ks.len()], n, 1, start)?;
            let head = jd! {"name" => l.name.as_str(), "file" => format!("{prefix}.events"), "encoding" => "events",
            "n" => n, "step" => 1, "phase" => l.phase, "start_level" => start as i64, "events" => ks.len()};
            return Ok(Entry {
                head: with_acq(head, &l.acquisition),
                data,
                end: l.phase + n,
                step: 1,
                logic: true,
            });
        }
    }
    let n = (n_ticks - l.phase + l.step - 1).div_euclid(l.step).max(0);
    let data = sample_levels(l, n as usize);
    let enc = l
        .reduction
        .as_deref()
        .filter(|r| matches!(*r, "any_high" | "any_low"))
        .unwrap_or("bits");
    let head = jd! {"name" => l.name.as_str(), "file" => format!("{prefix}.{enc}"), "encoding" => enc, "n" => n,
    "step" => l.step, "phase" => l.phase};
    Ok(Entry {
        head: with_acq(head, &l.acquisition),
        data,
        end: l.phase + n * l.step,
        step: l.step,
        logic: true,
    })
}

/// A line kept as its changes at a coarser step: `events`, or a `change_latch` one as its intervals (Python's
/// `fileio.to_events`).
fn events_entry(t: &Events, prefix: &str) -> Result<Entry, String> {
    let end = t.phase + t.n * t.step;
    if t.reduction.as_deref() == Some("change_latch") {
        // each interval's level: the last listed at or before it (start_level before any), bit 1 where it changed
        let n = t.n.max(0) as usize;
        let mut v = vec![0u8; n];
        let (mut level, mut j) = (t.start_level, 0usize);
        for (k, x) in v.iter_mut().enumerate() {
            let mut changed = false;
            while j < t.quanta.len() && t.quanta[j] as usize == k {
                level = t.levels[j];
                changed = true;
                j += 1;
            }
            *x = level | if changed { 2 } else { 0 };
        }
        let mut before = t.start_level;
        for (k, &x) in v.iter().enumerate() {
            if (x & 1) != before && x >> 1 == 0 {
                return Err(format!(
                    "{}: interval {k} ends at {} after {before} with its change bit 0",
                    t.name,
                    x & 1
                ));
            }
            before = x & 1;
        }
        let data = pack(v.iter().flat_map(|&x| [x & 1, x >> 1]), 2 * n);
        let head = jd! {"name" => t.name.as_str(), "file" => format!("{prefix}.change_latch"),
        "encoding" => "change_latch", "n" => t.n, "step" => t.step, "phase" => t.phase,
        "start_level" => t.start_level as i64};
        return Ok(Entry {
            head: with_acq(head, &t.acquisition),
            data,
            end,
            step: t.step,
            logic: true,
        });
    }
    let data = events_data(&t.name, &t.quanta, &t.levels, &t.counts, t.n, t.step, t.start_level)?;
    let head = jd! {"name" => t.name.as_str(), "file" => format!("{prefix}.events"), "encoding" => "events",
    "n" => t.n, "step" => t.step, "phase" => t.phase, "start_level" => t.start_level as i64,
    "events" => t.quanta.len()};
    Ok(Entry {
        head: with_acq(head, &t.acquisition),
        data,
        end,
        step: t.step,
        logic: true,
    })
}

fn analog_bytes(a: &Analog) -> Result<Vec<u8>, String> {
    let mut out = vec![];
    match (a.encoding.as_str(), a.width) {
        ("analog-f32", _) => a.values.iter().for_each(|&v| out.extend((v as f32).to_le_bytes())),
        ("analog", 8) => a.values.iter().for_each(|&v| out.push(v as u8)),
        ("analog", 16) => a.values.iter().for_each(|&v| out.extend((v as u16).to_le_bytes())),
        ("analog", 32) => a.values.iter().for_each(|&v| out.extend((v as u32).to_le_bytes())),
        (e, w) => {
            return Err(format!(
                "{}: encoding {e:?} of width {w}: analog, 8, 16 or 32 bits",
                a.name
            ));
        }
    }
    Ok(out)
}

/// The tick just after an analog channel's last sample (rounded up).
fn analog_end(a: &Analog, tick: (i64, i64)) -> i64 {
    // t0 + n * tick / rate = t0n / t0d + n * tick.0 * rate.1 / (tick.1 * rate.0)
    let (t0n, t0d) = (a.t0_ticks.0 as i128, a.t0_ticks.1 as i128);
    let (pn, pd) = (
        a.values.len() as i128 * tick.0 as i128 * a.rate_hz.1 as i128,
        tick.1 as i128 * a.rate_hz.0 as i128,
    );
    ceil_div(t0n * pd + pn * t0d, t0d * pd) as i64
}

fn analog_entry(a: &Analog, tick: (i64, i64), prefix: &str) -> Result<Entry, String> {
    if a.rate_hz.0 <= 0 || a.rate_hz.1 <= 0 {
        return Err(format!("{}: rate_hz must be > 0", a.name));
    }
    let f32 = a.encoding == "analog-f32";
    let mut head = jd! {"name" => a.name.as_str(), "file" => format!("{prefix}.{}", if f32 { "f32" } else { "raw" }),
    "encoding" => a.encoding.as_str(), "n" => a.values.len(), "rate_hz" => terms(a.rate_hz, &format!("{} rate_hz", a.name))?,
    "t0_ticks" => terms(a.t0_ticks, &format!("{} t0_ticks", a.name))?};
    if !f32 {
        head.set("width", a.width);
        if let Some(v) = a.value_bits {
            head.set("value_bits", v);
        }
        if let Some(v) = a.zero {
            head.set("zero", v);
        }
        if let Some(v) = a.scale_nv {
            head.set("scale_nv", v);
        }
    }
    if a.unit != "V" {
        head.set("unit", a.unit.as_str());
    }
    Ok(Entry {
        head: with_acq(head, &a.acquisition),
        data: analog_bytes(a)?,
        end: analog_end(a, tick),
        step: 1,
        logic: false,
    })
}

fn opts(method: CompressionMethod, level: Option<i64>, large: bool) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(method)
        .compression_level(level)
        .last_modified_time(DateTime::default())
        .large_file(large)
}

/// deflate, or stored for data that does not shrink (a noisy or floating line: deflating it takes long and saves
/// nothing).
fn data_opts(data: &[u8]) -> SimpleFileOptions {
    let large = data.len() as u64 >= 1 << 31;
    if data.len() < TRY_BYTES {
        return opts(CompressionMethod::Deflated, Some(1), large);
    }
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(1));
    let _ = e.write_all(&data[..TRY_BYTES]);
    let got = e.finish().map(|v| v.len()).unwrap_or(0);
    if got as f64 > 0.9 * TRY_BYTES as f64 {
        opts(CompressionMethod::Stored, None, large)
    } else {
        opts(CompressionMethod::Deflated, Some(1), large)
    }
}

fn zip_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// A part's channels for the file: its logic lines (but those kept as changes too), its lines kept as changes, its
/// analog channels, in that order.
fn entries(p: &Part, k: usize, tick: (i64, i64)) -> Result<Vec<Entry>, String> {
    let kept: Vec<&str> = p.events.iter().map(|t| t.name.as_str()).collect();
    let mut out = vec![];
    let mut i = 0;
    for l in p.lines.iter().filter(|l| !kept.contains(&l.name.as_str())) {
        out.push(logic_entry(l, p.ticks, &format!("part/{k}/ch/{i}"))?);
        i += 1;
    }
    for t in &p.events {
        out.push(events_entry(t, &format!("part/{k}/ch/{i}"))?);
        i += 1;
    }
    for a in &p.analog {
        out.push(analog_entry(a, tick, &format!("part/{k}/ch/{i}"))?);
        i += 1;
    }
    Ok(out)
}

/// A WireSkein file written (Python's `fileio.save` / `_fileformat.write`): the capture's parts (a timeline's each at
/// its tick0), its id (a new one when it has none), what was lost, its meta (a part's own facts, PART_META, in the
/// part's meta), and entries carried from another file (attachments, notes) as they are.
pub fn write_wireskein(path: &Path, cap: &Capture, extras: &[(String, Vec<u8>)]) -> Result<(), String> {
    let tick = cap.tick_hz;
    if tick.0 <= 0 || tick.1 <= 0 {
        return Err(format!("tick_hz {}/{}: must be > 0", tick.0, tick.1));
    }
    let id = match &cap.id {
        Some(i) => {
            if i.len() != 32 || !i.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)) {
                return Err(format!("capture id {i:?}: 32 lowercase hex digits"));
            }
            i.clone()
        }
        None => new_id(),
    };
    if cap.layout != "timeline" && cap.layout != "segments" || cap.parts.is_empty() {
        return Err(format!(
            "layout {:?}: one of timeline, segments, with one part or more",
            cap.layout
        ));
    }
    let mut files: Vec<(String, Vec<u8>)> = vec![];
    let mut heads = vec![];
    let mut names0: Option<Vec<String>> = None;
    for (k, p) in cap.parts.iter().enumerate() {
        let es = entries(p, k, tick)?;
        let mut names: Vec<String> = es
            .iter()
            .map(|e| e.head.get("name").s().unwrap_or("").to_string())
            .collect();
        let n0 = names.len();
        names.sort();
        names.dedup();
        if names.len() != n0 {
            return Err(format!("part {k}: channel names repeat"));
        }
        if let Some(n) = &names0
            && *n != names
        {
            return Err(format!(
                "part {k}: channels {names:?}, part 0 has {n:?} (every part has the same)"
            ));
        }
        names0 = Some(names);
        let ends: Vec<i64> = es.iter().map(|e| e.end).collect();
        let mut ticks = ends.iter().copied().max().unwrap_or(0);
        // a line at a coarser step may run past the part's end (its last sample or interval cut there)
        if es.iter().any(|e| e.logic && e.end > p.ticks) {
            let off: Vec<&str> = es
                .iter()
                .filter(|e| e.logic && e.end - e.step >= p.ticks && e.head.get("n").i().unwrap_or(0) > 0)
                .map(|e| e.head.get("name").s().unwrap_or(""))
                .collect();
            if !off.is_empty() || p.ticks > ticks {
                return Err(format!("part {k}: {} ticks, but its channels end at {ticks}", p.ticks));
            }
            ticks = p.ticks;
        }
        let tick0 = match (cap.layout.as_str(), p.tick0) {
            ("timeline", None) if k > 0 => return Err(format!("part {k}: a timeline part needs its tick0")),
            ("timeline", None) => Some(0),
            (_, t) => t,
        };
        // the part's own facts: its meta's, the capture's for the first part
        let mut meta = J::dict();
        for key in PART_META {
            if let Some(v) = p.meta.get(key).or(if k == 0 { cap.meta.get(key) } else { None }) {
                meta.set(key, j_of(v));
            }
        }
        let mut h = jd! {"ticks" => ticks};
        if let Some(t) = tick0 {
            h.set("tick0", t);
        }
        h.set("channels", J::List(es.iter().map(|e| e.head.clone()).collect()));
        h.set("meta", meta);
        heads.push(h);
        for e in es {
            files.push((e.head.get("file").s().unwrap_or("").to_string(), e.data));
        }
    }
    let mut lost = vec![];
    for x in &cap.lost {
        let ok = x["after_part"]
            .as_i64()
            .is_some_and(|a| -1 <= a && a < cap.parts.len() as i64)
            && x.get("reason").is_some();
        if !ok {
            return Err(format!(
                "a loss needs after_part (-1 .. {}) and a reason: {x}",
                cap.parts.len() as i64 - 1
            ));
        }
        let mut l = j_of(x);
        if !l.has("stopped") {
            l.set("stopped", false);
        }
        lost.push(l);
    }
    let mut head = jd! {"id" => id, "tick_hz" => terms(tick, "tick_hz")?, "layout" => cap.layout.as_str(),
    "complete" => lost.is_empty()};
    if !lost.is_empty() {
        head.set("lost", J::List(lost));
    }
    head.set("parts", J::List(heads));
    let mut meta = J::dict();
    for (k, v) in &cap.meta {
        if !PART_META.contains(&k.as_str()) {
            meta.set(k, j_of(v));
        }
    }
    head.set("meta", meta);
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut z = ZipWriter::new(std::io::BufWriter::new(f));
    z.start_file(crate::wsk::IDENT, opts(CompressionMethod::Stored, None, false))
        .map_err(zip_err)?;
    z.write_all(br#"{"format": "wireskein/1"}"#).map_err(zip_err)?;
    z.start_file(crate::wsk::CAPTURE, opts(CompressionMethod::Deflated, Some(6), false))
        .map_err(zip_err)?;
    z.write_all(head.dumps_py().as_bytes()).map_err(zip_err)?;
    for (name, data) in &files {
        z.start_file(name.as_str(), data_opts(data)).map_err(zip_err)?;
        z.write_all(data).map_err(zip_err)?;
    }
    for (name, data) in extras {
        z.start_file(
            name.as_str(),
            opts(CompressionMethod::Deflated, Some(6), data.len() as u64 >= 1 << 31),
        )
        .map_err(zip_err)?;
        z.write_all(data).map_err(zip_err)?;
    }
    z.finish().map_err(zip_err)?.flush().map_err(zip_err)?;
    Ok(())
}

/// The one part a .sr or a VCD holds.
fn only_part<'a>(cap: &'a Capture, what: &str) -> Result<&'a Part, String> {
    match cap.parts.as_slice() {
        [p] => Ok(p),
        ps => Err(format!("a {what} holds one part, the capture has {}", ps.len())),
    }
}

/// A capture's meta for the extra block (its id kept apart).
fn extra_meta(cap: &Capture) -> J {
    let mut m = J::dict();
    for (k, v) in &cap.meta {
        if k != "capture_id" {
            m.set(k, j_of(v));
        }
    }
    m
}

/// The lines kept as changes whose quanta hide changes (a .sr or a VCD has no form for them: wireskein-format §4.4).
fn refuse_hidden(p: &Part, what: &str) -> Result<(), String> {
    let hid: Vec<String> = p
        .events
        .iter()
        .filter(|t| t.hidden() > 0)
        .map(|t| {
            format!(
                "{} ({} quanta of {} ticks hold more than one change)",
                t.name,
                t.hidden(),
                t.step
            )
        })
        .collect();
    if hid.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "a {what} has no form for lines kept as changes whose quanta hide changes: {} (keep it as a WireSkein file)",
            hid.join(", ")
        ))
    }
}

/// A sigrok session written (Python's `srio.write_sr`): the logic lines one bit each in the order given (a slow line
/// repeated to the tick rate, `wireskein/sr-extra.json` keeping its real step); an analog channel as volts when its
/// samples land on ticks (repeated to the tick rate, NaN outside them), a raw one's values and settings in the extra
/// and `wireskein/analog-K.raw`, so reading it back restores them; entries carried from another file as they are.
pub fn write_sr(path: &Path, cap: &Capture, extras: &[(String, Vec<u8>)]) -> Result<(), String> {
    let p = only_part(cap, ".sr")?;
    refuse_hidden(p, ".sr")?;
    let lines = &p.lines;
    let n_ch = lines.len();
    let unitsize = match n_ch {
        0..=8 => 1,
        9..=16 => 2,
        17..=32 => 4,
        _ => return Err(format!("{n_ch} channels: a .sr holds up to 32 here")),
    };
    let tick = cap.tick_hz;
    // (index, trace, ticks a sample, first tick, volts)
    let mut held: Vec<(usize, &Analog, i64, i64, Vec<f32>)> = vec![];
    for (j, a) in p.analog.iter().enumerate() {
        // per = tick / rate, t0 whole ticks
        let (pn, pd) = (
            tick.0 as i128 * a.rate_hz.1 as i128,
            tick.1 as i128 * a.rate_hz.0 as i128,
        );
        let t0 = a.t0_ticks;
        if pd == 0 || pn % pd != 0 || t0.0 % t0.1 != 0 || t0.0 < 0 {
            return Err(format!(
                "analog {} does not land on the {} Hz ticks a .sr has",
                a.name,
                crate::pyfmt::g(tick.0 as f64 / tick.1 as f64, 6)
            ));
        }
        let v = a
            .volts()
            .ok_or_else(|| format!("analog {} has no conversion to volts (zero / scale_nv)", a.name))?;
        held.push((
            n_ch + j + 1,
            a,
            (pn / pd) as i64,
            t0.0 / t0.1,
            v.iter().map(|&x| x as f32).collect(),
        ));
    }
    let mut md = vec![
        "[global]".to_string(),
        "sigrok version=0.5.2".into(),
        String::new(),
        "[device 1]".into(),
        "capturefile=logic-1".into(),
        format!("total probes={n_ch}"),
        format!("samplerate={} Hz", round_frac(tick.0 as i128, tick.1 as i128)),
        format!("total analog={}", held.len()),
    ];
    md.extend(
        lines
            .iter()
            .enumerate()
            .map(|(k, l)| format!("probe{}={}", k + 1, l.name)),
    );
    md.extend(held.iter().map(|(idx, a, ..)| format!("analog{idx}={}", a.name)));
    md.push(format!("unitsize={unitsize}"));
    md.push(String::new());
    let mut chans = J::dict();
    for l in lines {
        if l.step != 1 || l.phase != 0 || !l.acquisition.is_empty() {
            let mut c = jd! {"step" => l.step, "phase" => l.phase};
            if !l.acquisition.is_empty() {
                c.set("acquisition", obj(&l.acquisition));
            }
            chans.set(&l.name, c);
        }
    }
    let mut names: Vec<&str> = extras.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    for n in &names {
        if ["version", "metadata", crate::sr::EXTRA].contains(n)
            || ["logic-1", "analog-1-", "wireskein/"].iter().any(|x| n.starts_with(x))
        {
            return Err(format!("the entry {n:?} would clash with the .sr's own"));
        }
    }
    let mut analog = J::dict();
    for (idx, a, ..) in &held {
        let mut info = jd! {"encoding" => a.encoding.as_str(), "n" => a.values.len(),
        "rate_hz" => vec![a.rate_hz.0, a.rate_hz.1], "t0_ticks" => vec![a.t0_ticks.0, a.t0_ticks.1],
        "acquisition" => obj(&a.acquisition)};
        if a.encoding == "analog" {
            info.set("width", a.width);
            info.set("value_bits", a.value_bits);
            info.set("zero", a.zero);
            info.set("scale_nv", a.scale_nv);
            info.set("file", format!("wireskein/analog-{idx}.raw"));
        } else {
            info.set("unit", a.unit.as_str());
        }
        analog.set(&a.name, info);
    }
    let extra = jd! {"format" => "wireskein-sr-extra/1", "id" => cap.id.clone(), "tick_hz" => vec![tick.0, tick.1],
    "channels" => chans, "analog" => analog, "meta" => extra_meta(cap),
    "extras" => J::List(names.iter().map(|&n| J::from(n)).collect())};
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut z = ZipWriter::new(std::io::BufWriter::new(f));
    let put = |z: &mut ZipWriter<_>, name: &str, data: &[u8]| -> Result<(), String> {
        z.start_file(
            name,
            opts(CompressionMethod::Deflated, Some(6), data.len() as u64 >= 1 << 31),
        )
        .map_err(zip_err)?;
        z.write_all(data).map_err(zip_err)
    };
    put(&mut z, "version", b"2")?;
    put(&mut z, "metadata", md.join("\n").as_bytes())?;
    put(&mut z, crate::sr::EXTRA, extra.dumps_py().as_bytes())?;
    for (name, data) in extras {
        put(&mut z, name, data)?;
    }
    for (idx, a, ..) in &held {
        if a.encoding == "analog" {
            put(&mut z, &format!("wireskein/analog-{idx}.raw"), &analog_bytes(a)?)?;
        }
    }
    let n = p.ticks;
    let mut s0 = 0i64;
    let mut part = 1;
    while s0 < n {
        let s1 = (s0 + SR_CHUNK).min(n);
        if n_ch > 0 {
            let mut out = vec![0u32; (s1 - s0) as usize];
            for (k, l) in lines.iter().enumerate() {
                // each stretch at 1 between the line's changes set at once
                let mut j = l.edges.partition_point(|&x| x <= s0);
                let mut lv = l.initial ^ (j & 1) as u8;
                let mut t = s0;
                while t < s1 {
                    let next = l.edges.get(j).copied().unwrap_or(s1).clamp(t, s1);
                    if lv == 1 {
                        out[(t - s0) as usize..(next - s0) as usize]
                            .iter_mut()
                            .for_each(|x| *x |= 1 << k);
                    }
                    if next >= s1 {
                        break;
                    }
                    t = next;
                    j += 1;
                    lv ^= 1;
                }
            }
            let mut bytes = Vec::with_capacity(out.len() * unitsize);
            for x in &out {
                bytes.extend_from_slice(&x.to_le_bytes()[..unitsize]);
            }
            put(&mut z, &format!("logic-1-{part}"), &bytes)?;
        }
        for (idx, _, per, t0, v) in &held {
            let mut seg = Vec::with_capacity(4 * (s1 - s0) as usize);
            for t in s0..s1 {
                let k = (t - t0).div_euclid(*per);
                let x = if k >= 0 && (k as usize) < v.len() {
                    v[k as usize]
                } else {
                    f32::NAN
                };
                seg.extend(x.to_le_bytes());
            }
            put(&mut z, &format!("analog-1-{idx}-{part}"), &seg)?;
        }
        s0 = s1;
        part += 1;
    }
    z.finish().map_err(zip_err)?.flush().map_err(zip_err)?;
    Ok(())
}

/// The largest VCD unit (1/10/100 x s..fs) the tick period is a whole multiple of: (its name, femtoseconds).
fn timescale(tick: (i64, i64)) -> (String, i128) {
    // period = tick.1 / tick.0 s = tick.1 * 1e15 / tick.0 fs
    let (pn, pd) = (tick.1 as i128 * 1_000_000_000_000_000, tick.0 as i128);
    for (name, fs) in [
        ("s", 1_000_000_000_000_000i128),
        ("ms", 1_000_000_000_000),
        ("us", 1_000_000_000),
        ("ns", 1_000_000),
        ("ps", 1_000),
        ("fs", 1),
    ] {
        for m in [100i128, 10, 1] {
            if pn % (pd * fs * m) == 0 {
                return (format!("{m} {name}"), fs * m);
            }
        }
    }
    ("1 fs".into(), 1) // not exact: times are rounded to fs
}

/// VCD identifiers: printable ASCII 33..126, then two characters, ...
fn vcd_id(mut n: usize) -> String {
    let mut s = vec![];
    loop {
        s.push((33 + n % 94) as u8 as char);
        if n / 94 == 0 {
            break;
        }
        n = n / 94 - 1;
    }
    s.iter().rev().collect()
}

/// A VCD written (Python's `vcdio.write_vcd`): the changes, each line at its own rate; what VCD cannot say (the exact
/// tick, each line's step and phase, the meta, an analog channel's rate and start) in a `$comment wireskein {...}`
/// block that other tools pass over. Attachments and notes have no place in it.
pub fn write_vcd(path: &Path, cap: &Capture) -> Result<(), String> {
    let p = only_part(cap, "VCD")?;
    refuse_hidden(p, "VCD")?;
    let tick = cap.tick_hz;
    let (scale, unit_fs) = timescale(tick);
    // timescale units a tick = tick.1 * 1e15 / (tick.0 * unit_fs), a fraction
    let (un, ud) = (tick.1 as i128 * 1_000_000_000_000_000, tick.0 as i128 * unit_fs);
    let g = gcd(un, ud).max(1);
    let (un, ud) = (un / g, ud / g);
    let mut k = 0usize;
    let mut next_id = || {
        k += 1;
        vcd_id(k - 1)
    };
    let logic: Vec<(String, &Line)> = p.lines.iter().map(|l| (next_id(), l)).collect();
    let analog: Vec<(String, &Analog, Vec<f64>, bool)> = p
        .analog
        .iter()
        .map(|a| {
            let v = a.volts();
            let volts = v.is_some();
            (next_id(), a, v.unwrap_or_else(|| a.values.clone()), volts)
        })
        .collect();
    let mut chans = J::dict();
    for (_, l) in &logic {
        let mut c = jd! {"step" => l.step, "phase" => l.phase};
        if !l.acquisition.is_empty() {
            c.set("acquisition", obj(&l.acquisition));
        }
        chans.set(&l.name, c);
    }
    let mut an = J::dict();
    for (_, a, _, volts) in &analog {
        an.set(
            &a.name,
            jd! {"rate_hz" => vec![a.rate_hz.0, a.rate_hz.1], "t0_ticks" => vec![a.t0_ticks.0, a.t0_ticks.1],
            "unit" => if *volts { a.unit.as_str() } else { "raw" }, "acquisition" => obj(&a.acquisition)},
        );
    }
    let extra = jd! {"format" => "wireskein-vcd-extra/1", "id" => cap.id.clone(), "tick_hz" => vec![tick.0, tick.1],
    "ticks" => p.ticks, "channels" => chans, "analog" => an, "meta" => extra_meta(cap)};
    // every change as (time in timescale units, text), the lines' first, then the analog channels'
    let mut ch: Vec<(i128, String)> = vec![];
    for (vid, l) in &logic {
        for (i, &e) in l.edges.iter().enumerate() {
            let lv = l.initial ^ ((i + 1) & 1) as u8;
            ch.push((round_frac(e as i128 * un, ud), format!("{lv}{vid}")));
        }
    }
    let per_tick = un as f64 / ud as f64;
    for (vid, a, vals, _) in &analog {
        let step = (tick.0 as f64 / tick.1 as f64) / (a.rate_hz.0 as f64 / a.rate_hz.1 as f64);
        let t0 = a.t0_ticks.0 as f64 / a.t0_ticks.1 as f64;
        for (i, &x) in vals.iter().enumerate() {
            let t = ((t0 + i as f64 * step) * per_tick).round_ties_even() as i128;
            if t >= 0 {
                ch.push((t, format!("r{} {vid}", crate::pyfmt::g(x, 9))));
            }
        }
    }
    ch.sort_by_key(|x| x.0); // stable
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut w = std::io::BufWriter::new(f);
    let mut out = String::new();
    out.push_str("$version wireskein $end\n");
    out.push_str(&format!("$comment wireskein {} $end\n", extra.dumps_py()));
    out.push_str(&format!("$timescale {scale} $end\n$scope module capture $end\n"));
    for (vid, l) in &logic {
        out.push_str(&format!("$var wire 1 {vid} {} $end\n", l.name));
    }
    for (vid, a, ..) in &analog {
        out.push_str(&format!("$var real 64 {vid} {} $end\n", a.name));
    }
    out.push_str("$upscope $end\n$enddefinitions $end\n#0\n$dumpvars\n");
    for (vid, l) in &logic {
        out.push_str(&format!("{}{vid}\n", l.initial));
    }
    out.push_str("$end\n");
    w.write_all(out.as_bytes()).map_err(zip_err)?;
    let mut last = 0i128;
    for (t, x) in &ch {
        if *t != last {
            writeln!(w, "#{t}").map_err(zip_err)?;
            last = *t;
        }
        writeln!(w, "{x}").map_err(zip_err)?;
    }
    let end = round_frac(p.ticks as i128 * un, ud);
    if end > last {
        writeln!(w, "#{end}").map_err(zip_err)?;
    }
    w.flush().map_err(zip_err)?;
    Ok(())
}

/// A capture written by the name: `.sr` a sigrok session, `.vcd` a VCD, anything else a WireSkein file (Python's
/// `decode._save`).
pub fn write(path: &Path, cap: &Capture, extras: &[(String, Vec<u8>)]) -> Result<(), String> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("sr") => write_sr(path, cap, extras),
        Some("vcd") => write_vcd(path, cap),
        _ => write_wireskein(path, cap, extras),
    }
}
