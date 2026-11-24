//! WireSkein's analysis document in plain Rust (docs/analysis-format.md §4; Python's `decode.analyze_file`): a capture
//! file (a WireSkein file, a sigrok session or a VCD) read (wireskein-io), each part analyzed by the core, each claim
//! given as the document gives it with what analyze tells beside it (`report`), a long part read in pieces (`split`).
//! Beside it, what is made from a capture without the engine's choice or from the document: the states of a line
//! (`states`), the overview of a capture (`survey`), the rows a viewer draws (`annotate`); and the buses a spec names
//! decoded with exactly those roles (`decode`, Python's `decode.decode_file`).
//! Shared by the command line (wireskein-cli) and the Python module (wireskein-py).

pub mod align;
pub mod analog;
pub mod annotate;
pub mod decode;
pub mod report;
pub mod segments;
pub mod split;
pub mod states;
pub mod survey;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use wireskein_core::{Channel, View};

/// The document's format.
pub const FORMAT: &str = "wireskein-analysis/1";

/// The layer a claim's result gives in final mode, the first of these that has items (Python's `export.FINAL`).
pub fn final_layers(protocol: &str) -> &'static [&'static str] {
    match protocol {
        "i2c" => &["transactions"],
        "spi" => &["sd", "transfers"],
        "uart" => &["nmea", "modbus_rtu", "lines", "bytes"],
        "lin" => &["frames"],
        "dmx512" => &["packets"],
        "rvswd" | "swio" => &["dm", "dmi"],
        "sync_unknown" => &["summary"],
        "swd" | "usb_pd" | "dcc" => &["packets"],
        "swim" => &["transactions"],
        "cec" | "sent" => &["messages"],
        "spdif" => &["subframes"],
        "jtag" => &["scans"],
        "dht" => &["readings"],
        "onewire" => &["resets"],
        "usb" => &["transactions", "packets"],
        _ => &["frames"],
    }
}

