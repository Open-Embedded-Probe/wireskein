//! A waveform view's frame as GPU instances (gpu.rs) and labels: prototype 2's `Scene.frame`
//! (proto/gui-web/wasm/view) on prototype 1's model (the lines' summaries shared with the threads that build them, the
//! analysis' tracks read where they are). Text is not drawn here: labels say where and what (the widget draws them).

use crate::app::Sel;
use crate::model::{Analysis, Capture};
use crate::theme::*;
use std::collections::HashMap;
use view_core::analog::AnalogView;
use view_core::ann::{AnnShape, Fit, Status};
use view_core::digital::{LineView, read_view};
use view_core::shapes::{Seg, digital_segs};
use view_core::tiles::{TileCache, placement, tile_viewport, tiles_for};
use view_core::{Tick, Viewport};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lane {
    Dig(usize),
    Ana(usize),
    Ann { claim: usize, layer: usize },
    Unread { claim: usize },
}

pub struct Label {
    pub x: f32,
    pub y: f32,
    pub color: usize,
    /// a range of `Out::text`
    pub a: u32,
    pub b: u32,
}

#[derive(Default)]
pub struct Out {
    pub inst: Vec<f32>,
    pub labels: Vec<Label>,
    pub text: String,
    clip: [f32; 4],
}

impl Out {
    pub fn clear(&mut self) {
        self.inst.clear();
        self.labels.clear();
        self.text.clear();
    }
    pub fn set_clip(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.clip = [x0, y0, x1, y1];
    }
    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: usize, alpha: f32) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        let (x0, x1, y0, y1) = (x0.max(cx0), x1.min(cx1), y0.max(cy0), y1.min(cy1));
        if x1 > x0 && y1 > y0 {
            self.inst
                .extend_from_slice(&[x0, y0, x1, y1, color as f32, alpha, 0.0, 0.0]);
        }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn seg(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: usize, alpha: f32, w: f32) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        if x0.max(x1) < cx0 || x0.min(x1) > cx1 || y0.max(y1) < cy0 || y0.min(y1) > cy1 {
            return;
        }
        let (mut a, mut b) = ((x0, y0), (x1, y1));
        if a.0 > b.0 {
            std::mem::swap(&mut a, &mut b);
        }
        let at = |p: (f32, f32), q: (f32, f32), x: f32| (x, p.1 + (q.1 - p.1) * (x - p.0) / (q.0 - p.0));
        if a.0 < cx0 {
            a = at(a, b, cx0);
        }
        if b.0 > cx1 {
            b = at(a, b, cx1);
        }
        self.inst
            .extend_from_slice(&[a.0, a.1, b.0, b.1, color as f32, alpha, 1.0, w]);
    }
    pub fn outline(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: usize, w: f32) {
        self.rect(x0, y0, x1, y0 + w, color, 1.0);
        self.rect(x0, y1 - w, x1, y1, color, 1.0);
        self.rect(x0, y0, x0 + w, y1, color, 1.0);
        self.rect(x1 - w, y0, x1, y1, color, 1.0);
    }
    pub fn label(&mut self, x: f32, y: f32, color: usize, text: &str) {
        let a = self.text.len() as u32;
        self.text.push_str(text);
        self.labels.push(Label {
            x,
            y,
            color,
            a,
            b: self.text.len() as u32,
        });
    }
    pub fn label_text(&self, l: &Label) -> &str {
        &self.text[l.a as usize..l.b as usize]
    }
}

/// What a view keeps between frames: its tiles, the minimap's columns, scratch.
#[derive(Default)]
pub struct SceneCache {
    pub tiles: TileCache<usize, Vec<Seg>>,
    pub out: Out,
    lv: LineView,
    segs: Vec<Seg>,
    ann: Vec<AnnShape>,
    minimap_key: (usize, Tick, usize),
    minimap: Vec<u64>,
    /// shapes made in the last frame (digital segments, analog points, annotation boxes)
    pub shapes: usize,
}

/// Where things go (logical px of the widget).
pub struct Geo {
    /// the plot's left edge and width
    pub px: f32,
    pub pw: f32,
    /// the lanes' area, top and bottom
    pub top: f32,
    pub bottom: f32,
    /// the minimap: x, y, w, h (w 0: none)
    pub mm: [f32; 4],
    pub char_px: f32,
    pub tiles: bool,
}

