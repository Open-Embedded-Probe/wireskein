//! The small panels: the search (Makepad's TextInput, the hits in a PortalList of ListRow), and the measure, the
//! claims and the speed as lines of text drawn by a widget of our own (`InfoText`), with a few buttons.

use crate::list::{RowAction, RowInfo};
use crate::state::*;
use crate::theme::*;
use makepad_widgets::*;
use view_core::ann::Status;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;
    use crate::list::ListRow;

    pub InfoTextBase = {{InfoText}} {}
    pub InfoText = <InfoTextBase> {
        width: Fill, height: Fill
        draw_text: { text_style: <THEME_FONT_REGULAR> { font_size: 9.0 } }
        draw_mono: { text_style: <THEME_FONT_CODE> { font_size: 8.5 } }
    }

    pub SearchPanelBase = {{SearchPanel}} {}
    pub SearchPanel = <SearchPanelBase> {
        width: Fill, height: Fill, flow: Down
        show_bg: true
        draw_bg: { color: (THEME_COLOR_BG_CONTAINER) }
        <View> {
            width: Fill, height: Fit, flow: Right, spacing: 6, padding: 6, align: { y: 0.5 }
            <Label> { text: "検索" }
            query = <TextInput> { width: Fill, empty_text: "例: 0x48 書き込み / addr=0x50 nack / エラー" }
            prev = <Button> { text: "◀ 前" }
            next = <Button> { text: "次 ▶" }
        }
        info = <Label> { margin: { left: 8, bottom: 4 }, text: "" draw_text: { color: (THEME_COLOR_TEXT_META) } }
        list = <PortalList> { width: Fill, height: Fill, Row = <ListRow> {} }
    }

    pub InfoPanelBase = {{InfoPanel}} {}
    pub MeasurePanel = <InfoPanelBase> {
        width: Fill, height: Fill, flow: Down
        show_bg: true
        draw_bg: { color: (THEME_COLOR_BG_CONTAINER) }
        lines = <InfoText> { kind: 0 }
        <View> { width: Fill, height: Fit, padding: 6
            clear = <Button> { text: "マーカーを消す" }
        }
    }
    pub ClaimsPanel = <InfoPanelBase> {
        width: Fill, height: Fill, flow: Down
        show_bg: true
        draw_bg: { color: (THEME_COLOR_BG_CONTAINER) }
        lines = <InfoText> { kind: 1 }
    }
    pub PerfPanel = <InfoPanelBase> {
        width: Fill, height: Fill, flow: Down
        show_bg: true
        draw_bg: { color: (THEME_COLOR_BG_CONTAINER) }
        lines = <InfoText> { kind: 2 }
        <View> { width: Fill, height: Fit, padding: 6, spacing: 12, flow: Right
            tiles = <CheckBox> { text: "タイル（256 px）" }
            cont = <CheckBox> { text: "描き続ける" }
        }
    }
}

/// Lines of text: (text, color, monospace).
type Lines = Vec<(String, Vec4, bool)>;

fn measure_lines(st: &St) -> Lines {
    let pal = &st.pal;
    let hz = st.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let mut v: Lines = vec![("カーソル".into(), pal.dim(), false)];
    match (&st.measure, &st.cap) {
        (Some(m), Some(cap)) => {
            v.push((cap.lines[m.line].name.clone(), pal.text(), false));
            for l in m.text(hz).lines() {
                v.push((l.to_string(), pal.text(), true));
            }
        }
        _ => v.push((
            "デジタルの線にマウスを載せると、パルスの幅・周期・周波数・デューティ".into(),
            pal.dim(),
            false,
        )),
    }
    v.push((String::new(), pal.dim(), false));
    v.push(("マーカー（右クリック / 1・2 キー）".into(), pal.dim(), false));
    for (k, m) in st.markers.iter().enumerate() {
        let s = m.map(|t| fmt_time(t / hz)).unwrap_or("–".into());
        v.push((format!("M{}  {s}", k + 1), pal.c[MARKER1 + k], true));
    }
    if let [Some(a), Some(b)] = st.markers {
        let d = (b - a).abs() / hz;
        v.push((
            format!(
                "Δ {}  ·  1/Δ {}",
                fmt_time(d),
                if d > 0.0 { fmt_freq(1.0 / d) } else { "–".into() }
            ),
            pal.text(),
            true,
        ));
        if let Some(cap) = &st.cap {
            let (t0, t1) = (a.min(b) as i64, a.max(b) as i64);
            let mut e = vec![];
            for l in &cap.lines {
                e.clear();
                let all = l.src.edges_in(t0, t1, 100_000, &mut e);
                v.push((
                    format!("{:<8} {}{} の変化", l.name, e.len(), if all { "" } else { "+" }),
                    pal.dim(),
                    true,
                ));
            }
        }
    }
    v
}

