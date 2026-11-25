//! `--bench`: the same run as prototype 1's (8 zooms × at most 96 frames panning, then a zoom sweep 6 % a frame) and
//! the frame's parts.
//!
//! Where the time goes: iced marks its stages (update, view, layout, interact, draw, present with the prepare and
//! render of each kind of primitive) for its debugger (comet), through `iced_debug` to a TCP server (`iced_beacon`).
//! With `--bench` this program is that server: a thread receives every finished span with its end time, and the run
//! adds them up between marks. wgpu's own `profiling` scopes (acquire, submit, present) go through prof.rs as in
//! prototype 1. The app's own parts (the scene, the text, the views) it times itself (app.rs `P_*`).

use crate::app::{App, P_CPU, P_LIST, P_N, P_SCENE, P_TEXT, P_UPDATE, P_VIEW, P_WAVE, rss_mb};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};
use view_core::Viewport;
use web_time::Instant;

static SPANS: Mutex<Vec<(SystemTime, &'static str, f64)>> = Mutex::new(Vec::new());

/// The beacon server on a free port of this machine (before iced starts: its client reads the address then).
pub fn start_server() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|l| l.local_addr().ok())
        .map(|a| a.port())
        .unwrap_or(9167);
    // SAFETY: set once at the start, before any other thread runs
    unsafe { std::env::set_var("ICED_BEACON_SERVER_ADDRESS", format!("127.0.0.1:{port}")) };
    std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            use iced::futures::StreamExt;
            use iced_beacon::span::Span;
            let mut s = std::pin::pin!(iced_beacon::run());
            while let Some(ev) = s.next().await {
                if let iced_beacon::Event::SpanFinished { at, duration, span } = ev {
                    let d = duration.as_secs_f64();
                    let mut v = SPANS.lock().unwrap();
                    match span {
                        Span::Boot => v.push((at, "boot", d)),
                        Span::Update { .. } => v.push((at, "update", d)),
                        Span::View { .. } => v.push((at, "view", d)),
                        Span::Layout { .. } => v.push((at, "layout", d)),
                        Span::Interact { .. } => v.push((at, "interact", d)),
                        Span::Draw { .. } => v.push((at, "draw", d)),
                        Span::Present { prepare, render, .. } => {
                            v.push((at, "present", d));
                            for (k, x) in [
                                ("prep quad", prepare.quads),
                                ("prep tri", prepare.triangles),
                                ("prep shader", prepare.shaders),
                                ("prep text", prepare.text),
                                ("prep image", prepare.images),
                                ("rend quad", render.quads),
                                ("rend tri", render.triangles),
                                ("rend shader", render.shaders),
                                ("rend text", render.text),
                                ("rend image", render.images),
                            ] {
                                v.push((at, k, x.as_secs_f64()));
                            }
                        }
                        Span::Custom { .. } => v.push((at, "custom", d)),
                    }
                }
            }
        });
    });
}

/// Seconds in each stage whose end is in [a, b).
fn spans_between(a: SystemTime, b: SystemTime) -> Vec<(&'static str, f64, usize)> {
    let v = SPANS.lock().unwrap();
    let mut out: Vec<(&'static str, f64, usize)> = vec![];
    for &(at, k, d) in v.iter() {
        if at >= a && at < b {
            match out.iter_mut().find(|x| x.0 == k) {
                Some(x) => {
                    x.1 += d;
                    x.2 += 1;
                }
                None => out.push((k, d, 1)),
            }
        }
    }
    out
}

const STAGES: [&str; 15] = [
    "update",
    "view",
    "layout",
    "interact",
    "draw",
    "present",
    "prep quad",
    "prep tri",
    "prep shader",
    "prep text",
    "rend quad",
    "rend tri",
    "rend shader",
    "rend text",
    "rend image",
];
/// wgpu's scopes (prof.rs names: wgpu's own, not egui-wgpu's as in prototype 1)
const WGPU: [(&str, &str); 4] = [
    ("acquire", "Surface::get_current_texture"),
    ("submit", "Queue::submit"),
    ("wgpu present", "Surface::present"),
    ("maintain", "Device::maintain"),
];

#[derive(Clone, Copy)]
pub enum Phase {
    Wait,
    Zoom(usize, u32),
    Sweep(u32),
    Done,
}

struct Mark {
    wall: Instant,
    sys: SystemTime,
    prof: crate::prof::Totals,
    acc: [f64; P_N],
    counts: (u64, u64, u64, u64),
}

pub struct Bench {
    pub out: PathBuf,
    pub phase: Phase,
    t0: Instant,
    zooms: Vec<f64>,
    samples: Vec<f32>,
    wave: Vec<f32>,
    mark: Option<Mark>,
    whole: Option<(SystemTime, crate::prof::Totals)>,
    lines: Vec<String>,
    parts: Vec<String>,
}

impl Bench {
    pub fn new(out: PathBuf) -> Self {
        Bench {
            out,
            phase: Phase::Wait,
            t0: Instant::now(),
            zooms: vec![1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7],
            samples: vec![],
            wave: vec![],
            mark: None,
            whole: None,
            lines: vec![],
            parts: vec![],
        }
    }
    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }
}

