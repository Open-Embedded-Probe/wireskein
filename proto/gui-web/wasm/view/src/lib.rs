//! Prototype 2 (web): the waveform's frame on the screen's thread, in wasm (bench/notes/2026-11-25-gui-study.ja.md
//! §10). A throwaway.
//!
//! `Scene` holds what a view reads: the digital lines (changes in memory or made up on the fly) and their summaries
//! (level 0 handed over by the workers, the levels above built here), analog lines, annotation tracks (handed over
//! whole: `Track::from_parts`). `frame` turns the lanes the screen lays out into instances for the GPU: 8 floats
//! each, [x0, y0, x1, y1, color, alpha, kind, width] (kind 0 a rectangle, 1 a segment `width` px thick), in CSS px of
//! the canvas, colors as indices into the screen's palette (dark / light switched there). Text is not drawn here:
//! `labels` gives each label's place and `label_text` its text (the screen draws them on a 2D canvas above).

use std::collections::HashMap;
use view_core::analog::{AnalogLine, AnalogView};
use view_core::ann::{AnnShape, Fit, Item, Status, Track};
use view_core::digital::{EdgeSource, LineView, Lod, VecEdges, around, read_view};
use view_core::shapes::{Seg, digital_segs};
use view_core::synth::SynthEdges;
use view_core::tiles::{TileCache, placement, tile_viewport, tiles_for};
use view_core::{MemSize, Tick, Viewport};
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn error(s: &str);
}

#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|p| error(&format!("wsk-web-view: {p}"))));
}

/// A made-up capture of about `total` changes over `n_lines` lines: [n_ticks, then each line's period and segment
/// length] (the same as the engine's: the lines are made on the fly wherever they are read).
#[wasm_bindgen]
pub fn synth_plan(total: f64, n_lines: u32) -> Vec<f64> {
    let (n, ls) = view_core::synth::synth_lines(total as u64, n_lines as usize);
    let mut v = vec![n as f64];
    for l in ls {
        v.push(l.period as f64);
        v.push(l.seg_len as f64);
    }
    v
}

// the screen's palette (src/gpu.ts PALETTE_KEYS in the same order)
const BG: f32 = 0.0;
const LANE_ALT: f32 = 1.0;
const GRID: f32 = 2.0;
const TEXT: f32 = 3.0;
const DIM: f32 = 4.0;
const LINE: f32 = 5.0;
const ANALOG: f32 = 6.0;
const ACCENT: f32 = 7.0;
const OK: f32 = 10.0;
const ERROR: f32 = 13.0;
const UNREAD: f32 = 14.0;
const SEL: f32 = 15.0;

fn status_color(s: Status) -> f32 {
    10.0 + (s as u8).min(4) as f32
}

struct DLine {
    src: Box<dyn EdgeSource>,
    lod: Lod,
}

#[derive(Default)]
struct Out {
    inst: Vec<f32>,
    labels: Vec<f32>,
    text: String,
    clip: [f32; 4],
}

impl Out {
    fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: f32, alpha: f32) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        let (x0, x1, y0, y1) = (x0.max(cx0), x1.min(cx1), y0.max(cy0), y1.min(cy1));
        if x1 > x0 && y1 > y0 {
            self.inst.extend_from_slice(&[x0, y0, x1, y1, color, alpha, 0.0, 0.0]);
        }
    }
    fn seg(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: f32, alpha: f32, w: f32) {
        let [cx0, cy0, cx1, cy1] = self.clip;
        if x0.max(x1) < cx0 || x0.min(x1) > cx1 || y0.max(y1) < cy0 || y0.min(y1) > cy1 {
            return;
        }
        self.inst.extend_from_slice(&[x0, y0, x1, y1, color, alpha, 1.0, w]);
    }
    /// A frame `w` px wide inside the rectangle.
    fn outline(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, color: f32, w: f32) {
        self.rect(x0, y0, x1, y0 + w, color, 1.0);
        self.rect(x0, y1 - w, x1, y1, color, 1.0);
        self.rect(x0, y0, x0 + w, y1, color, 1.0);
        self.rect(x1 - w, y0, x1, y1, color, 1.0);
    }
    fn label(&mut self, cx: f32, cy: f32, color: f32, text: &str) {
        if !self.labels.is_empty() {
            self.text.push('\u{1f}');
        }
        self.labels.extend_from_slice(&[cx, cy, color]);
        self.text.push_str(text);
    }
}

