//! `survey`, `states`, `annotate`, `align` and `segments` (Python's `wireskein survey` / `states` / `annotate` /
//! `align` / `segments`): views of a capture made without a spec.

use crate::{Args, Takes, emit, fail, hint, names, thresholds, usage_error};
use serde_json::{Map, Value};
use std::path::Path;
use std::process::ExitCode;
use wireskein_analysis::{align, split, states};
use wireskein_io::j::{J, py_str};
use wireskein_io::jd;
use wireskein_io::markers::{dumps_indent, put, read_header};
use wireskein_io::pyfmt::g;

/// A share as Python's `{x:W.P%}`.
fn pct(x: f64, prec: usize) -> String {
    format!("{:.*}%", prec, x * 100.0)
}

/// The overview as lines to read (Python's `survey.text`).
fn survey_text(doc: &J) -> Vec<String> {
    let rate = doc.get("rate_hz").f().unwrap_or(f64::NAN);
    let ticks = doc.get("ticks").f().unwrap_or(0.0);
    let mut out = vec![format!(
        "{}: {} ticks at {} Hz ({} s)",
        py_str(doc.get("capture").get("file")),
        py_str(doc.get("ticks")),
        g(rate, 6),
        g(ticks / rate, 6)
    )];
    for ln in doc.get("lines").list() {
        let act = ln.get("activity").list();
        let busy = if act.is_empty() {
            0.0
        } else {
            act.iter().filter(|v| v.f().unwrap_or(0.0) > 0.0).count() as f64 / act.len() as f64
        };
        let mut s = format!(
            "  {:<12} {:>12} changes, active in {:>6} of the time",
            py_str(ln.get("name")),
            py_str(ln.get("edges")),
            pct(busy, 1)
        );
        let fr = ln.get("free_running");
        if fr.truthy() {
            s += &format!(", free-running {} Hz", g(fr.get("freq_hz").f().unwrap_or(f64::NAN), 6));
            if fr.has("duty") {
                s += &format!(" duty {:.3}", fr.get("duty").f().unwrap_or(f64::NAN));
            }
        }
        let short = ln.get("short_share").f().unwrap_or(0.0);
        if short > 0.3 {
            s += &format!(", trembling ({} of intervals 1-2 ticks)", pct(short, 0));
        }
        let still = ln.get("longest_still");
        if still.truthy() {
            s += &format!(
                ", longest still {} s",
                g(still.list()[0].get("seconds").f().unwrap_or(f64::NAN), 6)
            );
        }
        out.push(s);
    }
    let all = doc.get("all_still");
    if all.truthy() {
        let st = &all.list()[0];
        out.push(format!(
            "  every line still at most {} s (from tick {})",
            g(st.get("seconds").f().unwrap_or(f64::NAN), 6),
            py_str(st.get("s"))
        ));
    }
    if doc.has("pieces") {
        let mut c: Vec<(String, usize)> = vec![];
        for p in doc.get("pieces").list() {
            let k = py_str(p.get("cut"));
            match c.iter_mut().find(|(x, _)| *x == k) {
                Some((_, n)) => *n += 1,
                None => c.push((k, 1)),
            }
        }
        out.push(format!(
            "  pieces: {} ({})",
            doc.get("pieces").list().len(),
            c.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
        ));
    }
    out
}

