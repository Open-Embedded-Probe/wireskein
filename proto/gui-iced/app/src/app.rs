//! The application: its state (the capture, the analysis, the choice, the markers, the search, the panels and their
//! layout, the windows), its messages and how they change the state, the views of the main window and of the panels
//! detached into windows of their own, the bar above and the status line, the bench and the screenshot.

use crate::bench::{self, Bench};
use crate::dock::{self, Conf, Kind, Pane};
use crate::model::{self, Analysis, Capture};
use crate::scene::{Lane, SceneCache};
use crate::theme::{self, Pal};
use crate::wave::{self, fmt_freq, fmt_time};
use iced::widget::pane_grid;
use iced::widget::{button, checkbox, column, container, pick_list, row, text, text_input};
use iced::{Background, Border, Color, Element, Length, Point, Size, Subscription, Task, keyboard, window};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use view_core::Viewport;
use web_time::Instant;

pub fn rss_mb() -> Option<f64> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().nth(1).and_then(|x| x.parse::<f64>().ok())).map(|p| p * 4096.0 / 1e6)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The CPU time of this thread, ms (Linux; elsewhere NaN).
pub fn thread_cpu_ms() -> f64 {
    #[cfg(target_os = "linux")]
    {
        let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        // SAFETY: a valid timespec to fill
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
        t.tv_sec as f64 * 1e3 + t.tv_nsec as f64 / 1e6
    }
    #[cfg(not(target_os = "linux"))]
    {
        f64::NAN
    }
}

#[derive(Clone, Debug, Default)]
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sel {
    pub claim: usize,
    pub layer: usize,
    pub row: usize,
}

