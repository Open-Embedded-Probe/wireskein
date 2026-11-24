//! A run judged (`verify._verify`): each expectation's section, the captures (or the stretch of one) it is judged on,
//! each check's verdict with what it measured, its notices and how finely it was sampled; the report
//! (docs/run-format.ja.md §3), its lines for people, its summary, JUnit XML.

use crate::cap::{self, Cap};
use crate::checks::{self, V};
use crate::j::J;
use crate::jd;
use crate::pyfmt::g;
use crate::tree::{self, Tree};
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

pub const REPORT_FORMAT: &str = "wireskein-report/1";
pub const RUN_FORMAT: &str = "wireskein-run/1";
pub const STATUSES: [&str; 4] = ["ok", "ng", "unchecked", "measured"];

#[derive(Debug, Clone)]
pub struct Res {
    pub path: String,
    pub capture: Option<String>,
    pub check: String,
    pub ok: Option<bool>,
    pub expected: J,
    pub measured: J,
    pub reason: String,
    pub status: String,
    pub notices: Vec<J>,
}

impl Res {
    fn new(path: &str, capture: Option<String>, check: &str, v: V, expected: J, measured: J, reason: String) -> Res {
        let (ok, status) = match v {
            V::Ok => (Some(true), "ok"),
            V::Ng => (Some(false), "ng"),
            V::Unchecked => (None, "unchecked"),
            V::Measured => (None, "measured"),
        };
        Res {
            path: path.into(),
            capture,
            check: check.into(),
            ok,
            expected,
            measured,
            reason,
            status: status.into(),
            notices: vec![],
        }
    }

    pub fn to_j(&self) -> J {
        jd! {"path" => &self.path, "capture" => self.capture.clone(), "check" => &self.check, "ok" => self.ok,
        "expected" => &self.expected, "measured" => &self.measured, "reason" => &self.reason,
        "status" => &self.status, "notices" => self.notices.clone()}
    }
}

fn status_of(v: V) -> &'static str {
    match v {
        V::Ok => "ok",
        V::Ng => "ng",
        V::Unchecked => "unchecked",
        V::Measured => "measured",
    }
}

/// Why a capture is incomplete, in one line (`verify._lost`).
fn lost(cap: &Cap) -> String {
    let parts: Vec<String> = cap
        .meta
        .get("lost")
        .list()
        .iter()
        .map(|x| {
            let mut s = format!(
                "after part {}: {}",
                x.get("after_part").repr(),
                x.get("reason")
                    .s()
                    .map(String::from)
                    .unwrap_or_else(|| x.get("reason").repr())
            );
            if x.get("ticks").truthy() {
                s += &format!(" ({} ticks)", x.get("ticks").repr());
            }
            if x.get("stopped").truthy() {
                s += ", stopped there";
            }
            s
        })
        .collect();
    format!("data lost while capturing: {}", parts.join("; "))
}

/// A section's stretch of a capture: the capture (or window) judged, the inner window (probe clock), the clipped ends.
enum Alt {
    None,
    Empty,
    Inner(Rc<Cap>),
}

struct Loaded {
    file: String,
    cap: Rc<Cap>,
    alt: Alt,
    clipped: Vec<&'static str>,
}

struct Ctx<'a> {
    run_dir: &'a Path,
    cache: BTreeMap<(String, String), Rc<Vec<Rc<Cap>>>>,
}

impl Ctx<'_> {
    fn load(&mut self, c: &J) -> Result<Rc<Vec<Rc<Cap>>>, String> {
        let file = c.get("file").s().unwrap_or("").to_string();
        let key = (file.clone(), c.get("host_ns").repr());
        if let Some(v) = self.cache.get(&key) {
            return Ok(v.clone());
        }
        let v = Rc::new(cap::load(self.run_dir, &file, c.get("host_ns"))?);
        self.cache.insert(key, v.clone());
        Ok(v)
    }
}