/// `wireskein survey`.
pub fn survey(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--pins", "--bins", "--top", "--split", "--out"],
        flags: &["--plan", "--json"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("survey", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("survey", "give CAPTURE (one)");
    };
    let int = |k: &str, d: usize| -> Result<usize, ExitCode> {
        a.get(k).map_or(Ok(d), |v| {
            v.parse()
                .map_err(|_| usage_error("survey", &format!("argument {k}: invalid int value: '{v}'")))
        })
    };
    let (bins, top) = match (int("--bins", 1000), int("--top", 10)) {
        (Ok(b), Ok(t)) => (b, t),
        (Err(c), _) | (_, Err(c)) => return c,
    };
    // --plan: the pieces of the split (its defaults, or --split's)
    let plan = if a.has("--plan") {
        match a.get("--split").map(split::Options::parse) {
            None => Some(split::Options::default()),
            Some(Ok(o)) => o,
            Some(Err(e)) => return fail("survey", &e),
        }
    } else {
        None
    };
    let pins = names(a.get("--pins"));
    let mut doc =
        match wireskein_analysis::survey::survey_file(Path::new(file), pins.as_deref(), bins, top, plan.as_ref()) {
            Ok(d) => d,
            Err(e) => return fail("survey", &e),
        };
    let cap = doc.remove("capture").unwrap_or_default();
    doc.set("wireskein", crate::VERSION);
    doc.set("capture", cap);
    let text = doc.dumps();
    if let Some(f) = a.get("--out")
        && let Err(e) = emit(&text, Some(f))
    {
        return fail("survey", &e);
    }
    if a.has("--json") {
        println!("{text}");
    } else {
        println!("{}", survey_text(&doc).join("\n"));
    }
    ExitCode::SUCCESS
}

/// One line per stretch and odd pulse, times in ms (Python's `states.text`).
fn states_text(doc: &J) -> Vec<String> {
    let hz = doc.get("tick_hz").f().unwrap_or(f64::NAN);
    let ms = |v: &J| v.f().unwrap_or(f64::NAN) / hz * 1e3;
    let f = |v: &J| v.f().unwrap_or(f64::NAN);
    let mut out = vec![];
    let groups = if doc.has("parts") {
        doc.get("parts")
    } else {
        doc.get("sections")
    };
    let several = groups.list().len() > 1;
    for p in groups.list() {
        if p.has("path") {
            let tail = if p.get("label").truthy() {
                py_str(p.get("label"))
            } else {
                py_str(p.get("why"))
            };
            out.push(format!("== {}  ({tail})", py_str(p.get("path"))));
            if !p.has("lines") {
                continue;
            }
        } else if several {
            out.push(format!(
                "part {}: {} s",
                py_str(p.get("part")),
                g(p.get("ticks").f().unwrap_or(0.0) / hz, 6)
            ));
        }
        for (pin, xs) in p.get("lines").items() {
            out.push(format!("{pin}:"));
            for x in xs.list() {
                if x.has("activity") {
                    out.push(format!(
                        "  {:12.3} - {:.3} ms  activity: {} pulses, no wave",
                        ms(x.get("t0")),
                        ms(x.get("t1")),
                        py_str(x.get("activity"))
                    ));
                } else if x.has("odd") {
                    let mut s = format!(
                        "  {:12.3} ms  odd pulse: high {:.1} us",
                        ms(x.get("odd")),
                        f(x.get("high_s")) * 1e6
                    );
                    if !x.get("period_s").is_null() {
                        s += &format!(", period {:.1} us", f(x.get("period_s")) * 1e6);
                    }
                    out.push(s);
                } else if x.has("level") {
                    out.push(format!(
                        "  {:12.3} - {:.3} ms  level {}",
                        ms(x.get("t0")),
                        ms(x.get("t1")),
                        py_str(x.get("level"))
                    ));
                } else {
                    out.push(format!(
                        "  {:12.3} - {:.3} ms  {} Hz, high {:.1} us (duty {:.4}), {} periods",
                        ms(x.get("t0")),
                        ms(x.get("t1")),
                        g(f(x.get("freq_hz")), 6),
                        f(x.get("high_s")) * 1e6,
                        f(x.get("duty")),
                        py_str(x.get("periods"))
                    ));
                }
            }
        }
    }
    out
}

/// {pin: [stretches]} of a capture's lines (Python's `states.of_capture`).
fn of_lines<'a>(
    lines: impl Iterator<Item = (&'a str, u8, &'a [i64], i64)>,
    n: i64,
    rate: f64,
    pins: &[&str],
    o: &states::Options,
) -> J {
    let mut out = J::dict();
    for (name, initial, edges, step) in lines {
        if pins.is_empty() || pins.contains(&name) {
            out.set(name, states::of_line(edges, initial, n, rate, step, o));
        }
    }
    out
}

/// Python's `fnmatch.fnmatchcase` of a section path with "[" literal (the repeat index).
fn section_matches(path: &str, pattern: &str) -> bool {
    wireskein_analysis::fnmatch(path, &pattern.replace('[', "[[]"))
}

