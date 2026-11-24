//! A WireSkein file (docs/wireskein-format.md): `wireskein.json`, `capture.json` (checked as §3.4 asks), each part's
//! channels decoded to logic lines, the parts of a timeline that follow on without a gap joined.

use crate::{Analog, Capture, Events, Kind, Line, Part, dense_edges, fraction};
use serde_json::{Map, Value, json};
use std::io::{Cursor, Read, Seek};

pub const IDENT: &str = "wireskein.json";
pub const CAPTURE: &str = "capture.json";
pub const FORMAT: &str = "wireskein/1";

fn archive(bytes: &[u8]) -> Result<zip::ZipArchive<Cursor<&[u8]>>, String> {
    zip::ZipArchive::new(Cursor::new(bytes)).map_err(|e| format!("not a zip: {e}"))
}

/// The names of a zip's entries.
pub fn names(bytes: &[u8]) -> Result<Vec<String>, String> {
    let z = archive(bytes)?;
    Ok(z.file_names().map(String::from).collect())
}

/// One entry of a zip, read whole.
pub fn entry(bytes: &[u8], name: &str) -> Result<Vec<u8>, String> {
    let mut z = archive(bytes)?;
    read_entry(&mut z, name)
}

fn read_entry<R: Read + Seek>(z: &mut zip::ZipArchive<R>, name: &str) -> Result<Vec<u8>, String> {
    let mut f = z.by_name(name).map_err(|e| format!("{name}: {e}"))?;
    let mut out = Vec::with_capacity(f.size() as usize);
    f.read_to_end(&mut out).map_err(|e| format!("{name}: {e}"))?;
    Ok(out)
}

fn int(v: &Value, k: &str) -> Result<i64, String> {
    v.get(k)
        .and_then(|x| x.as_i64())
        .ok_or_else(|| format!("a channel without its {k}"))
}

/// The header's checks (§3, §3.4): complete a boolean agreeing with lost, a known layout, parts, each timeline
/// part's tick0.
fn check_head(head: &Value) -> Result<(), String> {
    let Some(complete) = head.get("complete").and_then(|v| v.as_bool()) else {
        return Err("capture.json has no complete (true / false): not a file to read as complete".into());
    };
    let lost = head.get("lost").and_then(|v| v.as_array()).map_or(0, |a| a.len());
    if complete == (lost > 0) {
        return Err(format!("complete is {complete} but lost has {lost} entries"));
    }
    let layout = head.get("layout").and_then(|v| v.as_str()).unwrap_or("");
    if layout != "timeline" && layout != "segments" {
        return Err(format!("layout {layout:?}: one of timeline, segments"));
    }
    let Some(parts) = head.get("parts").and_then(|v| v.as_array()).filter(|p| !p.is_empty()) else {
        return Err("capture.json has no parts".into());
    };
    if layout == "timeline" && parts.iter().any(|p| p.get("tick0").is_none()) {
        return Err("a timeline part without its tick0".into());
    }
    Ok(())
}

/// A channel decoded: a line (of its step), or skipped (with why); with the changes it was kept as at a coarser step.
enum Decoded {
    Line(Line, Option<Events>),
    Skip(Value, Option<Events>),
}

/// The bits of `data`, `width` bits a value, n values.
fn values(data: &[u8], n: usize, width: usize) -> Vec<u8> {
    (0..n)
        .map(|k| {
            (0..width).fold(0u8, |a, j| {
                let b = k * width + j;
                a | (((data.get(b / 8).copied().unwrap_or(0) >> (b % 8)) & 1) << j)
            })
        })
        .collect()
}

/// An events channel's (quanta, levels after, counts).
/// (quanta, levels after, counts) of a line's events.
type Changes = (Vec<i64>, Vec<u8>, Vec<u8>);

fn events(data: &[u8], count: usize) -> Result<Changes, String> {
    let (mut ks, mut lv, mut cn) = (vec![], vec![], vec![]);
    let (mut k, mut i) = (-1i64, 0usize);
    while i < data.len() {
        let (mut gap, mut shift) = (0i64, 0);
        loop {
            let b = *data.get(i).ok_or("an event cut short")?;
            i += 1;
            gap |= ((b & 0x7F) as i64) << shift;
            shift += 7;
            if b < 0x80 {
                break;
            }
        }
        k += gap + 1;
        let b = *data.get(i).ok_or("an event cut short")?;
        i += 1;
        ks.push(k);
        lv.push(b >> 7);
        cn.push(b & 0x7F);
    }
    if ks.len() != count {
        return Err(format!("{} events in the data, {count} in the header", ks.len()));
    }
    Ok((ks, lv, cn))
}