#[wasm_bindgen]
pub struct Scene {
    n_ticks: Tick,
    lines: Vec<DLine>,
    analog: Vec<AnalogLine>,
    tracks: HashMap<(u32, u32), Track>,
    unread: HashMap<u32, Track>,
    hits: HashMap<(u32, u32), Vec<u32>>,
    hit_ticks: Vec<f64>,
    cache: TileCache<usize, Vec<Seg>>,
    out: Out,
    minimap_key: (usize, Tick, usize),
    minimap: Vec<u64>,
    lv: LineView,
    segs: Vec<Seg>,
    ann: Vec<AnnShape>,
    /// shapes made in the last frame (for the speed panel)
    pub shapes: u32,
}

#[wasm_bindgen]
impl Scene {
    #[wasm_bindgen(constructor)]
    pub fn new(n_ticks: f64) -> Scene {
        Scene {
            n_ticks: n_ticks as Tick,
            lines: vec![],
            analog: vec![],
            tracks: HashMap::new(),
            unread: HashMap::new(),
            hits: HashMap::new(),
            hit_ticks: vec![],
            cache: TileCache::default(),
            out: Out::default(),
            minimap_key: (usize::MAX, 0, 0),
            minimap: vec![],
            lv: LineView::default(),
            segs: vec![],
            ann: vec![],
            shapes: 0,
        }
    }

    // ------------------------------------------------------------------ the capture

    /// A line whose changes are in memory (a file's).
    pub fn add_vec_line(&mut self, initial: u8, edges: Vec<i64>) -> usize {
        let src = VecEdges::new(initial, self.n_ticks, edges);
        self.lines.push(DLine { lod: Lod::new(self.n_ticks, initial & 1, 0), src: Box::new(src) });
        self.lines.len() - 1
    }
    /// A made-up line (made on the fly here too: nothing is handed over but its summary).
    pub fn add_synth_line(&mut self, line: u32, period: f64, seg_len: f64) -> usize {
        let src = SynthEdges { line: line as u64, n_ticks: self.n_ticks, period: period as Tick, seg_len: seg_len as Tick };
        self.lines.push(DLine { lod: Lod::new(self.n_ticks, 0, 0), src: Box::new(src) });
        self.lines.len() - 1
    }
    /// A line's summary started over at level 0's shift (level 0 reserved whole: no growth to double it).
    pub fn lod_begin(&mut self, i: usize, shift0: u32) {
        let l = &mut self.lines[i];
        l.lod = Lod::new(self.n_ticks, l.src.initial(), shift0);
        let nb = ((self.n_ticks + (1 << shift0) - 1) >> shift0).max(0) as usize;
        l.lod.levels[0].count.reserve_exact(nb);
        l.lod.levels[0].ends.reserve_exact(nb);
    }
    /// Level 0's buckets appended (the levels above made here); `last` at the line's end.
    pub fn lod_append(&mut self, i: usize, counts: &[u32], ends: &[u8], last: bool) {
        let b: Vec<(u32, u8)> = counts.iter().copied().zip(ends.iter().copied()).collect();
        self.lines[i].lod.append(&b, last);
    }
    pub fn lod_built(&self, i: usize) -> f64 {
        self.lines[i].lod.built_until as f64
    }
    pub fn add_analog(&mut self, name: &str, unit: &str, t0: f64, dt: f64, values: Vec<f32>) -> usize {
        self.analog.push(AnalogLine::new(name, unit, t0, dt, values));
        self.analog.len() - 1
    }
    /// An analog line whose summary a worker built (the engine's AnalogParts: bands flat, min / max / mean each).
    #[allow(clippy::too_many_arguments)]
    pub fn add_analog_parts(&mut self, name: &str, unit: &str, t0: f64, dt: f64, values: Vec<f32>, levels: &[f32], lens: &[u32]) -> usize {
        let mut out = vec![];
        let mut k = 0;
        for &n in lens {
            let n = n as usize;
            out.push(
                levels[k..k + 3 * n]
                    .chunks_exact(3)
                    .map(|b| view_core::analog::Band { min: b[0], max: b[1], mean: b[2] })
                    .collect(),
            );
            k += 3 * n;
        }
        self.analog.push(AnalogLine::from_parts(name, unit, t0, dt, values, out));
        self.analog.len() - 1
    }
    pub fn n_lines(&self) -> usize {
        self.lines.len()
    }
    pub fn n_analog(&self) -> usize {
        self.analog.len()
    }
    pub fn analog_range(&self, i: usize) -> Vec<f32> {
        vec![self.analog[i].min, self.analog[i].max]
    }
    /// Bytes held here (changes, summaries, analog, annotations).
    pub fn mem_bytes(&self) -> f64 {
        let l: usize = self.lines.iter().map(|l| l.src.mem_bytes() + l.lod.mem_bytes()).sum();
        let a: usize = self.analog.iter().map(|a| a.mem_bytes()).sum();
        let t: usize = self.tracks.values().chain(self.unread.values()).map(|t| t.mem_bytes()).sum();
        (l + a + t) as f64
    }

