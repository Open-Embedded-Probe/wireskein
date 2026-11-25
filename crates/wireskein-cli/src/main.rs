//! `wireskein-rs`: WireSkein's command line in plain Rust (bench/notes/2026-11-24-rust-plan.ja.md §3, R5), the
//! subcommands of `wireskein` (src/wireskein/cli.py) with the same arguments, output, messages and exit codes:
//!
//!     wireskein-rs analyze CAPTURE [--mode final|all|select] [--select PATH ...] [--hint NAMES|JSON|@FILE]
//!                  [--alternatives] [--depth transport|frames|protocol|device] [--out FILE] [--threshold N=V ...]
//!                  [--window FROM TO] [--segment PATH [--markers JSON] [--events]] [--split S|off] [--items FILE]
//!                  [--pins A,B] [--part K]
//!     wireskein-rs decode CAPTURE --spec JSON|@FILE|FILE [--out FILE] [--split S|off] [--items FILE] [--pins A,B]
//!     wireskein-rs segments CAPTURE [--hint H] [--threshold N=V ...] [--markers JSON] [--results] [--events]
//!                  [--out FILE]
//!     wireskein-rs verify RUN_DIR [--junit FILE] [--json FILE] [--log] [--allow-unchecked] [-v] [--mark]
//!     wireskein-rs survey CAPTURE [--pins A,B] [--bins N] [--top N] [--plan] [--split S] [--json] [--out FILE]
//!     wireskein-rs convert IN OUT [--from S] [--to S] [--pins A,B] [--part K]
//!     wireskein-rs info FILE.wireskein
//!     wireskein-rs states FILE|RUN_DIR [--section GLOB] [--pin P ...] [--part K] [--tol X] [--min-periods N]
//!                  [--max-odd N] [--json]
//!     wireskein-rs annotate FILE [--save] [--hint H] [--threshold N=V ...]
//!     wireskein-rs align FILE --reference LOGIC --via CH [--to OTHER] [--threshold V|LOW,HIGH] [--max-ppm X]
//!                  [--max-offset T] [--apply-to A,B] [--save]
//!     wireskein-rs note FILE.wireskein TEXT [--json] [--kind K]
//!     wireskein-rs attach FILE.wireskein NAME [SRC | --text TEXT] [--replace]
//!     wireskein-rs capture --source oep:ADDRESS --channels NAME=ID[/MODE:D],... --rate R (--samples N |
//!                  --mode stream --duration T | --mode repeat --segments K --samples N) [--trigger NAME:KIND]
//!                  [--pretrigger N] [--arm pretrigger|now] [--timeout S] [--on-loss stop|continue]
//!                  [--glitch-filter NS] [--note TEXT] -o OUT
//!     wireskein-rs --version
//!     wireskein-rs --definitions DIR ... SUBCOMMAND ...   (a definitions directory's device packs, devices/)
//!
//! The analysis is the Rust core's (wireskein-core), the rest wireskein-analysis, wireskein-io and wireskein-verify.
//! Not here (Python's only, `wireskein`): gui, a definitions directory with declared protocols (protocols/*.toml)
//! or marker dialects (markers/*.toml); capture from sigrok, of analog channels, or from a probe the Rust OEP
//! client does not open (a serial port, HID, a line label).

mod analyze;
mod capture;
mod files;
mod notes;
mod views;

use std::process::ExitCode;

const PROG: &str = "wireskein-rs";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `devices/` of each definitions directory given (WIRESKEIN_DEFINITIONS, then --definitions), set once by main.
static DEVICE_DIRS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// The device pack directories of the definitions directories given (none: WireSkein's packs only).
pub fn device_dirs() -> &'static [String] {
    DEVICE_DIRS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// The hints with the definitions directories' device packs (`device_dirs`, read by the Rust core beside
/// `devices`); the hints as given when there are none (or the hints are not an object).
pub fn with_device_dirs(hints: &serde_json::Value) -> serde_json::Value {
    let dirs = device_dirs();
    match hints {
        _ if dirs.is_empty() => hints.clone(),
        serde_json::Value::Null => serde_json::json!({ "device_dirs": dirs }),
        serde_json::Value::Object(m) => {
            let mut m = m.clone();
            m.insert("device_dirs".into(), serde_json::json!(dirs));
            serde_json::Value::Object(m)
        }
        _ => hints.clone(),
    }
}

