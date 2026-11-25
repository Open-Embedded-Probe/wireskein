//! Prototype 1 of WireSkein's screens: egui (eframe, wgpu). A throwaway (bench/notes/2026-11-25-gui-study.ja.md §9).
//!
//! wsk-egui [FILE] [--part K] [--synth N [--items M]] [--light] [--layout NAME] [--zoom Z --at F] [--search Q]
//!          [--select ROW] [--no-analysis] [--detach-list] [--bench OUT.txt] [--shot OUT.png] [--size WxH]
//!          [--present auto|novsync|fifo|immediate|mailbox] [--latency N] [--old-draw] [--backend dx12|vulkan|gl]

mod app;
mod list;
mod meter;
mod model;
mod panels;
mod png;
#[cfg(not(target_arch = "wasm32"))]
mod prof;
mod theme;
mod wave;

use std::path::PathBuf;

fn parse() -> app::Args {
    let mut a = app::Args {
        file: None,
        synth: None,
        synth_items: 500_000,
        part: 0,
        light: false,
        layout: None,
        bench: None,
        shot: None,
        zoom: None,
        search: None,
        select: None,
        no_analysis: false,
        detach_list: false,
        size: (1600.0, 960.0),
        present: None,
        latency: None,
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
            "--shot" => a.shot = Some(PathBuf::from(next(&mut i))),
            "--zoom" => zoom = next(&mut i).parse::<f64>().ok(),
            "--at" => at = next(&mut i).parse().unwrap_or(0.5),
            "--search" => a.search = Some(next(&mut i)),
            "--select" => a.select = next(&mut i).parse().ok(),
            "--no-analysis" => a.no_analysis = true,
            "--detach-list" => a.detach_list = true,
            "--present" => a.present = Some(next(&mut i)),
            "--latency" => a.latency = next(&mut i).parse().ok(),
            // the same as the environment's WGPU_BACKEND (dx12 / vulkan / gl / metal), for a command line that sets no variable
            // SAFETY: at the start, before any other thread runs
            "--backend" => unsafe { std::env::set_var("WGPU_BACKEND", next(&mut i)) },
            "--old-draw" => wave::OLD_DRAW.store(true, std::sync::atomic::Ordering::Relaxed),
            "--size" => {
                let s = next(&mut i);
                if let Some((w, h)) = s.split_once('x') {
                    a.size = (w.parse().unwrap_or(1600.0), h.parse().unwrap_or(960.0));
                }
            }
            "-h" | "--help" => {
                println!(
                    "wsk-egui [FILE] [--part K] [--synth N [--items M]] [--light] [--layout 標準|一覧を大きく|波形 2 つ]\n\
                     [--zoom Z --at F] [--search Q] [--select ROW] [--no-analysis] [--detach-list]\n\
                     [--bench OUT.txt] [--shot OUT.png] [--size WxH] [--present auto|novsync|fifo|immediate|mailbox] [--latency N] [--old-draw] [--backend dx12|vulkan|gl]\n\
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
    a
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn present_mode(name: Option<&str>, unthrottled: bool) -> eframe::wgpu::PresentMode {
    use eframe::wgpu::PresentMode as P;
    match name {
        Some("fifo") => P::Fifo,
        Some("immediate") => P::Immediate,
        Some("mailbox") => P::Mailbox,
        Some("auto") => P::AutoVsync,
        Some("novsync") => P::AutoNoVsync,
        _ if unthrottled => P::AutoNoVsync,
        _ => P::AutoVsync,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    let args = parse();
    // what has to be measured before the frames (meter.rs: Windows' cycle rate)
    meter::init();
    let unthrottled = args.bench.is_some();
    if unthrottled {
        prof::install();
    }
    let opts = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([args.size.0, args.size.1]).with_title("WireSkein 試作 1（egui）"),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            surface: eframe::egui_wgpu::SurfaceConfig {
                present_mode: present_mode(args.present.as_deref(), unthrottled),
                // 1 (eframe's low latency) by default: the CPU waits for the frame before; 2 lets the next frame be built
                // while the GPU draws
                desired_maximum_frame_latency: Some(args.latency.unwrap_or(1)),
            },
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native("wsk-egui", opts, Box::new(move |cc| Ok(Box::new(app::App::new(cc, args)))))
}

/// The web: the canvas `wsk` of index.html; `?synth=1e8` for a made-up capture, else the example carried inside.
#[cfg(target_arch = "wasm32")]
fn main() {
    use eframe::wasm_bindgen::JsCast;
    let mut args = parse();
    let search = web_sys::window().and_then(|w| w.location().search().ok()).unwrap_or_default();
    for kv in search.trim_start_matches('?').split('&') {
        if let Some((k, v)) = kv.split_once('=') {
            match k {
                "synth" => args.synth = v.parse::<f64>().ok().map(|x| x as u64),
                "items" => args.synth_items = v.parse::<f64>().map(|x| x as usize).unwrap_or(args.synth_items),
                "light" => args.light = v == "1",
                _ => {}
            }
        }
    }
    wasm_bindgen_futures::spawn_local(async move {
        let doc = web_sys::window().unwrap().document().unwrap();
        let canvas = doc.get_element_by_id("wsk").unwrap().dyn_into::<web_sys::HtmlCanvasElement>().unwrap();
        let r = eframe::WebRunner::new().start(canvas, eframe::WebOptions::default(), Box::new(move |cc| Ok(Box::new(app::App::new(cc, args))))).await;
        if let Err(e) = r {
            web_sys::console::error_1(&format!("{e:?}").into());
        }
    });
}
