//! The application: Makepad's Root and Window, a bar above, the Dock with the panels as its tab templates, a status
//! line. The state (state.rs) goes to every widget through the Scope. The frame loop (NextFrame): the analysis polled,
//! the flings, the bench, a redraw of everything while something moves. Layouts: three presets and saved ones (JSON),
//! loaded into the Dock (`load_state`). Any panel can go to a window of its own (a Makepad Window made from a template
//! at run time) and comes back to the Dock when that window is closed. Dark / light: our palette at once, Makepad's
//! theme by relinking `theme` and reloading the DSL (live edit).

use crate::bench::{self, Bench};
use crate::state::*;
use crate::theme;
use makepad_widgets::splitter::{SplitterAlign, SplitterAxis};
use makepad_widgets::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;
use view_core::Viewport;

pub static ARGS: OnceLock<Args> = OnceLock::new();

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;
    use crate::wave::WaveView;
    use crate::list::ListPanel;
    use crate::panels::*;

    WavePanelBase = {{WavePanel}} {}
    WavePanel = <WavePanelBase> {
        width: Fill, height: Fill, flow: Down
        bar = <View> {
            width: Fill, height: Fit, flow: Right, spacing: 4, padding: { left: 6, right: 6, top: 3, bottom: 3 },
            align: { y: 0.5 }
            fit = <Button> { text: "全体 F" }
            zout = <Button> { text: "−" }
            zin = <Button> { text: "+" }
            prev_bad = <Button> { text: "◀ 前の誤り" }
            next_bad = <Button> { text: "次の誤り N ▶" }
            follow = <CheckBox> { text: "一覧と連動" }
            <View> { width: Fill, height: 1 }
            scale = <Label> { text: "" draw_text: { color: (THEME_COLOR_TEXT_META) } }
            detach = <Button> { text: "別の窓へ" }
        }
        wave = <WaveView> {}
    }

    DetachedList = <View> { flow: Down, <ListPanel> {} }

    App = {{App}} {
        ui: <Root> {
            main_window = <Window> {
                window: { inner_size: vec2(1600, 960), position: vec2(0, 0), title: "WireSkein 試作 4（Makepad）" }
                body = <View> {
                    width: Fill, height: Fill, flow: Down
                    show_bg: true
                    draw_bg: { color: (THEME_COLOR_BG_APP) }
                    top = <View> {
                        width: Fill, height: Fit, flow: Right, spacing: 8,
                        padding: { left: 10, right: 10, top: 6, bottom: 6 }, align: { y: 0.5 }
                        <Label> { text: "WireSkein" draw_text: { text_style: <THEME_FONT_BOLD> { font_size: 11 } } }
                        title = <Label> { text: "（記録なし）" draw_text: { color: (THEME_COLOR_TEXT_META) } }
                        <View> { width: Fill, height: 1 }
                        <Label> { text: "パネルを足す" draw_text: { color: (THEME_COLOR_TEXT_META) } }
                        add = <DropDown> { width: 120, labels: ["波形", "結果の一覧", "検索", "測定", "主張", "速さ"] }
                        <Label> { text: "別の窓へ" draw_text: { color: (THEME_COLOR_TEXT_META) } }
                        detach = <DropDown> { width: 120, labels: ["波形", "結果の一覧", "検索", "測定", "主張", "速さ"] }
                        <Label> { text: "レイアウト" draw_text: { color: (THEME_COLOR_TEXT_META) } }
                        layout = <DropDown> { width: 140, labels: ["標準", "一覧を大きく", "波形 2 つ"] }
                        layout_name = <TextInput> { width: 120, empty_text: "名前" }
                        save = <Button> { text: "保存" }
                        theme = <Button> { text: "ライトに" }
                    }
                    dock = <Dock> {
                        width: Fill, height: Fill
                        root = Tabs { tabs: [], selected: 0 }
                        WavePanel = <WavePanel> {}
                        ListPanel = <ListPanel> {}
                        SearchPanel = <SearchPanel> {}
                        MeasurePanel = <MeasurePanel> {}
                        ClaimsPanel = <ClaimsPanel> {}
                        PerfPanel = <PerfPanel> {}
                    }
                    status = <Label> {
                        margin: { left: 10, top: 3, bottom: 4 }
                        text: ""
                        draw_text: { color: (THEME_COLOR_TEXT_META), text_style: { font_size: 8.5 } }
                    }
                }
            }
        }
        win_wave: <Window> { window: { inner_size: vec2(1100, 600), position: vec2(200, 120), title: "波形 — WireSkein 試作 4" }
            body = { <WavePanel> {} } }
        win_list: <Window> { window: { inner_size: vec2(780, 720), position: vec2(1010, 40), title: "結果の一覧 — WireSkein 試作 4" }
            body = { <ListPanel> {} } }
        win_search: <Window> { window: { inner_size: vec2(700, 600), position: vec2(840, 200), title: "検索 — WireSkein 試作 4" }
            body = { <SearchPanel> {} } }
        win_measure: <Window> { window: { inner_size: vec2(520, 520), position: vec2(1000, 300), title: "測定 — WireSkein 試作 4" }
            body = { <MeasurePanel> {} } }
        win_claims: <Window> { window: { inner_size: vec2(520, 520), position: vec2(1000, 300), title: "主張 — WireSkein 試作 4" }
            body = { <ClaimsPanel> {} } }
        win_perf: <Window> { window: { inner_size: vec2(620, 420), position: vec2(900, 400), title: "速さ — WireSkein 試作 4" }
            body = { <PerfPanel> {} } }
    }
}

