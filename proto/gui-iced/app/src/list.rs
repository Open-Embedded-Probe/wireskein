//! The results list, apart from the waveform: one claim's one layer as rows (virtual: only the rows in view are
//! made, a slider for the place, the wheel to move), columns chosen, the claim and the layer switched, following the
//! waveform's choice or not. `vlist` is the virtual list (the search's hits use it too).

use crate::app::{App, ListView, Msg, Sel};
use crate::wave::fmt_time;
use iced::widget::{Column, button, checkbox, column, container, mouse_area, pick_list, responsive, row, rule, text, vertical_slider};
use iced::{Background, Color, Element, Font, Length, mouse};
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;
use view_core::ann::Status;

pub const ROW_H: f32 = 20.0;

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
            if s.chars().count() > 40 { format!("{}…", s.chars().take(40).collect::<String>()) } else { s }
        }
    }
}

/// A virtual list of `total` rows starting at `top`: the rows that fit are made, the wheel and the slider ask for a
/// new top (`scroll`). `vis` gets the number of rows that fit (for the followers).
#[allow(clippy::too_many_arguments)]
pub fn vlist<'a>(
    app: &'a App,
    total: usize,
    top: usize,
    sel: Option<usize>,
    make: impl Fn(usize) -> Element<'a, Msg> + 'a,
    pick: impl Fn(usize) -> Msg + 'a,
    scroll: impl Fn(usize) -> Msg + 'a,
) -> Element<'a, Msg> {
    vlist_vis(app, total, top, sel, make, pick, scroll, None)
}

#[allow(clippy::too_many_arguments)]
pub fn vlist_vis<'a>(
    app: &'a App,
    total: usize,
    top: usize,
    sel: Option<usize>,
    make: impl Fn(usize) -> Element<'a, Msg> + 'a,
    pick: impl Fn(usize) -> Msg + 'a,
    scroll: impl Fn(usize) -> Msg + 'a,
    vis_cell: Option<&'a Cell<usize>>,
) -> Element<'a, Msg> {
    let make = Rc::new(make);
    let pick = Rc::new(pick);
    let scroll = Rc::new(scroll);
    let pal = app.pal;
    responsive(move |size| {
        let vis = ((size.height / ROW_H) as usize).max(1);
        if let Some(c) = vis_cell {
            c.set(vis);
        }
        let max_top = total.saturating_sub(vis);
        let top = top.min(max_top);
        let mut col = Column::new();
        for i in top..(top + vis).min(total) {
            let selected = sel == Some(i);
            let bg = if selected {
                Color { a: 0.35, ..pal.accent() }
            } else if i % 2 == 1 {
                pal.c[crate::theme::LANE_ALT]
            } else {
                Color::TRANSPARENT
            };
            col = col.push(
                button(make(i))
                    .on_press(pick(i))
                    .padding([1, 6])
                    .height(ROW_H)
                    .width(Length::Fill)
                    .style(move |_, status| button::Style {
                        background: Some(Background::Color(if matches!(status, button::Status::Hovered) && !selected {
                            Color { a: 0.12, ..pal.text() }
                        } else {
                            bg
                        })),
                        text_color: pal.text(),
                        ..Default::default()
                    }),
            );
        }
        let s2 = scroll.clone();
        let body = mouse_area(col.width(Length::Fill).height(Length::Fill)).on_scroll(move |d| {
            let dy = match d {
                mouse::ScrollDelta::Lines { y, .. } => -y * 3.0,
                mouse::ScrollDelta::Pixels { y, .. } => -y / ROW_H,
            };
            s2(((top as f64 + dy as f64).round().max(0.0) as usize).min(max_top))
        });
        let s3 = scroll.clone();
        let bar: Element<'a, Msg> = if max_top > 0 {
            vertical_slider(0.0..=max_top as f64, (max_top - top) as f64, move |v: f64| s3(max_top - (v.round() as usize).min(max_top)))
                .step(1.0)
                .width(12)
                .height(Length::Fill)
                .into()
        } else {
            iced::widget::space().width(12).into()
        };
        row![body, bar].spacing(2).into()
    })
    .into()
}