/// `wireskein states`.
pub fn states(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--section", "--pin", "--part", "--tol", "--min-periods", "--max-odd"],
        flags: &["--json"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("states", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("states", "give FILE or RUN_DIR (one)");
    };
    macro_rules! num {
        ($k:expr, $d:expr, $t:ty) => {
            match a.get($k).map(|v| v.parse::<$t>()) {
                None => $d,
                Some(Ok(x)) => x,
                Some(Err(_)) => return usage_error("states", &format!("argument {}: invalid value", $k)),
            }
        };
    }
    let o = states::Options {
        tol: num!("--tol", 0.02, f64),
        min_periods: num!("--min-periods", 3, usize),
        max_odd: Some(num!("--max-odd", 3, usize)).filter(|m| *m != 0),
    };
    let part: Option<usize> = match a.get("--part").map(|v| v.parse::<usize>()) {
        None => None,
        Some(Ok(k)) => Some(k),
        Some(Err(_)) => return usage_error("states", "argument --part: invalid int value"),
    };
    let pins: Vec<&str> = a.all("--pin");
    let path = Path::new(file);
    let doc = if a.get("--section").is_some() || path.join("run.json").exists() {
        let pattern = a.get("--section").unwrap_or("*");
        let secs = match wireskein_verify::report::sections(path) {
            Ok(s) => s,
            Err(e) => return fail("states", &e),
        };
        let mut out = vec![];
        let mut hz = J::Null;
        for s in secs {
            if !section_matches(&s.path, pattern) {
                continue;
            }
            let Some(cap) = &s.cap else {
                out.push(jd! {"path" => s.path.as_str(), "why" => s.why.as_str()});
                continue;
            };
            hz = J::Float(cap.rate);
            let lines = cap
                .channels
                .iter()
                .map(|c| (c.name.as_str(), c.initial, c.edges.as_slice(), c.step));
            out.push(
                jd! {"path" => s.path.as_str(), "label" => s.label.clone(), "window" => s.window.clone(),
                "part" => s.part.clone(), "lines" => of_lines(lines, cap.n, cap.rate, &pins, &o)},
            );
        }
        jd! {"run" => wireskein_verify::report::run_name(path), "tick_hz" => hz, "sections" => J::List(out)}
    } else {
        let cap = match wireskein_io::read(path) {
            Ok(c) => c,
            Err(e) => return fail("states", &e),
        };
        let rate = cap.rate();
        let mut parts = vec![];
        for (k, p) in cap.parts.iter().enumerate() {
            if part.is_some_and(|q| q != k) {
                continue;
            }
            let lines = p
                .lines
                .iter()
                .map(|l| (l.name.as_str(), l.initial, l.edges.as_slice(), l.step));
            parts.push(jd! {"part" => k, "ticks" => p.ticks, "lines" => of_lines(lines, p.ticks, rate, &pins, &o)});
        }
        jd! {"file" => wireskein_verify::report::run_name(path),
        "tick_hz" => if cap.parts.is_empty() { J::Null } else { J::Float(rate) },
        "parts" => J::List(parts)}
    };
    if a.has("--json") {
        println!("{}", dumps_indent(&doc));
    } else {
        println!("{}", states_text(&doc).join("\n"));
    }
    ExitCode::SUCCESS
}

/// `wireskein annotate`.
pub fn annotate(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--hint", "--threshold"],
        flags: &["--save"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("annotate", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("annotate", "give FILE (one)");
    };
    let hints = match hint(a.get("--hint")) {
        Ok(h) => h,
        Err(e) => return fail("annotate", &e),
    };
    let ths = match thresholds(&a.all("--threshold")) {
        Ok(t) => t,
        Err(e) => return fail("annotate", &e),
    };
    let path = Path::new(file);
    let source = format!("wireskein {} analyze", crate::VERSION);
    let doc = match wireskein_analysis::annotate::annotations_file_asked(path, &hints, None, &source, &ths) {
        Ok(d) => d,
        Err(e) => return fail("annotate", &e),
    };
    let rows = doc.get("rows").list();
    for r in rows {
        let items = r.get("items").list();
        let bad = items.iter().filter(|i| i.get("level").s() != Some("ok")).count();
        let part = if r.has("part") {
            format!("part {}: ", r.get("part").i().unwrap_or(0) + 1)
        } else {
            String::new()
        };
        let near = if r.has("near") {
            py_str(r.get("near"))
        } else {
            "-".into()
        };
        println!(
            "{part}{:24} under {near:8} {:6} items{}",
            py_str(r.get("name")),
            items.len(),
            if bad > 0 {
                format!(", {bad} not ok")
            } else {
                String::new()
            }
        );
    }
    if rows.is_empty() {
        println!("nothing decoded");
    }
    if a.has("--save") {
        if let Err(e) = put(path, wireskein_analysis::annotate::ENTRY, doc.dumps().as_bytes()) {
            return fail("annotate", &e);
        }
        println!("saved as {}", wireskein_analysis::annotate::ENTRY);
    }
    ExitCode::SUCCESS
}

const ALIGN_FORMAT: &str = "wireskein-alignment/1";
const ALIGN_NAME: &str = "alignment.json";
/// The entries' keys in Python's order (align.find, align.between).
const ENTRY_KEYS: [&str; 16] = [
    "offset_ticks",
    "scale",
    "reference",
    "via",
    "method",
    "matched",
    "edges",
    "overlap_edges",
    "overlap_s",
    "residual_ticks",
    "scale_ppm",
    "scale_ppm_uncertainty",
    "threshold_v",
    "window_ticks",
    "offset_us",
    "start_shift_us",
];

/// An entry of the Rust align as Python's (its keys in Python's order, "Infinity" a float again).
fn entry_of(m: &Map<String, Value>, between: bool) -> J {
    let order: Vec<&str> = if between {
        vec![
            "offset_ticks",
            "scale",
            "reference",
            "via",
            "method",
            "matched",
            "edges",
            "overlap_edges",
            "residual_ticks",
            "scale_ppm",
            "scale_ppm_uncertainty",
            "offset_us",
            "threshold_v",
        ]
    } else {
        vec![
            "offset_ticks",
            "scale",
            "reference",
            "via",
            "method",
            "matched",
            "edges",
            "overlap_edges",
            "overlap_s",
            "residual_ticks",
            "scale_ppm_uncertainty",
            "threshold_v",
            "window_ticks",
            "offset_us",
            "scale_ppm",
            "start_shift_us",
        ]
    };
    let mut out = J::dict();
    let keys = order
        .iter()
        .map(|k| k.to_string())
        .chain(m.keys().filter(|k| !ENTRY_KEYS.contains(&k.as_str())).cloned());
    for k in keys {
        if let Some(v) = m.get(&k) {
            let j = match v {
                Value::String(s) if s == "Infinity" => J::Float(f64::INFINITY),
                v => wireskein_io::write::j_of(v),
            };
            out.set(&k, j);
        }
    }
    out
}

/// The alignment stored in a WireSkein file, or None (Python's `align.load`).
fn align_load(path: &Path) -> Result<Option<J>, String> {
    let got = wireskein_io::markers::entries(path, &format!("attach/{ALIGN_NAME}"))?;
    let Some((_, d)) = got.into_iter().find(|(n, _)| *n == format!("attach/{ALIGN_NAME}")) else {
        return Ok(None);
    };
    if d.is_empty() {
        return Ok(None);
    }
    let doc = J::parse(&String::from_utf8_lossy(&d))?;
    if doc.get("format").s() != Some(ALIGN_FORMAT) {
        return Err(format!(
            "{}: alignment format {}, this version reads {}",
            path.display(),
            doc.get("format").repr(),
            J::from(ALIGN_FORMAT).repr()
        ));
    }
    Ok(Some(doc))
}

/// Store it as attach/alignment.json (Python's `align.save`: the alignments to other files kept).
fn align_save(path: &Path, mut doc: J) -> Result<(), String> {
    if let Some(old) = align_load(path)?
        && old.get("files").truthy()
        && !doc.has("files")
    {
        doc.set("files", old.get("files").clone());
    }
    put(
        path,
        &format!("attach/{ALIGN_NAME}"),
        ascii(&dumps_indent(&doc)).as_bytes(),
    )
}

/// JSON text as Python's json.dumps writes it by default (ensure_ascii).
fn ascii(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut b = [0u16; 2];
            for u in c.encode_utf16(&mut b) {
                out += &format!("\\u{u:04x}");
            }
        }
    }
    out
}