app_main!(App);

// ---------------------------------------------------------------------------------------------- the wave panel

#[derive(Clone, Debug, DefaultNone)]
pub enum PanelAction {
    Detach(LiveId),
    None,
}

#[derive(Live, LiveHook, Widget)]
pub struct WavePanel {
    #[deref]
    view: View,
}

impl Widget for WavePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if actions.is_empty() {
            return;
        }
        let Some(st) = scope.data.get_mut::<St>() else { return };
        let Some(id) = st.panel_of(&scope.path.data) else {
            return;
        };
        let n = st.n_ticks();
        let vp = match st.panels.get(&id) {
            Some(Panel::Wave(w)) => w.vp,
            _ => None,
        };
        let set = |st: &mut St, v: Viewport| {
            if let Some(w) = st.wave_mut(id) {
                w.vp = Some(v.clamped(n));
                w.vel = 0.0;
            }
        };
        let mut changed = true;
        if let Some(v) = vp {
            if self.view.button(id!(fit)).clicked(&actions) {
                set(st, Viewport::fit(n, v.width));
            } else if self.view.button(id!(zin)).clicked(&actions) {
                set(st, v.zoomed(2.0, v.width / 2.0));
            } else if self.view.button(id!(zout)).clicked(&actions) {
                set(st, v.zoomed(0.5, v.width / 2.0));
            } else if self.view.button(id!(prev_bad)).clicked(&actions) {
                st.focus_wave = Some(id);
                st.jump_bad(false);
            } else if self.view.button(id!(next_bad)).clicked(&actions) {
                st.focus_wave = Some(id);
                st.jump_bad(true);
            } else {
                changed = false;
            }
        }
        if let Some(on) = self.view.check_box(id!(follow)).changed(&actions)
            && let Some(w) = st.wave_mut(id)
        {
            w.follow = on;
        }
        if self.view.button(id!(detach)).clicked(&actions) {
            cx.action(PanelAction::Detach(id));
        }
        if changed {
            cx.redraw_all();
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(st) = scope.data.get::<St>()
            && let Some(id) = st.panel_of(&scope.path.data)
            && let Some(Panel::Wave(w)) = st.panels.get(&id)
        {
            self.view.check_box(id!(follow)).set_active(cx, w.follow);
            let s = match (&st.cap, w.vp) {
                (Some(c), Some(v)) => format!(
                    "{} / px · 見ている幅 {}",
                    fmt_time(v.tpp / c.tick_hz),
                    fmt_time(v.tpp * v.width as f64 / c.tick_hz)
                ),
                _ => String::new(),
            };
            self.view.label(id!(scale)).set_text(cx, &s);
            self.view.button(id!(detach)).set_visible(cx, !is_detached(id));
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The panels living in windows of their own (set by the app; the wave panel hides its button there).
fn is_detached(id: LiveId) -> bool {
    DETACHED.with(|d| d.borrow().contains(&id))
}

thread_local! {
    static DETACHED: std::cell::RefCell<Vec<LiveId>> = const { std::cell::RefCell::new(Vec::new()) };
}

// ---------------------------------------------------------------------------------------------- layouts

/// A layout: splits and tab groups of panel kinds (saved as JSON; the Dock's own state holds tab ids, not kinds).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Conf {
    Split {
        side_by_side: bool,
        ratio: f64,
        a: Box<Conf>,
        b: Box<Conf>,
    },
    Tabs(Vec<Kind>),
}

fn split(side_by_side: bool, ratio: f64, a: Conf, b: Conf) -> Conf {
    Conf::Split {
        side_by_side,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}

pub const PRESETS: [&str; 3] = ["標準", "一覧を大きく", "波形 2 つ"];

pub fn preset(name: &str) -> Conf {
    use Kind::*;
    match name {
        "一覧を大きく" => split(
            true,
            0.5,
            Conf::Tabs(vec![Wave]),
            split(
                false,
                0.75,
                Conf::Tabs(vec![List, Search]),
                Conf::Tabs(vec![Measure, Claims, Perf]),
            ),
        ),
        "波形 2 つ" => split(
            true,
            0.68,
            split(false, 0.5, Conf::Tabs(vec![Wave]), Conf::Tabs(vec![Wave])),
            split(
                false,
                0.6,
                Conf::Tabs(vec![List]),
                Conf::Tabs(vec![Search, Measure, Claims, Perf]),
            ),
        ),
        _ => split(
            true,
            0.68,
            Conf::Tabs(vec![Wave]),
            split(
                false,
                0.62,
                Conf::Tabs(vec![List, Search]),
                Conf::Tabs(vec![Measure, Claims, Perf]),
            ),
        ),
    }
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wsk-makepad").join("layouts.json"))
}

fn load_saved() -> BTreeMap<String, Conf> {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_all(m: &BTreeMap<String, Conf>) {
    if let Some(p) = store_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(m).unwrap_or_default());
    }
}

// ---------------------------------------------------------------------------------------------- the app

#[derive(Live, LiveHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
    #[live]
    win_wave: Option<LivePtr>,
    #[live]
    win_list: Option<LivePtr>,
    #[live]
    win_search: Option<LivePtr>,
    #[live]
    win_measure: Option<LivePtr>,
    #[live]
    win_claims: Option<LivePtr>,
    #[live]
    win_perf: Option<LivePtr>,
    #[rust]
    st: Option<St>,
    #[rust]
    next_frame: NextFrame,
    #[rust]
    next_id: u64,
    #[rust]
    layouts: BTreeMap<String, Conf>,
    #[rust]
    layout_name: String,
    /// panels in windows of their own: (tab id, the window)
    #[rust]
    windows: Vec<(LiveId, WidgetRef)>,
    #[rust]
    bench: Option<Bench>,
    #[rust]
    last_frame: Option<f64>,
    #[rust]
    last_cpu: Option<f64>,
    #[rust]
    zoom_done: bool,
    #[rust]
    linux_drag: Option<LiveId>,
    #[rust]
    started: bool,
    #[rust]
    status_at: f64,
    #[rust]
    exit_timer: Timer,
    #[rust]
    sized: bool,
    #[rust]
    last_draw: Option<Instant>,
}

