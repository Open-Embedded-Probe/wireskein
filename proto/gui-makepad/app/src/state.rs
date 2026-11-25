//! The application's state, apart from Makepad's widgets: the capture, the analysis, the choice, the markers, the
//! search, every panel's own state (a waveform's view, a list's claim / layer / columns) keyed by its dock tab's id.
//! The widgets get it through Makepad's `Scope` (`scope.data`), find their panel by the tab id on `scope.path`, read
//! it to draw and change it on events (then everything is redrawn: `cx.redraw_all`, as egui would).
//! The logic is prototype 3's app.rs / panels.rs (select, follow, next error, the search's words) without iced.

use crate::clock::Instant;
use crate::model::{self, Analysis, Capture, Rows};
use crate::scene::{Lane, SceneCache};
use crate::theme::{self, Pal};
use makepad_widgets::{DVec2, LiveId};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use view_core::Viewport;
use view_core::ann::Status;

/// The process's memory and this thread's CPU time: prototype 1's meter.rs (the same way in every prototype)
pub use crate::meter::{rss_mb, thread_cpu_ms};

#[derive(Clone, Debug, Default)]
pub struct Args {
    pub file: Option<PathBuf>,
    pub synth: Option<u64>,
    pub synth_items: usize,
    pub part: usize,
    pub light: bool,
    pub layout: Option<String>,
    pub bench: Option<PathBuf>,
    pub zoom: Option<(f64, f64)>,
    pub search: Option<String>,
    pub select: Option<usize>,
    pub no_analysis: bool,
    pub detach_list: bool,
    pub size: (f64, f64),
    pub exit_after: Option<f64>,
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
            s += &format!(
                "\n周期 {} · {} · デューティ {:.1}%",
                fmt_time(per),
                fmt_freq(1.0 / per),
                duty * 100.0
            );
        }
        s
    }
}

pub fn fmt_time(s: f64) -> String {
    let a = s.abs();
    if a == 0.0 {
        "0 s".into()
    } else if a >= 1.0 {
        format!("{s:.6} s")
    } else if a >= 1e-3 {
        format!("{:.4} ms", s * 1e3)
    } else if a >= 1e-6 {
        format!("{:.4} µs", s * 1e6)
    } else {
        format!("{:.2} ns", s * 1e9)
    }
}

pub fn fmt_freq(f: f64) -> String {
    if f >= 1e6 {
        format!("{:.4} MHz", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.4} kHz", f / 1e3)
    } else {
        format!("{f:.3} Hz")
    }
}

/// The parts of a frame the app times itself (ms, added up).
pub const P_WAVE: usize = 0;
pub const P_SCENE: usize = 1;
pub const P_TEXT: usize = 2;
pub const P_DRAW: usize = 3;
pub const P_FRAME: usize = 4;
pub const P_LIST: usize = 5;
pub const P_CPU: usize = 6;
pub const P_EVENTS: usize = 7;
pub const P_INST: usize = 8;
/// the same, as this thread's CPU time (the machine may be busy: wall time then counts the waits)
pub const P_SCENE_CPU: usize = 9;
pub const P_DRAW_CPU: usize = 10;
pub const P_N: usize = 11;

#[derive(Default)]
pub struct Perf {
    frames: VecDeque<(f64, f32)>,
    wave_ms: VecDeque<f32>,
    pub shapes: usize,
    pub instances: usize,
    pub labels: usize,
    /// the first frame with the capture drawn, shown: the frame tick after the draw whose waveform drew it (s after the
    /// capture was opened)
    pub first_paint: Option<f64>,
    /// the waveform first drew the capture (its draw list built, not yet shown)
    pub drawn_at: Option<f64>,
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
    pub wave: LiveId,
    pub t: f64,
    pub lane: Option<Lane>,
    /// the mouse, local px of the waveform
    pub p: DVec2,
}

#[derive(Default)]
pub struct WaveState {
    pub vp: Option<Viewport>,
    /// the fling: ticks a second
    pub vel: f64,
    pub scroll_y: f64,
    pub follow: bool,
    pub cache: SceneCache,
}

