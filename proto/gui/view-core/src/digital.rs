//! A digital line: its changes (`EdgeSource`) and its summary layers (`Lod`).
//!
//! A change at tick t: the line has its new level from t on. The level at t is the initial one flipped once for each
//! change at or before t.
//!
//! The summary: buckets of 2^shift ticks; per bucket the number of changes in it and the levels at its start and its
//! end. Level 0's buckets hold about `per_bucket` changes on average (`shift_for`), each level above twice as wide.
//! Any view reads one level where a bucket is at most a pixel wide: about `width` to `2 * width` buckets a line,
//! whatever the capture's size. Below level 0 the changes themselves are read (only those in view).

use crate::{MemSize, Tick, Viewport};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

/// A line's changes, in memory or made on the fly.
pub trait EdgeSource: Send + Sync {
    fn n_ticks(&self) -> Tick;
    fn initial(&self) -> u8;
    /// The changes in [t0, t1) in order, at most `cap` of them pushed; false when it stopped at `cap`.
    fn edges_in(&self, t0: Tick, t1: Tick, cap: usize, out: &mut Vec<Tick>) -> bool;
    /// The level just before tick t, when the source can tell it without reading from the start.
    fn level_before(&self, _t: Tick) -> Option<u8> {
        None
    }
    /// Per-pixel columns read straight from the changes (a source in memory: two binary searches a pixel), for a
    /// view before its summary is built; false when the source cannot.
    fn columns_direct(&self, _vp: &Viewport, _out: &mut Vec<Col>) -> bool {
        false
    }
    /// About how many changes the line has (to choose the summary's level 0).
    fn approx_edges(&self) -> u64;
    fn mem_bytes(&self) -> usize;
}

/// Changes held in memory, sorted.
pub struct VecEdges {
    pub initial: u8,
    pub n_ticks: Tick,
    pub edges: Vec<Tick>,
}

impl VecEdges {
    pub fn new(initial: u8, n_ticks: Tick, edges: Vec<Tick>) -> Self {
        VecEdges { initial: initial & 1, n_ticks, edges }
    }
    /// The number of changes before tick t.
    pub fn rank(&self, t: Tick) -> usize {
        self.edges.partition_point(|&e| e < t)
    }
}

impl EdgeSource for VecEdges {
    fn n_ticks(&self) -> Tick {
        self.n_ticks
    }
    fn initial(&self) -> u8 {
        self.initial
    }
    fn edges_in(&self, t0: Tick, t1: Tick, cap: usize, out: &mut Vec<Tick>) -> bool {
        let (a, b) = (self.rank(t0), self.rank(t1));
        let take = (b - a).min(cap);
        out.extend_from_slice(&self.edges[a..a + take]);
        take == b - a
    }
    fn level_before(&self, t: Tick) -> Option<u8> {
        Some(self.initial ^ (self.rank(t) & 1) as u8)
    }
    fn columns_direct(&self, vp: &Viewport, out: &mut Vec<Col>) -> bool {
        out.clear();
        let w = vp.width.max(1.0).ceil() as usize;
        let mut r0 = self.rank(vp.t0.floor() as Tick);
        for x in 0..w {
            let tb = vp.t_of(x as f32 + 1.0).floor() as Tick;
            let r1 = self.rank(tb).max(r0);
            let t = vp.t_of(x as f32);
            let start = self.initial ^ (r0 & 1) as u8;
            out.push(Col {
                count: (r1 - r0) as u32,
                start,
                end: self.initial ^ (r1 & 1) as u8,
                known: t >= 0.0 && t < self.n_ticks as f64,
            });
            r0 = r1;
        }
        true
    }
    fn approx_edges(&self) -> u64 {
        self.edges.len() as u64
    }
    fn mem_bytes(&self) -> usize {
        self.edges.capacity() * 8
    }
}

/// One level of the summary: buckets of 2^shift ticks.
#[derive(Default, Clone)]
pub struct LodLevel {
    pub shift: u32,
    /// changes in the bucket (saturating)
    pub count: Vec<u32>,
    /// bit 0: the level at the bucket's start, bit 1: at its end
    pub ends: Vec<u8>,
}

impl LodLevel {
    fn push(&mut self, count: u32, ends: u8) {
        self.count.push(count);
        self.ends.push(ends);
    }
    pub fn len(&self) -> usize {
        self.count.len()
    }
    pub fn is_empty(&self) -> bool {
        self.count.is_empty()
    }
}

/// A line's summary layers (built from the start of the capture; `built_until` how far).
#[derive(Default, Clone)]
pub struct Lod {
    pub n_ticks: Tick,
    pub initial: u8,
    pub levels: Vec<LodLevel>,
    /// the summary covers ticks [0, built_until)
    pub built_until: Tick,
}

