//! `info` and `convert`: what a WireSkein file holds, a capture written in another format (Python's `wireskein
//! info` / `convert`).

use crate::{Args, Takes, fail, names, usage_error};
use serde_json::{Value, json};
use std::path::Path;
use std::process::ExitCode;
use wireskein_io::j::{J, py_str};
use wireskein_io::markers::{entries, extras, read_header};
use wireskein_io::pyfmt::g;
use wireskein_io::{Capture, Kind, Line, Part};

/// Python's `str(Fraction(n, d))`.
fn frac(n: i64, d: i64) -> String {
    let (n, d) = wireskein_io::fraction(n, d);
    if d == 1 { format!("{n}") } else { format!("{n}/{d}") }
}

fn ratio(v: &J) -> f64 {
    match v.list() {
        [n, d] => n.f().unwrap_or(f64::NAN) / d.f().unwrap_or(f64::NAN),
        _ => v.f().unwrap_or(f64::NAN),
    }
}

/// INCOMPLETE lines for a capture's losses (Python's `cli._loss_lines`).
fn loss_lines(raw: &J) -> Vec<String> {
    const SHOWN: usize = 3;
    let lost = raw.get("lost").list();
    let ticks: i64 = lost.iter().map(|x| x.get("ticks").i().unwrap_or(0)).sum();
    let taken: i64 = raw
        .get("parts")
        .list()
        .iter()
        .map(|p| p.get("ticks").i().unwrap_or(0))
        .sum();
    let mut head = format!(
        "INCOMPLETE: {} loss{}",
        lost.len(),
        if lost.len() != 1 { "es" } else { "" }
    );
    if ticks != 0 {
        head += &format!(
            ", {ticks} ticks lost against {taken} taken ({} %)",
            g(ticks as f64 / (ticks + taken) as f64 * 100.0, 2)
        );
    }
    let mut out = vec![head];
    let mut shown: Vec<&J> = lost.iter().take(SHOWN).collect();
    if lost.len() > SHOWN {
        shown.push(&lost[lost.len() - 1]);
    }
    for x in shown {
        let mut s = format!(
            "  after part {}: {}",
            py_str(x.get("after_part")),
            py_str(x.get("reason"))
        );
        if x.get("ticks").truthy() {
            s += &format!(" ({} ticks)", py_str(x.get("ticks")));
        }
        if x.get("stopped").truthy() {
            s += ", stopped there";
        }
        out.push(s);
    }
    if lost.len() > SHOWN + 1 {
        out.insert(1 + SHOWN, format!("  ... {} more", lost.len() - SHOWN - 1));
    }
    out
}

/// What the meta keys with a set meaning say (Python's `cli._meta_notes`).
fn meta_notes(meta: &J, tick: f64) -> Vec<String> {
    let mut out = vec![];
    if meta.get("time_base_slipped").truthy() {
        out.push(
            "time_base_slipped: the probe knows some samples were taken late (its sampling fell behind, e.g. at its \
             buffer limit), so times in this capture may be stretched there; verify still decides, and a failed check \
             says the probe reported a slip"
                .to_string(),
        );
    }
    if let J::Int(t) = meta.get("trigger_tick") {
        out.push(format!(
            "trigger_tick {t}: the trigger at tick {t} ({:.3} us from the start)",
            *t as f64 / tick * 1e6
        ));
    }
    if let J::Int(_) | J::Float(_) | J::Bool(_) = meta.get("start_uncertainty_ns") {
        out.push(format!(
            "start_ns is the probe clock of the first sample, +- {} us",
            g(meta.get("start_uncertainty_ns").f().unwrap_or(0.0) / 1000.0, 6)
        ));
    }
    out
}

const ENCODINGS: [&str; 7] = [
    "bits",
    "events",
    "analog",
    "analog-f32",
    "any_high",
    "any_low",
    "change_latch",
];

