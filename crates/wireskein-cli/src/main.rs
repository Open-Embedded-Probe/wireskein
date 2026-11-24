//! `wireskein-rs`: WireSkein's command line in plain Rust (bench/notes/2026-11-24-rust-plan.ja.md §3, R5).
//!
//!     wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all]
//!
//! prints what analyze finds as the analysis format's document (docs/analysis-format.md §4), as `wireskein analyze`
//! does: the capture, then each part (a run of parts that follow on without a gap read as one) with its claims and
//! the relations of its UART pairs. A WireSkein file, a sigrok session (.sr) or a VCD, told apart by content.
//!
//!     wireskein-rs verify RUN_DIR [--json FILE] [--junit FILE] [--log] [--allow-unchecked] [-v]
//!
//! checks a recorded run against its expectations as `wireskein verify` does (wireskein-verify): a line a result,
//! the summary, the report as JSON (--json FILE, `-` for stdout), JUnit XML; exit 1 when a check fails.

mod report;
mod split;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::ExitCode;
use wireskein_core::{Channel, View};

const FORMAT: &str = "wireskein-analysis/1";

/// The layer a claim's result gives in final mode, the first of these that has items (Python's `export.FINAL`).
pub(crate) fn final_layers(protocol: &str) -> &'static [&'static str] {
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

pub(crate) fn nonempty(v: &Value) -> bool {
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

pub(crate) fn claim_doc(
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
pub fn analyze_file(
    path: &Path,
    hints: &Value,
    part: Option<usize>,
    mode: &str,
    window_s: Option<(f64, f64)>,
    split_opts: Option<&split::Options>,
) -> Result<Value, String> {
    let at = |e: String| format!("{}: {e}", path.display());
    let splitting = split_opts.filter(|_| window_s.is_none() && part.is_none());
    // a WireSkein file of parts each read a block at a time when it is split (its header only, now); else read whole
    let kind = wireskein_io::sniff_path(path);
    let (cap, streamed) = match (kind, splitting) {
        (Some(wireskein_io::Kind::WireSkein), Some(_)) => {
            let f = std::fs::File::open(path).map_err(|e| at(e.to_string()))?;
            match wireskein_io::wsk::head_from(f).map_err(at)? {
                (c, true) if !cfg!(target_family = "wasm") => (c, true),
                _ => (wireskein_io::read(path)?, false),
            }
        }
        _ => (wireskein_io::read(path)?, false),
    };
    let rate = cap.rate();
    if window_s.is_some() && part.is_none() && cap.parts.len() > 1 {
        return Err(at(format!(
            "{} parts: give the part a window is inside (--part)",
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
            let got = if streamed {
                let mut src = wireskein_io::stream::file_source(path, p.part)
                    .map_err(at)?
                    .ok_or_else(|| at(format!("part {}: not a part to read a block at a time", p.part)))?;
                split::analyze(&mut src, rate, hints, o, mode)?
            } else {
                let mut src = wireskein_io::stream::MemSource::new(&p.lines, p.ticks);
                split::analyze(&mut src, rate, hints, o, mode)?
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
        let lines: &[wireskein_io::Line] = whole.as_deref().unwrap_or(&p.lines);
        let chans: Vec<Channel> = lines
            .iter()
            .map(|l| Channel::new(&l.name, l.initial, &l.edges))
            .collect();
        let view = View::new(rate, p.ticks, chans).map_err(|e| format!("part {}: {e}", p.part))?;
        let (claims, relations) = wireskein_core::analyze_full(&view, hints);
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
        if !p.skipped.is_empty() {
            d.insert("skipped_channels".into(), Value::Array(p.skipped.clone()));
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
fn sha256_of(path: &Path) -> Result<String, String> {
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
fn seconds(t: &str) -> Option<f64> {
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

fn usage() -> ExitCode {
    eprintln!(
        "usage: wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all] [--window FROM TO]"
    );
    eprintln!("       wireskein-rs verify RUN_DIR [--json FILE] [--junit FILE] [--log] [--allow-unchecked] [-v]");
    ExitCode::from(2)
}

/// `wireskein verify`: the lines, the summary, the report and JUnit when asked; 1 when the run fails.
fn verify_cmd(args: &[String]) -> ExitCode {
    let (mut run, mut json_out, mut junit_out) = (None, None, None);
    let (mut log, mut allow, mut verbose) = (false, false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => match it.next() {
                Some(f) => json_out = Some(f.clone()),
                None => return usage(),
            },
            "--junit" => match it.next() {
                Some(f) => junit_out = Some(f.clone()),
                None => return usage(),
            },
            "--log" => log = true,
            "--allow-unchecked" => allow = true,
            "-v" | "--verbose" => verbose = true,
            f if !f.starts_with('-') && run.is_none() => run = Some(f.to_string()),
            _ => return usage(),
        }
    }
    let Some(run) = run else { return usage() };
    let mut rep = match wireskein_verify::verify(Path::new(&run)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if !log {
        rep.remove("log");
    }
    for line in wireskein_verify::lines(&rep, true, verbose) {
        println!("{line}");
    }
    println!("{}", wireskein_verify::summary_line(&rep));
    let write = |f: &str, text: String| -> Result<(), String> {
        if f == "-" {
            println!("{text}");
            Ok(())
        } else {
            std::fs::write(f, text).map_err(|e| format!("{f}: {e}"))
        }
    };
    if let Some(f) = json_out
        && let Err(e) = write(&f, rep.dumps())
    {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    if let Some(f) = junit_out
        && let Err(e) = write(&f, wireskein_verify::junit(&rep, allow))
    {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    if wireskein_verify::failed(&rep, allow) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("verify") {
        return verify_cmd(&args[1..]);
    }
    if args.first().map(|s| s.as_str()) != Some("analyze") {
        return usage();
    }
    let (mut file, mut part, mut hints, mut mode) = (None, None, Value::Null, "final".to_string());
    let mut window = None;
    let mut split_opts = Some(split::Options::default());
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
            "--window" => match (it.next().and_then(|v| seconds(v)), it.next().and_then(|v| seconds(v))) {
                (Some(a), Some(b)) => window = Some((a, b)),
                _ => return usage(),
            },
            "--split" => match it.next().map(|v| split::Options::parse(v)) {
                Some(Ok(o)) => split_opts = o,
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
    match analyze_file(Path::new(&file), &hints, part, &mode, window, split_opts.as_ref()) {
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
