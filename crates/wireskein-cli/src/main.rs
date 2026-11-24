//! `wireskein-rs`: WireSkein's command line in plain Rust (bench/notes/2026-11-24-rust-plan.ja.md §3, R5).
//!
//!     wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all] [--window FROM TO]
//!                  [--split S|off] [--pins A,B] [--threshold NAME=V|NAME=LOW,HIGH ...] [--items FILE]
//!                  [--segment PATH [--markers JSON] [--events]] [--out FILE]
//!
//! prints what analyze finds as the analysis format's document (docs/analysis-format.md §4), as `wireskein analyze`
//! does: the capture, then each part (a run of parts that follow on without a gap read as one) with its claims and
//! the relations of its UART pairs. A WireSkein file, a sigrok session (.sr) or a VCD, told apart by content.
//! --pins: only these channels; --threshold: an analog channel also read as logic (repeatable); --items: a part read
//! in pieces writes its items to FILE as they are read; --segment: only the items inside a marker segment (the
//! heading dialect; --markers {"pin", "dialect"} names the marker source, --events adds its text lines).
//!
//!     wireskein-rs verify RUN_DIR [--json FILE] [--junit FILE] [--log] [--allow-unchecked] [--mark] [-v]
//!
//! checks a recorded run against its expectations as `wireskein verify` does (wireskein-verify): a line a result,
//! the summary, the report as JSON (--json FILE, `-` for stdout), JUnit XML; exit 1 when a check fails. --mark writes
//! what the checks found into each capture file's markers.
//!
//!     wireskein-rs decode FILE SPEC [--split S|off] [--pins A,B] [--items FILE]
//!
//! decodes the buses a spec (JSON, or @a file of it) names with exactly those roles, as `wireskein decode` does
//! (wireskein-analysis's decode).

use serde_json::Value;
use std::path::Path;
use std::process::ExitCode;
use wireskein_analysis::{analyze_file_asked, hints_of, seconds, split};

fn usage() -> ExitCode {
    eprintln!(
        "usage: wireskein-rs analyze FILE [--part K] [--hint NAMES|JSON|@FILE] [--mode final|all] [--window FROM TO]"
    );
    eprintln!(
        "                            [--split S|off] [--pins A,B] [--threshold NAME=V|NAME=LOW,HIGH ...] [--items FILE]"
    );
    eprintln!("                            [--segment PATH [--markers JSON] [--events]] [--out FILE]");
    eprintln!(
        "       wireskein-rs verify RUN_DIR [--json FILE] [--junit FILE] [--log] [--allow-unchecked] [--mark] [-v]"
    );
    eprintln!("       wireskein-rs decode FILE SPEC [--split S|off] [--pins A,B] [--items FILE]");
    ExitCode::from(2)
}

/// `wireskein decode`: the decode document of a capture file with a spec's roles.
fn decode_cmd(args: &[String]) -> ExitCode {
    let (mut file, mut spec, mut pins, mut items) = (None, None, None, None);
    let mut split_opts = Some(split::Options::default());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--split" => match it.next().map(|v| split::Options::parse(v)) {
                Some(Ok(o)) => split_opts = o,
                Some(Err(e)) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
                None => return usage(),
            },
            "--pins" => match it.next() {
                Some(v) => pins = Some(names(v)),
                None => return usage(),
            },
            "--items" => match it.next() {
                Some(v) => items = Some(v.clone()),
                None => return usage(),
            },
            f if !f.starts_with("--") && file.is_none() => file = Some(f.to_string()),
            f if !f.starts_with("--") && spec.is_none() => spec = Some(f.to_string()),
            _ => return usage(),
        }
    }
    let (Some(file), Some(spec)) = (file, spec) else {
        return usage();
    };
    let text = match spec.strip_prefix('@') {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("{p}: {e}");
                return ExitCode::from(2);
            }
        },
        None => spec,
    };
    let spec = match wireskein_analysis::decode::Spec::parse(&text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let items = items.as_deref().map(|f| (Path::new(f), f));
    match wireskein_analysis::decode::decode_file(Path::new(&file), &spec, split_opts.as_ref(), pins.as_deref(), items)
    {
        Ok(doc) => {
            println!("{doc}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wireskein-rs decode: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `wireskein verify`: the lines, the summary, the report and JUnit when asked; 1 when the run fails.
fn verify_cmd(args: &[String]) -> ExitCode {
    let (mut run, mut json_out, mut junit_out) = (None, None, None);
    let (mut log, mut allow, mut verbose, mut mark) = (false, false, false, false);
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
            "--mark" => mark = true,
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
    if mark && let Err(e) = wireskein_verify::mark(Path::new(&run), &rep) {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
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
    if args.first().map(|s| s.as_str()) == Some("decode") {
        return decode_cmd(&args[1..]);
    }
    if args.first().map(|s| s.as_str()) != Some("analyze") {
        return usage();
    }
    let (mut file, mut part, mut hints, mut mode) = (None, None, Value::Null, "final".to_string());
    let mut window = None;
    let mut split_opts = Some(split::Options::default());
    let (mut pins, mut thresholds, mut items, mut out) = (None, vec![], None, None);
    let (mut segment, mut markers, mut events) = (None, None, false);
    let mut it = args[1..].iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--pins" => match it.next() {
                Some(v) => pins = Some(names(v)),
                None => return usage(),
            },
            "--threshold" => match it.next().map(|v| v.split_once('=')) {
                Some(Some((name, v))) => match wireskein_analysis::analog::threshold_of(v) {
                    Ok((lo, hi)) => thresholds.push((name.to_string(), lo, hi)),
                    Err(e) => {
                        eprintln!("--threshold: {e}");
                        return ExitCode::from(2);
                    }
                },
                _ => return usage(),
            },
            "--items" => match it.next() {
                Some(v) => items = Some(v.clone()),
                None => return usage(),
            },
            "--out" => match it.next() {
                Some(v) => out = Some(v.clone()),
                None => return usage(),
            },
            "--segment" => match it.next() {
                Some(v) => segment = Some(v.clone()),
                None => return usage(),
            },
            "--markers" => match it.next().map(|v| serde_json::from_str::<Value>(v)) {
                Some(Ok(v)) => markers = Some(v),
                Some(Err(e)) => {
                    eprintln!("--markers: {e}");
                    return ExitCode::from(2);
                }
                None => return usage(),
            },
            "--events" => events = true,
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
    let asked = wireskein_analysis::Asked {
        pins: pins.as_deref(),
        thresholds: &thresholds,
        items: items.as_deref().map(|f| (Path::new(f), f)),
        segment: segment.as_deref(),
        markers: markers.as_ref(),
        dialects: None,
        events,
    };
    match analyze_file_asked(
        Path::new(&file),
        &hints,
        part,
        &mode,
        window,
        split_opts.as_ref(),
        &asked,
    ) {
        Ok(doc) => {
            let text = doc.to_string();
            match out {
                Some(f) => {
                    if let Err(e) = std::fs::write(&f, &text) {
                        eprintln!("{f}: {e}");
                        return ExitCode::from(2);
                    }
                    eprintln!("{} bytes -> {f}", text.len());
                }
                None => println!("{text}"),
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("wireskein-rs analyze: {e}");
            ExitCode::FAILURE
        }
    }
}

/// "A,B" -> ["A", "B"] (blanks dropped).
fn names(v: &str) -> Vec<String> {
    v.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}
