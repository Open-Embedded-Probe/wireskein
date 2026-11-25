//! The waveform panel: the minimap, the time ruler, digital lines (from view-core's summaries, by tiles), analog
//! bands, annotation lanes (3 layers a claim), the cursor's measure, markers, and the mouse and keys.

use crate::app::{Measure, Sel, Shared, TileCacheT};
use crate::theme::Pal;
use eframe::egui::{self, Align2, Color32, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Vec2, pos2};
use serde::{Deserialize, Serialize};
use view_core::Viewport;
use view_core::analog::AnalogView;
use view_core::ann::{AnnShape, Fit};
use view_core::digital::{LineView, around, read_view};
use view_core::shapes::{Seg, digital_segs};
use view_core::tiles::{placement, tile_viewport, tiles_for};
use web_time::Instant;

pub const LABEL_W: f32 = 132.0;
const MINIMAP_H: f32 = 26.0;
const RULER_H: f32 = 22.0;
const DIG_H: f32 = 26.0;
const ANN_H: f32 = 19.0;
const UNREAD_H: f32 = 6.0;
const ANA_H: f32 = 64.0;

#[derive(Default, Serialize, Deserialize)]
pub struct WaveTab {
    pub id: u32,
    pub follow: bool,
    #[serde(skip)]
    pub st: WaveState,
}

#[derive(Default)]
pub struct WaveState {
    pub vp: Option<Viewport>,
    /// inertia after a drag, ticks a second
    vel: f64,
    drag_hist: Vec<(f64, f32)>,
    select_from: Option<f32>,
    scroll_y: f32,
    seen_gen: u64,
    cap_gen: u64,
}

#[derive(Clone, Copy, PartialEq)]
enum Lane {
    Dig(usize),
    Ana(usize),
    Ann { claim: usize, layer: usize },
    Unread { claim: usize },
}

fn lanes(sh: &Shared) -> Vec<(Lane, f32)> {
    let mut out = vec![];
    let Some(cap) = &sh.cap else { return out };
    let claims = sh.analysis.as_ref().map(|a| &a.claims[..]).unwrap_or(&[]);
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

fn rect_x(mesh: &mut Mesh, x0: f32, x1: f32, y0: f32, y1: f32, c: Color32) {
    if x1 > x0 && y1 > y0 {
        mesh.add_colored_rect(Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)), c);
    }
}

/// `--old-draw`: the drawing as it was before the bench's breakdown (2026-11-25 §9.6), for comparing on one machine:
/// a rect pair a busy block and a rect an edge, the minimap made every frame, a clip rect a text.
pub static OLD_DRAW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn old_draw() -> bool {
    OLD_DRAW.load(std::sync::atomic::Ordering::Relaxed)
}

/// A line's shapes into the mesh, x = left + off + seg.x * scale, clipped to [cl, cr].
#[allow(clippy::too_many_arguments)]
fn draw_segs(mesh: &mut Mesh, segs: &[Seg], left: f32, off: f32, scale: f32, cl: f32, cr: f32, y_hi: f32, y_lo: f32, pal: &Pal) {
    let c = pal.line;
    let th = 1.5;
    let px = |x: f32| (left + off + x * scale).clamp(cl, cr);
    // Fewer shapes where a pixel holds many changes (the GPU's vertices and egui's tessellation follow the rects):
    // edges closer than 2 px are one solid block (the short runs between them are inside it), and a stretch of
    // busy blocks gets its two level lines once, not one pair a block.
    let merge = !old_draw();
    let mut edges: Option<(f32, f32, u32)> = None; // (first x, last x, how many)
    let mut busy: Option<(f32, f32)> = None; // the stretch's (a, b)
    let flush_edges = |mesh: &mut Mesh, e: &mut Option<(f32, f32, u32)>| {
        if let Some((a, b, k)) = e.take() {
            if k == 1 {
                rect_x(mesh, a - 0.5, a + 0.6, y_hi - th / 2.0, y_lo + th / 2.0, c);
            } else {
                rect_x(mesh, a - 0.5, b + 0.6, y_hi - th / 2.0, y_lo + th / 2.0, c);
            }
        }
    };
    let flush_busy = |mesh: &mut Mesh, s: &mut Option<(f32, f32)>| {
        if let Some((a, b)) = s.take() {
            rect_x(mesh, a, b, y_hi - th / 2.0, y_hi + th / 2.0, c);
            rect_x(mesh, a, b, y_lo - th / 2.0, y_lo + th / 2.0, c);
        }
    };
    for s in segs {
        match *s {
            Seg::Run { x0, x1, high } => {
                let (a, b) = (px(x0), px(x1).max(px(x0) + 0.5));
                if merge && edges.is_some() && b - a < 2.0 {
                    continue; // between close edges: inside the block
                }
                flush_edges(mesh, &mut edges);
                flush_busy(mesh, &mut busy);
                let y = if high { y_hi } else { y_lo };
                rect_x(mesh, a, b, y - th / 2.0, y + th / 2.0, c);
            }
            Seg::Edge { x } => {
                let x = px(x);
                if x > cl && x < cr {
                    flush_busy(mesh, &mut busy);
                    edges = match edges {
                        Some((a, b, k)) if merge && x - b < 2.0 => Some((a, x, k + 1)),
                        other => {
                            let mut o = other;
                            flush_edges(mesh, &mut o);
                            Some((x, x, 1))
                        }
                    };
                }
            }
            Seg::Busy { x0, x1, density } => {
                flush_edges(mesh, &mut edges);
                let (a, b) = (px(x0), px(x1).max(px(x0) + 1.0));
                let alpha = 0.30 + 0.65 * density;
                rect_x(mesh, a, b, y_hi, y_lo, c.gamma_multiply(alpha));
                busy = match busy {
                    Some((sa, sb)) if merge && a - sb < 0.5 => Some((sa, b.max(sb))),
                    other => {
                        let mut o = other;
                        flush_busy(mesh, &mut o);
                        Some((a, b))
                    }
                };
            }
            Seg::Unknown { x0, x1 } => {
                flush_edges(mesh, &mut edges);
                flush_busy(mesh, &mut busy);
                rect_x(mesh, px(x0), px(x1), y_hi, y_lo, pal.unread.gamma_multiply(0.25));
            }
        }
    }
    flush_edges(mesh, &mut edges);
    flush_busy(mesh, &mut busy);
}