    // ------------------------------------------------------------------ the analysis

    pub fn clear_analysis(&mut self) {
        self.tracks.clear();
        self.unread.clear();
        self.hits.clear();
        self.hit_ticks.clear();
    }
    /// A layer's track from the parts the analysis worker gave (items in order of start; texts' UTF-8 bytes and
    /// their offsets, two an item and one at the end).
    #[allow(clippy::too_many_arguments)]
    pub fn set_track(&mut self, c: u32, l: u32, name: &str, s: &[f64], e: &[f64], status: &[u8], buf: Vec<u8>, off: Vec<u32>) {
        let items: Vec<Item> = (0..s.len())
            .map(|i| Item { s: s[i] as Tick, e: e[i] as Tick, status: Status::from_u8(status[i]), id: i as u32 })
            .collect();
        let buf = String::from_utf8(buf).unwrap_or_default();
        self.tracks.insert((c, l), Track::from_parts(name, items, buf, off));
    }
    pub fn set_unread(&mut self, c: u32, s: &[f64], e: &[f64]) {
        let items: Vec<(Item, String, String)> = (0..s.len())
            .map(|i| (Item { s: s[i] as Tick, e: e[i] as Tick, status: Status::Unread, id: 0 }, String::new(), String::new()))
            .collect();
        self.unread.insert(c, Track::new("unread", items));
    }
    pub fn clear_hits(&mut self) {
        self.hits.clear();
        self.hit_ticks.clear();
    }
    pub fn set_hits(&mut self, c: u32, l: u32, rows: Vec<u32>) {
        self.hits.insert((c, l), rows);
    }
    pub fn set_hit_ticks(&mut self, t: Vec<f64>) {
        self.hit_ticks = t;
    }
    /// The item under tick t (-1: none).
    pub fn hit(&self, c: u32, l: u32, t: f64) -> i32 {
        self.tracks.get(&(c, l)).and_then(|tr| tr.hit(t as Tick)).map(|i| i as i32).unwrap_or(-1)
    }
    /// An item: [start, end, status].
    pub fn item(&self, c: u32, l: u32, i: u32) -> Vec<f64> {
        match self.tracks.get(&(c, l)) {
            Some(tr) if (i as usize) < tr.len() => {
                let it = tr.item(i as usize);
                vec![it.s as f64, it.e as f64, it.status as u8 as f64]
            }
            _ => vec![],
        }
    }
    pub fn item_text(&self, c: u32, l: u32, i: u32) -> String {
        self.tracks.get(&(c, l)).map(|tr| tr.text(i as usize).to_string()).unwrap_or_default()
    }
    /// The last item starting at or before t (-1: none).
    pub fn index_at(&self, c: u32, l: u32, t: f64) -> i32 {
        self.tracks.get(&(c, l)).and_then(|tr| tr.index_at(t as Tick)).map(|i| i as i32).unwrap_or(-1)
    }
    /// The next (or previous) item worth a look (Warn or worse) after item `after` (-1: after tick t).
    pub fn next_bad(&self, c: u32, l: u32, after: i32, t: f64, forward: bool) -> i32 {
        let Some(tr) = self.tracks.get(&(c, l)) else { return -1 };
        let a = (after >= 0).then_some(after as usize);
        let r = if forward { tr.next_bad(a, t as Tick) } else { tr.prev_bad(a, t as Tick) };
        r.map(|i| i as i32).unwrap_or(-1)
    }

    // ------------------------------------------------------------------ the cursor

