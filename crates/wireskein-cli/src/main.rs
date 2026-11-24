//! `wireskein-rs`: WireSkein's command line in plain Rust (bench/notes/2026-11-24-rust-plan.ja.md §3, R5).
//!
//!     wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all]
//!
//! prints what analyze finds as the analysis format's document (docs/analysis-format.md §4), as `wireskein analyze`
//! does: the capture, then each part (a run of parts that follow on without a gap read as one) with its claims and
//! the relations of its UART pairs. A WireSkein file, a sigrok session (.sr) or a VCD, told apart by content.

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::ExitCode;
use wireskein_core::{Channel, View};

const FORMAT: &str = "wireskein-analysis/1";

/// The layer a claim's result gives in final mode, the first of these that has items (Python's `export.FINAL`).
fn final_layers(protocol: &str) -> &'static [&'static str] {
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

fn nonempty(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// The layer a clocked bus reads frame by frame (Python's `export.LOWER`; RVSWD's and SWIO's DMI accesses).
fn lower(protocol: &str) -> &'static str {
    match protocol {
        "i2c" | "swim" => "transactions",
        "spi" => "transfers",
        "uart" => "bytes",
        "rvswd" | "swio" => "dmi",
        "swd" | "usb_pd" | "dcc" | "usb" => "packets",
        "dmx512" => "packets",
        "cec" | "sent" => "messages",
        "spdif" => "subframes",
        "jtag" => "scans",
        "dht" => "readings",
        "sync_unknown" => "summary",
        "onewire" => "resets",
        _ => "frames",
    }
}

/// Each frame of a clocked bus given its clock (Python's `decode.frame_clocks`): `period_ns` (the median of its
/// clock's periods, kept when the reader gave it) and `high_ns` (the median of its highs), for a frame of 8 clocks
/// or more; a clock of its own step left as it is.
fn frame_clocks(lay: &mut Map<String, Value>, protocol: &str, clock: &wireskein_io::Line, rate: f64) {
    if clock.step != 1 {
        return;
    }
    let Some(items) = lay.get_mut(lower(protocol)).and_then(|v| v.as_array_mut()) else {
        return;
    };
    let e = &clock.edges;
    // the rises with a fall after them, and each one's high
    let (mut t, mut high) = (vec![], vec![]);
    for i in 0..e.len().saturating_sub(1) {
        if clock.initial ^ (((i + 1) & 1) as u8) == 1 {
            t.push(e[i]);
            high.push((e[i + 1] - e[i]) as f64);
        }
    }
    let ns = 1e9 / rate;
    let median = |v: &mut Vec<f64>| {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    };
    let r1 = |x: f64| ((x * 10.0).round_ties_even()) / 10.0;
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

/// A claim as the document gives it: in final mode its result (the final layer), in all mode its layers and
/// evidence.
fn claim_doc(c: &wireskein_core::Claim, mode: &str, lines: &[wireskein_io::Line], rate: f64) -> Value {
    let mut out = Map::new();
    out.insert("protocol".into(), json!(c.protocol));
    out.insert("roles".into(), json!(c.roles));
    out.insert("verdict".into(), json!(c.verdict));
    out.insert("score".into(), json!(c.score));
    out.insert("margin".into(), json!(c.margin));
    out.insert("params".into(), Value::Object(c.params.clone()));
    let mut lay: Map<String, Value> = serde_json::from_str(c.layers.get()).unwrap_or_default();
    if let Some(clk) = c.roles.get("clk").and_then(|n| lines.iter().find(|l| &l.name == n)) {
        frame_clocks(&mut lay, &c.protocol, clk, rate);
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
    Value::Object(out)
}

/// The hints of `--hint`: protocol names ("uart,spi"), JSON, or @a file of JSON.
fn hints_of(arg: &str) -> Result<Value, String> {
    let text = match arg.strip_prefix('@') {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?,
        None => arg.to_string(),
    };
    if text.trim_start().starts_with('{') {
        return serde_json::from_str(&text).map_err(|e| format!("--hint: {e}"));
    }
    Ok(json!({"protocols": text.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect::<Vec<_>>()}))
}

/// The document of what analyze finds in a capture file.
pub fn analyze_file(path: &Path, hints: &Value, part: Option<usize>, mode: &str) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cap = wireskein_io::read_bytes(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    let rate = cap.rate();
    let mut capture = Map::new();
    capture.insert("file".into(), json!(path.file_name().map(|s| s.to_string_lossy())));
    capture.insert("sha256".into(), json!(format!("{:x}", Sha256::digest(&bytes))));
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
        let chans: Vec<Channel> = p
            .lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect();
        let view = View::new(rate, p.ticks, chans).map_err(|e| format!("part {}: {e}", p.part))?;
        let (claims, relations) = wireskein_core::analyze_full(&view, hints);
        let mut d = Map::new();
        d.insert("part".into(), json!(p.part));
        d.insert("ticks".into(), json!(p.ticks));
        if let Some((a, b)) = p.parts {
            d.insert("parts".into(), json!([a, b]));
        }
        if cap.kind == wireskein_io::Kind::WireSkein
            && let Some(t0) = p.tick0
        {
            d.insert("tick0".into(), json!(t0));
        } else if cap.kind != wireskein_io::Kind::WireSkein {
            d.insert("tick0".into(), json!(0));
        }
        d.extend(wireskein_io::wsk::part_keys(&p.meta));
        d.insert(
            "claims".into(),
            Value::Array(claims.iter().map(|c| claim_doc(c, mode, &p.lines, rate)).collect()),
        );
        if !relations.is_empty() {
            d.insert("relations".into(), Value::Array(relations));
        }
        if !p.skipped.is_empty() {
            d.insert("skipped_channels".into(), Value::Array(p.skipped.clone()));
        }
        parts.push(Value::Object(d));
    }
    if let Some(k) = part
        && parts.is_empty()
    {
        return Err(format!("{}: part {k} of {}", path.display(), cap.n_parts));
    }
    Ok(
        json!({"format": FORMAT, "wireskein": env!("CARGO_PKG_VERSION"), "hints": hints,
              "capture": capture, "parts": parts}),
    )
}

fn usage() -> ExitCode {
    eprintln!("usage: wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all]");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) != Some("analyze") {
        return usage();
    }
    let (mut file, mut part, mut hints, mut mode) = (None, None, Value::Null, "final".to_string());
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--part" => match it.next().and_then(|v| v.parse().ok()) {
                Some(k) => part = Some(k),
                None => return usage(),
            },
            "--hint" | "--hints" => match it.next().map(|v| hints_of(v)) {
                Some(Ok(h)) => hints = h,
                Some(Err(e)) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
                None => return usage(),
            },
            "--mode" => match it.next() {
                Some(m) if m == "final" || m == "all" => mode = m.clone(),
                _ => return usage(),
            },
            f if !f.starts_with("--") && file.is_none() => file = Some(f.to_string()),
            _ => return usage(),
        }
    }
    let Some(file) = file else { return usage() };
    match analyze_file(Path::new(&file), &hints, part, &mode) {
        Ok(doc) => {
            println!("{doc}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
