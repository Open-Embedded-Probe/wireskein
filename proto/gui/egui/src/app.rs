//! The app: what every panel shares, the dock of panels (egui_dock), layouts saved and switched, a list in an OS
//! window of its own, the bar above and the status line, and the scripted bench / screenshot runs.

use crate::list::{self, ListTab};
use crate::model::{self, Analysis, Capture};
use crate::panels::{self, Search};
use crate::theme::{self, Pal};
use crate::wave::{self, WaveTab, fmt_freq, fmt_time};
use eframe::egui::{self, RichText};
use egui_dock::{DockArea, DockState, NodeIndex, NodePath, Style, TabViewer};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use web_time::Instant;
use view_core::Viewport;
use view_core::digital::{LineView, read_view};
use view_core::shapes::Seg;
use view_core::tiles::TileCache;

pub type TileCacheT = TileCache<(u64, usize), Vec<Seg>>;

pub fn rss_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/statm")
            .ok()
            .and_then(|s| s.split_whitespace().nth(1).and_then(|x| x.parse::<f64>().ok()))
            .map(|p| p * 4096.0 / 1e6)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sel {
    pub claim: usize,
    pub layer: usize,
    pub row: usize,
}

pub struct Measure {
    pub line: usize,
    pub t: f64,
    pub level: u8,
    pub prev: Option<i64>,
    pub next: Option<i64>,
    pub next2: Option<i64>,
}

impl Measure {
    pub fn text(&self, hz: f64) -> String {
        let (Some(p), Some(q)) = (self.prev, self.next) else {
            return format!("水準 {}", if self.level == 1 { "H" } else { "L" });
        };
        let w = (q - p) as f64 / hz;
        let mut s = format!("{} 幅 {}", if self.level == 1 { "H" } else { "L" }, fmt_time(w));
        if let Some(r) = self.next2 {
            let per = (r - p) as f64 / hz;
            let duty = if self.level == 1 { w / per } else { 1.0 - w / per };
            s += &format!("\n周期 {} · {} · デューティ {:.1}%", fmt_time(per), fmt_freq(1.0 / per), duty * 100.0);
        }
        s
    }
}

#[derive(Default)]
pub struct Perf {
    frames: VecDeque<(f64, f32)>,
    wave_ms: VecDeque<f32>,
    pub shapes: usize,
    pub first_paint: Option<f64>,
    pub tiles: (usize, u64, u64),
}

impl Perf {
    pub fn frame(&mut self, now: f64, dt_ms: f32) {
        self.frames.push_back((now, dt_ms));
        while self.frames.front().is_some_and(|f| now - f.0 > 1.0) {
            self.frames.pop_front();
        }
    }
    pub fn wave(&mut self, ms: f32, shapes: usize) {
        self.wave_ms.push_back(ms);
        if self.wave_ms.len() > 120 {
            self.wave_ms.pop_front();
        }
        self.shapes = shapes;
    }
    pub fn fps(&self) -> (f32, f32, f32) {
        let n = self.frames.len() as f32;
        let avg = self.frames.iter().map(|f| f.1).sum::<f32>() / n.max(1.0);
        let worst = self.frames.iter().map(|f| f.1).fold(0.0, f32::max);
        (n, avg, worst)
    }
    pub fn wave_avg(&self) -> f32 {
        self.wave_ms.iter().sum::<f32>() / (self.wave_ms.len().max(1) as f32)
    }
}

pub struct Shared {
    pub cap: Option<Capture>,
    pub cgen: u64,
    pub analysis: Option<Analysis>,
    pub analysis_slot: Option<Arc<Mutex<Option<Analysis>>>>,
    pub sel: Option<Sel>,
    pub sel_gen: u64,
    /// the tab the choice came from (it does not follow itself)
    pub sel_src: u32,
    pub measure: Option<Measure>,
    pub markers: [Option<f64>; 2],
    pub hover_t: Option<f64>,
    pub search: Search,
    pub dark: bool,
    pub pal: Pal,
    pub tiles_on: bool,
    pub continuous: bool,
    pub perf: Perf,
    /// a view the bench sets on the first waveform
    pub force_vp: Option<Viewport>,
    pub minimap_key: (u64, usize, i64),
    pub minimap: Vec<u64>,
}