    /// The pulse under the cursor: [level, previous change, next, the one after] (NaN: none); empty when the line
    /// is not summarised there yet.
    pub fn measure(&self, i: usize, t: f64) -> Vec<f64> {
        let l = &self.lines[i];
        if !(l.lod.built_until as f64 > t || l.src.level_before(0).is_some()) {
            return vec![];
        }
        let (lvl, p, n, n2) = around(l.src.as_ref(), &l.lod, t as Tick);
        let f = |x: Option<Tick>| x.map(|v| v as f64).unwrap_or(f64::NAN);
        vec![lvl as f64, f(p), f(n), f(n2)]
    }
    /// The changes of a line in [a, b) (at most `cap`; -1 when there are more).
    pub fn count_edges(&self, i: usize, a: f64, b: f64, cap: u32) -> f64 {
        let mut v = vec![];
        let all = self.lines[i].src.edges_in(a as Tick, b as Tick, cap as usize, &mut v);
        if all { v.len() as f64 } else { -1.0 }
    }

    // ------------------------------------------------------------------ the frame

    /// One frame of a waveform view. `vp`: [t0, ticks a pixel, plot x, plot width, lanes' top, lanes' bottom,
    /// minimap x, y, w, h, char px, tiles (0/1)]; `lanes`: 6 numbers a lane, [kind (0 digital, 1 analog, 2 an
    /// annotation layer, 3 a claim's unread stretches), a, b, y, h, alt]; `sel`: [claim, layer, row] or empty;
    /// `grid`: the time ruler's lines (x), drawn under the lanes.
    /// Returns the instances made (`inst_ptr`, 8 floats each); labels in `labels_ptr` (3 floats each: center x, y,
    /// color) and `label_text` (joined by U+001F).
    pub fn frame(&mut self, vp: &[f64], lanes: &[f64], sel: &[f64], grid: &[f64]) -> u32 {
        self.out.inst.clear();
        self.out.labels.clear();
        self.out.text.clear();
        let (t0, tpp, px, pw) = (vp[0], vp[1], vp[2] as f32, vp[3] as f32);
        let (top, bottom) = (vp[4] as f32, vp[5] as f32);
        let (mx, my, mw, mh) = (vp[6] as f32, vp[7] as f32, vp[8] as f32, vp[9] as f32);
        let char_px = vp[10] as f32;
        let tiles_on = vp[11] != 0.0;
        let view = Viewport { t0, tpp, width: pw };
        let n = self.n_ticks;
        let sel = (sel.len() == 3).then(|| (sel[0] as u32, sel[1] as u32, sel[2] as u32));
        let mut n_shapes = 0usize;
        if mw > 0.0 {
            self.minimap_frame(&view, mx, my, mw, mh);
        }
        self.out.clip = [0.0, top, px + pw, bottom];
        for ln in lanes.chunks_exact(6) {
            if ln[5] != 0.0 {
                self.out.rect(0.0, ln[3] as f32, px + pw, (ln[3] + ln[4]) as f32, LANE_ALT, 1.0);
            }
        }
        self.out.clip = [px, top, px + pw, bottom];
        for &x in grid {
            self.out.rect(x as f32, top, x as f32 + 1.0, bottom, GRID, 0.6);
        }
        for ln in lanes.chunks_exact(6) {
            let (kind, a, b, y, h) = (ln[0] as u32, ln[1] as usize, ln[2] as usize, ln[3] as f32, ln[4] as f32);
            if y + h < top || y > bottom {
                continue;
            }
            self.out.clip = [px, top, px + pw, bottom];
            match kind {
                0 => n_shapes += self.digital_lane(a, &view, px, y, h, tiles_on),
                1 => n_shapes += self.analog_lane(a, &view, px, y, h),
                2 => n_shapes += self.ann_lane(a as u32, b as u32, &view, px, pw, y, h, char_px, sel),
                _ => {
                    if let Some(u) = self.unread.get(&(a as u32)) {
                        u.layout(&view, 100.0, &mut self.ann);
                        for s in &self.ann {
                            let (x0, x1) = match *s {
                                AnnShape::Box { x0, x1, .. } | AnnShape::Busy { x0, x1, .. } => (x0, x1),
                            };
                            self.out.rect(px + x0, y + 1.0, (px + x1).max(px + x0 + 1.0), y + h - 1.0, ERROR, 0.8);
                        }
                    }
                }
            }
        }
        let _ = n;
        self.cache.sweep(8);
        self.shapes = n_shapes as u32;
        (self.out.inst.len() / 8) as u32
    }
    pub fn inst_ptr(&self) -> *const f32 {
        self.out.inst.as_ptr()
    }
    pub fn labels_ptr(&self) -> *const f32 {
        self.out.labels.as_ptr()
    }
    pub fn n_labels(&self) -> u32 {
        (self.out.labels.len() / 3) as u32
    }
    pub fn label_text(&self) -> String {
        self.out.text.clone()
    }
    pub fn tiles(&self) -> Vec<f64> {
        vec![self.cache.len() as f64, self.cache.made as f64, self.cache.reused as f64]
    }
}