impl MemSize for Lod {
    fn mem_bytes(&self) -> usize {
        self.levels.iter().map(|l| l.count.capacity() * 4 + l.ends.capacity()).sum()
    }
}

/// Level 0's shift for `per_bucket` changes a bucket on average.
pub fn shift_for(n_ticks: Tick, n_edges: u64, per_bucket: u32) -> u32 {
    let ticks_per = (n_ticks.max(1) as f64) * per_bucket as f64 / (n_edges.max(1) as f64);
    (ticks_per.max(1.0).log2().floor() as u32).min(40)
}

fn combine(l: &LodLevel, i: usize) -> (u32, u8) {
    let (c0, e0) = (l.count[i], l.ends[i]);
    if i + 1 < l.len() {
        let (c1, e1) = (l.count[i + 1], l.ends[i + 1]);
        (c0.saturating_add(c1), (e0 & 1) | (e1 & 2))
    } else {
        (c0, e0)
    }
}

impl Lod {
    pub fn new(n_ticks: Tick, initial: u8, shift0: u32) -> Self {
        Lod {
            n_ticks,
            initial,
            levels: vec![LodLevel { shift: shift0, ..Default::default() }],
            built_until: 0,
        }
    }
    pub fn shift0(&self) -> u32 {
        self.levels[0].shift
    }
    /// Level 0's buckets appended; the levels above made from them as far as pairs are complete (all of them when
    /// `last`).
    pub fn append(&mut self, buckets: &[(u32, u8)], last: bool) {
        for &(c, e) in buckets {
            self.levels[0].push(c, e);
        }
        let mut k = 0;
        loop {
            let below_len = self.levels[k].len();
            if below_len <= 1 {
                break; // the top: one bucket over all that is built
            }
            if k + 1 == self.levels.len() {
                let shift = self.levels[k].shift + 1;
                self.levels.push(LodLevel { shift, ..Default::default() });
            }
            let (lo, hi) = self.levels.split_at_mut(k + 1);
            let (below, above) = (&lo[k], &mut hi[0]);
            // only whole pairs until the last append (then an odd one alone)
            let complete = if last { below_len.div_ceil(2) } else { below_len / 2 };
            while above.len() < complete {
                let (c, e) = combine(below, above.len() * 2);
                above.push(c, e);
            }
            k += 1;
        }
        let b0 = self.levels[0].len() as Tick;
        self.built_until = (b0 << self.shift0()).min(self.n_ticks);
        if last {
            self.built_until = self.n_ticks;
        }
    }

    /// The level just before tick t from the summary (the start of t's level-0 bucket) and the changes in between.
    pub fn level_before(&self, src: &dyn EdgeSource, t: Tick, scratch: &mut Vec<Tick>) -> u8 {
        if let Some(l) = src.level_before(t) {
            return l;
        }
        let l0 = &self.levels[0];
        let b = (t.max(0) >> l0.shift) as usize;
        if b >= l0.len() {
            return self.initial;
        }
        let start = (b as Tick) << l0.shift;
        scratch.clear();
        src.edges_in(start, t, usize::MAX, scratch);
        (l0.ends[b] & 1) ^ (scratch.len() & 1) as u8
    }

    /// About how many changes in [t0, t1) (from a level with at most ~64 buckets over the span; an over-estimate).
    pub fn estimate(&self, t0: Tick, t1: Tick) -> u64 {
        let span = (t1 - t0).max(1);
        let lv = self
            .levels
            .iter()
            .find(|l| (span >> l.shift) <= 64)
            .unwrap_or(self.levels.last().unwrap());
        let (a, b) = ((t0.max(0) >> lv.shift) as usize, ((t1 - 1).max(0) >> lv.shift) as usize);
        if a >= lv.len() {
            return u64::MAX; // not built yet
        }
        let b = b.min(lv.len() - 1);
        let mut s = 0u64;
        for i in a..=b {
            s += lv.count[i] as u64;
        }
        if ((t1 >> lv.shift) as usize) >= lv.len() && t1 > self.built_until {
            return u64::MAX;
        }
        s
    }
}

/// Builds a line's summary while its changes are read in order.
pub struct LodBuilder {
    shift0: u32,
    n_ticks: Tick,
    cur: Tick,
    cnt: u32,
    start: u8,
    lvl: u8,
    pending: Vec<(u32, u8)>,
}