/// The search's hits as the scene reads them: rows per (claim, layer), sorted; ticks of the coarsest layers' hits.
pub struct Hits<'a> {
    pub by_layer: &'a HashMap<(usize, usize), Vec<usize>>,
    pub ticks: &'a [i64],
}

/// One frame of a view into `sc.out`. `lanes`: (lane, y, h, alternate shade).
#[allow(clippy::too_many_arguments)]
pub fn frame(
    sc: &mut SceneCache,
    cap: &Capture,
    an: Option<&Analysis>,
    hits: &Hits,
    lanes: &[(Lane, f32, f32, bool)],
    vp: &Viewport,
    g: &Geo,
    sel: Option<Sel>,
    grid: &[f32],
) {
    sc.out.clear();
    let mut n_shapes = 0usize;
    if g.mm[2] > 0.0 {
        minimap(sc, cap, an, hits, vp, g.mm);
    }
    sc.out.set_clip(0.0, g.top, g.px + g.pw, g.bottom);
    for &(_, y, h, alt) in lanes {
        if alt {
            sc.out.rect(0.0, y, g.px + g.pw, y + h, LANE_ALT, 1.0);
        }
    }
    sc.out.set_clip(g.px, g.top, g.px + g.pw, g.bottom);
    for &x in grid {
        sc.out.rect(x, g.top, x + 1.0, g.bottom, GRID, 0.6);
    }
    for &(lane, y, h, _) in lanes {
        if y + h < g.top || y > g.bottom {
            continue;
        }
        sc.out.set_clip(g.px, g.top, g.px + g.pw, g.bottom);
        match lane {
            Lane::Dig(i) => n_shapes += digital_lane(sc, cap, i, vp, g.px, y, h, g.tiles),
            Lane::Ana(i) => n_shapes += analog_lane(sc, cap, i, vp, g.px, y, h),
            Lane::Ann { claim, layer } => {
                if let Some(a) = an {
                    n_shapes += ann_lane(sc, a, hits, claim, layer, vp, g, y, h, sel);
                }
            }
            Lane::Unread { claim } => {
                if let Some(u) = an.and_then(|a| a.claims.get(claim)).and_then(|c| c.unread.as_ref()) {
                    u.layout(vp, 100.0, &mut sc.ann);
                    for s in &sc.ann {
                        let (x0, x1) = match *s {
                            AnnShape::Box { x0, x1, .. } | AnnShape::Busy { x0, x1, .. } => (x0, x1),
                        };
                        sc.out.rect(
                            g.px + x0,
                            y + 1.0,
                            (g.px + x1).max(g.px + x0 + 1.0),
                            y + h - 1.0,
                            ERROR,
                            0.8,
                        );
                    }
                }
            }
        }
    }
    sc.tiles.sweep(8);
    sc.shapes = n_shapes;
}

#[allow(clippy::too_many_arguments)]
fn digital_lane(
    sc: &mut SceneCache,
    cap: &Capture,
    i: usize,
    vp: &Viewport,
    px: f32,
    y: f32,
    h: f32,
    tiles_on: bool,
) -> usize {
    let n = cap.n_ticks;
    let (y_hi, y_lo) = (y + 5.0, y + h - 5.0);
    let line = &cap.lines[i];
    let lod = line.lod.read().unwrap();
    let complete = lod.built_until >= n;
    let mut count = 0;
    let (cl, cr) = (px, px + vp.width);
    if tiles_on {
        for key in tiles_for(vp, n) {
            let make = || {
                let tv = tile_viewport(key);
                let mut lv = LineView::default();
                read_view(line.src.as_ref(), &lod, &tv, 256, &mut lv);
                let mut s = vec![];
                digital_segs(&lv, &tv, &mut s);
                s
            };
            let (off, scale) = placement(key, vp);
            if complete {
                let segs = sc.tiles.get_or_make(i, key, make);
                count += segs.len();
                draw_segs(&mut sc.out, segs, px + off, scale, cl, cr, y_hi, y_lo);
            } else {
                let segs = make();
                count += segs.len();
                draw_segs(&mut sc.out, &segs, px + off, scale, cl, cr, y_hi, y_lo);
            }
        }
    } else {
        read_view(line.src.as_ref(), &lod, vp, vp.width as usize, &mut sc.lv);
        digital_segs(&sc.lv, vp, &mut sc.segs);
        count += sc.segs.len();
        draw_segs(&mut sc.out, &sc.segs, px, 1.0, cl, cr, y_hi, y_lo);
    }
    count
}

