//! The waveform view: one Makepad widget. scene.rs makes the frame (prototype 3's instances: 8 floats, a rectangle or a
//! thick segment, a palette index); here each instance goes to `DrawSeg`, a shader of our own in Makepad's shader
//! language (a DrawQuad whose vertex shader places the quad from the instance), all of them in one batch
//! (`begin_many_instances`: Makepad appends the instance data to one draw call). Text by Makepad's DrawText above.
//! The mouse: zoom, pan with a fling, a range to zoom to, the minimap, a click on an annotation, the markers, keys.

use crate::scene::{self, Geo, Hits, Lane};
use crate::state::*;
use crate::theme::*;
use makepad_widgets::*;
use std::time::Instant;
use view_core::Viewport;

live_design! {
    use link::theme::*;
    use link::shaders::*;
    use link::widgets::*;

    pub DrawSeg = {{DrawSeg}} {
        // seg: x0 y0 x1 y1 (local px), kw: (kind 0 rectangle / 1 segment, width), rect_pos: the widget's origin
        fn vertex(self) -> vec4 {
            let k = self.geom_pos;
            let p = mix(self.seg.xy, self.seg.zw, k);
            if self.kw.x > 0.5 {
                let d = self.seg.zw - self.seg.xy;
                let len = max(length(d), 0.001);
                let nrm = vec2(-d.y, d.x) / len * (self.kw.y * 0.5);
                p = mix(self.seg.xy, self.seg.zw, k.x) + nrm * (k.y * 2.0 - 1.0);
            }
            let q = clamp(p + self.rect_pos, self.draw_clip.xy, self.draw_clip.zw);
            self.pos = k;
            self.world = self.view_transform * vec4(q.x, q.y, self.draw_depth + self.draw_zbias, 1.0);
            return self.camera_projection * (self.camera_view * self.world);
        }
        fn pixel(self) -> vec4 {
            return vec4(self.color.rgb * self.color.w, self.color.w);
        }
    }

    pub DrawTip = {{DrawTip}} {
        fn pixel(self) -> vec4 {
            let sdf = Sdf2d::viewport(self.pos * self.rect_size);
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 5.0);
            sdf.fill_keep(self.color);
            sdf.stroke(self.border, 1.0);
            return sdf.result;
        }
    }

    pub WaveViewBase = {{WaveView}} {}
    pub WaveView = <WaveViewBase> {
        width: Fill, height: Fill
        draw_text: { text_style: <THEME_FONT_REGULAR> { font_size: 8.0 } }
        draw_tip_text: { text_style: <THEME_FONT_REGULAR> { font_size: 8.5 } }
    }
}

#[derive(Live, LiveHook, LiveRegister)]
#[repr(C)]
pub struct DrawSeg {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    seg: Vec4,
    #[live]
    color: Vec4,
    #[live]
    kw: Vec2,
}

#[derive(Live, LiveHook, LiveRegister)]
#[repr(C)]
pub struct DrawTip {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4,
    #[live]
    border: Vec4,
}

pub const LABEL_W: f64 = 112.0;
const MM_H: f64 = 30.0;
const RULER_H: f64 = 18.0;
const DIG_H: f32 = 26.0;
const ANN_H: f32 = 18.0;
const ANA_H: f32 = 56.0;
const UNREAD_H: f32 = 6.0;
/// the annotations' text: px a character (for folding what does not fit)
const CHAR_PX: f32 = 6.6;

