//! Vector layers (src/app_vectors.cpp): field plots, samples, parcels... read through OGR (janus_geo::vectors) in
//! the background, placed on the active layer's grid (any CRS, through PROJ; again whenever the active layer's
//! grid changes) and drawn over every map panel with the painter's lines, cut to what is on screen and thinned to
//! about a vertex per screen point. A polygon becomes the ROI (Alt + click), a line the transect; the series at
//! every feature are written to a CSV table (Extract series).

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::{Color32, Pos2, Rect, Shape, Stroke, vec2};
use janus_geo::plane::{self, Polygon, Ring};
use janus_geo::vectors::{Kind, Placer, VectorData, csv_field, is_vector_path, load_vector_file};

use crate::app::State;
use crate::exports::{ExportJob, start_export_job, write_text_file};
use crate::map::body_font;
use crate::theme;

/// Colours of the vector layers: none is the ROI's yellow or a pin's colour.
const COLORS: [[f32; 3]; 5] =
    [[1.0, 0.45, 0.15], [0.95, 0.30, 0.95], [0.70, 1.00, 0.20], [1.0, 1.0, 1.0], [0.35, 0.55, 1.0]];

/// Vertices read at most from a file.
const MAX_VERTICES: usize = 20_000_000;

pub struct VectorLayer {
    pub id: u32,
    pub data: Arc<VectorData>,
    pub name: String,
    pub color: Color32,
    pub visible: bool,
    /// each feature's parts in the active layer's pixels (empty: not placed)
    pub px: Arc<Vec<Polygon>>,
    /// each feature's box (x0, y0, x1, y1; x0 > x1: nothing placed)
    pub boxes: Vec<[f32; 4]>,
    /// a point on each feature (NaN: none)
    pub anchor: Vec<[f64; 2]>,
    /// the grid it was placed on (vector_place_key; 0: not yet)
    pub placed_for: u64,
    /// why it is not drawn
    pub why: String,
    /// nothing of it over the active layer
    pub off_map: bool,
    /// a colour per feature (labels), else the layer's
    pub colors: Vec<Color32>,
    /// drawn by the GPU (vector_gpu.rs)
    pub gpu: crate::vector_gpu::LayerGpu,
}

/// Extract series: the dialog's choices.
#[derive(Default)]
pub struct ExtractUi {
    pub open: bool,
    pub vector: u32,
    pub all_layers: bool,
    /// a row per feature (else a row per feature and date)
    pub wide: bool,
    pub path: String,
}

/// Vector files being read.
#[derive(Default)]
pub struct VectorLoads {
    pending: Vec<Receiver<Result<Vec<VectorData>, String>>>,
}

impl VectorLoads {
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }
}

fn segment_distance(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let k = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    (px - ax - k * dx).hypot(py - ay - k * dy)
}

/// A polyline's segments as quads `width` wide (no joins, no antialiased edge), into `mesh`.
fn add_polyline(mesh: &mut egui::Mesh, pts: &[Pos2], closed: bool, width: f32, color: Color32) {
    let n = pts.len();
    let segments = if closed { n } else { n - 1 };
    for k in 0..segments {
        let (a, b) = (pts[k], pts[(k + 1) % n]);
        let d = b - a;
        let len = d.length();
        if len <= 0.0 {
            continue;
        }
        let nrm = egui::vec2(-d.y, d.x) * (width * 0.5 / len);
        let i = mesh.vertices.len() as u32;
        for p in [a + nrm, a - nrm, b - nrm, b + nrm] {
            mesh.colored_vertex(p, color);
        }
        mesh.add_triangle(i, i + 1, i + 2);
        mesh.add_triangle(i, i + 2, i + 3);
    }
}

/// A number of a table ("" for NaN).
fn number(v: f64) -> String {
    if v.is_nan() { String::new() } else { janus_cube::multidim::format_g(v, 7) }
}

impl State {
    /// Vector files among inputs become vector layers; the rest is returned.
    pub fn take_vector_inputs(&mut self, inputs: Vec<String>, ctx: &egui::Context) -> Vec<String> {
        let (vec, rest): (Vec<String>, Vec<String>) =
            inputs.into_iter().partition(|p| is_vector_path(p) && !std::path::Path::new(p).is_dir());
        if !vec.is_empty() {
            self.add_vector_files(vec, ctx);
        }
        rest
    }

    pub fn add_vector_files(&mut self, paths: Vec<String>, ctx: &egui::Context) {
        for path in paths {
            if janus_validation::is_project(&path) {
                // a validation project: its sample, labels and maps (validation.rs)
                self.open_validation_project(&path, ctx);
                continue;
            }
            let (tx, rx) = channel();
            let c = ctx.clone();
            std::thread::spawn(move || {
                let _ = tx.send(load_vector_file(&path, MAX_VERTICES));
                c.request_repaint();
            });
            self.vector_loads.pending.push(rx);
        }
    }

