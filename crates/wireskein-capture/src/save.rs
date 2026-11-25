//! A capture written out: as WireSkein's capture (wireskein-io, for its writers: `to_capture`, `save`) and as one
//! JSON document with its lines' data (for Python: `to_doc`).

use crate::error::{Error, Result};
use crate::packed;
use crate::parts::{self as pt, Acquired, Chan, Data, Loss, Part};
use crate::ratio;
use crate::spool::{Format, SegmentInfo, Spool};
use serde_json::{Map, Value, json};
use std::path::Path;

/// Python's `format(x, "g")`.
pub fn fmt_g(x: f64) -> String {
    wireskein_io::pyfmt::g(x, 6)
}

/// A part's length: the probe's (its samples) when a line runs at a coarser step, else its channels' latest end
/// (an analog line's at `tick`).
fn part_ticks(p: &Part, tick: ratio::Ratio) -> i64 {
    let end = p
        .channels
        .iter()
        .map(|c| match c {
            Chan::Analog(a) => a.end_at(tick),
            c => c.end() as i64,
        })
        .max()
        .unwrap_or(0);
    if p.channels.iter().any(|c| !c.is_analog() && c.step() > 1) {
        if let Some(s) = p.samples {
            return s as i64;
        }
        let whole = p
            .channels
            .iter()
            .filter(|c| matches!(c, Chan::Bits { step: 1, .. }))
            .map(|c| c.end() as i64)
            .max();
        return whole.unwrap_or(end);
    }
    end
}

/// A capture as WireSkein's (wireskein-io, for its writers): each part's lines as their changes, a change_latch line
/// as its events; `extra_meta` (source, requested_rate, ...) before the capture's own.
pub fn to_capture(a: &Acquired, extra_meta: &Map<String, Value>) -> Result<wireskein_io::Capture> {
    let mut meta = extra_meta.clone();
    for (k, v) in &a.meta {
        meta.insert(k.clone(), v.clone());
    }
    let mut parts = vec![];
    let tick = ratio::Ratio::from(a.tick_hz);
    for (k, p) in a.all_parts().iter().enumerate() {
        let mut lines = vec![];
        let mut events = vec![];
        let mut analog = vec![];
        for c in &p.channels {
            let acq: Map<String, Value> = match c {
                Chan::Bits { pin, .. } | Chan::Reduced { pin, .. } => {
                    [("pin".to_string(), json!(pin))].into_iter().collect()
                }
                Chan::Analog(x) => x.acquisition.clone(),
            };
            match c {
                Chan::Analog(x) => {
                    let w = (x.width / 8).max(1) as usize;
                    let values = x
                        .data
                        .chunks_exact(w)
                        .map(|b| {
                            let mut v = 0u64;
                            for (j, byte) in b.iter().enumerate() {
                                v |= (*byte as u64) << (8 * j);
                            }
                            v as f64
                        })
                        .collect();
                    analog.push(wireskein_io::Analog {
                        name: x.name.clone(),
                        encoding: "analog".into(),
                        rate_hz: x.rate.pair(),
                        t0_ticks: x.t0.pair(),
                        width: x.width as i64,
                        value_bits: x.value_bits.map(|b| b as i64),
                        zero: x.zero.map(|z| z as f64),
                        scale_nv: x.scale_nv.map(|s| s as f64),
                        unit: "V".into(),
                        values,
                        acquisition: acq,
                    });
                }
                Chan::Bits {
                    name, data, n, step, ..
                } => {
                    let bits = data.bytes()?;
                    let (initial, edges) = wireskein_io::packed_edges(&bits, *n as usize, *step as i64, 0);
                    lines.push(wireskein_io::Line {
                        name: name.clone(),
                        initial,
                        edges,
                        step: *step as i64,
                        phase: 0,
                        reduction: None,
                        acquisition: acq,
                    });
                }
                Chan::Reduced {
                    name,
                    kind,
                    data,
                    n,
                    step,
                    start_level,
                    ..
                } if *kind == pt::CHANGE_LATCH => {
                    let (mut ks, mut levels, mut counts) = (vec![], vec![], vec![]);
                    let mut before = *start_level;
                    for j in 0..*n as usize {
                        let (lv, ch) = (packed::bit(data, 2 * j), packed::bit(data, 2 * j + 1));
                        if ch == 1 {
                            ks.push(j as i64);
                            levels.push(lv);
                            counts.push(if lv != before { 1 } else { 2 });
                        } else if lv != before {
                            return Err(Error::Protocol(format!(
                                "{name}: interval {j} ends at {lv} after {before} with its change bit 0"
                            )));
                        }
                        before = lv;
                    }
                    events.push(wireskein_io::Events {
                        name: name.clone(),
                        step: *step as i64,
                        phase: 0,
                        n: *n as i64,
                        start_level: *start_level,
                        quanta: ks,
                        levels,
                        counts,
                        reduction: Some("change_latch".into()),
                        acquisition: acq,
                    });
                }
                Chan::Reduced {
                    name,
                    kind,
                    data,
                    n,
                    step,
                    ..
                } => {
                    let (initial, edges) = wireskein_io::packed_edges(data, *n as usize, *step as i64, 0);
                    lines.push(wireskein_io::Line {
                        name: name.clone(),
                        initial,
                        edges,
                        step: *step as i64,
                        phase: 0,
                        reduction: Some(pt::carriage_name(*kind).into()),
                        acquisition: acq,
                    });
                }
            }
        }
        parts.push(wireskein_io::Part {
            part: k,
            parts: None,
            ticks: part_ticks(p, tick),
            tick0: p.tick0,
            meta: p.meta.clone(),
            lines,
            events,
            analog,
            skipped: vec![],
        });
    }
    let lost: Vec<Value> = a.lost.iter().map(Loss::to_json).collect();
    Ok(wireskein_io::Capture {
        kind: wireskein_io::Kind::WireSkein,
        tick_hz: wireskein_io::fraction(a.tick_hz.0 as i64, a.tick_hz.1 as i64),
        id: None,
        complete: lost.is_empty(),
        lost,
        layout: a.layout.clone(),
        n_parts: parts.len(),
        meta,
        parts,
    })
}