/// Events at a step of 2 or more as a line of that step, where no quantum holds more than one change: each change
/// at its quantum's last tick (cut at the part's end); kept as the events too.
fn coarse(ev: Events, ticks: i64, enc: &str) -> Decoded {
    if ev.hidden() > 0 {
        let hidden = ev.hidden();
        return Decoded::Skip(json!({"name": ev.name, "encoding": enc, "hidden": hidden}), Some(ev));
    }
    let (step, phase) = (ev.step, ev.phase);
    let edges = ev
        .quanta
        .iter()
        .map(|&k| (phase + k * step + (step - 1)).min((ticks - 1).max(0)))
        .collect();
    Decoded::Line(
        Line {
            name: ev.name.clone(),
            initial: ev.start_level,
            edges,
            step,
            phase: phase + step - 1,
            reduction: ev.reduction.clone(),
        },
        Some(ev),
    )
}

fn decode(c: &Value, data: &[u8], ticks: i64) -> Result<Decoded, String> {
    let name = c["name"].as_str().unwrap_or("").to_string();
    let enc = c["encoding"].as_str().unwrap_or("");
    let line = |initial, edges, step, phase, reduction: Option<&str>| {
        Decoded::Line(
            Line {
                name: name.clone(),
                initial,
                edges,
                step,
                phase,
                reduction: reduction.map(String::from),
            },
            None,
        )
    };
    Ok(match enc {
        "bits" => {
            let (n, step, phase) = (int(c, "n")? as usize, int(c, "step")?, int(c, "phase")?);
            let (init, e) = dense_edges(values(data, n, 1), step, phase);
            line(init, e, step, phase, None)
        }
        "any_high" | "any_low" => {
            let (n, step, phase) = (int(c, "n")? as usize, int(c, "step")?, int(c, "phase")?);
            let (init, e) = dense_edges(values(data, n, 1), step, phase);
            line(init, e, step, phase, Some(enc))
        }
        "events" => {
            let (step, phase) = (int(c, "step")?, int(c, "phase")?);
            let start = int(c, "start_level")? as u8;
            let (ks, levels, counts) = events(data, int(c, "events")? as usize)?;
            if step == 1 {
                // lossless: each event one change at its tick (one at the first tick: the line starts after it)
                let (mut init, mut ks) = (start, ks.as_slice());
                if ks.first() == Some(&0) {
                    init ^= 1;
                    ks = &ks[1..];
                }
                line(init, ks.iter().map(|&k| phase + k).collect(), 1, phase, None)
            } else {
                let n = c
                    .get("n")
                    .and_then(|v| v.as_i64())
                    .unwrap_or((ticks - phase + step - 1).div_euclid(step));
                coarse(
                    Events {
                        name: name.clone(),
                        step,
                        phase,
                        n,
                        start_level: start,
                        quanta: ks,
                        levels,
                        counts,
                        reduction: None,
                    },
                    ticks,
                    enc,
                )
            }
        }
        "change_latch" => {
            let (n, step, phase) = (int(c, "n")? as usize, int(c, "step")?, int(c, "phase")?);
            let start = c.get("start_level").and_then(|v| v.as_i64()).unwrap_or(0) as u8;
            let v = values(data, n, 2);
            let (mut ks, mut levels, mut counts) = (vec![], vec![], vec![]);
            let mut before = start;
            for (k, &x) in v.iter().enumerate() {
                let (lv, ch) = (x & 1, x >> 1);
                if lv != before && ch == 0 {
                    return Err(format!(
                        "{name}: interval {k} ends at {lv} after {before} with its change bit 0"
                    ));
                }
                if ch == 1 {
                    ks.push(k as i64);
                    levels.push(lv);
                    counts.push(if lv != before { 1 } else { 2 });
                }
                before = lv;
            }
            coarse(
                Events {
                    name: name.clone(),
                    step,
                    phase,
                    n: n as i64,
                    start_level: start,
                    quanta: ks,
                    levels,
                    counts,
                    reduction: Some("change_latch".into()),
                },
                ticks,
                enc,
            )
        }
        _ => Decoded::Skip(json!({"name": name, "encoding": enc}), None),
    })
}

