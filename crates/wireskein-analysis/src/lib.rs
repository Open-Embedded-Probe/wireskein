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
pub mod order;
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
                // (a clocked reading's bits: its sampling edges `t`, each data line's row of `values` alike)
                Value::Object(mut o) if o.get("t").is_some_and(|x| x.is_array()) => {
                    let sel: Vec<bool> = o["t"].as_array().unwrap().iter().map(inside).collect();
                    let keep = |a: &mut Vec<Value>| {
                        let mut k = sel.iter();
                        a.retain(|_| k.next().copied().unwrap_or(false));
                    };
                    if let Some(Value::Array(t)) = o.get_mut("t") {
                        keep(t);
                    }
                    if let Some(Value::Array(rows)) = o.get_mut("values") {
                        for r in rows.iter_mut() {
                            if let Value::Array(r) = r {
                                keep(r);
                            }
                        }
                    }
                    Value::Object(o)
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
    claim_doc_shaped(c, &Shape::of(mode), lines, rate, n, window, notes).unwrap_or(Value::Null)
}

/// How the document gives each claim (Python's `export.export`'s mode, select and depth): "final" (its result, the
/// layer `depth` reaches), "all" (its layers and evidence) or "select" (only the layers the paths name,
/// "<protocol>.<layer>" with wildcards, "<protocol>.final" its final layer; a claim none names is left out).
#[derive(Debug, Clone, Copy)]
pub struct Shape<'a> {
    pub mode: &'a str,
    pub select: &'a [String],
    /// "transport" (no result), "frames" (the lower layer), "protocol" (the final layer) or "device" (the final
    /// layer and the device packs' matches)
    pub depth: &'a str,
}

