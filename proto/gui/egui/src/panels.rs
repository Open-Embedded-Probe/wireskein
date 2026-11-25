//! The small panels: measure (the cursor's pulse, the two markers), search (filter the results, hits marked on the
//! waveform), claims (what the analysis found), performance (frame times, memory, times to the first picture).

use crate::app::{Sel, Shared};
use crate::list::cell;
use crate::wave::{fmt_freq, fmt_time};
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use serde_json::Value;
use std::collections::HashMap;
use web_time::Instant;
use view_core::ann::Status;

pub fn measure_ui(ui: &mut egui::Ui, sh: &mut Shared) {
    let Some(cap) = &sh.cap else { return };
    let pal = sh.pal;
    ui.heading("カーソル");
    match &sh.measure {
        Some(m) => {
            ui.label(format!("線 {}  ·  {}", cap.lines[m.line].name, fmt_time(cap.secs(m.t))));
            ui.label(m.text(cap.tick_hz));
        }
        None => {
            ui.label(RichText::new("デジタルの線にマウスを載せると、幅・周期・周波数・デューティを測る").color(pal.dim));
        }
    }
    ui.separator();
    ui.heading("マーカー");
    ui.label(RichText::new("波形の上で 1 / 2 キー、または右クリック").color(pal.dim));
    egui::Grid::new("markers").num_columns(2).show(ui, |ui| {
        for k in 0..2 {
            ui.colored_label(pal.marker[k], format!("M{}", k + 1));
            ui.label(sh.markers[k].map(|t| fmt_time(cap.secs(t))).unwrap_or("–".into()));
            ui.end_row();
        }
        if let (Some(a), Some(b)) = (sh.markers[0], sh.markers[1]) {
            let d = cap.secs((b - a).abs());
            ui.label("Δ");
            ui.label(format!("{}  ·  {}", fmt_time(d), fmt_freq(1.0 / d.max(1e-15))));
            ui.end_row();
        }
    });
    if let (Some(a), Some(b)) = (sh.markers[0], sh.markers[1]) {
        let (a, b) = (a.min(b) as i64, a.max(b) as i64);
        ui.separator();
        ui.label("マーカーの間の変化の数");
        egui::Grid::new("counts").num_columns(2).striped(true).show(ui, |ui| {
            for l in &cap.lines {
                let mut v = vec![];
                let complete = l.src.edges_in(a, b, 1_000_000, &mut v);
                ui.label(&l.name);
                ui.label(if complete { format!("{}", v.len()) } else { "> 1,000,000".into() });
                ui.end_row();
            }
        });
    }
}