/// A section opened and closed by the probe's lines, in one part (`verify._on_probe_clock`).
#[allow(clippy::type_complexity)]
fn on_probe_clock(
    t: &Tree,
    seg: usize,
    cap: &Cap,
) -> (Option<(Option<(i64, i64)>, (i64, i64), Vec<&'static str>)>, String) {
    let (a, ua, why) = tree::place(t.opened(seg).unwrap(), &cap.meta, cap.tick_hz);
    let (b, ub, why_b) = tree::place(t.closed(seg).unwrap(), &cap.meta, cap.tick_hz);
    let (Some(a), Some(b)) = (a, b) else {
        return (None, if why.is_empty() { why_b } else { why });
    };
    let n = cap.n as f64;
    if b < 0.0 || a > n {
        return (None, "the section is outside this capture".into());
    }
    let mut clipped = vec![];
    if a - ua < 0.0 {
        clipped.push("begin");
    }
    if b + ub > n {
        clipped.push("end");
    }
    let outer = (0.max((a - ua).floor() as i64), cap.n.min((b + ub).ceil() as i64));
    let inner = (0.max((a + ua).ceil() as i64), cap.n.min((b - ub).floor() as i64));
    (
        Some((if inner.0 < inner.1 { Some(inner) } else { None }, outer, clipped)),
        String::new(),
    )
}

fn in_segment(c: &J, t: i64, path: &str, seg: &tree::Seg) -> bool {
    if let Some(p) = c.get("path").s() {
        return p == path || p.starts_with(&format!("{path}/"));
    }
    seg.begin <= t && t < seg.end.unwrap_or(1 << 62)
}

fn segment_captures(ctx: &mut Ctx, doc: &J, t: &Tree, path: &str, seg: usize) -> Result<(Vec<Loaded>, String), String> {
    if t.probe_section(seg) {
        let (mut loaded, mut whys) = (vec![], std::collections::BTreeSet::new());
        for c in doc.get("captures").list() {
            for part in ctx.load(c)?.iter() {
                let (w, why) = on_probe_clock(t, seg, part);
                let Some((inner, outer, clipped)) = w else {
                    whys.insert(why);
                    continue;
                };
                let mut win = cap::window(part, outer.0, outer.1);
                win.meta
                    .set("label", format!("{}@{}:{}", part.label(), outer.0, outer.1));
                let alt = match inner {
                    Some((a, b)) => Alt::Inner(Rc::new(cap::window(part, a, b))),
                    None => Alt::Empty,
                };
                loaded.push(Loaded {
                    file: c.get("file").s().unwrap_or("").into(),
                    cap: Rc::new(win),
                    alt,
                    clipped,
                });
            }
        }
        let why = if loaded.is_empty() {
            format!(
                "no capture holds the section on the probe's clock ({})",
                whys.into_iter().collect::<Vec<_>>().join("; ")
            )
        } else {
            String::new()
        };
        return Ok((loaded, why));
    }
    let s = &t.segs[seg];
    let inside: Vec<&J> = doc
        .get("captures")
        .list()
        .iter()
        .filter(|c| in_segment(c, c.get("host_ns").i().unwrap_or(0).div_euclid(1000), path, s))
        .collect();
    if inside.is_empty() {
        return Ok((vec![], "no capture inside the segment".into()));
    }
    let mut out = vec![];
    for c in inside {
        for part in ctx.load(c)?.iter() {
            out.push(Loaded {
                file: c.get("file").s().unwrap_or("").into(),
                cap: part.clone(),
                alt: Alt::None,
                clipped: vec![],
            });
        }
    }
    Ok((out, String::new()))
}

/// A segment of a run with a capture that holds it, cut to it (`verify.sections`' entries): its path, the capture
/// file, its label, its window (ticks of the capture part, or None for a capture assigned whole), its part and the
/// stretch; without a capture, why.
#[derive(Debug, Clone)]
pub struct Section {
    pub path: String,
    pub file: Option<String>,
    pub label: J,
    pub window: J,
    pub part: J,
    pub cap: Option<Rc<Cap>>,
    pub why: String,
}

/// Every segment of a run with the captures that hold it, cut to it (Python's `verify.sections`), for analysis
/// without expectations (`wireskein states --section`).
pub fn sections(run_dir: &Path) -> Result<Vec<Section>, String> {
    let text = std::fs::read_to_string(run_dir.join("run.json")).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    let doc = J::parse(&text).map_err(|e| format!("{}/run.json: {e}", run_dir.display()))?;
    if doc.get("format").s() != Some(RUN_FORMAT) {
        return Err(format!(
            "{}: format {}, expected {}",
            run_dir.display(),
            doc.get("format").repr(),
            J::from(RUN_FORMAT).repr()
        ));
    }
    let t = tree::segment_tree(&doc);
    let mut ctx = Ctx {
        run_dir,
        cache: BTreeMap::new(),
    };
    let mut out = vec![];
    for i in t.walk() {
        if t.segs[i].root {
            continue;
        }
        let path = t.path(i);
        let (loaded, why) = segment_captures(&mut ctx, &doc, &t, &path, i)?;
        if loaded.is_empty() {
            out.push(Section {
                path: path.clone(),
                file: None,
                label: J::Null,
                window: J::Null,
                part: J::Null,
                cap: None,
                why,
            });
        }
        for l in loaded {
            let span = l.cap.meta.get("parts");
            let part = match span.list().first() {
                Some(p) if span.truthy() => p.clone(),
                _ if l.cap.meta.has("part") => l.cap.meta.get("part").clone(),
                _ => J::Int(0),
            };
            let label = if l.cap.meta.has("label") {
                l.cap.meta.get("label").clone()
            } else {
                J::from(l.file.as_str())
            };
            out.push(Section {
                path: path.clone(),
                file: Some(l.file.clone()),
                label,
                window: l.cap.meta.get("window").clone(),
                part,
                cap: Some(l.cap.clone()),
                why: String::new(),
            });
        }
    }
    Ok(out)
}

/// A run directory as Python's `str(Path(p))` writes it.
pub fn run_name(p: &Path) -> String {
    py_path(p)
}

/// A path as Python's `str(Path(p))` writes it: no trailing separator, no "." parts.
fn py_path(p: &Path) -> String {
    let c: std::path::PathBuf = p
        .components()
        .filter(|c| !matches!(c, std::path::Component::CurDir))
        .collect();
    let s = c.display().to_string();
    if s.is_empty() { ".".into() } else { s }
}

/// The report of a run directory.
pub fn verify(run_dir: &Path) -> Result<J, String> {
    let text = std::fs::read_to_string(run_dir.join("run.json")).map_err(|e| format!("{}: {e}", run_dir.display()))?;
    let doc = J::parse(&text).map_err(|e| format!("{}/run.json: {e}", run_dir.display()))?;
    if doc.get("format").s() != Some(RUN_FORMAT) {
        return Err(format!(
            "{}: format {}, expected {}",
            run_dir.display(),
            doc.get("format").repr(),
            J::from(RUN_FORMAT).repr()
        ));
    }
    let t = tree::segment_tree(&doc);
    let mut segs: Vec<(String, usize)> = vec![];
    for i in t.walk() {
        if !t.segs[i].root {
            let p = t.path(i);
            if !segs.iter().any(|(q, _)| *q == p) {
                segs.push((p, i));
            } else if let Some(e) = segs.iter_mut().find(|(q, _)| *q == p) {
                e.1 = i;
            }
        }
    }
    let seg_of = |p: &str| segs.iter().find(|(q, _)| q == p).map(|(_, i)| *i);
    let mut ctx = Ctx {
        run_dir,
        cache: BTreeMap::new(),
    };
    let mut results: Vec<Res> = vec![];
    for (path, spec) in doc.get("expect").items() {
        let Some(seg) = seg_of(path) else {
            results.push(Res::new(
                path,
                None,
                "segment",
                V::Ng,
                J::dict(),
                J::dict(),
                "no segment with this path in the log".into(),
            ));
            continue;
        };
        let (loaded, why) = segment_captures(&mut ctx, &doc, &t, path, seg)?;
        if loaded.is_empty() {
            results.push(Res::new(path, None, "capture", V::Ng, J::dict(), J::dict(), why));
            continue;
        }
        let mut files: Vec<&str> = vec![];
        for l in &loaded {
            if !files.contains(&l.file.as_str()) {
                files.push(&l.file);
            }
        }
        for f in files {
            let first = &loaded.iter().find(|l| l.file == f).unwrap().cap;
            if first.meta.get("complete") == &J::Bool(false) {
                results.push(Res::new(
                    path,
                    Some(f.into()),
                    "capture",
                    V::Ng,
                    J::dict(),
                    J::dict(),
                    lost(first),
                ));
            }
        }
        for x in spec.get("checks").list() {
            let kind = x.get("kind").s().unwrap_or("").to_string();
            let pins = checks::pins_of(x);
            let mut hit: Vec<&Loaded> = loaded
                .iter()
                .filter(|l| {
                    let names = l.cap.names();
                    pins.iter().all(|p| names.contains(p.as_str()))
                })
                .collect();
            if hit.is_empty() && kind == "only_moving" {
                hit = loaded.iter().collect();
            }
            if hit.is_empty() {
                let mut skipped: BTreeMap<String, J> = BTreeMap::new();
                for l in &loaded {
                    for c in l.cap.meta.get("skipped_channels").list() {
                        if let Some(n) = c.get("name").s() {
                            skipped.insert(n.into(), c.get("encoding").clone());
                        }
                    }
                }
                let mut why = format!(
                    "pins not in any capture: {}",
                    pins.iter().cloned().collect::<Vec<_>>().join(", ")
                );
                let named: Vec<String> = pins
                    .iter()
                    .filter_map(|p| {
                        skipped
                            .get(p)
                            .map(|e| format!("{p} has encoding {}, not read by this version", e.repr()))
                    })
                    .collect();
                if !named.is_empty() {
                    why += &format!(" ({})", named.join(", "));
                }
                results.push(Res::new(path, None, &kind, V::Unchecked, x.clone(), J::dict(), why));
                continue;
            }
            for l in hit {
                let label = l.cap.label();
                let (mut ok, mut got, mut why) = checks::judge(&l.cap, x, &pins);
                match &l.alt {
                    Alt::Empty => {
                        ok = V::Unchecked;
                        why = "the section is shorter than its bounds' uncertainty".into();
                    }
                    Alt::Inner(alt) => {
                        let (ok2, got2, why2) = checks::judge(alt, x, &pins);
                        if ok2 != ok {
                            got = jd! {"outer" => got, "inner" => got2};
                            let nr = |w: &str| {
                                if w.is_empty() {
                                    "no reason".to_string()
                                } else {
                                    w.to_string()
                                }
                            };
                            why = format!(
                                "the verdict depends on where the section's bounds fall: {} on {} ({}), {} on {} ({})",
                                status_of(ok),
                                l.cap.meta.get("window").repr(),
                                nr(&why),
                                status_of(ok2),
                                alt.meta.get("window").repr(),
                                nr(&why2)
                            );
                            ok = V::Unchecked;
                        }
                    }
                    Alt::None => {}
                }
                if !matches!(l.alt, Alt::None) {
                    got.set("window", l.cap.meta.get("window").clone());
                }
                if !l.clipped.is_empty() {
                    got.set(
                        "clipped",
                        J::from(l.clipped.iter().map(|s| J::from(*s)).collect::<Vec<_>>()),
                    );
                    if ok == V::Ng {
                        let w: Vec<&str> = l
                            .clipped
                            .iter()
                            .map(|k| if *k == "begin" { "began before" } else { "ended after" })
                            .collect();
                        why = format!(
                            "{}the section {} the capture",
                            if why.is_empty() {
                                String::new()
                            } else {
                                format!("{why}; ")
                            },
                            w.join(" and ")
                        );
                    }
                }
                let has_outer = got.has("outer");
                let found;
                {
                    let target = if has_outer {
                        got.get_mut("outer").unwrap()
                    } else {
                        &mut got
                    };
                    (ok, why) = sampling(ok, why, target, x, &l.cap);
                    let r = notices(ok, why, target, x, &l.cap);
                    (ok, why, found) = r;
                }
                if l.cap.meta.get("time_base_slipped").truthy() {
                    got.set("time_base_slipped", true);
                    if ok == V::Ng {
                        why = format!(
                            "{}the probe reported a time base slip in this capture",
                            if why.is_empty() {
                                String::new()
                            } else {
                                format!("{why}; ")
                            }
                        );
                    }
                }
                let mut r = Res::new(path, Some(label), &kind, ok, x.clone(), got, why);
                r.notices = found;
                results.push(r);
            }
        }
    }
    for tr in doc.get("results").list() {
        let st = tr.get("status").s().unwrap_or("");
        let mut why = tr.get("reason").s().unwrap_or("").to_string();
        let path = tr.get("path").s().unwrap_or("");
        let mut v = match st {
            "ok" => V::Ok,
            "ng" => V::Ng,
            "measured" => V::Measured,
            _ => V::Unchecked,
        };
        if seg_of(path).is_none() {
            v = V::Ng;
            why = [why.as_str(), "no segment with this path in the log"]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("; ");
        }
        let mut r = Res::new(
            path,
            tr.get("capture").s().map(String::from),
            tr.get("check").s().unwrap_or(""),
            v,
            jd! {"by" => "test"},
            if tr.has("measured") {
                tr.get("measured").clone()
            } else {
                J::dict()
            },
            why,
        );
        if v == V::Measured {
            r.ok = None;
        }
        r.notices = tr.get("notices").list().to_vec();
        results.push(r);
    }
    for e in doc.get("log").list() {
        if e.get("src").s() == Some("lost") {
            results.push(Res::new(
                "",
                None,
                "probe_log",
                V::Ng,
                J::dict(),
                J::dict(),
                e.get("text").s().unwrap_or("").into(),
            ));
        }
    }
    for (path, i) in &segs {
        for issue in &t.segs[*i].issues {
            results.push(Res::new(
                path,
                None,
                "markers",
                V::Ng,
                J::dict(),
                J::dict(),
                issue.dumps_py(),
            ));
        }
    }
    for issue in &t.segs[0].issues {
        results.push(Res::new(
            "",
            None,
            "markers",
            V::Ng,
            J::dict(),
            J::dict(),
            issue.dumps_py(),
        ));
    }
    let mut summary = J::dict();
    for s in STATUSES {
        summary.set(s, results.iter().filter(|r| r.status == s).count());
    }
    summary.set("notices", results.iter().map(|r| r.notices.len()).sum::<usize>());
    summary.set("segments", segs.len());
    summary.set("captures", doc.get("captures").list().len());
    let log: Vec<J> = doc
        .get("log")
        .list()
        .iter()
        .map(|e| {
            let mut d = J::dict();
            for k in ["host_ns", "src", "text", "probe_ns"] {
                if e.has(k) {
                    d.set(k, e.get(k).clone());
                }
            }
            d
        })
        .collect();
    Ok(jd! {"format" => REPORT_FORMAT, "run" => py_path(run_dir),
    "meta" => if doc.has("meta") { doc.get("meta").clone() } else { J::dict() },
    "results" => results.iter().map(Res::to_j).collect::<Vec<_>>(), "summary" => summary,
    "tree_issues" => t.segs[0].issues.clone(), "log" => log})
}

// ---- how finely each length a check looks at was sampled (`verify._sampling`) ----

pub const LEAST_SAMPLES: f64 = 5.0;
pub const BEST_SAMPLES: f64 = 10.0;

pub fn sps(r: f64) -> String {
    for (unit, f) in [("GS/s", 1e9), ("MS/s", 1e6), ("kS/s", 1e3), ("S/s", 1.0)] {
        if r >= f * 0.9995 || unit == "S/s" {
            return format!("{} {unit}", g(r / f, 3));
        }
    }
    unreachable!()
}

fn short_half(cap: &Cap, pin: &str) -> Option<f64> {
    let ch = cap.ch(pin)?;
    crate::timing::short_half(ch, cap.n)
        .filter(|&h| h != 0.0)
        .map(|h| h / cap.rate)
}

/// (what, T in s, whole, notice kind) of each length a check looks at (`verify._lengths`).
fn lengths(cap: &Cap, x: &J, got: &J) -> Vec<(String, f64, bool, Option<&'static str>)> {
    let k = x.get("kind").s().unwrap_or("");
    let mut rows = vec![];
    let half = |rows: &mut Vec<(String, f64, bool, Option<&'static str>)>, pin: &J, what: &str| {
        if let Some(p) = pin.s().filter(|s| !s.is_empty())
            && let Some(h) = short_half(cap, p)
        {
            rows.push((what.to_string(), h, true, None));
        }
    };
    match k {
        "i2c" => {
            half(&mut rows, x.get("scl"), "SCL's shorter half");
            rows.push((
                "a spike (spike_max_s)".into(),
                x.f_truthy_or("spike_max_s", 50e-9),
                false,
                Some("bus_glitch"),
            ));
        }
        "spi" | "sd_spi" | "qspi" => {
            half(&mut rows, x.get("clk"), "SCK's shorter half");
            for (key, what) in [("setup_min_s", "the setup bound"), ("hold_min_s", "the hold bound")] {
                if x.get(key).truthy() {
                    rows.push((format!("{what} ({key})"), x.get(key).f().unwrap(), false, None));
                }
            }
        }
        "uart" | "rs485" => {
            let b = if x.get("baud").truthy() {
                x.get("baud")
            } else {
                got.get("baud")
            };
            if b.truthy() {
                rows.push(("a bit".into(), 1.0 / b.f().unwrap(), true, None));
            }
        }
        "lin" if x.get("baud").truthy() => rows.push(("a bit".into(), 1.0 / x.get("baud").f().unwrap(), true, None)),
        "can" => {
            let b = if x.get("bitrate").truthy() {
                x.get("bitrate")
            } else {
                got.get("bitrate")
            };
            if b.truthy() {
                rows.push(("a bit".into(), 1.0 / b.f().unwrap(), true, None));
            }
        }
        "i2s" => half(&mut rows, x.get("bclk"), "BCLK's shorter half"),
        "spdif" => {
            let f = if x.get("fs_hz").truthy() {
                x.get("fs_hz")
            } else {
                got.get("fs_hz")
            };
            if f.truthy() {
                rows.push(("half a cell".into(), 1.0 / (128.0 * f.f().unwrap()), true, None));
            }
        }
        "square" => {
            if !x.get("duty").is_null() && x.get("freq_hz").truthy() {
                let d = x.get("duty").f().unwrap();
                if 0.0 < d && d < 1.0 {
                    rows.push((
                        "the wave's shorter half".into(),
                        d.min(1.0 - d) / x.get("freq_hz").f().unwrap(),
                        true,
                        None,
                    ));
                }
            } else {
                half(&mut rows, x.get("pin"), "the wave's shorter half");
            }
        }
        "pulses" => half(&mut rows, x.get("pin"), "the pulses' shorter half"),
        "i2c_timing" => {
            let mut lim = crate::timing::i2c_limits(x);
            lim.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, (lo, hi)) in &lim {
                if name != "f_scl" {
                    for (side, v) in [("min", lo), ("max", hi)] {
                        if let Some(v) = v
                            && *v != 0.0
                        {
                            rows.push((format!("{name} {side} {}", crate::checks::si(*v)), *v, false, None));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    rows
}

pub const UNDER_RATE: &str = "a length the check looks at under 5 samples, or within the probe's glitch filter: what \
                              it read there is not sure (what, t_s, interval_s, samples, whole: the check answers on \
                              it, least_rate_hz, best_rate_hz, glitch_filter_s)";

fn sampling(ok: V, mut why: String, got: &mut J, x: &J, cap: &Cap) -> (V, String) {
    let rows = lengths(cap, x, got);
    if rows.is_empty() {
        return (ok, why);
    }
    let pins = checks::pins_of(x);
    let d = pins.iter().filter_map(|p| cap.ch(p)).map(|c| c.step).max().unwrap_or(1) as f64 / cap.rate;
    let gf = cap.meta.get("probe").get("glitch_filter_ns");
    let gf = if gf.truthy() { gf.f().map(|v| v * 1e-9) } else { None };
    let (mut out, mut under) = (vec![], vec![]);
    for (what, t, whole, kind) in rows {
        let n = t / d;
        let mut r =
            jd! {"what" => what.clone(), "t_s" => t, "samples" => crate::pyfmt::round_n(n, 2), "whole" => whole};
        if let Some(gf) = gf
            && t <= gf
        {
            r.set("glitch_filter_s", gf);
        }
        if n < LEAST_SAMPLES || r.has("glitch_filter_s") {
            under.push((r.clone(), kind));
        }
        out.push(r);
    }
    got.set(
        "sampling",
        jd! {"interval_s" => d, "lengths" => out, "least" => LEAST_SAMPLES as i64},
    );
    let say = |r: &J| -> String {
        let t = r.get("t_s").f().unwrap();
        if r.has("glitch_filter_s") {
            return format!(
                "{} is {}, within the probe's glitch filter of {}: not seen",
                r.get("what").s().unwrap(),
                checks::si(t),
                checks::si(r.get("glitch_filter_s").f().unwrap())
            );
        }
        format!(
            "{} is {}, {} samples of {}: under {}, not sure at this rate (capture at {} or more, {} best)",
            r.get("what").s().unwrap(),
            checks::si(t),
            g(r.get("samples").f().unwrap(), 6),
            checks::si(d),
            LEAST_SAMPLES as i64,
            sps(LEAST_SAMPLES / t),
            sps(BEST_SAMPLES / t)
        )
    };
    for (r, _kind) in under {
        let t = r.get("t_s").f().unwrap();
        let mut n = jd! {"kind" => "under_rate", "tick" => 0, "what" => r.get("what"), "t_s" => t, "interval_s" => d,
        "samples" => r.get("samples"), "whole" => r.get("whole"),
        "least_rate_hz" => g6f(LEAST_SAMPLES / t), "best_rate_hz" => g6f(BEST_SAMPLES / t)};
        if r.has("glitch_filter_s") {
            n.set("glitch_filter_s", r.get("glitch_filter_s").clone());
        }
        push_notice(got, n);
        if ok == V::Ng && r.get("whole").truthy() {
            let s = say(&r);
            why = if why.is_empty() { s } else { format!("{why}; {s}") };
        }
    }
    (ok, why)
}

/// `float(f"{v:.6g}")`.
fn g6f(v: f64) -> f64 {
    g(v, 6).parse().unwrap_or(v)
}

pub fn push_notice(got: &mut J, n: J) {
    if !got.has("notices") {
        got.set("notices", J::List(vec![]));
    }
    if let Some(J::List(v)) = got.get_mut("notices") {
        v.push(n);
    }
}

/// A tick of a part that may be runs joined, as (the file's part, the tick in it) (`fileio.locate`).
fn locate(meta: &J, tick: i64) -> (i64, i64) {
    let starts = meta.get("part_starts").list();
    if starts.is_empty() {
        return (meta.get("part").i().unwrap_or(0), tick);
    }
    let (mut k, mut off) = (starts[0].list()[0].i().unwrap(), starts[0].list()[1].i().unwrap());
    for s in starts {
        let (kk, o) = (s.list()[0].i().unwrap(), s.list()[1].i().unwrap());
        if tick >= o {
            (k, off) = (kk, o);
        }
    }
    (k, tick - off)
}

fn notices(mut ok: V, mut why: String, got: &mut J, x: &J, cap: &Cap) -> (V, String, Vec<J>) {
    let found = got.remove("notices").map(|v| v.list().to_vec()).unwrap_or_default();
    let base = cap.window_base();
    let mut out = vec![];
    for n in found {
        let (part, tick) = locate(&cap.meta, n.get("tick").i().unwrap_or(0) + base);
        let mut m = n.clone();
        m.set("tick", tick);
        m.set("part", part);
        out.push(m);
    }
    if let Some(t) = got.get("change_tick").i() {
        let (p, tk) = locate(&cap.meta, t);
        got.set("change_part", p);
        got.set("change_tick", tk);
    }
    let kind = x.get("kind").s().unwrap_or("");
    let mut known: Vec<&str> = checks::notice_kinds(kind).to_vec();
    known.push("under_rate");
    let limits = x.get("notices");
    let mut bad: Vec<&str> = limits
        .items()
        .iter()
        .map(|(k, _)| k.as_str())
        .filter(|k| !known.contains(k))
        .collect();
    bad.sort();
    if !bad.is_empty() {
        let s = format!(
            "no notice {} in {kind} (it finds: {})",
            bad.join(", "),
            if known.is_empty() {
                "none".to_string()
            } else {
                known.join(", ")
            }
        );
        why = if why.is_empty() { s } else { format!("{why}; {s}") };
        return (V::Ng, why, out);
    }
    for (k, lim) in limits.items() {
        let ng = lim.s() == Some("ng");
        let most = if ng { 0 } else { lim.i().unwrap_or(0) };
        let n: i64 = out
            .iter()
            .filter(|m| m.get("kind").s() == Some(k.as_str()))
            .map(|m| {
                if m.has("count") {
                    m.get("count").i().unwrap_or(1)
                } else {
                    1
                }
            })
            .sum();
        if n > most {
            ok = V::Ng;
            let first = out
                .iter()
                .find(|m| m.get("kind").s() == Some(k.as_str()))
                .unwrap()
                .get("tick")
                .repr();
            let s = format!(
                "{n} {k}{}, first at tick {first}",
                if ng {
                    String::new()
                } else {
                    format!(" (at most {most})")
                }
            );
            why = if why.is_empty() { s } else { format!("{why}; {s}") };
        }
    }
    (ok, why, out)
}

// ---- for people ----

pub fn failed(rep: &J, allow_unchecked: bool) -> bool {
    let s = rep.get("summary");
    s.get("ng").truthy() || (s.get("unchecked").truthy() && !allow_unchecked)
}

fn mark(status: &str) -> &'static str {
    match status {
        "ok" => "OK",
        "ng" => "NG",
        "unchecked" => "--",
        _ => "ME",
    }
}

/// One line per result, as `wireskein verify` prints them (`verify.lines`).
pub fn lines(rep: &J, ok: bool, notices: bool) -> Vec<String> {
    let mut out = vec![];
    for r in rep.get("results").list() {
        let ns = r.get("notices").list();
        let st = r.get("status").s().unwrap_or("");
        if st == "ok" && !ok && !(notices && !ns.is_empty()) {
            continue;
        }
        let mut tail = if ns.is_empty() {
            String::new()
        } else {
            format!("  [{} notice{}]", ns.len(), if ns.len() != 1 { "s" } else { "" })
        };
        let sm = r.get("measured").get("sampling");
        if !sm.get("lengths").list().is_empty() {
            let key = |v: &J| {
                if v.get("whole").truthy() || !v.has("whole") {
                    v.get("samples").f().unwrap_or(0.0)
                } else {
                    1e9
                }
            };
            let s = sm
                .get("lengths")
                .list()
                .iter()
                .fold(None::<&J>, |a, v| match a {
                    Some(a) if key(a) <= key(v) => Some(a),
                    _ => Some(v),
                })
                .unwrap();
            let least = sm.get("least").f().unwrap_or(5.0);
            let low = if s.get("samples").f().unwrap_or(0.0) < least {
                format!(": under {}, not sure", sm.get("least").repr())
            } else {
                String::new()
            };
            tail = format!(
                "  [{} {}: {} samples of {}{low}]{tail}",
                s.get("what").s().unwrap_or(""),
                checks::si(s.get("t_s").f().unwrap_or(0.0)),
                g(s.get("samples").f().unwrap_or(0.0), 6),
                checks::si(sm.get("interval_s").f().unwrap_or(0.0))
            );
        }
        let line = format!(
            "{}  {}  {}  {}  {}",
            mark(st),
            r.get("path").s().unwrap_or(""),
            r.get("check").s().unwrap_or(""),
            r.get("capture").s().unwrap_or(""),
            r.get("reason").s().unwrap_or("")
        );
        out.push(format!("{}{tail}", line.trim_end()));
        if notices {
            for m in ns {
                let extra: Vec<String> = m
                    .items()
                    .iter()
                    .filter(|(k, _)| !["kind", "tick", "part"].contains(&k.as_str()))
                    .map(|(k, v)| format!("{k} {}", v.s().map(String::from).unwrap_or_else(|| v.repr())))
                    .collect();
                let part = m.get("part");
                out.push(format!(
                    "      notice {} at tick {}{}{}",
                    m.get("kind").s().unwrap_or(""),
                    m.get("tick").repr(),
                    if part.truthy() {
                        format!(" (part {})", part.repr())
                    } else {
                        String::new()
                    },
                    if extra.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", extra.join(", "))
                    }
                ));
            }
        }
    }
    out
}

pub fn summary_line(rep: &J) -> String {
    let s = rep.get("summary");
    let n = |k: &str| s.get(k).i().unwrap_or(0);
    let nn = n("notices");
    format!(
        "{} ok, {} ng, {} unchecked, {} measured{} ({} segments, {} captures)",
        n("ok"),
        n("ng"),
        n("unchecked"),
        n("measured"),
        if nn > 0 {
            format!(", {nn} notice{}", if nn != 1 { "s" } else { "" })
        } else {
            String::new()
        },
        n("segments"),
        n("captures")
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn quoteattr(s: &str) -> String {
    let e = xml_escape(s)
        .replace('\n', "&#10;")
        .replace('\r', "&#13;")
        .replace('\t', "&#9;");
    if e.contains('"') {
        if e.contains('\'') {
            format!("\"{}\"", e.replace('"', "&quot;"))
        } else {
            format!("'{e}'")
        }
    } else {
        format!("\"{e}\"")
    }
}

/// JUnit XML: ng a failure, unchecked a failure (skipped when allowed), measured passed with its values.
pub fn junit(rep: &J, allow_unchecked: bool) -> String {
    let rs = rep.get("results").list();
    let is_fail = |r: &J| {
        let s = r.get("status").s().unwrap_or("");
        s == "ng" || (s == "unchecked" && !allow_unchecked)
    };
    let is_skip = |r: &J| r.get("status").s() == Some("unchecked") && allow_unchecked;
    let mut out = vec![format!(
        "<testsuite name={} tests=\"{}\" failures=\"{}\" skipped=\"{}\">",
        quoteattr(&format!("wireskein verify {}", rep.get("run").s().unwrap_or(""))),
        rs.len(),
        rs.iter().filter(|r| is_fail(r)).count(),
        rs.iter().filter(|r| is_skip(r)).count()
    )];
    for r in rs {
        let name = format!(
            "{} [{}]",
            r.get("check").s().unwrap_or(""),
            r.get("capture").s().filter(|s| !s.is_empty()).unwrap_or("-")
        );
        out.push(format!(
            "  <testcase classname={} name={}>",
            quoteattr(r.get("path").s().unwrap_or("")),
            quoteattr(&name)
        ));
        let values = xml_escape(&r.get("measured").dumps_py());
        let reason = r.get("reason").s().unwrap_or("");
        if is_fail(r) {
            let why = if r.get("status").s() == Some("ng") {
                reason.to_string()
            } else {
                format!("unchecked: {reason}")
            };
            out.push(format!("    <failure message={}>{values}</failure>", quoteattr(&why)));
        } else if is_skip(r) {
            out.push(format!("    <skipped message={}/>", quoteattr(reason)));
        } else if r.get("status").s() == Some("measured") {
            out.push(format!("    <system-out>{values}</system-out>"));
        }
        out.push("  </testcase>".into());
    }
    out.push("</testsuite>".into());
    out.join("\n")
}