pub struct Measure {
    pub line: usize,
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

/// The parts of a frame the app times itself (ms, added up).
pub const P_WAVE: usize = 0;
pub const P_SCENE: usize = 1;
pub const P_TEXT: usize = 2;
pub const P_VIEW: usize = 3;
pub const P_UPDATE: usize = 4;
pub const P_LIST: usize = 5;
pub const P_CPU: usize = 6;
pub const P_N: usize = 7;

#[derive(Default)]
pub struct Perf {
    frames: VecDeque<(f64, f32)>,
    wave_ms: VecDeque<f32>,
    pub shapes: usize,
    pub instances: usize,
    pub labels: usize,
    pub first_paint: Option<f64>,
    pub acc: [f64; P_N],
    /// instances, labels, shapes, waves drawn (summed, for the bench)
    pub counts: (u64, u64, u64, u64),
}

impl Perf {
    pub fn frame(&mut self, now: f64, dt_ms: f32) {
        self.frames.push_back((now, dt_ms));
        while self.frames.front().is_some_and(|f| now - f.0 > 1.0) {
            self.frames.pop_front();
        }
    }
    pub fn wave(&mut self, ms: f32, shapes: usize, inst: usize, labels: usize) {
        self.wave_ms.push_back(ms);
        if self.wave_ms.len() > 120 {
            self.wave_ms.pop_front();
        }
        self.shapes = shapes;
        self.instances = inst;
        self.labels = labels;
        self.counts.0 += inst as u64;
        self.counts.1 += labels as u64;
        self.counts.2 += shapes as u64;
        self.counts.3 += 1;
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

#[derive(Clone, Copy, Debug)]
pub struct Hover {
    pub wave: u32,
    pub t: f64,
    pub lane: Option<Lane>,
    pub p: Point,
}

pub struct WaveView {
    pub id: u32,
    pub vp: Option<Viewport>,
    /// the fling: ticks a second
    pub vel: f64,
    pub scroll_y: f32,
    pub follow: bool,
    pub cache: RefCell<SceneCache>,
}

pub struct ListView {
    pub id: u32,
    pub claim: usize,
    pub layer: usize,
    pub hidden: Vec<String>,
    pub follow: bool,
    pub top: usize,
    pub vis: Cell<usize>,
}

pub enum Panel {
    Wave(WaveView),
    List(ListView),
    Other(Kind),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Frame(Instant),
    Key(keyboard::Key, keyboard::Modifiers),
    WaveVp(u32, Viewport),
    WaveFling(u32, f64),
    WaveScrollY(u32, f32),
    WaveFit(u32),
    WaveZoom(u32, f64),
    WaveFollow(u32, bool),
    FocusWave(u32),
    Hover(u32, Option<(f64, Option<Lane>, Point)>),
    Select(Sel, u32),
    ZoomToSel(u32),
    MarkerClick(f64),
    ClearMarkers,
    NextBad(bool),
    ListScroll(u32, usize),
    ListClaim(u32, usize),
    ListLayer(u32, usize),
    ListCol(u32, String, bool),
    ListFollow(u32, bool),
    SearchInput(String),
    SearchStep(bool, u32),
    SearchPick(usize, u32),
    SearchScroll(usize),
    PaneDrag(pane_grid::DragEvent),
    PaneResize(pane_grid::ResizeEvent),
    PaneClick(pane_grid::Pane),
    TabSelect(pane_grid::Pane, usize),
    CloseTab(pane_grid::Pane),
    SplitOut(pane_grid::Pane),
    Maximize(pane_grid::Pane),
    Detach(pane_grid::Pane),
    AddPanel(Kind),
    LayoutPick(String),
    LayoutName(String),
    LayoutSave,
    WinOpened(window::Id),
    WinClosed(window::Id),
    WinResized(window::Id, Size),
    Theme,
    Tiles(bool),
    Continuous(bool),
    Shot(window::Screenshot),
    Info(String, f32),
    Gpu(String),
}

pub struct App {
    pub args: Args,
    pub pal: Pal,
    pub cap: Option<Capture>,
    pub analysis: Option<Analysis>,
    pub analysis_slot: Option<Arc<Mutex<Option<Analysis>>>>,
    pub sel: Option<Sel>,
    pub markers: [Option<f64>; 2],
    pub next_marker: usize,
    pub measure: Option<Measure>,
    pub hover: Option<Hover>,
    pub search: crate::panels::Search,
    pub tiles_on: bool,
    pub continuous: bool,
    pub panels: BTreeMap<u32, Panel>,
    pub panes: pane_grid::State<Pane>,
    pub detached: HashMap<window::Id, u32>,
    pub main_win: window::Id,
    pub layouts: BTreeMap<String, Conf>,
    pub layout_name: String,
    pub perf: RefCell<Perf>,
    pub bench: Option<Bench>,
    pub started: Instant,
    pub last_frame: Option<Instant>,
    pub last_cpu: Option<f64>,
    pub next_id: u32,
    pub focus_wave: u32,
    pub gpu: RefCell<String>,
    pub win_size: Size,
    pub scale: f32,
    shot_at: Option<Instant>,
    shot_asked: bool,
    zoom_done: bool,
}

impl App {
    pub fn boot(args: Args) -> (App, Task<Msg>) {
        let (main_win, open) = window::open(window::Settings {
            size: Size::new(args.size.0, args.size.1),
            exit_on_close_request: true,
            ..window::Settings::default()
        });
        let mut app = App {
            pal: theme::pal(!args.light),
            cap: None,
            analysis: None,
            analysis_slot: None,
            sel: None,
            markers: [None, None],
            next_marker: 0,
            measure: None,
            hover: None,
            search: Default::default(),
            tiles_on: true,
            continuous: args.bench.is_some(),
            panels: BTreeMap::new(),
            panes: pane_grid::State::new(Pane { tabs: vec![], active: 0 }).0,
            detached: HashMap::new(),
            main_win,
            layouts: dock::load_saved(),
            layout_name: args.layout.clone().unwrap_or("標準".into()),
            perf: RefCell::new(Perf::default()),
            bench: args.bench.clone().map(Bench::new),
            started: Instant::now(),
            last_frame: None,
            last_cpu: None,
            next_id: 1,
            focus_wave: 0,
            gpu: RefCell::new(String::from("?")),
            win_size: Size::new(args.size.0, args.size.1),
            scale: 1.0,
            shot_at: None,
            shot_asked: false,
            zoom_done: false,
            args,
        };
        let conf = app.layouts.get(&app.layout_name).cloned().unwrap_or_else(|| dock::preset(&app.layout_name));
        dock::apply(&mut app, &conf);
        app.open();
        if let Some(q) = app.args.search.clone() {
            app.search.query = q;
        }
        let mut tasks = vec![
            open.map(Msg::WinOpened),
            iced::system::information().map(|i| Msg::Gpu(format!("{} ({})", i.graphics_adapter, i.graphics_backend))),
        ];
        if app.args.detach_list {
            let found = app.panes.iter().find(|(_, p)| p.tabs.iter().any(|&t| app.kind_of(t) == Kind::List)).map(|(a, _)| *a);
            if let Some(pane) = found {
                let k = app.panes.get(pane).unwrap().tabs.iter().position(|&t| app_kind(&app.panels, t) == Kind::List).unwrap_or(0);
                app.panes.get_mut(pane).unwrap().active = k;
                tasks.push(app.detach(pane));
            }
        }
        (app, Task::batch(tasks))
    }

    fn open(&mut self) {
        if let Some(total) = self.args.synth {
            let c = model::open_synth(total);
            self.analysis_slot = Some(model::start_fake_analysis(self.args.synth_items, c.n_ticks));
            self.cap = Some(c);
        } else if let Some(p) = self.args.file.clone() {
            match model::open_file(&p, self.args.part) {
                Ok(c) => {
                    let names = c.lines.iter().map(|l| l.name.clone()).collect();
                    if !self.args.no_analysis {
                        self.analysis_slot = Some(model::start_analysis(p, c.part, c.n_parts, names, c.total_edges));
                    }
                    self.cap = Some(c);
                }
                Err(e) => eprintln!("{}: {e}", p.display()),
            }
        }
    }

    fn poll(&mut self) {
        if let Some(slot) = &self.analysis_slot {
            let got = slot.lock().unwrap().take();
            if let Some(mut a) = got {
                if let Some(cap) = &self.cap {
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
                self.analysis = Some(a);
                self.analysis_slot = None;
                crate::panels::run_search(self);
                if let Some(r) = self.args.select.take() {
                    self.select(Sel { claim: 0, layer: 0, row: r }, 999_999);
                }
            }
        }
    }

    // ------------------------------------------------------------------ panels

    pub fn new_panel(&mut self, k: Kind) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        let p = match k {
            Kind::Wave => Panel::Wave(WaveView { id, vp: None, vel: 0.0, scroll_y: 0.0, follow: true, cache: RefCell::new(SceneCache::default()) }),
            Kind::List => Panel::List(ListView { id, claim: 0, layer: 0, hidden: vec![], follow: true, top: 0, vis: Cell::new(20) }),
            k => Panel::Other(k),
        };
        if k == Kind::Wave && self.focus_wave == 0 {
            self.focus_wave = id;
        }
        self.panels.insert(id, p);
        id
    }
    pub fn drop_panel(&mut self, id: u32) {
        self.panels.remove(&id);
        if self.focus_wave == id {
            let f = self.waves().next().map(|w| w.id).unwrap_or(0);
            self.focus_wave = f;
        }
    }
    pub fn kind_of(&self, id: u32) -> Kind {
        app_kind(&self.panels, id)
    }
    pub fn title_of(&self, id: u32) -> &'static str {
        self.kind_of(id).name()
    }
    fn waves(&self) -> impl Iterator<Item = &WaveView> {
        self.panels.values().filter_map(|p| if let Panel::Wave(w) = p { Some(w) } else { None })
    }
    fn wave_mut(&mut self, id: u32) -> Option<&mut WaveView> {
        match self.panels.get_mut(&id) {
            Some(Panel::Wave(w)) => Some(w),
            _ => None,
        }
    }
    fn list_mut(&mut self, id: u32) -> Option<&mut ListView> {
        match self.panels.get_mut(&id) {
            Some(Panel::List(l)) => Some(l),
            _ => None,
        }
    }
    fn first_wave_id(&self) -> Option<u32> {
        // the first in the grid's order (the bench drives it)
        let mut ids: Vec<u32> = self.panes.iter().flat_map(|(_, p)| p.tabs.iter().copied()).filter(|&t| self.kind_of(t) == Kind::Wave).collect();
        ids.sort();
        ids.first().copied().or_else(|| self.waves().next().map(|w| w.id))
    }

    pub fn panel_body(&self, id: u32) -> Element<'_, Msg> {
        let t = Instant::now();
        let e = match self.panels.get(&id) {
            Some(Panel::Wave(w)) => self.wave_panel(w),
            Some(Panel::List(l)) => {
                let e = crate::list::list_view(self, l);
                self.perf.borrow_mut().acc[P_LIST] += t.elapsed().as_secs_f64() * 1e3;
                e
            }
            Some(Panel::Other(Kind::Search)) => crate::panels::search_view(self, id),
            Some(Panel::Other(Kind::Measure)) => crate::panels::measure_view(self),
            Some(Panel::Other(Kind::Claims)) => crate::panels::claims_view(self),
            Some(Panel::Other(Kind::Perf)) => crate::panels::perf_view(self),
            _ => text("?").into(),
        };
        e
    }