/// The minimap's mesh: the changes a pixel, every claim's bad items, the search's hits.
fn minimap_mesh(sh: &Shared, mm: Rect, n: i64, fit: &Viewport, w: f32, pal: &Pal) -> Mesh {
    let act = &sh.minimap;
    let maxc = act.iter().copied().fold(1u64, u64::max) as f32;
    let mut mesh = Mesh::default();
    for (x, &c) in act.iter().enumerate() {
        if c > 0 {
            let h = (c as f32 / maxc).sqrt().clamp(0.08, 1.0) * (mm.height() - 4.0);
            rect_x(&mut mesh, mm.left() + x as f32, mm.left() + x as f32 + 1.0, mm.bottom() - 2.0 - h, mm.bottom() - 2.0, pal.line.gamma_multiply(0.55));
        }
    }
    // the bad items of each claim's coarsest layer, and the search's hits
    if let Some(a) = &sh.analysis {
        let mut shapes = vec![];
        for c in &a.claims {
            if let Some(l) = c.layers.first() {
                l.track.layout(fit, 7.0, &mut shapes);
                for s in &shapes {
                    let (x0, x1, st) = match *s {
                        AnnShape::Busy { x0, x1, status, .. } => (x0, x1, status),
                        AnnShape::Box { x0, x1, status, .. } => (x0, x1, status),
                    };
                    if st >= view_core::ann::Status::Warn {
                        rect_x(&mut mesh, mm.left() + x0, mm.left() + x1.max(x0 + 1.5), mm.top() + 1.0, mm.top() + 7.0, pal.status(st).gamma_multiply(2.0));
                    }
                }
            }
        }
    }
    for hit in sh.search.hit_ticks.iter() {
        let x = mm.left() + (*hit as f64 / n as f64 * w as f64) as f32;
        rect_x(&mut mesh, x, x + 1.0, mm.bottom() - 6.0, mm.bottom(), pal.accent);
    }
    mesh
}

/// A new capture: the view and the follow state start over.
pub fn sync(tab: &mut WaveTab, sh: &Shared) {
    if tab.st.cap_gen != sh.cgen {
        tab.st = WaveState { cap_gen: sh.cgen, seen_gen: sh.sel_gen, ..Default::default() };
    }
}