fn claims_lines(st: &St) -> Lines {
    let pal = &st.pal;
    let mut v: Lines = vec![];
    match (&st.analysis, st.analysis_slot.is_some()) {
        (_, true) => v.push(("解析中…".into(), pal.accent(), false)),
        (None, false) => v.push(("解析の結果はまだありません".into(), pal.dim(), false)),
        (Some(a), false) => {
            if let Some(e) = &a.error {
                v.push((format!("解析の誤り: {e}"), pal.status_text(Status::Error), false));
            }
            v.push((
                format!(
                    "主張 {} 件 · 解析 {:.2} s{}",
                    a.claims.len(),
                    a.seconds,
                    if a.fake { "（作り物）" } else { "" }
                ),
                pal.dim(),
                false,
            ));
            for (ci, c) in a.claims.iter().enumerate() {
                v.push((String::new(), pal.dim(), false));
                v.push((
                    format!("#{ci} {}    {} {:.2}", c.protocol, c.verdict, c.score),
                    pal.verdict(&c.verdict),
                    false,
                ));
                v.push((
                    c.roles
                        .iter()
                        .map(|(r, n)| format!("{r}={n}"))
                        .collect::<Vec<_>>()
                        .join("  "),
                    pal.text(),
                    true,
                ));
                v.push((
                    c.layers
                        .iter()
                        .map(|l| format!("{} {}", l.name, l.len()))
                        .collect::<Vec<_>>()
                        .join(" · "),
                    pal.dim(),
                    false,
                ));
                let bad = c
                    .layers
                    .first()
                    .map(|l| {
                        l.track
                            .items()
                            .iter()
                            .filter(|i| i.status >= Status::Warn && i.status != Status::Unread)
                            .count()
                    })
                    .unwrap_or(0);
                v.push((
                    format!(
                        "誤り・疑わしい {bad}{}",
                        c.bus.as_ref().map(|b| format!(" · {b}")).unwrap_or_default()
                    ),
                    pal.dim(),
                    false,
                ));
            }
        }
    }
    v
}

fn perf_lines(st: &St) -> Lines {
    let pal = &st.pal;
    let p = &st.perf;
    let (n, avg, worst) = p.fps();
    let mut v: Lines = vec![
        (
            format!("fps（直近 1 秒）{n:.0}  ·  平均 {avg:.1} ms  ·  最悪 {worst:.1} ms"),
            pal.text(),
            true,
        ),
        (
            format!(
                "波形を作る（scene）{:.2} ms  ·  図形 {}  ·  インスタンス {}  ·  文字 {}",
                p.wave_avg(),
                p.shapes,
                p.instances,
                p.labels
            ),
            pal.text(),
            true,
        ),
    ];
    if let Some(cap) = &st.cap {
        v.push((
            format!(
                "記録: {} 本 · {:.2e} の変化 · {} tick · 読む {:.0} ms",
                cap.lines.len(),
                cap.total_edges as f64,
                cap.n_ticks,
                cap.read_s * 1e3
            ),
            pal.text(),
            true,
        ));
        v.push((
            format!(
                "最初の絵 {}  ·  要約 {}",
                p.first_paint
                    .map(|s| format!("{:.0} ms", s * 1e3))
                    .unwrap_or("–".into()),
                cap.lod_s
                    .lock()
                    .unwrap()
                    .map(|s| format!("{s:.2} s"))
                    .unwrap_or("作っている".into())
            ),
            pal.text(),
            true,
        ));
        v.push((
            format!(
                "モデル {:.0} MB  ·  RSS {}",
                cap.mem_bytes() as f64 / 1e6,
                rss_mb().map(|m| format!("{m:.0} MB")).unwrap_or("–".into())
            ),
            pal.text(),
            true,
        ));
    }
    v.push((format!("描画: {}", st.gpu), pal.text(), true));
    v.push((
        "Makepad 1.0（Linux は OpenGL、Windows は D3D11）。波形は自前のシェーダ DrawSeg のインスタンス（1 回の draw）、文字は DrawText"
            .into(),
        pal.dim(),
        false,
    ));
    v
}

#[derive(Live, LiveHook, Widget)]
pub struct InfoText {
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_mono: DrawText,
    #[live]
    kind: f64,
}

impl Widget for InfoText {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let Some(st) = scope.data.get::<St>() else {
            return DrawStep::done();
        };
        self.draw_bg.color = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        let lines = match self.kind as i32 {
            0 => measure_lines(st),
            1 => claims_lines(st),
            _ => perf_lines(st),
        };
        cx.begin_turtle(Walk::abs_rect(rect), Layout::default());
        let mut y = rect.pos.y + 8.0;
        for (s, color, mono) in &lines {
            // (Makepad's code font has no Japanese fallback: such lines in the regular font)
            let dt = if *mono && s.is_ascii() {
                &mut self.draw_mono
            } else {
                &mut self.draw_text
            };
            dt.color = *color;
            // long lines wrapped at the panel's width
            let lt = dt.layout(
                cx,
                0.0,
                0.0,
                Some((rect.size.x - 20.0).max(40.0) as f32),
                true,
                Align::default(),
                s,
            );
            let h = (lt.size_in_lpxs.height as f64).max(14.0);
            dt.draw_walk_laidout(
                cx,
                Walk {
                    abs_pos: Some(dvec2(rect.pos.x + 10.0, y)),
                    ..Walk::default()
                },
                &lt,
            );
            y += h + 3.0;
            if y > rect.pos.y + rect.size.y {
                break;
            }
        }
        cx.end_turtle();
        DrawStep::done()
    }
    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}
}

