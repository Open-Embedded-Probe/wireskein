//! An analog channel: its samples and their summary (min, max, mean of 4^k samples, k = 1, 2, ...), read per pixel
//! column as a band (min to max) with the mean, or as the samples themselves when few are in view.

use crate::{MemSize, Tick, Viewport};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Band {
    pub min: f32,
    pub max: f32,
    pub mean: f32,
}

pub struct AnalogLine {
    pub name: String,
    pub unit: String,
    /// the tick of sample 0 and ticks a sample
    pub t0: f64,
    pub dt: f64,
    pub values: Vec<f32>,
    /// level k: bands of 4^(k+1) samples
    levels: Vec<Vec<Band>>,
    pub min: f32,
    pub max: f32,
}

pub enum AnalogView {
    /// few samples in view: (x, value) each
    Points(Vec<(f32, f32)>),
    /// a band a pixel (None where no sample is)
    Bands(Vec<Option<Band>>),
}

impl AnalogLine {
    pub fn new(name: &str, unit: &str, t0: f64, dt: f64, values: Vec<f32>) -> Self {
        let mut levels: Vec<Vec<Band>> = vec![];
        let mut below: Vec<Band> = values.iter().map(|&v| Band { min: v, max: v, mean: v }).collect();
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &v in &values {
            if v.is_finite() {
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        while below.len() > 1 {
            let up: Vec<Band> = below
                .chunks(4)
                .map(|c| Band {
                    min: c.iter().map(|b| b.min).fold(f32::INFINITY, f32::min),
                    max: c.iter().map(|b| b.max).fold(f32::NEG_INFINITY, f32::max),
                    mean: c.iter().map(|b| b.mean).sum::<f32>() / c.len() as f32,
                })
                .collect();
            levels.push(up.clone());
            below = up;
        }
        AnalogLine { name: name.into(), unit: unit.into(), t0, dt, values, levels, min: lo, max: hi }
    }

    /// The summary's levels, to move the line to another thread or wasm instance (`from_parts` makes it again
    /// without building them): level k's bands of 4^(k+1) samples.
    pub fn levels(&self) -> &[Vec<Band>] {
        &self.levels
    }
    /// A line from its samples and the levels `levels` gave.
    pub fn from_parts(name: &str, unit: &str, t0: f64, dt: f64, values: Vec<f32>, levels: Vec<Vec<Band>>) -> Self {
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for b in levels.last().map(|l| &l[..]).unwrap_or(&[]) {
            lo = lo.min(b.min);
            hi = hi.max(b.max);
        }
        if levels.is_empty() {
            for &v in &values {
                if v.is_finite() {
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
        }
        AnalogLine { name: name.into(), unit: unit.into(), t0, dt, values, levels, min: lo, max: hi }
    }

    pub fn end_tick(&self) -> Tick {
        (self.t0 + self.dt * self.values.len() as f64).ceil() as Tick
    }

    pub fn read_view(&self, vp: &Viewport) -> AnalogView {
        let n = self.values.len();
        let s0 = ((vp.t0 - self.t0) / self.dt).floor().max(0.0) as usize;
        let s1 = (((vp.t1() - self.t0) / self.dt).ceil().max(0.0) as usize).min(n);
        let w = vp.width.max(1.0) as usize;
        if s1 <= s0 {
            return AnalogView::Bands(vec![None; w]);
        }
        if s1 - s0 <= 2 * w {
            // one point before and after the view too, so the line reaches its edges
            let a = s0.saturating_sub(1);
            let b = (s1 + 1).min(n);
            return AnalogView::Points(
                (a..b).map(|i| (vp.x_of(self.t0 + i as f64 * self.dt), self.values[i])).collect(),
            );
        }
        let spp = vp.tpp / self.dt; // samples a pixel
        // the coarsest level whose bands are at most a pixel wide
        let mut k: Option<usize> = None;
        let mut size = 4.0;
        for i in 0..self.levels.len() {
            if size <= spp {
                k = Some(i);
            }
            size *= 4.0;
        }
        let mut out: Vec<Option<Band>> = vec![None; w];
        let (src, per): (&[Band], usize) = match k {
            Some(i) => (&self.levels[i], 4usize.pow(i as u32 + 1)),
            None => (&[], 1),
        };
        let mut put = |x: f64, b: Band| {
            if x < 0.0 || x >= w as f64 {
                return;
            }
            let o = &mut out[x as usize];
            *o = Some(match *o {
                None => b,
                Some(a) => Band { min: a.min.min(b.min), max: a.max.max(b.max), mean: (a.mean + b.mean) * 0.5 },
            });
        };
        if per == 1 {
            for i in s0..s1 {
                let v = self.values[i];
                put(((self.t0 + i as f64 * self.dt - vp.t0) / vp.tpp).floor(), Band { min: v, max: v, mean: v });
            }
        } else {
            for j in s0 / per..s1.div_ceil(per).min(src.len()) {
                let t = self.t0 + (j * per) as f64 * self.dt;
                put(((t - vp.t0) / vp.tpp).floor(), src[j]);
            }
        }
        AnalogView::Bands(out)
    }
}

impl MemSize for AnalogLine {
    fn mem_bytes(&self) -> usize {
        self.values.capacity() * 4 + self.levels.iter().map(|l| l.capacity() * 12).sum::<usize>()
    }
}