impl LiveRegister for App {
    fn live_register(cx: &mut Cx) {
        makepad_widgets::live_design(cx);
        crate::theme_light::live_design(cx);
        if ARGS.get().is_some_and(|a| a.light) {
            cx.link(live_id!(theme), live_id!(theme_desktop_light_fixed));
        }
        crate::wave::live_design(cx);
        crate::list::live_design(cx);
        crate::panels::live_design(cx);
    }
}

impl App {
    fn st(&mut self) -> &mut St {
        self.st.as_mut().unwrap()
    }

    fn new_tab_id(&mut self) -> LiveId {
        self.next_id += 1;
        LiveId(0x5753_4b00_0000_0000 + self.next_id)
    }

    /// The Dock's state from a layout: new panels for its tabs (the panels in windows stay).
    fn apply_layout(&mut self, cx: &mut Cx, conf: &Conf) {
        let detached: Vec<LiveId> = self.windows.iter().map(|w| w.0).collect();
        self.st().panels.retain(|id, _| detached.contains(id));
        let mut items: HashMap<LiveId, DockItem> = HashMap::new();
        self.build(conf, live_id!(root), &mut items);
        if let Some(mut d) = self.ui.dock(id!(dock)).borrow_mut() {
            d.load_state(cx, items);
        }
        let st = self.st();
        if st.focus_wave.is_some_and(|f| !st.panels.contains_key(&f)) {
            st.focus_wave = None;
        }
        cx.redraw_all();
    }

