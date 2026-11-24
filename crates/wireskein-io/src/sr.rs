//! A sigrok session (`.sr`, Python's `_engine/srio.py`): one rate for every channel, the logic samples in chunks
//! (`logic-1-1`, `logic-1-2`, ...) of `unitsize` bytes a sample, read a block at a time and kept as each line's
//! changes. `wireskein/sr-extra.json` (written by WireSkein) gives back the exact tick, each channel's step and
//! phase, the capture's id and meta. Analog channels are left out (named).

use crate::{Capture, Kind, Line, Part, fraction, limit_denominator};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::{Cursor, Read};

pub const EXTRA: &str = "wireskein/sr-extra.json";
const BLOCK: usize = 1 << 20; // samples read at a time

/// The metadata's sections (each line stands alone: sigrok 0.2 indented a probe's trigger on a line of its own).
fn metadata(text: &str) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut sect = String::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') && t.ends_with(']') {
            sect = t[1..t.len() - 1].to_string();
            out.entry(sect.clone()).or_default();
        } else if let Some((k, v)) = t.split_once('=') {
            out.entry(sect.clone())
                .or_default()
                .insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    out
}

fn rate_of(text: &str) -> Result<f64, String> {
    let t = text.trim().to_lowercase();
    let (num, unit) = t.split_at(t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len()));
    let mult = match unit.trim() {
        "hz" => 1.0,
        "khz" => 1e3,
        "mhz" => 1e6,
        "ghz" => 1e9,
        _ => return Err(format!("unknown samplerate {text:?}")),
    };
    Ok(num.parse::<f64>().map_err(|_| format!("unknown samplerate {text:?}"))? * mult)
}

/// A sigrok session read.
pub fn read(bytes: &[u8]) -> Result<Capture, String> {
    read_from(Cursor::new(bytes))
}