/// The changes a pixel over the whole capture, every line together (for the minimap; kept while nothing changes).
pub fn minimap(cap: &Capture, cgen: u64, key_slot: &mut (u64, usize, i64), out: &mut Vec<u64>, w: usize) {
    let built: i64 = cap.lines.iter().map(|l| l.lod.read().unwrap().built_until).sum();
    let key = (cgen, w, built);
    if key != *key_slot {
        let fit = Viewport::fit(cap.n_ticks, w as f32);
        let mut acc = vec![0u64; w];
        let mut lv = LineView::default();
        for l in &cap.lines {
            let lod = l.lod.read().unwrap();
            read_view(l.src.as_ref(), &lod, &fit, 0, &mut lv);
            for (x, c) in lv.cols.iter().enumerate().take(w) {
                acc[x] += c.count as u64;
            }
        }
        *out = acc;
        *key_slot = key;
    }
}

impl Shared {
    pub fn select(&mut self, s: Sel, src: u32) {
        self.sel = Some(s);
        self.sel_gen += 1;
        self.sel_src = src;
    }
    /// The chosen item's start and end.
    pub fn sel_span(&self) -> Option<(i64, i64)> {
        let s = self.sel?;
        let it = self.analysis.as_ref()?.claims.get(s.claim)?.layers.get(s.layer)?.track.item(s.row);
        Some((it.s, it.e))
    }
}

#[derive(Serialize, Deserialize)]
pub enum Tab {
    Wave(WaveTab),
    List(ListTab),
    Measure,
    Search,
    Info,
    Perf,
}

const SEARCH_ID: u32 = 1_000_000;

struct Viewer<'a> {
    sh: &'a mut Shared,
    cache: &'a mut TileCacheT,
    add_at: Option<NodePath>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;
    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        match tab {
            Tab::Wave(w) => egui::Id::new(("wave", w.id)),
            Tab::List(l) => egui::Id::new(("list", l.id)),
            Tab::Measure => egui::Id::new("measure"),
            Tab::Search => egui::Id::new("search"),
            Tab::Info => egui::Id::new("info"),
            Tab::Perf => egui::Id::new("perf"),
        }
    }
    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Wave(w) => format!("波形 {}", w.id).into(),
            Tab::List(l) => {
                let name = self
                    .sh
                    .analysis
                    .as_ref()
                    .and_then(|a| a.claims.get(l.claim))
                    .map(|c| format!("{} {}", c.protocol, c.layers.get(l.layer).map(|x| x.name.as_str()).unwrap_or("")))
                    .unwrap_or_default();
                format!("一覧 {} {name}", l.id).into()
            }
            Tab::Measure => "測定".into(),
            Tab::Search => "検索".into(),
            Tab::Info => "主張".into(),
            Tab::Perf => "速さ".into(),
        }
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::Wave(w) => {
                wave::sync(w, self.sh);
                if let Some(vp) = self.sh.force_vp.take() {
                    w.st.vp = Some(vp);
                }
                wave::ui(ui, w, self.sh, self.cache)
            }
            Tab::List(l) => list::ui(ui, l, self.sh),
            Tab::Measure => panels::measure_ui(ui, self.sh),
            Tab::Search => panels::search_ui(ui, self.sh, SEARCH_ID),
            Tab::Info => panels::info_ui(ui, self.sh),
            Tab::Perf => panels::perf_ui(ui, self.sh),
        }
    }
    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::Wave(_) | Tab::List(_) | Tab::Search => [false, false],
            _ => [false, true],
        }
    }
    fn add_popup(&mut self, ui: &mut egui::Ui, path: NodePath) {
        ui.set_min_width(120.0);
        if ui.button("波形").clicked() || ui.button("一覧").clicked() || ui.button("測定").clicked() {
            self.add_at = Some(path);
        }
        // (which one: told by the button pressed; egui gives no value back from here, so the app adds a menu below)
    }
}