    fn build(&mut self, c: &Conf, id: LiveId, items: &mut HashMap<LiveId, DockItem>) {
        match c {
            Conf::Split {
                side_by_side,
                ratio,
                a,
                b,
            } => {
                let (ia, ib) = (self.new_tab_id(), self.new_tab_id());
                items.insert(
                    id,
                    DockItem::Splitter {
                        axis: if *side_by_side {
                            SplitterAxis::Horizontal
                        } else {
                            SplitterAxis::Vertical
                        },
                        align: SplitterAlign::Weighted(*ratio),
                        a: ia,
                        b: ib,
                    },
                );
                self.build(a, ia, items);
                self.build(b, ib, items);
            }
            Conf::Tabs(kinds) => {
                let mut tabs = vec![];
                for &k in kinds {
                    let t = self.new_tab_id();
                    items.insert(
                        t,
                        DockItem::Tab {
                            name: k.name().to_string(),
                            template: live_id!(CloseableTab),
                            kind: k.template(),
                        },
                    );
                    self.st().panels.insert(t, Panel::new(k));
                    tabs.push(t);
                }
                items.insert(
                    id,
                    DockItem::Tabs {
                        tabs,
                        selected: 0,
                        closable: true,
                    },
                );
            }
        }
    }

    /// The Dock's state as a layout (tab ids back to kinds).
    fn conf_of(&self, cx: &mut Cx) -> Option<Conf> {
        let _ = cx;
        let items = self.ui.dock(id!(dock)).clone_state()?;
        let st = self.st.as_ref()?;
        fn walk(st: &St, items: &HashMap<LiveId, DockItem>, id: LiveId) -> Conf {
            match items.get(&id) {
                Some(DockItem::Splitter { axis, align, a, b }) => split(
                    matches!(axis, SplitterAxis::Horizontal),
                    match align {
                        SplitterAlign::Weighted(w) => *w,
                        _ => 0.5,
                    },
                    walk(st, items, *a),
                    walk(st, items, *b),
                ),
                Some(DockItem::Tabs { tabs, .. }) => {
                    Conf::Tabs(tabs.iter().filter_map(|t| st.panels.get(t).map(|p| p.kind())).collect())
                }
                _ => Conf::Tabs(vec![]),
            }
        }
        Some(walk(st, &items, live_id!(root)))
    }

    /// Makepad 1.0's Dock loses its root when the last tab of a group right under the root goes (close_tab / a move:
    /// `unsplit_tabs` removes the root splitter and has no parent to give the other side to). The orphaned side is
    /// made the root again.
    fn fix_root(&mut self, cx: &mut Cx) {
        let dock = self.ui.dock(id!(dock));
        let Some(mut items) = dock.clone_state() else { return };
        if items.contains_key(&live_id!(root)) {
            return;
        }
        let children: Vec<LiveId> = items
            .values()
            .flat_map(|it| match it {
                DockItem::Splitter { a, b, .. } => vec![*a, *b],
                DockItem::Tabs { tabs, .. } => tabs.clone(),
                _ => vec![],
            })
            .collect();
        let orphan = items
            .iter()
            .find(|(id, it)| !matches!(it, DockItem::Tab { .. }) && !children.contains(id))
            .map(|(id, _)| *id);
        if let Some(o) = orphan
            && let Some(it) = items.remove(&o)
        {
            items.insert(live_id!(root), it);
            if let Some(mut d) = dock.borrow_mut() {
                d.load_state(cx, items);
            }
            eprintln!("dock: the root lost by close / move, the orphaned side made the root again");
        }
        cx.redraw_all();
    }