#[derive(Live, LiveHook, Widget)]
pub struct InfoPanel {
    #[deref]
    view: View,
}

impl Widget for InfoPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if actions.is_empty() {
            return;
        }
        let Some(st) = scope.data.get_mut::<St>() else { return };
        if self.view.button(id!(clear)).clicked(&actions) {
            st.markers = [None, None];
            st.next_marker = 0;
            cx.redraw_all();
        }
        if let Some(on) = self.view.check_box(id!(tiles)).changed(&actions) {
            st.tiles_on = on;
            cx.redraw_all();
        }
        if let Some(on) = self.view.check_box(id!(cont)).changed(&actions) {
            st.continuous = on;
            cx.redraw_all();
        }
    }
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(st) = scope.data.get::<St>() {
            self.view.check_box(id!(tiles)).set_active(cx, st.tiles_on);
            self.view.check_box(id!(cont)).set_active(cx, st.continuous);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[derive(Live, LiveHook, Widget)]
pub struct SearchPanel {
    #[deref]
    view: View,
    #[rust]
    shown_query: Option<String>,
}

impl Widget for SearchPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if actions.is_empty() {
            return;
        }
        let Some(st) = scope.data.get_mut::<St>() else { return };
        let id = st.panel_of(&scope.path.data);
        let mut changed = false;
        let q = self.view.text_input(id!(query));
        if let Some(text) = q.changed(&actions) {
            st.search.query = text;
            if st.search.query != st.search.last {
                st.run_search();
            }
            self.shown_query = Some(st.search.query.clone());
            changed = true;
        }
        if q.returned(&actions).is_some() || self.view.button(id!(next)).clicked(&actions) {
            st.search_step(true);
            changed = true;
        }
        if self.view.button(id!(prev)).clicked(&actions) {
            st.search_step(false);
            changed = true;
        }
        for a in actions.iter() {
            if let RowAction::Clicked(i, double) = a.as_widget_action().cast() {
                st.search.cur = i;
                if let Some(&s) = st.search.hits.get(i) {
                    st.select(s, id);
                    if double && let Some(w) = st.focus_wave {
                        st.zoom_to_sel(w);
                    }
                }
                changed = true;
            }
        }
        if changed {
            cx.redraw_all();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(st) = scope.data.get_mut::<St>() {
            // the query set from elsewhere (the command line) shown once
            if self.shown_query.as_deref() != Some(st.search.query.as_str()) {
                self.view.text_input(id!(query)).set_text(cx, &st.search.query);
                self.shown_query = Some(st.search.query.clone());
            }
            let s = &st.search;
            let info = if s.query.trim().is_empty() {
                "言葉はすべて合うもの。0x48 書き込み · addr=0x50 nack · エラー · 疑わしい · ほかは項目の文字から"
                    .to_string()
            } else {
                format!(
                    "{} 件{} · {:.0} ms",
                    s.hits.len(),
                    if s.hits.is_empty() {
                        String::new()
                    } else {
                        format!("（{} 件目）", s.cur + 1)
                    },
                    s.took_ms
                )
            };
            self.view.label(id!(info)).set_text(cx, &info);
            if let Some(r) = st.search.scroll_to.take() {
                let pl = self.view.portal_list(id!(list));
                let first = pl.first_id();
                if r < first || r >= first + 8 {
                    pl.set_first_id_and_scroll(r.saturating_sub(3), 0.0);
                }
            }
        }
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let Some(st) = scope.data.get::<St>() else { continue };
                Self::rows(cx, st, &mut list);
            }
        }
        DrawStep::done()
    }
}

impl SearchPanel {
    fn rows(cx: &mut Cx2d, st: &St, list: &mut PortalList) {
        let pal = st.pal;
        let s = &st.search;
        let Some(a) = &st.analysis else { return };
        let hz = st.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
        list.set_item_range(cx, 0, s.hits.len());
        while let Some(i) = list.next_visible_item(cx) {
            let Some(h) = s.hits.get(i) else { continue };
            let c = &a.claims[h.claim];
            let l = &c.layers[h.layer];
            let it = l.track.item(h.row);
            let info = RowInfo {
                idx: i,
                cells: vec![
                    (fmt_time(it.s as f64 / hz), 120.0, pal.dim(), true),
                    (format!("{} {}", c.protocol, l.name), 150.0, pal.dim(), false),
                    (
                        l.track.text(h.row).replace('\n', " "),
                        0.0,
                        if it.status >= Status::Note && it.status != Status::Unread {
                            pal.status_text(it.status)
                        } else {
                            pal.text()
                        },
                        false,
                    ),
                ],
                bg: if i == s.cur {
                    Some(with_alpha(pal.c[OK], 0.9))
                } else if i % 2 == 1 {
                    Some(pal.c[LANE_ALT])
                } else {
                    None
                },
            };
            let item = list.item(cx, i, live_id!(Row));
            item.draw_all(cx, &mut Scope::with_props(&info));
        }
    }
}