fn one_part(path: &Path) -> Result<(wireskein_io::Capture, usize), String> {
    let cap = wireskein_io::read(path)?;
    if cap.parts.len() != 1 {
        return Err(format!(
            "{}: {} parts ({}): read one with part=, or all with read_parts()",
            path.display(),
            cap.n_parts,
            cap.layout
        ));
    }
    Ok((cap, 0))
}

/// `wireskein align`.
pub fn align(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &[
            "--to",
            "--reference",
            "--via",
            "--threshold",
            "--max-ppm",
            "--max-offset",
            "--apply-to",
        ],
        flags: &["--save"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("align", &e),
    };
    let ([file], Some(reference), Some(via)) = (a.pos.as_slice(), a.get("--reference"), a.get("--via")) else {
        return usage_error("align", "give FILE, --reference and --via");
    };
    let max_ppm = match a.get("--max-ppm").map(|v| v.parse::<f64>()) {
        None => 200.0,
        Some(Ok(x)) => x,
        Some(Err(_)) => return usage_error("align", "argument --max-ppm: invalid float value"),
    };
    match align_file(Path::new(file), reference, via, &a, max_ppm) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("align", &e),
    }
}

fn align_file(path: &Path, reference: &str, via: &str, a: &Args, max_ppm: f64) -> Result<(), String> {
    let (cap, k) = one_part(path)?;
    let p = &cap.parts[k];
    let th = match a.get("--threshold") {
        Some(t) => Some(wireskein_analysis::analog::threshold_of(t)?),
        None => None,
    };
    let offset = |rate: f64| -> Result<Option<f64>, String> {
        match a.get("--max-offset") {
            None => Ok(None),
            Some(t) => Ok(Some(
                wireskein_analysis::seconds(t).ok_or_else(|| format!("not a time: {}", J::from(t).repr()))? * rate,
            )),
        }
    };
    if let Some(to) = a.get("--to") {
        // onto another file's ticks (spec §5.1.1)
        let to = Path::new(to);
        let (rcap, rk) = one_part(to)?;
        let rp = &rcap.parts[rk];
        let window = offset(rcap.rate())?;
        let Some(rl) = rp.lines.iter().find(|l| l.name == reference) else {
            if rp.analog.iter().any(|x| x.name == reference) {
                return Err(format!("{reference} is analog: give a threshold"));
            }
            return Err(format!("{reference}: no such channel"));
        };
        let rf = align::logic_edges(&rl.edges, rl.initial, rl.step);
        let (t, thv) = match p.lines.iter().find(|l| l.name == via) {
            Some(l) => (align::logic_edges(&l.edges, l.initial, l.step), None),
            None => {
                let tr = p
                    .analog
                    .iter()
                    .find(|x| x.name == via)
                    .ok_or_else(|| format!("{via}: no such channel"))?;
                let (lo, hi) = th.ok_or_else(|| format!("{via} is analog: give a threshold"))?;
                (align::analog_edges(tr, cap.tick_hz, lo, hi)?, Some((lo, hi)))
            }
        };
        let e = align::between(
            &rf,
            reference,
            &t,
            via,
            rcap.rate(),
            cap.rate(),
            rp.ticks,
            window,
            max_ppm,
            thv,
        )?;
        let e = entry_of(&e, true);
        let f = |k: &str| e.get(k).f().unwrap_or(f64::NAN);
        let to_name = to
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        println!(
            "{} onto {to_name}: tick 0 at {:+.3} us, clock {:+.2} +- {:.2} ppm, {}/{} edges matched, residual {:.3} us",
            path.display(),
            f("offset_us"),
            f("scale_ppm"),
            f("scale_ppm_uncertainty"),
            py_str(e.get("matched")),
            py_str(e.get("overlap_edges")),
            f("residual_ticks") / rcap.rate() * 1e6
        );
        if a.has("--save") {
            let id = read_header(to)?.get("id").clone();
            if !id.truthy() {
                return Err(format!("{}: its capture has no id", to.display()));
            }
            let mut doc = align_load(path)?.unwrap_or_else(|| jd! {"format" => ALIGN_FORMAT, "channels" => J::dict()});
            let mut x = jd! {"capture_id" => id};
            for (k, v) in e.items() {
                x.set(k, v.clone());
            }
            if !doc.has("files") {
                doc.set("files", J::dict());
            }
            doc.get_mut("files").unwrap().set(&to_name, x);
            align_save(path, doc)?;
            println!("saved in attach/{ALIGN_NAME} (files: {to_name})");
        }
        return Ok(());
    }
    let (lo, hi) = th.ok_or("give --threshold (the analog channel is read as logic)")?;
    let window = offset(cap.rate())?;
    let rl = p
        .lines
        .iter()
        .find(|l| l.name == reference)
        .ok_or_else(|| J::from(reference).repr())?;
    let tr = p
        .analog
        .iter()
        .find(|x| x.name == via)
        .ok_or_else(|| format!("{via}: not an analog channel in this capture"))?;
    let names: Vec<String> = match a.get("--apply-to") {
        Some(s) => s.split(',').map(String::from).collect(),
        None => p.analog.iter().map(|x| x.name.clone()).collect(),
    };
    let missing: Vec<&str> = names
        .iter()
        .map(|n| n.as_str())
        .filter(|n| !p.analog.iter().any(|x| x.name == *n))
        .collect();
    let tick = cap.rate();
    let ns: Vec<f64> = [
        p.meta.get("start_uncertainty_ns"),
        tr.acquisition.get("start_uncertainty_ns"),
    ]
    .into_iter()
    .flatten()
    .filter_map(|v| v.as_f64())
    .collect();
    let unc = (!ns.is_empty()).then(|| ns.iter().sum::<f64>() * 1e-9 * tick);
    let rf = align::logic_edges(&rl.edges, rl.initial, rl.step);
    let e = align::find(&rf, reference, tr, cap.tick_hz, lo, hi, window, unc, p.ticks, 5000.0)?;
    if !missing.is_empty() {
        return Err(format!("not analog channels here: {}", missing.join(", ")));
    }
    let c = entry_of(&e, false);
    let f = |k: &str| c.get(k).f().unwrap_or(f64::NAN);
    println!(
        "{via} against {reference}: start {:+.3} us, scale {:+.1} +- {:.1} ppm, {}/{} edges matched, residual {:.3} us",
        f("start_shift_us"),
        f("scale_ppm"),
        f("scale_ppm_uncertainty"),
        py_str(c.get("matched")),
        py_str(c.get("overlap_edges")),
        f("residual_ticks") / tick * 1e6
    );
    if f("overlap_edges") < 0.9 * f("edges") {
        println!(
            "note: {reference} has edges over {} ms only; {} of the {} edges of {via} lie outside it (the scale comes \
             from that span)",
            g(f("overlap_s") * 1e3, 3),
            c.get("edges").i().unwrap_or(0) - c.get("overlap_edges").i().unwrap_or(0),
            py_str(c.get("edges"))
        );
    }
    println!("applies to: {}", names.join(", "));
    if a.has("--save") {
        let mut chans = J::dict();
        for n in &names {
            chans.set(n, c.clone());
        }
        align_save(path, jd! {"format" => ALIGN_FORMAT, "channels" => chans})?;
        println!("saved as attach/{ALIGN_NAME}");
    }
    Ok(())
}

/// `wireskein segments`.
pub fn segments(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--hint", "--threshold", "--markers", "--out"],
        flags: &["--results", "--events"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("segments", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("segments", "give CAPTURE (one)");
    };
    let hints = match hint(a.get("--hint")) {
        Ok(h) => h,
        Err(e) => return fail("segments", &e),
    };
    let ths = match thresholds(&a.all("--threshold")) {
        Ok(t) => t,
        Err(e) => return fail("segments", &e),
    };
    let markers = match a.get("--markers").map(serde_json::from_str::<Value>) {
        None => None,
        Some(Ok(v)) => Some(v),
        Some(Err(e)) => return fail("segments", &format!("--markers: {e}")),
    };
    let got = wireskein_analysis::segments::segments_file(
        Path::new(file),
        &hints,
        &ths,
        markers.as_ref(),
        None,
        a.has("--results"),
        a.has("--events"),
    );
    match got {
        Ok(Some(doc)) => match emit(&doc.dumps(), a.get("--out")) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => fail("segments", &e),
        },
        Ok(None) => {
            eprintln!("no marker stream found (give --markers)");
            ExitCode::FAILURE
        }
        Err(e) => fail("segments", &e),
    }
}
