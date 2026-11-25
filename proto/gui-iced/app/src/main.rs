//! Prototype 3 of WireSkein's screens: iced 0.14 (wgpu), a declarative Rust GUI (bench/notes/2026-11-25-gui-study.ja.md
//! §12). A throwaway.
//!
//! wsk-iced [FILE] [--part K] [--synth N [--items M]] [--light] [--layout NAME] [--zoom Z --at F] [--search Q]
//!          [--select ROW] [--no-analysis] [--detach-list] [--bench OUT.txt] [--shot OUT.png] [--size WxH]
//!          [--present vsync|no_vsync|immediate|fifo|mailbox] [--vsync] [--backend dx12|vulkan|gl]

mod app;
mod bench;
mod dock;
mod gpu;
mod list;
#[path = "../../../gui/egui/src/meter.rs"]
mod meter;
mod model;
mod panels;
mod png;
mod prof;
mod scene;
mod theme;
mod wave;

use std::path::PathBuf;

fn parse() -> (app::Args, Option<String>, bool) {
    let mut a = app::Args {
        synth_items: 500_000,
        size: (1600.0, 960.0),
        ..Default::default()
    };
    let mut present = None;
    let mut vsync = false;
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
            "--shot" => a.shot = Some(PathBuf::from(next(&mut i))),
            "--zoom" => zoom = next(&mut i).parse::<f64>().ok(),
            "--at" => at = next(&mut i).parse().unwrap_or(0.5),
            "--search" => a.search = Some(next(&mut i)),
            "--select" => a.select = next(&mut i).parse().ok(),
            "--no-analysis" => a.no_analysis = true,
            "--detach-list" => a.detach_list = true,
            "--present" => present = Some(next(&mut i)),
            "--vsync" => vsync = true,
            // the same as the environment's WGPU_BACKEND (dx12 / vulkan / gl), for a command line that sets no variable
            // SAFETY: at the start, before any other thread runs
            "--backend" => unsafe { std::env::set_var("WGPU_BACKEND", next(&mut i)) },
            "--size" => {
                let s = next(&mut i);
                if let Some((w, h)) = s.split_once('x') {
                    a.size = (w.parse().unwrap_or(1600.0), h.parse().unwrap_or(960.0));
                }
            }
            "-h" | "--help" => {
                println!(
                    "wsk-iced [FILE] [--part K] [--synth N [--items M]] [--light] [--layout 標準|一覧を大きく|波形 2 つ]\n\
                     [--zoom Z --at F] [--search Q] [--select ROW] [--no-analysis] [--detach-list]\n\
                     [--bench OUT.txt] [--shot OUT.png] [--size WxH] [--present vsync|no_vsync|immediate|fifo|mailbox] [--vsync] [--backend dx12|vulkan|gl]\n\
                     (the backend: the environment's WGPU_BACKEND=dx12|vulkan|gl|metal)"
                );
                std::process::exit(0);
            }
            f => a.file = Some(PathBuf::from(f)),
        }
        i += 1;
    }
    if let Some(z) = zoom {
        a.zoom = Some((z, at));
    }
    (a, present, vsync)
}

fn main() -> iced::Result {
    let (mut args, present, vsync) = parse();
    // what has to be measured before the frames (meter.rs: Windows' cycle rate)
    meter::init();
    let bench = args.bench.is_some();
    if bench {
        // iced's stages (its debug spans) to our own beacon server, wgpu's scopes to prof.rs
        bench::start_server();
        prof::install();
    } else {
        iced_debug::disable();
    }
    if let Some(p) = &present {
        // SAFETY: before any other thread runs
        unsafe { std::env::set_var("ICED_PRESENT_MODE", p) };
    }
    let settings = iced::Settings {
        // the bench: frames as fast as they come (prototype 1's AutoNoVsync), unless asked
        vsync: !bench || vsync,
        antialiasing: false,
        ..Default::default()
    };
    // what iced_wgpu asks of the surface: ICED_PRESENT_MODE (ours from --present, or the environment's) over the settings
    args.present_asked = std::env::var("ICED_PRESENT_MODE")
        .ok()
        .unwrap_or(if settings.vsync { "vsync" } else { "no_vsync" }.into());
    iced::daemon(move || app::App::boot(args.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .settings(settings)
        .run()
}
