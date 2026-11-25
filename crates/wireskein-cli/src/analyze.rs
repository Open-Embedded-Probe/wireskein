//! `analyze` and `decode`: the analysis document of a capture (Python's `wireskein analyze` / `decode`), what each
//! claim or bus read said on stderr (`_warn_undersampled`).

use crate::{Args, Takes, emit, fail, hint, hint_j, names, thresholds, usage_error};
use serde_json::{Value, json};
use std::path::Path;
use std::process::ExitCode;
use wireskein_analysis::split;
use wireskein_io::j::{J, py_str};
use wireskein_io::pyfmt::{g, round_n};

/// A share as Python's `{x:.1%}`.
fn pct(x: f64, prec: usize) -> String {
    format!("{:.*}%", prec, x * 100.0)
}

/// How each bus (decode) or claim (analyze) read, a line each on stderr, and what to do when it did not read well
/// (Python's `cli._warn_undersampled`). order: a bus's pins in the order its spec gave them.
fn warn(items: &[J], order: &dyn Fn(&J) -> Vec<(String, J)>) {
    for x in items {
        let r = x.get("reading");
        let pins = order(x);
        let name = format!(
            "{}({})",
            py_str(x.get("protocol")),
            pins.iter()
                .map(|(k, v)| format!("{k}={}", py_str(v)))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let ru = x.get("roles_unknown");
        if ru.truthy() {
            let join = |v: &J| v.list().iter().map(py_str).collect::<Vec<_>>().join(", ");
            eprintln!(
                "{name}: {} not a role of {} (its roles: {})",
                join(ru.get("given")),
                py_str(x.get("protocol")),
                join(ru.get("roles"))
            );
        }
        if !r.truthy() {
            if x.get("decoded") == &J::Bool(false) {
                eprintln!("{name}: not read");
            }
            continue;
        }
        let mut line = format!(
            "{name}: {} frames read, {} ok",
            py_str(r.get("frames")),
            py_str(r.get("ok"))
        );
        for (k, w) in [
            ("unanswered", "unanswered"),
            ("restored", "with a lost clock restored"),
            ("repaired", "with glitch clocks dropped"),
        ] {
            if r.get(k).truthy() {
                line += &format!(", {} {w}", py_str(r.get(k)));
            }
        }
        if r.has("unread") {
            line += &if r.get("unread").truthy() {
                format!(
                    "; {} stretches unread ({} changes)",
                    py_str(r.get("unread")),
                    py_str(r.get("unread_edges"))
                )
            } else {
                "; nothing unread".to_string()
            };
        }
        eprintln!("{line}");
        let f = |v: &J| v.f().unwrap_or(f64::NAN);
        let us = x.get("undersampled");
        if us.truthy() {
            eprintln!(
                "  speed: {} frames at a clock too fast for {} MS/s (period {} ns, high {} ns, low {} ns): a clock \
                 may fall between samples; capture at {} MS/s at least, {} MS/s best (docs/capturing.md §2)",
                py_str(us.get("frames")),
                g(f(us.get("rate_hz")) / 1e6, 6),
                g(f(us.get("period_ns")), 6),
                g(f(us.get("high_ns")), 6),
                g(f(us.get("low_ns")), 6),
                g(f(us.get("least_rate_hz")) / 1e6, 6),
                g(f(us.get("best_rate_hz")) / 1e6, 6)
            );
            for s in x.get("clock_speeds").list() {
                eprintln!(
                    "    {} MHz (high {} ns, low {} ns): {} frames, ticks {}..{}{}",
                    g(f(s.get("freq_hz")) / 1e6, 3),
                    g(f(s.get("high_ns")), 6),
                    g(f(s.get("low_ns")), 6),
                    py_str(s.get("frames")),
                    py_str(s.get("s")),
                    py_str(s.get("e")),
                    if s.get("rate_ok").truthy() { "" } else { ", too fast" }
                );
            }
        }
        if r.get("same_sample").truthy() {
            eprintln!(
                "  waveform: SDA changed in the same sample as an SCL edge {} times (setup or hold under a sample): \
                 it may read as a START or STOP that is not there; the controller's waveform, or a rate too low for it",
                py_str(r.get("same_sample"))
            );
        }
        let filt = if r.get("glitch_filter_ns").truthy() {
            format!(
                "; the probe's glitch filter of {} ns can lock up or oscillate on slow, noisy edges",
                py_str(r.get("glitch_filter_ns"))
            )
        } else {
            String::new()
        };
        for s in r.get("stopped").list() {
            eprintln!(
                "  line: {} stopped changing at tick {}, while the bus's other lines changed {} times more (an input \
                 stuck or a line come loose{filt})",
                py_str(s.get("line")),
                py_str(s.get("tick")),
                py_str(s.get("others_after"))
            );
        }
        let mut osc: Vec<(String, Vec<&J>)> = vec![];
        for s in r.get("oscillating").list() {
            let l = py_str(s.get("line"));
            match osc.iter_mut().find(|(k, _)| *k == l) {
                Some((_, v)) => v.push(s),
                None => osc.push((l, vec![s])),
            }
        }
        for (line, ss) in &osc {
            let span = if ss.len() == 1 {
                format!("from tick {} to {}", py_str(ss[0].get("s")), py_str(ss[0].get("e")))
            } else {
                format!("{} times, first from tick {}", ss.len(), py_str(ss[0].get("s")))
            };
            let changes: i64 = ss.iter().map(|s| s.get("changes").i().unwrap_or(0)).sum();
            eprintln!("  line: {line} oscillates, {changes} changes a sample or two apart, {span}{filt}");
        }
        if r.get("noisy").truthy() {
            let mut what = vec![];
            if r.get("glitch_share").truthy() {
                what.push(format!(
                    "{} of the clock's changes are glitches",
                    pct(f(r.get("glitch_share")), 1)
                ));
            }
            for (k, v) in r.get("ringing_share").items() {
                if v.truthy() {
                    what.push(format!("{} of {k}'s are ringing", pct(f(v), 1)));
                }
            }
            eprintln!(
                "  noise: {} (read past where it could be; a reflection, a neighbouring line picked up, or a ground \
                 far away: docs/capturing.md §3)",
                what.join(", ")
            );
        }
    }
}

/// A claim's roles (analyze) or a bus's pins (decode), in the document's order.
fn own_order(x: &J) -> Vec<(String, J)> {
    let p = if x.get("pins").truthy() {
        x.get("pins")
    } else {
        x.get("roles")
    };
    p.items().to_vec()
}

fn split_of(cmd: &str, v: Option<&str>) -> Result<Option<split::Options>, ExitCode> {
    match v.map(split::Options::parse) {
        None => Ok(Some(split::Options::default())),
        Some(Ok(o)) => Ok(o),
        Some(Err(e)) => Err(fail(cmd, &e)),
    }
}

/// `wireskein analyze`.
pub fn analyze(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &[
            "--mode",
            "--hint",
            "--depth",
            "--out",
            "--threshold",
            "--segment",
            "--markers",
            "--split",
            "--items",
            "--pins",
            "--part",
        ],
        flags: &["--alternatives", "--events"],
        lists: &[("--select", None), ("--devices", None), ("--window", Some(2))],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("analyze", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("analyze", "give CAPTURE (one)");
    };
    let mode = a.get("--mode").unwrap_or("final");
    if !["final", "all", "select"].contains(&mode) {
        return usage_error(
            "analyze",
            &format!("argument --mode: invalid choice: '{mode}' (choose from 'final', 'all', 'select')"),
        );
    }
    let depth = a.get("--depth").unwrap_or("protocol");
    if !wireskein_analysis::DEPTHS.contains(&depth) {
        return usage_error(
            "analyze",
            &format!(
                "argument --depth: invalid choice: '{depth}' (choose from 'transport', 'frames', 'protocol', 'device')"
            ),
        );
    }
    let part = match a.get("--part").map(|v| v.parse::<usize>()) {
        None => None,
        Some(Ok(k)) => Some(k),
        Some(Err(_)) => return usage_error("analyze", "argument --part: invalid int value"),
    };
    let window = match a.list("--window") {
        None => None,
        Some(w) => match (wireskein_analysis::seconds(&w[0]), wireskein_analysis::seconds(&w[1])) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => return usage_error("analyze", "argument --window: not a time"),
        },
    };
    let mut hints = match hint(a.get("--hint")) {
        Ok(h) => h,
        Err(e) => return fail("analyze", &e),
    };
    // --devices PATTERN ...: the device packs used (Python's `(hints or {}) | {"devices": args.devices}`; none
    // given, every pack)
    let mut hints_doc = hint_j(a.get("--hint"), &hints);
    if let Some(d) = a.list("--devices") {
        match &mut hints {
            Value::Object(m) => {
                m.insert("devices".into(), json!(d));
            }
            Value::Null => hints = json!({ "devices": d }),
            _ => return fail("analyze", "--devices: the hints are not an object"),
        }
        if hints_doc.is_null() {
            hints_doc = J::dict();
        }
        hints_doc.set("devices", wireskein_io::write::j_of(&json!(d)));
    }
    let ths = match thresholds(&a.all("--threshold")) {
        Ok(t) => t,
        Err(e) => return fail("analyze", &e),
    };
    let markers = match a.get("--markers").map(serde_json::from_str::<Value>) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => return fail("analyze", &format!("--markers: {e}")),
    };
    let split_opts = match split_of("analyze", a.get("--split")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let pins = names(a.get("--pins"));
    let select: Vec<String> = a.list("--select").map(|s| s.to_vec()).unwrap_or_default();
    let items = a.get("--items");
    let asked = wireskein_analysis::Asked {
        pins: pins.as_deref(),
        thresholds: &ths,
        items: items.map(|f| (Path::new(f), f)),
        segment: a.get("--segment"),
        markers: markers.as_ref(),
        dialects: None,
        events: a.has("--events"),
        select: &select,
        depth: Some(depth),
        alternatives: a.has("--alternatives"),
    };
    let t0 = std::time::Instant::now();
    let doc = match wireskein_analysis::analyze_file_asked(
        Path::new(file),
        &crate::with_device_dirs(&hints),
        part,
        mode,
        window,
        split_opts.as_ref(),
        &asked,
    ) {
        Ok(d) => d,
        Err(e) => return fail("analyze", &e),
    };
    let mut doc = wireskein_analysis::order::analysis(&doc, Some(&hints_doc));
    doc.set("analysis_seconds", round_n(t0.elapsed().as_secs_f64(), 3));
    let claims: Vec<J> = doc
        .get("parts")
        .list()
        .iter()
        .flat_map(|p| p.get("claims").list().iter().cloned())
        .collect();
    warn(&claims, &own_order);
    match emit(&doc.dumps(), a.get("--out")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("analyze", &e),
    }
}