pub struct ListState {
    pub claim: usize,
    pub layer: usize,
    pub hidden: Vec<String>,
    pub follow: bool,
    /// a row to bring into view (the list widget scrolls there once)
    pub scroll_to: Option<usize>,
    pub vis: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    Wave,
    List,
    Search,
    Measure,
    Claims,
    Perf,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Wave,
        Kind::List,
        Kind::Search,
        Kind::Measure,
        Kind::Claims,
        Kind::Perf,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Kind::Wave => "波形",
            Kind::List => "結果の一覧",
            Kind::Search => "検索",
            Kind::Measure => "測定",
            Kind::Claims => "主張",
            Kind::Perf => "速さ",
        }
    }
    /// The template's name in the Dock (app.rs live_design).
    pub fn template(self) -> LiveId {
        match self {
            Kind::Wave => LiveId::from_str("WavePanel"),
            Kind::List => LiveId::from_str("ListPanel"),
            Kind::Search => LiveId::from_str("SearchPanel"),
            Kind::Measure => LiveId::from_str("MeasurePanel"),
            Kind::Claims => LiveId::from_str("ClaimsPanel"),
            Kind::Perf => LiveId::from_str("PerfPanel"),
        }
    }
}

pub enum Panel {
    Wave(WaveState),
    List(ListState),
    Other(Kind),
}

impl Panel {
    pub fn new(k: Kind) -> Panel {
        match k {
            Kind::Wave => Panel::Wave(WaveState {
                follow: true,
                ..Default::default()
            }),
            Kind::List => Panel::List(ListState {
                claim: 0,
                layer: 0,
                hidden: vec![],
                follow: true,
                scroll_to: None,
                vis: 20,
            }),
            k => Panel::Other(k),
        }
    }
    pub fn kind(&self) -> Kind {
        match self {
            Panel::Wave(_) => Kind::Wave,
            Panel::List(_) => Kind::List,
            Panel::Other(k) => *k,
        }
    }
}

/// The search: every word must match. `key=value` (0x.. compared as a number), `nack`, `error` / `エラー` / `誤り`,
/// `warn` / `疑わしい`, `write` / `書き込み`, `read` / `読み出し`; other words in the item's text.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub last: String,
    pub hits: Vec<Sel>,
    pub by_layer: HashMap<(usize, usize), Vec<usize>>,
    pub hit_ticks: Vec<i64>,
    pub cur: usize,
    pub took_ms: f64,
    pub scroll_to: Option<usize>,
}

pub struct St {
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
    pub search: Search,
    pub tiles_on: bool,
    pub continuous: bool,
    pub panels: BTreeMap<LiveId, Panel>,
    pub focus_wave: Option<LiveId>,
    pub perf: Perf,
    pub started: Instant,
    pub gpu: String,
    pub win_size: DVec2,
    pub dpi: f64,
    pub detached: usize,
    /// bumped when Makepad reloads its DSL (the theme switched): widgets set their texts again
    pub ui_gen: u64,
}

impl St {
    pub fn new(args: Args) -> St {
        St {
            pal: theme::pal(!args.light),
            cap: None,
            analysis: None,
            analysis_slot: None,
            sel: None,
            markers: [None, None],
            next_marker: 0,
            measure: None,
            hover: None,
            search: Search {
                query: args.search.clone().unwrap_or_default(),
                ..Default::default()
            },
            tiles_on: true,
            continuous: args.bench.is_some(),
            panels: BTreeMap::new(),
            focus_wave: None,
            perf: Perf::default(),
            started: Instant::now(),
            gpu: String::from("?"),
            win_size: DVec2 {
                x: args.size.0,
                y: args.size.1,
            },
            dpi: 1.0,
            detached: 0,
            ui_gen: 0,
            args,
        }
    }