/// What a definitions directory may hold here: device packs (devices/). Python's `plugins.add_definitions` refuses
/// tables at its top; the declared protocols (protocols/*.toml) and marker dialects (markers/*.toml) are read by
/// Python's wireskein only (Err((exit code, message))).
fn definitions(dirs: &[String]) -> Result<Vec<String>, (u8, String)> {
    let tomls = |d: &std::path::Path| -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(d)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "toml"))
            .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
            .collect();
        v.sort();
        v
    };
    let mut out: Vec<String> = vec![];
    for d in dirs {
        let p = std::path::Path::new(d);
        let Ok(p) = p
            .canonicalize()
            .map_err(|_| ())
            .and_then(|p| if p.is_dir() { Ok(p) } else { Err(()) })
        else {
            return Err((1, format!("{d}: not a directory")));
        };
        let top = tomls(&p);
        if !top.is_empty() {
            return Err((
                1,
                format!(
                    "{d}: protocol tables at its top ({}): a protocol on the wire itself is part of WireSkein's \
                     analysis engine, not an extension point (docs/protocols.md §5)",
                    top.join(", ")
                ),
            ));
        }
        for (sub, what) in [("protocols", "declared protocols"), ("markers", "marker dialects")] {
            if !tomls(&p.join(sub)).is_empty() {
                return Err((
                    2,
                    format!("--definitions {d}: {what} ({sub}/*.toml) are read by Python's wireskein only"),
                ));
            }
        }
        let dev = p.join("devices");
        if dev.is_dir() {
            let dev = dev.to_string_lossy().to_string();
            if !out.contains(&dev) {
                out.push(dev);
            }
        }
    }
    Ok(out)
}

/// The arguments of a subcommand: its positionals, the options with a value (each time given), the flags, the
/// options with a list of values (nargs="*" / 2).
#[derive(Debug, Default)]
pub struct Args {
    pub pos: Vec<String>,
    vals: Vec<(String, String)>,
    flags: Vec<String>,
    lists: Vec<(String, Vec<String>)>,
}

/// What a subcommand takes: options with a value, flags, options with a list ("*": every value up to the next
/// option; a number: that many).
pub struct Takes {
    pub values: &'static [&'static str],
    pub flags: &'static [&'static str],
    pub lists: &'static [(&'static str, Option<usize>)],
}

impl Args {
    /// The last value given for an option.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.vals.iter().rev().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }
    /// Every value given for a repeatable option, in order.
    pub fn all(&self, k: &str) -> Vec<&str> {
        self.vals
            .iter()
            .filter(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
            .collect()
    }
    pub fn has(&self, k: &str) -> bool {
        self.flags.iter().any(|f| f == k)
    }
    pub fn list(&self, k: &str) -> Option<&[String]> {
        self.lists.iter().rev().find(|(a, _)| a == k).map(|(_, v)| v.as_slice())
    }

    /// The arguments as `takes` reads them (`--opt value` or `--opt=value`).
    pub fn parse(args: &[String], takes: &Takes) -> Result<Args, String> {
        let mut out = Args::default();
        let mut i = 0;
        let is_opt = |s: &str| s.starts_with('-') && s.len() > 1 && s.parse::<f64>().is_err();
        while i < args.len() {
            let a = &args[i];
            i += 1;
            if !is_opt(a) {
                out.pos.push(a.clone());
                continue;
            }
            let (name, inline) = match a.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n.to_string(), Some(v.to_string())),
                _ => (a.clone(), None),
            };
            if takes.flags.contains(&name.as_str()) {
                if let Some(v) = inline {
                    return Err(format!("argument {name}: ignored explicit argument {v:?}"));
                }
                out.flags.push(name);
            } else if takes.values.contains(&name.as_str()) {
                let v = match inline {
                    Some(v) => v,
                    None => {
                        let Some(v) = args.get(i).filter(|v| !is_opt(v)) else {
                            return Err(format!("argument {name}: expected one argument"));
                        };
                        i += 1;
                        v.clone()
                    }
                };
                out.vals.push((name, v));
            } else if let Some((_, n)) = takes.lists.iter().find(|(k, _)| *k == name) {
                let mut vs: Vec<String> = inline.into_iter().collect();
                while i < args.len() && n.is_none_or(|n| vs.len() < n) && !is_opt(&args[i]) {
                    vs.push(args[i].clone());
                    i += 1;
                }
                if let Some(n) = n
                    && vs.len() != *n
                {
                    return Err(format!("argument {name}: expected {n} arguments"));
                }
                out.lists.push((name, vs));
            } else {
                return Err(format!("unrecognized arguments: {a}"));
            }
        }
        Ok(out)
    }
}