pub fn lanes(st: &St) -> Vec<(Lane, f32)> {
    let mut out = vec![];
    let Some(cap) = &st.cap else { return out };
    let claims = st.analysis.as_ref().map(|a| &a.claims[..]).unwrap_or(&[]);
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
#[derive(Clone, Copy)]
pub struct Frame {
    pub px: f64,
    pub pw: f64,
    pub top: f64,
    pub bottom: f64,
    pub mm: [f64; 4],
    pub ruler_y: f64,
}

pub fn frame_geo(size: DVec2) -> Frame {
    let px = LABEL_W;
    let pw = (size.x - LABEL_W - 4.0).max(10.0);
    Frame {
        px,
        pw,
        top: MM_H + 6.0 + RULER_H,
        bottom: size.y,
        mm: [px, 2.0, pw, MM_H],
        ruler_y: MM_H + 4.0,
    }
}

/// The lanes placed: (lane, y, h, alternate).
fn placed(st: &St, scroll_y: f64, f: &Frame) -> Vec<(Lane, f32, f32, bool)> {
    let mut y = (f.top - scroll_y) as f32;
    let mut out = vec![];
    let mut k = 0;
    for (lane, h) in lanes(st) {
        let alt = matches!(lane, Lane::Dig(_) | Lane::Ana(_)) && {
            k += 1;
            k % 2 == 0
        };
        out.push((lane, y, h, alt));
        y += h;
    }
    out
}

fn view_of(st: &St, ws: &WaveState, width: f32) -> Viewport {
    let n = st.n_ticks();
    match ws.vp {
        Some(v) if (v.width - width).abs() < 0.5 => v,
        Some(v) => Viewport { width, ..v },
        None => Viewport::fit(n, width),
    }
}

#[derive(Clone, Copy)]
enum Drag {
    Pan {
        x0: f64,
        t0: f64,
        last: (f64, Instant),
        vel: f64,
        moved: bool,
    },
    Range {
        x0: f64,
        x1: f64,
    },
    Minimap,
}

#[derive(Live, LiveHook, Widget)]
pub struct WaveView {
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_seg: DrawSeg,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_tip: DrawTip,
    #[live]
    draw_tip_text: DrawText,
    #[rust]
    drag: Option<Drag>,
    #[rust]
    last_click: Option<(Instant, f64)>,
    #[rust]
    rect: Rect,
}

#[derive(Clone, Copy)]
enum Anchor {
    Left,
    LeftTop,
    Center,
    Right,
}

impl WaveView {
    /// A line of text at `p` (absolute px) anchored by `a`.
    fn put(dt: &mut DrawText, cx: &mut Cx2d, s: &str, p: DVec2, color: Vec4, a: Anchor) {
        dt.color = color;
        let lt = dt.layout(cx, 0.0, 0.0, None, false, Align::default(), s);
        let (w, h) = (lt.size_in_lpxs.width as f64, lt.size_in_lpxs.height as f64);
        let pos = match a {
            Anchor::Left => dvec2(p.x, p.y - h / 2.0),
            Anchor::LeftTop => p,
            Anchor::Center => dvec2(p.x - w / 2.0, p.y - h / 2.0),
            Anchor::Right => dvec2(p.x - w, p.y - h / 2.0),
        };
        dt.draw_abs(cx, pos, s);
    }

    fn local(&self, abs: DVec2) -> DVec2 {
        abs - self.rect.pos
    }
}

impl Widget for WaveView {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let t_all = Instant::now();
        let rect = cx.walk_turtle(walk);
        self.rect = rect;
        let Some(st) = scope.data.get_mut::<St>() else {
            return DrawStep::done();
        };
        let pal = st.pal;
        self.draw_bg.color = pal.c[BG];
        self.draw_bg.draw_abs(cx, rect);
        let Some(id) = st.panel_of(&scope.path.data) else {
            return DrawStep::done();
        };
        let o = rect.pos;
        if st.cap.is_none() {
            Self::put(
                &mut self.draw_text,
                cx,
                "ファイルを開いてください",
                o + rect.size * 0.5,
                pal.dim(),
                Anchor::Center,
            );
            return DrawStep::done();
        }
        let f = frame_geo(rect.size);
        // the view follows the widget's width (the first frame: the whole capture)
        let (vp, scroll_y) = {
            let tmp = match st.panels.get(&id) {
                Some(Panel::Wave(w)) => (view_of(st, w, f.pw as f32), w.scroll_y),
                _ => return DrawStep::done(),
            };
            if let Some(w) = st.wave_mut(id)
                && w.vp.is_none_or(|v| (v.width - f.pw as f32).abs() >= 0.5)
            {
                w.vp = Some(tmp.0);
            }
            tmp
        };
        if st.focus_wave.is_none() {
            st.focus_wave = Some(id);
        }
        let lanes = placed(st, scroll_y, &f);
        let cap = st.cap.as_ref().unwrap();
        let hz = cap.tick_hz;
        // the time ruler's ticks
        let step = nice_step(vp.tpp * 90.0 / hz);
        let mut grid = vec![];
        let mut ruler = vec![];
        let s0 = (vp.t0 / hz / step).floor() as i64;
        for k in s0..s0 + 64 {
            let s = k as f64 * step;
            let x = vp.x_of(s * hz) as f64;
            if x > f.pw {
                break;
            }
            if x >= 0.0 {
                grid.push((f.px + x) as f32);
                ruler.push((f.px + x, fmt_ruler(s, step)));
            }
        }
        let geo = Geo {
            px: f.px as f32,
            pw: f.pw as f32,
            top: f.top as f32,
            bottom: f.bottom as f32,
            mm: [f.mm[0] as f32, f.mm[1] as f32, f.mm[2] as f32, f.mm[3] as f32],
            char_px: CHAR_PX,
            tiles: st.tiles_on,
        };
        let t_scene = Instant::now();
        let c_scene = thread_cpu_ms();
        let mut sc = match st.panels.get_mut(&id) {
            Some(Panel::Wave(w)) => std::mem::take(&mut w.cache),
            _ => return DrawStep::done(),
        };
        {
            let hits = Hits {
                by_layer: &st.search.by_layer,
                ticks: &st.search.hit_ticks,
            };
            scene::frame(
                &mut sc,
                cap,
                st.analysis.as_ref(),
                &hits,
                &lanes,
                &vp,
                &geo,
                st.sel,
                &grid,
            );
        }
        // the markers, the cursor, a range being chosen
        let out = &mut sc.out;
        out.set_clip(f.px as f32, f.mm[1] as f32, (f.px + f.pw) as f32, f.bottom as f32);
        for (k, m) in st.markers.iter().enumerate() {
            if let Some(t) = m {
                let x = f.px as f32 + vp.x_of(*t);
                out.rect(x - 0.75, f.ruler_y as f32, x + 0.75, f.bottom as f32, MARKER1 + k, 1.0);
            }
        }
        let hover = st.hover.filter(|h| h.wave == id);
        if let Some(h) = hover {
            let x = f.px as f32 + vp.x_of(h.t);
            out.rect(x - 0.5, f.top as f32, x + 0.5, f.bottom as f32, DIM, 0.6);
        }
        if let Some(Drag::Range { x0, x1 }) = self.drag {
            out.rect(
                x0.min(x1) as f32,
                f.top as f32,
                x0.max(x1) as f32,
                f.bottom as f32,
                ACCENT,
                0.15,
            );
        }
        let scene_ms = t_scene.elapsed().as_secs_f64() * 1e3;
        let scene_cpu = thread_cpu_ms() - c_scene;
        // the instances: one batch of DrawSeg
        let t_inst = Instant::now();
        let n_inst = out.inst.len() / 8;
        self.draw_seg.rect_pos = vec2(o.x as f32, o.y as f32);
        self.draw_seg.draw_clip = vec4(
            o.x as f32,
            o.y as f32,
            (o.x + rect.size.x) as f32,
            (o.y + rect.size.y) as f32,
        );
        self.draw_seg.begin_many_instances(cx);
        for c in out.inst.chunks_exact(8) {
            self.draw_seg.seg = vec4(c[0], c[1], c[2], c[3]);
            self.draw_seg.color = with_alpha(pal.c[c[4] as usize & 15], c[5]);
            self.draw_seg.kw = vec2(c[6], c[7]);
            self.draw_seg.draw(cx);
        }
        self.draw_seg.end_many_instances(cx);
        let inst_ms = t_inst.elapsed().as_secs_f64() * 1e3;

        // text above the shapes: the annotations clipped to the plot
        let t_text = Instant::now();
        cx.begin_turtle(
            Walk::abs_rect(Rect {
                pos: o + dvec2(f.px, f.top),
                size: dvec2(f.pw, (f.bottom - f.top).max(0.0)),
            }),
            Layout::default(),
        );
        for l in &out.labels {
            if (l.y as f64) < f.top {
                continue;
            }
            let txt = out.label_text(l);
            Self::put(
                &mut self.draw_text,
                cx,
                txt,
                o + dvec2(l.x as f64, l.y as f64),
                pal.c[l.color],
                Anchor::Center,
            );
        }
        cx.end_turtle();
        let n_labels = out.labels.len();
        // the minimap's labels (above the plot)
        for l in &out.labels {
            if (l.y as f64) >= f.top {
                continue;
            }
            Self::put(
                &mut self.draw_text,
                cx,
                out.label_text(l),
                o + dvec2(l.x as f64, l.y as f64),
                pal.c[l.color],
                Anchor::Center,
            );
        }
        for (x, s) in &ruler {
            Self::put(
                &mut self.draw_text,
                cx,
                s,
                o + dvec2(x + 3.0, f.ruler_y + RULER_H / 2.0),
                pal.dim(),
                Anchor::Left,
            );
        }
        // the lanes' names
        cx.begin_turtle(
            Walk::abs_rect(Rect {
                pos: o + dvec2(0.0, f.top),
                size: dvec2(f.px - 4.0, (f.bottom - f.top).max(0.0)),
            }),
            Layout::default(),
        );
        for &(lane, y, h, _) in &lanes {
            if ((y + h) as f64) < f.top || y as f64 > f.bottom {
                continue;
            }
            let (name, color) = match lane {
                Lane::Dig(i) => (cap.lines[i].name.clone(), pal.text()),
                Lane::Ana(i) => (
                    format!("{} ({})", cap.analog[i].name, cap.analog[i].unit),
                    pal.c[ANALOG],
                ),
                Lane::Ann { claim, layer } => match &st.analysis {
                    Some(a) => {
                        let c = &a.claims[claim];
                        let l = &c.layers[layer];
                        let short = l.name.split(' ').next().unwrap_or("");
                        (
                            format!("{} {short}", c.protocol),
                            if layer == 0 { pal.verdict(&c.verdict) } else { pal.dim() },
                        )
                    }
                    None => continue,
                },
                Lane::Unread { .. } => continue,
            };
            Self::put(
                &mut self.draw_text,
                cx,
                &name,
                o + dvec2(8.0, (y + h / 2.0) as f64),
                color,
                Anchor::Left,
            );
        }
        cx.end_turtle();
        // the summary still being built
        let done = cap.lod_done.load(std::sync::atomic::Ordering::Relaxed);
        if done < cap.lines.len() {
            let built: f64 = cap
                .lines
                .iter()
                .map(|l| l.lod.read().unwrap().built_until as f64)
                .sum::<f64>()
                / (cap.lines.len().max(1) as f64 * cap.n_ticks.max(1) as f64);
            Self::put(
                &mut self.draw_text,
                cx,
                &format!("要約 {:.0}%", built * 100.0),
                o + dvec2(rect.size.x - 8.0, f.ruler_y + RULER_H / 2.0),
                pal.accent(),
                Anchor::Right,
            );
        }
        for (k, m) in st.markers.iter().enumerate() {
            if let Some(t) = m {
                let x = f.px + vp.x_of(*t) as f64;
                if x >= f.px && x <= f.px + f.pw {
                    Self::put(
                        &mut self.draw_text,
                        cx,
                        &format!("M{}", k + 1),
                        o + dvec2(x + 3.0, f.ruler_y + 1.0),
                        pal.c[MARKER1 + k],
                        Anchor::LeftTop,
                    );
                }
            }
        }
        let text_ms = t_text.elapsed().as_secs_f64() * 1e3;
        // the cursor's box: the pulse under it, or the annotation's full text (a draw call of its own, above the rest)
        if let Some(h) = hover {
            let mut lines: Vec<String> = vec![fmt_time(h.t / hz)];
            match h.lane {
                Some(Lane::Dig(i)) if st.measure.as_ref().is_some_and(|m| m.line == i) => {
                    lines.extend(st.measure.as_ref().unwrap().text(hz).lines().map(String::from));
                }
                Some(Lane::Ann { claim, layer }) => {
                    if let Some(a) = &st.analysis {
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
            let w = lines.iter().map(|s| s.chars().count()).max().unwrap_or(0) as f64 * 7.4 + 18.0;
            let hh = lines.len() as f64 * 16.0 + 10.0;
            let mut x = h.p.x + 14.0;
            if x + w > rect.size.x {
                x = h.p.x - 14.0 - w;
            }
            let y = (h.p.y + 14.0).min(rect.size.y - hh);
            self.draw_tip.new_draw_call(cx);
            self.draw_tip.color = with_alpha(pal.panel, 0.96);
            self.draw_tip.border = pal.border;
            self.draw_tip.draw_abs(
                cx,
                Rect {
                    pos: o + dvec2(x, y),
                    size: dvec2(w, hh),
                },
            );
            cx.new_draw_call(&self.draw_tip_text.draw_vars);
            for (i, s) in lines.iter().enumerate() {
                Self::put(
                    &mut self.draw_tip_text,
                    cx,
                    s,
                    o + dvec2(x + 9.0, y + 5.0 + i as f64 * 16.0),
                    if i == 0 { pal.dim() } else { pal.text() },
                    Anchor::LeftTop,
                );
            }
        }
        if let Some(Panel::Wave(w)) = st.panels.get_mut(&id) {
            w.cache = sc;
        }
        let shapes = match st.panels.get(&id) {
            Some(Panel::Wave(w)) => w.cache.shapes,
            _ => 0,
        };
        let p = &mut st.perf;
        p.acc[P_SCENE] += scene_ms;
        p.acc[P_SCENE_CPU] += scene_cpu;
        p.acc[P_INST] += inst_ms;
        p.acc[P_TEXT] += text_ms;
        p.acc[P_WAVE] += t_all.elapsed().as_secs_f64() * 1e3;
        p.wave(scene_ms as f32, shapes, n_inst, n_labels);
        if p.first_paint.is_none() {
            p.first_paint = st.cap.as_ref().map(|c| c.opened.elapsed().as_secs_f64());
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let area = self.draw_bg.area();
        let hit = event.hits(cx, area);
        if matches!(hit, Hit::Nothing) {
            return;
        }
        let Some(st) = scope.data.get_mut::<St>() else { return };
        let Some(id) = st.panel_of(&scope.path.data) else {
            return;
        };
        if st.cap.is_none() {
            return;
        }
        let n = st.n_ticks();
        let f = frame_geo(self.rect.size);
        let (vp, scroll_y) = match st.panels.get(&id) {
            Some(Panel::Wave(w)) => (view_of(st, w, f.pw as f32), w.scroll_y),
            _ => return,
        };
        let set_vp = |st: &mut St, v: Viewport| {
            if let Some(w) = st.wave_mut(id) {
                w.vp = Some(v.clamped(n));
            }
        };
        let mut changed = true;
        match hit {
            Hit::FingerScroll(e) => {
                let p = self.local(e.abs);
                let dy = -e.scroll.y;
                if p.x < f.px && p.y > f.top {
                    let lanes_h: f32 = lanes(st).iter().map(|l| l.1).sum();
                    if let Some(w) = st.wave_mut(id) {
                        w.scroll_y = (w.scroll_y - dy).clamp(0.0, (lanes_h as f64 - 100.0).max(0.0));
                    }
                } else if e.modifiers.shift {
                    let mut v = vp;
                    v.t0 -= dy * vp.tpp;
                    set_vp(st, v);
                } else {
                    let factor = 1.0015f64.powf(dy);
                    set_vp(st, vp.zoomed(factor, ((p.x - f.px).clamp(0.0, f.pw)) as f32));
                }
            }
            Hit::FingerDown(e) => {
                cx.set_key_focus(area);
                st.focus_wave = Some(id);
                let p = self.local(e.abs);
                let in_mm = p.y <= f.mm[1] + f.mm[3] + 2.0 && p.x >= f.px;
                let btn = e.device.mouse_button().unwrap_or(MouseButton::PRIMARY);
                if btn.is_primary() && in_mm {
                    self.drag = Some(Drag::Minimap);
                    let t = (p.x - f.px) / f.pw * n as f64;
                    let mut v = vp;
                    v.t0 = t - vp.tpp * vp.width as f64 / 2.0;
                    set_vp(st, v);
                } else if p.x >= f.px && (btn.is_secondary() || btn.is_primary() && e.modifiers.shift) {
                    self.drag = Some(Drag::Range { x0: p.x, x1: p.x });
                } else if btn.is_primary() && p.x >= f.px {
                    self.drag = Some(Drag::Pan {
                        x0: p.x,
                        t0: vp.t0,
                        last: (p.x, Instant::now()),
                        vel: 0.0,
                        moved: false,
                    });
                    if let Some(w) = st.wave_mut(id) {
                        w.vel = 0.0;
                    }
                }
            }
            Hit::FingerMove(e) => {
                let p = self.local(e.abs);
                match &mut self.drag {
                    Some(Drag::Pan {
                        x0,
                        t0,
                        last,
                        vel,
                        moved,
                    }) => {
                        if (p.x - *x0).abs() > 3.0 {
                            *moved = true;
                        }
                        let mut v = vp;
                        v.t0 = *t0 - (p.x - *x0) * vp.tpp;
                        let now = Instant::now();
                        let dt = now.duration_since(last.1).as_secs_f64().max(1e-3);
                        let inst = -(p.x - last.0) * vp.tpp / dt;
                        *vel = *vel * 0.6 + inst * 0.4;
                        *last = (p.x, now);
                        set_vp(st, v);
                    }
                    Some(Drag::Range { x1, .. }) => *x1 = p.x.clamp(f.px, f.px + f.pw),
                    Some(Drag::Minimap) => {
                        let t = (p.x - f.px) / f.pw * n as f64;
                        let mut v = vp;
                        v.t0 = t - vp.tpp * vp.width as f64 / 2.0;
                        set_vp(st, v);
                    }
                    None => changed = false,
                }
            }
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let p = self.local(e.abs);
                if p.x >= f.px && p.y >= f.top {
                    let t = vp.t_of((p.x - f.px) as f32);
                    let lane = placed(st, scroll_y, &f)
                        .into_iter()
                        .find(|l| p.y >= l.1 as f64 && p.y < (l.1 + l.2) as f64)
                        .map(|l| l.0);
                    st.hover_at(id, Some((t, lane, p)));
                } else if st.hover.is_some_and(|h| h.wave == id) {
                    st.hover_at(id, None);
                } else {
                    changed = false;
                }
            }
            Hit::FingerHoverOut(_) => {
                if st.hover.is_some_and(|h| h.wave == id) {
                    st.hover_at(id, None);
                }
            }
            Hit::FingerUp(e) => {
                let p = self.local(e.abs);
                let btn = e.device.mouse_button().unwrap_or(MouseButton::PRIMARY);
                match self.drag.take() {
                    Some(Drag::Pan { moved: false, .. }) => {
                        // a click: an annotation chosen (twice: zoomed to it)
                        let now = Instant::now();
                        let double = self
                            .last_click
                            .is_some_and(|(t, x)| now.duration_since(t).as_millis() < 400 && (x - p.x).abs() < 4.0);
                        self.last_click = Some((now, p.x));
                        let t = vp.t_of((p.x - f.px) as f32);
                        let found = placed(st, scroll_y, &f)
                            .into_iter()
                            .find(|l| p.y >= l.1 as f64 && p.y < (l.1 + l.2) as f64);
                        if let Some((Lane::Ann { claim, layer }, ..)) = found
                            && let Some(a) = &st.analysis
                            && let Some(row) = a.claims[claim].layers[layer].track.hit(t as i64)
                        {
                            st.select(Sel { claim, layer, row }, Some(id));
                            if double {
                                st.zoom_to_sel(id);
                            }
                        }
                    }
                    Some(Drag::Pan { vel, last, .. }) => {
                        if last.1.elapsed().as_millis() < 80
                            && let Some(w) = st.wave_mut(id)
                        {
                            w.vel = vel;
                        }
                    }
                    Some(Drag::Range { x0, x1 }) => {
                        if (x1 - x0).abs() > 3.0 {
                            let (a, b2) = (x0.min(x1) - f.px, x0.max(x1) - f.px);
                            let mut v = vp;
                            v.t0 = vp.t_of(a as f32);
                            v.tpp = ((b2 - a) * vp.tpp / f.pw).max(1e-3);
                            set_vp(st, v);
                        } else if btn.is_secondary() {
                            st.marker_click(vp.t_of((x0 - f.px) as f32));
                        }
                    }
                    _ => {}
                }
            }
            Hit::KeyDown(ke) => {
                let hover_t = st.hover.filter(|h| h.wave == id).map(|h| h.t);
                let pan = |v: Viewport, k: f64| Viewport {
                    t0: v.t0 + v.tpp * v.width as f64 * k,
                    ..v
                };
                match ke.key_code {
                    KeyCode::KeyF => set_vp(st, Viewport::fit(n, vp.width)),
                    KeyCode::Equals | KeyCode::NumpadAdd => set_vp(st, vp.zoomed(2.0, vp.width / 2.0)),
                    KeyCode::Minus | KeyCode::NumpadSubtract => set_vp(st, vp.zoomed(0.5, vp.width / 2.0)),
                    KeyCode::ArrowLeft => set_vp(st, pan(vp, -0.2)),
                    KeyCode::ArrowRight => set_vp(st, pan(vp, 0.2)),
                    KeyCode::KeyN => st.jump_bad(!ke.modifiers.shift),
                    KeyCode::Key1 => st.markers[0] = Some(hover_t.unwrap_or(vp.t_of(vp.width / 2.0))),
                    KeyCode::Key2 => st.markers[1] = Some(hover_t.unwrap_or(vp.t_of(vp.width / 2.0))),
                    _ => changed = false,
                }
                if changed && let Some(w) = st.wave_mut(id) {
                    w.vel = 0.0;
                }
            }
            _ => changed = false,
        }
        if changed {
            cx.redraw_all();
        }
    }
}