    /// A panel of `kind` added: a tab of the first group holding its kind, else of the root's first group.
    fn add_panel(&mut self, cx: &mut Cx, k: Kind) {
        let dock = self.ui.dock(id!(dock));
        let Some(items) = dock.clone_state() else { return };
        let st = self.st.as_ref().unwrap();
        let mut groups: Vec<(LiveId, bool)> = items
            .iter()
            .filter_map(|(id, it)| match it {
                DockItem::Tabs { tabs, .. } => Some((
                    *id,
                    tabs.iter().any(|t| st.panels.get(t).is_some_and(|p| p.kind() == k)),
                )),
                _ => None,
            })
            .collect();
        groups.sort_by_key(|g| (!g.1, g.0.0));
        let Some(&(parent, _)) = groups.first() else { return };
        let t = self.new_tab_id();
        self.st().panels.insert(t, Panel::new(k));
        dock.create_and_select_tab(
            cx,
            parent,
            t,
            k.template(),
            k.name().to_string(),
            live_id!(CloseableTab),
            None,
        );
        cx.redraw_all();
    }

    fn detach_kind(&mut self, cx: &mut Cx, k: Kind) {
        let dock = self.ui.dock(id!(dock));
        let Some(items) = dock.clone_state() else { return };
        let st = self.st.as_ref().unwrap();
        let found = items
            .iter()
            .filter(|(_, it)| matches!(it, DockItem::Tab { .. }))
            .map(|(id, _)| *id)
            .filter(|id| st.panels.get(id).is_some_and(|p| p.kind() == k))
            .min_by_key(|id| id.0);
        if let Some(id) = found {
            self.detach(cx, id);
        }
    }

    /// A panel out of the Dock into a window of its own (the same panel state: its tab id stays on the scope path).
    fn detach(&mut self, cx: &mut Cx, id: LiveId) {
        let Some(k) = self.st.as_ref().and_then(|s| s.panels.get(&id)).map(|p| p.kind()) else {
            return;
        };
        let ptr = match k {
            Kind::Wave => self.win_wave,
            Kind::List => self.win_list,
            Kind::Search => self.win_search,
            Kind::Measure => self.win_measure,
            Kind::Claims => self.win_claims,
            Kind::Perf => self.win_perf,
        };
        let dock = self.ui.dock(id!(dock));
        dock.close_tab(cx, id);
        self.fix_root(cx);
        let w = WidgetRef::new_from_ptr(cx, ptr);
        self.windows.push((id, w));
        DETACHED.with(|d| d.borrow_mut().push(id));
        self.st().detached = self.windows.len();
        cx.redraw_all();
    }

    /// A panel back from its closed window: a tab of the first group holding its kind, else of the first group.
    fn redock(&mut self, cx: &mut Cx, id: LiveId) {
        DETACHED.with(|d| d.borrow_mut().retain(|x| *x != id));
        let Some(k) = self.st.as_ref().and_then(|s| s.panels.get(&id)).map(|p| p.kind()) else {
            return;
        };
        let dock = self.ui.dock(id!(dock));
        let Some(items) = dock.clone_state() else { return };
        let st = self.st.as_ref().unwrap();
        let mut groups: Vec<(LiveId, bool)> = items
            .iter()
            .filter_map(|(gid, it)| match it {
                DockItem::Tabs { tabs, .. } => Some((
                    *gid,
                    tabs.iter().any(|t| st.panels.get(t).is_some_and(|p| p.kind() == k)),
                )),
                _ => None,
            })
            .collect();
        groups.sort_by_key(|g| (!g.1, g.0.0));
        if let Some(&(parent, _)) = groups.first() {
            dock.create_and_select_tab(
                cx,
                parent,
                id,
                k.template(),
                k.name().to_string(),
                live_id!(CloseableTab),
                None,
            );
        } else {
            self.st().panels.remove(&id);
        }
        self.st().detached = self.windows.len();
        cx.redraw_all();
    }

    fn set_theme(&mut self, cx: &mut Cx, dark: bool) {
        self.st().pal = theme::pal(dark);
        // Makepad's own widgets: the theme relinked and the DSL reloaded (the live edit path)
        cx.link(
            live_id!(theme),
            if dark {
                live_id!(theme_desktop_dark)
            } else {
                live_id!(theme_desktop_light_fixed)
            },
        );
        cx.reload_ui_dsl();
        cx.redraw_all();
    }