pub fn nonempty(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// Only the items inside ticks [s0, s1) (Python's `export._window`: what a GUI needs for the visible range).
fn windowed(lay: Map<String, Value>, s0: i64, s1: i64) -> Map<String, Value> {
    let inside = |v: &Value| v.as_i64().is_some_and(|s| s0 <= s && s < s1);
    lay.into_iter()
        .map(|(k, v)| {
            let w = match v {
                Value::Array(a) if a.first().is_some_and(|x| x.get("s").is_some()) => {
                    Value::Array(a.into_iter().filter(|x| x.get("s").is_some_and(inside)).collect())
                }
                Value::Object(o) if o.get("s").is_some_and(|x| x.is_array()) => {
                    let sel: Vec<bool> = o["s"].as_array().unwrap().iter().map(inside).collect();
                    let n = sel.len();
                    let mut w = Map::new();
                    for (kk, vv) in &o {
                        let x = match vv {
                            Value::Array(a) if a.len() == n => Value::Array(
                                a.iter()
                                    .zip(&sel)
                                    .filter(|(_, k)| **k)
                                    .map(|(x, _)| x.clone())
                                    .collect(),
                            ),
                            Value::String(h) if kk == "hex" => Value::String(
                                (0..h.len() / 2)
                                    .filter(|&i| sel.get(i).copied().unwrap_or(false))
                                    .map(|i| &h[2 * i..2 * i + 2])
                                    .collect(),
                            ),
                            x => x.clone(),
                        };
                        w.insert(kk.clone(), x);
                    }
                    Value::Object(w)
                }
                v => v,
            };
            (k, w)
        })
        .collect()
}

/// A claim as the document gives it: in final mode its result (the final layer), in all mode its layers and
/// evidence.
/// lines / rate / n: the part's, for what analyze tells beside it (notes); window: only the items inside these ticks.
pub fn claim_doc(
    c: &wireskein_core::Claim,
    mode: &str,
    lines: &[wireskein_io::Line],
    rate: f64,
    n: i64,
    window: Option<(i64, i64)>,
    notes: bool,
) -> Value {
    let mut out = Map::new();
    out.insert("protocol".into(), json!(c.protocol));
    out.insert("roles".into(), json!(c.roles));
    out.insert("verdict".into(), json!(c.verdict));
    out.insert("score".into(), json!(c.score));
    out.insert("margin".into(), json!(c.margin));
    out.insert("params".into(), Value::Object(c.params.clone()));
    let mut lay: Map<String, Value> = serde_json::from_str(c.layers.get()).unwrap_or_default();
    let notes = if notes {
        note(c, &mut lay, lines, rate, n)
    } else {
        Map::new()
    };
    if let Some((s0, s1)) = window {
        lay = windowed(lay, s0, s1);
    }
    if mode == "all" {
        out.insert("layers".into(), Value::Object(lay));
        out.insert("evidence".into(), Value::Object(c.evidence.clone()));
    } else {
        let mut res = Map::new();
        if let Some(k) = final_layers(&c.protocol)
            .iter()
            .find(|k| lay.get(**k).is_some_and(nonempty))
        {
            res.insert(k.to_string(), lay[*k].clone());
        }
        out.insert("result".into(), Value::Object(res));
    }
    out.extend(notes);
    Value::Object(out)
}

/// What analyze tells beside a claim (Python's `decode._note_undersampled`): an RVSWD or SWIO claim's unread
/// stretches and `reading`; a clocked claim's frames given their clock (`period_ns`, `high_ns`), its `reading`,
/// `clock_speeds` and `undersampled`.
fn note(
    c: &wireskein_core::Claim,
    lay: &mut Map<String, Value>,
    lines: &[wireskein_io::Line],
    rate: f64,
    n: i64,
) -> Map<String, Value> {
    let mut out = Map::new();
    let line = |name: &str| lines.iter().find(|l| l.name == name);
    let pins: Vec<&wireskein_io::Line> = c.roles.values().filter_map(|p| line(p)).collect();
    let clk = c.roles.get("clk").and_then(|p| line(p));
    let data: Vec<&wireskein_io::Line> = c
        .roles
        .iter()
        .filter(|(k, _)| *k != "clk")
        .filter_map(|(_, p)| line(p))
        .collect();
    let un = if c.protocol == "rvswd" || c.protocol == "swio" {
        // 16 bit times (the median interval of the clock's rises, as Python's `decode._idle` takes another engine's
        // reading); without a clock, 1 ms
        let rises: Vec<i64> = clk
            .map(|l| {
                (0..l.edges.len())
                    .filter(|&i| l.initial ^ (((i + 1) & 1) as u8) == 1)
                    .map(|i| l.edges[i])
                    .collect()
            })
            .unwrap_or_default();
        let idle = if rises.len() > 1 {
            let mut d: Vec<f64> = rises.windows(2).map(|w| (w[1] - w[0]) as f64).collect();
            (report::IDLE_BITS as f64 * report::median(&mut d)) as i64
        } else {
            (report::IDLE_S * rate).round_ties_even() as i64
        };
        let mut empty = vec![];
        let un = report::unread(&c.protocol, &pins, lay, idle, None, n, &mut empty);
        if clk.is_none() {
            let frames = report::frame_items(lay, &c.protocol).cloned().unwrap_or_default();
            let r = report::reading(&frames, Some(&un), &[], &pins, None, &[], n);
            out.insert("reading".into(), Value::Object(r));
        }
        Some(un)
    } else {
        None
    };
    let Some(clk) = clk else { return out };
    let Some(frames) = report::frame_items(lay, &c.protocol) else {
        let r = report::reading(&[], un.as_deref(), &[], &pins, Some(clk), &data, n);
        out.insert("reading".into(), Value::Object(r));
        return out;
    };
    report::frame_clocks(frames, clk, rate);
    let speeds = report::clock_speeds(rate, frames);
    let r = report::reading(frames, un.as_deref(), &speeds, &pins, Some(clk), &data, n);
    out.insert("reading".into(), Value::Object(r));
    if !speeds.is_empty() {
        out.insert("clock_speeds".into(), json!(speeds));
    }
    if let Some(u) = report::undersampled(rate, &speeds, &clk.name) {
        out.insert("undersampled".into(), u);
    }
    out
}

/// The hints of `--hint`: protocol names ("uart,spi"), JSON, or @a file of JSON.
pub fn hints_of(arg: &str) -> Result<Value, String> {
    let text = match arg.strip_prefix('@') {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        None => arg.to_string(),
    };
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(&text).map_err(|e| format!("--hint: {e}"));
    }
    Ok(json!({"protocols": text.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>()}))
}

