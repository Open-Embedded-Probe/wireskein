//! The waveform view: one custom iced widget. It lays out the lanes, has scene.rs make the frame (instances for the
//! shader primitive of gpu.rs, labels), draws the text with iced's text renderer above, and turns the mouse into
//! messages (zoom, pan with a fling, a range to zoom to, the minimap, a click on an annotation, the markers).

use crate::app::{App, Msg, P_SCENE, P_TEXT, P_WAVE, WaveView};
use crate::gpu::WavePrim;
use crate::scene::{self, Geo, Hits, Lane};
use crate::theme::*;
use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad, Renderer as _};
use iced::advanced::text::{self, Renderer as _};
use iced::advanced::widget::{self, Widget, tree};
use iced::advanced::{Clipboard, Shell};
use iced::mouse::{self, Button, ScrollDelta};
use iced::{Color, Element, Event, Font, Length, Pixels, Point, Rectangle, Size, Theme, keyboard};
use iced_wgpu::primitive::Renderer as _;
use std::sync::Arc;
use view_core::Viewport;
use web_time::Instant;

pub const LABEL_W: f32 = 112.0;
const MM_H: f32 = 30.0;
const RULER_H: f32 = 18.0;
const DIG_H: f32 = 26.0;
const ANN_H: f32 = 18.0;
const ANA_H: f32 = 56.0;
const UNREAD_H: f32 = 6.0;
const ANN_FONT: f32 = 11.0;
/// the annotations' text: px a character (for folding what does not fit)
const CHAR_PX: f32 = 6.6;

pub fn lanes(app: &App) -> Vec<(Lane, f32)> {
    let mut out = vec![];
    let Some(cap) = &app.cap else { return out };
    let claims = app.analysis.as_ref().map(|a| &a.claims[..]).unwrap_or(&[]);
    let add_claim = |out: &mut Vec<(Lane, f32)>, ci: usize| {
        let c = &claims[ci];
        for li in 0..c.layers.len().min(3) {
            out.push((Lane::Ann { claim: ci, layer: li }, ANN_H));
        }
        if c.unread.as_ref().is_some_and(|u| !u.is_empty()) {
            out.push((Lane::Unread { claim: ci }, UNREAD_H));
        }
    };
    for i in 0..cap.lines.len() {
        out.push((Lane::Dig(i), DIG_H));
        for (ci, c) in claims.iter().enumerate() {
            if c.anchor == Some(i) {
                add_claim(&mut out, ci);
            }
        }
    }
    for (ci, c) in claims.iter().enumerate() {
        if c.anchor.is_none() {
            add_claim(&mut out, ci);
        }
    }
    for i in 0..cap.analog.len() {
        out.push((Lane::Ana(i), ANA_H));
    }
    out
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

fn nice_step(min_s: f64) -> f64 {
    let p = 10f64.powf(min_s.log10().floor());
    for m in [1.0, 2.0, 5.0, 10.0] {
        if p * m >= min_s {
            return p * m;
        }
    }
    p * 10.0
}

fn fmt_ruler(s: f64, step: f64) -> String {
    let (div, unit) = if step >= 1.0 {
        (1.0, "s")
    } else if step >= 1e-3 {
        (1e-3, "ms")
    } else if step >= 1e-6 {
        (1e-6, "µs")
    } else {
        (1e-9, "ns")
    };
    let digits = ((div / step).log10().ceil().max(0.0)) as usize;
    format!("{:.*} {unit}", digits, s / div)
}

/// The geometry of a view of `size` (local px).
pub struct Frame {
    pub px: f32,
    pub pw: f32,
    pub top: f32,
    pub bottom: f32,
    pub mm: [f32; 4],
    pub ruler_y: f32,
}

pub fn frame_geo(size: Size) -> Frame {
    let px = LABEL_W;
    let pw = (size.width - LABEL_W - 4.0).max(10.0);
    Frame {
        px,
        pw,
        top: MM_H + 6.0 + RULER_H,
        bottom: size.height,
        mm: [px, 2.0, pw, MM_H],
        ruler_y: MM_H + 4.0,
    }
}

/// The lanes placed: (lane, y, h, alternate).
fn placed(app: &App, wv: &WaveView, f: &Frame) -> Vec<(Lane, f32, f32, bool)> {
    let mut y = f.top - wv.scroll_y;
    let mut out = vec![];
    let mut k = 0;
    for (lane, h) in lanes(app) {
        let alt = matches!(lane, Lane::Dig(_) | Lane::Ana(_)) && {
            k += 1;
            k % 2 == 0
        };
        out.push((lane, y, h, alt));
        y += h;
    }
    out
}

pub fn view_of(app: &App, wv: &WaveView, width: f32) -> Viewport {
    let n = app.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1);
    match wv.vp {
        Some(v) if (v.width - width).abs() < 0.5 => v,
        // the whole capture stays the whole when the width changes (the web: the page's canvas is laid out after the
        // first frames, and a view fitted to its first width kept that scale)
        Some(v) if v.t0 <= 0.0 && v.t0 + v.tpp * v.width as f64 >= n as f64 => Viewport::fit(n, width),
        Some(v) => Viewport { width, ..v },
        None => Viewport::fit(n, width),
    }
}