/// A usage error (argparse's): exit 2.
pub fn usage_error(cmd: &str, msg: &str) -> ExitCode {
    eprintln!("usage: {PROG} {cmd} ... (see {PROG} --help)");
    eprintln!("{PROG} {cmd}: error: {msg}");
    ExitCode::from(2)
}

/// A failure of the command (Python's `sys.exit(f"wireskein {cmd}: {e}")`): exit 1.
pub fn fail(cmd: &str, msg: &str) -> ExitCode {
    eprintln!("{PROG} {cmd}: {msg}");
    ExitCode::FAILURE
}

/// A document written to FILE (its length in characters said on stderr, as Python's) or printed.
pub fn emit(text: &str, out: Option<&str>) -> Result<(), String> {
    match out {
        Some(f) => {
            std::fs::write(f, text).map_err(|e| format!("[Errno] {e}: {f:?}"))?;
            eprintln!("{} bytes -> {f}", text.chars().count());
        }
        None => println!("{text}"),
    }
    Ok(())
}

/// "A,B" -> ["A", "B"] (blanks dropped); None when empty.
pub fn names(v: Option<&str>) -> Option<Vec<String>> {
    let out: Vec<String> = v?
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    (!out.is_empty()).then_some(out)
}

/// --threshold NAME=V|NAME=LOW,HIGH (repeatable) -> (name, low, high).
pub fn thresholds(items: &[&str]) -> Result<Vec<(String, f64, f64)>, String> {
    let mut out = vec![];
    for it in items {
        let Some((name, v)) = it.split_once('=') else {
            return Err(format!(
                "--threshold {}: give NAME=V or NAME=LOW,HIGH",
                wireskein_io::j::py_str_repr(it)
            ));
        };
        let (lo, hi) = wireskein_analysis::analog::threshold_of(v)?;
        out.push((name.to_string(), lo, hi));
    }
    Ok(out)
}

/// --hint as Python's `parse_hint` reads it: JSON, @file.json, or protocol names ("rvswd", "uart,spi").
pub fn hint(text: Option<&str>) -> Result<serde_json::Value, String> {
    let Some(text) = text.filter(|t| !t.is_empty()) else {
        return Ok(serde_json::Value::Null);
    };
    let body = match text.strip_prefix('@') {
        Some(p) => std::fs::read_to_string(p).map_err(|e| format!("[Errno] {e}: {p:?}"))?,
        None => text.to_string(),
    };
    let t = body.trim();
    if t.starts_with('{') || t.starts_with('[') {
        return serde_json::from_str(t)
            .map_err(|e| format!("--hint: not JSON ({e}); give JSON, @file.json or protocol names"));
    }
    let names: Vec<&str> = t.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).collect();
    if names.is_empty() || !names.iter().all(|x| x.chars().all(|c| c == '_' || c.is_alphanumeric())) {
        return Err(format!(
            "--hint: {} is neither JSON nor protocol names",
            wireskein_io::j::py_str_repr(text)
        ));
    }
    Ok(serde_json::json!({ "protocols": names }))
}

/// The hints `hint` read, as the document gives them: JSON in its own keys' order.
pub fn hint_j(text: Option<&str>, hints: &serde_json::Value) -> wireskein_io::j::J {
    let body = match text.and_then(|t| t.strip_prefix('@')) {
        Some(p) => std::fs::read_to_string(p).ok(),
        None => text.map(String::from),
    };
    body.and_then(|b| {
        let t = b.trim();
        (t.starts_with('{') || t.starts_with('[')).then(|| wireskein_io::j::J::parse(t).ok())?
    })
    .unwrap_or_else(|| wireskein_io::write::j_of(hints))
}