impl Scene {
    fn digital_lane(&mut self, i: usize, vp: &Viewport, px: f32, y: f32, h: f32, tiles_on: bool) -> usize {
        let n = self.n_ticks;
        let (y_hi, y_lo) = (y + 5.0, y + h - 5.0);
        let line = &self.lines[i];
        let complete = line.lod.built_until >= n;
        let mut count = 0;
        let (cl, cr) = (px, px + vp.width);
        if tiles_on {
            for key in tiles_for(vp, n) {
                let make = || {
                    let tv = tile_viewport(key);
                    let mut lv = LineView::default();
                    read_view(line.src.as_ref(), &line.lod, &tv, 256, &mut lv);
                    let mut s = vec![];
                    digital_segs(&lv, &tv, &mut s);
                    s
                };
                let (off, scale) = placement(key, vp);
                if complete {
                    let segs = self.cache.get_or_make(i, key, make);
                    count += segs.len();
                    draw_segs(&mut self.out, segs, px + off, scale, cl, cr, y_hi, y_lo);
                } else {
                    let segs = make();
                    count += segs.len();
                    draw_segs(&mut self.out, &segs, px + off, scale, cl, cr, y_hi, y_lo);
                }
            }
        } else {
            read_view(line.src.as_ref(), &line.lod, vp, vp.width as usize, &mut self.lv);
            digital_segs(&self.lv, vp, &mut self.segs);
            count += self.segs.len();
            draw_segs(&mut self.out, &self.segs, px, 1.0, cl, cr, y_hi, y_lo);
        }
        count
    }