    /// The texts the app sets on Makepad's widgets (again after a reload of the DSL: it puts the DSL's back).
    fn set_texts(&mut self, cx: &mut Cx) {
        let Some(st) = &self.st else { return };
        if let Some(c) = &st.cap {
            let title = format!(
                "{}{}",
                c.title,
                if c.n_parts > 1 {
                    format!("（区画 {}/{}）", c.part + 1, c.n_parts)
                } else {
                    String::new()
                }
            );
            self.ui.label(id!(title)).set_text(cx, &title);
        }
        let dark = st.pal.dark;
        self.ui
            .button(id!(theme))
            .set_text(cx, if dark { "ライトに" } else { "ダークに" });
        let mut names: Vec<String> = PRESETS.iter().map(|s| s.to_string()).collect();
        for k in self.layouts.keys() {
            if !names.contains(k) {
                names.push(k.clone());
            }
        }
        let dd = self.ui.drop_down(id!(layout));
        dd.set_labels(cx, names);
        dd.set_selected_by_label(&self.layout_name.clone(), cx);
        self.update_status(cx);
    }

    fn update_status(&mut self, cx: &mut Cx) {
        let Some(st) = &self.st else { return };
        let mut parts: Vec<String> = vec![];
        if let Some(cap) = &st.cap {
            let done = cap.lod_done.load(std::sync::atomic::Ordering::Relaxed);
            parts.push(format!(
                "{} 本 · {:.1e} の変化 · 要約 {}/{}",
                cap.lines.len(),
                cap.total_edges as f64,
                done,
                cap.lines.len()
            ));
            if !cap.complete || !cap.lost.is_empty() {
                parts.push(format!("データ落ち {} 区間（失敗）", cap.lost.len()));
            }
        }
        if st.analysis_slot.is_some() {
            parts.push("解析中…".into());
        } else if let Some(a) = &st.analysis {
            let rows: usize = a.claims.iter().flat_map(|c| c.layers.first()).map(|l| l.len()).sum();
            parts.push(format!(
                "主張 {} · 項目 {rows} · 解析 {:.2} s",
                a.claims.len(),
                a.seconds
            ));
        }
        let (n, avg, _) = st.perf.fps();
        parts.push(format!("{n:.0} fps · {avg:.1} ms"));
        if let Some(m) = rss_mb() {
            parts.push(format!("RSS {m:.0} MB"));
        }
        if st.detached > 0 {
            parts.push(format!("別の窓 {}", st.detached));
        }
        let s = parts.join("   ·   ");
        self.ui.label(id!(status)).set_text(cx, &s);
    }

    fn startup(&mut self, cx: &mut Cx) {
        let args = ARGS.get().cloned().unwrap_or_default();
        let mut st = St::new(args.clone());
        st.open();
        st.gpu = match cx.os_type() {
            OsType::Windows => "Direct3D 11（Makepad の Windows の描画）".into(),
            OsType::Macos => "Metal".into(),
            OsType::LinuxWindow(_) => "OpenGL（X11、EGL）".into(),
            OsType::Web(_) => "WebGL2".into(),
            _ => format!("{:?}", cx.os_type()),
        };
        self.st = Some(st);
        self.bench = args.bench.clone().map(Bench::new);
        self.layouts = load_saved();
        self.layout_name = args.layout.clone().unwrap_or("標準".into());
        let conf = self
            .layouts
            .get(&self.layout_name)
            .cloned()
            .unwrap_or_else(|| preset(&self.layout_name));
        self.apply_layout(cx, &conf);
        self.set_texts(cx);
        if args.detach_list {
            self.detach_kind(cx, Kind::List);
        }
        if let Some(secs) = args.exit_after {
            self.exit_timer = cx.start_timeout(secs);
        }
        self.next_frame = cx.new_next_frame();
        self.started = true;
    }