fn mark(app: &App) -> Mark {
    let p = app.perf.borrow();
    Mark {
        wall: Instant::now(),
        sys: SystemTime::now(),
        prof: crate::prof::snapshot(),
        acc: p.acc,
        counts: p.counts,
    }
}

fn header() -> Vec<String> {
    let mut h = String::from("| view | frames | dt | ");
    for k in STAGES {
        h += &format!("{k} | ");
    }
    for (k, _) in WGPU {
        h += &format!("{k} | ");
    }
    h += "main CPU | App::update | App::view | wave draw | scene | text | list view | gap | instances | labels | shapes |";
    let n = h.matches('|').count() - 1;
    vec![h, format!("|{}", "---|".repeat(n))]
}

fn row(label: &str, frames: usize, dt_ms: f64, a: &Mark, b: &Mark) -> String {
    let f = frames.max(1) as f64;
    let mut r = format!("| {label} | {frames} | {dt_ms:.2} | ");
    let sp = spans_between(a.sys, b.sys);
    let get = |k: &str| sp.iter().find(|x| x.0 == k).map(|x| x.1 * 1e3 / f).unwrap_or(0.0);
    let mut busy = 0.0;
    for k in STAGES {
        let ms = get(k);
        if matches!(k, "update" | "view" | "layout" | "interact" | "draw" | "present") {
            busy += ms;
        }
        r += &format!("{ms:.2} | ");
    }
    for (_, name) in WGPU {
        r += &format!("{:.2} | ", crate::prof::between(&a.prof, &b.prof, name).0 * 1e3 / f);
    }
    let d = |i: usize| (b.acc[i] - a.acc[i]) / f;
    let c = |k: usize| {
        let (x, y) = match k {
            0 => (a.counts.0, b.counts.0),
            1 => (a.counts.1, b.counts.1),
            _ => (a.counts.2, b.counts.2),
        };
        let n = (b.counts.3 - a.counts.3).max(1);
        (y - x) / n
    };
    r += &format!(
        "{:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {} |",
        d(P_CPU),
        d(P_UPDATE),
        d(P_VIEW),
        d(P_WAVE),
        d(P_SCENE),
        d(P_TEXT),
        d(P_LIST),
        dt_ms - busy,
        c(0),
        c(1),
        c(2)
    );
    r
}