pub fn ui(ui: &mut egui::Ui, tab: &mut WaveTab, sh: &mut Shared, cache: &mut TileCacheT) {
    let t_start = Instant::now();
    let pal = sh.pal;
    // the bar above: follow, fit, tiles
    ui.horizontal(|ui| {
        ui.checkbox(&mut tab.follow, "一覧と連動").on_hover_text("一覧で選んだ項目へこの波形が動く");
        if ui.button("全体").on_hover_text("F").clicked() {
            tab.st.vp = None;
        }
        if ui.button("−").clicked()
            && let Some(vp) = &mut tab.st.vp
        {
            *vp = vp.zoomed(0.5, vp.width / 2.0);
        }
        if ui.button("+").clicked()
            && let Some(vp) = &mut tab.st.vp
        {
            *vp = vp.zoomed(2.0, vp.width / 2.0);
        }
        if ui.button("前の誤り").on_hover_text("Shift+N").clicked() {
            jump_bad(tab, sh, false);
        }
        if ui.button("次の誤り").on_hover_text("N").clicked() {
            jump_bad(tab, sh, true);
        }
        if let (Some(cap), Some(vp)) = (&sh.cap, &tab.st.vp) {
            ui.label(
                egui::RichText::new(format!("{} / px  ·  幅 {}", fmt_time(cap.secs(vp.tpp)), fmt_time(cap.secs(vp.tpp * vp.width as f64)))).color(pal.dim),
            );
        }
    });
    let Some(cap) = &sh.cap else {
        ui.centered_and_justified(|ui| ui.label("ファイルを開いてください（引数で渡すか、上の「開く」）"));
        return;
    };
    let n = cap.n_ticks;
    let avail = ui.available_rect_before_wrap();
    let (resp, painter) = ui.allocate_painter(avail.size(), Sense::click_and_drag());
    let rect = resp.rect;
    painter.rect_filled(rect, 0.0, pal.bg);
    let plot_l = rect.left() + LABEL_W;
    let plot_w = (rect.right() - plot_l).max(10.0);
    let mut vp = tab.st.vp.unwrap_or_else(|| Viewport::fit(n, plot_w));
    if (vp.width - plot_w).abs() > 0.5 {
        // keep the same time span when the panel's width changes
        let span = vp.tpp * vp.width as f64;
        vp = Viewport { t0: vp.t0, tpp: span / plot_w as f64, width: plot_w };
    }

    // follow the list's choice
    if tab.follow && sh.sel_gen != tab.st.seen_gen {
        tab.st.seen_gen = sh.sel_gen;
        if sh.sel_src != tab.id
            && let Some((s, e)) = sh.sel_span()
        {
            let span = vp.tpp * vp.width as f64;
            let w = (e - s) as f64;
            if w * 3.0 > span || w * 2000.0 < span {
                let tpp = (w * 4.0 / vp.width as f64).max(0.01);
                vp = Viewport { t0: s as f64 - w * 1.5, tpp, width: vp.width };
            } else {
                vp.t0 = (s as f64 + e as f64) / 2.0 - span / 2.0;
            }
        }
    }

    let top = rect.top();
    let mm = Rect::from_min_max(pos2(plot_l, top), pos2(rect.right(), top + MINIMAP_H));
    let ruler = Rect::from_min_max(pos2(plot_l, mm.bottom()), pos2(rect.right(), mm.bottom() + RULER_H));
    let lanes_top = ruler.bottom() + 2.0;
    let lanes_rect = Rect::from_min_max(pos2(rect.left(), lanes_top), rect.max);
    let plot = Rect::from_min_max(pos2(plot_l, lanes_top), rect.max);

    // ---------------------------------------------------------------- input
    let dt = ui.input(|i| i.stable_dt).min(0.1) as f64;
    let hover = resp.hover_pos();
    let in_plot = hover.is_some_and(|p| p.x >= plot_l);
    let shift = ui.input(|i| i.modifiers.shift);
    if let Some(p) = hover {
        let (sd, zd) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta()));
        if p.x < plot_l || p.y < lanes_top && p.y >= mm.bottom() && false {
            tab.st.scroll_y = (tab.st.scroll_y - sd.y).max(0.0);
        } else if p.y < mm.bottom() {
            // on the minimap: the wheel zooms about the view's center
            let f = (sd.y as f64 * 0.003).exp() * zd as f64;
            if f != 1.0 {
                vp = vp.zoomed(f, vp.width / 2.0);
            }
        } else {
            let f = (sd.y as f64 * 0.003).exp() * zd as f64;
            if f != 1.0 {
                vp = vp.zoomed(f, p.x - plot_l);
                tab.st.vel = 0.0;
            }
            if sd.x != 0.0 {
                vp.t0 -= sd.x as f64 * vp.tpp;
            }
        }
    }
    let press = ui.input(|i| i.pointer.press_origin());
    let on_mm = press.is_some_and(|p| mm.contains(p));
    if resp.dragged() && on_mm {
        if let Some(p) = resp.interact_pointer_pos() {
            let span = vp.tpp * vp.width as f64;
            let t = (p.x - plot_l) as f64 / plot_w as f64 * n as f64;
            vp.t0 = t - span / 2.0;
        }
    } else if resp.drag_started_by(egui::PointerButton::Secondary) || resp.drag_started_by(egui::PointerButton::Primary) && shift {
        tab.st.select_from = press.map(|p| p.x - plot_l);
    } else if resp.dragged() && tab.st.select_from.is_none() && in_plot {
        let d = resp.drag_delta().x;
        vp.t0 -= d as f64 * vp.tpp;
        tab.st.vel = 0.0;
        tab.st.drag_hist.push((ui.input(|i| i.time), d));
        if tab.st.drag_hist.len() > 6 {
            tab.st.drag_hist.remove(0);
        }
    }
    if resp.drag_stopped() {
        if let Some(x0) = tab.st.select_from.take()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let x1 = p.x - plot_l;
            let (a, b) = (x0.min(x1), x0.max(x1));
            if b - a > 4.0 {
                let (ta, tb) = (vp.t_of(a), vp.t_of(b));
                vp = Viewport { t0: ta, tpp: (tb - ta) / vp.width as f64, width: vp.width };
            }
        } else if !on_mm && tab.st.drag_hist.len() >= 2 {
            let (t0, _) = tab.st.drag_hist[0];
            let (t1, _) = *tab.st.drag_hist.last().unwrap();
            let sum: f32 = tab.st.drag_hist.iter().skip(1).map(|x| x.1).sum();
            if t1 > t0 {
                tab.st.vel = -(sum as f64) / (t1 - t0) * vp.tpp;
            }
        }
        tab.st.drag_hist.clear();
    }
    if tab.st.vel != 0.0 && !resp.dragged() {
        vp.t0 += tab.st.vel * dt;
        tab.st.vel *= (-dt * 4.0).exp();
        if (tab.st.vel / vp.tpp).abs() < 5.0 {
            tab.st.vel = 0.0;
        }
        ui.ctx().request_repaint();
    }
    // keys while the pointer is over the panel
    let mut want_jump = None;
    if hover.is_some() {
        let (kf, k1, k2, kn, kl, kr, kp, km) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::Num1),
                i.key_pressed(egui::Key::Num2),
                i.key_pressed(egui::Key::N),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals),
                i.key_pressed(egui::Key::Minus),
            )
        });
        let no_text = !ui.ctx().egui_wants_keyboard_input();
        if no_text {
            if kf {
                vp = Viewport::fit(n, plot_w);
            }
            if let Some(p) = hover.filter(|_| in_plot) {
                let t = vp.t_of(p.x - plot_l);
                if k1 {
                    sh.markers[0] = Some(t);
                }
                if k2 {
                    sh.markers[1] = Some(t);
                }
            }
            if kn {
                want_jump = Some(!shift);
            }
            let span = vp.tpp * vp.width as f64;
            if kl {
                vp.t0 -= span * 0.2;
            }
            if kr {
                vp.t0 += span * 0.2;
            }
            if kp {
                vp = vp.zoomed(2.0, vp.width / 2.0);
            }
            if km {
                vp = vp.zoomed(0.5, vp.width / 2.0);
            }
        }
    }
    let vp = vp.clamped(n);
    tab.st.vp = Some(vp);

    // ---------------------------------------------------------------- minimap
    painter.rect_filled(mm, 2.0, pal.lane_alt);
    let t_mm = Instant::now();
    {
        let w = mm.width().max(1.0);
        let fit = Viewport::fit(n, w);
        crate::app::minimap(cap, sh.cgen, &mut sh.minimap_key, &mut sh.minimap, w as usize);
        let key = [
            sh.minimap_key.0,
            sh.minimap_key.1 as u64,
            sh.minimap_key.2 as u64,
            sh.analysis.as_ref().map(|a| a.seconds.to_bits()).unwrap_or(0),
            sh.analysis.as_ref().map(|a| a.claims.len() as u64 + 1).unwrap_or(0),
            sh.search.hit_ticks.len() as u64,
            sh.search.hit_ticks.first().copied().unwrap_or(-1) as u64,
            mm.left().to_bits() as u64,
            mm.top().to_bits() as u64,
            sh.dark as u64,
        ];
        if old_draw() || sh.minimap_mesh.as_ref().is_none_or(|m| m.0 != key) {
            sh.minimap_mesh = Some((key, std::sync::Arc::new(minimap_mesh(sh, mm, n, &fit, w, &pal))));
        }
        painter.add(Shape::Mesh(sh.minimap_mesh.as_ref().unwrap().1.clone()));
        let a = mm.left() + (vp.t0 / n as f64 * w as f64) as f32;
        let b = mm.left() + (vp.t1() / n as f64 * w as f64) as f32;
        let r = Rect::from_min_max(pos2(a.max(mm.left()), mm.top()), pos2(b.max(a + 2.0).min(mm.right()), mm.bottom()));
        painter.rect_filled(r, 2.0, pal.accent.gamma_multiply(0.18));
        painter.rect_stroke(r, 2.0, Stroke::new(1.0, pal.accent), egui::StrokeKind::Inside);
    }
    sh.perf.acc[crate::app::P_MINIMAP] += t_mm.elapsed().as_secs_f64() * 1e3;
    painter.text(pos2(rect.left() + 6.0, mm.center().y), Align2::LEFT_CENTER, &cap.title, FontId::proportional(12.0), pal.text);

    // ---------------------------------------------------------------- ruler
    {
        let secs_pp = vp.tpp / cap.tick_hz;
        let step = nice_step(secs_pp * 90.0);
        let s0 = cap.secs(vp.t0);
        let mut k = (s0 / step).floor();
        let font = FontId::monospace(10.5);
        loop {
            let s = k * step;
            let x = plot_l + ((s * cap.tick_hz - vp.t0) / vp.tpp) as f32;
            if x > rect.right() {
                break;
            }
            if x >= plot_l {
                painter.line_segment([pos2(x, ruler.bottom() - 6.0), pos2(x, ruler.bottom())], Stroke::new(1.0, pal.dim));
                painter.line_segment([pos2(x, lanes_top), pos2(x, rect.bottom())], Stroke::new(1.0, pal.grid.gamma_multiply(0.6)));
                painter.text(pos2(x + 3.0, ruler.top() + 2.0), Align2::LEFT_TOP, fmt_ruler(s, step), font.clone(), pal.dim);
            }
            k += 1.0;
        }
    }

    // ---------------------------------------------------------------- lanes
    let lanes = lanes(sh);
    let total_h: f32 = lanes.iter().map(|l| l.1).sum();
    tab.st.scroll_y = tab.st.scroll_y.min((total_h - plot.height() + 20.0).max(0.0));
    let lane_painter = painter.with_clip_rect(lanes_rect);
    let plot_painter = painter.with_clip_rect(plot);
    let mut mesh = Mesh::default();
    let mut y = lanes_top - tab.st.scroll_y;
    let mut lv = LineView::default();
    let mut segs = vec![];
    let mut ann_shapes = vec![];
    let mut n_shapes = 0usize;
    let font = FontId::monospace(11.0);
    let char_px = 6.7;
    let hover_t = hover.filter(|_| in_plot).map(|p| vp.t_of(p.x - plot_l));
    let mut hovered_lane: Option<(Lane, Rect)> = None;
    let mut texts: Vec<(Rect, String, Color32)> = vec![];
    let analysis = sh.analysis.as_ref();
    for (li, &(lane, h)) in lanes.iter().enumerate() {
        let lr = Rect::from_min_max(pos2(rect.left(), y), pos2(rect.right(), y + h));
        y += h;
        if lr.bottom() < lanes_top || lr.top() > rect.bottom() {
            continue;
        }
        if li % 2 == 1 && matches!(lane, Lane::Dig(_) | Lane::Ana(_)) {
            rect_x(&mut mesh, lr.left(), lr.right(), lr.top(), lr.bottom(), pal.lane_alt);
        }
        if hover.is_some_and(|p| lr.contains(p)) {
            hovered_lane = Some((lane, lr));
        }
        let pr = Rect::from_min_max(pos2(plot_l, lr.top()), lr.max);
        match lane {
            Lane::Dig(i) => {
                let line = &cap.lines[i];
                lane_painter.text(pos2(rect.left() + 8.0, lr.center().y), Align2::LEFT_CENTER, &line.name, FontId::proportional(12.5), pal.text);
                let lod = line.lod.read().unwrap();
                let (y_hi, y_lo) = (pr.top() + 5.0, pr.bottom() - 5.0);
                let complete = lod.built_until >= n;
                if sh.tiles_on {
                    for key in tiles_for(&vp, n) {
                        let make = || {
                            let tv = tile_viewport(key);
                            let mut lv = LineView::default();
                            read_view(line.src.as_ref(), &lod, &tv, 256, &mut lv);
                            let mut s = vec![];
                            digital_segs(&lv, &tv, &mut s);
                            s
                        };
                        let (off, scale) = placement(key, &vp);
                        if complete {
                            let segs = cache.get_or_make((sh.cgen, i), key, make);
                            n_shapes += segs.len();
                            draw_segs(&mut mesh, segs, plot_l, off, scale, plot_l, rect.right(), y_hi, y_lo, &pal);
                        } else {
                            let segs = make();
                            n_shapes += segs.len();
                            draw_segs(&mut mesh, &segs, plot_l, off, scale, plot_l, rect.right(), y_hi, y_lo, &pal);
                        }
                    }
                } else {
                    read_view(line.src.as_ref(), &lod, &vp, vp.width as usize, &mut lv);
                    digital_segs(&lv, &vp, &mut segs);
                    n_shapes += segs.len();
                    draw_segs(&mut mesh, &segs, plot_l, 0.0, 1.0, plot_l, rect.right(), y_hi, y_lo, &pal);
                }
                if !complete {
                    let pct = lod.built_until as f64 / n.max(1) as f64 * 100.0;
                    lane_painter.text(
                        pos2(rect.left() + 8.0, lr.bottom() - 2.0),
                        Align2::LEFT_BOTTOM,
                        format!("要約 {pct:.0}%"),
                        FontId::proportional(9.5),
                        pal.dim,
                    );
                }
            }
            Lane::Ana(i) => {
                let a = &cap.analog[i];
                lane_painter.text(
                    pos2(rect.left() + 8.0, lr.center().y),
                    Align2::LEFT_CENTER,
                    format!("{} [{}]", a.name, a.unit),
                    FontId::proportional(12.5),
                    pal.text,
                );
                let (lo, hi) = (a.min, a.max.max(a.min + 1e-6));
                let ymap = |v: f32| pr.bottom() - 4.0 - (v - lo) / (hi - lo) * (pr.height() - 8.0);
                match a.read_view(&vp) {
                    AnalogView::Points(pts) => {
                        let ps: Vec<Pos2> = pts.iter().map(|&(x, v)| pos2(plot_l + x, ymap(v))).collect();
                        n_shapes += ps.len();
                        plot_painter.add(Shape::line(ps, Stroke::new(1.5, pal.analog)));
                    }
                    AnalogView::Bands(bs) => {
                        let mut mean = vec![];
                        for (x, b) in bs.iter().enumerate() {
                            if let Some(b) = b {
                                let x = plot_l + x as f32;
                                rect_x(&mut mesh, x, x + 1.0, ymap(b.max) - 0.5, ymap(b.min) + 0.5, pal.analog.gamma_multiply(0.45));
                                mean.push(pos2(x + 0.5, ymap(b.mean)));
                            }
                        }
                        n_shapes += bs.len();
                        plot_painter.add(Shape::line(mean, Stroke::new(1.0, pal.analog)));
                    }
                }
            }
            Lane::Ann { claim, layer } => {
                let Some(c) = analysis.and_then(|a| a.claims.get(claim)) else {
                    continue;
                };
                let l = &c.layers[layer];
                let name = if layer == 0 { format!("{} {}", c.protocol, l.name) } else { format!("  {}", l.name) };
                lane_painter.text(pos2(rect.left() + 14.0, lr.center().y), Align2::LEFT_CENTER, name, FontId::proportional(11.0), pal.dim);
                if layer == 0 {
                    // the claim's verdict as a dot before its name
                    lane_painter.circle_filled(pos2(rect.left() + 7.0, lr.center().y), 3.5, pal.verdict(&c.verdict));
                }
                l.track.layout(&vp, char_px, &mut ann_shapes);
                n_shapes += ann_shapes.len();
                let hits = sh.search.by_layer.get(&(claim, layer));
                let (y0, y1) = (pr.top() + 1.5, pr.bottom() - 1.5);
                for s in &ann_shapes {
                    match *s {
                        AnnShape::Box { x0, x1, idx, fit, status } => {
                            let (a, b) = ((plot_l + x0).max(plot_l), (plot_l + x1).min(rect.right()));
                            let r = Rect::from_min_max(pos2(a, y0), pos2(b, y1));
                            let base = if status == view_core::ann::Status::Ok { pal.ok } else { pal.status(status) };
                            rect_x(&mut mesh, a, b, y0, y1, base);
                            rect_x(&mut mesh, a, a + 1.0, y0, y1, pal.text.gamma_multiply(0.35));
                            let selected = sh.sel == Some(Sel { claim, layer, row: idx as usize });
                            let hit = hits.is_some_and(|h| h.binary_search(&(idx as usize)).is_ok());
                            if hit {
                                plot_painter.rect_stroke(r, 2.0, Stroke::new(2.0, pal.accent), egui::StrokeKind::Inside);
                            }
                            if selected {
                                plot_painter.rect_stroke(r.expand(1.0), 2.0, Stroke::new(2.0, pal.sel), egui::StrokeKind::Outside);
                            }
                            let t = match fit {
                                Fit::Full => Some(l.track.text(idx as usize).to_string()),
                                Fit::Short => Some(l.track.short(idx as usize).to_string()),
                                Fit::None => None,
                            };
                            if let Some(t) = t {
                                texts.push((r, t, pal.text));
                            }
                        }
                        AnnShape::Busy { x0, x1, n: k, status } => {
                            let (a, b) = ((plot_l + x0).max(plot_l), (plot_l + x1).min(rect.right()));
                            let base = if status == view_core::ann::Status::Ok { pal.ok } else { pal.status(status) };
                            rect_x(&mut mesh, a, b.max(a + 1.0), y0 + 3.0, y1 - 3.0, base.gamma_multiply(0.85));
                            if b - a > 40.0 {
                                texts.push((Rect::from_min_max(pos2(a, y0), pos2(b, y1)), format!("×{k}"), pal.dim));
                            }
                        }
                    }
                }
            }
            Lane::Unread { claim } => {
                let Some(u) = analysis.and_then(|a| a.claims.get(claim)).and_then(|c| c.unread.as_ref()) else {
                    continue;
                };
                u.layout(&vp, 100.0, &mut ann_shapes);
                for s in &ann_shapes {
                    let (x0, x1) = match *s {
                        AnnShape::Box { x0, x1, .. } | AnnShape::Busy { x0, x1, .. } => (x0, x1),
                    };
                    rect_x(
                        &mut mesh,
                        (plot_l + x0).max(plot_l),
                        (plot_l + x1).min(rect.right()).max(plot_l + x0 + 1.0),
                        pr.top() + 1.0,
                        pr.bottom() - 1.0,
                        pal.error.gamma_multiply(0.8),
                    );
                }
                lane_painter.text(pos2(rect.left() + 14.0, lr.center().y), Align2::LEFT_CENTER, "読めなかった所", FontId::proportional(8.5), pal.dim);
            }
        }
    }
    // the selection being dragged
    if let (Some(x0), Some(p)) = (tab.st.select_from, hover) {
        let (a, b) = (plot_l + x0.min(p.x - plot_l), plot_l + x0.max(p.x - plot_l));
        rect_x(&mut mesh, a, b, plot.top(), plot.bottom(), pal.accent.gamma_multiply(0.15));
    }
    sh.perf.wave_vertices = mesh.vertices.len();
    plot_painter.add(Shape::mesh(mesh));
    // the texts under the plot's one clip (a clip of their own made each one a draw call of its own): a text wider
    // than its box is not drawn
    for (r, t, c) in texts {
        if old_draw() {
            plot_painter.with_clip_rect(r.shrink2(Vec2::new(2.0, 0.0)).intersect(plot)).text(r.center(), Align2::CENTER_CENTER, t, font.clone(), c);
            continue;
        }
        let g = plot_painter.layout_no_wrap(t, font.clone(), c);
        if g.size().x <= r.width() - 2.0 {
            plot_painter.galley(r.center() - g.size() / 2.0, g, c);
        }
    }
    painter.line_segment([pos2(plot_l, lanes_top), pos2(plot_l, rect.bottom())], Stroke::new(1.0, pal.grid));

    // markers
    for (k, m) in sh.markers.iter().enumerate() {
        if let Some(t) = m {
            let x = plot_l + vp.x_of(*t);
            if x >= plot_l && x <= rect.right() {
                painter.line_segment([pos2(x, ruler.top()), pos2(x, rect.bottom())], Stroke::new(1.5, pal.marker[k]));
                painter.text(pos2(x + 3.0, ruler.bottom() - 1.0), Align2::LEFT_BOTTOM, format!("M{}", k + 1), FontId::proportional(11.0), pal.marker[k]);
            }
        }
    }
    if let (Some(a), Some(b)) = (sh.markers[0], sh.markers[1]) {
        let (xa, xb) = (plot_l + vp.x_of(a), plot_l + vp.x_of(b));
        let yb = ruler.center().y;
        painter.line_segment([pos2(xa, yb), pos2(xb, yb)], Stroke::new(1.0, pal.accent));
        let d = cap.secs((b - a).abs());
        painter.text(
            pos2((xa + xb) / 2.0, yb),
            Align2::CENTER_BOTTOM,
            format!("Δ {}  ({})", fmt_time(d), fmt_freq(1.0 / d.max(1e-15))),
            FontId::proportional(11.0),
            pal.accent,
        );
    }

    // the cursor: its line, the pulse under it measured
    sh.hover_t = hover_t;
    if let Some(t) = hover_t {
        let x = plot_l + vp.x_of(t);
        painter.line_segment([pos2(x, ruler.top()), pos2(x, rect.bottom())], Stroke::new(1.0, pal.dim.gamma_multiply(0.6)));
        painter.text(pos2(x + 4.0, ruler.top()), Align2::LEFT_TOP, fmt_time(cap.secs(t)), FontId::monospace(10.5), pal.text);
        if let Some((Lane::Dig(i), lr)) = hovered_lane {
            let line = &cap.lines[i];
            let lod = line.lod.read().unwrap();
            if lod.built_until > t as i64 || line.src.level_before(0).is_some() {
                let (level, prev, next, next2) = around(line.src.as_ref(), &lod, t as i64);
                let m = Measure { line: i, t, level, prev, next, next2 };
                if let (Some(p), Some(q)) = (prev, next) {
                    let (xa, xb) = (plot_l + vp.x_of(p as f64), plot_l + vp.x_of(q as f64));
                    let yb = lr.top() + 2.0;
                    plot_painter.line_segment([pos2(xa, yb), pos2(xb, yb)], Stroke::new(2.0, pal.accent));
                }
                let txt = m.text(cap.tick_hz);
                if !txt.is_empty() {
                    let galley = painter.layout_no_wrap(txt, FontId::proportional(11.5), pal.text);
                    let p = pos2((x + 12.0).min(rect.right() - galley.size().x - 6.0), lr.bottom() + 2.0);
                    let r = Rect::from_min_size(p, galley.size()).expand(3.0);
                    painter.rect_filled(r, 3.0, pal.bg.gamma_multiply(0.92));
                    painter.rect_stroke(r, 3.0, Stroke::new(1.0, pal.grid), egui::StrokeKind::Inside);
                    painter.galley(p, galley, pal.text);
                }
                sh.measure = Some(m);
            }
        }
        if let Some((Lane::Ann { claim, layer }, _)) = hovered_lane
            && let Some(l) = sh.analysis.as_ref().and_then(|a| a.claims.get(claim)).map(|c| &c.layers[layer])
            && let Some(idx) = l.track.hit(t as i64)
        {
            let txt = l.track.text(idx).to_string();
            let it = l.track.item(idx);
            let galley = painter.layout_no_wrap(
                format!("{txt}\n{} – {}", fmt_time(cap.secs(it.s as f64)), fmt_time(cap.secs(it.e as f64))),
                FontId::proportional(11.5),
                pal.text,
            );
            let p = pos2((x + 12.0).min(rect.right() - galley.size().x - 6.0), hover.unwrap().y + 14.0);
            let r = Rect::from_min_size(p, galley.size()).expand(3.0);
            painter.rect_filled(r, 3.0, pal.bg.gamma_multiply(0.95));
            painter.rect_stroke(r, 3.0, Stroke::new(1.0, pal.grid), egui::StrokeKind::Inside);
            painter.galley(p, galley, pal.text);
        }
    }

    // clicks: an annotation chosen (the lists follow), a double click zooms to it
    if (resp.clicked() || resp.double_clicked())
        && let Some(t) = hover_t
        && let Some((Lane::Ann { claim, layer }, _)) = hovered_lane
        && let Some(l) = sh.analysis.as_ref().and_then(|a| a.claims.get(claim)).map(|c| &c.layers[layer])
        && let Some(idx) = l.track.hit(t as i64)
    {
        let it = *l.track.item(idx);
        sh.select(Sel { claim, layer, row: idx }, tab.id);
        if resp.double_clicked() {
            let w = (it.e - it.s) as f64;
            tab.st.vp = Some(Viewport { t0: it.s as f64 - w * 0.25, tpp: w * 1.5 / vp.width as f64, width: vp.width });
        }
    }
    resp.context_menu(|ui| {
        if let Some(t) = sh.hover_t.or(Some(vp.t_of(vp.width / 2.0))) {
            if ui.button("マーカー 1 をここに（1）").clicked() {
                sh.markers[0] = Some(t);
                ui.close();
            }
            if ui.button("マーカー 2 をここに（2）").clicked() {
                sh.markers[1] = Some(t);
                ui.close();
            }
        }
        if ui.button("マーカーを消す").clicked() {
            sh.markers = [None, None];
            ui.close();
        }
        if ui.button("全体を表示（F）").clicked() {
            tab.st.vp = None;
            ui.close();
        }
    });
    let _ = Vec2::ZERO;
    if let Some(fwd) = want_jump {
        jump_bad(tab, sh, fwd);
    }
    sh.perf.wave(t_start.elapsed().as_secs_f32() * 1e3, n_shapes);
    sh.perf.acc[crate::app::P_WAVE] += t_start.elapsed().as_secs_f64() * 1e3;
}

fn jump_bad(tab: &mut WaveTab, sh: &mut Shared, forward: bool) {
    let Some(a) = &sh.analysis else { return };
    let Some(vp) = tab.st.vp else { return };
    let center = vp.t_of(vp.width / 2.0) as i64;
    // the nearest bad item over every claim's coarsest layer
    let mut best: Option<(i64, Sel)> = None;
    for (ci, c) in a.claims.iter().enumerate() {
        let Some(l) = c.layers.first() else { continue };
        let cur = sh.sel.filter(|s| s.claim == ci && s.layer == 0).map(|s| s.row);
        let got = if forward { l.track.next_bad(cur, center) } else { l.track.prev_bad(cur, center) };
        if let Some(i) = got {
            let s = l.track.item(i).s;
            let better = match best {
                None => true,
                Some((bs, _)) => (forward && s < bs) || (!forward && s > bs),
            };
            if better {
                best = Some((s, Sel { claim: ci, layer: 0, row: i }));
            }
        }
    }
    if let Some((s, sel)) = best {
        let span = vp.tpp * vp.width as f64;
        tab.st.vp = Some(Viewport { t0: s as f64 - span / 2.0, ..vp });
        sh.select(sel, tab.id);
    }
}
