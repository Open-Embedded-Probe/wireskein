//! A small bench of view-core: build time, query time and memory of the summary layers, the annotation index and the
//! analog bands. `view-bench [TOTAL_CHANGES] [ITEMS]` (defaults 1e8 changes, 1e6 items).

use std::time::Instant;
use view_core::analog::AnalogLine;
use view_core::digital::{EdgeSource, LineView, VecEdges, build_lod, read_view, shift_for};
use view_core::shapes::digital_segs;
use view_core::synth::{fake_track, synth_analog, synth_lines};
use view_core::tiles::{TileCache, tile_viewport, tiles_for};
use view_core::{MemSize, Viewport};

fn rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1).and_then(|x| x.parse::<f64>().ok()))
        .map(|p| p * 4096.0 / 1e6)
        .unwrap_or(f64::NAN)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let total: u64 = args.get(1).map(|s| s.parse::<f64>().unwrap() as u64).unwrap_or(100_000_000);
    let n_items: usize = args.get(2).map(|s| s.parse::<f64>().unwrap() as usize).unwrap_or(1_000_000);
    let per_bucket: u32 = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(32);
    let width = 1920.0f32;
    println!("# view-core bench: {total:.0e} changes over 16 lines, {n_items} items, {per_bucket} changes a bucket");
    println!("rss at start: {:.0} MB", rss_mb());

    // the summary of made-up lines (made on the fly: only the summary in memory)
    let (n, lines) = synth_lines(total, 16);
    let t = Instant::now();
    let mut lods = vec![];
    let mut edges = 0u64;
    for l in &lines {
        let s0 = shift_for(n, l.approx_edges(), per_bucket);
        let lod = build_lod(l, s0);
        edges += lod.levels[0].count.iter().map(|&c| c as u64).sum::<u64>();
        lods.push(lod);
    }
    let build = t.elapsed().as_secs_f64();
    let lod_mb = lods.iter().map(|l| l.mem_bytes()).sum::<usize>() as f64 / 1e6;
    println!(
        "summary: {edges} changes, {n} ticks, built in {build:.2} s ({:.0} M changes/s, one thread), {lod_mb:.1} MB ({:.2} B a change), rss {:.0} MB",
        edges as f64 / build / 1e6,
        lod_mb * 1e6 / edges as f64,
        rss_mb()
    );

    // a frame: all 16 lines read and shaped, at zooms from the whole capture to a few ticks a pixel
    let mut lv = LineView::default();
    let mut segs = vec![];
    println!("| ticks/px | frame (16 lines) µs | shapes | mode |");
    println!("|---|---|---|---|");
    let full = Viewport::fit(n, width);
    for zoom in [1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e8] {
        let mut vp = full.zoomed(zoom, width * 0.37);
        vp.t0 = vp.t0.max(0.0);
        let reps = 20;
        let t = Instant::now();
        let mut nseg = 0;
        let mut mode = String::new();
        for _ in 0..reps {
            nseg = 0;
            for (l, lod) in lines.iter().zip(&lods) {
                read_view(l, lod, &vp, width as usize, &mut lv);
                digital_segs(&lv, &vp, &mut segs);
                nseg += segs.len();
            }
            mode = if lv.exact { "exact".into() } else { format!("level {}", lv.level) };
        }
        let us = t.elapsed().as_secs_f64() / reps as f64 * 1e6;
        println!("| {:.3} | {us:.0} | {nseg} | {mode} |", vp.tpp);
    }

    // tiles: a pan of 10 px a frame for 200 frames at one zoom
    let vp0 = full.zoomed(1e3, width * 0.37);
    let mut cache: TileCache<usize, Vec<view_core::shapes::Seg>> = TileCache::default();
    let t = Instant::now();
    let mut worst = 0f64;
    for f in 0..200 {
        let tf = Instant::now();
        let vp = Viewport { t0: vp0.t0 + f as f64 * 10.0 * vp0.tpp, ..vp0 };
        for (i, (l, lod)) in lines.iter().zip(&lods).enumerate() {
            for k in tiles_for(&vp, n) {
                cache.get_or_make(i, k, || {
                    let tv = tile_viewport(k);
                    let mut lv = LineView::default();
                    read_view(l, lod, &tv, 256, &mut lv);
                    let mut s = vec![];
                    digital_segs(&lv, &tv, &mut s);
                    s
                });
            }
        }
        cache.sweep(2);
        worst = worst.max(tf.elapsed().as_secs_f64());
    }
    println!(
        "tiles: 200 frames of a pan in {:.1} ms ({:.0} µs a frame, worst {:.0} µs), {} made, {} reused",
        t.elapsed().as_secs_f64() * 1e3,
        t.elapsed().as_secs_f64() / 200.0 * 1e6,
        worst * 1e6,
        cache.made,
        cache.reused
    );

    // changes in memory: 1e7 on one line, the summary against a binary search a pixel
    let (n2, l2) = synth_lines(10_000_000, 1);
    let mut v = vec![];
    l2[0].edges_in(0, n2, usize::MAX, &mut v);
    let ve = VecEdges::new(0, n2, v);
    let t = Instant::now();
    let lod2 = build_lod(&ve, shift_for(n2, ve.approx_edges(), per_bucket));
    println!(
        "in memory: {} changes ({:.0} MB), summary built in {:.0} ms ({:.1} MB)",
        ve.edges.len(),
        ve.mem_bytes() as f64 / 1e6,
        t.elapsed().as_secs_f64() * 1e3,
        lod2.mem_bytes() as f64 / 1e6
    );
    let vp = Viewport::fit(n2, width);
    let t = Instant::now();
    for _ in 0..100 {
        read_view(&ve, &lod2, &vp, width as usize, &mut lv);
    }
    let a = t.elapsed().as_secs_f64() / 100.0 * 1e6;
    let t = Instant::now();
    let mut cnt = 0usize;
    for _ in 0..100 {
        for x in 0..width as usize {
            let (ta, tb) = (vp.t_of(x as f32) as i64, vp.t_of(x as f32 + 1.0) as i64);
            cnt += ve.rank(tb) - ve.rank(ta);
        }
    }
    let b = t.elapsed().as_secs_f64() / 100.0 * 1e6;
    println!("  whole view, one line: summary {a:.0} µs, a binary search a pixel {b:.0} µs ({cnt})");

    // annotations
    let t = Instant::now();
    let track = fake_track("i2c", n_items, n);
    println!(
        "annotations: {n_items} items indexed in {:.0} ms, {:.0} MB (texts included)",
        t.elapsed().as_secs_f64() * 1e3,
        track.mem_bytes() as f64 / 1e6
    );
    let mut shapes = vec![];
    for zoom in [1.0, 1e2, 1e4, 1e6] {
        let vp = full.zoomed(zoom, width * 0.37);
        let t = Instant::now();
        for _ in 0..50 {
            track.layout(&vp, 7.0, &mut shapes);
        }
        println!(
            "  layout at {:.0} ticks/px: {:.0} µs, {} shapes",
            vp.tpp,
            t.elapsed().as_secs_f64() / 50.0 * 1e6,
            shapes.len()
        );
    }

    // analog
    let n_s = 20_000_000;
    let vals = synth_analog(n_s, 50.0);
    let t = Instant::now();
    let al = AnalogLine::new("A0", "V", 0.0, 50.0, vals);
    println!(
        "analog: {n_s} samples, bands built in {:.0} ms, {:.0} MB",
        t.elapsed().as_secs_f64() * 1e3,
        al.mem_bytes() as f64 / 1e6
    );
    for zoom in [1.0, 1e3, 1e5] {
        let vp = Viewport::fit(al.end_tick(), width).zoomed(zoom, 100.0);
        let t = Instant::now();
        for _ in 0..50 {
            let _ = al.read_view(&vp);
        }
        println!("  read at {:.1} ticks/px: {:.0} µs", vp.tpp, t.elapsed().as_secs_f64() / 50.0 * 1e6);
    }
    println!("rss at end: {:.0} MB", rss_mb());
}