fn help() {
    println!(
        "usage: {PROG} [--version] {{analyze,decode,segments,verify,survey,convert,info,states,annotate,align,note,attach,capture}} ..."
    );
    println!();
    println!("WireSkein's command line in Rust: the subcommands of `wireskein` with the same arguments and output.");
    println!(
        "--definitions DIR (repeatable, before the subcommand; WIRESKEIN_DEFINITIONS too): a definitions directory's \
         device packs (devices/)."
    );
    println!(
        "Python's only (use `wireskein`): gui, declared protocols and marker dialects of --definitions; capture of \
         analog or sigrok."
    );
}

/// `wireskein verify`: the lines, the summary, the report and JUnit when asked; 1 when the run fails.
fn verify_cmd(args: &[String]) -> ExitCode {
    let takes = Takes {
        values: &["--json", "--junit"],
        flags: &["--log", "--allow-unchecked", "--mark", "-v", "--verbose"],
        lists: &[],
    };
    let a = match Args::parse(args, &takes) {
        Ok(a) => a,
        Err(e) => return usage_error("verify", &e),
    };
    let [run] = a.pos.as_slice() else {
        return usage_error("verify", "give RUN_DIR (one)");
    };
    let run = std::path::Path::new(run);
    let mut rep = match wireskein_verify::verify(run) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    if a.has("--mark")
        && let Err(e) = wireskein_verify::mark(run, &rep)
    {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    if !a.has("--log") {
        rep.remove("log");
    }
    for line in wireskein_verify::lines(&rep, true, a.has("-v") || a.has("--verbose")) {
        println!("{line}");
    }
    println!("{}", wireskein_verify::summary_line(&rep));
    let allow = a.has("--allow-unchecked");
    let write = |f: &str, text: String| -> Result<(), String> {
        if f == "-" {
            println!("{text}");
            Ok(())
        } else {
            std::fs::write(f, text).map_err(|e| format!("{f}: {e}"))
        }
    };
    if let Some(f) = a.get("--json")
        && let Err(e) = write(f, rep.dumps())
    {
        eprintln!("{e}");
        return ExitCode::from(2);
    }
    if let Some(f) = a.get("--junit")
        && let Err(e) = write(f, wireskein_verify::junit(&rep, allow))
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // --definitions DIR before the subcommand (Python's global option), after the environment's directories
    let mut defs: Vec<String> = std::env::var("WIRESKEIN_DEFINITIONS")
        .unwrap_or_default()
        .split(if cfg!(windows) { ';' } else { ':' })
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect();
    while let Some(a) = args.first() {
        if let Some(d) = a.strip_prefix("--definitions=") {
            defs.push(d.to_string());
            args.remove(0);
        } else if a == "--definitions" {
            if args.len() < 2 || args[1].starts_with('-') {
                return usage_error("", "argument --definitions: expected one argument");
            }
            defs.push(args[1].clone());
            args.drain(..2);
        } else {
            break;
        }
    }
    match definitions(&defs) {
        Ok(d) => {
            DEVICE_DIRS.set(d).ok();
        }
        Err((2, e)) => return usage_error("", &e),
        Err((_, e)) => {
            eprintln!("{PROG}: {e}");
            return ExitCode::FAILURE;
        }
    }
    let Some(cmd) = args.first().map(|s| s.as_str()) else {
        help();
        return ExitCode::from(2);
    };
    let rest = &args[1..];
    match cmd {
        "--version" => {
            println!("{PROG} {VERSION}");
            ExitCode::SUCCESS
        }
        "-h" | "--help" => {
            help();
            ExitCode::SUCCESS
        }
        "analyze" => analyze::analyze(rest),
        "decode" => analyze::decode(rest),
        "segments" => views::segments(rest),
        "verify" => verify_cmd(rest),
        "survey" => views::survey(rest),
        "states" => views::states(rest),
        "annotate" => views::annotate(rest),
        "align" => views::align(rest),
        "convert" => files::convert(rest),
        "info" => files::info(rest),
        "note" => notes::note(rest),
        "attach" => notes::attach(rest),
        "capture" => capture::capture(rest),
        "gui" => usage_error(
            cmd,
            &format!("{cmd} is not in the Rust command line yet: use `wireskein {cmd}`"),
        ),
        _ => usage_error(
            "",
            &format!(
                "argument cmd: invalid choice: {} (choose from analyze, decode, segments, verify, survey, convert, \
                 info, states, annotate, align, note, attach, capture)",
                wireskein_io::j::py_str_repr(cmd)
            ),
        ),
    }
}