pub struct Wave<'a> {
    app: &'a App,
    wv: &'a WaveView,
}

pub fn wave<'a>(app: &'a App, wv: &'a WaveView) -> Element<'a, Msg> {
    Element::new(Wave { app, wv })
}

#[derive(Clone, Copy)]
enum Drag {
    Pan {
        x0: f32,
        t0: f64,
        last: (f32, Instant),
        vel: f64,
        moved: bool,
    },
    Range {
        x0: f32,
        x1: f32,
    },
    Minimap,
}

#[derive(Default)]
struct State {
    drag: Option<Drag>,
    mods: keyboard::Modifiers,
    last_click: Option<(Instant, f32)>,
    width: f32,
}

impl Wave<'_> {
    fn n(&self) -> i64 {
        self.app.cap.as_ref().map(|c| c.n_ticks).unwrap_or(1)
    }
    fn publish_vp(&self, shell: &mut Shell<'_, Msg>, vp: Viewport) {
        shell.publish(Msg::WaveVp(self.wv.id, vp.clamped(self.n())));
    }
}

impl Widget<Msg, Theme, iced::Renderer> for Wave<'_> {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }
    fn state(&self) -> tree::State {
        tree::State::new(State::default())
    }
    fn size(&self) -> Size<Length> {
        Size::new(Length::Fill, Length::Fill)
    }
    fn layout(&mut self, _tree: &mut widget::Tree, _r: &iced::Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.max())
    }

    fn update(
        &mut self,
        tree: &mut widget::Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _r: &iced::Renderer,
        _c: &mut dyn Clipboard,
        shell: &mut Shell<'_, Msg>,
        _vp: &Rectangle,
    ) {
        let st = tree.state.downcast_mut::<State>();
        let b = layout.bounds();
        let f = frame_geo(b.size());
        if self.app.cap.is_none() {
            return;
        }
        let vp = view_of(self.app, self.wv, f.pw);
        // the view follows the widget's width (the first frame: the whole capture)
        if self.wv.vp.is_none_or(|v| (v.width - f.pw).abs() >= 0.5) && (st.width - f.pw).abs() >= 0.5 {
            st.width = f.pw;
            shell.publish(Msg::WaveVp(self.wv.id, vp));
        }
        let local = cursor.position_in(b);
        match event {
            Event::Keyboard(keyboard::Event::ModifiersChanged(m)) => st.mods = *m,
            Event::Mouse(mouse::Event::WheelScrolled { delta }) => {
                let Some(p) = local else { return };
                let dy = match delta {
                    ScrollDelta::Lines { y, .. } => *y * 40.0,
                    ScrollDelta::Pixels { y, .. } => *y,
                };
                if p.x < f.px && p.y > f.top {
                    shell.publish(Msg::WaveScrollY(self.wv.id, -dy));
                } else if st.mods.shift() {
                    let mut v = vp;
                    v.t0 -= dy as f64 * vp.tpp;
                    self.publish_vp(shell, v);
                } else {
                    let factor = 1.0015f64.powf(dy as f64);
                    self.publish_vp(shell, vp.zoomed(factor, (p.x - f.px).clamp(0.0, f.pw)));
                }
                shell.request_redraw();
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::ButtonPressed(btn)) => {
                let Some(p) = local else { return };
                shell.publish(Msg::FocusWave(self.wv.id));
                let in_mm = p.y <= f.mm[1] + f.mm[3] + 2.0 && p.x >= f.px;
                match btn {
                    Button::Left if in_mm => {
                        st.drag = Some(Drag::Minimap);
                        let t = (p.x - f.px) as f64 / f.pw as f64 * self.n() as f64;
                        let mut v = vp;
                        v.t0 = t - vp.tpp * vp.width as f64 / 2.0;
                        self.publish_vp(shell, v);
                    }
                    Button::Left if p.x >= f.px && st.mods.shift() => st.drag = Some(Drag::Range { x0: p.x, x1: p.x }),
                    Button::Right if p.x >= f.px => st.drag = Some(Drag::Range { x0: p.x, x1: p.x }),
                    Button::Left if p.x >= f.px => {
                        st.drag = Some(Drag::Pan {
                            x0: p.x,
                            t0: vp.t0,
                            last: (p.x, Instant::now()),
                            vel: 0.0,
                            moved: false,
                        });
                        shell.publish(Msg::WaveFling(self.wv.id, 0.0));
                    }
                    _ => return,
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorMoved { .. }) => {
                let p = cursor.position().map(|q| Point::new(q.x - b.x, q.y - b.y));
                match (&mut st.drag, p) {
                    (
                        Some(Drag::Pan {
                            x0,
                            t0,
                            last,
                            vel,
                            moved,
                        }),
                        Some(p),
                    ) => {
                        if (p.x - *x0).abs() > 3.0 {
                            *moved = true;
                        }
                        let mut v = vp;
                        v.t0 = *t0 - (p.x - *x0) as f64 * vp.tpp;
                        let now = Instant::now();
                        let dt = now.duration_since(last.1).as_secs_f64().max(1e-3);
                        // ticks a second, smoothed
                        let inst = -((p.x - last.0) as f64) * vp.tpp / dt;
                        *vel = *vel * 0.6 + inst * 0.4;
                        *last = (p.x, now);
                        self.publish_vp(shell, v);
                    }
                    (Some(Drag::Range { x1, .. }), Some(p)) => {
                        *x1 = p.x.clamp(f.px, f.px + f.pw);
                        shell.request_redraw();
                    }
                    (Some(Drag::Minimap), Some(p)) => {
                        let t = (p.x - f.px) as f64 / f.pw as f64 * self.n() as f64;
                        let mut v = vp;
                        v.t0 = t - vp.tpp * vp.width as f64 / 2.0;
                        self.publish_vp(shell, v);
                    }
                    _ => {
                        let hover = local.filter(|p| p.x >= f.px && p.y >= f.top).map(|p| {
                            let t = vp.t_of(p.x - f.px);
                            let lane = placed(self.app, self.wv, &f)
                                .into_iter()
                                .find(|l| p.y >= l.1 && p.y < l.1 + l.2)
                                .map(|l| l.0);
                            (t, lane, p)
                        });
                        if hover.is_some() || self.app.hover.is_some_and(|h| h.wave == self.wv.id) {
                            shell.publish(Msg::Hover(self.wv.id, hover));
                        }
                    }
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(btn)) => {
                let Some(d) = st.drag.take() else { return };
                match (d, btn) {
                    (Drag::Pan { moved: false, .. }, Button::Left) => {
                        // a click: an annotation chosen (twice: zoomed to it)
                        let Some(p) = local else { return };
                        let now = Instant::now();
                        let double = st
                            .last_click
                            .is_some_and(|(t, x)| now.duration_since(t).as_millis() < 400 && (x - p.x).abs() < 4.0);
                        st.last_click = Some((now, p.x));
                        let t = vp.t_of(p.x - f.px);
                        if let Some((Lane::Ann { claim, layer }, ..)) = placed(self.app, self.wv, &f)
                            .into_iter()
                            .find(|l| p.y >= l.1 && p.y < l.1 + l.2)
                            && let Some(a) = &self.app.analysis
                            && let Some(row) = a.claims[claim].layers[layer].track.hit(t as i64)
                        {
                            shell.publish(Msg::Select(crate::app::Sel { claim, layer, row }, self.wv.id));
                            if double {
                                shell.publish(Msg::ZoomToSel(self.wv.id));
                            }
                        }
                    }
                    (Drag::Pan { vel, last, .. }, Button::Left) => {
                        // a fling: only when the hand was still moving
                        if last.1.elapsed().as_millis() < 80 {
                            shell.publish(Msg::WaveFling(self.wv.id, vel));
                        }
                    }
                    (Drag::Range { x0, x1 }, _) => {
                        if (x1 - x0).abs() > 3.0 {
                            let (a, b2) = (x0.min(x1) - f.px, x0.max(x1) - f.px);
                            let mut v = vp;
                            v.t0 = vp.t_of(a);
                            v.tpp = ((b2 - a) as f64 * vp.tpp / f.pw as f64).max(1e-3);
                            self.publish_vp(shell, v);
                        } else if *btn == Button::Right {
                            shell.publish(Msg::MarkerClick(vp.t_of(x0 - f.px)));
                        }
                    }
                    _ => {}
                }
                shell.capture_event();
            }
            Event::Mouse(mouse::Event::CursorLeft) => {
                if self.app.hover.is_some_and(|h| h.wave == self.wv.id) {
                    shell.publish(Msg::Hover(self.wv.id, None));
                }
            }
            _ => {}
        }
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        _v: &Rectangle,
        _r: &iced::Renderer,
    ) -> mouse::Interaction {
        let st = tree.state.downcast_ref::<State>();
        match st.drag {
            Some(Drag::Pan { .. }) => mouse::Interaction::Grabbing,
            Some(Drag::Range { .. }) => mouse::Interaction::Crosshair,
            _ if cursor.is_over(layout.bounds()) => mouse::Interaction::Crosshair,
            _ => mouse::Interaction::default(),
        }
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        r: &mut iced::Renderer,
        _theme: &Theme,
        _style: &renderer::Style,
        layout: Layout<'_>,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let t_all = Instant::now();
        let st = tree.state.downcast_ref::<State>();
        let b = layout.bounds();
        let app = self.app;
        let pal = &app.pal;
        r.fill_quad(
            Quad {
                bounds: b,
                ..Quad::default()
            },
            pal.c[BG],
        );
        let Some(cap) = &app.cap else {
            put(
                r,
                b,
                "ファイルを開いてください",
                Point::new(b.width / 2.0, b.height / 2.0),
                14.0,
                pal.dim(),
                Center,
            );
            return;
        };
        let f = frame_geo(b.size());
        let vp = view_of(app, self.wv, f.pw);
        let lanes = placed(app, self.wv, &f);
        // the time ruler's ticks
        let hz = cap.tick_hz;
        let step = nice_step(vp.tpp * 90.0 / hz);
        let mut grid = vec![];
        let mut ruler = vec![];
        let s0 = (vp.t0 / hz / step).floor() as i64;
        for k in s0..s0 + 64 {
            let s = k as f64 * step;
            let x = vp.x_of(s * hz);
            if x > f.pw {
                break;
            }
            if x >= 0.0 {
                grid.push(f.px + x);
                ruler.push((f.px + x, fmt_ruler(s, step)));
            }
        }
        let geo = Geo {
            px: f.px,
            pw: f.pw,
            top: f.top,
            bottom: f.bottom,
            mm: f.mm,
            char_px: CHAR_PX,
            tiles: app.tiles_on,
        };
        let hits = Hits {
            by_layer: &app.search.by_layer,
            ticks: &app.search.hit_ticks,
        };
        let mut sc = self.wv.cache.borrow_mut();
        let t_scene = Instant::now();
        scene::frame(
            &mut sc,
            cap,
            app.analysis.as_ref(),
            &hits,
            &lanes,
            &vp,
            &geo,
            app.sel,
            &grid,
        );
        // the markers, the cursor, a range being chosen
        let out = &mut sc.out;
        out.set_clip(f.px, f.mm[1], f.px + f.pw, f.bottom);
        for (k, m) in app.markers.iter().enumerate() {
            if let Some(t) = m {
                let x = f.px + vp.x_of(*t);
                out.rect(x - 0.75, f.ruler_y, x + 0.75, f.bottom, MARKER1 + k, 1.0);
            }
        }
        if let Some(h) = app.hover.filter(|h| h.wave == self.wv.id) {
            let x = f.px + vp.x_of(h.t);
            out.rect(x - 0.5, f.top, x + 0.5, f.bottom, DIM, 0.6);
        }
        if let Some(Drag::Range { x0, x1 }) = st.drag {
            out.rect(x0.min(x1), f.top, x0.max(x1), f.bottom, ACCENT, 0.15);
        }
        let n_inst = out.inst.len() / 8;
        let scene_ms = t_scene.elapsed().as_secs_f64() * 1e3;
        let prim = WavePrim {
            key: self.wv.id as u64,
            inst: Arc::new(out.inst.clone()),
            pal: pal.gpu(),
        };
        r.draw_primitive(b, prim);

        // text above the shapes (iced draws a layer's text after its primitives)
        let t_text = Instant::now();
        let plot = Rectangle::new(
            Point::new(b.x + f.px, b.y + f.top),
            Size::new(f.pw, (f.bottom - f.top).max(0.0)),
        );
        for l in &out.labels {
            let txt = out.label_text(l);
            let clip = if l.y < f.top {
                Rectangle::new(Point::new(b.x + f.px, b.y), Size::new(f.pw, f.top))
            } else {
                plot
            };
            put_clip(
                r,
                txt,
                Point::new(b.x + l.x, b.y + l.y),
                ANN_FONT,
                pal.c[l.color],
                Center,
                clip,
            );
        }
        let n_labels = out.labels.len();
        for (x, s) in &ruler {
            put(
                r,
                b,
                s,
                Point::new(x + 3.0, f.ruler_y + RULER_H / 2.0),
                10.5,
                pal.dim(),
                Left,
            );
        }
        // the lanes' names
        let names = Rectangle::new(
            Point::new(b.x, b.y + f.top),
            Size::new(f.px - 4.0, (f.bottom - f.top).max(0.0)),
        );
        for &(lane, y, h, _) in &lanes {
            if y + h < f.top || y > f.bottom {
                continue;
            }
            let (name, color, size) = match lane {
                Lane::Dig(i) => (cap.lines[i].name.clone(), pal.text(), 12.5),
                Lane::Ana(i) => (
                    format!("{} ({})", cap.analog[i].name, cap.analog[i].unit),
                    pal.c[ANALOG],
                    12.0,
                ),
                Lane::Ann { claim, layer } => match &app.analysis {
                    Some(a) => {
                        let c = &a.claims[claim];
                        let l = &c.layers[layer];
                        let short = l.name.split(' ').next().unwrap_or("");
                        (
                            format!("{} {short}", c.protocol),
                            if layer == 0 { pal.verdict(&c.verdict) } else { pal.dim() },
                            10.5,
                        )
                    }
                    None => continue,
                },
                Lane::Unread { .. } => continue,
            };
            put_clip(
                r,
                &name,
                Point::new(b.x + 8.0, b.y + y + h / 2.0),
                size,
                color,
                Left,
                names,
            );
        }
        // the summary still being built
        let done = cap.lod_done.load(std::sync::atomic::Ordering::Relaxed);
        if done < cap.lines.len() {
            let built: f64 = cap
                .lines
                .iter()
                .map(|l| l.lod.read().unwrap().built_until as f64)
                .sum::<f64>()
                / (cap.lines.len().max(1) as f64 * cap.n_ticks.max(1) as f64);
            put(
                r,
                b,
                &format!("要約 {:.0}%", built * 100.0),
                Point::new(b.width - 8.0, f.ruler_y + RULER_H / 2.0),
                11.0,
                pal.accent(),
                Right,
            );
        }
        // the markers' labels and their difference
        for (k, m) in app.markers.iter().enumerate() {
            if let Some(t) = m {
                let x = f.px + vp.x_of(*t);
                if x >= f.px && x <= f.px + f.pw {
                    put(
                        r,
                        b,
                        &format!("M{}", k + 1),
                        Point::new(x + 3.0, f.ruler_y + 2.0),
                        10.5,
                        pal.c[MARKER1 + k],
                        LeftTop,
                    );
                }
            }
        }
        let text_ms = t_text.elapsed().as_secs_f64() * 1e3;
        // the cursor's box: the pulse under it, or the annotation's full text (a layer of its own, above the rest)
        if let Some(h) = app.hover.filter(|h| h.wave == self.wv.id) {
            let mut lines: Vec<String> = vec![fmt_time(h.t / hz)];
            match h.lane {
                Some(Lane::Dig(i)) if app.measure.as_ref().is_some_and(|m| m.line == i) => {
                    lines.extend(app.measure.as_ref().unwrap().text(hz).lines().map(String::from));
                }
                Some(Lane::Ann { claim, layer }) => {
                    if let Some(a) = &app.analysis {
                        let tr = &a.claims[claim].layers[layer].track;
                        if let Some(i) = tr.hit(h.t as i64) {
                            let it = tr.item(i);
                            lines.push(tr.text(i).to_string());
                            lines.push(format!(
                                "{} 〜 {}",
                                fmt_time(it.s as f64 / hz),
                                fmt_time(it.e as f64 / hz)
                            ));
                        }
                    }
                }
                _ => {}
            }
            let w = lines.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f32 * 7.2 + 16.0;
            let hh = lines.len() as f32 * 16.0 + 8.0;
            let mut x = h.p.x + 14.0;
            if x + w > b.width {
                x = h.p.x - 14.0 - w;
            }
            let y = (h.p.y + 14.0).min(b.height - hh);
            let bx = Rectangle::new(Point::new(b.x + x, b.y + y), Size::new(w, hh));
            r.with_layer(b, |r| {
                r.fill_quad(
                    Quad {
                        bounds: bx,
                        border: iced::Border {
                            color: pal.border,
                            width: 1.0,
                            radius: 6.0.into(),
                        },
                        ..Quad::default()
                    },
                    Color { a: 0.96, ..pal.panel },
                );
                for (i, s) in lines.iter().enumerate() {
                    put(
                        r,
                        b,
                        s,
                        Point::new(x + 8.0, y + 4.0 + i as f32 * 16.0),
                        12.0,
                        if i == 0 { pal.dim() } else { pal.text() },
                        LeftTop,
                    );
                }
            });
        }
        let mut perf = app.perf.borrow_mut();
        perf.acc[P_SCENE] += scene_ms;
        perf.acc[P_TEXT] += text_ms;
        perf.acc[P_WAVE] += t_all.elapsed().as_secs_f64() * 1e3;
        perf.wave(scene_ms as f32, sc.shapes, n_inst, n_labels);
        if perf.drawn_at.is_none() {
            perf.drawn_at = Some(cap.opened.elapsed().as_secs_f64());
        }
    }
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    LeftTop,
    Center,
    Right,
}
use Align::*;

fn put(r: &mut iced::Renderer, b: Rectangle, s: &str, p: Point, size: f32, color: Color, a: Align) {
    put_clip(r, s, Point::new(b.x + p.x, b.y + p.y), size, color, a, b);
}

/// A line of text at `p` (screen px), anchored by `a`.
fn put_clip(r: &mut iced::Renderer, s: &str, p: Point, size: f32, color: Color, a: Align, clip: Rectangle) {
    let (ax, ay) = match a {
        Left => (text::Alignment::Left, iced::alignment::Vertical::Center),
        LeftTop => (text::Alignment::Left, iced::alignment::Vertical::Top),
        Center => (text::Alignment::Center, iced::alignment::Vertical::Center),
        Right => (text::Alignment::Right, iced::alignment::Vertical::Center),
    };
    r.fill_text(
        text::Text {
            content: s.to_string(),
            bounds: Size::new(f32::INFINITY, size * 1.5),
            size: Pixels(size),
            line_height: text::LineHeight::default(),
            font: Font::DEFAULT,
            align_x: ax,
            align_y: ay,
            shaping: text::Shaping::Advanced,
            wrapping: text::Wrapping::None,
        },
        p,
        color,
        clip,
    );
}