/// A sigrok session read from a reader (a file on disk: its chunks decompressed as they are read).
pub fn read_from<R: Read + std::io::Seek>(r: R) -> Result<Capture, String> {
    let mut z = zip::ZipArchive::new(r).map_err(|e| e.to_string())?;
    let mut text = String::new();
    z.by_name("metadata")
        .map_err(|e| e.to_string())?
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let md = metadata(&text);
    let dev = md.get("device 1").ok_or("no [device 1] in the metadata")?;
    let rate = rate_of(
        dev.get("samplerate")
            .ok_or("the .sr gives no samplerate (its metadata has none); analysis needs the sample rate")?,
    )?;
    let unitsize: usize = dev.get("unitsize").and_then(|v| v.parse().ok()).unwrap_or(1);
    if !(1..=8).contains(&unitsize) {
        return Err(format!("a .sr sample of {unitsize} bytes: up to 8 (64 channels) read"));
    }
    let prefix = dev.get("capturefile").cloned().unwrap_or_else(|| "logic-1".into());
    let names: BTreeMap<usize, String> = dev
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("probe")?.parse::<usize>().ok()?, v.clone())))
        .collect();
    let analog: Vec<String> = dev
        .iter()
        .filter_map(|(k, v)| k.strip_prefix("analog")?.parse::<usize>().ok().map(|_| v.clone()))
        .collect();
    let total: usize = dev
        .get("total probes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(unitsize * 8);
    let mut chunks: Vec<(i64, String)> = z
        .file_names()
        .filter(|n| *n == prefix || n.starts_with(&format!("{prefix}-")))
        .map(|n| {
            let k = if n == prefix {
                0
            } else {
                n.rsplit('-').next().and_then(|x| x.parse().ok()).unwrap_or(0)
            };
            (k, n.to_string())
        })
        .collect();
    chunks.sort();
    let extra: Value = match z.by_name(EXTRA) {
        Ok(mut f) => {
            let mut s = String::new();
            f.read_to_string(&mut s).map_err(|e| e.to_string())?;
            serde_json::from_str(&s).unwrap_or(Value::Null)
        }
        Err(_) => Value::Null,
    };
    let extra = if extra["format"] == json!("wireskein-sr-extra/1") {
        extra
    } else {
        Value::Null
    };
    let bits: Vec<usize> = if names.is_empty() {
        (0..total.min(unitsize * 8)).collect()
    } else {
        names.keys().map(|b| b - 1).collect()
    };
    let bits: Vec<usize> = bits.into_iter().filter(|&b| b < unitsize * 8).collect();
    let own = |name: &str, k: &str| extra["channels"][name][k].as_i64();
    let lname = |b: usize| names.get(&(b + 1)).cloned().unwrap_or_else(|| format!("bit{b}"));
    // each line read at its own step and phase (step 1 unless the extra says): its samples at phase + k * step
    let plan: Vec<(usize, String, i64, i64)> = bits
        .iter()
        .map(|&b| {
            let n = lname(b);
            let (s, p) = (
                own(&n, "step").unwrap_or(1).max(1),
                own(&n, "phase").unwrap_or(0).max(0),
            );
            (b, n, s, p)
        })
        .collect();
    let mut state: Vec<(Option<u8>, u8, Vec<i64>)> = vec![(None, 0, vec![]); plan.len()];
    let mut first_sample: Option<u64> = None;
    let mut idx: i64 = 0;
    let mut buf = vec![0u8; BLOCK * unitsize];
    let mut carry: Vec<u8> = vec![];
    for (_, name) in &chunks {
        let mut f = z.by_name(name).map_err(|e| e.to_string())?;
        loop {
            let got = f.read(&mut buf).map_err(|e| e.to_string())?;
            if got == 0 {
                break;
            }
            carry.extend_from_slice(&buf[..got]);
            let whole = carry.len() / unitsize * unitsize;
            for s in carry[..whole].chunks_exact(unitsize) {
                let v = s.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64);
                if first_sample.is_none() {
                    first_sample = Some(v);
                }
                for (st, &(b, _, step, phase)) in state.iter_mut().zip(&plan) {
                    if idx < phase || (idx - phase) % step != 0 {
                        continue;
                    }
                    let lv = ((v >> b) & 1) as u8;
                    match st.0 {
                        None => {
                            st.0 = Some(lv);
                            st.1 = lv;
                        }
                        Some(_) if lv != st.1 => {
                            st.2.push(idx);
                            st.1 = lv;
                        }
                        _ => {}
                    }
                }
                idx += 1;
            }
            carry.drain(..whole);
        }
    }
    let n_ticks = idx;
    let mut lines = vec![];
    for (st, (b, name, step, phase)) in state.into_iter().zip(plan) {
        // a line of its own phase starts at the capture's first sample's level (as srio.read_sr)
        let initial = if phase > 0 {
            first_sample.map_or(0, |v| ((v >> b) & 1) as u8)
        } else {
            st.0.unwrap_or(0)
        };
        lines.push(Line {
            name,
            initial,
            edges: st.2,
            step,
            phase,
            reduction: None,
        });
    }
    let tick_hz = match extra["tick_hz"].as_array() {
        Some(a) if a.len() == 2 => fraction(a[0].as_i64().unwrap_or(1), a[1].as_i64().unwrap_or(1)),
        _ => limit_denominator(rate),
    };
    let mut meta: Map<String, Value> = extra["meta"].as_object().cloned().unwrap_or_default();
    meta.insert("unitsize".into(), json!(unitsize));
    let skipped: Vec<Value> = analog
        .iter()
        .map(|n| json!({"name": n, "encoding": "analog-f32", "analog": true}))
        .collect();
    Ok(Capture {
        kind: Kind::Sr,
        tick_hz,
        id: extra["id"].as_str().map(String::from),
        complete: true,
        lost: vec![],
        layout: "timeline".into(),
        n_parts: 1,
        meta: meta.clone(),
        parts: vec![Part {
            part: 0,
            parts: None,
            ticks: n_ticks,
            tick0: Some(0),
            meta,
            lines,
            events: vec![],
            analog: vec![],
            skipped,
        }],
    })
}
