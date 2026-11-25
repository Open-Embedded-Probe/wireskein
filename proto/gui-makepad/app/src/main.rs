//! Prototype 4 of WireSkein's screens: Makepad 1.0 (bench/notes/2026-11-25-gui-study.ja.md §13). A throwaway.
//!
//! wsk-makepad [FILE] [--part K] [--synth N [--items M]] [--light] [--layout NAME] [--zoom Z --at F] [--search Q]
//!             [--select ROW] [--no-analysis] [--detach-list] [--bench OUT.txt] [--size WxH] [--exit-after S]

mod app;
mod bench;
mod list;
mod model;
mod panels;
mod scene;
mod state;
mod theme;
mod theme_light;
mod wave;

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

fn main() {
    let _ = app::ARGS.set(parse());
    app::app_main();
}