pub struct Args {
    pub file: Option<PathBuf>,
    pub synth: Option<u64>,
    pub synth_items: usize,
    pub part: usize,
    pub light: bool,
    pub layout: Option<String>,
    pub bench: Option<PathBuf>,
    pub shot: Option<PathBuf>,
    pub zoom: Option<(f64, f64)>,
    pub search: Option<String>,
    pub select: Option<usize>,
    pub no_analysis: bool,
    pub detach_list: bool,
    pub size: (f32, f32),
}

enum Phase {
    Wait,
    Zoom(usize, u32),
    Sweep(u32),
    Done,
}

struct Bench {
    out: PathBuf,
    phase: Phase,
    zooms: Vec<f64>,
    samples: Vec<f32>,
    wave: Vec<f32>,
    lines: Vec<String>,
    t0: Instant,
}

pub struct App {
    sh: Shared,
    dock: DockState<Tab>,
    cache: TileCacheT,
    next_id: u32,
    layouts: BTreeMap<String, String>,
    layout_name: String,
    os_lists: Vec<ListTab>,
    started: Instant,
    last_frame: Option<Instant>,
    args: Args,
    bench: Option<Bench>,
    shot_asked: Option<Instant>,
    shot_done: Arc<Mutex<bool>>,
    font: Option<String>,
    add_kind: u8,
    zoom_done: bool,
    frames: u64,
}

fn default_layout(kind: &str, next: &mut u32) -> DockState<Tab> {
    let mut id = || {
        *next += 1;
        *next
    };
    match kind {
        "一覧を大きく" => {
            let mut d = DockState::new(vec![Tab::Wave(WaveTab { id: id(), follow: true, ..Default::default() })]);
            let t = d.main_surface_mut();
            let [_, b] = t.split_below(NodeIndex::root(), 0.45, vec![Tab::List(ListTab { id: id(), follow: true, ..Default::default() })]);
            t.split_right(b, 0.62, vec![Tab::Search, Tab::Measure, Tab::Info, Tab::Perf]);
            d
        }
        "波形 2 つ" => {
            let mut d = DockState::new(vec![Tab::Wave(WaveTab { id: id(), follow: false, ..Default::default() })]);
            let t = d.main_surface_mut();
            let [a, _] = t.split_below(NodeIndex::root(), 0.42, vec![Tab::Wave(WaveTab { id: id(), follow: true, ..Default::default() })]);
            t.split_right(a, 0.7, vec![Tab::List(ListTab { id: id(), follow: true, ..Default::default() }), Tab::Search]);
            d
        }
        _ => {
            let mut d = DockState::new(vec![Tab::Wave(WaveTab { id: id(), follow: true, ..Default::default() })]);
            let t = d.main_surface_mut();
            let [_, r] = t.split_right(NodeIndex::root(), 0.68, vec![Tab::List(ListTab { id: id(), follow: true, ..Default::default() }), Tab::Search]);
            t.split_below(r, 0.62, vec![Tab::Measure, Tab::Info, Tab::Perf]);
            d
        }
    }
}

const PRESETS: [&str; 3] = ["標準", "一覧を大きく", "波形 2 つ"];

