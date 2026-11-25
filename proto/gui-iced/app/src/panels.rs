//! The small panels (search, measure, claims, speed) and the search itself (prototype 1's panels.rs: the same words).

use crate::app::{App, Msg, Sel};
use crate::list::{cell, vlist};
use crate::model::Rows;
use crate::wave::{fmt_freq, fmt_time};
use iced::widget::{button, column, container, row, rule, scrollable, text, text_input};
use iced::{Element, Font, Length};
use serde_json::Value;
use std::collections::HashMap;
use view_core::ann::Status;
use web_time::Instant;

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
    pub top: usize,
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

pub fn run_search(app: &mut App) {
    let t = Instant::now();
    let s = &mut app.search;
    s.hits.clear();
    s.by_layer.clear();
    s.hit_ticks.clear();
    s.cur = 0;
    s.top = 0;
    s.last = s.query.clone();
    let ws = words(&s.query);
    if ws.is_empty() {
        return;
    }
    let Some(a) = &app.analysis else { return };
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

pub fn search_view(app: &App, id: u32) -> Element<'_, Msg> {
    let pal = &app.pal;
    let s = &app.search;
    let input = text_input("例: 0x48 書き込み / addr=0x50 nack / エラー", &s.query)
        .on_input(Msg::SearchInput)
        .on_submit(Msg::SearchStep(true, id))
        .padding(6)
        .size(13);
    let head = column![
        row![text("検索").size(13), input].spacing(8).align_y(iced::Alignment::Center),
        row![
            text(format!("{} 件（{:.1} ms）", s.hits.len(), s.took_ms)).size(12).color(pal.dim()),
            iced::widget::space::horizontal(),
            button(text("前へ").size(12)).padding([3, 10]).on_press(Msg::SearchStep(false, id)),
            button(text("次へ（Enter）").size(12)).padding([3, 10]).on_press(Msg::SearchStep(true, id)),
        ]
        .spacing(6)
        .align_y(iced::Alignment::Center),
        text("言葉はすべて合うもの: key=値（0x.. は数で比べる）、nack、エラー / 誤り、疑わしい、書き込み、読み出し、ほかは項目の文字の中から")
            .size(11)
            .color(pal.dim()),
    ]
    .spacing(6)
    .padding(8);
    let Some(a) = &app.analysis else {
        return column![head].into();
    };
    let tick_hz = app.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let list = vlist(
        app,
        s.hits.len(),
        s.top,
        Some(s.cur),
        move |i| {
            let h = s.hits[i];
            let l = &a.claims[h.claim].layers[h.layer];
            row![
                text(format!("{}.{}", a.claims[h.claim].protocol, l.name))
                    .size(11)
                    .width(110)
                    .color(pal.dim()),
                text(fmt_time(l.track.item(h.row).s as f64 / tick_hz))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .width(120),
                text(l.track.text(h.row).replace('\n', " "))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .wrapping(iced::widget::text::Wrapping::None),
            ]
            .spacing(8)
            .into()
        },
        move |i| Msg::SearchPick(i, id),
        Msg::SearchScroll,
    );
    column![head, rule::horizontal(1), list].into()
}

pub fn measure_view(app: &App) -> Element<'_, Msg> {
    let pal = &app.pal;
    let hz = app.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let mut col = column![].spacing(6).padding(10);
    col = col.push(text("カーソル").size(12).color(pal.dim()));
    match (&app.measure, &app.cap) {
        (Some(m), Some(cap)) => {
            col = col.push(text(cap.lines[m.line].name.clone()).size(13));
            col = col.push(text(m.text(hz)).size(14).font(Font::MONOSPACE));
        }
        _ => {
            col = col.push(
                text("デジタルの線にマウスを載せると、パルスの幅・周期・周波数・デューティ")
                    .size(12)
                    .color(pal.dim()),
            )
        }
    }
    col = col.push(rule::horizontal(1));
    col = col.push(text("マーカー（右クリック / 1・2 キー）").size(12).color(pal.dim()));
    for (k, m) in app.markers.iter().enumerate() {
        let v = m.map(|t| fmt_time(t / hz)).unwrap_or("–".into());
        col = col.push(row![
            text(format!("M{}", k + 1))
                .size(13)
                .color(pal.c[crate::theme::MARKER1 + k])
                .width(36),
            text(v).size(13).font(Font::MONOSPACE)
        ]);
    }
    if let [Some(a), Some(b)] = app.markers {
        let d = (b - a).abs() / hz;
        col = col.push(
            text(format!(
                "Δ {}  ·  1/Δ {}",
                fmt_time(d),
                if d > 0.0 { fmt_freq(1.0 / d) } else { "–".into() }
            ))
            .size(14)
            .font(Font::MONOSPACE),
        );
        if let Some(cap) = &app.cap {
            // the changes between the markers, a line each
            let (t0, t1) = (a.min(b) as i64, a.max(b) as i64);
            let mut v = vec![];
            for l in &cap.lines {
                v.clear();
                let all = l.src.edges_in(t0, t1, 100_000, &mut v);
                col = col.push(
                    text(format!(
                        "{:<8} {}{} の変化",
                        l.name,
                        v.len(),
                        if all { "" } else { "+" }
                    ))
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(pal.dim()),
                );
            }
        }
        col = col.push(
            button(text("マーカーを消す").size(12))
                .padding([3, 10])
                .on_press(Msg::ClearMarkers),
        );
    }
    scrollable(col).height(Length::Fill).into()
}

