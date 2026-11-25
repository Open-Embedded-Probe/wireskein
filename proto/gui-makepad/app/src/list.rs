//! The results list, apart from the waveform: one claim's one layer as rows in Makepad's PortalList (virtual: it asks
//! for the rows in view only, its own scrolling with a fling and a scroll bar). A row is a small widget of our own
//! (`ListRow`) that draws its cells with DrawText from what the list hands it (`scope.props`); the columns are chosen
//! with check boxes, the claim and the layer with drop-downs, following the waveform's choice or not.

use crate::state::*;
use makepad_widgets::*;
use view_core::ann::Status;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    pub ListRowBase = {{ListRow}} {}
    pub ListRow = <ListRowBase> {
        width: Fill, height: 20
        draw_text: { text_style: <THEME_FONT_REGULAR> { font_size: 8.5 } }
        draw_mono: { text_style: <THEME_FONT_CODE> { font_size: 8.0 } }
    }

    pub ListPanelBase = {{ListPanel}} {}
    pub ListPanel = <ListPanelBase> {
        width: Fill, height: Fill, flow: Down
        show_bg: true
        draw_bg: { color: (THEME_COLOR_BG_CONTAINER) }
        head = <View> {
            width: Fill, height: Fit, flow: Right, spacing: 6,
            padding: { left: 6, right: 6, top: 4, bottom: 2 }, align: { y: 0.5 }
            claim_dd = <DropDown> { width: 240 }
            layer_dd = <DropDown> { width: 220 }
            follow = <CheckBox> { text: "波形と連動" }
            <View> { width: Fill, height: 1 }
            count = <Label> { text: "" draw_text: { color: (THEME_COLOR_TEXT_META) } }
        }
        cols = <View> {
            width: Fill, height: Fit, flow: RightWrap, spacing: 2,
            padding: { left: 6, right: 6, top: 0, bottom: 2 }, align: { y: 0.5 }
            <Label> { text: "列:" draw_text: { color: (THEME_COLOR_TEXT_META) } }
            v0 = <View> { width: Fit, height: Fit, visible: false, c0 = <CheckBox> {} }
            v1 = <View> { width: Fit, height: Fit, visible: false, c1 = <CheckBox> {} }
            v2 = <View> { width: Fit, height: Fit, visible: false, c2 = <CheckBox> {} }
            v3 = <View> { width: Fit, height: Fit, visible: false, c3 = <CheckBox> {} }
            v4 = <View> { width: Fit, height: Fit, visible: false, c4 = <CheckBox> {} }
            v5 = <View> { width: Fit, height: Fit, visible: false, c5 = <CheckBox> {} }
            v6 = <View> { width: Fit, height: Fit, visible: false, c6 = <CheckBox> {} }
            v7 = <View> { width: Fit, height: Fit, visible: false, c7 = <CheckBox> {} }
            v8 = <View> { width: Fit, height: Fit, visible: false, c8 = <CheckBox> {} }
            v9 = <View> { width: Fit, height: Fit, visible: false, c9 = <CheckBox> {} }
        }
        header = <ListRow> { header: true }
        list = <PortalList> {
            width: Fill, height: Fill
            Row = <ListRow> {}
        }
        msg = <Label> { margin: 12, text: "" }
    }
}

const NCOLS: usize = 10;
const VIEWS: [&[LiveId]; NCOLS] = [
    id!(v0),
    id!(v1),
    id!(v2),
    id!(v3),
    id!(v4),
    id!(v5),
    id!(v6),
    id!(v7),
    id!(v8),
    id!(v9),
];
const CHECKS: [&[LiveId]; NCOLS] = [
    id!(c0),
    id!(c1),
    id!(c2),
    id!(c3),
    id!(c4),
    id!(c5),
    id!(c6),
    id!(c7),
    id!(c8),
    id!(c9),
];

/// What a row draws: cells (text, width px or 0 for the rest, color, monospace), its background.
#[derive(Default)]
pub struct RowInfo {
    pub idx: usize,
    pub cells: Vec<(String, f64, Vec4, bool)>,
    pub bg: Option<Vec4>,
}