    /// The active layer's grid (its CRS and geotransform): placing again when it changes.
    pub(crate) fn vector_place_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let Some(s) = &self.session else { return 0 };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        s.info.crs_wkt.hash(&mut h);
        s.info.geo_transform.map(|g| g.map(f64::to_bits)).hash(&mut h);
        h.finish() | 1 // never 0 (not placed)
    }

    pub fn place_vector(&mut self, i: usize) {
        let key = self.vector_place_key();
        let info = self.session.as_ref().map(|s| s.info.clone());
        let v = &mut self.vectors[i];
        v.placed_for = key;
        (v.px, v.boxes, v.anchor) = (Arc::default(), Vec::new(), Vec::new());
        let Some(info) = info else {
            v.why = "drawn over a raster series: open one".into();
            return;
        };
        if let Err(e) = Placer::new(&v.data, &info.crs_wkt, info.geo_transform) {
            v.why = format!("not drawn: {e}");
            return;
        }
        // in parallel: a placer (its own transformation) per chunk of features
        use rayon::prelude::*;
        let data = v.data.clone();
        let feats = &data.features;
        let chunk = (feats.len() / (rayon::current_num_threads() * 4)).max(256);
        let placed: Vec<(Polygon, [f32; 4], [f64; 2], bool)> = feats
            .par_chunks(chunk)
            .flat_map_iter(|fs| {
                let p = Placer::new(&data, &info.crs_wkt, info.geo_transform).ok();
                fs.iter()
                    .map(|f| {
                        let Some(p) = &p else { return (Vec::new(), [1.0, 1.0, 0.0, 0.0], [f64::NAN; 2], false) };
                        let px = p.placed(&f.parts);
                        let b = plane::bounds(&px);
                        let a = (f.kind != Kind::None).then(|| p.to_pixel(f.cx, f.cy)).flatten();
                        let on = b.is_some_and(|b| {
                            b[2] >= 0.0 && b[3] >= 0.0 && b[0] <= info.width as f64 && b[1] <= info.height as f64
                        });
                        let bb = b.map_or([1.0, 1.0, 0.0, 0.0], |b| b.map(|x| x as f32));
                        (px, bb, a.map_or([f64::NAN; 2], |(x, y)| [x, y]), on)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let on_map = placed.iter().filter(|p| p.3).count();
        let mut px = Vec::with_capacity(placed.len());
        for (p, b, a, _) in placed {
            px.push(p);
            v.boxes.push(b);
            v.anchor.push(a);
        }
        v.px = Arc::new(px);
        v.why.clear();
        v.off_map = on_map == 0; // still drawn: it may be near
    }

    /// Files read: new vector layers; every layer placed on the active layer's grid.
    pub(crate) fn pump_vectors(&mut self) {
        let mut done = Vec::new();
        self.vector_loads.pending.retain(|rx| match rx.try_recv() {
            Ok(r) => {
                done.push(r);
                false
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(_) => false,
        });
        for r in done {
            let layers = match r {
                Ok(l) => l,
                Err(e) => {
                    self.error_popup = Some(e);
                    continue;
                }
            };
            let several = layers.len() > 1;
            for d in layers {
                let id = self.next_vector_id;
                self.next_vector_id += 1;
                let file = std::path::Path::new(&d.path);
                let fname = file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let stem = file.file_stem().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                let name = if several || stem != d.layer { format!("{fname}: {}", d.layer) } else { fname };
                let c = COLORS[(id as usize - 1) % COLORS.len()].map(|v| (v * 255.0).round() as u8);
                self.vectors.push(VectorLayer {
                    id,
                    data: Arc::new(d),
                    name,
                    color: Color32::from_rgb(c[0], c[1], c[2]),
                    visible: true,
                    px: Arc::default(),
                    boxes: Vec::new(),
                    anchor: Vec::new(),
                    placed_for: 0,
                    why: String::new(),
                    off_map: false,
                    colors: Vec::new(),
                    gpu: Default::default(),
                });
            }
            self.map.dirty = true;
        }
        let key = self.vector_place_key();
        for i in 0..self.vectors.len() {
            let v = &self.vectors[i];
            if v.placed_for != key || (key == 0 && v.why.is_empty()) {
                self.place_vector(i);
            }
        }
    }

    pub fn vector_index(&self, id: u32) -> Option<usize> {
        self.vectors.iter().position(|v| v.id == id)
    }

    pub fn remove_vector(&mut self, i: usize) {
        if i >= self.vectors.len() {
            return;
        }
        if self.labels.vector == Some(self.vectors[i].id) {
            self.stop_interpretation();
        }
        self.vectors.remove(i);
        self.hover_vector = None;
    }

    /// The vector layers over a map canvas, under the pins and the ROI: what is on screen, about a vertex per
    /// 1.5 screen points, at most 2 million vertices a frame.
    pub(crate) fn draw_vector_marks(&self, painter: &egui::Painter, rect: Rect, at: &dyn Fn(f64, f64) -> Pos2) {
        if self.vectors.is_empty() {
            return;
        }
        let Some(g) = self.grid() else { return };
        let k = self.screen_per_px();
        // what is on screen, in active pixels (a margin for the lines' width)
        let clip = painter.clip_rect().intersect(rect);
        let m = 8.0 / k;
        let [ax, ay] = self.map.image_at(rect, clip.min);
        let [bx, by] = self.map.image_at(rect, clip.max);
        let (vx0, vy0, vx1, vy1) = (ax * g.fx - m, ay * g.fy - m, bx * g.fx + m, by * g.fy + m);
        let in_view = |b: &[f32; 4]| {
            b[0] <= b[2]
                && f64::from(b[2]) >= vx0
                && f64::from(b[0]) <= vx1
                && f64::from(b[3]) >= vy0
                && f64::from(b[1]) <= vy1
        };
        let dark = Color32::from_black_alpha(170);
        let mut budget: usize = 2_000_000; // vertices per frame: beyond, the rest waits for a closer zoom
        let interp = self.labels.vector.and_then(|id| self.vector_index(id));
        let mut pts: Vec<Pos2> = Vec::new();
        for (li, v) in self.vectors.iter().enumerate() {
            if !v.visible || v.px.is_empty() {
                continue;
            }
            // drawn by the GPU: here only its highlighted feature
            let on_gpu = v.gpu.draws(v.placed_for);
            if on_gpu && self.hover_vector.map(|h| h.0) != Some(li) && interp != Some(li) {
                continue;
            }
            // many points in view: squares (fewer vertices than discs)
            let squares = !on_gpu
                && v.data.points > 2000
                && v.px
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| v.data.features[*i].kind == Kind::Point && in_view(&v.boxes[*i]))
                    .count()
                    > 2000;
            // many lines and polygons in view: their outlines as plain quads in one mesh (ImGui's thin lines are not
            // feathered either; egui's antialiased strokes cost 3x more to tessellate), else antialiased strokes
            let shapes = v.data.lines + v.data.areas;
            let quads = !on_gpu
                && shapes > 2000
                && v.px
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| v.data.features[*i].kind >= Kind::Line && in_view(&v.boxes[*i]))
                    .count()
                    > 2000;
            let mut lines_mesh = egui::Mesh::default();
            // the squares of many points: one mesh, 4 vertices each without an antialiased edge (as ImGui draws a
            // filled rectangle; egui's would tessellate each with a feathered edge)
            let mut squares_mesh = egui::Mesh::default();
            if squares {
                squares_mesh.reserve_vertices(v.data.points * 8);
                squares_mesh.reserve_triangles(v.data.points * 4);
            }
            for (i, parts) in v.px.iter().enumerate() {
                if budget == 0 {
                    break;
                }
                let b = &v.boxes[i];
                if !in_view(b) {
                    continue;
                }
                let f = &v.data.features[i];
                let hot = self.hover_vector == Some((li, i)) || (interp == Some(li) && self.labels.current == Some(i));
                if on_gpu && !hot {
                    continue;
                }
                let col = v.colors.get(i).copied().unwrap_or(v.color);
                if f.kind == Kind::Point {
                    for r in parts.iter().filter(|r| r.len() >= 2) {
                        let c = at(r[0], r[1]);
                        if squares && !hot {
                            squares_mesh.add_colored_rect(Rect::from_center_size(c, vec2(5.0, 5.0)), dark);
                            squares_mesh.add_colored_rect(Rect::from_center_size(c, vec2(3.0, 3.0)), col);
                            continue;
                        }
                        let rad = if hot { 6.0 } else { 4.0 };
                        painter.circle_filled(c, rad + 1.5, dark);
                        painter.circle_filled(c, rad, col);
                        if hot {
                            painter.circle_stroke(c, rad + 3.0, Stroke::new(2.0, Color32::WHITE));
                        }
                    }
                    budget -= 1;
                    continue;
                }
                // smaller than a couple of screen points: a dot
                if f64::from(b[2] - b[0]) * k < 2.0 && f64::from(b[3] - b[1]) * k < 2.0 {
                    let c = at(f64::from(b[0] + b[2]) * 0.5, f64::from(b[1] + b[3]) * 0.5);
                    painter.rect_filled(Rect::from_center_size(c, vec2(2.0, 2.0)), 0.0, col);
                    budget -= 1;
                    continue;
                }
                let closed = f.kind == Kind::Area;
                let thick = if hot { 3.0 } else { 1.0 };
                for r in parts {
                    pts.clear();
                    let n = r.len() / 2;
                    for (j, v) in r.as_chunks::<2>().0.iter().enumerate() {
                        let p = at(v[0], v[1]);
                        // a vertex per 1.5 screen points or so (the last one kept)
                        if let Some(&last) = pts.last()
                            && j + 1 < n
                            && (p - last).length_sq() < 2.25
                        {
                            continue;
                        }
                        pts.push(p);
                    }
                    if pts.len() < 2 {
                        continue;
                    }
                    budget = budget.saturating_sub(pts.len());
                    if quads && !hot {
                        add_polyline(&mut lines_mesh, &pts, closed, thick + 2.0, dark);
                        add_polyline(&mut lines_mesh, &pts, closed, thick, col);
                        continue;
                    }
                    let line =
                        |p: Vec<Pos2>, s: Stroke| if closed { Shape::closed_line(p, s) } else { Shape::line(p, s) };
                    painter.add(line(pts.clone(), Stroke::new(thick + 2.0, dark)));
                    painter.add(line(
                        std::mem::take(&mut pts),
                        Stroke::new(thick, if hot { Color32::WHITE } else { col }),
                    ));
                }
            }
            if !lines_mesh.is_empty() {
                painter.add(Shape::mesh(lines_mesh));
            }
            if !squares_mesh.is_empty() {
                painter.add(Shape::mesh(squares_mesh));
            }
        }
        if budget == 0 {
            painter.text(
                Pos2::new(clip.min.x + 10.0, clip.max.y - 52.0),
                egui::Align2::LEFT_TOP,
                "Vectors: too many vertices to draw at this zoom; zoom in",
                body_font(painter),
                Color32::from_rgb(255, 220, 120),
            );
        }
    }

    /// The feature under the active layer's point (x, y), within `tol` screen points: a point before a line
    /// before a polygon (the smallest of nested ones), the top layer first.
    pub fn vector_feature_at(&self, x: f64, y: f64, tol: f64) -> Option<(usize, usize)> {
        let t = tol / self.screen_per_px(); // in active pixels
        let (mut best_point, mut best_line, mut best_area) = (f64::INFINITY, f64::INFINITY, f64::INFINITY);
        let mut kind = 0; // of the best so far: 3 point, 2 line, 1 area
        let mut found = None;
        for (li, v) in self.vectors.iter().enumerate().rev() {
            if !v.visible || v.px.is_empty() {
                continue;
            }
            for (i, parts) in v.px.iter().enumerate() {
                let b = v.boxes[i].map(f64::from);
                if b[0] > b[2] || x < b[0] - t || x > b[2] + t || y < b[1] - t || y > b[3] + t {
                    continue;
                }
                match v.data.features[i].kind {
                    Kind::Point => {
                        for r in parts.iter().filter(|r| r.len() >= 2) {
                            let d = (r[0] - x).hypot(r[1] - y);
                            if d <= t && d < best_point {
                                (best_point, kind, found) = (d, 3, Some((li, i)));
                            }
                        }
                    }
                    Kind::Line if kind < 3 => {
                        for r in parts {
                            for s in r.windows(4).step_by(2) {
                                let d = segment_distance(x, y, s[0], s[1], s[2], s[3]);
                                if d <= t * 0.7 && d < best_line {
                                    (best_line, kind, found) = (d, 2, Some((li, i)));
                                }
                            }
                        }
                    }
                    Kind::Area if kind < 2 && plane::contains(parts, x, y) => {
                        let a = (b[2] - b[0]) * (b[3] - b[1]); // nested polygons: the smallest
                        if a < best_area {
                            (best_area, kind, found) = (a, 1, Some((li, i)));
                        }
                    }
                    _ => {}
                }
            }
        }
        found
    }

    /// A polygon of a vector layer as the ROI; false if it is not one (or covers no pixel).
    pub fn feature_as_roi(&mut self, li: usize, i: usize) -> bool {
        let Some(v) = self.vectors.get(li) else { return false };
        if v.data.features.get(i).is_none_or(|f| f.kind != Kind::Area) || i >= v.px.len() {
            return false;
        }
        let p: Polygon = v.px[i].iter().filter(|r| r.len() >= 6).cloned().collect();
        !p.is_empty() && self.set_roi_polygon(p)
    }

    /// A line of a vector layer as the transect (the longest part of a line of several).
    pub fn feature_as_transect(&mut self, li: usize, i: usize) -> bool {
        let Some(v) = self.vectors.get(li) else { return false };
        if v.data.features.get(i).is_none_or(|f| f.kind != Kind::Line) || i >= v.px.len() {
            return false;
        }
        let len = |r: &Ring| r.windows(4).step_by(2).map(|s| (s[2] - s[0]).hypot(s[3] - s[1])).sum::<f64>();
        let Some(best) = v.px[i].iter().filter(|r| r.len() >= 4).max_by(|a, b| len(a).total_cmp(&len(b))) else {
            return false;
        };
        let mut pts: Vec<[f64; 2]> = Vec::new();
        for p in best.as_chunks::<2>().0 {
            if pts.last().is_none_or(|l| (p[0] - l[0]).abs() + (p[1] - l[1]).abs() > 1e-3) {
                pts.push([p[0], p[1]]);
            }
        }
        // nearly straight vertices thinned to about one per pixel of the layer (a GPS track has thousands)
        let mut thin: Vec<[f64; 2]> = Vec::new();
        for (j, p) in pts.iter().enumerate() {
            if thin.last().is_none_or(|l| (p[0] - l[0]).hypot(p[1] - l[1]) >= 1.0) || j + 1 == pts.len() {
                thin.push(*p);
            }
        }
        if thin.len() < 2 {
            return false;
        }
        if thin.len() == 2 {
            // two vertices would be clipped like a drawn line: a polyline keeps them
            thin.insert(1, [(thin[0][0] + thin[1][0]) / 2.0, (thin[0][1] + thin[1][1]) / 2.0]);
        }
        self.set_transect(thin);
        if !self.transect.on {
            return false;
        }
        self.transect.fit = true;
        true
    }

    /// The map centred on a feature, at least `min_scale` screen points per pixel (a polygon or a line in view
    /// whole).
    pub fn center_on_feature(&mut self, li: usize, i: usize, min_scale: f64) {
        let Some(g) = self.grid() else { return };
        let Some(v) = self.vectors.get(li) else { return };
        let Some(&a) = v.anchor.get(i) else { return };
        if a[0].is_nan() {
            return;
        }
        let m = &mut self.map;
        let c = if m.canvas[0] > 0.0 { m.canvas } else { [800.0, 600.0] };
        // screen points per active pixel
        let mut s = (m.scale / g.fx).max(min_scale.min(crate::map::MAX_ZOOM * g.ow / g.w / g.fx));
        let b = v.boxes[i];
        let mid = if v.data.features[i].kind != Kind::Point && b[0] <= b[2] {
            let fit = 0.8
                * (f64::from(c[0]) / f64::from((b[2] - b[0]).max(1.0)))
                    .min(f64::from(c[1]) / f64::from((b[3] - b[1]).max(1.0)));
            s = s.min(fit).max(0.01);
            [f64::from(b[0] + b[2]) * 0.5, f64::from(b[1] + b[3]) * 0.5]
        } else {
            a
        };
        m.set_view([mid[0] / g.fx, mid[1] / g.fy], s * g.fx);
        m.touched = true;
    }

    /// The whole of a vector layer in view.
    pub(crate) fn zoom_to_vector(&mut self, li: usize) {
        let Some(g) = self.grid() else { return };
        let v = &self.vectors[li];
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for x in v.boxes.iter().filter(|x| x[0] <= x[2]) {
            b = [b[0].min(x[0].into()), b[1].min(x[1].into()), b[2].max(x[2].into()), b[3].max(x[3].into())];
        }
        let m = &mut self.map;
        if b[0] > b[2] || m.canvas[0] <= 0.0 {
            return;
        }
        let s = (0.9
            * (f64::from(m.canvas[0]) / (b[2] - b[0]).max(1.0)).min(f64::from(m.canvas[1]) / (b[3] - b[1]).max(1.0)))
        .clamp(0.01, crate::map::MAX_ZOOM);
        m.set_view([(b[0] + b[2]) * 0.5 / g.fx, (b[1] + b[3]) * 0.5 / g.fy], s * g.fx);
        m.touched = true;
    }

    /// The feature under the cursor: its attributes (points always, lines and polygons with Alt held).
    pub(crate) fn vector_tooltip(&self, resp: &egui::Response) {
        let Some((li, i)) = self.hover_vector else { return };
        let Some(v) = self.vectors.get(li) else { return };
        let Some(f) = v.data.features.get(i) else { return };
        if f.kind != Kind::Point && !resp.ctx.input(|i| i.modifiers.alt) {
            return;
        }
        resp.clone().on_hover_ui_at_pointer(|ui| {
            ui.colored_label(v.color, &v.name);
            let hint = match f.kind {
                Kind::Area => "  (Alt + click: use as the ROI)",
                Kind::Line => "  (Alt + click: transect along it)",
                _ => "",
            };
            ui.weak(format!("feature {}{hint}", f.fid));
            let shown = v.data.fields.len().min(16);
            egui::Grid::new("attrs").num_columns(2).show(ui, |ui| {
                for k in 0..shown {
                    ui.weak(&v.data.fields[k]);
                    ui.label(&f.values[k]);
                    ui.end_row();
                }
            });
            if v.data.fields.len() > shown {
                ui.weak(format!("... {} more fields", v.data.fields.len() - shown));
            }
        });
    }

    /// The status line's note of the feature under the cursor.
    pub(crate) fn vector_status(&self, alt: bool) -> Option<String> {
        let (li, i) = self.hover_vector?;
        let v = self.vectors.get(li)?;
        let f = v.data.features.get(i)?;
        Some(format!(
            "  |  {} #{}{}",
            v.name,
            f.fid,
            if f.kind == Kind::Point || alt { "" } else { " (Alt: attributes)" }
        ))
    }

    /// The Layers panel's vector layers: shown, colour, name (its file, CRS and fields), Extract series,
    /// Interpret, Zoom to it, Remove.
    /// The Extract series dialog for a vector layer: its table's name in the export folder.
    pub fn open_extract(&mut self, id: u32) {
        let Some(li) = self.vector_index(id) else { return };
        if self.session.is_none() {
            return;
        }
        let v = &self.vectors[li];
        let stem = std::path::Path::new(&v.data.path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let layer = self.layers.get(self.active).map(|l| format!("_{}", l.name)).unwrap_or_default();
        let name = crate::exports::sanitize(&format!("{stem}_series{layer}.csv"));
        let path = std::path::Path::new(&self.export_folder()).join(name).to_string_lossy().into_owned();
        self.extract = ExtractUi { open: true, vector: id, path, ..std::mem::take(&mut self.extract) };
    }

    pub(crate) fn extract_window(&mut self, ctx: &egui::Context) {
        if !self.extract.open {
            return;
        }
        let Some(li) = self.vector_index(self.extract.vector).filter(|_| self.session.is_some()) else {
            self.extract.open = false;
            return;
        };
        let visible = self.layers.iter().filter(|l| l.visible).count();
        let deferred = self.session.as_ref().is_some_and(|s| s.s.defer_random_reads());
        let (mut go, mut cancel) = (false, false);
        let mut target = String::new();
        egui::Modal::new(egui::Id::new("extract series")).show(ctx, |ui| {
            let v = &self.vectors[li];
            let e = &mut self.extract;
            ui.heading("Extract series");
            ui.colored_label(v.color, &v.name);
            ui.weak(v.data.kind_text());
            crate::panels::section(ui, "Series of");
            ui.radio_value(&mut e.all_layers, false, "The active layer");
            ui.radio_value(&mut e.all_layers, true, format!("Every visible layer ({visible})"));
            ui.weak(
                "Points: the pixel under each. Polygons: the mean of the pixels whose centres\n\
                 are inside (and their count). Lines are left out. Full resolution, the values\n\
                 the chart shows (band, normalized difference, quality mask applied).",
            );
            crate::panels::section(ui, "Table");
            ui.radio_value(&mut e.wide, true, "A row per feature, a column per date");
            ui.radio_value(&mut e.wide, false, "A row per feature and date (long)")
                .on_hover_text("Easier to read in R (tidyverse) or pandas, and with several layers of different dates");
            crate::panels::section(ui, "Save to");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut e.path)
                        .desired_width(ui.text_style_height(&egui::TextStyle::Body) * 34.0),
                );
                if ui.button("Browse...").clicked() {
                    let p = std::path::PathBuf::from(&e.path);
                    let mut d = rfd::FileDialog::new().add_filter("CSV table", &["csv"]);
                    if let Some(n) = p.file_name() {
                        d = d.set_file_name(n.to_string_lossy());
                    }
                    if let Some(dir) = p.parent().filter(|d| d.is_dir()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(f) = d.save_file() {
                        e.path = f.to_string_lossy().into_owned();
                    }
                }
            });
            target = e.path.trim().trim_matches('"').to_string();
            if !target.is_empty() && std::path::Path::new(&target).extension().is_none() {
                target += ".csv";
            }
            let folder_ok = !target.is_empty() && std::path::Path::new(&target).parent().is_some_and(|p| p.is_dir());
            if !folder_ok {
                ui.colored_label(theme::warning(), "Type a file name in an existing folder, or Browse...");
            } else if std::path::Path::new(&target).exists() {
                ui.colored_label(theme::warning(), "The file exists and will be replaced.");
            }
            if deferred {
                ui.colored_label(theme::warning(), "Building the overview from an HDD: available once it ends.");
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                go = ui
                    .add_enabled(folder_ok && !deferred, egui::Button::new("Extract").min_size(vec2(120.0, 0.0)))
                    .clicked();
                cancel = ui.add(egui::Button::new("Cancel").min_size(vec2(120.0, 0.0))).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if go {
            self.remember_export_folder(&target);
            let (all, wide) = (self.extract.all_layers, self.extract.wide);
            self.extract_series(li, all, wide, target, ctx);
        }
        if go || cancel {
            self.extract.open = false;
        }
    }

    /// The series at every feature of a vector layer (a point's pixel, a polygon's mean and its pixels) of the
    /// active layer or every visible one, written as a CSV table in the background.
    pub fn extract_series(
        &mut self,
        li: usize,
        all_layers: bool,
        wide: bool,
        path: String,
        ctx: &egui::Context,
    ) -> Option<Arc<ExportJob>> {
        struct Target {
            info: Arc<janus_cube::CubeInfo>,
            full_res: Arc<janus_cache::FullResCache>,
            name: String,
        }
        let mut targets: Vec<Target> = Vec::new();
        for i in 0..self.layers.len() {
            if i == self.active || (all_layers && self.layers[i].visible) {
                let s = self.layer_session(i)?;
                let t =
                    Target { info: s.info.clone(), full_res: s.s.full_res.clone(), name: self.layers[i].name.clone() };
                // the active layer first: its columns lead
                if i == self.active { targets.insert(0, t) } else { targets.push(t) }
            }
        }
        if targets.is_empty() {
            return None;
        }
        let v = &self.vectors[li];
        let data = v.data.clone();
        let title = format!(
            "Series at {} features of {}, {} layer{}",
            data.features.len(),
            v.name,
            targets.len(),
            if targets.len() == 1 { "" } else { "s" }
        );
        let job = start_export_job(title, path.clone(), ctx, move |j| {
            let (n, k_n) = (data.features.len(), targets.len());
            // values[k][i * T + t]; pixels[k][i]: pixels read (1 for a point, those inside a polygon)
            let mut values: Vec<Vec<f32>> = Vec::with_capacity(k_n);
            let mut pixels = vec![vec![0usize; n]; k_n];
            let work: usize = targets.iter().map(|t| n * t.info.t()).sum::<usize>().max(1);
            let mut done = 0usize;
            let areas = data.features.iter().filter(|f| f.kind == Kind::Area).count();
            for (k, tg) in targets.iter().enumerate() {
                if j.cancelled() {
                    return Err("cancelled".into());
                }
                let info = &tg.info;
                let t_n = info.t();
                values.push(vec![f32::NAN; n * t_n]);
                let Ok(p) = Placer::new(&data, &info.crs_wkt, info.geo_transform) else {
                    done += n * t_n;
                    continue;
                };
                let mut reader = janus_cube::CubeReader::new(info.clone());
                let mut read = |t: usize, win: (usize, usize, usize, usize), buf: &mut [f32], size: (usize, usize)| {
                    tg.full_res.read_window(t, win, buf, size) || reader.read_window(t, win, buf, size)
                };
                // points: date by date (each date's blocks stay in GDAL's cache), in row order
                let mut px = vec![(0usize, 0usize); n];
                let mut order: Vec<usize> = Vec::new();
                for (i, f) in data.features.iter().enumerate() {
                    if f.kind != Kind::Point {
                        continue;
                    }
                    let Some((x, y)) = p.to_pixel(f.parts[0][0], f.parts[0][1]) else { continue };
                    let (ix, iy) = (x.floor(), y.floor());
                    if ix < 0.0 || iy < 0.0 || ix >= info.width as f64 || iy >= info.height as f64 {
                        continue;
                    }
                    px[i] = (ix as usize, iy as usize);
                    pixels[k][i] = 1;
                    order.push(i);
                }
                order.sort_by_key(|&i| px[i].1);
                done += (n - order.len() - areas) * t_n; // lines, points off this layer: nothing to read
                for t in 0..t_n {
                    if j.cancelled() {
                        return Err("cancelled".into());
                    }
                    for &i in &order {
                        let mut v = [f32::NAN];
                        if read(t, (px[i].0, px[i].1, 1, 1), &mut v, (1, 1)) {
                            values[k][i * t_n + t] = v[0];
                        }
                    }
                    done += order.len();
                    j.set_progress(done as f64 / work as f64);
                }
                // polygons: the mean of the pixels whose centres are inside, read in their bounding box; date by date
                // and from the top down, so each date's blocks are decoded once (polygon by polygon, through every
                // date, GDAL's cache of a file in strips thrashes: 577 s for 5000 polygons over 10 dates in the C++)
                struct Win {
                    i: usize,
                    poly: Polygon,
                    win: (usize, usize, usize, usize),
                    size: (usize, usize),
                }
                let mut wins: Vec<Win> = Vec::new();
                for (i, f) in data.features.iter().enumerate() {
                    if f.kind != Kind::Area {
                        continue;
                    }
                    let poly: Polygon = p.placed(&f.parts).into_iter().filter(|r| r.len() >= 6).collect();
                    let Some(b) = plane::bounds(&poly) else { continue };
                    let (w, h) = (info.width as f64, info.height as f64);
                    let (x0, y0) = (b[0].floor().clamp(0.0, w) as usize, b[1].floor().clamp(0.0, h) as usize);
                    let (x1, y1) = (b[2].ceil().clamp(0.0, w) as usize, b[3].ceil().clamp(0.0, h) as usize);
                    if x1 <= x0 || y1 <= y0 {
                        continue;
                    }
                    let (pw, ph) = (x1 - x0, y1 - y0);
                    let s = (1.0e6 / (pw as f64 * ph as f64)).sqrt().min(1.0); // large ones read subsampled
                    let (bw, bh) = (((pw as f64 * s) as usize).max(1), ((ph as f64 * s) as usize).max(1));
                    let inside = plane::mask(&poly, x0 as f64, y0 as f64, pw as f64, ph as f64, bw, bh)
                        .iter()
                        .filter(|&&m| m)
                        .count();
                    pixels[k][i] = if s < 1.0 { (inside as f64 / (s * s)).round() as usize } else { inside };
                    if inside > 0 {
                        wins.push(Win { i, poly, win: (x0, y0, pw, ph), size: (bw, bh) });
                    }
                }
                wins.sort_by_key(|w| w.win.1);
                let skipped = areas - wins.len();
                done += skipped * t_n;
                let mut buf = Vec::new();
                for t in 0..t_n {
                    for w in &wins {
                        if j.cancelled() {
                            return Err("cancelled".into());
                        }
                        let (x0, y0, pw, ph) = w.win;
                        let (bw, bh) = w.size;
                        buf.resize(bw * bh, 0f32);
                        if !read(t, w.win, &mut buf, w.size) {
                            continue;
                        }
                        let mask = plane::mask(&w.poly, x0 as f64, y0 as f64, pw as f64, ph as f64, bw, bh);
                        let (mut sum, mut c) = (0f64, 0usize);
                        for (&m, &v) in mask.iter().zip(&buf) {
                            if m && v.is_finite() {
                                sum += f64::from(v);
                                c += 1;
                            }
                        }
                        if c > 0 {
                            values[k][w.i * t_n + t] = (sum / c as f64) as f32;
                        }
                    }
                    done += wins.len();
                    j.set_progress(done as f64 / work as f64);
                }
            }
            // the table: the feature's fields, its place in lon/lat, then the values
            let ll = Placer::lon_lat(&data);
            let mut out = String::from("fid");
            for f in &data.fields {
                out += &format!(",{}", csv_field(f));
            }
            out += ",lon,lat";
            let col = |k: usize, t: usize| {
                let d = &targets[k].info.layers[t].label;
                if k_n == 1 { d.clone() } else { format!("{} {d}", targets[k].name) }
            };
            let pixels_name =
                |k: usize| if k_n == 1 { "pixels".to_string() } else { format!("pixels {}", targets[k].name) };
            if wide {
                for (k, tg) in targets.iter().enumerate() {
                    if data.areas > 0 {
                        out += &format!(",{}", csv_field(&pixels_name(k)));
                    }
                    for t in 0..tg.info.t() {
                        out += &format!(",{}", csv_field(&col(k, t)));
                    }
                }
            } else {
                out += ",layer,date,value";
                if data.areas > 0 {
                    out += ",pixels";
                }
            }
            out.push('\n');
            for (i, f) in data.features.iter().enumerate() {
                if f.kind != Kind::Point && f.kind != Kind::Area {
                    continue;
                }
                let mut head = f.fid.to_string();
                for v in &f.values {
                    head += &format!(",{}", csv_field(v));
                }
                head += &match ll.to_lon_lat(f.cx, f.cy) {
                    Some((lon, lat)) => format!(",{},{}", number(lon), number(lat)),
                    None => ",,".into(),
                };
                if wide {
                    out += &head;
                    for k in 0..k_n {
                        let t_n = targets[k].info.t();
                        if data.areas > 0 {
                            out += &format!(",{}", pixels[k][i]);
                        }
                        for t in 0..t_n {
                            out += &format!(",{}", number(f64::from(values[k][i * t_n + t])));
                        }
                    }
                    out.push('\n');
                } else {
                    for k in 0..k_n {
                        let t_n = targets[k].info.t();
                        for t in 0..t_n {
                            out += &format!(
                                "{head},{},{},{}",
                                csv_field(&targets[k].name),
                                csv_field(&targets[k].info.layers[t].label),
                                number(f64::from(values[k][i * t_n + t]))
                            );
                            if data.areas > 0 {
                                out += &format!(",{}", pixels[k][i]);
                            }
                            out.push('\n');
                        }
                    }
                }
            }
            write_text_file(&path, &out)
        });
        self.exports.push(job.clone());
        self.show_exports = true;
        Some(job)
    }
}