/// One part's channels (Python's `cli._info_part`).
fn info_part(raw: &J, k: usize, p: Option<&Part>, tick: f64, many: bool) {
    let rp = &raw.get("parts").list()[k];
    let chans = rp.get("channels").list();
    for c in chans {
        let enc = c.get("encoding").s().unwrap_or("bits");
        if !ENCODINGS.contains(&enc) {
            continue;
        }
        let name = py_str(c.get("name"));
        let n = c.get("n").i().unwrap_or(0);
        match enc {
            "analog" | "analog-f32" => {
                let unit = c.get("unit").s().unwrap_or("V").to_string();
                let (zero, scale) = (c.get("zero").f(), c.get("scale_nv").f());
                let vb = c.get("value_bits").i().filter(|v| *v != 0);
                let conv = if enc == "analog" {
                    format!(
                        "raw {}-bit{}{}",
                        c.get("width").i().unwrap_or(16),
                        vb.map_or(String::new(), |v| format!(" ({v} valid)")),
                        match (zero, scale) {
                            (Some(z), Some(s)) => format!(", {unit} = (raw - {}) x {} n{unit}", g(z, 6), g(s, 6)),
                            _ => ", no volt conversion".into(),
                        }
                    )
                } else {
                    format!("float32 {unit}")
                };
                println!(
                    "  {name:12} {n:>12} samples  {enc:10} {} Hz from tick {}  {conv}",
                    g(ratio(c.get("rate_hz")), 6),
                    g(ratio(c.get("t0_ticks")), 6)
                );
                if enc == "analog"
                    && let (Some(vb), Some(z), Some(s)) = (vb, zero, scale)
                {
                    let top = ((1i64 << vb) - 1) as f64;
                    let vals: &[f64] = p
                        .and_then(|p| p.analog.iter().find(|a| a.name == name))
                        .map_or(&[], |a| a.values.as_slice());
                    let (mut lo, mut hi) = (
                        vals.iter().filter(|v| **v <= 0.0).count(),
                        vals.iter().filter(|v| **v >= top).count(),
                    );
                    let (mut lo_v, mut hi_v) = (-z * s * 1e-9, (top - z) * s * 1e-9);
                    if s < 0.0 {
                        (lo, hi, lo_v, hi_v) = (hi, lo, hi_v, lo_v);
                    }
                    let mut line = format!("  {:12} range {lo_v:.3}..{hi_v:.3} {unit}", "");
                    if lo > 0 || hi > 0 {
                        line += &format!(": {lo} samples clipped at <= {lo_v:.3}, {hi} at >= {hi_v:.3} (not voltages)");
                    }
                    println!("{line}");
                }
            }
            "any_high" | "any_low" | "change_latch" => {
                let mut extra = String::new();
                if enc == "change_latch" {
                    let counts: &[u8] = p
                        .and_then(|p| p.events.iter().find(|e| e.name == name))
                        .map_or(&[], |e| e.counts.as_slice());
                    let back = counts.iter().filter(|c| **c > 1).count();
                    extra = format!(", {} changed", counts.len());
                    if back > 0 {
                        extra += &format!(", {back} and came back");
                    }
                }
                println!(
                    "  {name:12} {n:>12} intervals {enc} at {} ticks an interval from tick {}{extra}",
                    py_str(c.get("step")),
                    py_str(c.get("phase"))
                );
            }
            "events" => {
                let step = c.get("step").i().unwrap_or(1);
                let hidden = p
                    .and_then(|p| p.events.iter().find(|e| e.name == name))
                    .map_or(0, |e| e.hidden());
                println!(
                    "  {name:12} {n:>12} quanta   events {} at {step} ticks a quantum from tick {}{}{}",
                    py_str(c.get("events")),
                    py_str(c.get("phase")),
                    if hidden > 0 {
                        format!(" ({hidden} quanta hold more than one change)")
                    } else {
                        String::new()
                    },
                    if step > 1 { "" } else { " (lossless)" }
                );
            }
            _ => {
                let step = c.get("step").i().unwrap_or(1);
                let phase = c.get("phase").i().unwrap_or(0);
                println!(
                    "  {name:12} {n:>12} samples  step {step:<4} phase {phase:<4} {} Hz",
                    g(tick / step as f64, 6)
                );
            }
        }
        let acq = c.get("acquisition");
        if acq.truthy() {
            println!("  {:12} acquisition: {}", "", acq.dumps_py());
        }
    }
    let mut seen: Vec<String> = vec![];
    for c in chans {
        let enc = c.get("encoding");
        let name = py_str(c.get("name"));
        if !enc.s().is_some_and(|e| ENCODINGS.contains(&e)) && !seen.contains(&name) {
            println!("  {name:12} encoding {}: not read by this version", enc.repr());
            seen.push(name);
        }
    }
    let pmeta = rp.get("meta");
    if pmeta.truthy() && !many {
        println!("part meta: {}", pmeta.dumps_py());
    }
    let mut meta = raw.get("meta").clone();
    if !meta.truthy() {
        meta = J::dict();
    }
    for (key, v) in pmeta.items() {
        meta.set(key, v.clone());
    }
    for line in meta_notes(&meta, tick) {
        println!("  {line}");
    }
}