#[derive(Clone, Debug, DefaultNone)]
pub enum RowAction {
    Clicked(usize, bool),
    None,
}

#[derive(Live, LiveHook, Widget)]
pub struct ListRow {
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
    header: bool,
    #[rust]
    idx: usize,
    #[rust]
    last_click: Option<(f64, usize)>,
}

impl ListRow {
    fn draw_cells(&mut self, cx: &mut Cx2d, rect: Rect, info: &RowInfo) {
        if let Some(bg) = info.bg {
            self.draw_bg.color = bg;
            self.draw_bg.draw_abs(cx, rect);
        } else {
            // an invisible background: the area to hit
            self.draw_bg.color = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_bg.draw_abs(cx, rect);
        }
        cx.begin_turtle(Walk::abs_rect(rect), Layout::default());
        let mut x = rect.pos.x + 8.0;
        for (s, w, color, mono) in &info.cells {
            // (Makepad's code font has no Japanese fallback: such cells in the regular font)
            let dt = if *mono && s.is_ascii() {
                &mut self.draw_mono
            } else {
                &mut self.draw_text
            };
            dt.color = *color;
            let lt = dt.layout(cx, 0.0, 0.0, None, false, Align::default(), s);
            let h = lt.size_in_lpxs.height as f64;
            dt.draw_abs(cx, dvec2(x, rect.pos.y + (rect.size.y - h) / 2.0), s);
            if *w <= 0.0 {
                break;
            }
            x += w + 6.0;
        }
        cx.end_turtle();
    }
}

/// The header of a list panel's layer (the row's columns, named).
fn header_info(st: &St, id: LiveId) -> Option<RowInfo> {
    let Some(Panel::List(lv)) = st.panels.get(&id) else {
        return None;
    };
    let a = st.analysis.as_ref()?;
    let c = a.claims.get(lv.claim.min(a.claims.len().saturating_sub(1)))?;
    let layer = c.layers.get(lv.layer.min(c.layers.len().saturating_sub(1)))?;
    let dim = st.pal.dim();
    let mut cells = vec![
        ("時刻".to_string(), 120.0, dim, false),
        (String::new(), 16.0, dim, false),
    ];
    for col in layer.columns.iter().filter(|c| !lv.hidden.contains(c)) {
        cells.push((col.clone(), 110.0, dim, false));
    }
    cells.push(("中身".into(), 0.0, dim, false));
    Some(RowInfo {
        idx: 0,
        cells,
        bg: Some(st.pal.c[crate::theme::LANE_ALT]),
    })
}

impl Widget for ListRow {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        if self.header {
            if let Some(st) = scope.data.get::<St>()
                && let Some(id) = st.panel_of(&scope.path.data)
                && let Some(info) = header_info(st, id)
            {
                self.draw_cells(cx, rect, &info);
            }
            return DrawStep::done();
        }
        if let Some(info) = scope.props.get::<RowInfo>() {
            self.idx = info.idx;
            self.draw_cells(cx, rect, info);
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.header {
            return;
        }
        if let Hit::FingerUp(e) = event.hits(cx, self.draw_bg.area())
            && e.is_over
            && e.was_tap()
        {
            let double = self.last_click.is_some_and(|(t, i)| i == self.idx && e.time - t < 0.4);
            self.last_click = Some((e.time, self.idx));
            cx.widget_action(self.widget_uid(), &scope.path, RowAction::Clicked(self.idx, double));
        }
    }
}

#[derive(Live, LiveHook, Widget)]
pub struct ListPanel {
    #[deref]
    view: View,
    #[rust]
    shown_cols: Vec<String>,
    #[rust]
    labels_key: (usize, usize, usize, u64),
}