impl LodBuilder {
    pub fn new(n_ticks: Tick, initial: u8, shift0: u32) -> Self {
        LodBuilder { shift0, n_ticks, cur: 0, cnt: 0, start: initial & 1, lvl: initial & 1, pending: vec![] }
    }
    fn close(&mut self) {
        self.pending.push((self.cnt, self.start | (self.lvl << 1)));
        self.cur += 1;
        self.cnt = 0;
        self.start = self.lvl;
    }
    #[inline]
    pub fn push(&mut self, t: Tick) {
        let b = t >> self.shift0;
        while self.cur < b {
            self.close();
        }
        self.cnt = self.cnt.saturating_add(1);
        self.lvl ^= 1;
    }
    /// Level 0's buckets closed before tick t (none of their changes come later).
    pub fn close_before(&mut self, t: Tick) {
        let b = t >> self.shift0;
        while self.cur < b {
            self.close();
        }
    }
    pub fn take_pending(&mut self) -> Vec<(u32, u8)> {
        std::mem::take(&mut self.pending)
    }
    /// The rest of level 0 (to the capture's end).
    pub fn finish(&mut self) -> Vec<(u32, u8)> {
        let n_buckets = (self.n_ticks + (1 << self.shift0) - 1) >> self.shift0;
        while self.cur < n_buckets {
            self.close();
        }
        self.take_pending()
    }
}

/// Ticks read a step while building (a block of level-0 buckets).
fn block_ticks(shift0: u32) -> Tick {
    (1 as Tick) << (shift0 + 14).min(40)
}

/// A line's summary built at once.
pub fn build_lod(src: &dyn EdgeSource, shift0: u32) -> Lod {
    let n = src.n_ticks();
    let mut lod = Lod::new(n, src.initial(), shift0);
    let mut b = LodBuilder::new(n, src.initial(), shift0);
    let mut buf = Vec::new();
    let step = block_ticks(shift0);
    let mut t = 0;
    while t < n {
        let t1 = (t + step).min(n);
        buf.clear();
        src.edges_in(t, t1, usize::MAX, &mut buf);
        for &e in &buf {
            b.push(e);
        }
        b.close_before(t1);
        t = t1;
    }
    let rest = b.finish();
    lod.append(&rest, true);
    lod
}

/// A line's summary built into `shared` a block at a time, so a view can draw what is built while the rest is read.
/// Stops early when `cancel` is set.
pub fn build_lod_shared(src: &dyn EdgeSource, shift0: u32, shared: &RwLock<Lod>, cancel: &AtomicBool) {
    let n = src.n_ticks();
    *shared.write().unwrap() = Lod::new(n, src.initial(), shift0);
    let mut b = LodBuilder::new(n, src.initial(), shift0);
    let mut buf = Vec::new();
    let step = block_ticks(shift0) * 4;
    let mut t = 0;
    while t < n {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let t1 = (t + step).min(n);
        buf.clear();
        src.edges_in(t, t1, usize::MAX, &mut buf);
        for &e in &buf {
            b.push(e);
        }
        b.close_before(t1);
        let got = b.take_pending();
        if t1 < n {
            shared.write().unwrap().append(&got, false);
        } else {
            let mut all = got;
            all.extend(b.finish());
            shared.write().unwrap().append(&all, true);
        }
        t = t1;
    }
    if n == 0 {
        shared.write().unwrap().append(&[], true);
    }
}

/// A pixel column of a line: the changes in it, the levels at its start and end, whether it is known (summarized).
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Col {
    pub count: u32,
    pub start: u8,
    pub end: u8,
    pub known: bool,
}

/// A line read for a view: its exact changes when few are in view, else per-pixel columns.
#[derive(Default, Clone, Debug)]
pub struct LineView {
    /// the level at the view's first tick (exact) or of the first column
    pub start_level: u8,
    /// the first and last tick in view, inside the capture
    pub ta: Tick,
    pub tb: Tick,
    /// the changes in view (when `exact`)
    pub edges: Vec<Tick>,
    pub exact: bool,
    pub cols: Vec<Col>,
    /// which level the columns came from (-1: the changes themselves)
    pub level: i32,
}