/// `wireskein info`.
pub fn info(args: &[String]) -> ExitCode {
    let a = match Args::parse(
        args,
        &Takes {
            values: &[],
            flags: &[],
            lists: &[],
        },
    ) {
        Ok(a) => a,
        Err(e) => return usage_error("info", &e),
    };
    let [file] = a.pos.as_slice() else {
        return usage_error("info", "give FILE (one)");
    };
    let path = Path::new(file);
    let raw = match read_header(path) {
        Ok(r) => r,
        Err(e) => return fail("info", &e),
    };
    let data = match wireskein_io::read_parts(path, None) {
        Ok(c) => c,
        Err(e) => return fail("info", &e),
    };
    let th = raw.get("tick_hz").list();
    let (tn, td) = (
        th.first().and_then(J::i).unwrap_or(1),
        th.get(1).and_then(J::i).unwrap_or(1),
    );
    let tick = tn as f64 / td as f64;
    let parts = raw.get("parts").list();
    let many = parts.len() > 1;
    let total: i64 = if raw.get("layout").s() == Some("timeline") {
        parts
            .last()
            .map_or(0, |p| p.get("tick0").i().unwrap_or(0) + p.get("ticks").i().unwrap_or(0))
    } else {
        parts.iter().map(|p| p.get("ticks").i().unwrap_or(0)).sum()
    };
    let id = if raw.has("id") {
        py_str(raw.get("id"))
    } else {
        "-".into()
    };
    println!(
        "{file}: {}, tick {} Hz ({}), {total} ticks ({} s), id {id}{}",
        wireskein_io::wsk::FORMAT,
        g(tick, 6),
        frac(tn, td),
        g(total as f64 / tick, 6),
        if many {
            format!(", {} parts ({})", parts.len(), py_str(raw.get("layout")))
        } else {
            String::new()
        }
    );
    if raw.get("complete") == &J::Bool(false) {
        for line in loss_lines(&raw) {
            println!("{line}");
        }
    }
    if raw.get("meta").truthy() {
        println!("meta: {}", raw.get("meta").dumps_py());
    }
    for (k, rp) in parts.iter().enumerate() {
        if many {
            let pm = rp.get("meta");
            println!(
                "part {k}: {} ticks{}{}",
                py_str(rp.get("ticks")),
                if rp.has("tick0") {
                    format!(" from tick {}", py_str(rp.get("tick0")))
                } else {
                    String::new()
                },
                if pm.truthy() {
                    format!(", meta {}", pm.dumps_py())
                } else {
                    String::new()
                }
            );
        }
        info_part(&raw, k, data.parts.iter().find(|p| p.part == k), tick, many);
    }
    let attach = match entries(path, "attach/") {
        Ok(e) => e,
        Err(e) => return fail("info", &e),
    };
    for (name, d) in attach {
        println!("{name}: {} bytes", d.len());
    }
    let mut notes = match entries(path, "notes/") {
        Ok(e) => e,
        Err(e) => return fail("info", &e),
    };
    notes.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, (_, d)) in notes.iter().enumerate() {
        let n = J::parse(&String::from_utf8_lossy(d)).unwrap_or_default();
        let extra = J::Dict(
            n.items()
                .iter()
                .filter(|(x, _)| x != "time" && x != "content")
                .cloned()
                .collect(),
        );
        let body = match n.get("content") {
            J::Str(s) => s.clone(),
            c => c.dumps_py(),
        };
        println!(
            "note {} {}{}: {body}",
            k + 1,
            py_str(n.get("time")),
            if extra.truthy() {
                format!(" {}", extra.dumps_py())
            } else {
                String::new()
            }
        );
    }
    ExitCode::SUCCESS
}

