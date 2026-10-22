//! The ROI's shape (src/app_roi.cpp): a rectangle (Shift + drag) or a polygon (R, then clicks for vertices or a
//! freehand drag), kept in the active layer's pixels with its bounding box. The active layer reads the bounding box
//! and keeps the pixels whose centres are inside (Session::start_roi with a mask); every other user of the ROI
//! (other layers, statistics, Copy Python) asks `roi_contains` / `roi_on_layer`.

use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Response, Shape, Stroke, Ui, vec2};
use janus_geo::plane::{self, Polygon, Ring};
use janus_session::RoiData;

use crate::app::State;
use crate::layers::SeriesLayer;
use crate::map::body_font;
use crate::session::Session;

/// A polygon being drawn (R): its vertices in the active layer's pixels.
#[derive(Default)]
pub struct PolygonDraft {
    pub on: bool,
    pub vertices: Vec<[f64; 2]>,
    /// the canvas the button went down on
    panel: Option<egui::Id>,
    press: [f64; 2],
    /// a freehand stroke (the button dragged)
    lasso: bool,
}

/// The ROI's statistics on a session: its bounding box, the pixels whose centres are inside the polygon (none: the
/// whole box).
pub(crate) fn start_roi(s: &Session, r: [usize; 4], poly: &Polygon) -> Arc<RoiData> {
    let mask = |x0: usize, y0: usize, w: usize, h: usize, bw: usize, bh: usize| {
        plane::mask(poly, x0 as f64, y0 as f64, w as f64, h as f64, bw, bh)
    };
    s.s.start_roi((r[0], r[1], r[2], r[3]), if poly.is_empty() { None } else { Some(&mask) })
}

impl State {
    /// Screen points per pixel of the active layer.
    fn screen_per_px(&self) -> f64 {
        self.grid().map_or(1.0, |g| self.map.scale / g.fx)
    }

    /// A new ROI of a polygon (active pixels, even-odd rings): false if it covers no pixel's centre of the image.
    pub fn set_roi_polygon(&mut self, poly: Polygon) -> bool {
        let Some(s) = &self.session else { return false };
        let Some(b) = plane::bounds(&poly) else { return false };
        let (w, h) = (s.info.width as f64, s.info.height as f64);
        let r = [
            b[0].floor().clamp(0.0, w),
            b[1].floor().clamp(0.0, h),
            b[2].ceil().clamp(0.0, w),
            b[3].ceil().clamp(0.0, h),
        ];
        if r[2] <= r[0] || r[3] <= r[1] {
            return false; // off the image
        }
        // at least one pixel's centre inside (a sliver between centres has no statistics)
        let (rw, rh) = (r[2] - r[0], r[3] - r[1]);
        if rw * rh <= 16e6 && !plane::mask(&poly, r[0], r[1], rw, rh, rw as usize, rh as usize).contains(&true) {
            return false;
        }
        self.set_roi_shape([r[0] as i64, r[1] as i64, r[2] as i64, r[3] as i64], poly);
        true
    }

    /// Whether the active layer's point (x, y) is in the ROI (everywhere without one).
    pub fn roi_contains(&self, x: f64, y: f64) -> bool {
        let Some(r) = self.series.roi_rect else { return true };
        if x < r[0] as f64 || y < r[1] as f64 || x >= r[2] as f64 || y >= r[3] as f64 {
            return false;
        }
        self.series.roi_poly.is_empty() || plane::contains(&self.series.roi_poly, x, y)
    }

    /// The ROI as a polygon in another layer's pixels; empty: the rectangle, straight on that layer (or nothing of it
    /// lands there).
    pub(crate) fn roi_on_layer(&self, l: &SeriesLayer) -> Polygon {
        let Some([x0, y0, x1, y1]) = self.series.roi_rect.map(|r| r.map(|v| v as f64)) else { return vec![] };
        let mut p = self.series.roi_poly.clone();
        let reproj = l.place.reproj.as_ref().map(|(r, _)| r);
        if p.is_empty() {
            if reproj.is_none() {
                return vec![];
            }
            p = vec![vec![x0, y0, x1, y0, x1, y1, x0, y1]];
        }
        if let Some(r) = reproj {
            // edges become curves in another CRS: about 16 points along each side of the box
            let b = plane::bounds(&p).unwrap_or_default();
            p = plane::densify(&p, ((b[2] - b[0]).max(b[3] - b[1]) / 16.0).max(1.0));
            plane::transform(&mut p, |x, y| r.to_layer(x, y));
        } else {
            plane::transform(&mut p, |x, y| l.place.to_layer(x, y));
        }
        p
    }

