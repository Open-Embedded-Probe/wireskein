//! The part of an OEP client a capture needs (bench/notes/2026-11-24-rust-plan.ja.md §37), in plain Rust: find the
//! probe (an oep:// address's unit_id as its USB serial, or a TCP address), open it (USB vendor bulk with many IN
//! transfers queued, or TCP), take its lease, run a logic capture on oep.fixture.logic (one-shot, repeat, stream;
//! capture format 0 and format 1's reductions) and put it together: the parts with their losses, times and the probe's
//! facts, as oep-client-python's `acquire` gives them, and as WireSkein's capture (`to_capture`, wireskein-io).
//!
//! Not here (oep-client-python's, which wireskein's Python source keeps using for them): analog channels and the
//! capture group, serial ports (COBS, port_speed), HID and CDC routes, DNS-SD, brokers, a line named by its label
//! (S.N), the kept session of a run before.
//!
//! ```no_run
//! let req = wireskein_oep::Request::new(vec![("A".into(), "GPIO30".into())], 20_000_000, 100_000);
//! let got = wireskein_oep::capture_at("oep://30eda0ea068b", &req, "wireskein capture")?;
//! # Ok::<(), wireskein_oep::Error>(())
//! ```

pub mod acquire;
pub mod assemble;
pub mod blocks;
pub mod capture;
pub mod host;
pub mod link;
pub mod msg;
pub mod transport;

pub use acquire::{Acquired, Address, Mode, Request, capture_at, parse_address};
pub use assemble::{Chan, Data, Loss, Part};
pub use msg::{Error, Result};

use serde_json::{Map, Value, json};

/// Python's `format(x, "g")`.
pub fn fmt_g(x: f64) -> String {
    wireskein_io::pyfmt::g(x, 6)
}

/// A part's length: the probe's (its samples) when a line runs at a coarser step, else its channels' latest end.
fn part_ticks(p: &Part) -> i64 {
    let end = p.channels.iter().map(|c| c.end() as i64).max().unwrap_or(0);
    if p.channels.iter().any(|c| c.step() > 1) {
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
    for (k, p) in a.all_parts().iter().enumerate() {
        let mut lines = vec![];
        let mut events = vec![];
        for c in &p.channels {
            let acq: Map<String, Value> = match c {
                Chan::Bits { pin, .. } | Chan::Reduced { pin, .. } => {
                    [("pin".to_string(), json!(pin))].into_iter().collect()
                }
            };
            match c {
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
                } if *kind == blocks::CHANGE_LATCH => {
                    let (mut ks, mut levels, mut counts) = (vec![], vec![], vec![]);
                    let mut before = *start_level;
                    for j in 0..*n as usize {
                        let (lv, ch) = (blocks::bit(data, 2 * j), blocks::bit(data, 2 * j + 1));
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
                        reduction: Some(blocks::carriage_name(*kind).into()),
                        acquisition: acq,
                    });
                }
            }
        }
        parts.push(wireskein_io::Part {
            part: k,
            parts: None,
            ticks: part_ticks(p),
            tick0: p.tick0,
            meta: p.meta.clone(),
            lines,
            events,
            analog: vec![],
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
                    json!({"kind": "reduced", "reduction": blocks::carriage_name(*kind), "name": name, "n": n,
                           "step": step, "pin": pin, "start_level": start_level, "data": datas.len() - 1})
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
    format: assemble::Format,
    stop_on_loss: bool,
    pushes: &[(u64, Vec<u8>, Option<capture::SegmentInfo>)],
    write_pos: Option<u64>,
) -> Result<(Vec<Part>, Vec<Loss>)> {
    let mut s = assemble::Spool::new(names, ids, rate, format, stop_on_loss, None);
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