/// What analyze is asked beyond the file, the part, the mode and the window (Python's `decode.analyze_file`'s
/// other arguments; the defaults: none of them).
#[derive(Debug, Default, Clone)]
pub struct Asked<'a> {
    /// only these channels read (the others dropped before anything reads them)
    pub pins: Option<&'a [String]>,
    /// analog channels also read as logic: (name, low, high) in volts
    pub thresholds: &'a [(String, f64, f64)],
    /// a part read in pieces: its items written to this file as they are read (ndjson), with the name the
    /// document gives it
    pub items: Option<(&'a Path, &'a str)>,
    /// only the items inside the first marker segment whose path matches this glob
    pub segment: Option<&'a str>,
    /// the marker source ({"pin", "dialect"}), the dialects ({name: {"dialect", "marker"}}), and whether the
    /// segment's text lines go in as events
    pub markers: Option<&'a Value>,
    pub dialects: Option<&'a Value>,
    pub events: bool,
}

/// The document of what analyze finds in a capture file.
pub fn analyze_file(
    path: &Path,
    hints: &Value,
    part: Option<usize>,
    mode: &str,
    window_s: Option<(f64, f64)>,
    split_opts: Option<&split::Options>,
) -> Result<Value, String> {
    analyze_file_asked(path, hints, part, mode, window_s, split_opts, &Asked::default())
}

/// The lines of a part as analyze reads them: the analog channels asked read as logic too (after the part's own),
/// then only the pins asked (Python's `as_logic`, then `only`).
pub fn lines_asked(
    p: &wireskein_io::Part,
    tick_hz: (i64, i64),
    asked: &Asked,
) -> Result<Vec<wireskein_io::Line>, String> {
    let mut extra = vec![];
    for (name, lo, hi) in asked.thresholds {
        let a = p
            .analog
            .iter()
            .find(|a| &a.name == name)
            .ok_or_else(|| format!("{name}: not an analog channel in this capture"))?;
        if p.lines.iter().any(|l| &l.name == name) {
            return Err(format!("{name}: already a logic channel"));
        }
        extra.push(analog::to_logic(a, tick_hz, *lo, *hi)?);
    }
    let Some(pins) = asked.pins.filter(|x| !x.is_empty()) else {
        let mut out = p.lines.clone();
        out.extend(extra);
        return Ok(out);
    };
    let names: std::collections::BTreeSet<&str> = p
        .lines
        .iter()
        .map(|l| l.name.as_str())
        .chain(p.analog.iter().map(|a| a.name.as_str()))
        .chain(p.skipped.iter().filter_map(|x| x["name"].as_str()))
        .chain(p.events.iter().map(|e| e.name.as_str()))
        .collect();
    let miss: Vec<&str> = pins.iter().map(|x| x.as_str()).filter(|x| !names.contains(x)).collect();
    if !miss.is_empty() {
        return Err(format!("pins not in the capture: {}", miss.join(", ")));
    }
    Ok(p.lines
        .iter()
        .cloned()
        .chain(extra)
        .filter(|l| pins.contains(&l.name))
        .collect())
}