    pub fn open(&mut self) {
        if cfg!(target_arch = "wasm32") && self.args.synth.is_none() {
            self.open_web();
        } else if let Some(total) = self.args.synth {
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

    /// The web: the capture the page fetched (`?file=`), else the example carried in the program; the summaries and
    /// the analysis made at once, here (no threads: prototypes 1 and 3 on the web do the same).
    fn open_web(&mut self) {
        const EXAMPLE: &[u8] = include_bytes!("../../../../tests/data/examples/mixed-1.wireskein");
        #[cfg(target_arch = "wasm32")]
        let fetched = crate::web::file();
        #[cfg(not(target_arch = "wasm32"))]
        let fetched: Option<(String, Vec<u8>)> = None;
        let (name, bytes) = fetched.unwrap_or_else(|| ("mixed-1.wireskein".into(), EXAMPLE.to_vec()));
        match model::open_bytes(&name, &bytes, self.args.part) {
            Ok(c) => {
                if !self.args.no_analysis {
                    let names: Vec<String> = c.lines.iter().map(|l| l.name.clone()).collect();
                    let a = model::analysis_of_bytes(&bytes, c.part, &names);
                    self.analysis_slot = Some(Arc::new(Mutex::new(Some(a))));
                }
                self.cap = Some(c);
            }
            Err(e) => {
                let m = format!("{name}: {e}");
                eprintln!("{m}");
                #[cfg(target_arch = "wasm32")]
                crate::web::log(&m);
            }
        }
    }

    /// The analysis in, once: true when something came.
    pub fn poll(&mut self) -> bool {
        let Some(slot) = &self.analysis_slot else { return false };
        let got = slot.lock().unwrap().take();
        let Some(mut a) = got else { return false };
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
            a.claims
                .iter()
                .flat_map(|c| c.layers.iter())
                .map(|l| l.len())
                .sum::<usize>()
        );
        self.analysis = Some(a);
        self.analysis_slot = None;
        self.run_search();
        if let Some(r) = self.args.select.take() {
            self.select(
                Sel {
                    claim: 0,
                    layer: 0,
                    row: r,
                },
                None,
            );
        }
        true
    }

    pub fn n_ticks(&self) -> i64 {
        self.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1)
    }

    pub fn wave_mut(&mut self, id: LiveId) -> Option<&mut WaveState> {
        match self.panels.get_mut(&id) {
            Some(Panel::Wave(w)) => Some(w),
            _ => None,
        }
    }
    pub fn list_mut(&mut self, id: LiveId) -> Option<&mut ListState> {
        match self.panels.get_mut(&id) {
            Some(Panel::List(l)) => Some(l),
            _ => None,
        }
    }
    pub fn wave_ids(&self) -> Vec<LiveId> {
        self.panels
            .iter()
            .filter(|(_, p)| matches!(p, Panel::Wave(_)))
            .map(|(id, _)| *id)
            .collect()
    }

    /// The panel a widget belongs to: the nearest id on its scope's path that is a panel.
    pub fn panel_of(&self, path: &[LiveId]) -> Option<LiveId> {
        path.iter().rev().find(|id| self.panels.contains_key(id)).copied()
    }

    pub fn animating(&self) -> bool {
        let loading = self.cap.as_ref().is_some_and(|c| c.lod_s.lock().unwrap().is_none());
        loading
            || self.analysis_slot.is_some()
            || self.continuous
            || self
                .panels
                .values()
                .any(|p| matches!(p, Panel::Wave(w) if w.vel != 0.0))
    }

    /// The flings, one frame of `dt` seconds.
    pub fn flings(&mut self, dt: f64) {
        let n = self.n_ticks();
        for p in self.panels.values_mut() {
            if let Panel::Wave(w) = p
                && w.vel != 0.0
                && let Some(v) = w.vp
            {
                w.vp = Some(
                    Viewport {
                        t0: v.t0 + w.vel * dt,
                        ..v
                    }
                    .clamped(n),
                );
                w.vel *= (-dt * 4.0).exp();
                if w.vel.abs() < v.tpp * 5.0 {
                    w.vel = 0.0;
                }
            }
        }
    }