fn terms(v: &Value, what: &str) -> Result<(i64, i64), String> {
    match v.as_array() {
        Some(a) if a.len() == 2 => Ok(fraction(
            a[0].as_i64().ok_or(format!("{what}: not an integer"))?,
            a[1].as_i64().ok_or(format!("{what}: not an integer"))?,
        )),
        _ => v.as_i64().map(|n| (n, 1)).ok_or(format!("{what}: not [num, den]")),
    }
}

/// An analog channel's samples (raw integers of its width, little-endian, or float32 volts).
fn read_analog(c: &Value, data: &[u8]) -> Result<Analog, String> {
    let name = c["name"].as_str().unwrap_or("").to_string();
    let enc = c["encoding"].as_str().unwrap_or("").to_string();
    let n = int(c, "n")? as usize;
    let width = c.get("width").and_then(|v| v.as_i64()).unwrap_or(16);
    let size = if enc == "analog-f32" { 4 } else { (width / 8) as usize };
    if size == 0 || ![1, 2, 4].contains(&size) {
        return Err(format!("{name}: an analog width of {width} bits"));
    }
    let values = data
        .chunks_exact(size)
        .take(n)
        .map(|b| match (enc.as_str(), size) {
            ("analog-f32", _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            (_, 1) => b[0] as f64,
            (_, 2) => u16::from_le_bytes([b[0], b[1]]) as f64,
            _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
        })
        .collect();
    Ok(Analog {
        name: name.clone(),
        encoding: enc,
        rate_hz: terms(&c["rate_hz"], &format!("{name} rate_hz"))?,
        t0_ticks: terms(&c["t0_ticks"], &format!("{name} t0_ticks")).unwrap_or((0, 1)),
        width,
        value_bits: c.get("value_bits").and_then(|v| v.as_i64()),
        zero: c.get("zero").and_then(|v| v.as_f64()),
        scale_nv: c.get("scale_nv").and_then(|v| v.as_f64()),
        unit: c.get("unit").and_then(|v| v.as_str()).unwrap_or("V").to_string(),
        values,
    })
}

/// A WireSkein file read: its parts, the runs of a timeline joined.
pub fn read(bytes: &[u8]) -> Result<Capture, String> {
    read_from(Cursor::new(bytes))
}

/// A WireSkein file read from a reader (a file on disk: its entries read as they are needed).
pub fn read_from<R: Read + Seek>(r: R) -> Result<Capture, String> {
    let mut z = zip::ZipArchive::new(r).map_err(|e| format!("not a zip: {e}"))?;
    let id: Value = serde_json::from_slice(&read_entry(&mut z, IDENT)?).map_err(|e| e.to_string())?;
    let fmt = id["format"].as_str().unwrap_or("");
    if fmt != FORMAT {
        return Err(format!("format {fmt:?}; this reader reads {FORMAT:?} only"));
    }
    let head: Value = serde_json::from_slice(&read_entry(&mut z, CAPTURE).map_err(|_| "holds no capture")?)
        .map_err(|e| e.to_string())?;
    check_head(&head)?;
    let th = head["tick_hz"].as_array().ok_or("no tick_hz")?;
    let tick_hz = fraction(
        th.first().and_then(|v| v.as_i64()).ok_or("tick_hz")?,
        th.get(1).and_then(|v| v.as_i64()).ok_or("tick_hz")?,
    );
    let cmeta = head
        .get("meta")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let raw_parts = head["parts"].as_array().unwrap();
    let mut parts: Vec<(Part, bool, bool)> = vec![]; // (part, holds coarse events, holds analog or hidden lines)
    for (k, p) in raw_parts.iter().enumerate() {
        let ticks = p["ticks"].as_i64().ok_or("a part without its ticks")?;
        let mut meta = cmeta.clone();
        if let Some(m) = p.get("meta").and_then(|v| v.as_object()) {
            meta.extend(m.clone());
        }
        let (mut lines, mut evs, mut skipped, mut coarse_ev, mut other) = (vec![], vec![], vec![], false, false);
        let mut analog = vec![];
        for c in p["channels"].as_array().ok_or("a part without channels")? {
            let enc = c["encoding"].as_str().unwrap_or("");
            if enc == "analog" || enc == "analog-f32" {
                other = true;
                skipped.push(json!({"name": c["name"], "encoding": enc, "analog": true}));
                analog.push(read_analog(c, &read_entry(&mut z, c["file"].as_str().unwrap_or(""))?)?);
                continue;
            }
            let known = ["bits", "events", "any_high", "any_low", "change_latch"].contains(&enc);
            let data = if known {
                read_entry(&mut z, c["file"].as_str().unwrap_or(""))?
            } else {
                vec![]
            };
            match decode(c, &data, ticks)? {
                Decoded::Line(l, ev) => {
                    if let Some(ev) = ev {
                        coarse_ev = true;
                        evs.push(ev);
                    }
                    lines.push(l);
                }
                Decoded::Skip(s, ev) => {
                    if let Some(ev) = ev {
                        coarse_ev = true;
                        other = true;
                        evs.push(ev);
                    }
                    skipped.push(s);
                }
            }
        }
        parts.push((
            Part {
                part: k,
                parts: None,
                ticks,
                tick0: p.get("tick0").and_then(|v| v.as_i64()),
                meta,
                lines,
                events: evs,
                analog,
                skipped,
            },
            coarse_ev,
            other,
        ));
    }
    let layout = head["layout"].as_str().unwrap_or("timeline").to_string();
    let n_parts = parts.len();
    let joined = if layout == "timeline" && n_parts > 1 {
        runs(parts)
    } else {
        parts.into_iter().map(|p| p.0).collect()
    };
    Ok(Capture {
        kind: Kind::WireSkein,
        tick_hz,
        id: head.get("id").and_then(|v| v.as_str()).map(String::from),
        complete: head["complete"].as_bool().unwrap_or(false),
        lost: head.get("lost").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
        layout,
        n_parts,
        meta: cmeta,
        parts: joined,
    })
}

/// The runs of parts that follow on without a gap (or with the last quantum of a part's events running past the next
/// part's start by less than a quantum) joined, where they hold the same lines at the same steps and none hides
/// changes or holds analog (`fileio.load_runs`).
fn runs(parts: Vec<(Part, bool, bool)>) -> Vec<Part> {
    let mut out = vec![];
    let mut run: Vec<(Part, bool, bool)> = vec![];
    for p in parts {
        if let Some(prev) = run.last() {
            let t0 = |x: &Part| x.tick0.unwrap_or(0);
            let over = t0(&prev.0) + prev.0.ticks - t0(&p.0);
            let max_step = prev.0.lines.iter().map(|l| l.step).max().unwrap_or(1).max(1);
            if !(0 <= over && over < max_step && (over == 0 || prev.1)) {
                out.extend(join(std::mem::take(&mut run)));
            }
        }
        run.push(p);
    }
    out.extend(join(run));
    out
}

fn join(run: Vec<(Part, bool, bool)>) -> Vec<Part> {
    let first = &run[0].0;
    let names: Vec<&str> = first.lines.iter().map(|l| l.name.as_str()).collect();
    let steps: Vec<i64> = first.lines.iter().map(|l| l.step).collect();
    let same = |p: &Part| {
        p.lines.iter().map(|l| l.name.as_str()).collect::<Vec<_>>() == names
            && p.lines.iter().map(|l| l.step).collect::<Vec<_>>() == steps
    };
    if run.len() == 1 || run.iter().any(|(p, _, other)| *other || !same(p)) {
        return run.into_iter().map(|p| p.0).collect();
    }
    let t00 = first.tick0.unwrap_or(0);
    let mut lines = vec![];
    for (j, l0) in first.lines.iter().enumerate() {
        let mut edges = l0.edges.clone();
        for (p, _, _) in &run[1..] {
            let c = &p.lines[j];
            let off = p.tick0.unwrap_or(0) - t00;
            edges.retain(|&e| e < off);
            let level = l0.initial ^ (edges.len() & 1) as u8;
            if c.initial != level {
                edges.push(off);
            }
            edges.extend(c.edges.iter().map(|&e| e + off));
        }
        lines.push(Line {
            name: l0.name.clone(),
            initial: l0.initial,
            edges,
            step: l0.step,
            phase: l0.phase,
            reduction: l0.reduction.clone(),
        });
    }
    let last = &run[run.len() - 1].0;
    let mut meta = first.meta.clone();
    meta.insert(
        "part_starts".into(),
        json!(
            run.iter()
                .map(|(p, _, _)| [p.part as i64, p.tick0.unwrap_or(0) - t00])
                .collect::<Vec<_>>()
        ),
    );
    vec![Part {
        part: first.part,
        parts: Some((first.part, last.part)),
        ticks: last.tick0.unwrap_or(0) - t00 + last.ticks,
        tick0: first.tick0,
        meta,
        lines,
        events: vec![],
        analog: vec![],
        skipped: first.skipped.clone(),
    }]
}

/// A WireSkein file's header alone (its parts without their lines), and whether each part can be read a block at a
/// time (`stream::file_source`): parts that do not follow on, of lines all `bits` / `events` at one tick a sample.
pub fn head_from<R: Read + Seek>(r: R) -> Result<(Capture, bool), String> {
    let mut z = zip::ZipArchive::new(r).map_err(|e| format!("not a zip: {e}"))?;
    let id: Value = serde_json::from_slice(&read_entry(&mut z, IDENT)?).map_err(|e| e.to_string())?;
    if id["format"].as_str() != Some(FORMAT) {
        return Err(format!("format {:?}; this reader reads {FORMAT:?} only", id["format"]));
    }
    let head: Value = serde_json::from_slice(&read_entry(&mut z, CAPTURE).map_err(|_| "holds no capture")?)
        .map_err(|e| e.to_string())?;
    check_head(&head)?;
    let th = head["tick_hz"].as_array().ok_or("no tick_hz")?;
    let tick_hz = fraction(
        th.first().and_then(|v| v.as_i64()).ok_or("tick_hz")?,
        th.get(1).and_then(|v| v.as_i64()).ok_or("tick_hz")?,
    );
    let cmeta = head
        .get("meta")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let raw = head["parts"].as_array().unwrap();
    let layout = head["layout"].as_str().unwrap_or("timeline").to_string();
    let mut streamable = raw.iter().all(|p| {
        p["channels"].as_array().is_some_and(|cs| {
            !cs.is_empty()
                && cs.iter().all(|c| {
                    matches!(c["encoding"].as_str(), Some("bits" | "events"))
                        && c["step"].as_i64() == Some(1)
                        && c["phase"].as_i64().unwrap_or(0) == 0
                })
        })
    });
    if layout == "timeline" {
        let t = |p: &Value, k: &str| p.get(k).and_then(|v| v.as_i64()).unwrap_or(0);
        streamable &= raw
            .windows(2)
            .all(|w| t(&w[0], "tick0") + t(&w[0], "ticks") < t(&w[1], "tick0"));
    }
    let parts = raw
        .iter()
        .enumerate()
        .map(|(k, p)| {
            let mut meta = cmeta.clone();
            if let Some(m) = p.get("meta").and_then(|v| v.as_object()) {
                meta.extend(m.clone());
            }
            Part {
                part: k,
                parts: None,
                ticks: p["ticks"].as_i64().unwrap_or(0),
                tick0: p.get("tick0").and_then(|v| v.as_i64()),
                meta,
                lines: vec![],
                skipped: vec![],
            }
        })
        .collect();
    Ok((
        Capture {
            kind: Kind::WireSkein,
            tick_hz,
            id: head.get("id").and_then(|v| v.as_str()).map(String::from),
            complete: head["complete"].as_bool().unwrap_or(false),
            lost: head.get("lost").and_then(|v| v.as_array()).cloned().unwrap_or_default(),
            layout,
            n_parts: raw.len(),
            meta: cmeta,
            parts,
        },
        streamable,
    ))
}

/// A line kept as `events` at one tick a sample, decoded (each event one change at its tick).
pub fn read_events_line(c: &Value, data: &[u8]) -> Result<Line, String> {
    match decode(c, data, i64::MAX)? {
        Decoded::Line(l, _) => Ok(l),
        Decoded::Skip(_) => Err(format!("{}: not a line of changes", c["name"])),
    }
}

/// The parts' meta keys copied into an analysis document's part (§2.1).
pub fn part_keys(meta: &Map<String, Value>) -> Map<String, Value> {
    ["start_ns", "start_uncertainty_ns", "boot_id"]
        .iter()
        .filter_map(|k| meta.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect()
}
