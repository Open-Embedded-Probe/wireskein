//! `capture`: logic and analog channels from a device into a WireSkein file (Python's `wireskein capture --source
//! KIND:ADDRESS`, `cli.capture_cmd`), the kind choosing the driver (wireskein-sources: `oep`, `demo`). An OEP probe
//! opened by an OEP address on USB (oep://UNIT_ID[?via=usb|bulk|hid|cdc]: vendor bulk, HID, its CDC port), DNS-SD
//! (oep://UNIT_ID not on USB, ?via=net), TCP (oep://UNIT_ID?via=tcp:HOST:PORT, tcp://HOST:PORT) or a serial port
//! (/dev/..., oep://UNIT_ID?via=tty:PATH; its link raised as --fast asks, by default as the record of the port and
//! probe says); its lines by the probe's channel names or their labels (S.N, S@P). What the Rust client does not do
//! (sigrok, a broker) is refused with a pointer to `wireskein capture`. `--list` lists the devices that can be seen.

use crate::{Args, Takes, usage_error};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wireskein_capture::{Acquired, Error, Mode, Request};
use wireskein_io::j::J;

const LOSS_LINES: usize = 3;
const BUFFER_WARN_MS: f64 = 5.0;

/// --fast (Python's sources.parse_fast): absent or "1": None (the client's record decides); "0": none (the boot
/// speed); "1500000,921600": those rates, in that order.
pub fn parse_fast(text: Option<&str>) -> Result<Option<Vec<u32>>, String> {
    let Some(t) = text else { return Ok(None) };
    let low = t.to_ascii_lowercase();
    if ["1", "yes", "on", "true"].contains(&low.as_str()) {
        return Ok(None);
    }
    if ["", "0", "no", "off", "false"].contains(&low.as_str()) {
        return Ok(Some(vec![]));
    }
    t.split(',')
        .map(|r| r.trim().parse::<u32>())
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
        .map_err(|_| format!("ValueError: --fast {t}: 0, 1, or link rates (1500000,921600)"))
}

/// "20M", "250k", "1000" -> a count (only k / K, M and G).
pub fn parse_count(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let (num, suf) = match t.char_indices().find(|(_, c)| c.is_ascii_alphabetic()) {
        Some((i, _)) => (t[..i].trim(), t[i..].trim()),
        None => (t, ""),
    };
    let mul = match suf {
        "" => 1.0,
        "k" | "K" => 1e3,
        "M" => 1e6,
        "G" => 1e9,
        _ => {
            let hint = if suf == "m" { " (M for mega)" } else { "" };
            return Err(format!(
                "not a count: {}{hint}; use a number with k, M or G",
                wireskein_io::j::py_str_repr(text)
            ));
        }
    };
    let v: f64 = num
        .parse()
        .ok()
        .filter(|_| !num.is_empty() && num.chars().all(|c| c.is_ascii_digit() || c == '.'))
        .ok_or_else(|| {
            format!(
                "not a count: {}; use a number with k, M or G",
                wireskein_io::j::py_str_repr(text)
            )
        })?;
    Ok(wireskein_io::pyfmt::round(v * mul) as u64)
}

/// "300us", "1.5ms", "2s", "0.001" -> seconds.
fn seconds(text: &str) -> Result<f64, String> {
    let t = text.trim();
    let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
    let (num, unit) = (t[..split].trim(), t[split..].trim());
    let mul = match unit {
        "ns" => 1e-9,
        "us" | "µs" => 1e-6,
        "ms" => 1e-3,
        "s" | "" => 1.0,
        _ => return Err(format!("not a time: {}", wireskein_io::j::py_str_repr(text))),
    };
    num.parse::<f64>()
        .map(|v| v * mul)
        .map_err(|_| format!("not a time: {}", wireskein_io::j::py_str_repr(text)))
}

