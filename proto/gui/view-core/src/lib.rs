//! view-core (prototype): the parts of drawing a capture that do not depend on the screens
//! (bench/notes/2026-11-25-gui-study.ja.md §3). Plain Rust without dependencies, so it builds for wasm too and every
//! prototype (egui now, TS + WebGPU next) draws from the same parts.
//!
//! - `digital`: a line's changes (`EdgeSource`: in memory or made on the fly) and its summary layers (`Lod`: per
//!   bucket of 2^k ticks the level at its start and end and the number of changes), read per pixel column.
//! - `analog`: a channel's samples and their min / max / mean layers.
//! - `ann`: annotation items (transactions, frames, bits) indexed by time, laid out for a view (text folded where it
//!   does not fit, many items in a pixel merged).
//! - `shapes`: what to draw for a line in a view: flat runs, edges, busy blocks (many changes in a pixel).
//! - `tiles`: the view cut into tiles of 256 px at a power-of-two zoom, built once and reused while panning.
//! - `synth`: made-up captures (1e8-1e9 changes made on the fly; an I2C bus with its transactions).

pub mod analog;
pub mod ann;
pub mod digital;
pub mod shapes;
pub mod synth;
pub mod tiles;

/// A time in ticks of the capture.
pub type Tick = i64;

/// What is in view: the tick at the left edge, ticks a pixel, the width in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    pub t0: f64,
    pub tpp: f64,
    pub width: f32,
}

impl Viewport {
    pub fn new(t0: f64, tpp: f64, width: f32) -> Self {
        Viewport { t0, tpp, width }
    }
    /// The view that shows ticks [0, n) across `width` pixels.
    pub fn fit(n: Tick, width: f32) -> Self {
        Viewport { t0: 0.0, tpp: (n.max(1) as f64) / width.max(1.0) as f64, width }
    }
    pub fn t1(&self) -> f64 {
        self.t0 + self.tpp * self.width as f64
    }
    pub fn x_of(&self, t: f64) -> f32 {
        ((t - self.t0) / self.tpp) as f32
    }
    pub fn t_of(&self, x: f32) -> f64 {
        self.t0 + x as f64 * self.tpp
    }
    /// Zoomed by `factor` (>1: in) about the pixel `x`.
    pub fn zoomed(&self, factor: f64, x: f32) -> Self {
        let t = self.t_of(x);
        let tpp = (self.tpp / factor).max(1e-3);
        Viewport { t0: t - x as f64 * tpp, tpp, width: self.width }
    }
    /// Kept inside [0, n) as far as the zoom allows.
    pub fn clamped(&self, n: Tick) -> Self {
        let span = self.tpp * self.width as f64;
        let n = n as f64;
        let mut v = *self;
        if span >= n {
            v.tpp = v.tpp.min(n / self.width.max(1.0) as f64 * 1.0);
            let span = v.tpp * v.width as f64;
            v.t0 = (n - span) / 2.0;
            if span >= n {
                v.t0 = 0.0;
            }
            return v;
        }
        v.t0 = v.t0.clamp(0.0, n - span);
        v
    }
}

/// The bytes a value takes in memory (for the bench and the status bar).
pub trait MemSize {
    fn mem_bytes(&self) -> usize;
}
