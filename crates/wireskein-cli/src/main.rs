//! `wireskein-rs`: WireSkein's command line in plain Rust (bench/notes/2026-11-24-rust-plan.ja.md §3, R5).
//!
//!     wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all]
//!
//! prints what analyze finds as the analysis format's document (docs/analysis-format.md §4), as `wireskein analyze`
//! does: the capture, then each part (a run of parts that follow on without a gap read as one) with its claims and
//! the relations of its UART pairs. A WireSkein file, a sigrok session (.sr) or a VCD, told apart by content.

mod report;

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

/// A claim as the document gives it: in final mode its result (the final layer), in all mode its layers and
/// evidence.
fn claim_doc(c: &wireskein_core::Claim, mode: &str, lines: &[wireskein_io::Line], rate: f64, n: i64) -> Value {
    let mut out = Map::new();
    out.insert("protocol".into(), json!(c.protocol));
    out.insert("roles".into(), json!(c.roles));
    out.insert("verdict".into(), json!(c.verdict));
    out.insert("score".into(), json!(c.score));
    out.insert("margin".into(), json!(c.margin));
    out.insert("params".into(), Value::Object(c.params.clone()));
    let mut lay: Map<String, Value> = serde_json::from_str(c.layers.get()).unwrap_or_default();
    let notes = note(c, &mut lay, lines, rate, n);
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
        let idle = (report::IDLE_S * rate).round_ties_even() as i64;
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
            Value::Array(
                claims
                    .iter()
                    .map(|c| claim_doc(c, mode, &p.lines, rate, p.ticks))
                    .collect(),
            ),
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