fn analog_lane(sc: &mut SceneCache, cap: &Capture, i: usize, vp: &Viewport, px: f32, y: f32, h: f32) -> usize {
    let a = &cap.analog[i];
    let (lo, hi) = (a.min, a.max.max(a.min + 1e-6));
    let ymap = |v: f32| y + h - 4.0 - (v - lo) / (hi - lo) * (h - 8.0);
    match a.read_view(vp) {
        AnalogView::Points(pts) => {
            for w in pts.windows(2) {
                sc.out
                    .seg(px + w[0].0, ymap(w[0].1), px + w[1].0, ymap(w[1].1), ANALOG, 1.0, 1.5);
            }
            pts.len()
        }
        AnalogView::Bands(bs) => {
            let mut prev: Option<(f32, f32)> = None;
            for (x, b) in bs.iter().enumerate() {
                if let Some(b) = b {
                    let x = px + x as f32;
                    sc.out
                        .rect(x, ymap(b.max) - 0.5, x + 1.0, ymap(b.min) + 0.5, ANALOG, 0.45);
                    let p = (x + 0.5, ymap(b.mean));
                    if let Some(q) = prev {
                        sc.out.seg(q.0, q.1, p.0, p.1, ANALOG, 1.0, 1.0);
                    }
                    prev = Some(p);
                } else {
                    prev = None;
                }
            }
            bs.len()
        }
    }
}

fn status_color(s: Status) -> usize {
    if s == Status::Ok { OK } else { 10 + (s as usize).min(4) }
}

#[allow(clippy::too_many_arguments)]
fn ann_lane(
    sc: &mut SceneCache,
    an: &Analysis,
    hits: &Hits,
    c: usize,
    l: usize,
    vp: &Viewport,
    g: &Geo,
    y: f32,
    h: f32,
    sel: Option<Sel>,
) -> usize {
    let Some(layer) = an.claims.get(c).and_then(|cl| cl.layers.get(l)) else {
        return 0;
    };
    let tr = &layer.track;
    tr.layout(vp, g.char_px, &mut sc.ann);
    let hl = hits.by_layer.get(&(c, l));
    let (y0, y1) = (y + 1.5, y + h - 1.5);
    let (px, right) = (g.px, g.px + g.pw);
    let sel_row = sel.filter(|s| s.claim == c && s.layer == l).map(|s| s.row as u32);
    for s in &sc.ann {
        match *s {
            AnnShape::Box {
                x0,
                x1,
                idx,
                fit,
                status,
            } => {
                let (a, b) = ((px + x0).max(px), (px + x1).min(right));
                sc.out.rect(a, y0, b, y1, status_color(status), 1.0);
                sc.out.rect(a, y0, a + 1.0, y1, TEXT, 0.35);
                if hl.is_some_and(|h| h.binary_search(&(idx as usize)).is_ok()) {
                    sc.out.outline(a, y0, b, y1, ACCENT, 2.0);
                }
                if sel_row == Some(idx) {
                    sc.out.outline(a - 1.0, y0 - 1.0, b + 1.0, y1 + 1.0, SEL, 2.0);
                }
                let t = match fit {
                    Fit::Full => Some(tr.text(idx as usize)),
                    Fit::Short => Some(tr.short(idx as usize)),
                    Fit::None => None,
                };
                if let Some(t) = t {
                    sc.out.label((a + b) / 2.0, (y0 + y1) / 2.0, TEXT, t);
                }
            }
            AnnShape::Busy { x0, x1, n: k, status } => {
                let (a, b) = ((px + x0).max(px), (px + x1).min(right));
                sc.out
                    .rect(a, y0 + 3.0, b.max(a + 1.0), y1 - 3.0, status_color(status), 0.85);
                if b - a > 40.0 {
                    sc.out.label((a + b) / 2.0, (y0 + y1) / 2.0, DIM, &format!("×{k}"));
                }
            }
        }
    }
    sc.ann.len()
}