/// One step a frame: returns the view to force on the first waveform (None: leave it), and whether the run ended.
pub fn step(app: &App, b: &mut Bench, dt_ms: f32, width: f32, gpu: &str) -> (Option<Viewport>, bool) {
    let Some(cap) = &app.cap else { return (None, false) };
    let n = cap.n_ticks;
    let perf_first = app.perf.borrow().first_paint;
    let ready =
        cap.lod_s.lock().unwrap().is_some() && app.analysis_slot.is_none() && perf_first.is_some() && width > 50.0;
    const WARM: u32 = 6;
    let enough = |f: u32, b: &Bench| {
        f >= 95 || f >= WARM + 24 && b.mark.as_ref().is_some_and(|m| m.wall.elapsed().as_secs_f32() > 3.0)
    };
    match b.phase {
        Phase::Wait => {
            if ready || b.t0.elapsed().as_secs() > 600 {
                b.lines.push(format!(
                    "# {} ({} lines, {} changes, {} ticks): first paint {}, read {:.0} ms, summaries {:.2} s, analysis {}",
                    cap.title,
                    cap.lines.len(),
                    cap.total_edges,
                    cap.n_ticks,
                    perf_first.map(|s| format!("{:.0} ms", s * 1e3)).unwrap_or("-".into()),
                    cap.read_s * 1e3,
                    cap.lod_s.lock().unwrap().unwrap_or(f64::NAN),
                    app.analysis.as_ref().map(|a| format!("{:.2} s", a.seconds)).unwrap_or("-".into())
                ));
                let wo = app.perf.borrow().win_opened;
                b.lines.push(format!(
                    "# the main window opened {} after the capture (iced's window, wgpu's device and surface); its first paint {} after that",
                    wo.map(|s| format!("{:.0} ms", s * 1e3)).unwrap_or("-".into()),
                    match (wo, perf_first) {
                        (Some(a), Some(b)) => format!("{:.0} ms", (b - a) * 1e3),
                        _ => "-".into(),
                    }
                ));
                b.lines.push(format!(
                    "# window {:.0}x{:.0} (logical px), scale {:.2}, waveform {:.0} px wide",
                    app.win_size.width, app.win_size.height, app.scale, width
                ));
                b.lines.push("# gpu: (at the end)".into());
                b.lines.push(format!(
                    "# present: {} (ICED_PRESENT_MODE; vsync off unless --vsync)",
                    std::env::var("ICED_PRESENT_MODE").unwrap_or("no_vsync".into())
                ));
                b.lines
                    .push("| ticks/px | fps (median) | frame ms p50 / p95 | waveform CPU ms (scene) |".into());
                b.lines.push("|---|---|---|---|".into());
                b.whole = Some((SystemTime::now(), crate::prof::snapshot()));
                b.parts = header();
                b.phase = Phase::Zoom(0, 0);
            }
            (None, false)
        }
        Phase::Zoom(zi, f) => {
            if f == WARM {
                b.mark = Some(mark(app));
            }
            if f > WARM {
                b.samples.push(dt_ms);
                b.wave.push(app.perf.borrow().wave_avg());
            }
            let fit = Viewport::fit(n, width.max(100.0));
            let z = b.zooms[zi];
            let mut vp = fit.zoomed(z, width * 0.37);
            vp.t0 += f as f64 * 7.0 * vp.tpp;
            if f > WARM && enough(f, b) {
                let mut s = std::mem::take(&mut b.samples);
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let p = |q: f32| s[((s.len() - 1) as f32 * q) as usize];
                let wv = b.wave.iter().sum::<f32>() / b.wave.len().max(1) as f32;
                b.wave.clear();
                b.lines.push(format!(
                    "| {:.3} | {:.0} | {:.1} / {:.1} | {:.2} |",
                    vp.tpp,
                    1000.0 / p(0.5),
                    p(0.5),
                    p(0.95),
                    wv
                ));
                if let Some(m) = b.mark.take() {
                    let dt = s.iter().sum::<f32>() as f64 / s.len().max(1) as f64;
                    b.parts
                        .push(row(&format!("{:.3} t/px", vp.tpp), s.len(), dt, &m, &mark(app)));
                }
                b.phase = if zi + 1 < b.zooms.len() && b.zooms[zi + 1] <= n as f64 * 20.0 / width as f64 {
                    Phase::Zoom(zi + 1, 0)
                } else {
                    Phase::Sweep(0)
                };
            } else {
                b.phase = Phase::Zoom(zi, f + 1);
            }
            (Some(vp.clamped(n)), false)
        }
        Phase::Sweep(f) => {
            if f == 3 {
                b.mark = Some(mark(app));
            }
            if f > 3 {
                b.samples.push(dt_ms);
            }
            let fit = Viewport::fit(n, width.max(100.0));
            let vp = fit.zoomed(1.06f64.powi(f as i32), width * 0.37);
            let long = b.mark.as_ref().is_some_and(|m| m.wall.elapsed().as_secs_f32() > 12.0);
            if f >= 240 || vp.tpp < 0.05 || long && f > 30 {
                let mut s = std::mem::take(&mut b.samples);
                s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let p = |q: f32| s[((s.len() - 1) as f32 * q) as usize];
                b.lines.push(format!(
                    "zoom sweep (6 %/frame, {} frames): frame ms p50 {:.1}, p95 {:.1}, max {:.1}",
                    s.len(),
                    p(0.5),
                    p(0.95),
                    s[s.len() - 1]
                ));
                // the spans' last messages arrive a little after the frames
                std::thread::sleep(Duration::from_millis(300));
                if let Some(m) = b.mark.take() {
                    let dt = s.iter().sum::<f32>() as f64 / s.len().max(1) as f64;
                    b.parts.push(row("sweep", s.len(), dt, &m, &mark(app)));
                }
                b.lines.push(format!(
                    "memory: model {:.0} MB, rss {:.0} MB",
                    cap.mem_bytes() as f64 / 1e6,
                    rss_mb().unwrap_or(f64::NAN)
                ));
                b.lines.push(String::new());
                b.lines.push(
                    "## the frame's parts (ms a frame, mean; iced's stages from its debug spans, wgpu's scopes, the app's own; gap = dt - (update + view + layout + interact + draw + present))"
                        .into(),
                );
                b.lines.append(&mut b.parts);
                if let Some((s0, w0)) = &b.whole {
                    b.lines.push(String::new());
                    b.lines.push("## iced's stages over the run (ms in all, count)".into());
                    let mut sp = spans_between(*s0, SystemTime::now());
                    sp.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap());
                    for (k, s, c) in sp {
                        b.lines.push(format!("{:9.1} ms {:7}  {k}", s * 1e3, c));
                    }
                    b.lines.push(String::new());
                    b.lines.push(
                        "## every profiling span over the run (ms in all, calls; inclusive, the main thread)".into(),
                    );
                    for (name, secs, calls) in crate::prof::all_between(w0, &crate::prof::snapshot())
                        .into_iter()
                        .take(40)
                    {
                        b.lines.push(format!("{:9.1} ms {:7}  {name}", secs * 1e3, calls));
                    }
                }
                b.lines.push(format!(
                    "bench: {:.1} s from the start",
                    app.started.elapsed().as_secs_f64()
                ));
                for l in b.lines.iter_mut() {
                    if l == "# gpu: (at the end)" {
                        *l = format!("# gpu: {gpu}");
                    }
                }
                let text = b.lines.join("\n") + "\n";
                println!("{text}");
                let _ = std::fs::write(&b.out, text);
                b.phase = Phase::Done;
                return (Some(vp.clamped(n)), true);
            }
            b.phase = Phase::Sweep(f + 1);
            (Some(vp.clamped(n)), false)
        }
        Phase::Done => (None, true),
    }
}