/// Ticks [s0, s1) of a line, tick 0 at s0 (Python's `verify._window`).
fn window_line(l: &Line, s0: i64, s1: i64) -> Line {
    Line {
        initial: l.initial ^ (l.edges.partition_point(|&e| e <= s0) as u8 & 1),
        edges: l.edges.iter().filter(|&&e| e > s0 && e < s1).map(|&e| e - s0).collect(),
        phase: (l.phase - s0).rem_euclid(l.step.max(1)),
        ..l.clone()
    }
}

/// Ticks [s0, s1) of a line kept as changes (Python's `verify._window`).
fn window_events(t: &wireskein_io::Events, s0: i64, s1: i64) -> wireskein_io::Events {
    let k0 = 0.max(-((t.phase - s0).div_euclid(t.step)));
    let k1 = k0.max(t.n.min((s1 - t.phase).div_euclid(t.step)));
    let before = t.quanta.partition_point(|&q| q < k0);
    let sel: Vec<usize> = (0..t.quanta.len())
        .filter(|&i| t.quanta[i] >= k0 && t.quanta[i] < k1)
        .collect();
    wireskein_io::Events {
        phase: t.phase + k0 * t.step - s0,
        n: k1 - k0,
        start_level: if before > 0 {
            t.levels[before - 1]
        } else {
            t.start_level
        },
        quanta: sel.iter().map(|&i| t.quanta[i] - k0).collect(),
        levels: sel.iter().map(|&i| t.levels[i]).collect(),
        counts: sel.iter().map(|&i| t.counts[i]).collect(),
        ..t.clone()
    }
}

/// The logic channels, as Python's `cli._count` says them.
fn count(p: &Part) -> String {
    let kept: Vec<&str> = p.events.iter().map(|e| e.name.as_str()).collect();
    let logic = p.lines.iter().filter(|l| !kept.contains(&l.name.as_str())).count() + p.events.len();
    let mut s = format!("{logic} logic");
    if !p.events.is_empty() {
        s += &format!(" ({} as changes at a coarser step)", p.events.len());
    }
    if !p.analog.is_empty() {
        s += &format!(" + {} analog", p.analog.len());
    }
    s + " channels"
}

/// `wireskein convert IN OUT`.
pub fn convert(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--from", "--to", "--pins", "--part"],
        flags: &[],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("convert", &e),
    };
    let [input, output] = a.pos.as_slice() else {
        return usage_error("convert", "give IN and OUT");
    };
    let num = |k: &str| -> Result<Option<f64>, ExitCode> {
        match a.get(k).map(|v| v.parse::<f64>()) {
            None => Ok(None),
            Some(Ok(x)) => Ok(Some(x)),
            Some(Err(_)) => Err(usage_error("convert", &format!("argument {k}: invalid float value"))),
        }
    };
    let (t0, t1) = match (num("--from"), num("--to")) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(c), _) | (_, Err(c)) => return c,
    };
    let part = match a.get("--part").map(|v| v.parse::<usize>()) {
        None => None,
        Some(Ok(k)) => Some(k),
        Some(Err(_)) => return usage_error("convert", "argument --part: invalid int value"),
    };
    match convert_file(
        Path::new(input),
        Path::new(output),
        t0,
        t1,
        names(a.get("--pins")),
        part,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail("convert", &e),
    }
}

