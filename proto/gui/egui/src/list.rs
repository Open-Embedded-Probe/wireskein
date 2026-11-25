//! The results list, apart from the waveform: one claim's one layer as rows (virtual: only the rows in view are
//! made), columns chosen, the claim and the layer switched, following the waveform's choice or not.

use crate::app::{Sel, Shared};
use crate::wave::fmt_time;
use eframe::egui::{self, RichText};
use egui_extras::{Column, TableBuilder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use view_core::ann::Status;

#[derive(Default, Serialize, Deserialize)]
pub struct ListTab {
    pub id: u32,
    pub claim: usize,
    pub layer: usize,
    pub hidden: Vec<String>,
    pub follow: bool,
    #[serde(skip)]
    pub seen_gen: u64,
    /// asked to be shown in a window of its own (native)
    #[serde(skip)]
    pub detach: bool,
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
        Some(Value::Bool(b)) => if *b { "✓".into() } else { "×".into() },
        Some(Value::Null) => "–".into(),
        Some(x) => {
            let s = x.to_string();
            if s.len() > 40 { format!("{}…", &s[..40]) } else { s }
        }
    }
}

pub fn ui(ui: &mut egui::Ui, tab: &mut ListTab, sh: &mut Shared) {
    let pal = sh.pal;
    let Some(a) = &sh.analysis else {
        let msg = if sh.analysis_slot.is_some() { "解析中…（終わると一覧が出ます）" } else { "解析の結果はまだありません" };
        ui.centered_and_justified(|ui| ui.label(msg));
        return;
    };
    if a.claims.is_empty() {
        ui.label(a.error.clone().unwrap_or("主張なし（読めたバスがない）".into()));
        return;
    }
    tab.claim = tab.claim.min(a.claims.len() - 1);
    let tick_hz = sh.cap.as_ref().map(|c| c.tick_hz).unwrap_or(1.0);
    let mut new_sel: Option<Sel> = None;
    ui.horizontal_wrapped(|ui| {
        let c = &a.claims[tab.claim];
        egui::ComboBox::from_id_salt(("claim", tab.id))
            .selected_text(format!("{} ({})", c.protocol, c.verdict))
            .show_ui(ui, |ui| {
                for (i, c) in a.claims.iter().enumerate() {
                    let roles: Vec<String> = c.roles.iter().map(|(r, n)| format!("{r}={n}")).collect();
                    let label = format!("{} · {} · {}", c.protocol, c.verdict, roles.join(" "));
                    if ui.selectable_label(tab.claim == i, label).clicked() {
                        tab.claim = i;
                        tab.layer = 0;
                    }
                }
            });
        let c = &a.claims[tab.claim];
        if c.layers.is_empty() {
            return;
        }
        tab.layer = tab.layer.min(c.layers.len() - 1);
        egui::ComboBox::from_id_salt(("layer", tab.id))
            .selected_text(&c.layers[tab.layer].name)
            .show_ui(ui, |ui| {
                for (i, l) in c.layers.iter().enumerate() {
                    if ui.selectable_label(tab.layer == i, format!("{} ({})", l.name, l.len())).clicked() {
                        tab.layer = i;
                    }
                }
            });
        let l = &c.layers[tab.layer];
        ui.menu_button("列", |ui| {
            for col in &l.columns {
                let mut on = !tab.hidden.contains(col);
                if ui.checkbox(&mut on, col).changed() {
                    if on {
                        tab.hidden.retain(|x| x != col);
                    } else {
                        tab.hidden.push(col.clone());
                    }
                }
            }
        });
        ui.checkbox(&mut tab.follow, "波形と連動");
        #[cfg(not(target_arch = "wasm32"))]
        if ui.button("別の窓へ").on_hover_text("この一覧を OS の別の窓に出す").clicked() {
            tab.detach = true;
        }
        ui.label(RichText::new(format!("{} 行", l.len())).color(pal.dim));
    });
    let c = &a.claims[tab.claim];
    if c.layers.is_empty() {
        return;
    }
    let l = &c.layers[tab.layer];
    let cols: Vec<&String> = l.columns.iter().filter(|c| !tab.hidden.contains(c)).collect();
    let sel_row = sh.sel.filter(|s| s.claim == tab.claim && s.layer == tab.layer).map(|s| s.row);
    let mut scroll_to = None;
    if sh.sel_gen != tab.seen_gen {
        tab.seen_gen = sh.sel_gen;
        if tab.follow && sh.sel_src != tab.id {
            if let Some(r) = sel_row {
                scroll_to = Some(r);
            } else if let Some(s) = sh.sel
                && let Some((t, _)) = sh.sel_span()
            {
                // chosen in another claim or layer: the row at that time
                let _ = s;
                scroll_to = l.track.index_at(t);
            }
        }
    }
    let hits = sh.search.by_layer.get(&(tab.claim, tab.layer));
    let row_h = 19.0;
    let mut tb = TableBuilder::new(ui)
        .id_salt(("list", tab.id))
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(Column::auto().at_least(56.0))
        .column(Column::auto().at_least(110.0))
        .column(Column::auto().at_least(80.0));
    for _ in &cols {
        tb = tb.column(Column::auto().at_least(50.0).clip(true));
    }
    tb = tb.column(Column::remainder());
    if let Some(r) = scroll_to {
        tb = tb.scroll_to_row(r, Some(egui::Align::Center));
    }
    tb.header(20.0, |mut h| {
        h.col(|ui| {
            ui.strong("#");
        });
        h.col(|ui| {
            ui.strong("時刻");
        });
        h.col(|ui| {
            ui.strong("長さ");
        });
        for c in &cols {
            h.col(|ui| {
                ui.strong(c.as_str());
            });
        }
        h.col(|_| {});
    })
    .body(|body| {
        body.rows(row_h, l.len(), |mut row| {
            let i = row.index();
            let it = l.track.item(i);
            let v = l.value(i);
            let v = v.as_ref();
            row.set_selected(sel_row == Some(i));
            let color = match it.status {
                Status::Error => Some(pal.error),
                Status::Warn => Some(pal.warn),
                Status::Note => Some(pal.note),
                _ => None,
            };
            let hit = hits.is_some_and(|h| h.binary_search(&i).is_ok());
            let txt = |s: String| {
                let mut r = RichText::new(s).monospace();
                if hit {
                    r = r.color(pal.accent);
                }
                r
            };
            row.col(|ui| {
                if let Some(c) = color {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(4.0, 14.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 1.0, if c == pal.note { pal.verdict("likely") } else { c.gamma_multiply(1.6) });
                }
                ui.label(txt(format!("{i}")));
            });
            row.col(|ui| {
                ui.label(txt(fmt_time(it.s as f64 / tick_hz)));
            });
            row.col(|ui| {
                ui.label(txt(fmt_time((it.e - it.s) as f64 / tick_hz)));
            });
            for c in &cols {
                row.col(|ui| {
                    ui.label(txt(cell(v, c)));
                });
            }
            row.col(|_| {});
            if row.response().clicked() {
                new_sel = Some(Sel { claim: tab.claim, layer: tab.layer, row: i });
            }
        });
    });
    if let Some(s) = new_sel {
        sh.select(s, tab.id);
    }
}