impl<'a> Shape<'a> {
    pub fn of(mode: &'a str) -> Shape<'a> {
        Shape {
            mode,
            select: &[],
            depth: "protocol",
        }
    }
}

/// The depths a final document can reach (Python's `export.DEPTHS`).
pub const DEPTHS: [&str; 4] = ["transport", "frames", "protocol", "device"];

/// Python's `fnmatch.fnmatchcase`: `*`, `?`, `[seq]` and `[!seq]` (a `[` without its `]` is itself).
pub fn fnmatch(name: &str, pat: &str) -> bool {
    /// p: after the "[" -> (whether c is in the class, the class's length with its "]"); None: no "]"
    fn class(p: &[char], c: char) -> Option<(bool, usize)> {
        let neg = p.first() == Some(&'!');
        let start = usize::from(neg);
        let (mut i, mut hit) = (start, false);
        while i < p.len() {
            if p[i] == ']' && i > start {
                return Some((hit != neg, i + 1));
            }
            if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                hit |= p[i] <= c && c <= p[i + 2];
                i += 3;
            } else {
                hit |= p[i] == c;
                i += 1;
            }
        }
        None
    }
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => (0..=s.len()).any(|k| go(&p[1..], &s[k..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some('[') => match class(&p[1..], s.first().copied().unwrap_or('\0')) {
                Some((ok, k)) => !s.is_empty() && ok && go(&p[1 + k..], &s[1..]),
                None => s.first() == Some(&'[') && go(&p[1..], &s[1..]),
            },
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    let p: Vec<char> = pat.chars().collect();
    let s: Vec<char> = name.chars().collect();
    go(&p, &s)
}

/// A claim as the document gives it in a shape (`Shape`); None when the select paths name none of its layers.
pub fn claim_doc_shaped(
    c: &wireskein_core::Claim,
    shape: &Shape,
    lines: &[wireskein_io::Line],
    rate: f64,
    n: i64,
    window: Option<(i64, i64)>,
    notes: bool,
) -> Option<Value> {
    let mut out = Map::new();
    out.insert("protocol".into(), json!(c.protocol));
    out.insert("roles".into(), json!(c.roles));
    out.insert("verdict".into(), json!(c.verdict));
    out.insert("score".into(), json!(c.score));
    out.insert("margin".into(), json!(c.margin));
    out.insert("params".into(), Value::Object(c.params.clone()));
    // (a final document gives none of a clocked reading's sampled layers: left unread)
    let mut lay: Map<String, Value> = if matches!(shape.mode, "all" | "select") {
        order::layers(&c.layers)
    } else {
        order::layers_without(&c.layers, order::sampled_layers(&c.protocol))
    };
    let notes = if notes {
        note(c, &mut lay, lines, rate, n)
    } else {
        Map::new()
    };
    if let Some((s0, s1)) = window {
        lay = windowed(lay, s0, s1);
    }
    let final_of = |lay: &Map<String, Value>| {
        let mut res = Map::new();
        if let Some(k) = final_layers(&c.protocol)
            .iter()
            .find(|k| lay.get(**k).is_some_and(nonempty))
        {
            res.insert(k.to_string(), lay[*k].clone());
        }
        res
    };
    match shape.mode {
        "all" => {
            out.insert("layers".into(), Value::Object(lay));
            out.insert("evidence".into(), Value::Object(c.evidence.clone()));
        }
        "select" => {
            let mut picked = Map::new();
            for path in shape.select {
                let (proto, layer) = path.split_once('.').unwrap_or((path, ""));
                if !fnmatch(&c.protocol, proto) {
                    continue;
                }
                if layer == "final" {
                    picked.extend(final_of(&lay));
                } else {
                    let layer = if layer.is_empty() { "*" } else { layer };
                    for (k, v) in &lay {
                        if fnmatch(k, layer) {
                            picked.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            if picked.is_empty() {
                return None;
            }
            out.insert("result".into(), Value::Object(picked));
        }
        _ => match shape.depth {
            "transport" => {}
            "frames" => {
                let k = report::lower(&c.protocol);
                if let Some(v) = lay.get(k) {
                    out.insert("result".into(), json!({k: v}));
                }
            }
            depth => {
                out.insert("result".into(), Value::Object(final_of(&lay)));
                if depth == "device" {
                    // the device packs' matches, candidates (an address alone) too (Python's `export.devices_of`)
                    let ds: Vec<Value> = lay
                        .get("devices")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter(|m| {
                                    matches!(
                                        m.get("level").and_then(|v| v.as_str()),
                                        Some("identified" | "consistent" | "address_only")
                                    )
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default();
                    if !ds.is_empty() {
                        out.insert("devices".into(), Value::Array(ds));
                    }
                }
            }
        },
    }
    out.extend(notes);
    Some(Value::Object(out))
}

/// The relations a shape keeps: in select mode only those of a protocol the paths name.
pub fn relations_shaped(relations: Vec<Value>, shape: &Shape) -> Vec<Value> {
    if shape.mode != "select" {
        return relations;
    }
    relations
        .into_iter()
        .filter(|r| {
            let p = r.get("protocol").and_then(|v| v.as_str()).unwrap_or("");
            shape
                .select
                .iter()
                .any(|s| fnmatch(p, s.split_once('.').map_or(s.as_str(), |x| x.0)))
        })
        .collect()
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
    /// select paths ("i2c.transactions", "*.final"): given, the mode is "select" (Python's `analyze_file`)
    pub select: &'a [String],
    /// how far up a final document goes (None: "protocol")
    pub depth: Option<&'a str>,
    /// each claim's runner-up in all mode: the Rust core tells none (its rival readings are not kept), as Python's
    /// document of the Rust engine
    pub alternatives: bool,
}

impl Asked<'_> {
    /// The shape the document gives its claims in, for a mode.
    pub fn shape<'b>(&'b self, mode: &'b str) -> Shape<'b> {
        Shape {
            mode: if self.select.is_empty() { mode } else { "select" },
            select: self.select,
            depth: self.depth.unwrap_or("protocol"),
        }
    }
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
    if !matches!(mode, "final" | "all" | "select") {
        return Err(format!(
            "mode {}: final, all or select",
            wireskein_io::j::py_str_repr(mode)
        ));
    }
    let shape = asked.shape(mode);
    if !DEPTHS.contains(&shape.depth) {
        return Err(format!(
            "depth {}: one of {}",
            wireskein_io::j::py_str_repr(shape.depth),
            DEPTHS.join(", ")
        ));
    }
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
                split::analyze(&mut src, rate, hints, o, asked.items, &head, gf.as_ref(), shape.depth)?
            } else {
                let lines = lines_asked(p, cap.tick_hz, asked)?;
                let mut src = wireskein_io::stream::MemSource::new(&lines, p.ticks);
                split::analyze(&mut src, rate, hints, o, asked.items, &head, gf.as_ref(), shape.depth)?
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
                    .filter_map(|c| {
                        let mut d = claim_doc_shaped(c, &shape, lines, rate, p.ticks, window, true)?;
                        if let Some(b) = &c.bus {
                            // a target's claim: the claims with the same bus share its lines
                            d["bus"] = json!(b);
                        }
                        Some(d)
                    })
                    .collect(),
            ),
        );
        if let Some((a, b)) = window {
            d.insert("window".into(), json!([a, b]));
        }
        let relations = relations_shaped(relations, &shape);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_keeps_the_bits_inside_it() {
        // (Python's `export._window`: a clocked reading's `t` and each row of `values` alike)
        let lay: Map<String, Value> = serde_json::from_value(json!({
            "bits": {"clock": "SCL", "data": ["SDA"], "t": [5, 10, 15, 20], "values": [[1, 0, 1, 1]], "deglitch": 0},
            "frames": [{"s": 5, "e": 10, "bits": 2}, {"s": 15, "e": 20, "bits": 2}],
        }))
        .unwrap();
        let w = windowed(lay, 10, 20);
        assert_eq!(w["bits"]["t"], json!([10, 15]));
        assert_eq!(w["bits"]["values"], json!([[0, 1]]));
        assert_eq!(w["bits"]["clock"], json!("SCL"));
        assert_eq!(w["frames"], json!([{"s": 15, "e": 20, "bits": 2}]));
    }

    #[test]
    fn a_final_document_leaves_the_sampled_layers_unread() {
        let raw = wireskein_core::claim::raw(
            r#"{"bits":{"t":[1,2]},"frames":[{"s":1,"e":2,"bits":2}],"words":[],"transactions":[{"s":0,"e":3}],"devices":[]}"#
                .into(),
        );
        let lay = order::layers_without(&raw, order::sampled_layers("i2c"));
        assert_eq!(lay.keys().collect::<Vec<_>>(), ["devices", "transactions"]);
        assert_eq!(lay["transactions"], json!([{"s": 0, "e": 3}]));
        assert_eq!(order::layers_without(&raw, &[]).len(), 5);
    }
}