/// The search: words all to match. `key=value` (a field: 0x.. or a number compared as a number, else as text),
/// `nack`, `error` / `エラー` / `誤り`, `warn` / `疑わしい`, `write` / `書き込み`, `read` / `読み出し`; other words
/// in the item's text.
#[derive(Default)]
pub struct Search {
    pub query: String,
    pub last: String,
    pub hits: Vec<Sel>,
    pub by_layer: HashMap<(usize, usize), Vec<usize>>,
    pub hit_ticks: Vec<i64>,
    pub cur: usize,
    pub took_ms: f64,
    pub analysis_seen: bool,
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
            Some(Value::Bool(b)) => matches!((want.as_str(), b), ("true" | "1" | "✓", true) | ("false" | "0" | "✗", false)),
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

pub fn run_search(sh: &mut Shared) {
    let t = Instant::now();
    let s = &mut sh.search;
    s.hits.clear();
    s.by_layer.clear();
    s.hit_ticks.clear();
    s.cur = 0;
    s.last = s.query.clone();
    let ws = words(&s.query);
    if ws.is_empty() {
        return;
    }
    let Some(a) = &sh.analysis else { return };
    for (ci, c) in a.claims.iter().enumerate() {
        for (li, l) in c.layers.iter().enumerate() {
            let mut v = vec![];
            if matches!(l.rows, crate::model::Rows::Bits(_)) {
                continue; // (bits: not searched)
            }
            for i in 0..l.len() {
                let it = l.track.item(i);
                let row = l.value(i);
                if matches(&ws, row.as_ref(), l.track.text(i), it.status) {
                    v.push(i);
                    if s.hits.len() < 2_000_000 {
                        s.hits.push(Sel { claim: ci, layer: li, row: i });
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

pub fn search_ui(ui: &mut egui::Ui, sh: &mut Shared, my_id: u32) {
    let pal = sh.pal;
    ui.horizontal(|ui| {
        ui.label("検索");
        let r = ui.add(
            egui::TextEdit::singleline(&mut sh.search.query)
                .hint_text("例: 0x48 書き込み / addr=0x50 nack / エラー")
                .desired_width(f32::INFINITY),
        );
        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            step(sh, true, my_id);
        }
    });
    if sh.search.query != sh.search.last || (!sh.search.analysis_seen && sh.analysis.is_some()) {
        sh.search.analysis_seen = sh.analysis.is_some();
        run_search(sh);
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new(format!("{} 件（{:.1} ms）", sh.search.hits.len(), sh.search.took_ms)).color(pal.dim));
        if ui.button("前へ").clicked() {
            step(sh, false, my_id);
        }
        if ui.button("次へ（Enter）").clicked() {
            step(sh, true, my_id);
        }
    });
    ui.label(
        RichText::new("言葉はすべて合うもの: key=値（0x.. は数で比べる）、nack、エラー / 誤り、疑わしい、書き込み、読み出し、ほかは項目の文字の中から")
            .small()
            .color(pal.dim),
    );
    ui.separator();
    let Some(a) = &sh.analysis else { return };
    let tick_hz = sh.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let mut pick = None;
    let cur = sh.search.cur;
    TableBuilder::new(ui)
        .id_salt("hits")
        .striped(true)
        .sense(egui::Sense::click())
        .column(Column::auto().at_least(60.0))
        .column(Column::auto().at_least(110.0))
        .column(Column::remainder())
        .body(|body| {
            body.rows(18.0, sh.search.hits.len(), |mut row| {
                let i = row.index();
                let h = sh.search.hits[i];
                let l = &a.claims[h.claim].layers[h.layer];
                row.set_selected(i == cur);
                row.col(|ui| {
                    ui.label(RichText::new(format!("{}.{}", a.claims[h.claim].protocol, l.name)).small());
                });
                row.col(|ui| {
                    ui.monospace(fmt_time(l.track.item(h.row).s as f64 / tick_hz));
                });
                row.col(|ui| {
                    ui.monospace(l.track.text(h.row));
                });
                if row.response().clicked() {
                    pick = Some(i);
                }
            });
        });
    if let Some(i) = pick {
        sh.search.cur = i;
        let s = sh.search.hits[i];
        sh.select(s, my_id);
    }
}

fn step(sh: &mut Shared, fwd: bool, my_id: u32) {
    let n = sh.search.hits.len();
    if n == 0 {
        return;
    }
    let s = &mut sh.search;
    s.cur = if fwd { (s.cur + 1) % n } else { (s.cur + n - 1) % n };
    let h = s.hits[s.cur];
    sh.select(h, my_id);
}

pub fn info_ui(ui: &mut egui::Ui, sh: &mut Shared) {
    let pal = sh.pal;
    let Some(cap) = &sh.cap else { return };
    ui.heading(&cap.title);
    ui.label(format!(
        "{} 本の線 · {} の変化 · {} · tick {} Hz · 区画 {}/{}{}",
        cap.lines.len(),
        cap.total_edges,
        fmt_time(cap.secs(cap.n_ticks as f64)),
        cap.tick_hz,
        cap.part,
        cap.n_parts,
        if cap.complete { "" } else { " · 不完全（データ落ち）" }
    ));
    if !cap.lost.is_empty() {
        ui.colored_label(pal.error, format!("データ落ち: {}", serde_json::to_string(&cap.lost).unwrap_or_default()));
    }
    ui.separator();
    match (&sh.analysis, &sh.analysis_slot) {
        (Some(a), _) => {
            ui.label(format!("解析 {:.2} s{}", a.seconds, if a.fake { "（作り物の項目）" } else { "（wireskein-analysis）" }));
            if let Some(e) = &a.error {
                ui.colored_label(pal.error, e);
            }
            for c in &a.claims {
                ui.horizontal(|ui| {
                    ui.colored_label(pal.verdict(&c.verdict), "●");
                    ui.strong(&c.protocol);
                    ui.label(RichText::new(&c.verdict).color(pal.verdict(&c.verdict)));
                    ui.label(format!("score {:.3}", c.score));
                    if let Some(b) = &c.bus {
                        ui.label(format!("bus {b}"));
                    }
                });
                let roles: Vec<String> = c.roles.iter().map(|(r, n)| format!("{r}={n}")).collect();
                ui.label(RichText::new(format!("   {}", roles.join("  "))).color(pal.dim));
                let ls: Vec<String> = c.layers.iter().map(|l| format!("{} {}", l.name, l.len())).collect();
                ui.label(RichText::new(format!("   {}", ls.join(" · "))).color(pal.dim));
            }
        }
        (None, Some(_)) => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("解析中…");
            });
        }
        _ => {}
    }
}

pub fn perf_ui(ui: &mut egui::Ui, sh: &mut Shared) {
    let p = &sh.perf;
    let (fps, avg, worst) = p.fps();
    egui::Grid::new("perf").num_columns(2).striped(true).show(ui, |ui| {
        ui.label("fps（直近 1 秒）");
        ui.label(format!("{fps:.0}  （平均 {avg:.1} ms、最悪 {worst:.1} ms）"));
        ui.end_row();
        ui.label("波形を作る時間（CPU）");
        ui.label(format!("{:.2} ms（平均） · 図形 {}", p.wave_avg(), p.shapes));
        ui.end_row();
        ui.label("タイル");
        ui.label(format!("{} 個 · 作った {} · 使い回し {}", p.tiles.0, p.tiles.1, p.tiles.2));
        ui.end_row();
        ui.label("最初の絵まで");
        ui.label(p.first_paint.map(|s| format!("{:.0} ms", s * 1e3)).unwrap_or("–".into()));
        ui.end_row();
        if let Some(cap) = &sh.cap {
            ui.label("ファイルを読む");
            ui.label(format!("{:.0} ms", cap.read_s * 1e3));
            ui.end_row();
            ui.label("要約（全部の線）");
            ui.label(
                cap.lod_s.lock().unwrap().map(|s| format!("{:.2} s", s)).unwrap_or(format!(
                    "作成中 {}/{}",
                    cap.lod_done.load(std::sync::atomic::Ordering::Relaxed),
                    cap.lines.len()
                )),
            );
            ui.end_row();
            ui.label("記録と要約のメモリ");
            ui.label(format!("{:.1} MB", cap.mem_bytes() as f64 / 1e6));
            ui.end_row();
        }
        ui.label("プロセスのメモリ（RSS）");
        ui.label(crate::app::rss_mb().map(|m| format!("{m:.0} MB")).unwrap_or("–".into()));
        ui.end_row();
    });
    ui.checkbox(&mut sh.tiles_on, "タイルを使う（256 px、2 の冪の倍率ごと）");
    ui.checkbox(&mut sh.continuous, "毎フレーム描く（fps を測る）");
}