/// Every line's changes a pixel over the whole capture (kept while nothing changes), the bad items of each claim's
/// coarsest layer, the search's hits, the view's place.
fn minimap(sc: &mut SceneCache, cap: &Capture, an: Option<&Analysis>, hits: &Hits, vp: &Viewport, mm: [f32; 4]) {
    let [mx, my, mw, mh] = mm;
    sc.out.set_clip(mx, my, mx + mw, my + mh);
    let w = mw.max(1.0) as usize;
    let built: Tick = cap.lines.iter().map(|l| l.lod.read().unwrap().built_until).sum();
    let key = (w, built, cap.lines.len());
    let fit = Viewport::fit(cap.n_ticks, w as f32);
    if key != sc.minimap_key {
        let mut acc = vec![0u64; w];
        for l in &cap.lines {
            let lod = l.lod.read().unwrap();
            read_view(l.src.as_ref(), &lod, &fit, 0, &mut sc.lv);
            for (x, c) in sc.lv.cols.iter().enumerate().take(w) {
                acc[x] += c.count as u64;
            }
        }
        sc.minimap = acc;
        sc.minimap_key = key;
    }
    sc.out.rect(mx, my, mx + mw, my + mh, LANE_ALT, 1.0);
    let maxc = sc.minimap.iter().copied().fold(1u64, u64::max) as f32;
    for (x, &c) in sc.minimap.iter().enumerate() {
        if c > 0 {
            let hh = (c as f32 / maxc).sqrt().clamp(0.08, 1.0) * (mh - 4.0);
            sc.out.rect(
                mx + x as f32,
                my + mh - 2.0 - hh,
                mx + x as f32 + 1.0,
                my + mh - 2.0,
                LINE,
                0.55,
            );
        }
    }
    if let Some(a) = an {
        for c in &a.claims {
            let Some(l) = c.layers.first() else { continue };
            l.track.layout(&fit, 7.0, &mut sc.ann);
            for s in &sc.ann {
                let (x0, x1, st) = match *s {
                    AnnShape::Busy { x0, x1, status, .. } | AnnShape::Box { x0, x1, status, .. } => (x0, x1, status),
                };
                if st >= Status::Warn {
                    sc.out.rect(
                        mx + x0,
                        my + 1.0,
                        mx + x1.max(x0 + 1.5),
                        my + 7.0,
                        status_color(st),
                        1.0,
                    );
                }
            }
        }
    }
    let n = cap.n_ticks.max(1) as f64;
    for &t in hits.ticks {
        let x = mx + (t as f64 / n * mw as f64) as f32;
        sc.out.rect(x, my + mh - 6.0, x + 1.0, my + mh, ACCENT, 1.0);
    }
    let a = mx + (vp.t0 / n * mw as f64) as f32;
    let b = mx + (vp.t1() / n * mw as f64) as f32;
    let (a, b) = (a.max(mx), b.max(a + 2.0).min(mx + mw));
    sc.out.rect(a, my, b, my + mh, ACCENT, 0.18);
    sc.out.outline(a, my, b, my + mh, ACCENT, 1.0);
}

/// A line's shapes, x = off + seg.x * scale, clipped to [cl, cr] (prototype 1's draw_segs after §9.6).
#[allow(clippy::too_many_arguments)]
fn draw_segs(out: &mut Out, segs: &[Seg], off: f32, scale: f32, cl: f32, cr: f32, y_hi: f32, y_lo: f32) {
    let th = 1.5;
    let px = |x: f32| (off + x * scale).clamp(cl, cr);
    for s in segs {
        match *s {
            Seg::Run { x0, x1, high } => {
                let y = if high { y_hi } else { y_lo };
                out.rect(px(x0), y - th / 2.0, px(x1).max(px(x0) + 0.5), y + th / 2.0, LINE, 1.0);
            }
            Seg::Edge { x } => {
                let x = px(x);
                if x > cl && x < cr {
                    out.rect(x - 0.5, y_hi - th / 2.0, x + 0.6, y_lo + th / 2.0, LINE, 1.0);
                }
            }
            Seg::Busy { x0, x1, density } => {
                let (a, b) = (px(x0), px(x1).max(px(x0) + 1.0));
                out.rect(a, y_hi, b, y_lo, LINE, 0.30 + 0.65 * density);
                out.rect(a, y_hi - th / 2.0, b, y_hi + th / 2.0, LINE, 1.0);
                out.rect(a, y_lo - th / 2.0, b, y_lo + th / 2.0, LINE, 1.0);
            }
            Seg::Unknown { x0, x1 } => {
                out.rect(px(x0), y_hi, px(x1), y_lo, UNREAD, 0.25);
            }
        }
    }
}