/// `wireskein decode`: the buses a spec names read with exactly those roles.
pub fn decode(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--spec", "--out", "--split", "--items", "--pins"],
        flags: &[],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("decode", &e),
    };
    // the spec: --spec, or a second positional (as before)
    let (file, spec) = match (a.pos.as_slice(), a.get("--spec")) {
        ([f], Some(s)) => (f, s.to_string()),
        ([f, s], None) => (f, s.clone()),
        _ => return usage_error("decode", "give CAPTURE and --spec JSON|@FILE|FILE"),
    };
    let src = spec.strip_prefix('@').unwrap_or(&spec);
    let text = if src.trim_start().starts_with('{') {
        src.to_string()
    } else {
        match std::fs::read_to_string(src) {
            Ok(t) => t,
            Err(e) => return fail("decode", &format!("[Errno] {e}: {src:?}")),
        }
    };
    let mut spec = match wireskein_analysis::decode::Spec::parse(&text) {
        Ok(v) => v,
        Err(e) => return fail("decode", &e),
    };
    spec.device_dirs = crate::device_dirs().to_vec();
    let split_opts = match split_of("decode", a.get("--split")) {
        Ok(o) => o,
        Err(c) => return c,
    };
    let pins = names(a.get("--pins"));
    let items = a.get("--items").map(|f| (Path::new(f), f));
    let doc = match wireskein_analysis::decode::decode_file(
        Path::new(file),
        &spec,
        split_opts.as_ref(),
        pins.as_deref(),
        items,
    ) {
        Ok(d) => spec.ordered(&d),
        Err(e) => return fail("decode", &e),
    };
    // a bus's pins as its spec gave them (the document's are sorted by role)
    let order = |x: &J| -> Vec<(String, J)> {
        let mine = own_order(x);
        let mut want: Vec<(String, String)> = mine.iter().map(|(k, v)| (k.clone(), py_str(v))).collect();
        want.sort();
        for b in &spec.buses {
            let mut have = b.pins.clone();
            have.sort();
            if b.protocol == py_str(x.get("protocol")) && have == want {
                return b.pins.iter().map(|(k, v)| (k.clone(), J::from(v.as_str()))).collect();
            }
        }
        mine
    };
    let buses: Vec<J> = doc
        .get("parts")
        .list()
        .iter()
        .flat_map(|p| p.get("buses").list().iter().cloned())
        .collect();
    warn(&buses, &order);
    match emit(&doc.dumps(), a.get("--out")) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("decode", &e),
    }
}