    fn frame(&mut self, cx: &mut Cx, time: f64) {
        let t_frame = Instant::now();
        let dt_ms = self.last_frame.map(|l| ((time - l) * 1e3) as f32).unwrap_or(16.7);
        self.last_frame = Some(time);
        let mut redraw = false;
        if !self.sized {
            // (the window exists only after the first frames: resized then)
            self.sized = true;
            let size = self.st.as_ref().unwrap().args.size;
            if size != (1600.0, 960.0) {
                self.ui.window(id!(main_window)).resize(cx, dvec2(size.0, size.1));
            }
        }
        {
            let st = self.st.as_mut().unwrap();
            if st.poll() {
                redraw = true;
            }
            let cpu = thread_cpu_ms();
            if let Some(c0) = self.last_cpu {
                st.perf.acc[P_CPU] += cpu - c0;
            }
            self.last_cpu = Some(cpu);
            let anim = st.animating();
            st.flings((dt_ms as f64 / 1e3).min(0.1));
            if anim {
                redraw = true;
            }
        }
        // the view asked for on the command line, once the first wave knows its width
        if !self.zoom_done
            && let Some((z, at)) = self.st.as_ref().unwrap().args.zoom
        {
            let st = self.st.as_mut().unwrap();
            let n = st.n_ticks();
            if let Some(id) = st.wave_ids().first().copied()
                && let Some(w) = st.wave_mut(id)
                && let Some(v) = w.vp
            {
                let fit = Viewport::fit(n, v.width);
                let mut vp = fit.zoomed(z, 0.0);
                vp.t0 = at * n as f64 - vp.tpp * v.width as f64 / 2.0;
                w.vp = Some(vp.clamped(n));
                self.zoom_done = true;
                redraw = true;
            }
        }
        if let Some(mut b) = self.bench.take() {
            if !b.done() {
                let st = self.st.as_mut().unwrap();
                let fid = st.wave_ids().first().copied();
                let width = fid
                    .and_then(|id| match st.panels.get(&id) {
                        Some(Panel::Wave(w)) => w.vp.map(|v| v.width),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                let (vp, ended) = bench::step(st, &mut b, dt_ms, width);
                if let (Some(vp), Some(id)) = (vp, fid)
                    && let Some(w) = st.wave_mut(id)
                {
                    w.vp = Some(vp);
                    w.vel = 0.0;
                }
                redraw = true;
                if ended {
                    st.continuous = false;
                    cx.quit();
                }
            }
            self.bench = Some(b);
        }
        if time - self.status_at > 0.25 {
            self.status_at = time;
            self.update_status(cx);
        }
        if redraw {
            cx.redraw_all();
        }
        let st = self.st.as_mut().unwrap();
        st.perf.acc[P_FRAME] += t_frame.elapsed().as_secs_f64() * 1e3;
        // keep the frames coming while something moves (and a few seconds after the start, for the status line)
        if st.animating() || st.started.elapsed().as_secs_f64() < 3.0 {
            self.next_frame = cx.new_next_frame();
        } else {
            self.last_frame = None;
        }
    }
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.st.is_none() {
            return;
        }
        let dock = self.ui.dock(id!(dock));
        for a in actions.iter() {
            if let PanelAction::Detach(id) = a.cast() {
                self.detach(cx, id);
            }
            match a.as_widget_action().cast::<DockAction>() {
                DockAction::TabCloseWasPressed(tab) => {
                    dock.close_tab(cx, tab);
                    self.fix_root(cx);
                    self.st().panels.remove(&tab);
                    cx.redraw_all();
                }
                DockAction::ShouldTabStartDrag(tab) => {
                    if matches!(cx.os_type(), OsType::LinuxWindow(_)) {
                        // Makepad 1.0 has no drag and drop on X11: the tab goes where the mouse is let go
                        self.linux_drag = Some(tab);
                    } else {
                        dock.tab_start_drag(
                            cx,
                            tab,
                            DragItem::FilePath {
                                path: String::new(),
                                internal_id: Some(tab),
                            },
                        );
                    }
                }
                DockAction::Drag(dh) => {
                    if dh.items.len() == 1 {
                        dock.accept_drag(cx, dh, DragResponse::Move);
                    }
                }
                DockAction::Drop(dh) => {
                    if let Some(DragItem::FilePath {
                        internal_id: Some(id), ..
                    }) = dh.items.first()
                    {
                        dock.drop_move(cx, dh.abs, *id);
                        self.fix_root(cx);
                    }
                }
                DockAction::TabWasPressed(tab) => {
                    if self
                        .st
                        .as_ref()
                        .unwrap()
                        .panels
                        .get(&tab)
                        .is_some_and(|p| p.kind() == Kind::Wave)
                    {
                        self.st().focus_wave = Some(tab);
                    }
                }
                _ => {}
            }
        }
        if let Some(i) = self.ui.drop_down(id!(add)).changed(actions) {
            self.add_panel(cx, Kind::ALL[i]);
        }
        if let Some(i) = self.ui.drop_down(id!(detach)).changed(actions) {
            self.detach_kind(cx, Kind::ALL[i]);
        }
        if let Some(name) = self.ui.drop_down(id!(layout)).changed_label(actions) {
            let conf = self.layouts.get(&name).cloned().unwrap_or_else(|| preset(&name));
            self.layout_name = name;
            self.apply_layout(cx, &conf);
        }
        let name_in = self.ui.text_input(id!(layout_name));
        if let Some(s) = name_in.changed(actions) {
            self.layout_name = s;
        }
        if self.ui.button(id!(save)).clicked(actions) || name_in.returned(actions).is_some() {
            let name = self.layout_name.trim().to_string();
            if !name.is_empty()
                && let Some(conf) = self.conf_of(cx)
            {
                self.layouts.insert(name, conf);
                save_all(&self.layouts);
                self.set_texts(cx);
            }
        }
        if self.ui.button(id!(theme)).clicked(actions) {
            let dark = !self.st.as_ref().unwrap().pal.dark;
            self.set_theme(cx, dark);
        }
    }
}

impl AppMain for App {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        let t = Instant::now();
        if let Event::Startup = event {
            self.startup(cx);
        }
        if !self.started {
            self.ui.handle_event(cx, event, &mut Scope::empty());
            return;
        }
        if self.exit_timer.is_event(event).is_some() {
            cx.quit();
        }
        if let Some(ne) = self.next_frame.is_event(event) {
            self.frame(cx, ne.time);
        }
        if let Event::MouseUp(e) = event
            && let Some(tab) = self.linux_drag.take()
        {
            self.ui.dock(id!(dock)).drop_move(cx, e.abs, tab);
            self.fix_root(cx);
        }
        if let Event::LiveEdit = event {
            // the DSL reloaded (the theme): our texts again, and the panels' (ui_gen)
            self.st().ui_gen += 1;
            self.set_texts(cx);
            cx.redraw_all();
        }
        if let Event::WindowGeomChange(g) = event {
            self.st().win_size = g.new_geom.inner_size;
            self.st().dpi = g.new_geom.dpi_factor;
        }
        self.match_event(cx, event);
        // anything that changes the state asks for frames again (the flings, the analysis coming)
        if matches!(
            event,
            Event::Actions(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::Scroll(_) | Event::KeyDown(_)
        ) && self.last_frame.is_none()
        {
            self.next_frame = cx.new_next_frame();
        }
        let st = self.st.as_mut().unwrap();
        let mut scope = Scope::with_data(st);
        self.ui.handle_event(cx, event, &mut scope);
        // the panels in windows of their own: their tab id first on the path (the panel finds its state by it)
        let mut closed = vec![];
        for (id, w) in &self.windows {
            let st = self.st.as_mut().unwrap();
            let mut scope = Scope::with_data(st);
            let actions = cx.capture_actions(|cx| {
                scope.with_id(*id, |scope| w.handle_event(cx, event, scope));
            });
            for a in actions.iter() {
                if let WindowAction::WindowClosed = a.as_widget_action().cast() {
                    closed.push(*id);
                }
                if let PanelAction::Detach(_) = a.cast() {}
            }
        }
        for id in closed {
            self.windows.retain(|w| w.0 != id);
            self.redock(cx, id);
        }
        let st = self.st.as_mut().unwrap();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        if let Event::Draw(_) = event {
            st.perf.acc[P_DRAW] += ms;
            // the frame rate as drawn frames (the frame loop may run without drawing)
            let now = Instant::now();
            if let Some(l) = self.last_draw {
                let el = st.started.elapsed().as_secs_f64();
                st.perf.frame(el, now.duration_since(l).as_secs_f32() * 1e3);
            }
            self.last_draw = Some(now);
        }
        st.perf.acc[P_EVENTS] += ms;
    }
}