impl App {
    pub fn new(cc: &eframe::CreationContext, args: Args) -> Self {
        let ctx = &cc.egui_ctx;
        let font = theme::install_fonts(ctx);
        let dark = !args.light;
        ctx.set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
        let mut layouts = BTreeMap::new();
        if let Some(st) = cc.storage
            && let Some(s) = st.get_string("layouts")
            && let Ok(m) = serde_json::from_str::<BTreeMap<String, String>>(&s)
        {
            layouts = m;
        }
        let mut next_id = 0;
        let layout_name = args.layout.clone().unwrap_or("標準".into());
        let dock = match layouts.get(&layout_name).and_then(|j| serde_json::from_str::<DockState<Tab>>(j).ok()) {
            Some(d) => {
                next_id = 100;
                d
            }
            None => default_layout(&layout_name, &mut next_id),
        };
        let sh = Shared {
            cap: None,
            cgen: 0,
            analysis: None,
            analysis_slot: None,
            sel: None,
            sel_gen: 0,
            sel_src: 0,
            measure: None,
            markers: [None, None],
            hover_t: None,
            search: Search { query: args.search.clone().unwrap_or_default(), ..Default::default() },
            dark,
            pal: theme::pal(dark),
            tiles_on: true,
            continuous: args.bench.is_some(),
            perf: Perf::default(),
            force_vp: None,
            minimap_key: (u64::MAX, 0, 0),
            minimap: vec![],
        };
        let bench = args.bench.clone().map(|out| Bench {
            out,
            phase: Phase::Wait,
            zooms: vec![1.0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7],
            samples: vec![],
            wave: vec![],
            lines: vec![],
            t0: Instant::now(),
        });
        let mut app = App {
            sh,
            dock,
            cache: TileCache::default(),
            next_id,
            layouts,
            layout_name,
            os_lists: vec![],
            started: Instant::now(),
            last_frame: None,
            args,
            bench,
            shot_asked: None,
            shot_done: Arc::new(Mutex::new(false)),
            font,
            add_kind: 0,
            zoom_done: false,
            frames: 0,
        };
        app.open(ctx);
        if app.args.detach_list {
            app.os_lists.push(ListTab { id: 900, follow: true, ..Default::default() });
        }
        app
    }