    fn wave_panel<'a>(&'a self, w: &'a WaveView) -> Element<'a, Msg> {
        let pal = self.pal;
        let id = w.id;
        let b = |s: &'static str, m: Msg| button(text(s).size(12)).padding([2, 8]).on_press(m).style(move |_, st| flat(pal, st));
        let bar = row![
            b("全体 F", Msg::WaveFit(id)),
            b("−", Msg::WaveZoom(id, 0.5)),
            b("+", Msg::WaveZoom(id, 2.0)),
            b("◀ 前の誤り", Msg::NextBad(false)),
            b("次の誤り N ▶", Msg::NextBad(true)),
            checkbox(w.follow).label("一覧と連動").size(14).text_size(12).on_toggle(move |v| Msg::WaveFollow(id, v)),
            iced::widget::space::horizontal(),
            text(match (&self.cap, w.vp) {
                (Some(c), Some(v)) => format!("{} / px · 見ている幅 {}", fmt_time(v.tpp / c.tick_hz), fmt_time(v.tpp * v.width as f64 / c.tick_hz)),
                _ => String::new(),
            })
            .size(11)
            .color(pal.dim()),
        ]
        .spacing(4)
        .padding([3, 6])
        .align_y(iced::Alignment::Center);
        column![bar, wave::wave(self, w)].into()
    }