/// "SDA=47,SCL=48" or "D0,D1", a resolution after the id: "CS=5/any_low:8". -> (name, id), name -> (mode, D)
#[allow(clippy::type_complexity)]
fn parse_channels(text: &str) -> Result<(Vec<(String, String)>, Vec<(String, (String, u64))>), String> {
    let (mut chans, mut res) = (vec![], vec![]);
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, cid) = part.split_once('=').map(|(a, b)| (a.trim(), b)).unwrap_or((part, part));
        let (cid, how) = cid.split_once('/').unwrap_or((cid, ""));
        chans.push((name.to_string(), cid.trim().to_string()));
        let how = how.trim();
        if !how.is_empty() {
            let (mode, d) = how.split_once(':').unwrap_or((how, ""));
            let ok = wireskein_capture::request::REDUCTIONS
                .contains(&mode)
                .then(|| parse_count(d).ok())
                .flatten();
            let Some(d) = ok else {
                return Err(format!(
                    "{name}=.../{how}: the resolution is MODE:D, MODE one of {}, D samples (CS=5/any_low:8)",
                    wireskein_capture::request::REDUCTIONS.join(", ")
                ));
            };
            res.push((name.to_string(), (mode.to_string(), d)));
        }
    }
    Ok((chans, res))
}

fn err_text(e: &Error) -> String {
    match e {
        Error::Value(s) | Error::NotFound(s) => s.clone(),
        Error::Timeout(s) => format!("TimeoutError: {s}"),
        Error::Io(s) => format!("OSError: {s}"),
        e => format!("RuntimeError: {e}"),
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("wireskein-rs capture: {msg}");
    ExitCode::FAILURE
}

/// `wireskein-rs capture --source oep:ADDRESS --channels ... --rate R (--samples N | --mode stream --duration T) -o OUT`
pub fn capture(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &[
            "--source",
            "--fast",
            "--channels",
            "--analog",
            "--analog-rate",
            "--analog-samples",
            "--rate",
            "--samples",
            "--trigger",
            "--mode",
            "--segments",
            "--duration",
            "--glitch-filter",
            "--on-loss",
            "--pretrigger",
            "--arm",
            "--timeout",
            "--threshold",
            "--note",
            "-o",
            "--output",
        ],
        flags: &["--list"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("capture", &e),
    };
    if !a.pos.is_empty() {
        return usage_error("capture", &format!("unrecognized arguments: {}", a.pos.join(" ")));
    }
    if a.has("--list") {
        return list(a.get("--source"));
    }
    let Some(source) = a.get("--source") else {
        return usage_error("capture", "the following arguments are required: --source, -o/--output");
    };
    let Some(output) = a.get("--output").or(a.get("-o")) else {
        return usage_error("capture", "the following arguments are required: -o/--output");
    };
    for (opt, choices) in [
        ("--mode", &["oneshot", "stream", "repeat"][..]),
        ("--on-loss", &["stop", "continue"]),
        ("--arm", &["pretrigger", "now"]),
    ] {
        if let Some(v) = a.get(opt)
            && !choices.contains(&v)
        {
            let quoted: Vec<String> = choices.iter().map(|c| format!("'{c}'")).collect();
            return usage_error(
                "capture",
                &format!(
                    "argument {opt}: invalid choice: '{v}' (choose from {})",
                    quoted.join(", ")
                ),
            );
        }
    }
    let registry = wireskein_sources::builtin();
    let (kind, address) = match wireskein_capture::source::split(source).and_then(|(k, a)| {
        registry.driver(k)?;
        Ok((k, a))
    }) {
        Ok(x) => x,
        Err(e) => {
            return fail(&format!(
                "{source}: {}; other sources (sigrok) are `wireskein capture`'s",
                err_text(&e)
            ));
        }
    };
    if kind != "oep" {
        return match run(&a, source, &registry, Path::new(output)) {
            Ok(code) => code,
            Err(e) => fail(&e),
        };
    }
    let _ = address;
    match run(&a, source, &registry, Path::new(output)) {
        Ok(code) => code,
        Err(e) => fail(&e),
    }
}

/// The devices that can be seen, of every kind or of `--source KIND`, one a line: its source, then its label.
fn list(kind: Option<&str>) -> ExitCode {
    let registry = wireskein_sources::builtin();
    let kind = kind.map(|k| k.split(':').next().unwrap_or(k));
    match registry.list(kind) {
        Ok((found, failed)) => {
            for f in found {
                println!("{}\t{}", f.source, f.label);
            }
            for (k, e) in failed {
                eprintln!("{k}: {}", err_text(&e));
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&err_text(&e)),
    }
}

fn run(a: &Args, source: &str, registry: &wireskein_capture::Registry, out: &Path) -> Result<ExitCode, String> {
    let (chans, resolution) = parse_channels(a.get("--channels").unwrap_or(""))?;
    let logic = !chans.is_empty();
    let mode = a.get("--mode").unwrap_or("oneshot");
    let stream = mode == "stream";
    // analog only: its own rate / samples do
    let rate = a.get("--rate").or(if logic { None } else { a.get("--analog-rate") });
    let samples = a
        .get("--samples")
        .or(if logic { None } else { a.get("--analog-samples") });
    if stream && (rate.is_none() || a.get("--duration").is_none()) {
        return Err("--mode stream: give --rate and --duration".into());
    }
    if !stream && (rate.is_none() || samples.is_none()) {
        return Err(if logic {
            "give --rate and --samples".into()
        } else {
            "give --analog-rate (or --rate) and --analog-samples (or --samples)".into()
        });
    }
    let mut req = Request::new(
        chans,
        parse_count(rate.unwrap_or("0"))?,
        samples.map(parse_count).transpose()?.unwrap_or(0),
    );
    req.mode = Mode::parse(mode).map_err(|e| err_text(&e))?;
    req.fast = parse_fast(a.get("--fast"))?;
    req.duration_s = a.get("--duration").map(seconds).transpose()?;
    req.on_loss_stop = a.get("--on-loss").unwrap_or("stop") == "stop";
    req.segments = a
        .get("--segments")
        .map(|s| {
            s.parse::<u64>()
                .map_err(|_| format!("argument --segments: invalid int value: '{s}'"))
        })
        .transpose()?;
    if let Some(text) = a.get("--analog") {
        let (list, _) = parse_channels(text)?;
        for (name, id) in list {
            let (id, fe) = id.split_once('@').unwrap_or((&id, ""));
            let fe = if fe.is_empty() {
                None
            } else {
                Some(
                    fe.parse::<u8>()
                        .map_err(|_| format!("--analog {text}: @FRONTEND is a number"))?,
                )
            };
            req.analog.push((name.clone(), id.to_string(), fe));
        }
    }
    req.analog_rate_hz = a.get("--analog-rate").map(parse_count).transpose()?;
    req.analog_samples = a.get("--analog-samples").map(parse_count).transpose()?;
    if let Some(t) = a.get("--trigger") {
        let parts: Vec<&str> = t.split(':').collect();
        if parts.len() == 3 {
            let v = parts[2].trim_end_matches(['V', 'v']).parse::<f64>();
            let Ok(v) = v else {
                return Err(format!("trigger {t}: NAME:KIND:VOLTS (VDD:fall:2.9)"));
            };
            req.trigger = Some((parts[0].to_string(), parts[1].to_string()));
            req.trigger_volts = Some(v);
        } else {
            let (name, kind) = t.rsplit_once(':').unwrap_or(("", t));
            req.trigger = Some((name.to_string(), kind.to_string()));
        }
    }
    req.pretrigger = a.get("--pretrigger").map(parse_count).transpose()?;
    req.arm_now = a.get("--arm") == Some("now");
    req.timeout_s = a
        .get("--timeout")
        .map(|s| {
            s.parse::<f64>()
                .map_err(|_| format!("argument --timeout: invalid float value: '{s}'"))
        })
        .transpose()?
        .unwrap_or(10.0);
    req.resolution = resolution;
    req.threshold_v = a
        .get("--threshold")
        .map(|s| {
            s.trim_end_matches(['V', 'v'])
                .parse::<f64>()
                .map_err(|_| format!("argument --threshold: invalid volts: '{s}'"))
        })
        .transpose()?;
    req.glitch_filter_ns = a
        .get("--glitch-filter")
        .map(|s| {
            s.parse::<u32>()
                .map_err(|_| format!("argument --glitch-filter: invalid int value: '{s}'"))
        })
        .transpose()?;
    if stream {
        let full = std::path::absolute(out).map_err(|e| e.to_string())?;
        req.spool_dir = full.parent().map(PathBuf::from);
    }
    let got = registry
        .capture(
            source,
            &req,
            &wireskein_capture::Open::new("wireskein capture"),
            &wireskein_capture::Ctl::new(),
        )
        .map_err(|e| err_text(&e))?;
    let mut extra = Map::new();
    extra.insert("source".into(), json!(source));
    extra.insert("requested_rate".into(), json!(req.rate_hz));
    let written = wireskein_capture::to_capture(&got, &extra)
        .map_err(|e| err_text(&e))
        .and_then(|cap| wireskein_io::write::write(out, &cap, &[]));
    if let Some(d) = &got.temp_dir {
        let _ = std::fs::remove_dir_all(d);
    }
    written?;
    let wsk = std::fs::read(out)
        .ok()
        .and_then(|b| wireskein_io::sniff(&b))
        .is_some_and(|k| k == wireskein_io::Kind::WireSkein);
    if let Some(n) = a.get("--note")
        && wsk
    {
        wireskein_io::attach::note(out, &J::from(n), &[]).map_err(|e| e.to_string())?;
    }
    Ok(say(out, &req, &got, wsk))
}

fn num(v: &J) -> f64 {
    v.f().unwrap_or(0.0)
}

/// What was taken, how the link went, the losses (Python's capture_cmd lines). Exit 1 when data was lost.
fn say(out: &Path, req: &Request, got: &Acquired, wsk: bool) -> ExitCode {
    let shown = out.display();
    let rate = got.tick_hz.0 as f64 / got.tick_hz.1 as f64;
    let parts = got.all_parts();
    let (what, n_parts, total) = if wsk && let Ok(raw) = wireskein_io::markers::read_header(out) {
        let ps = raw.get("parts").list();
        let chans = ps
            .first()
            .map(|p| p.get("channels").list().to_vec())
            .unwrap_or_default();
        let coarse = chans.iter().filter(|c| c.get("encoding").s() == Some("events")).count();
        let n_analog = chans
            .iter()
            .filter(|c| c.get("encoding").s().is_some_and(|e| e.starts_with("analog")))
            .count();
        let what = format!(
            "{} logic{}{} channels",
            chans.len() - n_analog,
            if coarse > 0 {
                format!(" ({coarse} as changes at a coarser step)")
            } else {
                String::new()
            },
            if n_analog > 0 {
                format!(" + {n_analog} analog")
            } else {
                String::new()
            }
        );
        let total: i64 = ps.iter().map(|p| p.get("ticks").i().unwrap_or(0)).sum();
        (what, ps.len(), total)
    } else {
        let n = parts.first().map(|p| p.channels.len()).unwrap_or(0);
        let total = parts.iter().map(|p| p.samples.unwrap_or(0) as i64).sum();
        (format!("{n} logic channels"), parts.len(), total)
    };
    let g = |x: f64| wireskein_capture::fmt_g(x);
    if n_parts > 1 {
        println!(
            "{shown}: {what}, {n_parts} parts, {total} ticks taken at {} Hz ({} s)",
            g(rate),
            g(total as f64 / rate)
        );
    } else {
        println!(
            "{shown}: {what}, {total} ticks at {} Hz ({} s)",
            g(rate),
            g(total as f64 / rate)
        );
        if let Some(p) = parts.first() {
            for c in &p.channels {
                if let wireskein_capture::Chan::Analog(x) = c {
                    if let Some(want) = req.analog_samples.filter(|&w| x.n < w) {
                        println!(
                            "note: {}: {} samples of the {want} asked (the probe's limit)",
                            x.name, x.n
                        );
                    }
                    continue;
                }
                let end = c.end();
                if c.step() == 1 && req.samples > 0 && end < req.samples {
                    println!(
                        "note: {}: {end} samples of the {} asked (the probe's limit)",
                        c.name(),
                        req.samples
                    );
                } else if c.step() > 1 && req.samples > 0 && end < req.samples {
                    println!(
                        "note: {}: covers {end} of the {} samples asked (the probe's limit)",
                        c.name(),
                        req.samples
                    );
                }
            }
        }
    }
    let probe = got.meta.get("probe").cloned().unwrap_or(Value::Null);
    if let Some(link) = probe.get("link") {
        let mut items = vec![];
        if link.get("rate").and_then(|v| v.as_f64()) == Some(0.0) {
            items.push(format!(
                "not raised ({})",
                link.get("why").and_then(|v| v.as_str()).unwrap_or("no serial port")
            ));
        }
        let jl = |k: &str| link.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        items.push(format!("opened in {:.2} s", jl("open_s")));
        let kb = jl("read_bytes") / 1000.0;
        let r = if jl("read_s") > 0.0 {
            format!(" ({:.1} KB/s)", kb / jl("read_s"))
        } else {
            String::new()
        };
        items.push(format!("read {kb:.1} KB in {:.2} s{r}", jl("read_s")));
        println!("link: {}", items.join(", "));
    }
    if let Some(d) = probe.get("data") {
        let dj = |k: &str| d.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
        println!(
            "data: format {}, {:.1} KB (expected {:.1} KB, format 0 {:.1} KB), blocks of {} samples",
            d.get("format").unwrap_or(&Value::Null),
            dj("bytes") / 1000.0,
            dj("expected_bytes") / 1000.0,
            dj("format0_bytes") / 1000.0,
            d.get("block").unwrap_or(&Value::Null)
        );
    }
    if let Some(r) = probe.get("data_rate").and_then(|v| v.as_f64()) {
        println!("data rate: {} MB/s", wireskein_io::pyfmt::g(r / 1e6, 3));
    }
    if (rate - req.rate_hz as f64).abs() > 1e-6 * req.rate_hz as f64 {
        println!(
            "note: the probe took {} Hz (asked {}): the nearest it keeps",
            g(rate),
            g(req.rate_hz as f64)
        );
    }
    if let Some(ms) = probe
        .get("stream_buffer")
        .and_then(|b| b.get("tolerable_delay_s"))
        .and_then(|v| v.as_f64())
        .map(|s| s * 1e3)
    {
        let bytes = probe["stream_buffer"]["bytes"].clone();
        println!(
            "buffer: {bytes} B on the probe: this host may pause {} ms before it drops{}",
            wireskein_io::pyfmt::g(ms, 3),
            if ms < BUFFER_WARN_MS {
                " (short: a busy host will lose data; a lower --rate or fewer channels gives more)"
            } else {
                ""
            }
        );
    }
    if let Some(st) = got.meta.get("stream") {
        let kb = st["bytes"].as_f64().unwrap_or(0.0) / 1000.0;
        let rs = st["read_s"].as_f64().unwrap_or(0.0);
        println!(
            "stream: {kb:.1} KB in {} frames over {rs:.2} s{}",
            st["frames"],
            if rs > 0.0 {
                format!(" ({:.1} KB/s)", kb / rs)
            } else {
                String::new()
            }
        );
    }
    if let Some(odd) = got
        .meta
        .get("repeat")
        .and_then(|r| r.get("without_trigger"))
        .and_then(|v| v.as_array())
    {
        let first: Vec<String> = odd.iter().take(8).map(|v| v.to_string()).collect();
        eprintln!(
            "note: {} of the {} acquisitions came without a trigger (parts {}{}): the probe took them straight after \
             the one before",
            odd.len(),
            n_parts,
            first.join(", "),
            if odd.len() > 8 { ", ..." } else { "" }
        );
    }
    if !got.lost.is_empty() {
        let lost = &got.lost;
        let ticks: i64 = lost.iter().filter_map(|x| x.ticks).sum();
        let mut lines = vec![format!(
            "INCOMPLETE: {} loss{}",
            lost.len(),
            if lost.len() != 1 { "es" } else { "" }
        )];
        if ticks > 0 {
            lines[0] += &format!(
                ", {ticks} ticks lost against {total} taken ({} %)",
                wireskein_io::pyfmt::g(ticks as f64 / (ticks + total) as f64 * 100.0, 2)
            );
        }
        let mut shown_: Vec<&wireskein_capture::Loss> = lost.iter().take(LOSS_LINES).collect();
        if lost.len() > LOSS_LINES {
            shown_.push(lost.last().expect("a loss"));
        }
        for x in shown_ {
            lines.push(format!(
                "  after part {}: {}{}{}",
                x.after_part,
                x.reason,
                x.ticks
                    .filter(|&t| t != 0)
                    .map(|t| format!(" ({t} ticks)"))
                    .unwrap_or_default(),
                if x.stopped { ", stopped there" } else { "" }
            ));
        }
        if lost.len() > LOSS_LINES + 1 {
            lines.insert(1 + LOSS_LINES, format!("  ... {} more", lost.len() - LOSS_LINES - 1));
        }
        for l in lines {
            eprintln!("{l}");
        }
        eprintln!("{shown}: kept what was taken; the capture failed");
        return ExitCode::FAILURE;
    }
    let _ = num;
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_times_read_as_python_reads_them() {
        assert_eq!(parse_count("20M"), Ok(20_000_000));
        assert_eq!(parse_count("2.5k"), Ok(2500));
        assert!(parse_count("20m").unwrap_err().contains("M for mega"));
        assert_eq!(seconds("500ms"), Ok(0.5));
        assert_eq!(seconds("2"), Ok(2.0));
        let (c, r) = parse_channels("A=GPIO30,B=GPIO31/any_low:64,C").unwrap();
        assert_eq!(c[2], ("C".into(), "C".into()));
        assert_eq!(r, vec![("B".to_string(), ("any_low".to_string(), 64))]);
    }
}