    fn analog_lane(&mut self, i: usize, vp: &Viewport, px: f32, y: f32, h: f32) -> usize {
        let a = &self.analog[i];
        let (lo, hi) = (a.min, a.max.max(a.min + 1e-6));
        let ymap = |v: f32| y + h - 4.0 - (v - lo) / (hi - lo) * (h - 8.0);
        match a.read_view(vp) {
            AnalogView::Points(pts) => {
                for w in pts.windows(2) {
                    self.out.seg(px + w[0].0, ymap(w[0].1), px + w[1].0, ymap(w[1].1), ANALOG, 1.0, 1.5);
                }
                pts.len()
            }
            AnalogView::Bands(bs) => {
                let mut prev: Option<(f32, f32)> = None;
                for (x, b) in bs.iter().enumerate() {
                    if let Some(b) = b {
                        let x = px + x as f32;
                        self.out.rect(x, ymap(b.max) - 0.5, x + 1.0, ymap(b.min) + 0.5, ANALOG, 0.45);
                        let p = (x + 0.5, ymap(b.mean));
                        if let Some(q) = prev {
                            self.out.seg(q.0, q.1, p.0, p.1, ANALOG, 1.0, 1.0);
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

    #[allow(clippy::too_many_arguments)]
    fn ann_lane(&mut self, c: u32, l: u32, vp: &Viewport, px: f32, pw: f32, y: f32, h: f32, char_px: f32, sel: Option<(u32, u32, u32)>) -> usize {
        let Some(tr) = self.tracks.get(&(c, l)) else { return 0 };
        tr.layout(vp, char_px, &mut self.ann);
        let hits = self.hits.get(&(c, l));
        let (y0, y1) = (y + 1.5, y + h - 1.5);
        let right = px + pw;
        for s in &self.ann {
            match *s {
                AnnShape::Box { x0, x1, idx, fit, status } => {
                    let (a, b) = ((px + x0).max(px), (px + x1).min(right));
                    let base = if status == Status::Ok { OK } else { status_color(status) };
                    self.out.rect(a, y0, b, y1, base, 1.0);
                    self.out.rect(a, y0, a + 1.0, y1, TEXT, 0.35);
                    if hits.is_some_and(|h| h.binary_search(&idx).is_ok()) {
                        self.out.outline(a, y0, b, y1, ACCENT, 2.0);
                    }
                    if sel == Some((c, l, idx)) {
                        self.out.outline(a - 1.0, y0 - 1.0, b + 1.0, y1 + 1.0, SEL, 2.0);
                    }
                    let t = match fit {
                        Fit::Full => Some(tr.text(idx as usize)),
                        Fit::Short => Some(tr.short(idx as usize)),
                        Fit::None => None,
                    };
                    if let Some(t) = t {
                        self.out.label((a + b) / 2.0, (y0 + y1) / 2.0, TEXT, t);
                    }
                }
                AnnShape::Busy { x0, x1, n: k, status } => {
                    let (a, b) = ((px + x0).max(px), (px + x1).min(right));
                    let base = if status == Status::Ok { OK } else { status_color(status) };
                    self.out.rect(a, y0 + 3.0, b.max(a + 1.0), y1 - 3.0, base, 0.85);
                    if b - a > 40.0 {
                        self.out.label((a + b) / 2.0, (y0 + y1) / 2.0, DIM, &format!("×{k}"));
                    }
                }
            }
        }
        self.ann.len()
    }

    /// The minimap: every line's changes a pixel over the whole capture (kept while nothing changes), the bad items
    /// of each claim's coarsest layer, the search's hits.
    fn minimap_frame(&mut self, vp: &Viewport, mx: f32, my: f32, mw: f32, mh: f32) {
        self.out.clip = [mx, my, mx + mw, my + mh];
        let w = mw.max(1.0) as usize;
        let built: Tick = self.lines.iter().map(|l| l.lod.built_until).sum();
        let key = (w, built, self.lines.len());
        let fit = Viewport::fit(self.n_ticks, w as f32);
        if key != self.minimap_key {
            let mut acc = vec![0u64; w];
            for l in &self.lines {
                read_view(l.src.as_ref(), &l.lod, &fit, 0, &mut self.lv);
                for (x, c) in self.lv.cols.iter().enumerate().take(w) {
                    acc[x] += c.count as u64;
                }
            }
            self.minimap = acc;
            self.minimap_key = key;
        }
        self.out.rect(mx, my, mx + mw, my + mh, LANE_ALT, 1.0);
        let maxc = self.minimap.iter().copied().fold(1u64, u64::max) as f32;
        for (x, &c) in self.minimap.iter().enumerate() {
            if c > 0 {
                let hh = (c as f32 / maxc).sqrt().clamp(0.08, 1.0) * (mh - 4.0);
                self.out.rect(mx + x as f32, my + mh - 2.0 - hh, mx + x as f32 + 1.0, my + mh - 2.0, LINE, 0.55);
            }
        }
        let mut c = 0u32;
        while let Some(tr) = self.tracks.get(&(c, 0)) {
            tr.layout(&fit, 7.0, &mut self.ann);
            for s in &self.ann {
                let (x0, x1, st) = match *s {
                    AnnShape::Busy { x0, x1, status, .. } | AnnShape::Box { x0, x1, status, .. } => (x0, x1, status),
                };
                if st >= Status::Warn {
                    self.out.rect(mx + x0, my + 1.0, mx + x1.max(x0 + 1.5), my + 7.0, status_color(st), 1.0);
                }
            }
            c += 1;
        }
        let n = self.n_ticks.max(1) as f64;
        for &t in &self.hit_ticks {
            let x = mx + (t / n * mw as f64) as f32;
            self.out.rect(x, my + mh - 6.0, x + 1.0, my + mh, ACCENT, 1.0);
        }
        let a = mx + (vp.t0 / n * mw as f64) as f32;
        let b = mx + (vp.t1() / n * mw as f64) as f32;
        let (a, b) = (a.max(mx), b.max(a + 2.0).min(mx + mw));
        self.out.rect(a, my, b, my + mh, ACCENT, 0.18);
        self.out.outline(a, my, b, my + mh, ACCENT, 1.0);
        let _ = BG;
    }
}

/// A line's shapes, x = off + seg.x * scale, clipped to [cl, cr] (prototype 1's draw_segs).
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