    fn open(&mut self, ctx: &egui::Context) {
        if let Some(c) = &self.sh.cap {
            c.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.sh.cgen += 1;
        self.cache.clear();
        self.sh.analysis = None;
        self.sh.sel = None;
        self.sh.perf.first_paint = None;
        self.started = Instant::now();
        if let Some(total) = self.args.synth {
            let c = model::open_synth(ctx, total);
            self.sh.analysis_slot = Some(model::start_fake_analysis(ctx, self.args.synth_items, c.n_ticks));
            self.sh.cap = Some(c);
        } else if cfg!(target_arch = "wasm32") {
            // the web: an example carried in the program, analyzed by the core at once (no threads, no files)
            const EXAMPLE: &[u8] = include_bytes!("../../../../tests/data/examples/mixed-1.wireskein");
            match model::open_bytes(ctx, "mixed-1.wireskein", EXAMPLE, 0) {
                Ok(c) => {
                    let names: Vec<String> = c.lines.iter().map(|l| l.name.clone()).collect();
                    let a = model::analysis_of_bytes(EXAMPLE, 0, &names);
                    self.sh.analysis_slot = Some(Arc::new(Mutex::new(Some(a))));
                    self.sh.cap = Some(c);
                }
                Err(e) => eprintln!("example: {e}"),
            }
        } else if let Some(p) = self.args.file.clone() {
            match model::open_file(ctx, &p, self.args.part) {
                Ok(c) => {
                    let names = c.lines.iter().map(|l| l.name.clone()).collect();
                    if !self.args.no_analysis {
                        self.sh.analysis_slot = Some(model::start_analysis(ctx, p, c.part, c.n_parts, names, c.total_edges));
                    }
                    self.sh.cap = Some(c);
                }
                Err(e) => {
                    eprintln!("{}: {e}", p.display());
                    self.sh.cap = None;
                }
            }
        }
    }

    fn poll(&mut self) {
        if let Some(slot) = &self.sh.analysis_slot {
            let got = slot.lock().unwrap().take();
            if let Some(mut a) = got {
                if let Some(cap) = &self.sh.cap {
                    let t = Instant::now();
                    for c in &mut a.claims {
                        model::derive_tiers(c, cap);
                    }
                    eprintln!("tiers read from the lines: {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
                }
                if let Some(e) = &a.error {
                    eprintln!("analysis: {e}");
                }
                eprintln!(
                    "analysis: {:.2} s, {} claims, {} rows",
                    a.seconds,
                    a.claims.len(),
                    a.claims.iter().flat_map(|c| c.layers.iter()).map(|l| l.len()).sum::<usize>()
                );
                self.sh.analysis = Some(a);
                self.sh.analysis_slot = None;
                self.sh.search.last = String::from("\u{0}"); // search again over the new results
                if let Some(r) = self.args.select.take() {
                    self.sh.select(Sel { claim: 0, layer: 0, row: r }, 999_999);
                }
            }
        }
    }

    fn bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.strong("WireSkein 試作 1（egui）");
            ui.separator();
            #[cfg(not(target_arch = "wasm32"))]
            {
                let mut path = self.args.file.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                let r = ui.add(egui::TextEdit::singleline(&mut path).hint_text("ファイルのパス").desired_width(260.0));
                if r.changed() {
                    self.args.file = Some(PathBuf::from(&path));
                }
                if ui.button("開く").clicked() || r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    self.args.synth = None;
                    let ctx = ui.ctx().clone();
                    self.open(&ctx);
                }
                ui.menu_button("合成", |ui| {
                    for (label, n) in [("1e7 の変化", 1e7), ("1e8 の変化", 1e8), ("1e9 の変化", 1e9)] {
                        if ui.button(label).clicked() {
                            self.args.synth = Some(n as u64);
                            let ctx = ui.ctx().clone();
                            self.open(&ctx);
                            ui.close();
                        }
                    }
                });
            }
            if let Some(cap) = &self.sh.cap
                && cap.n_parts > 1
            {
                let mut part = self.args.part;
                egui::ComboBox::from_id_salt("part").selected_text(format!("区画 {part}")).show_ui(ui, |ui| {
                    for k in 0..cap.n_parts {
                        ui.selectable_value(&mut part, k, format!("区画 {k}"));
                    }
                });
                if part != self.args.part {
                    self.args.part = part;
                    let ctx = ui.ctx().clone();
                    self.open(&ctx);
                }
            }
            ui.separator();
            ui.menu_button("パネル", |ui| {
                for (k, label) in ["波形を足す", "一覧を足す", "測定", "検索", "主張", "速さ"].iter().enumerate() {
                    if ui.button(*label).clicked() {
                        self.next_id += 1;
                        let id = self.next_id;
                        let t = match k {
                            0 => Tab::Wave(WaveTab { id, follow: true, ..Default::default() }),
                            1 => Tab::List(ListTab { id, follow: true, ..Default::default() }),
                            2 => Tab::Measure,
                            3 => Tab::Search,
                            4 => Tab::Info,
                            _ => Tab::Perf,
                        };
                        self.dock.push_to_focused_leaf(t);
                        ui.close();
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                if ui.button("一覧を別の窓に").clicked() {
                    self.next_id += 1;
                    self.os_lists.push(ListTab { id: self.next_id, follow: true, ..Default::default() });
                    ui.close();
                }
            });
            ui.menu_button(format!("レイアウト: {}", self.layout_name), |ui| {
                ui.label(RichText::new("既定").small());
                for p in PRESETS {
                    if ui.button(p).clicked() {
                        self.dock = default_layout(p, &mut self.next_id);
                        self.layout_name = p.into();
                        ui.close();
                    }
                }
                if !self.layouts.is_empty() {
                    ui.separator();
                    ui.label(RichText::new("保存したもの").small());
                    let names: Vec<String> = self.layouts.keys().cloned().collect();
                    for n in names {
                        ui.horizontal(|ui| {
                            if ui.button(&n).clicked()
                                && let Some(d) = self.layouts.get(&n).and_then(|j| serde_json::from_str::<DockState<Tab>>(j).ok())
                            {
                                self.dock = d;
                                self.next_id = self.next_id.max(1000);
                                self.layout_name = n.clone();
                                ui.close();
                            }
                            if ui.small_button("消す").clicked() {
                                self.layouts.remove(&n);
                            }
                        });
                    }
                }
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.layout_name).desired_width(120.0));
                    if ui.button("今の形を保存").clicked()
                        && let Ok(j) = serde_json::to_string(&self.dock)
                    {
                        self.layouts.insert(self.layout_name.clone(), j);
                    }
                });
            });
            ui.separator();
            let mut dark = self.sh.dark;
            ui.selectable_value(&mut dark, true, "ダーク");
            ui.selectable_value(&mut dark, false, "ライト");
            if dark != self.sh.dark {
                self.sh.dark = dark;
                self.sh.pal = theme::pal(dark);
                ui.ctx().set_theme(if dark { egui::Theme::Dark } else { egui::Theme::Light });
            }
        });
    }

    fn status(&mut self, ui: &mut egui::Ui) {
        let pal = self.sh.pal;
        ui.horizontal(|ui| {
            let (fps, avg, _) = self.sh.perf.fps();
            ui.label(RichText::new(format!("{fps:.0} fps · {avg:.1} ms · 波形 {:.2} ms", self.sh.perf.wave_avg())).color(pal.dim));
            if let Some(cap) = &self.sh.cap {
                let done = cap.lod_done.load(std::sync::atomic::Ordering::Relaxed);
                if done < cap.lines.len() {
                    ui.spinner();
                    ui.label(format!("要約を作成中 {done}/{}", cap.lines.len()));
                }
                ui.label(RichText::new(format!("{} 本 · {:.1e} の変化", cap.lines.len(), cap.total_edges as f64)).color(pal.dim));
            }
            if self.sh.analysis_slot.is_some() {
                ui.spinner();
                ui.label("解析中…");
            } else if let Some(a) = &self.sh.analysis {
                let rows: usize = a.claims.iter().flat_map(|c| c.layers.first()).map(|l| l.len()).sum();
                ui.label(RichText::new(format!("主張 {} · 項目 {rows} · 解析 {:.2} s", a.claims.len(), a.seconds)).color(pal.dim));
            }
            if let Some(m) = rss_mb() {
                ui.label(RichText::new(format!("RSS {m:.0} MB")).color(pal.dim));
            }
            if let Some(f) = &self.font {
                ui.label(RichText::new(format!("日本語: {}", f.rsplit('/').next().unwrap_or(f))).color(pal.dim).small());
            }
        });
    }

    fn run_bench(&mut self, ui: &egui::Ui, dt_ms: f32) {
        let Some(b) = &mut self.bench else { return };
        let Some(cap) = &self.sh.cap else { return };
        let ready = cap.lod_s.lock().unwrap().is_some() && self.sh.analysis_slot.is_none();
        let n = cap.n_ticks;
        let w = ui.ctx().content_rect().width() * 0.68 - wave::LABEL_W;
        match b.phase {
            Phase::Wait => {
                if ready || b.t0.elapsed().as_secs() > 600 {
                    b.lines.push(format!(
                        "# {} ({} lines, {} changes, {} ticks): first paint {:.0} ms, read {:.0} ms, summaries {:.2} s, analysis {}",
                        cap.title,
                        cap.lines.len(),
                        cap.total_edges,
                        cap.n_ticks,
                        self.sh.perf.first_paint.unwrap_or(f64::NAN) * 1e3,
                        cap.read_s * 1e3,
                        cap.lod_s.lock().unwrap().unwrap_or(f64::NAN),
                        self.sh.analysis.as_ref().map(|a| format!("{:.2} s", a.seconds)).unwrap_or("-".into())
                    ));
                    b.lines.push("| ticks/px | fps (median) | frame ms p50 / p95 | waveform CPU ms |".into());
                    b.lines.push("|---|---|---|---|".into());
                    b.phase = Phase::Zoom(0, 0);
                }
            }
            Phase::Zoom(zi, f) => {
                if f > 5 {
                    b.samples.push(dt_ms);
                    b.wave.push(self.sh.perf.wave_avg());
                }
                let fit = Viewport::fit(n, w.max(100.0));
                let z = b.zooms[zi];
                let mut vp = fit.zoomed(z, w * 0.37);
                vp.t0 += f as f64 * 7.0 * vp.tpp;
                self.sh.force_vp = Some(vp.clamped(n));
                if f >= 95 {
                    let mut s = std::mem::take(&mut b.samples);
                    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let p = |q: f32| s[((s.len() - 1) as f32 * q) as usize];
                    let wv = b.wave.iter().sum::<f32>() / b.wave.len().max(1) as f32;
                    b.wave.clear();
                    b.lines.push(format!("| {:.3} | {:.0} | {:.1} / {:.1} | {:.2} |", vp.tpp, 1000.0 / p(0.5), p(0.5), p(0.95), wv));
                    b.phase = if zi + 1 < b.zooms.len() && b.zooms[zi + 1] <= n as f64 * 20.0 / w as f64 {
                        Phase::Zoom(zi + 1, 0)
                    } else {
                        Phase::Sweep(0)
                    };
                } else {
                    b.phase = Phase::Zoom(zi, f + 1);
                }
            }
            Phase::Sweep(f) => {
                // a zoom from the whole capture in, 6 % a frame (new tiles all the time)
                if f > 3 {
                    b.samples.push(dt_ms);
                }
                let fit = Viewport::fit(n, w.max(100.0));
                let vp = fit.zoomed(1.06f64.powi(f as i32), w * 0.37);
                self.sh.force_vp = Some(vp.clamped(n));
                if f >= 240 || vp.tpp < 0.05 {
                    let mut s = std::mem::take(&mut b.samples);
                    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    let p = |q: f32| s[((s.len() - 1) as f32 * q) as usize];
                    b.lines.push(format!("zoom sweep (6 %/frame): frame ms p50 {:.1}, p95 {:.1}, max {:.1}", p(0.5), p(0.95), s[s.len() - 1]));
                    b.lines.push(format!(
                        "memory: model {:.0} MB, rss {:.0} MB",
                        cap.mem_bytes() as f64 / 1e6,
                        rss_mb().unwrap_or(f64::NAN)
                    ));
                    let text = b.lines.join("\n") + "\n";
                    println!("{text}");
                    let _ = std::fs::write(&b.out, text);
                    b.phase = Phase::Done;
                    if self.args.shot.is_none() {
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                } else {
                    b.phase = Phase::Sweep(f + 1);
                }
            }
            Phase::Done => {}
        }
    }

    fn shot(&mut self, ui: &egui::Ui) {
        let Some(path) = self.args.shot.clone() else { return };
        if *self.shot_done.lock().unwrap() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if self.bench.as_ref().is_some_and(|b| !matches!(b.phase, Phase::Done)) {
            return;
        }
        let ready = self.sh.cap.as_ref().is_some_and(|c| c.lod_s.lock().unwrap().is_some()) && self.sh.analysis_slot.is_none();
        if !ready {
            return;
        }
        match self.shot_asked {
            None => {
                self.shot_asked = Some(Instant::now());
                self.zoom_done = false; // the view asked for, again after the bench
            }
            Some(t) if t.elapsed().as_secs_f32() > 1.5 => {
                let done = self.shot_done.clone();
                let ctx = ui.ctx().clone();
                ui.ctx().request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    match crate::png::write(&path, w as u32, h as u32, &rgba) {
                        Ok(()) => eprintln!("screenshot: {}", path.display()),
                        Err(e) => eprintln!("screenshot: {e}"),
                    }
                    *done.lock().unwrap() = true;
                    ctx.request_repaint();
                });
                self.shot_asked = Some(Instant::now() + std::time::Duration::from_secs(3600));
            }
            _ => {}
        }
        ui.ctx().request_repaint();
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = Instant::now();
        let dt_ms = self.last_frame.map(|t| (now - t).as_secs_f32() * 1e3).unwrap_or(0.0);
        self.last_frame = Some(now);
        self.frames += 1;
        self.sh.perf.frame(ui.input(|i| i.time), dt_ms);
        self.poll();
        // the first frame with the capture drawn
        if self.sh.perf.first_paint.is_none() && self.sh.cap.is_some() && self.frames > 1 {
            self.sh.perf.first_paint = Some(self.started.elapsed().as_secs_f64());
            if let Some(c) = &self.sh.cap {
                eprintln!("first paint: {:.0} ms (read {:.0} ms)", self.sh.perf.first_paint.unwrap() * 1e3, c.read_s * 1e3);
            }
        }
        if let Some((z, at)) = self.args.zoom.filter(|_| !self.zoom_done)
            && let Some(cap) = &self.sh.cap
        {
            self.zoom_done = true;
            let w = ui.ctx().content_rect().width() * 0.68 - wave::LABEL_W;
            let fit = Viewport::fit(cap.n_ticks, w);
            let c = at * cap.n_ticks as f64;
            let span = cap.n_ticks as f64 / z;
            self.sh.force_vp = Some(Viewport { t0: c - span / 2.0, tpp: span / fit.width as f64, width: fit.width });
        }
        self.run_bench(ui, dt_ms);
        self.shot(ui);
        egui::Panel::top("bar").show(ui, |ui| self.bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| self.status(ui));
        let mut add_at = None;
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| {
            let mut v = Viewer { sh: &mut self.sh, cache: &mut self.cache, add_at: None };
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_add_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, &mut v);
            add_at = v.add_at;
        });
        let _ = add_at;
        let _ = self.add_kind;
        self.sh.perf.tiles = (self.cache.len(), self.cache.made, self.cache.reused);
        self.cache.sweep(3);

        // lists asked to go to a window of their own
        let mut out = vec![];
        for (path, tab) in self.dock.iter_all_tabs() {
            if let Tab::List(l) = tab
                && l.detach
            {
                out.push((path, l.id));
            }
        }
        for (path, _) in out.into_iter().rev() {
            if let Some(Tab::List(mut l)) = self.dock.remove_tab(path) {
                l.detach = false;
                self.os_lists.push(l);
            }
        }
        // the lists in OS windows (immediate viewports: they borrow the app's state)
        let ctx = ui.ctx().clone();
        let mut back = vec![];
        for (k, l) in self.os_lists.iter_mut().enumerate() {
            let vid = egui::ViewportId::from_hash_of(("list-window", l.id));
            let sh = &mut self.sh;
            let close = ctx.show_viewport_immediate(
                vid,
                egui::ViewportBuilder::default().with_title(format!("結果の一覧 {}", l.id)).with_inner_size([620.0, 760.0]).with_position([1420.0, 40.0]),
                |ui, _class| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("別の窓（閉じると元のドックに戻る）").small());
                        });
                        list::ui(ui, l, sh);
                    });
                    ui.input(|i| i.viewport().close_requested())
                },
            );
            if close {
                back.push(k);
            }
        }
        for k in back.into_iter().rev() {
            let l = self.os_lists.remove(k);
            self.dock.push_to_focused_leaf(Tab::List(l));
        }
        if self.sh.continuous {
            ctx.request_repaint();
        }
        // keep the progress moving while the summaries and the analysis are made
        if self.sh.analysis_slot.is_some() || self.sh.cap.as_ref().is_some_and(|c| c.lod_s.lock().unwrap().is_none()) {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Ok(s) = serde_json::to_string(&self.layouts) {
            storage.set_string("layouts", s);
        }
    }
}