fn convert_file(
    input: &Path,
    output: &Path,
    t0: Option<f64>,
    t1: Option<f64>,
    pins: Option<Vec<String>>,
    part: Option<usize>,
) -> Result<(), String> {
    let kind = wireskein_io::sniff_path(input);
    if part.is_none() && kind == Some(Kind::WireSkein) {
        let head = read_header(input)?;
        let n = head.get("parts").list().len();
        if n > 1 {
            return Err(format!(
                "{} has {n} parts ({}): --part K writes one of them (from 0); a .sr or a VCD holds one gapless run",
                input.display(),
                py_str(head.get("layout"))
            ));
        }
    }
    let mut cap: Capture = match part {
        Some(k) => {
            let c = wireskein_io::read_parts(input, Some(k))?;
            if c.parts.is_empty() {
                return Err(format!("{}: part {k} of {}", input.display(), c.n_parts));
            }
            c
        }
        None => wireskein_io::read(input)?,
    };
    let rate = cap.rate();
    let n_parts = cap.n_parts;
    let p = &mut cap.parts[0];
    if let Some(pins) = &pins {
        let have: Vec<&str> = p
            .lines
            .iter()
            .map(|l| l.name.as_str())
            .chain(p.analog.iter().map(|a| a.name.as_str()))
            .chain(p.events.iter().map(|e| e.name.as_str()))
            .collect();
        let miss: Vec<&str> = pins.iter().map(|x| x.as_str()).filter(|x| !have.contains(x)).collect();
        if !miss.is_empty() {
            return Err(format!("pins not in the capture: {}", miss.join(", ")));
        }
        p.lines.retain(|l| pins.contains(&l.name));
        p.analog.retain(|x| pins.contains(&x.name));
        p.events.retain(|x| pins.contains(&x.name));
        p.skipped
            .retain(|x| x["name"].as_str().is_some_and(|n| pins.iter().any(|q| q == n)));
    }
    let excerpt = t0.is_some() || t1.is_some();
    // a logic-only .sr written as a WireSkein file goes straight through in Python (`srio.convert_sr`), said shortly
    let ext = output
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    let straight = kind == Some(Kind::Sr)
        && !matches!(ext.as_deref(), Some("sr" | "vcd"))
        && p.analog.is_empty()
        && p.lines.iter().all(|l| l.step == 1 && l.phase == 0);
    if excerpt {
        let s0 = (t0.unwrap_or(0.0) * rate).round_ties_even() as i64;
        let s1 = t1.map_or(p.ticks, |t| p.ticks.min((t * rate).round_ties_even() as i64));
        if !(0 <= s0 && s0 < s1) {
            return Err(format!("--from / --to: no ticks between them in {} ticks", p.ticks));
        }
        p.lines = p.lines.iter().map(|l| window_line(l, s0, s1)).collect();
        p.events = p.events.iter().map(|t| window_events(t, s0, s1)).collect();
        p.analog = p
            .analog
            .iter()
            .map(|x| wireskein_verify::analog::window(x, cap.tick_hz, s0, s1))
            .collect();
        p.ticks = s1 - s0;
        for meta in [&mut p.meta, &mut cap.meta] {
            meta.insert("excerpt".into(), json!({"from_tick": s0, "to_tick": s1}));
            if let Some(ns) = meta.get("start_ns").and_then(|v| v.as_i64()) {
                // the probe clock of the excerpt's own first sample: s0 / tick_hz in ns, rounded half to even
                let (num, den) = (cap.tick_hz.0 as i128, cap.tick_hz.1 as i128);
                let (q, r) = (
                    (s0 as i128 * den * 1_000_000_000).div_euclid(num),
                    (s0 as i128 * den * 1_000_000_000).rem_euclid(num),
                );
                let up = 2 * r > num || (2 * r == num && q % 2 == 1);
                meta.insert("start_ns".into(), json!(ns + (q + up as i128) as i64));
            }
            if let Some(t) = meta.get("trigger_tick").and_then(|v| v.as_i64()) {
                meta.insert("trigger_tick".into(), json!(t - s0));
            }
        }
    }
    for k in [
        "file",
        "sr_file",
        "vcd_file",
        "tick_hz",
        "unitsize",
        "extras",
        "skipped_channels",
        "vcd_not_read",
    ] {
        cap.meta.remove(k);
    }
    let not_read: Vec<Value> = cap.parts[0]
        .meta
        .get("vcd_not_read")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if let Some(k) = part
        && n_parts > 1
    {
        let lost = if cap.complete {
            vec![]
        } else {
            eprintln!(
                "note: {} lost data (a failed capture); part {k} is written on its own and stays incomplete",
                input.display()
            );
            vec![json!({"after_part": -1, "stopped": false,
                        "reason": format!("part {k} of a capture that lost data ({} losses in all; meta.from_part)",
                                          cap.lost.len())})]
        };
        cap.meta.insert(
            "from_part".into(),
            json!({"part": k, "of": n_parts, "layout": cap.layout, "complete": cap.complete}),
        );
        cap.complete = lost.is_empty();
        cap.lost = lost;
        cap.layout = "timeline".into();
        cap.n_parts = 1;
        let p = &mut cap.parts[0];
        p.part = 0;
        p.parts = None;
        p.tick0 = None;
    }
    if kind == Some(Kind::WireSkein) && matches!(ext.as_deref(), Some("sr" | "vcd")) {
        // a .sr / VCD keeps the capture's facts in wireskein's own data, as Python's load gives them in meta
        let p = &cap.parts[0];
        for k in wireskein_io::write::PART_META {
            if let Some(v) = p.meta.get(k) {
                cap.meta.insert(k.into(), v.clone());
            }
        }
        cap.meta.insert("complete".into(), json!(cap.complete));
        cap.meta.insert("lost".into(), Value::Array(cap.lost.clone()));
        if part.is_none() || n_parts <= 1 {
            cap.meta.insert("layout".into(), json!(cap.layout));
            cap.meta.insert("part".into(), json!(p.part));
            cap.meta.insert("n_parts".into(), json!(cap.n_parts));
        }
    }
    let ex = extras(input)?;
    let ex: Vec<(String, Vec<u8>)> = if matches!(ext.as_deref(), Some("vcd")) {
        vec![]
    } else {
        ex
    };
    let n_extras = extras(input)?.len();
    wireskein_io::write::write(output, &cap, &ex)?;
    let p = &cap.parts[0];
    if straight {
        println!(
            "{} -> {}: {} logic channels, {} ticks at {} Hz",
            input.display(),
            output.display(),
            p.lines.len(),
            p.ticks,
            g(rate, 6)
        );
        return Ok(());
    }
    let slow: Vec<String> = p
        .lines
        .iter()
        .filter(|l| l.step != 1)
        .map(|l| format!("{}/{}", l.name, l.step))
        .collect();
    println!(
        "{} -> {}: {}, {} ticks at {} Hz{}",
        input.display(),
        output.display(),
        count(p),
        p.ticks,
        g(rate, 6),
        if slow.is_empty() {
            String::new()
        } else {
            format!(", decimated: {}", slow.join(", "))
        }
    );
    for c in &not_read {
        println!(
            "note: {} not read ({})",
            c["name"].as_str().unwrap_or(""),
            c["encoding"].as_str().unwrap_or("")
        );
    }
    if matches!(ext.as_deref(), Some("vcd")) && n_extras > 0 {
        println!("note: a VCD has no place for attachments and notes: {n_extras} entries not carried");
    }
    Ok(())
}
