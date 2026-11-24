//! A sigrok session (`.sr`, Python's `_engine/srio.py`): one rate for every channel, the logic samples in chunks
//! (`logic-1-1`, `logic-1-2`, ...) of `unitsize` bytes a sample, read a block at a time and kept as each line's
//! changes. `wireskein/sr-extra.json` (written by WireSkein) gives back the exact tick, each channel's step and
//! phase, the capture's id and meta. Analog channels (`analog-1-K-N`, float32 volts at the tick rate) are read as
//! Python's `read_sr` reads them: written by WireSkein, a raw channel's values from `wireskein/analog-K.raw`, a volts
//! channel's own samples at its own rate; else every tick's value. They are also named among the channels not read
//! as logic.

use crate::{Analog, Capture, Kind, Line, Part, fraction, limit_denominator};
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
    let analog: BTreeMap<usize, String> = dev
        .iter()
        .filter_map(|(k, v)| Some((k.strip_prefix("analog")?.parse::<usize>().ok()?, v.clone())))
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
            name: name.clone(),
            initial,
            edges: st.2,
            step,
            phase,
            reduction: None,
            acquisition: extra["channels"][&name]["acquisition"]
                .as_object()
                .cloned()
                .unwrap_or_default(),
        });
    }
    let tick_hz = match extra["tick_hz"].as_array() {
        Some(a) if a.len() == 2 => fraction(a[0].as_i64().unwrap_or(1), a[1].as_i64().unwrap_or(1)),
        _ => limit_denominator(rate),
    };
    let mut meta: Map<String, Value> = extra["meta"].as_object().cloned().unwrap_or_default();
    meta.insert("unitsize".into(), json!(unitsize));
    let skipped: Vec<Value> = analog
        .values()
        .map(|n| json!({"name": n, "encoding": "analog-f32", "analog": true}))
        .collect();
    let mut traces = vec![];
    // the tick a .sr's samples are at: the extra's, else its samplerate (as Python's Fraction(round(rate)))
    let sr_tick = match extra["tick_hz"].as_array() {
        Some(_) => tick_hz,
        None => (rate.round_ties_even() as i64, 1),
    };
    for (idx, name) in &analog {
        let mut parts: Vec<(i64, String)> = z
            .file_names()
            .filter_map(|n| {
                let k = n.strip_prefix(&format!("analog-1-{idx}-"))?.parse::<i64>().ok()?;
                Some((k, n.to_string()))
            })
            .collect();
        parts.sort();
        let mut raw = vec![];
        for (_, n) in &parts {
            z.by_name(n)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut raw)
                .map_err(|e| e.to_string())?;
        }
        let vals: Vec<f64> = raw
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b) as f64)
            .collect();
        let info = &extra["analog"][name];
        let acquisition = info["acquisition"].as_object().cloned().unwrap_or_default();
        let terms = |v: &Value| match v.as_array() {
            Some(a) if a.len() == 2 => fraction(a[0].as_i64().unwrap_or(0), a[1].as_i64().unwrap_or(1)),
            _ => (0, 1),
        };
        let file = info["file"]
            .as_str()
            .filter(|f| z.file_names().any(|n| n == *f))
            .map(String::from);
        let trace = if let (Some(f), true) = (file, info.is_object()) {
            // written by WireSkein: the raw values come back
            let width = info["width"].as_i64().unwrap_or(16);
            let mut data = vec![];
            z.by_name(&f)
                .map_err(|e| e.to_string())?
                .read_to_end(&mut data)
                .map_err(|e| e.to_string())?;
            let size = (width / 8).max(1) as usize;
            let values = data
                .chunks_exact(size)
                .map(|b| match size {
                    1 => b[0] as f64,
                    2 => u16::from_le_bytes([b[0], b[1]]) as f64,
                    _ => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
                })
                .collect();
            Analog {
                name: name.clone(),
                encoding: "analog".into(),
                rate_hz: terms(&info["rate_hz"]),
                t0_ticks: terms(&info["t0_ticks"]),
                width,
                value_bits: info["value_bits"].as_i64(),
                zero: info["zero"].as_f64(),
                scale_nv: info["scale_nv"].as_f64(),
                unit: "V".into(),
                values,
                acquisition,
            }
        } else if info.is_object() {
            // volts written by WireSkein at its own rate: its samples picked from the ticks
            let rate_hz = terms(&info["rate_hz"]);
            let t0_ticks = terms(&info["t0_ticks"]);
            let per = ((sr_tick.0 as i128 * rate_hz.1 as i128) / (sr_tick.1 as i128 * rate_hz.0.max(1) as i128)).max(1)
                as usize;
            let t0 = t0_ticks.0.div_euclid(t0_ticks.1.max(1)).max(0) as usize;
            let n = info["n"].as_u64().unwrap_or(u64::MAX) as usize;
            Analog {
                name: name.clone(),
                encoding: "analog-f32".into(),
                rate_hz,
                t0_ticks,
                width: 16,
                value_bits: None,
                zero: None,
                scale_nv: None,
                unit: info["unit"].as_str().unwrap_or("V").into(),
                values: vals.iter().skip(t0).step_by(per).take(n).copied().collect(),
                acquisition,
            }
        } else {
            Analog {
                name: name.clone(),
                encoding: "analog-f32".into(),
                rate_hz: sr_tick,
                t0_ticks: (0, 1),
                width: 16,
                value_bits: None,
                zero: None,
                scale_nv: None,
                unit: "V".into(),
                values: vals,
                acquisition,
            }
        };
        traces.push(trace);
    }
    // no logic: as long as the longest analog channel's ticks
    let n_ticks = if n_ticks > 0 {
        n_ticks
    } else {
        traces.iter().map(|a| a.values.len() as i64).max().unwrap_or(0)
    };
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
            analog: traces,
            skipped,
        }],
    })
}