    pub fn select(&mut self, s: Sel, src: u32) {
        self.sel = Some(s);
        let Some((a, b)) = self.sel_span() else { return };
        let n = self.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1);
        // the waves that follow go there (zoomed out if the item does not fit)
        let ids: Vec<u32> = self.waves().filter(|w| w.follow && w.id != src).map(|w| w.id).collect();
        for id in ids {
            if let Some(w) = self.wave_mut(id)
                && let Some(vp) = w.vp
            {
                let span = vp.tpp * vp.width as f64;
                let len = (b - a) as f64;
                let mut v = vp;
                if len > span * 0.8 || len * 400.0 < span {
                    v.tpp = (len * 3.0 / vp.width as f64).max(1e-3);
                }
                v.t0 = (a + b) as f64 / 2.0 - v.tpp * v.width as f64 / 2.0;
                w.vp = Some(v.clamped(n));
                w.vel = 0.0;
            }
        }
        // the lists that follow show the row
        let ids: Vec<u32> = self.panels.values().filter_map(|p| if let Panel::List(l) = p { Some(l.id) } else { None }).collect();
        for id in ids {
            if id == src {
                continue;
            }
            if let Some(l) = self.list_mut(id)
                && l.follow
            {
                l.claim = s.claim;
                l.layer = s.layer;
                let vis = l.vis.get().max(1);
                if s.row < l.top || s.row >= l.top + vis {
                    l.top = s.row.saturating_sub(vis / 3);
                }
            }
        }
    }

    pub fn sel_span(&self) -> Option<(i64, i64)> {
        let s = self.sel?;
        let it = self.analysis.as_ref()?.claims.get(s.claim)?.layers.get(s.layer)?.track.item(s.row);
        Some((it.s, it.e))
    }

    fn jump_bad(&mut self, forward: bool) {
        let Some(a) = &self.analysis else { return };
        let id = self.focus_wave;
        let Some(vp) = self.waves().find(|w| w.id == id).and_then(|w| w.vp) else { return };
        let center = vp.t_of(vp.width / 2.0) as i64;
        let mut best: Option<(i64, Sel)> = None;
        for (ci, c) in a.claims.iter().enumerate() {
            let Some(l) = c.layers.first() else { continue };
            let cur = self.sel.filter(|s| s.claim == ci && s.layer == 0).map(|s| s.row);
            let got = if forward { l.track.next_bad(cur, center) } else { l.track.prev_bad(cur, center) };
            if let Some(i) = got {
                let s = l.track.item(i).s;
                let better = match best {
                    None => true,
                    Some((bs, _)) => (forward && s < bs) || (!forward && s > bs),
                };
                if better {
                    best = Some((s, Sel { claim: ci, layer: 0, row: i }));
                }
            }
        }
        if let Some((s, sel)) = best {
            let span = vp.tpp * vp.width as f64;
            if let Some(w) = self.wave_mut(id) {
                w.vp = Some(Viewport { t0: s as f64 - span / 2.0, ..vp });
            }
            self.select(sel, 0);
        }
    }

    fn detach(&mut self, pane: pane_grid::Pane) -> Task<Msg> {
        let Some(p) = self.panes.get_mut(pane) else { return Task::none() };
        if p.tabs.is_empty() {
            return Task::none();
        }
        let id = p.tabs.remove(p.active.min(p.tabs.len() - 1));
        p.active = p.active.min(p.tabs.len().saturating_sub(1));
        if p.tabs.is_empty() {
            if self.panes.len() > 1 {
                self.panes.close(pane);
            } else {
                // the last pane: keep it with an empty wave in its place
                let w = self.new_panel(Kind::Wave);
                self.panes.get_mut(pane).unwrap().tabs.push(w);
            }
        }
        let size = match self.kind_of(id) {
            Kind::Wave => Size::new(1100.0, 600.0),
            Kind::List => Size::new(760.0, 720.0),
            _ => Size::new(520.0, 520.0),
        };
        let (wid, task) = window::open(window::Settings { size, exit_on_close_request: true, ..window::Settings::default() });
        self.detached.insert(wid, id);
        task.map(Msg::WinOpened)
    }

    /// A panel back from its window: a tab of the first pane of its kind, else a pane split off the first.
    fn redock(&mut self, id: u32) {
        let k = self.kind_of(id);
        let same = self.panes.iter().find(|(_, p)| p.tabs.iter().any(|&t| self.kind_of(t) == k)).map(|(pn, _)| *pn);
        let to = same.or_else(|| dock::first_pane(self));
        if let Some(pn) = to
            && let Some(p) = self.panes.get_mut(pn)
        {
            p.tabs.push(id);
            p.active = p.tabs.len() - 1;
        }
    }

    // ------------------------------------------------------------------ update

    pub fn update(&mut self, msg: Msg) -> Task<Msg> {
        let t = Instant::now();
        let r = self.update_inner(msg);
        self.perf.borrow_mut().acc[P_UPDATE] += t.elapsed().as_secs_f64() * 1e3;
        r
    }

