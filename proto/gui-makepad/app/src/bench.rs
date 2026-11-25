//! `--bench`: the same run as prototypes 1 and 3 (8 zooms × at most 96 frames panning, then a zoom sweep 6 % a frame)
//! and the frame's parts.
//!
//! Where the time goes: Makepad has no profiling spans of its own. The app times what it does in a frame: the frame
//! loop (`NextFrame`: the analysis polled, the flings, this bench), the drawing (`Event::Draw`: every widget builds
//! its draw lists; inside it the waveform with its scene, its instances and its text, the list), all the events it
//! handled. What is left of the frame's interval (`gap`) is Makepad's platform layer: the draw lists to the GPU
//! (OpenGL on Linux, D3D11 on Windows), the swap and the wait for the next frame.

use crate::state::*;
use std::path::PathBuf;
use std::time::Instant;
use view_core::Viewport;

#[derive(Clone, Copy)]
pub enum Phase {
    Wait,
    Zoom(usize, u32),
    Sweep(u32),
    Done,
}

struct Mark {
    wall: Instant,
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
            lines: vec![],
            parts: vec![],
        }
    }
    pub fn done(&self) -> bool {
        matches!(self.phase, Phase::Done)
    }
}

fn mark(st: &St) -> Mark {
    Mark {
        wall: Instant::now(),
        acc: st.perf.acc,
        counts: st.perf.counts,
    }
}

fn header() -> Vec<String> {
    let h = "| view | frames | dt | main CPU | events | frame loop | draw | wave draw | scene | instances push | text | list draw | gap | instances | labels | shapes |";
    let n = h.matches('|').count() - 1;
    vec![h.to_string(), format!("|{}", "---|".repeat(n))]
}

fn row(label: &str, frames: usize, dt_ms: f64, a: &Mark, b: &Mark) -> String {
    let f = frames.max(1) as f64;
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
    format!(
        "| {label} | {frames} | {dt_ms:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {} |",
        d(P_CPU),
        d(P_EVENTS),
        d(P_FRAME),
        d(P_DRAW),
        d(P_WAVE),
        d(P_SCENE),
        d(P_INST),
        d(P_TEXT),
        d(P_LIST),
        dt_ms - d(P_EVENTS),
        c(0),
        c(1),
        c(2)
    )
}

/// One step a frame: returns the view to force on the first waveform (None: leave it), and whether the run ended.
pub fn step(st: &St, b: &mut Bench, dt_ms: f32, width: f32) -> (Option<Viewport>, bool) {
    let Some(cap) = &st.cap else { return (None, false) };
    let n = cap.n_ticks;
    let perf_first = st.perf.first_paint;
    let ready =
        cap.lod_s.lock().unwrap().is_some() && st.analysis_slot.is_none() && perf_first.is_some() && width > 50.0;
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
                    st.analysis.as_ref().map(|a| format!("{:.2} s", a.seconds)).unwrap_or("-".into())
                ));
                b.lines.push(format!(
                    "# window {:.0}x{:.0} (logical px), dpi factor {:.2}, waveform {:.0} px wide",
                    st.win_size.x, st.win_size.y, st.dpi, width
                ));
                b.lines
                    .push(format!("# gpu: {} (Makepad 1.0; vsync as Makepad sets it)", st.gpu));
                b.lines
                    .push("| ticks/px | fps (median) | frame ms p50 / p95 | waveform CPU ms (scene) |".into());
                b.lines.push("|---|---|---|---|".into());
                b.parts = header();
                b.phase = Phase::Zoom(0, 0);
            }
            (None, false)
        }
        Phase::Zoom(zi, f) => {
            if f == WARM {
                b.mark = Some(mark(st));
            }
            if f > WARM {
                b.samples.push(dt_ms);
                b.wave.push(st.perf.wave_avg());
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
                        .push(row(&format!("{:.3} t/px", vp.tpp), s.len(), dt, &m, &mark(st)));
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
                b.mark = Some(mark(st));
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
                if let Some(m) = b.mark.take() {
                    let dt = s.iter().sum::<f32>() as f64 / s.len().max(1) as f64;
                    b.parts.push(row("sweep", s.len(), dt, &m, &mark(st)));
                }
                b.lines.push(format!(
                    "memory: model {:.0} MB, rss {:.0} MB",
                    cap.mem_bytes() as f64 / 1e6,
                    rss_mb().unwrap_or(f64::NAN)
                ));
                b.lines.push(String::new());
                b.lines.push(
                    "## the frame's parts (ms a frame, mean; events = all the app's event handling (frame loop + draw + the rest); gap = dt - events: Makepad's platform layer, the GPU, the swap, the wait)"
                        .into(),
                );
                b.lines.append(&mut b.parts);
                b.lines.push(format!(
                    "bench: {:.1} s from the start",
                    st.started.elapsed().as_secs_f64()
                ));
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