/// The document of what analyze finds in a capture file, with what else was asked (`Asked`).
#[allow(clippy::too_many_arguments)]
pub fn analyze_file_asked(
    path: &Path,
    hints: &Value,
    part: Option<usize>,
    mode: &str,
    window_s: Option<(f64, f64)>,
    split_opts: Option<&split::Options>,
    asked: &Asked,
) -> Result<Value, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let splitting = split_opts.filter(|_| window_s.is_none() && asked.segment.is_none() && part.is_none());
    // a WireSkein file of parts each read a block at a time when it is split (its header only, now); else read whole
    let kind = wireskein_io::sniff_path(path);
    let (cap, streamed) = match (kind, splitting) {
        (Some(wireskein_io::Kind::WireSkein), Some(_)) if asked.thresholds.is_empty() => {
            let f = std::fs::File::open(path).map_err(|e| at(e.to_string()))?;
            match wireskein_io::wsk::head_from(f).map_err(at)? {
                (c, true) if !cfg!(target_family = "wasm") => (c, true),
                _ => (wireskein_io::read(path)?, false),
            }
        }
        _ => (wireskein_io::read(path)?, false),
    };
    if let Some((f, _)) = asked.items {
        std::fs::write(f, b"").map_err(|e| format!("{}: {e}", f.display()))?; // written part after part
    }
    let rate = cap.rate();
    if (window_s.is_some() || asked.segment.is_some()) && part.is_none() && cap.parts.len() > 1 {
        return Err(at(format!(
            "{} parts: give the part a window or segment is inside (--part)",
            cap.parts.len()
        )));
    }
    let window = window_s.map(|(a, b)| ((a * rate) as i64, (b * rate) as i64));
    let mut capture = Map::new();
    capture.insert("file".into(), json!(path.file_name().map(|s| s.to_string_lossy())));
    capture.insert("sha256".into(), json!(sha256_of(path).map_err(at)?));
    capture.insert("id".into(), json!(cap.id));
    capture.insert("tick_hz".into(), json!([cap.tick_hz.0, cap.tick_hz.1]));
    capture.insert("complete".into(), json!(cap.complete));
    if !cap.lost.is_empty() {
        capture.insert("lost".into(), json!(cap.lost));
    }
    capture.insert("layout".into(), json!(cap.layout));
    capture.insert("parts".into(), json!(cap.n_parts));
    let mut parts = vec![];
    for p in &cap.parts {
        if let Some(k) = part {
            let (a, b) = p.parts.unwrap_or((p.part, p.part));
            if !(a <= k && k <= b) {
                continue;
            }
        }
        let mut head = Map::new();
        head.insert("part".into(), json!(p.part));
        head.insert("ticks".into(), json!(p.ticks));
        if let Some((a, b)) = p.parts {
            head.insert("parts".into(), json!([a, b]));
        }
        if cap.kind == wireskein_io::Kind::WireSkein {
            if let Some(t0) = p.tick0 {
                head.insert("tick0".into(), json!(t0));
            }
        } else {
            head.insert("tick0".into(), json!(0));
        }
        head.extend(wireskein_io::wsk::part_keys(&p.meta));
        // a long part read in pieces (a WireSkein file's part straight from the file, a block at a time)
        let mut whole: Option<Vec<wireskein_io::Line>> = None;
        if let Some(o) = splitting {
            let gf = decode::glitch_filter(&p.meta);
            let got = if streamed {
                let mut src = wireskein_io::stream::file_source_of(path, p.part, asked.pins.filter(|x| !x.is_empty()))
                    .map_err(at)?
                    .ok_or_else(|| at(format!("part {}: not a part to read a block at a time", p.part)))?;
                split::analyze(&mut src, rate, hints, o, asked.items, &head, gf.as_ref())?
            } else {
                let lines = lines_asked(p, cap.tick_hz, asked)?;
                let mut src = wireskein_io::stream::MemSource::new(&lines, p.ticks);
                split::analyze(&mut src, rate, hints, o, asked.items, &head, gf.as_ref())?
            };
            match got {
                split::Split::Pieces(claims, sp) => {
                    let mut d = head;
                    d.insert("claims".into(), Value::Array(claims));
                    d.insert("split".into(), sp);
                    parts.push(Value::Object(d));
                    continue;
                }
                split::Split::Whole(lines) => {
                    if streamed {
                        whole = Some(lines);
                    }
                }
            }
        }
        let whole = match whole {
            Some(w) => w,
            None => lines_asked(p, cap.tick_hz, asked)?,
        };
        let lines: &[wireskein_io::Line] = &whole;
        let chans: Vec<Channel> = lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect();
        let view = View::new(rate, p.ticks, chans).map_err(|e| format!("part {}: {e}", p.part))?;
        let (claims, relations) = wireskein_core::analyze_full(&view, hints);
        // a marker segment: its window, the text lines in it
        let (mut window, mut seg, mut events) = (window, None, None);
        if let Some(pat) = asked.segment {
            let own = segments::heading();
            let ds = segments::dialects(asked.dialects.unwrap_or(&own))?;
            let us = segments::uarts(&claims);
            let f = segments::find(&us, &ds, rate, p.ticks, asked.markers)
                .ok_or("no marker stream found (give --markers)")?;
            let Some(&h) = f.tree.select(pat).first() else {
                return Err(format!(
                    "no segment matches {}; see: wireskein segments CAPTURE",
                    wireskein_io::j::py_str_repr(pat)
                ));
            };
            let s = &f.tree.segs[h];
            let w = (s.begin, s.end.unwrap_or(p.ticks));
            window = Some(w);
            seg = Some(json!({"path": f.tree.path(h), "raw": s.raw, "issues": s.issues}));
            if asked.events || mode == "all" {
                events = Some(segments::events_in(&us, &f, &ds, rate, w));
            }
        }
        let mut d = head;
        d.insert(
            "claims".into(),
            Value::Array(
                claims
                    .iter()
                    .map(|c| claim_doc(c, mode, lines, rate, p.ticks, window, true))
                    .collect(),
            ),
        );
        if let Some((a, b)) = window {
            d.insert("window".into(), json!([a, b]));
        }
        if !relations.is_empty() {
            d.insert("relations".into(), Value::Array(relations));
        }
        if let Some(x) = seg {
            d.insert("segment".into(), x);
        }
        if let Some(x) = events {
            d.insert("events".into(), Value::Array(x));
        }
        // the channels not read as logic (an analog one read at a threshold is; those not asked for are not told)
        let skipped: Vec<Value> = p
            .skipped
            .iter()
            .filter(|x| {
                let name = x["name"].as_str().unwrap_or("");
                !asked.thresholds.iter().any(|(t, _, _)| t == name)
                    && asked
                        .pins
                        .filter(|ps| !ps.is_empty())
                        .is_none_or(|ps| ps.iter().any(|q| q == name))
            })
            .cloned()
            .collect();
        if !skipped.is_empty() {
            d.insert("skipped_channels".into(), Value::Array(skipped));
        }
        parts.push(Value::Object(d));
    }
    if let Some(k) = part
        && parts.is_empty()
    {
        return Err(at(format!("part {k} of {}", cap.n_parts)));
    }
    Ok(
        json!({"format": FORMAT, "wireskein": env!("CARGO_PKG_VERSION"), "hints": hints,
              "capture": capture, "parts": parts}),
    )
}

/// A file's SHA-256, read a block at a time.
pub fn sha256_of(path: &Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let k = f.read(&mut buf).map_err(|e| e.to_string())?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
    }
    Ok(format!("{:x}", h.finalize()))
}

/// "300us", "1.5ms", "2s", "0.001" as seconds.
pub fn seconds(t: &str) -> Option<f64> {
    let t = t.trim();
    let k = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let mult = match t[k..].trim() {
        "ns" => 1e-9,
        "us" | "µs" => 1e-6,
        "ms" => 1e-3,
        "s" | "" => 1.0,
        _ => return None,
    };
    Some(t[..k].parse::<f64>().ok()? * mult)
}