pub fn claims_view(app: &App) -> Element<'_, Msg> {
    let pal = &app.pal;
    let mut col = column![].spacing(8).padding(10);
    match (&app.analysis, app.analysis_slot.is_some()) {
        (_, true) => col = col.push(text("解析中…").size(13).color(pal.accent())),
        (None, false) => col = col.push(text("解析の結果はまだありません").size(13).color(pal.dim())),
        (Some(a), false) => {
            if let Some(e) = &a.error {
                col = col.push(
                    text(format!("解析の誤り: {e}"))
                        .size(12)
                        .color(pal.status_text(Status::Error)),
                );
            }
            col = col.push(
                text(format!(
                    "主張 {} 件 · 解析 {:.2} s{}",
                    a.claims.len(),
                    a.seconds,
                    if a.fake { "（作り物）" } else { "" }
                ))
                .size(12)
                .color(pal.dim()),
            );
            for (ci, c) in a.claims.iter().enumerate() {
                let roles = c
                    .roles
                    .iter()
                    .map(|(r, n)| format!("{r}={n}"))
                    .collect::<Vec<_>>()
                    .join("  ");
                let layers = c
                    .layers
                    .iter()
                    .map(|l| format!("{} {}", l.name, l.len()))
                    .collect::<Vec<_>>()
                    .join(" · ");
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
                col = col.push(
                    container(
                        column![
                            row![
                                text(format!("#{ci} {}", c.protocol)).size(14),
                                text(format!("{} {:.2}", c.verdict, c.score))
                                    .size(12)
                                    .color(pal.verdict(&c.verdict)),
                            ]
                            .spacing(10)
                            .align_y(iced::Alignment::Center),
                            text(roles).size(12).font(Font::MONOSPACE),
                            text(layers).size(11).color(pal.dim()),
                            text(format!(
                                "誤り・疑わしい {bad}{}",
                                c.bus.as_ref().map(|b| format!(" · {b}")).unwrap_or_default()
                            ))
                            .size(11)
                            .color(pal.dim()),
                        ]
                        .spacing(3),
                    )
                    .padding(8)
                    .style(move |_| container::Style {
                        border: iced::Border {
                            color: app.pal.border,
                            width: 1.0,
                            radius: 6.0.into(),
                        },
                        ..Default::default()
                    })
                    .width(Length::Fill),
                );
            }
        }
    }
    scrollable(col).height(Length::Fill).into()
}

pub fn perf_view(app: &App) -> Element<'_, Msg> {
    let pal = &app.pal;
    let p = app.perf.borrow();
    let (n, avg, worst) = p.fps();
    let mut lines = vec![
        format!("fps（直近 1 秒）{n:.0}  ·  平均 {avg:.1} ms  ·  最悪 {worst:.1} ms"),
        format!(
            "波形を作る（scene）{:.2} ms  ·  図形 {}  ·  インスタンス {}  ·  文字 {}",
            p.wave_avg(),
            p.shapes,
            p.instances,
            p.labels
        ),
    ];
    if let Some(cap) = &app.cap {
        lines.push(format!(
            "記録: {} 本 · {:.2e} の変化 · {} tick · 読む {:.0} ms",
            cap.lines.len(),
            cap.total_edges as f64,
            cap.n_ticks,
            cap.read_s * 1e3
        ));
        lines.push(format!(
            "最初の絵 {}  ·  要約 {}",
            p.first_paint
                .map(|s| format!("{:.0} ms", s * 1e3))
                .unwrap_or("–".into()),
            cap.lod_s
                .lock()
                .unwrap()
                .map(|s| format!("{s:.2} s"))
                .unwrap_or("作っている".into())
        ));
        lines.push(format!(
            "モデル {:.0} MB  ·  RSS {}",
            cap.mem_bytes() as f64 / 1e6,
            crate::app::rss_mb().map(|m| format!("{m:.0} MB")).unwrap_or("–".into())
        ));
    }
    lines.push(format!("GPU: {}", app.gpu.borrow()));
    let mut col = column![].spacing(5).padding(10);
    for l in lines {
        col = col.push(text(l).size(12).font(Font::MONOSPACE));
    }
    col = col.push(
        row![
            iced::widget::checkbox(app.tiles_on)
                .label("タイル（256 px）")
                .on_toggle(Msg::Tiles)
                .size(14)
                .text_size(12),
            iced::widget::checkbox(app.continuous)
                .label("描き続ける")
                .on_toggle(Msg::Continuous)
                .size(14)
                .text_size(12),
        ]
        .spacing(14),
    );
    col = col.push(
        text("iced 0.14（wgpu）。波形は shader の primitive（1 回の draw）、文字は iced の文字（cosmic-text）")
            .size(11)
            .color(pal.dim()),
    );
    scrollable(col).height(Length::Fill).into()
}