/// A capture as one JSON document and its channels' data (for Python: the doc's channels name their data by index
/// into the list, or by a file's path).
pub fn to_doc(a: &Acquired) -> (Value, Vec<Vec<u8>>) {
    let mut datas = vec![];
    let mut parts = vec![];
    for p in a.all_parts() {
        let chans: Vec<Value> = p
            .channels
            .iter()
            .map(|c| match c {
                Chan::Bits {
                    name,
                    data,
                    n,
                    step,
                    pin,
                } => {
                    let (index, path) = match data {
                        Data::Mem(b) => {
                            datas.push(b.clone());
                            (json!(datas.len() - 1), Value::Null)
                        }
                        Data::File(p) => (Value::Null, json!(p.to_string_lossy())),
                    };
                    json!({"kind": "bits", "name": name, "n": n, "step": step, "pin": pin, "data": index,
                           "path": path})
                }
                Chan::Reduced {
                    name,
                    kind,
                    data,
                    n,
                    step,
                    start_level,
                    pin,
                } => {
                    datas.push(data.clone());
                    json!({"kind": "reduced", "reduction": pt::carriage_name(*kind), "name": name, "n": n,
                           "step": step, "pin": pin, "start_level": start_level, "data": datas.len() - 1})
                }
                Chan::Analog(x) => {
                    datas.push(x.data.clone());
                    json!({"kind": "analog", "name": x.name, "n": x.n, "rate": [x.rate.num as i64, x.rate.den as i64],
                           "t0": [x.t0.num as i64, x.t0.den as i64], "width": x.width, "value_bits": x.value_bits,
                           "zero": x.zero, "scale_nv": x.scale_nv, "acquisition": x.acquisition,
                           "data": datas.len() - 1})
                }
            })
            .collect();
        parts.push(json!({"tick0": p.tick0, "meta": p.meta, "samples": p.samples, "channels": chans}));
    }
    let doc = json!({
        "tick_hz": [a.tick_hz.0, a.tick_hz.1],
        "meta": a.meta,
        "one_part": a.parts.is_none(),
        "samples": a.samples,
        "layout": a.layout,
        "lost": a.lost.iter().map(Loss::to_json).collect::<Vec<_>>(),
        "temp_dir": a.temp_dir.as_ref().map(|p| p.to_string_lossy().to_string()),
        "parts": parts,
    });
    (doc, datas)
}

/// A stream's pushes put together as `capture_at` does (for a test that gives the frames itself): the format,
/// whether a loss stops it, the pushes (position, data, segment_info) in the order they came, and an error stop's
/// write position. -> (parts, losses)
pub fn assemble_pushes(
    names: Vec<String>,
    ids: Vec<u16>,
    rate: (u64, u64),
    format: Format,
    stop_on_loss: bool,
    pushes: &[(u64, Vec<u8>, Option<SegmentInfo>)],
    write_pos: Option<u64>,
) -> Result<(Vec<Part>, Vec<Loss>)> {
    let mut s = Spool::new(names, ids, rate, format, stop_on_loss, None);
    for (pos, data, info) in pushes {
        if let Some(i) = info {
            s.segment(*i);
        }
        s.run(*pos, data)?;
        s.work(false, None)?;
    }
    s.finish(write_pos)?;
    Ok(s.take())
}

/// A capture saved to `out` (a WireSkein file, or .sr / .vcd by its suffix), `extra_meta` (source, requested_rate,
/// ...) before its own; a stream's files on disk (`temp_dir`) removed once written, whether or not it was.
pub fn save(a: &Acquired, out: &Path, extra_meta: &Map<String, Value>) -> Result<()> {
    let written = to_capture(a, extra_meta).and_then(|cap| {
        wireskein_io::write::write(out, &cap, &[]).map_err(|e| Error::Io(format!("{}: {e}", out.display())))
    });
    if let Some(d) = &a.temp_dir {
        let _ = std::fs::remove_dir_all(d);
    }
    written
}