    /// R: clicks add vertices (on the first one, a double click or Enter close it), a drag draws a freehand outline
    /// closed when released; Shift + drag is still a rectangle. Returns whether the pointer was taken.
    pub(crate) fn polygon_input(&mut self, ui: &Ui, resp: &Response, p: Option<[f64; 2]>) -> bool {
        if !self.series.poly.on {
            return false;
        }
        let k = self.screen_per_px();
        let d = &mut self.series.poly;
        let (pressed, shift) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift));
        if resp.hovered() && pressed {
            if shift {
                return false; // a rectangle, as ever
            }
            let Some(p) = p else { return false };
            (d.panel, d.press, d.lasso) = (Some(resp.id), p, false);
            return true;
        }
        if d.panel != Some(resp.id) {
            return false;
        }
        let Some(p) = p else { return true };
        if resp.is_pointer_button_down_on() {
            let moved = ui.input(|i| {
                i.pointer.press_origin().zip(i.pointer.interact_pos()).is_some_and(|(a, b)| a.distance(b) > 4.0)
            });
            if !d.lasso && moved {
                d.lasso = true;
                d.vertices.push(d.press); // the stroke starts where the button went down
            }
            if d.lasso {
                let last = *d.vertices.last().unwrap();
                if ((p[0] - last[0]) * k).hypot((p[1] - last[1]) * k) >= 3.0 {
                    d.vertices.push(p); // a vertex every 3 screen points
                }
            }
            return true;
        }
        // released: a stroke closes at once; a click closes on the first vertex or a double click, else adds one
        d.panel = None;
        if d.lasso {
            d.vertices.push(p);
            self.finish_polygon();
            return true;
        }
        let double = resp.double_clicked();
        if d.vertices.len() >= 3 {
            let f = d.vertices[0];
            if double || ((p[0] - f[0]) * k).hypot((p[1] - f[1]) * k) <= 8.0 {
                self.finish_polygon();
                return true;
            }
        }
        if !double {
            d.vertices.push(p);
        }
        true
    }

    /// The polygon drawn becomes the ROI (without repeated vertices: a double click is two clicks).
    pub fn finish_polygon(&mut self) {
        let k = self.screen_per_px();
        let mut r: Ring = Vec::new();
        for v in std::mem::take(&mut self.series.poly.vertices) {
            let n = r.len();
            if n == 0 || (r[n - 2] - v[0]).abs() * k > 1.0 || (r[n - 1] - v[1]).abs() * k > 1.0 {
                r.extend(v);
            }
        }
        if r.len() >= 6 && self.set_roi_polygon(vec![r]) {
            self.series.poly.on = false;
        }
    }

    /// R toggles the polygon; while drawing, Enter closes it, Backspace / Delete take the last vertex back, Esc
    /// cancels.
    pub(crate) fn roi_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        let (r, enter, back, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::R) && i.modifiers.is_none(),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete),
                i.key_pressed(Key::Escape),
            )
        });
        if r {
            self.toggle_polygon();
        }
        if !self.series.poly.on {
            return;
        }
        if enter {
            self.finish_polygon();
        }
        if back {
            self.series.poly.vertices.pop();
        }
        if esc {
            self.series.poly = PolygonDraft::default();
        }
    }

    pub(crate) fn toggle_polygon(&mut self) {
        let on = !self.series.poly.on;
        self.series.poly = PolygonDraft { on, ..Default::default() };
    }

    /// The ROI's outline (a dark line under it: visible over any colormap), its progress, and the polygon being
    /// drawn with a hint under the cursor.
    pub(crate) fn draw_roi_marks(&self, painter: &egui::Painter, rect: Rect, at: &dyn Fn(f64, f64) -> Pos2) {
        let roi = crate::series::ROI_COLOR;
        let sr = &self.series;
        let outline = |r: [usize; 4], thick: f32| {
            let dark = Stroke::new(thick + 2.0, Color32::from_black_alpha(160));
            if sr.roi_poly.is_empty() {
                let rr = Rect::from_min_max(at(r[0] as f64, r[1] as f64), at(r[2] as f64, r[3] as f64));
                painter.rect_stroke(rr, 0.0, dark, egui::StrokeKind::Middle);
                painter.rect_stroke(rr, 0.0, Stroke::new(thick, roi), egui::StrokeKind::Middle);
                return;
            }
            for ring in &sr.roi_poly {
                let pts: Vec<Pos2> = ring.as_chunks::<2>().0.iter().map(|v| at(v[0], v[1])).collect();
                painter.add(Shape::closed_line(pts.clone(), dark));
                painter.add(Shape::closed_line(pts, Stroke::new(thick, roi)));
            }
        };
        if let Some((a, b)) = sr.roi_drag {
            let rr = Rect::from_two_pos(at(a[0], a[1]), at(b[0], b[1]));
            painter.rect_stroke(rr, 0.0, Stroke::new(2.0, roi), egui::StrokeKind::Middle);
        } else if let Some(r) = sr.roi_rect {
            let p = at(r[0] as f64, r[1] as f64) + vec2(4.0, 2.0);
            if sr.roi_pending {
                outline(r, 1.0);
                painter.text(p, egui::Align2::LEFT_TOP, "ROI: waiting for the overview (HDD)", body_font(painter), roi);
            } else if let Some(d) = &sr.roi.data {
                outline(r, 2.0);
                let (done, n) = (d.done.load(std::sync::atomic::Ordering::Relaxed), d.per_t.len());
                if done < n {
                    painter.text(p, egui::Align2::LEFT_TOP, format!("ROI {done}/{n}"), body_font(painter), roi);
                }
            }
        }
        let d = &sr.poly;
        if !d.on {
            return;
        }
        // the polygon being drawn: its vertices, and the edge that follows the cursor
        let hover = painter.ctx().pointer_hover_pos().filter(|p| rect.contains(*p));
        let mut pts: Vec<Pos2> = d.vertices.iter().map(|v| at(v[0], v[1])).collect();
        if let Some(h) = hover.filter(|_| !d.lasso && !pts.is_empty()) {
            pts.push(h);
        }
        if pts.len() >= 2 {
            painter.add(Shape::line(pts, Stroke::new(4.0, Color32::from_black_alpha(160))));
            let pts: Vec<Pos2> = d.vertices.iter().map(|v| at(v[0], v[1])).chain(hover.filter(|_| !d.lasso)).collect();
            painter.add(Shape::line(pts, Stroke::new(2.0, roi)));
        }
        if !d.lasso {
            for (i, v) in d.vertices.iter().enumerate() {
                let c = at(v[0], v[1]);
                let r = if i == 0 && d.vertices.len() >= 3 { 6.0 } else { 3.5 }; // the first closes it
                painter.circle_filled(c, r, roi);
                painter.circle_stroke(c, r, Stroke::new(1.5, Color32::from_black_alpha(220)));
            }
        }
        if let Some(h) = hover {
            let hint = match d.vertices.len() {
                0 => "ROI polygon: click vertices or drag a freehand outline (Esc cancels)",
                1 | 2 => "click: next vertex, Backspace: undo, Esc: cancel",
                _ => "click the first vertex, double click or Enter: close",
            };
            let galley = painter.layout_no_wrap(hint.into(), body_font(painter), roi);
            let p = h + vec2(16.0, 12.0);
            painter.rect_filled(
                Rect::from_min_max(p - vec2(4.0, 2.0), p + galley.size() + vec2(4.0, 2.0)),
                3.0,
                Color32::from_black_alpha(190),
            );
            painter.galley(p, galley, roi);
        }
    }
}
