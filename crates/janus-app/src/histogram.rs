//! The Display panel's histogram: the distribution of the values shown, filled in the colormap as the map paints
//! them, the stretch's two ends as handles to drag (between them, both move), the colormap strip under it.

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, CursorIcon, Rect, Sense, Shape, Stroke, Ui, pos2, vec2};
use janus_gpu::colormaps;

use crate::display::{Histogram, Range};
use crate::theme;

/// Height of the distribution, of the colormap strip, of the labels under it.
const PLOT_H: f32 = 92.0;
const STRIP_H: f32 = 8.0;
const LABEL_H: f32 = 16.0;
/// How near (points) a handle is grabbed.
const GRAB: f32 = 7.0;

#[derive(Clone, Copy, PartialEq)]
enum Grip {
    Lo,
    Hi,
    /// both ends together
    Both,
}

/// The histogram `h` with the stretch `r` (dragged: set by hand). `cmap`: the colormap (none: red, green and blue
/// drawn as one). `label`: a value as text (dates for a date mode). `discrete`: one bar per value (dates). Returns
/// whether the stretch changed.
pub fn histogram_ui(
    ui: &mut Ui,
    h: &Histogram,
    r: &mut Range,
    cmap: Option<usize>,
    discrete: bool,
    label: &dyn Fn(f32) -> String,
) -> bool {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, PLOT_H + STRIP_H + LABEL_H + 6.0), Sense::click_and_drag());
    let id = resp.id;
    let plot = Rect::from_min_size(rect.min, vec2(width, PLOT_H));
    let strip = Rect::from_min_size(pos2(rect.min.x, plot.max.y + 3.0), vec2(width, STRIP_H));
    let v = ui.visuals().clone();
    let radius = if theme::modern() { 4.0 } else { 2.0 };

    // the values across: the histogram's extent and the stretch, kept while a handle is dragged (the scale does
    // not move under the pointer)
    let (h0, h1) = (h.xs[0] - h.bar_w / 2.0, h.xs[h.xs.len() - 1] + h.bar_w / 2.0);
    let dom_id = id.with("domain");
    let domain = match ui.data(|d| d.get_temp::<(f64, f64)>(dom_id)) {
        Some(d) if resp.dragged() => d,
        _ => {
            let (a, b) = (h0.min(f64::from(r.lo)), h1.max(f64::from(r.hi)));
            let pad = (b - a).max(1e-9) * 0.02;
            (a - pad, b + pad)
        }
    };
    ui.data_mut(|d| d.insert_temp(dom_id, domain));
    let (d0, d1) = domain;
    let to_x = |val: f64| plot.left() + ((val - d0) / (d1 - d0)) as f32 * plot.width();
    let to_val = |x: f32| d0 + f64::from((x - plot.left()) / plot.width()) * (d1 - d0);

    // dragging: a handle, or both between them; a double click: the automatic range again
    let mut changed = false;
    let grip_id = id.with("grip");
    let (xl, xh) = (to_x(f64::from(r.lo)), to_x(f64::from(r.hi)));
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let near = |x: f32| -> Option<Grip> {
        let (dl, dh) = ((x - xl).abs(), (x - xh).abs());
        if dl.min(dh) <= GRAB {
            Some(if dl <= dh { Grip::Lo } else { Grip::Hi })
        } else if x > xl && x < xh {
            Some(Grip::Both)
        } else {
            None
        }
    };
    if resp.drag_started()
        && let Some(p) = pointer
    {
        // outside the stretch: its nearer end jumps there
        let g = near(p.x).unwrap_or(if p.x < xl { Grip::Lo } else { Grip::Hi });
        ui.data_mut(|d| d.insert_temp(grip_id, (g, f64::from(r.lo), f64::from(r.hi), p.x)));
    }
    if resp.dragged()
        && let (Some(p), Some((g, lo0, hi0, x0))) = (pointer, ui.data(|d| d.get_temp::<(Grip, f64, f64, f32)>(grip_id)))
    {
        let eps = (d1 - d0) * 1e-4;
        let val = to_val(p.x).clamp(d0, d1);
        let (lo, hi) = match g {
            Grip::Lo => (val.min(hi0 - eps), hi0),
            Grip::Hi => (lo0, val.max(lo0 + eps)),
            Grip::Both => {
                let dv = to_val(p.x) - to_val(x0);
                (lo0 + dv, hi0 + dv)
            }
        };
        if (lo as f32, hi as f32) != (r.lo, r.hi) {
            (r.lo, r.hi) = (lo as f32, hi as f32);
            r.manual = true;
            changed = true;
        }
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<(Grip, f64, f64, f32)>(grip_id));
    }
    if resp.double_clicked() {
        r.manual = false;
        r.invalidate();
        changed = true;
    }
    let held = ui.data(|d| d.get_temp::<(Grip, f64, f64, f32)>(grip_id)).map(|g| g.0).filter(|_| resp.dragged());
    let hot = held.or_else(|| resp.hover_pos().and_then(|p| near(p.x)));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(match hot {
            Some(Grip::Lo | Grip::Hi) => CursorIcon::ResizeHorizontal,
            Some(Grip::Both) if resp.dragged() => CursorIcon::Grabbing,
            Some(Grip::Both) => CursorIcon::Grab,
            None => CursorIcon::Default,
        });
    }
    let (lo, hi) = (f64::from(r.lo), f64::from(r.hi));
    let (xl, xh) = (to_x(lo), to_x(hi));

    // a value's colour as the map paints it (clamped at the stretch's ends), dimmed outside the stretch
    let ramp = cmap.map(|k| &colormaps::all()[k]);
    let neutral = v.text_color();
    let colour = |val: f64| -> Color32 {
        let t = ((val - lo) / (hi - lo).max(1e-12)).clamp(0.0, 1.0) as f32;
        match ramp {
            Some(cm) => {
                let [r, g, b, _] = cm.sample(t);
                Color32::from_rgb(r, g, b)
            }
            None => neutral.gamma_multiply(0.75),
        }
    };
    let dim = |c: Color32, val: f64| if val < lo || val > hi { c.gamma_multiply(0.28) } else { c };

    let painter = ui.painter_at(rect);
    painter.rect_filled(plot, radius, v.extreme_bg_color);
    painter.rect_stroke(
        plot,
        radius,
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6)),
        egui::StrokeKind::Inside,
    );
    // quarter lines, faint
    let grid = Stroke::new(1.0, v.weak_text_color().gamma_multiply(0.12));
    for f in [0.25, 0.5, 0.75] {
        painter.hline(plot.x_range(), plot.bottom() - f * (plot.height() - 8.0), grid);
    }
    let top = h.ys.iter().copied().fold(0.0, f64::max).max(1e-12) * 1.08;
    let to_y = |y: f64| plot.bottom() - (y / top) as f32 * (plot.height() - 4.0);
    let base = plot.bottom();
    if discrete {
        // a bar per date
        let gap = (plot.width() / h.xs.len() as f32 * 0.18).min(2.0);
        for (&x, &y) in h.xs.iter().zip(&h.ys) {
            let (a, b) = (to_x(x - h.bar_w / 2.0) + gap / 2.0, to_x(x + h.bar_w / 2.0) - gap / 2.0);
            let bar = Rect::from_x_y_ranges(a..=b.max(a + 1.0), to_y(y)..=base);
            painter.rect_filled(bar, 1.0, dim(colour(x), x));
        }
    } else {
        // the distribution through the bins' centres, a vertex at each end of the stretch (where the colour
        // changes from dim to full)
        // (x, y, the value its colour is taken from)
        let mut pts: Vec<(f64, f64, f64)> = Vec::with_capacity(h.xs.len() + 6);
        pts.push((h0, h.ys[0], h0));
        pts.extend(h.xs.iter().zip(&h.ys).map(|(&x, &y)| (x, y, x)));
        pts.push((h1, h.ys[h.ys.len() - 1], h1));
        let at = |pts: &[(f64, f64, f64)], x: f64| -> f64 {
            let i = pts.partition_point(|p| p.0 < x).clamp(1, pts.len() - 1);
            let (a, b) = (pts[i - 1], pts[i]);
            a.1 + (b.1 - a.1) * ((x - a.0) / (b.0 - a.0).max(1e-12)).clamp(0.0, 1.0)
        };
        for e in [lo, hi] {
            if e > h0 && e < h1 {
                let y = at(&pts, e);
                let i = pts.partition_point(|p| p.0 < e);
                // twice: dim on one side, full on the other
                let tiny = (d1 - d0) * 1e-9;
                pts.splice(i..i, [(e, y, e - tiny), (e, y, e + tiny)]);
            }
        }
        let mut mesh = Mesh::default();
        let mut line = Vec::with_capacity(pts.len());
        for (k, &(x, y, cv)) in pts.iter().enumerate() {
            let c = dim(colour(cv), cv);
            let (px, py) = (to_x(x), to_y(y));
            let i = mesh.vertices.len() as u32;
            mesh.vertices.push(Vertex { pos: pos2(px, py), uv: WHITE_UV, color: c.gamma_multiply(0.92) });
            mesh.vertices.push(Vertex { pos: pos2(px, base), uv: WHITE_UV, color: c.gamma_multiply(0.38) });
            if k > 0 {
                mesh.add_triangle(i - 2, i - 1, i);
                mesh.add_triangle(i - 1, i + 1, i);
            }
            line.push(pos2(px, py));
        }
        painter.add(Shape::mesh(mesh));
        painter.add(Shape::line(line, Stroke::new(1.0, v.text_color().gamma_multiply(0.55))));
    }

    // the stretch's ends: a line over the distribution, a grip on the strip
    let accent = v.selection.bg_fill;
    for (x, g) in [(xl, Grip::Lo), (xh, Grip::Hi)] {
        if x < plot.left() - 1.0 || x > plot.right() + 1.0 {
            continue;
        }
        let lit = hot == Some(g) || hot == Some(Grip::Both);
        let c = if lit { accent } else { v.text_color().gamma_multiply(0.75) };
        painter.vline(x, plot.y_range(), Stroke::new(if lit { 1.6 } else { 1.0 }, c));
    }

    // the colormap strip: what each value becomes on the map
    let n = 96;
    for i in 0..n {
        let (a, b) = (
            strip.left() + strip.width() * i as f32 / n as f32,
            strip.left() + strip.width() * (i + 1) as f32 / n as f32,
        );
        let val = to_val((a + b) / 2.0);
        painter.rect_filled(Rect::from_x_y_ranges(a..=b + 0.5, strip.y_range()), 0.0, colour(val));
    }
    painter.rect_stroke(
        strip,
        0.0,
        Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color),
        egui::StrokeKind::Outside,
    );
    for (x, g) in [(xl, Grip::Lo), (xh, Grip::Hi)] {
        let x = x.clamp(plot.left() + 4.0, plot.right() - 4.0);
        let lit = hot == Some(g) || hot == Some(Grip::Both);
        let fill = if lit { accent } else { v.text_color() };
        let tip = pos2(x, strip.top() + 1.0);
        let tri = vec![tip, pos2(x + 5.0, strip.bottom() + 3.0), pos2(x - 5.0, strip.bottom() + 3.0)];
        painter.add(Shape::convex_polygon(tri, fill, Stroke::new(1.0, v.extreme_bg_color)));
    }

    // the ends' values under their grips (the low one to its left, the high one to its right)
    let font = egui::TextStyle::Small.resolve(ui.style());
    let ly = strip.bottom() + 4.0;
    let weak = v.text_color().gamma_multiply(0.8);
    let lo_t = painter.layout_no_wrap(label(r.lo), font.clone(), weak);
    let hi_t = painter.layout_no_wrap(label(r.hi), font.clone(), weak);
    let lo_x = (xl - lo_t.size().x / 2.0).clamp(rect.left(), rect.right() - lo_t.size().x);
    let hi_x = (xh - hi_t.size().x / 2.0).clamp(rect.left(), rect.right() - hi_t.size().x);
    // apart when they would overlap
    let overlap = (lo_x + lo_t.size().x + 6.0 - hi_x).max(0.0);
    let lo_x = (lo_x - overlap / 2.0).max(rect.left());
    let hi_x = (hi_x + overlap / 2.0).min(rect.right() - hi_t.size().x);
    painter.galley(pos2(lo_x, ly), lo_t, weak);
    painter.galley(pos2(hi_x, ly), hi_t, weak);

    // under the pointer: its bin's values and share
    if let Some(p) = resp.hover_pos().filter(|p| plot.contains(*p) && !resp.dragged()) {
        let val = to_val(p.x);
        let i = ((val - h0) / h.bar_w).floor();
        if i >= 0.0 && (i as usize) < h.xs.len() {
            let i = i as usize;
            painter.vline(p.x, plot.y_range(), Stroke::new(1.0, v.text_color().gamma_multiply(0.3)));
            let what = if discrete {
                label(h.xs[i] as f32)
            } else {
                let w = h.bar_w as f32;
                format!("{} .. {}", label(h.xs[i] as f32 - w / 2.0), label(h.xs[i] as f32 + w / 2.0))
            };
            let text = format!("{what}   {:.2} %", h.ys[i]);
            let g = painter.layout_no_wrap(text, font, v.text_color());
            let at = pos2(plot.right() - g.size().x - 6.0, plot.top() + 4.0);
            let bg = Rect::from_min_size(at, g.size()).expand2(vec2(4.0, 2.0));
            painter.rect_filled(bg, 3.0, v.window_fill.gamma_multiply(0.9));
            painter.galley(at, g, v.text_color());
        }
    }
    if resp.hover_pos().is_some_and(|p| p.y > plot.bottom()) {
        resp.on_hover_text(
        "The values shown and the colours they get on the map.\nDrag an end of the stretch, or between them to move \
         both; double-click: automatic range.",
        );
    }
    changed
}