    pub fn select(&mut self, s: Sel, src: Option<LiveId>) {
        self.sel = Some(s);
        let Some((a, b)) = self.sel_span() else { return };
        let n = self.n_ticks();
        for (id, p) in self.panels.iter_mut() {
            if Some(*id) == src {
                continue;
            }
            match p {
                Panel::Wave(w) if w.follow => {
                    if let Some(vp) = w.vp {
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
                Panel::List(l) if l.follow => {
                    l.claim = s.claim;
                    l.layer = s.layer;
                    l.scroll_to = Some(s.row);
                }
                _ => {}
            }
        }
    }

    pub fn sel_span(&self) -> Option<(i64, i64)> {
        let s = self.sel?;
        let it = self
            .analysis
            .as_ref()?
            .claims
            .get(s.claim)?
            .layers
            .get(s.layer)?
            .track
            .item(s.row);
        Some((it.s, it.e))
    }

    pub fn zoom_to_sel(&mut self, id: LiveId) {
        let n = self.n_ticks();
        if let Some((a, b)) = self.sel_span()
            && let Some(w) = self.wave_mut(id)
            && let Some(v) = w.vp
        {
            let len = ((b - a) as f64).max(1.0);
            let tpp = len * 1.4 / v.width as f64;
            w.vp = Some(
                Viewport {
                    t0: a as f64 - len * 0.2,
                    tpp,
                    width: v.width,
                }
                .clamped(n),
            );
        }
    }

    pub fn jump_bad(&mut self, forward: bool) {
        let Some(a) = &self.analysis else { return };
        let Some(id) = self.focus_wave.or_else(|| self.wave_ids().first().copied()) else {
            return;
        };
        let Some(vp) = (match self.panels.get(&id) {
            Some(Panel::Wave(w)) => w.vp,
            _ => None,
        }) else {
            return;
        };
        let center = vp.t_of(vp.width / 2.0) as i64;
        let mut best: Option<(i64, Sel)> = None;
        for (ci, c) in a.claims.iter().enumerate() {
            let Some(l) = c.layers.first() else { continue };
            let cur = self.sel.filter(|s| s.claim == ci && s.layer == 0).map(|s| s.row);
            let got = if forward {
                l.track.next_bad(cur, center)
            } else {
                l.track.prev_bad(cur, center)
            };
            if let Some(i) = got {
                let s = l.track.item(i).s;
                let better = match best {
                    None => true,
                    Some((bs, _)) => (forward && s < bs) || (!forward && s > bs),
                };
                if better {
                    best = Some((
                        s,
                        Sel {
                            claim: ci,
                            layer: 0,
                            row: i,
                        },
                    ));
                }
            }
        }
        if let Some((s, sel)) = best {
            let span = vp.tpp * vp.width as f64;
            if let Some(w) = self.wave_mut(id) {
                w.vp = Some(Viewport {
                    t0: s as f64 - span / 2.0,
                    ..vp
                });
            }
            self.select(sel, None);
        }
    }

    pub fn hover_at(&mut self, wave: LiveId, h: Option<(f64, Option<Lane>, DVec2)>) {
        self.hover = h.map(|(t, lane, p)| Hover { wave, t, lane, p });
        self.measure = None;
        if let (
            Some(Hover {
                t,
                lane: Some(Lane::Dig(i)),
                ..
            }),
            Some(cap),
        ) = (self.hover, &self.cap)
        {
            let l = &cap.lines[i];
            let lod = l.lod.read().unwrap();
            if lod.built_until as f64 > t || l.src.level_before(0).is_some() {
                let (level, prev, next, next2) = view_core::digital::around(l.src.as_ref(), &lod, t as i64);
                self.measure = Some(Measure {
                    line: i,
                    level,
                    prev,
                    next,
                    next2,
                });
            }
        }
    }

    pub fn marker_click(&mut self, t: f64) {
        self.markers[self.next_marker] = Some(t);
        self.next_marker = 1 - self.next_marker;
    }

    pub fn search_step(&mut self, fwd: bool) {
        let k = self.search.hits.len();
        if k > 0 {
            self.search.cur = if fwd {
                (self.search.cur + 1) % k
            } else {
                (self.search.cur + k - 1) % k
            };
            let s = self.search.hits[self.search.cur];
            self.search.scroll_to = Some(self.search.cur);
            self.select(s, None);
        }
    }

    pub fn run_search(&mut self) {
        let t = Instant::now();
        let s = &mut self.search;
        s.hits.clear();
        s.by_layer.clear();
        s.hit_ticks.clear();
        s.cur = 0;
        s.scroll_to = Some(0);
        s.last = s.query.clone();
        let ws = words(&s.query);
        if ws.is_empty() {
            return;
        }
        let Some(a) = &self.analysis else { return };
        for (ci, c) in a.claims.iter().enumerate() {
            for (li, l) in c.layers.iter().enumerate() {
                if matches!(l.rows, Rows::Bits(_)) {
                    continue; // (bits: not searched)
                }
                let mut v = vec![];
                for i in 0..l.len() {
                    let it = l.track.item(i);
                    let row = l.value(i);
                    if matches(&ws, row.as_ref(), l.track.text(i), it.status) {
                        v.push(i);
                        if s.hits.len() < 2_000_000 {
                            s.hits.push(Sel {
                                claim: ci,
                                layer: li,
                                row: i,
                            });
                        }
                        if li == 0 && s.hit_ticks.len() < 20_000 {
                            s.hit_ticks.push(it.s);
                        }
                    }
                }
                if !v.is_empty() {
                    s.by_layer.insert((ci, li), v);
                }
            }
        }
        s.took_ms = t.elapsed().as_secs_f64() * 1e3;
    }
}

pub fn cell(v: &Value, col: &str) -> String {
    match v.get(col) {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => {
            if matches!(col, "addr" | "id" | "reg" | "data" | "value") {
                n.as_i64().map(|i| format!("0x{i:02x}")).unwrap_or(n.to_string())
            } else {
                n.to_string()
            }
        }
        Some(Value::Bool(b)) => {
            if *b {
                "✓".into()
            } else {
                "×".into()
            }
        }
        Some(Value::Null) => "–".into(),
        Some(x) => {
            let s = x.to_string();
            if s.chars().count() > 40 {
                format!("{}…", s.chars().take(40).collect::<String>())
            } else {
                s
            }
        }
    }
}

fn num(s: &str) -> Option<i64> {
    if let Some(h) = s.strip_prefix("0x") {
        i64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

enum Word {
    Field(String, String),
    Status(Status),
    Rw(&'static str),
    Text(String),
}

fn words(q: &str) -> Vec<Word> {
    q.split_whitespace()
        .map(|w| {
            let lw = w.to_lowercase();
            if let Some((k, v)) = w.split_once('=') {
                return Word::Field(k.into(), v.to_lowercase());
            }
            match lw.as_str() {
                "nack" | "ナック" => Word::Status(Status::Note),
                "error" | "エラー" | "誤り" => Word::Status(Status::Error),
                "warn" | "疑わしい" => Word::Status(Status::Warn),
                "write" | "書き込み" | "書き" => Word::Rw("write"),
                "read" | "読み出し" | "読み" => Word::Rw("read"),
                _ => Word::Text(lw),
            }
        })
        .collect()
}

fn matches(ws: &[Word], v: &Value, text: &str, status: Status) -> bool {
    ws.iter().all(|w| match w {
        Word::Field(k, want) => match v.get(k) {
            Some(Value::Number(n)) => num(want).is_some_and(|x| n.as_i64() == Some(x)),
            Some(Value::String(s)) => s.to_lowercase().contains(want.as_str()),
            Some(Value::Bool(b)) => matches!(
                (want.as_str(), b),
                ("true" | "1" | "✓", true) | ("false" | "0" | "✗", false)
            ),
            _ => false,
        },
        Word::Status(s) => {
            if *s == Status::Note {
                status == Status::Note || text.contains("NACK")
            } else {
                status >= *s && status != Status::Unread
            }
        }
        Word::Rw(rw) => v.get("rw").and_then(|x| x.as_str()) == Some(rw),
        Word::Text(t) => text.to_lowercase().contains(t.as_str()) || cell(v, "addr").to_lowercase() == *t,
    })
}