impl Widget for ListPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.view.handle_event(cx, event, scope));
        if actions.is_empty() {
            return;
        }
        let Some(st) = scope.data.get_mut::<St>() else { return };
        let Some(id) = st.panel_of(&scope.path.data) else {
            return;
        };
        let mut changed = false;
        if let Some(i) = self.view.drop_down(id!(claim_dd)).selected(&actions)
            && let Some(l) = st.list_mut(id)
        {
            l.claim = i;
            l.layer = 0;
            l.scroll_to = Some(0);
            changed = true;
        }
        if let Some(i) = self.view.drop_down(id!(layer_dd)).selected(&actions)
            && let Some(l) = st.list_mut(id)
        {
            l.layer = i;
            l.scroll_to = Some(0);
            changed = true;
        }
        if let Some(on) = self.view.check_box(id!(follow)).changed(&actions)
            && let Some(l) = st.list_mut(id)
        {
            l.follow = on;
        }
        for (k, path) in CHECKS.iter().enumerate() {
            if let Some(on) = self.view.check_box(path).changed(&actions)
                && let Some(col) = self.shown_cols.get(k).cloned()
                && let Some(l) = st.list_mut(id)
            {
                l.hidden.retain(|x| *x != col);
                if !on {
                    l.hidden.push(col);
                }
                changed = true;
            }
        }
        for a in actions.iter() {
            if let RowAction::Clicked(row, double) = a.as_widget_action().cast() {
                let (claim, layer) = match st.panels.get(&id) {
                    Some(Panel::List(l)) => (l.claim, l.layer),
                    _ => continue,
                };
                st.select(Sel { claim, layer, row }, Some(id));
                if double && let Some(w) = st.focus_wave {
                    st.zoom_to_sel(w);
                }
                changed = true;
            }
        }
        if changed {
            cx.redraw_all();
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let t = std::time::Instant::now();
        let ok = self.prepare(cx, scope);
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if !ok {
                continue;
            }
            if let Some(mut list) = item.as_portal_list().borrow_mut() {
                let Some(st) = scope.data.get_mut::<St>() else { continue };
                let Some(id) = st.panel_of(&scope.path.data) else {
                    continue;
                };
                Self::rows(cx, st, id, &mut list);
            }
        }
        if let Some(st) = scope.data.get_mut::<St>() {
            st.perf.acc[P_LIST] += t.elapsed().as_secs_f64() * 1e3;
        }
        DrawStep::done()
    }
}

impl ListPanel {
    /// The head (drop-downs, columns, the count) from the state; false: nothing to list (a message instead).
    fn prepare(&mut self, cx: &mut Cx2d, scope: &mut Scope) -> bool {
        let Some(st) = scope.data.get_mut::<St>() else {
            return false;
        };
        let Some(id) = st.panel_of(&scope.path.data) else {
            return false;
        };
        let msg = self.view.label(id!(msg));
        let Some(a) = &st.analysis else {
            msg.set_text(
                cx,
                if st.analysis_slot.is_some() {
                    "解析中…（終わると一覧が出ます）"
                } else {
                    "解析の結果はまだありません"
                },
            );
            self.view.view(id!(head)).set_visible(cx, false);
            self.view.view(id!(cols)).set_visible(cx, false);
            self.view.widget(id!(list)).set_visible(cx, false);
            return false;
        };
        if a.claims.is_empty() {
            msg.set_text(cx, &a.error.clone().unwrap_or("主張なし（読めたバスがない）".into()));
            return false;
        }
        msg.set_text(cx, "");
        self.view.view(id!(head)).set_visible(cx, true);
        self.view.view(id!(cols)).set_visible(cx, true);
        self.view.widget(id!(list)).set_visible(cx, true);
        let Some(Panel::List(lv)) = st.panels.get(&id) else {
            return false;
        };
        let ci = lv.claim.min(a.claims.len() - 1);
        let c = &a.claims[ci];
        let li = lv.layer.min(c.layers.len().saturating_sub(1));
        let Some(layer) = c.layers.get(li) else { return false };
        let key = (ci, li, a.claims.len(), st.ui_gen);
        if key != self.labels_key {
            self.labels_key = key;
            let cdd = self.view.drop_down(id!(claim_dd));
            cdd.set_labels(
                cx,
                a.claims
                    .iter()
                    .enumerate()
                    .map(|(i, c)| format!("#{i} {} ({})", c.protocol, c.verdict))
                    .collect(),
            );
            cdd.set_selected_item(cx, ci);
            let ldd = self.view.drop_down(id!(layer_dd));
            ldd.set_labels(
                cx,
                c.layers.iter().map(|l| format!("{} ({})", l.name, l.len())).collect(),
            );
            ldd.set_selected_item(cx, li);
        }
        self.view.check_box(id!(follow)).set_active(cx, lv.follow);
        self.view.label(id!(count)).set_text(cx, &format!("{} 行", layer.len()));
        self.shown_cols = layer.columns.iter().take(NCOLS).cloned().collect();
        for (k, path) in CHECKS.iter().enumerate() {
            let cb = self.view.check_box(path);
            let v = self.view.view(VIEWS[k]);
            match self.shown_cols.get(k) {
                Some(col) => {
                    v.set_visible(cx, true);
                    cb.set_text(col);
                    cb.set_active(cx, !lv.hidden.contains(col));
                }
                None => v.set_visible(cx, false),
            }
        }
        // a row asked for (the waveform's choice, the search): brought into view once
        let to = match st.panels.get_mut(&id) {
            Some(Panel::List(l)) => l.scroll_to.take().map(|r| (r, l.vis)),
            _ => None,
        };
        if let Some((r, vis)) = to {
            let pl = self.view.portal_list(id!(list));
            let first = pl.first_id();
            if r < first || r >= first + vis.saturating_sub(1) {
                pl.set_first_id_and_scroll(r.saturating_sub(vis / 3), 0.0);
            }
        }
        true
    }

