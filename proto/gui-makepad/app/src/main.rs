//! Prototype 4 of WireSkein's screens: Makepad 1.0 (bench/notes/2026-11-25-gui-study.ja.md §13). A throwaway.
//!
//! wsk-makepad [FILE] [--part K] [--synth N [--items M]] [--light] [--layout NAME] [--zoom Z --at F] [--search Q]
//!             [--select ROW] [--no-analysis] [--detach-list] [--bench OUT.txt] [--size WxH] [--exit-after S]

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

mod app;
mod bench;
mod clock;
mod list;
#[cfg(not(target_arch = "wasm32"))]
#[path = "../../../gui/egui/src/meter.rs"]
mod meter;
#[cfg(target_arch = "wasm32")]
#[path = "web_meter.rs"]
mod meter;
mod model;
mod panels;
mod scene;
mod state;
mod theme;
mod theme_light;
mod wave;
#[cfg(target_arch = "wasm32")]
mod web;

use std::path::PathBuf;

fn parse() -> state::Args {
    let mut a = state::Args {
        synth_items: 500_000,
        size: (1600.0, 960.0),
        ..Default::default()
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    let mut at = 0.5;
    let mut zoom = None;
    while i < args.len() {
        let next = |i: &mut usize| -> String {
            *i += 1;
            args.get(*i).cloned().unwrap_or_default()
        };
        match args[i].as_str() {
            "--part" => a.part = next(&mut i).parse().unwrap_or(0),
            "--synth" => a.synth = Some(next(&mut i).parse::<f64>().unwrap_or(1e8) as u64),
            "--items" => a.synth_items = next(&mut i).parse::<f64>().unwrap_or(5e5) as usize,
            "--light" => a.light = true,
            "--layout" => a.layout = Some(next(&mut i)),
            "--bench" => a.bench = Some(PathBuf::from(next(&mut i))),
            "--zoom" => zoom = next(&mut i).parse::<f64>().ok(),
            "--at" => at = next(&mut i).parse().unwrap_or(0.5),
            "--search" => a.search = Some(next(&mut i)),
            "--select" => a.select = next(&mut i).parse().ok(),
            "--no-analysis" => a.no_analysis = true,
            "--detach-list" => a.detach_list = true,
            "--exit-after" => a.exit_after = next(&mut i).parse().ok(),
            "--size" => {
                let s = next(&mut i);
                if let Some((w, h)) = s.split_once('x') {
                    a.size = (w.parse().unwrap_or(1600.0), h.parse().unwrap_or(960.0));
                }
            }
            "-h" | "--help" => {
                println!(
                    "wsk-makepad [FILE] [--part K] [--synth N [--items M]] [--light] [--layout 標準|一覧を大きく|波形 2 つ]\n\
                     [--zoom Z --at F] [--search Q] [--select ROW] [--no-analysis] [--detach-list]\n\
                     [--bench OUT.txt] [--size WxH] [--exit-after SECONDS]"
                );
                std::process::exit(0);
            }
            f if f.starts_with("--") => {} // (Makepad's own, e.g. --stdin-loop)
            f => a.file = Some(PathBuf::from(f)),
        }
        i += 1;
    }
    if let Some(z) = zoom {
        a.zoom = Some((z, at));
    }
    a
}

/// The web (web/index.html; notes §16): the URL's arguments in place of the command line (`?synth=1e8`, `?file=URL`
/// fetched by the page, else the example carried inside). Makepad's web build does not run main: the app reads these
/// when it registers (app.rs).
#[cfg(target_arch = "wasm32")]
pub fn web_args() -> state::Args {
    let mut a = parse();
    for (k, v) in web::query() {
        let num = |d: f64| v.parse::<f64>().unwrap_or(d);
        match k.as_str() {
            "synth" => a.synth = Some(num(1e8) as u64),
            "items" => a.synth_items = num(5e5) as usize,
            "part" => a.part = num(0.0) as usize,
            "light" => a.light = v != "0",
            "layout" => a.layout = Some(v),
            "zoom" => a.zoom = Some((num(1.0), a.zoom.map(|z| z.1).unwrap_or(0.5))),
            "at" => a.zoom = Some((a.zoom.map(|z| z.0).unwrap_or(1.0), num(0.5))),
            "search" => a.search = Some(v),
            "select" => a.select = v.parse().ok(),
            "noanalysis" => a.no_analysis = v != "0",
            "bench" if v != "0" => a.bench = Some(PathBuf::from("bench")),
            _ => {}
        }
    }
    a
}

fn main() {
    let _ = app::ARGS.set(parse());
    // what has to be measured before the frames (meter.rs: Windows' cycle rate)
    meter::init();
    app::app_main();
}
