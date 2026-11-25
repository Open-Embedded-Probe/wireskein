//! The panels' layout: iced's pane_grid (split, drag a pane by its title onto an edge, resize, maximize) with tabs of
//! our own on top (a pane dropped onto another's middle joins it as a tab; a tab split out again), panels detached
//! into windows of their own (and back when closed), layouts saved by name and switched (a JSON file).

use crate::app::{App, Msg};
use iced::widget::pane_grid::{self, Axis, Configuration, Content, Node, Pane as PaneId, TitleBar};
use iced::widget::{button, container, row, text};
use iced::{Background, Border, Color, Element, Length};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash, PartialOrd, Ord)]
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
            Kind::List => "一覧",
            Kind::Search => "検索",
            Kind::Measure => "測定",
            Kind::Claims => "主張",
            Kind::Perf => "速さ",
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A pane of the grid: its tabs (panel ids) and the one shown.
#[derive(Clone, Debug)]
pub struct Pane {
    pub tabs: Vec<u32>,
    pub active: usize,
}

/// A layout as saved: splits and tabs of panel kinds.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Conf {
    Split {
        side_by_side: bool,
        ratio: f32,
        a: Box<Conf>,
        b: Box<Conf>,
    },
    Tabs {
        tabs: Vec<Kind>,
        active: usize,
    },
}

fn split(side_by_side: bool, ratio: f32, a: Conf, b: Conf) -> Conf {
    Conf::Split {
        side_by_side,
        ratio,
        a: Box::new(a),
        b: Box::new(b),
    }
}
fn tabs(k: &[Kind]) -> Conf {
    Conf::Tabs {
        tabs: k.to_vec(),
        active: 0,
    }
}

pub const PRESETS: [&str; 3] = ["標準", "一覧を大きく", "波形 2 つ"];

pub fn preset(name: &str) -> Conf {
    use Kind::*;
    match name {
        "一覧を大きく" => split(
            false,
            0.42,
            tabs(&[Wave]),
            split(true, 0.7, tabs(&[List]), tabs(&[Search, Measure, Claims, Perf])),
        ),
        "波形 2 つ" => split(
            true,
            0.66,
            split(false, 0.5, tabs(&[Wave]), tabs(&[Wave])),
            split(false, 0.6, tabs(&[List]), tabs(&[Search, Measure, Claims, Perf])),
        ),
        _ => split(
            true,
            0.68,
            tabs(&[Wave]),
            split(false, 0.6, tabs(&[List]), tabs(&[Search, Measure, Claims, Perf])),
        ),
    }
}

/// The grid as a layout to save.
pub fn conf_of(app: &App) -> Conf {
    fn walk(app: &App, n: &Node) -> Conf {
        match n {
            Node::Split { axis, ratio, a, b, .. } => split(*axis == Axis::Vertical, *ratio, walk(app, a), walk(app, b)),
            Node::Pane(p) => {
                let pane = app.panes.get(*p).unwrap();
                Conf::Tabs {
                    tabs: pane.tabs.iter().map(|id| app.kind_of(*id)).collect(),
                    active: pane.active,
                }
            }
        }
    }
    walk(app, app.panes.layout())
}

/// The grid built from a layout: panels of each kind reused in order (their views kept), new ones made, the rest
/// dropped (the detached ones stay in their windows).
pub fn apply(app: &mut App, conf: &Conf) {
    let mut pool: BTreeMap<Kind, Vec<u32>> = BTreeMap::new();
    for (_, p) in app.panes.iter() {
        for &id in &p.tabs {
            pool.entry(app.kind_of(id)).or_default().push(id);
        }
    }
    for v in pool.values_mut() {
        v.sort();
        v.reverse();
    }
    fn build(app: &mut App, pool: &mut BTreeMap<Kind, Vec<u32>>, c: &Conf) -> Configuration<Pane> {
        match c {
            Conf::Split {
                side_by_side,
                ratio,
                a,
                b,
            } => Configuration::Split {
                axis: if *side_by_side {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                },
                ratio: *ratio,
                a: Box::new(build(app, pool, a)),
                b: Box::new(build(app, pool, b)),
            },
            Conf::Tabs { tabs, active } => {
                let ids = tabs
                    .iter()
                    .map(|&k| {
                        pool.get_mut(&k)
                            .and_then(|v| v.pop())
                            .unwrap_or_else(|| app.new_panel(k))
                    })
                    .collect::<Vec<_>>();
                Configuration::Pane(Pane {
                    active: (*active).min(ids.len().saturating_sub(1)),
                    tabs: ids,
                })
            }
        }
    }
    let cfg = build(app, &mut pool, conf);
    for ids in pool.into_values() {
        for id in ids {
            app.drop_panel(id);
        }
    }
    app.panes = pane_grid::State::with_configuration(cfg);
}

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wsk-iced").join("layouts.json"))
}

