//! Tiles: the time axis cut into tiles of `TILE_PX` pixels at a power-of-two zoom (2^z ticks a pixel, z the largest
//! with 2^z <= the view's ticks a pixel). A tile's shapes are made once (`tile_viewport` → `digital::read_view` →
//! `shapes::digital_segs`) and placed by `placement` at any zoom from 2^z to 2^(z+1) ticks a pixel, so panning and
//! zooming inside an octave make nothing new. `TileCache` keeps the tiles last used.

use crate::{Tick, Viewport};
use std::collections::HashMap;
use std::hash::Hash;

pub const TILE_PX: u32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileKey {
    /// 2^z ticks a pixel (z may be negative: zoomed past a tick a pixel)
    pub z: i32,
    pub index: i64,
}

pub fn zoom_of(tpp: f64) -> i32 {
    tpp.max(1e-9).log2().floor() as i32
}

pub fn tpp_of(z: i32) -> f64 {
    2f64.powi(z)
}

/// The tiles that cover a view, left to right.
pub fn tiles_for(vp: &Viewport, n_ticks: Tick) -> Vec<TileKey> {
    let z = zoom_of(vp.tpp);
    let span = tpp_of(z) * TILE_PX as f64;
    let t0 = vp.t0.max(0.0);
    let t1 = vp.t1().min(n_ticks as f64);
    if t1 <= t0 {
        return vec![];
    }
    let (a, b) = ((t0 / span).floor() as i64, ((t1 / span).ceil() as i64).max((t0 / span).floor() as i64 + 1));
    (a..b).map(|index| TileKey { z, index }).collect()
}

/// The view a tile is made in (its own pixels, 0..TILE_PX).
pub fn tile_viewport(k: TileKey) -> Viewport {
    let tpp = tpp_of(k.z);
    Viewport { t0: k.index as f64 * tpp * TILE_PX as f64, tpp, width: TILE_PX as f32 }
}

/// Where a tile's pixel 0 lands in the view and how wide its pixels are there: x_view = offset + x_tile * scale.
pub fn placement(k: TileKey, vp: &Viewport) -> (f32, f32) {
    let tv = tile_viewport(k);
    (vp.x_of(tv.t0), (tv.tpp / vp.tpp) as f32)
}

/// The tiles last used, by (lane, key); `sweep` drops those not used since the frame before.
pub struct TileCache<L: Eq + Hash + Copy, T> {
    map: HashMap<(L, TileKey), (u64, T)>,
    frame: u64,
    pub made: u64,
    pub reused: u64,
}

impl<L: Eq + Hash + Copy, T> Default for TileCache<L, T> {
    fn default() -> Self {
        TileCache { map: HashMap::new(), frame: 0, made: 0, reused: 0 }
    }
}

impl<L: Eq + Hash + Copy, T> TileCache<L, T> {
    pub fn get_or_make(&mut self, lane: L, k: TileKey, make: impl FnOnce() -> T) -> &T {
        let f = self.frame;
        let e = self.map.entry((lane, k));
        match e {
            std::collections::hash_map::Entry::Occupied(o) => {
                self.reused += 1;
                let v = o.into_mut();
                v.0 = f;
                &v.1
            }
            std::collections::hash_map::Entry::Vacant(v) => {
                self.made += 1;
                &v.insert((f, make())).1
            }
        }
    }
    /// The end of a frame: tiles not used in the last `keep` frames dropped.
    pub fn sweep(&mut self, keep: u64) {
        let f = self.frame;
        self.map.retain(|_, v| v.0 + keep >= f);
        self.frame += 1;
    }
    pub fn clear(&mut self) {
        self.map.clear();
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}