    fn rows(cx: &mut Cx2d, st: &mut St, id: LiveId, list: &mut PortalList) {
        let pal = st.pal;
        let Some(a) = &st.analysis else { return };
        let Some(Panel::List(lv)) = st.panels.get(&id) else {
            return;
        };
        let ci = lv.claim.min(a.claims.len().saturating_sub(1));
        let Some(c) = a.claims.get(ci) else { return };
        let li = lv.layer.min(c.layers.len().saturating_sub(1));
        let Some(layer) = c.layers.get(li) else { return };
        let shown: Vec<&String> = layer.columns.iter().filter(|c| !lv.hidden.contains(c)).collect();
        let tick_hz = st.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
        let sel = st.sel.filter(|s| s.claim == ci && s.layer == li).map(|s| s.row);
        let hits = st.search.by_layer.get(&(ci, li));
        list.set_item_range(cx, 0, layer.len());
        let mut n_vis = 0;
        while let Some(i) = list.next_visible_item(cx) {
            if i >= layer.len() {
                continue;
            }
            n_vis += 1;
            let it = layer.track.item(i);
            let v = layer.value(i);
            let hit = hits.is_some_and(|h| h.binary_search(&i).is_ok());
            let color = if hit {
                pal.accent()
            } else if it.status >= Status::Note && it.status != Status::Unread {
                pal.status_text(it.status)
            } else {
                pal.text()
            };
            let mark = match it.status {
                Status::Ok => "",
                Status::Note => "●",
                Status::Warn => "▲",
                Status::Error => "✖",
                Status::Unread => "?",
            };
            let mut cells = vec![
                (fmt_time(it.s as f64 / tick_hz), 120.0, pal.dim(), true),
                (mark.to_string(), 16.0, pal.status_text(it.status), false),
            ];
            for col in &shown {
                cells.push((cell(v.as_ref(), col), 110.0, color, true));
            }
            cells.push((layer.track.text(i).replace('\n', " "), 0.0, color, false));
            let info = RowInfo {
                idx: i,
                cells,
                bg: if sel == Some(i) {
                    Some(crate::theme::with_alpha(pal.c[crate::theme::OK], 0.9))
                } else if i % 2 == 1 {
                    Some(pal.c[crate::theme::LANE_ALT])
                } else {
                    None
                },
            };
            let item = list.item(cx, i, live_id!(Row));
            let mut s = Scope::with_props(&info);
            item.draw_all(cx, &mut s);
        }
        if let Some(Panel::List(l)) = st.panels.get_mut(&id) {
            l.vis = n_vis.max(1);
        }
    }
}