pub fn load_saved() -> BTreeMap<String, Conf> {
    store_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_all(m: &BTreeMap<String, Conf>) {
    if let Some(p) = store_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        let _ = std::fs::write(p, serde_json::to_string_pretty(m).unwrap_or_default());
    }
}

fn small_btn<'a>(label: &'a str, msg: Msg, pal: crate::theme::Pal, on: bool) -> Element<'a, Msg> {
    button(text(label).size(12))
        .padding([2, 8])
        .on_press(msg)
        .style(move |_, st| button::Style {
            background: Some(Background::Color(if on {
                pal.c[crate::theme::BG]
            } else if matches!(st, button::Status::Hovered) {
                Color { a: 0.15, ..pal.text() }
            } else {
                Color::TRANSPARENT
            })),
            text_color: if on { pal.text() } else { pal.dim() },
            border: Border {
                radius: 4.0.into(),
                color: if on { pal.border } else { Color::TRANSPARENT },
                width: 1.0,
            },
            ..Default::default()
        })
        .into()
}

pub fn view(app: &App) -> Element<'_, Msg> {
    let pal = app.pal;
    let total = app.panes.len();
    iced::widget::PaneGrid::new(&app.panes, move |pane, p, maximized| {
        let id = p.tabs[p.active.min(p.tabs.len() - 1)];
        let mut tabs = row![text("⠿").size(13).color(pal.dim())]
            .spacing(2)
            .align_y(iced::Alignment::Center);
        for (i, &t) in p.tabs.iter().enumerate() {
            tabs = tabs.push(small_btn(app.title_of(t), Msg::TabSelect(pane, i), pal, i == p.active));
        }
        let mut ctl = row![].spacing(2);
        if p.tabs.len() > 1 {
            ctl = ctl.push(small_btn("分ける", Msg::SplitOut(pane), pal, false));
        }
        // the web has one window (the page's canvas)
        if !cfg!(target_arch = "wasm32") {
            ctl = ctl.push(small_btn("別の窓へ", Msg::Detach(pane), pal, false));
        }
        ctl = ctl.push(small_btn(
            if maximized { "戻す" } else { "最大" },
            Msg::Maximize(pane),
            pal,
            false,
        ));
        if total > 1 || p.tabs.len() > 1 {
            ctl = ctl.push(small_btn("×", Msg::CloseTab(pane), pal, false));
        }
        let title = TitleBar::new(tabs)
            .controls(pane_grid::Controls::new(ctl))
            .padding([3, 6])
            .style(move |_| container::Style {
                background: Some(Background::Color(pal.c[crate::theme::LANE_ALT])),
                ..Default::default()
            });
        Content::new(container(app.panel_body(id)).width(Length::Fill).height(Length::Fill))
            .title_bar(title)
            .style(move |_| container::Style {
                background: Some(Background::Color(pal.panel)),
                border: Border {
                    color: pal.border,
                    width: 1.0,
                    radius: 6.0.into(),
                },
                ..Default::default()
            })
    })
    .on_drag(Msg::PaneDrag)
    .on_resize(8, Msg::PaneResize)
    .on_click(Msg::PaneClick)
    .spacing(5)
    .into()
}

/// A pane dropped onto another's middle: its tabs join that pane.
pub fn drop(app: &mut App, ev: pane_grid::DragEvent) {
    use pane_grid::{DragEvent, Region, Target};
    if let DragEvent::Dropped { pane, target } = ev {
        match target {
            Target::Pane(to, Region::Center) if to != pane => {
                if let Some((p, _)) = app.panes.close(pane)
                    && let Some(dst) = app.panes.get_mut(to)
                {
                    dst.tabs.extend(p.tabs);
                    dst.active = dst.tabs.len() - 1;
                }
            }
            _ => app.panes.drop(pane, target),
        }
    }
}

pub fn first_pane(app: &App) -> Option<PaneId> {
    app.panes.iter().next().map(|(p, _)| *p)
}