/// Reads a line for a view (`max_exact` changes at most drawn one by one; about `width` is a good choice).
pub fn read_view(src: &dyn EdgeSource, lod: &Lod, vp: &Viewport, max_exact: usize, out: &mut LineView) {
    out.edges.clear();
    out.cols.clear();
    out.exact = false;
    out.level = -1;
    let n = src.n_ticks();
    let ta = vp.t0.floor().max(0.0) as Tick;
    let tb = (vp.t1().ceil() as Tick).min(n);
    out.ta = ta;
    out.tb = tb;
    if tb <= ta {
        return;
    }
    let width = vp.width.max(1.0).ceil() as usize;
    let built = lod.built_until;
    let base = (1 as Tick) << lod.shift0();
    // few changes in view: each one drawn where it is (a source in memory is asked at once: it stops at the cap)
    let in_memory = src.level_before(0).is_some();
    let est = if tb <= built { lod.estimate(ta, tb) } else { u64::MAX };
    if in_memory || est <= (max_exact as u64) * 4 || (vp.tpp < base as f64 && tb <= built) {
        if src.edges_in(ta, tb, max_exact, &mut out.edges) {
            out.start_level = lod.level_before(src, ta, &mut Vec::new());
            out.exact = true;
            return;
        }
        out.edges.clear();
    }
    // the summary not built so far: a source in memory reads its columns itself
    if tb > built && src.columns_direct(vp, &mut out.cols) {
        out.start_level = out.cols.first().map(|c| c.start).unwrap_or(0);
        out.level = -2;
        return;
    }
    out.cols.resize(width, Col::default());
    let tpp = vp.tpp;
    // the level with buckets at most a pixel wide (or level 0, coarser than a pixel, when the changes are too many)
    let k = lod.levels.iter().rposition(|l| ((1u64 << l.shift) as f64) <= tpp).unwrap_or(0);
    let lv = &lod.levels[k];
    out.level = k as i32;
    let w = (1 as Tick) << lv.shift;
    let b_first = (ta >> lv.shift) as usize;
    let b_last = (((tb - 1) >> lv.shift) as usize).min(lv.len().saturating_sub(1));
    let mut filled = vec![false; width];
    if lv.len() > 0 && b_first < lv.len() {
        if (w as f64) <= tpp {
            for b in b_first..=b_last {
                let bt = (b as Tick) << lv.shift;
                let x = (((bt.max(ta)) as f64 - vp.t0) / tpp).floor();
                if x < 0.0 || x >= width as f64 {
                    continue;
                }
                let x = x as usize;
                let c = &mut out.cols[x];
                let (cnt, ends) = (lv.count[b], lv.ends[b]);
                if !filled[x] {
                    *c = Col { count: cnt, start: ends & 1, end: ends >> 1, known: true };
                    filled[x] = true;
                } else {
                    c.count = c.count.saturating_add(cnt);
                    c.end = ends >> 1;
                }
            }
        } else {
            // buckets wider than a pixel: each spread over the pixels it covers
            for b in b_first..=b_last {
                let bt0 = (b as Tick) << lv.shift;
                let bt1 = bt0 + w;
                let x0 = ((bt0 as f64 - vp.t0) / tpp).floor().max(0.0) as usize;
                let x1 = (((bt1 as f64 - vp.t0) / tpp).ceil().max(0.0) as usize).min(width);
                let (cnt, ends) = (lv.count[b], lv.ends[b]);
                let span = (x1.saturating_sub(x0)).max(1) as u32;
                for x in x0..x1 {
                    let per = if cnt == 0 { 0 } else { (cnt / span).max(2) };
                    out.cols[x] = Col {
                        count: per,
                        start: if x == x0 { ends & 1 } else { ends >> 1 },
                        end: ends >> 1,
                        known: true,
                    };
                    filled[x] = true;
                }
            }
        }
    }
    // columns with no bucket starting in them carry the level on; past what is built: unknown
    let mut lvl = lod.level_before(src, ta, &mut Vec::new());
    out.start_level = lvl;
    for x in 0..width {
        let t = vp.t_of(x as f32);
        if filled[x] {
            lvl = out.cols[x].end;
        } else if t < built as f64 && t < n as f64 {
            out.cols[x] = Col { count: 0, start: lvl, end: lvl, known: true };
        } else {
            out.cols[x] = Col { known: false, ..Default::default() };
        }
    }
}

/// The level at tick t (after the changes at or before it), and the changes around t for the cursor's measure:
/// the last change at or before t and the next two after it.
pub fn around(src: &dyn EdgeSource, lod: &Lod, t: Tick) -> (u8, Option<Tick>, Option<Tick>, Option<Tick>) {
    let mut scratch = Vec::new();
    let lvl = lod.level_before(src, t + 1, &mut scratch);
    // the previous change: look back in growing windows (the first with a change in it has few)
    let mut prev = None;
    let mut span: Tick = 64;
    loop {
        let a = (t + 1 - span).max(0);
        let mut v = Vec::new();
        src.edges_in(a, t + 1, 1 << 20, &mut v);
        if let Some(&e) = v.last() {
            prev = Some(e);
            break;
        }
        if a == 0 {
            break;
        }
        span = span.saturating_mul(8);
    }
    let mut next = Vec::new();
    let mut span: Tick = 64;
    let n = src.n_ticks();
    while next.len() < 2 && t + 1 < n {
        next.clear();
        src.edges_in(t + 1, (t + 1 + span).min(n), 2, &mut next);
        if next.len() >= 2 || t + 1 + span >= n {
            break;
        }
        span *= 8;
    }
    (lvl, prev, next.first().copied(), next.get(1).copied())
}