    fn update_inner(&mut self, msg: Msg) -> Task<Msg> {
        let n = self.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1);
        match msg {
            Msg::Frame(now) => return self.frame(now),
            Msg::Key(k, m) => return self.key(k, m),
            Msg::WaveVp(id, vp) => {
                if let Some(w) = self.wave_mut(id) {
                    w.vp = Some(vp);
                }
            }
            Msg::WaveFling(id, v) => {
                if let Some(w) = self.wave_mut(id) {
                    w.vel = v;
                }
            }
            Msg::WaveScrollY(id, dy) => {
                let lanes_h: f32 = wave::lanes(self).iter().map(|l| l.1).sum();
                if let Some(w) = self.wave_mut(id) {
                    w.scroll_y = (w.scroll_y + dy).clamp(0.0, (lanes_h - 100.0).max(0.0));
                }
            }
            Msg::WaveFit(id) => {
                if let Some(w) = self.wave_mut(id)
                    && let Some(v) = w.vp
                {
                    w.vp = Some(Viewport::fit(n, v.width));
                    w.vel = 0.0;
                }
            }
            Msg::WaveZoom(id, f) => {
                if let Some(w) = self.wave_mut(id)
                    && let Some(v) = w.vp
                {
                    w.vp = Some(v.zoomed(f, v.width / 2.0).clamped(n));
                }
            }
            Msg::WaveFollow(id, on) => {
                if let Some(w) = self.wave_mut(id) {
                    w.follow = on;
                }
            }
            Msg::FocusWave(id) => self.focus_wave = id,
            Msg::Hover(id, h) => {
                self.hover = h.map(|(t, lane, p)| Hover { wave: id, t, lane, p });
                self.measure = None;
                if let (Some(Hover { t, lane: Some(Lane::Dig(i)), .. }), Some(cap)) = (self.hover, &self.cap) {
                    let l = &cap.lines[i];
                    let lod = l.lod.read().unwrap();
                    if lod.built_until as f64 > t || l.src.level_before(0).is_some() {
                        let (level, prev, next, next2) = view_core::digital::around(l.src.as_ref(), &lod, t as i64);
                        self.measure = Some(Measure { line: i, level, prev, next, next2 });
                    }
                }
            }
            Msg::Select(s, src) => self.select(s, src),
            Msg::ZoomToSel(id) => {
                if let Some((a, b)) = self.sel_span()
                    && let Some(w) = self.wave_mut(id)
                    && let Some(v) = w.vp
                {
                    let len = ((b - a) as f64).max(1.0);
                    let tpp = len * 1.4 / v.width as f64;
                    w.vp = Some(Viewport { t0: a as f64 - len * 0.2, tpp, width: v.width }.clamped(n));
                }
            }
            Msg::MarkerClick(t) => {
                self.markers[self.next_marker] = Some(t);
                self.next_marker = 1 - self.next_marker;
            }
            Msg::ClearMarkers => {
                self.markers = [None, None];
                self.next_marker = 0;
            }
            Msg::NextBad(fwd) => self.jump_bad(fwd),
            Msg::ListScroll(id, top) => {
                if let Some(l) = self.list_mut(id) {
                    l.top = top;
                }
            }
            Msg::ListClaim(id, c) => {
                if let Some(l) = self.list_mut(id) {
                    l.claim = c;
                    l.layer = 0;
                    l.top = 0;
                }
            }
            Msg::ListLayer(id, x) => {
                if let Some(l) = self.list_mut(id) {
                    l.layer = x;
                    l.top = 0;
                }
            }
            Msg::ListCol(id, c, on) => {
                if let Some(l) = self.list_mut(id) {
                    l.hidden.retain(|x| *x != c);
                    if !on {
                        l.hidden.push(c);
                    }
                }
            }
            Msg::ListFollow(id, on) => {
                if let Some(l) = self.list_mut(id) {
                    l.follow = on;
                }
            }
            Msg::SearchInput(q) => {
                self.search.query = q;
                if self.search.query != self.search.last {
                    crate::panels::run_search(self);
                }
            }
            Msg::SearchStep(fwd, id) => {
                let k = self.search.hits.len();
                if k > 0 {
                    self.search.cur = if fwd { (self.search.cur + 1) % k } else { (self.search.cur + k - 1) % k };
                    let s = self.search.hits[self.search.cur];
                    if self.search.cur < self.search.top || self.search.cur >= self.search.top + 10 {
                        self.search.top = self.search.cur.saturating_sub(3);
                    }
                    self.select(s, id);
                }
            }
            Msg::SearchPick(i, id) => {
                self.search.cur = i;
                if let Some(&s) = self.search.hits.get(i) {
                    self.select(s, id);
                }
            }
            Msg::SearchScroll(top) => self.search.top = top,
            Msg::PaneDrag(ev) => dock::drop(self, ev),
            Msg::PaneResize(ev) => self.panes.resize(ev.split, ev.ratio),
            Msg::PaneClick(p) => {
                if let Some(pane) = self.panes.get(p) {
                    let id = pane.tabs[pane.active.min(pane.tabs.len() - 1)];
                    if self.kind_of(id) == Kind::Wave {
                        self.focus_wave = id;
                    }
                }
            }
            Msg::TabSelect(p, i) => {
                if let Some(pane) = self.panes.get_mut(p) {
                    pane.active = i.min(pane.tabs.len() - 1);
                }
            }
            Msg::CloseTab(p) => {
                if let Some(pane) = self.panes.get_mut(p) {
                    let id = pane.tabs.remove(pane.active.min(pane.tabs.len() - 1));
                    pane.active = pane.active.min(pane.tabs.len().saturating_sub(1));
                    if pane.tabs.is_empty() {
                        self.panes.close(p);
                    }
                    self.drop_panel(id);
                }
            }
            Msg::SplitOut(p) => {
                if let Some(pane) = self.panes.get_mut(p)
                    && pane.tabs.len() > 1
                {
                    let id = pane.tabs.remove(pane.active.min(pane.tabs.len() - 1));
                    pane.active = pane.active.min(pane.tabs.len() - 1);
                    self.panes.split(pane_grid::Axis::Horizontal, p, Pane { tabs: vec![id], active: 0 });
                }
            }
            Msg::Maximize(p) => {
                if self.panes.maximized().is_some() {
                    self.panes.restore();
                } else {
                    self.panes.maximize(p);
                }
            }
            Msg::Detach(p) => return self.detach(p),
            Msg::AddPanel(k) => {
                let id = self.new_panel(k);
                if let Some(p) = dock::first_pane(self) {
                    let axis = if k == Kind::Wave { pane_grid::Axis::Horizontal } else { pane_grid::Axis::Vertical };
                    self.panes.split(axis, p, Pane { tabs: vec![id], active: 0 });
                }
            }
            Msg::LayoutPick(name) => {
                let conf = self.layouts.get(&name).cloned().unwrap_or_else(|| dock::preset(&name));
                self.layout_name = name;
                dock::apply(self, &conf);
            }
            Msg::LayoutName(s) => self.layout_name = s,
            Msg::LayoutSave => {
                let name = self.layout_name.trim().to_string();
                if !name.is_empty() {
                    self.layouts.insert(name, dock::conf_of(self));
                    dock::save_all(&self.layouts);
                }
            }
            Msg::WinOpened(id) => {
                if id == self.main_win {
                    return window::scale_factor(id).map(|s| Msg::Info("scale".into(), s));
                }
            }
            Msg::WinClosed(id) => {
                if id == self.main_win {
                    return iced::exit();
                }
                if let Some(p) = self.detached.remove(&id) {
                    self.redock(p);
                }
            }
            Msg::WinResized(id, s) => {
                if id == self.main_win {
                    self.win_size = s;
                }
            }
            Msg::Theme => self.pal = theme::pal(!self.pal.dark),
            Msg::Tiles(on) => self.tiles_on = on,
            Msg::Continuous(on) => self.continuous = on,
            Msg::Shot(s) => {
                if let Some(p) = &self.args.shot {
                    match crate::png::write(p, s.size.width, s.size.height, &s.rgba) {
                        Ok(()) => eprintln!("screenshot: {} ({}x{})", p.display(), s.size.width, s.size.height),
                        Err(e) => eprintln!("screenshot: {e}"),
                    }
                }
                return iced::exit();
            }
            Msg::Gpu(g) => *self.gpu.borrow_mut() = g,
            Msg::Info(k, v) => {
                if k == "scale" {
                    self.scale = v;
                }
            }
        }
        Task::none()
    }

    fn key(&mut self, k: keyboard::Key, m: keyboard::Modifiers) -> Task<Msg> {
        use keyboard::key::Named;
        let id = self.focus_wave;
        let n = self.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1);
        let hover_t = self.hover.filter(|h| h.wave == id).map(|h| h.t);
        let Some(vp) = self.waves().find(|w| w.id == id).and_then(|w| w.vp) else { return Task::none() };
        let set = |app: &mut App, v: Viewport| {
            if let Some(w) = app.wave_mut(id) {
                w.vp = Some(v.clamped(n));
                w.vel = 0.0;
            }
        };
        match k.as_ref() {
            keyboard::Key::Character("f") => set(self, Viewport::fit(n, vp.width)),
            keyboard::Key::Character("+") | keyboard::Key::Character("=") => set(self, vp.zoomed(2.0, vp.width / 2.0)),
            keyboard::Key::Character("-") => set(self, vp.zoomed(0.5, vp.width / 2.0)),
            keyboard::Key::Named(Named::ArrowLeft) => set(self, Viewport { t0: vp.t0 - vp.tpp * vp.width as f64 * 0.2, ..vp }),
            keyboard::Key::Named(Named::ArrowRight) => set(self, Viewport { t0: vp.t0 + vp.tpp * vp.width as f64 * 0.2, ..vp }),
            keyboard::Key::Character("n") => self.jump_bad(!m.shift()),
            keyboard::Key::Character("N") => self.jump_bad(false),
            keyboard::Key::Character("1") => self.markers[0] = Some(hover_t.unwrap_or(vp.t_of(vp.width / 2.0))),
            keyboard::Key::Character("2") => self.markers[1] = Some(hover_t.unwrap_or(vp.t_of(vp.width / 2.0))),
            _ => {}
        }
        Task::none()
    }

    fn animating(&self) -> bool {
        let loading = self.cap.as_ref().is_some_and(|c| c.lod_s.lock().unwrap().is_none());
        loading
            || self.analysis_slot.is_some()
            || self.continuous
            || self.waves().any(|w| w.vel != 0.0)
            || self.bench.as_ref().is_some_and(|b| !b.done())
            || self.args.shot.is_some()
    }

    fn frame(&mut self, now: Instant) -> Task<Msg> {
        self.poll();
        let dt_ms = self.last_frame.map(|l| now.duration_since(l).as_secs_f32() * 1e3).unwrap_or(16.7);
        self.last_frame = Some(now);
        {
            let mut p = self.perf.borrow_mut();
            p.frame(self.started.elapsed().as_secs_f64(), dt_ms);
            let cpu = thread_cpu_ms();
            if let Some(c0) = self.last_cpu {
                p.acc[P_CPU] += cpu - c0;
            }
            self.last_cpu = Some(cpu);
        }
        let n = self.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1);
        // the flings
        let dt = (dt_ms as f64 / 1e3).min(0.1);
        let ids: Vec<u32> = self.waves().filter(|w| w.vel != 0.0).map(|w| w.id).collect();
        for id in ids {
            let w = self.wave_mut(id).unwrap();
            if let Some(v) = w.vp {
                w.vp = Some(Viewport { t0: v.t0 + w.vel * dt, ..v }.clamped(n));
                w.vel *= (-dt * 4.0).exp();
                if w.vel.abs() < v.tpp * 5.0 {
                    w.vel = 0.0;
                }
            }
        }
        // the view asked for on the command line, once the first wave knows its width
        if !self.zoom_done
            && let Some((z, at)) = self.args.zoom
            && let Some(id) = self.first_wave_id()
            && let Some(w) = self.wave_mut(id)
            && let Some(v) = w.vp
        {
            let fit = Viewport::fit(n, v.width);
            let mut vp = fit.zoomed(z, 0.0);
            vp.t0 = at * n as f64 - vp.tpp * v.width as f64 / 2.0;
            w.vp = Some(vp.clamped(n));
            self.zoom_done = true;
        }
        let mut tasks = vec![];
        if let Some(mut b) = self.bench.take() {
            if !b.done() {
                let fid = self.first_wave_id();
                let width = fid.and_then(|id| self.waves().find(|w| w.id == id)).and_then(|w| w.vp).map(|v| v.width).unwrap_or(0.0);
                let gpu = self.gpu.borrow().clone();
                let (vp, ended) = bench::step(self, &mut b, dt_ms, width, &gpu);
                if let (Some(vp), Some(id)) = (vp, fid)
                    && let Some(w) = self.wave_mut(id)
                {
                    w.vp = Some(vp);
                    w.vel = 0.0;
                }
                if ended && self.args.shot.is_none() {
                    tasks.push(iced::exit());
                }
                if ended {
                    self.continuous = false;
                }
            }
            self.bench = Some(b);
        }
        // the screenshot: once everything is in (and the bench over), a moment later
        if self.args.shot.is_some() && !self.shot_asked {
            let ready = self.cap.as_ref().is_none_or(|c| c.lod_s.lock().unwrap().is_some())
                && self.analysis_slot.is_none()
                && self.bench.as_ref().is_none_or(|b| b.done());
            if ready {
                let at = *self.shot_at.get_or_insert(Instant::now());
                if at.elapsed().as_secs_f32() > 1.5 {
                    self.shot_asked = true;
                    tasks.push(window::screenshot(self.main_win).map(Msg::Shot));
                }
            }
        }
        Task::batch(tasks)
    }

    // ------------------------------------------------------------------ views

    pub fn title(&self, id: window::Id) -> String {
        let file = self.cap.as_ref().map(|c| c.title.clone()).unwrap_or_default();
        match self.detached.get(&id) {
            Some(p) => format!("{} — {file} — WireSkein 試作 3（iced）", self.title_of(*p)),
            None => format!("{file} — WireSkein 試作 3（iced）"),
        }
    }

    pub fn theme(&self, _id: window::Id) -> iced::Theme {
        self.pal.iced_theme()
    }

    pub fn view(&self, id: window::Id) -> Element<'_, Msg> {
        let t = Instant::now();
        let pal = self.pal;
        let e: Element<'_, Msg> = if let Some(&p) = self.detached.get(&id) {
            container(self.panel_body(p)).width(Length::Fill).height(Length::Fill).style(move |_| container::Style {
                background: Some(Background::Color(pal.panel)),
                text_color: Some(pal.text()),
                ..Default::default()
            })
            .into()
        } else {
            column![self.bar(), container(dock::view(self)).padding([4, 6]).height(Length::Fill), self.status()].into()
        };
        let e = container(e).width(Length::Fill).height(Length::Fill).style(move |_| container::Style {
            background: Some(Background::Color(pal.c[theme::BG])),
            text_color: Some(pal.text()),
            ..Default::default()
        });
        self.perf.borrow_mut().acc[P_VIEW] += t.elapsed().as_secs_f64() * 1e3;
        e.into()
    }

    fn bar(&self) -> Element<'_, Msg> {
        let pal = self.pal;
        let mut names: Vec<String> = dock::PRESETS.iter().map(|s| s.to_string()).collect();
        for k in self.layouts.keys() {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
        let title = self.cap.as_ref().map(|c| format!("{}{}", c.title, if c.n_parts > 1 { format!("（区画 {}/{}）", c.part + 1, c.n_parts) } else { String::new() }));
        let r = row![
            text("WireSkein").size(15),
            text(title.unwrap_or("（記録なし）".into())).size(13).color(pal.dim()),
            iced::widget::space::horizontal(),
            text("パネル").size(12).color(pal.dim()),
            pick_list(Kind::ALL, None::<Kind>, Msg::AddPanel).placeholder("足す…").text_size(12).padding([3, 8]),
            text("レイアウト").size(12).color(pal.dim()),
            pick_list(names, Some(self.layout_name.clone()), Msg::LayoutPick).text_size(12).padding([3, 8]),
            text_input("名前", &self.layout_name).on_input(Msg::LayoutName).on_submit(Msg::LayoutSave).size(12).padding([3, 6]).width(120),
            button(text("保存").size(12)).padding([3, 10]).on_press(Msg::LayoutSave),
            button(text(if pal.dark { "ライトに" } else { "ダークに" }).size(12)).padding([3, 10]).on_press(Msg::Theme),
        ]
        .spacing(8)
        .padding([6, 10])
        .align_y(iced::Alignment::Center);
        container(r)
            .style(move |_| container::Style {
                background: Some(Background::Color(pal.panel)),
                border: Border { color: pal.border, width: 1.0, radius: 0.0.into() },
                ..Default::default()
            })
            .into()
    }

    fn status(&self) -> Element<'_, Msg> {
        let pal = self.pal;
        let mut r = row![].spacing(14).padding([3, 10]).align_y(iced::Alignment::Center);
        if let Some(cap) = &self.cap {
            let done = cap.lod_done.load(std::sync::atomic::Ordering::Relaxed);
            r = r.push(text(format!("{} 本 · {:.1e} の変化 · 要約 {}/{}", cap.lines.len(), cap.total_edges as f64, done, cap.lines.len())).size(11).color(pal.dim()));
            if !cap.complete || !cap.lost.is_empty() {
                r = r.push(text(format!("データ落ち {} 区間（失敗）", cap.lost.len())).size(11).color(pal.status_text(view_core::ann::Status::Error)));
            }
        }
        if self.analysis_slot.is_some() {
            r = r.push(text("解析中…").size(11).color(pal.accent()));
        } else if let Some(a) = &self.analysis {
            let rows: usize = a.claims.iter().flat_map(|c| c.layers.first()).map(|l| l.len()).sum();
            r = r.push(text(format!("主張 {} · 項目 {rows} · 解析 {:.2} s", a.claims.len(), a.seconds)).size(11).color(pal.dim()));
        }
        let (n, avg, _) = self.perf.borrow().fps();
        r = r.push(text(format!("{n:.0} fps · {avg:.1} ms")).size(11).color(pal.dim()));
        if let Some(m) = rss_mb() {
            r = r.push(text(format!("RSS {m:.0} MB")).size(11).color(pal.dim()));
        }
        if !self.detached.is_empty() {
            r = r.push(text(format!("別の窓 {}", self.detached.len())).size(11).color(pal.dim()));
        }
        r.into()
    }

    pub fn subscription(&self) -> Subscription<Msg> {
        let mut subs = vec![
            keyboard::listen().filter_map(|e| match e {
                keyboard::Event::KeyPressed { key, modifiers, .. } => Some(Msg::Key(key, modifiers)),
                _ => None,
            }),
            window::close_events().map(Msg::WinClosed),
            window::resize_events().map(|(id, s)| Msg::WinResized(id, s)),
        ];
        if self.animating() {
            subs.push(window::frames().map(Msg::Frame));
        }
        Subscription::batch(subs)
    }
}

fn app_kind(panels: &BTreeMap<u32, Panel>, id: u32) -> Kind {
    match panels.get(&id) {
        Some(Panel::Wave(_)) => Kind::Wave,
        Some(Panel::List(_)) => Kind::List,
        Some(Panel::Other(k)) => *k,
        None => Kind::Perf,
    }
}

pub fn flat(pal: Pal, st: button::Status) -> button::Style {
    button::Style {
        background: Some(Background::Color(if matches!(st, button::Status::Hovered | button::Status::Pressed) {
            Color { a: 0.14, ..pal.text() }
        } else {
            Color { a: 0.05, ..pal.text() }
        })),
        text_color: pal.text(),
        border: Border { radius: 4.0.into(), color: pal.border, width: 1.0 },
        ..Default::default()
    }
}