pub fn list_view<'a>(app: &'a App, lv: &'a ListView) -> Element<'a, Msg> {
    let pal = app.pal;
    let id = lv.id;
    let Some(a) = &app.analysis else {
        let msg = if app.analysis_slot.is_some() { "解析中…（終わると一覧が出ます）" } else { "解析の結果はまだありません" };
        return container(text(msg).size(13).color(pal.dim())).center(Length::Fill).into();
    };
    if a.claims.is_empty() {
        return container(text(a.error.clone().unwrap_or("主張なし（読めたバスがない）".into())).size(13)).padding(10).into();
    }
    let ci = lv.claim.min(a.claims.len() - 1);
    let c = &a.claims[ci];
    let li = lv.layer.min(c.layers.len().saturating_sub(1));
    let Some(layer) = c.layers.get(li) else { return text("層なし").into() };
    let claims: Vec<String> = a.claims.iter().enumerate().map(|(i, c)| format!("#{i} {} ({})", c.protocol, c.verdict)).collect();
    let layers: Vec<String> = c.layers.iter().map(|l| format!("{} ({})", l.name, l.len())).collect();
    let cur_c = claims[ci].clone();
    let cur_l = layers[li].clone();
    let claims2 = claims.clone();
    let layers2 = layers.clone();
    let mut cols_row = row![text("列:").size(11).color(pal.dim())].spacing(8).align_y(iced::Alignment::Center);
    for col in &layer.columns {
        let on = !lv.hidden.contains(col);
        let name = col.clone();
        cols_row = cols_row.push(checkbox(on).label(col.clone()).size(12).text_size(11).on_toggle(move |v| Msg::ListCol(id, name.clone(), v)));
    }
    let head = column![
        row![
            pick_list(claims, Some(cur_c), move |s: String| Msg::ListClaim(id, claims2.iter().position(|x| *x == s).unwrap_or(0))).text_size(12).padding([3, 8]),
            pick_list(layers, Some(cur_l), move |s: String| Msg::ListLayer(id, layers2.iter().position(|x| *x == s).unwrap_or(0))).text_size(12).padding([3, 8]),
            checkbox(lv.follow).label("波形と連動").size(14).text_size(12).on_toggle(move |v| Msg::ListFollow(id, v)),
            iced::widget::space::horizontal(),
            text(format!("{} 行", layer.len())).size(11).color(pal.dim()),
        ]
        .spacing(8)
        .align_y(iced::Alignment::Center),
        cols_row,
    ]
    .spacing(4)
    .padding([6, 8]);
    let shown: Vec<&String> = layer.columns.iter().filter(|c| !lv.hidden.contains(c)).collect();
    let mut header = row![text("時刻").size(11).color(pal.dim()).width(120), text("").width(16)].spacing(6);
    for c in &shown {
        header = header.push(text((*c).clone()).size(11).color(pal.dim()).width(110));
    }
    header = header.push(text("中身").size(11).color(pal.dim()));
    let tick_hz = app.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let sel = app.sel.filter(|s| s.claim == ci && s.layer == li).map(|s| s.row);
    let hits = app.search.by_layer.get(&(ci, li));
    let body = vlist_vis(
        app,
        layer.len(),
        lv.top,
        sel,
        move |i| {
            let it = layer.track.item(i);
            let v = layer.value(i);
            let hit = hits.is_some_and(|h| h.binary_search(&i).is_ok());
            let color = if hit { pal.accent() } else if it.status >= Status::Note && it.status != Status::Unread { pal.status_text(it.status) } else { pal.text() };
            let mark = match it.status {
                Status::Ok => "",
                Status::Note => "●",
                Status::Warn => "▲",
                Status::Error => "✖",
                Status::Unread => "?",
            };
            let mut r = row![
                text(fmt_time(it.s as f64 / tick_hz)).size(12).font(Font::MONOSPACE).width(120).color(pal.dim()),
                text(mark).size(11).width(16).color(pal.status_text(it.status)),
            ]
            .spacing(6);
            for c in &shown {
                r = r.push(text(cell(v.as_ref(), c)).size(12).font(Font::MONOSPACE).width(110).color(color));
            }
            r = r.push(text(layer.track.text(i).to_string()).size(12).color(color));
            r.into()
        },
        move |i| Msg::Select(Sel { claim: ci, layer: li, row: i }, id),
        move |t| Msg::ListScroll(id, t),
        Some(&lv.vis),
    );
    column![head, rule::horizontal(1), container(header).padding([2, 20]), body].into()
}
