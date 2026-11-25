//! What to draw for a digital line in a view, in pixels from the view's left edge (no colors, no GUI): flat runs at a
//! level, single edges, busy blocks where a pixel holds many changes (their density for the shade), and stretches not
//! yet read.

use crate::Viewport;
use crate::digital::LineView;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    /// the line at one level from x0 to x1
    Run { x0: f32, x1: f32, high: bool },
    /// a change at x
    Edge { x: f32 },
    /// 2 or more changes a pixel from x0 to x1; density 0..1 (log of the changes a pixel)
    Busy { x0: f32, x1: f32, density: f32 },
    /// not read yet (the summary is still being built)
    Unknown { x0: f32, x1: f32 },
}

/// The density shade of `count` changes in a pixel: 2 → 0, 2^16 or more → 1, a step for every factor of 4.
pub fn density(count: u32) -> f32 {
    if count < 2 {
        return 0.0;
    }
    let l = (count as f32).log2() - 1.0;
    ((l / 15.0).clamp(0.0, 1.0) * 8.0).round() / 8.0
}

/// The shapes of a line read for a view.
pub fn digital_segs(lv: &LineView, vp: &Viewport, out: &mut Vec<Seg>) {
    out.clear();
    if lv.tb <= lv.ta {
        return;
    }
    let xa = vp.x_of(lv.ta as f64).max(0.0);
    let xb = vp.x_of(lv.tb as f64).min(vp.width);
    if lv.exact {
        let mut lvl = lv.start_level;
        let mut x = xa;
        for &e in &lv.edges {
            let xe = vp.x_of(e as f64);
            out.push(Seg::Run { x0: x, x1: xe, high: lvl == 1 });
            out.push(Seg::Edge { x: xe });
            lvl ^= 1;
            x = xe;
        }
        out.push(Seg::Run { x0: x, x1: xb, high: lvl == 1 });
        return;
    }
    // columns: runs merged, busy pixels merged while their shade is the same
    let mut run: Option<(f32, bool)> = None; // (x0, high)
    let mut busy: Option<(f32, f32)> = None; // (x0, density)
    let mut unknown: Option<f32> = None;
    let flush_run = |run: &mut Option<(f32, bool)>, x: f32, out: &mut Vec<Seg>| {
        if let Some((x0, h)) = run.take() {
            out.push(Seg::Run { x0, x1: x, high: h });
        }
    };
    let flush_busy = |busy: &mut Option<(f32, f32)>, x: f32, out: &mut Vec<Seg>| {
        if let Some((x0, d)) = busy.take() {
            out.push(Seg::Busy { x0, x1: x, density: d });
        }
    };
    for (i, c) in lv.cols.iter().enumerate() {
        let x = i as f32;
        if !c.known {
            flush_run(&mut run, x, out);
            flush_busy(&mut busy, x, out);
            if unknown.is_none() {
                unknown = Some(x);
            }
            continue;
        }
        if let Some(x0) = unknown.take() {
            out.push(Seg::Unknown { x0, x1: x });
        }
        match c.count {
            0 => {
                flush_busy(&mut busy, x, out);
                let h = c.start == 1;
                match run {
                    Some((_, rh)) if rh == h => {}
                    _ => {
                        flush_run(&mut run, x, out);
                        run = Some((x, h));
                    }
                }
            }
            1 => {
                flush_busy(&mut busy, x, out);
                flush_run(&mut run, x + 0.5, out);
                out.push(Seg::Edge { x: x + 0.5 });
                run = Some((x + 0.5, c.end == 1));
            }
            n => {
                flush_run(&mut run, x, out);
                let d = density(n);
                match busy {
                    Some((_, bd)) if bd == d => {}
                    _ => {
                        flush_busy(&mut busy, x, out);
                        busy = Some((x, d));
                    }
                }
                // the level after the block
                let _ = c.end;
            }
        }
    }
    let x = lv.cols.len() as f32;
    flush_run(&mut run, x.min(xb.max(x)), out);
    flush_busy(&mut busy, x, out);
    if let Some(x0) = unknown.take() {
        out.push(Seg::Unknown { x0, x1: x });
    }
}
