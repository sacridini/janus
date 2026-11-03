//! The ROI as a shape to keep and to adjust. Save ROI as vector... writes it as a GeoPackage or GeoJSON (by
//! extension) in the active layer's CRS (janus_geo::polygon_file), opened as a vector layer if asked: Alt + click on
//! it makes the same ROI again, the same pixels inside. Edit ROI vertices (Shift+R) adjusts its polygon on the map:
//! drag a vertex; drag the handle in the middle of an edge for a new vertex there; right click or Delete on a vertex
//! removes it (a ring keeps three). A rectangle becomes the polygon of its corners; a polygon of a vector layer taken
//! as the ROI (Alt + click) is edited as the ROI, its file left as it is (Save ROI as vector keeps the new shape).
//! Each change is one step of Edit > Undo (the ROI's, undo.rs): the polygon is the ROI once the button is released.
//! Not a vector editor: the IDEIAS left full editing out (it pulls towards a GIS).

use egui::{Color32, Pos2, Rect, Response, Shape, Stroke, Ui, vec2};
use janus_geo::plane::{self, Polygon};
use janus_geo::polygon_file::{FieldValue, write_polygon};

use crate::app::State;
use crate::map::body_font;
use crate::theme;

/// A handle of the polygon being edited: a vertex (ring, vertex), or the middle of the edge from vertex i to the
/// next (ring, i).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    Vertex(usize, usize),
    Middle(usize, usize),
}

/// Screen points within which a vertex, then an edge's middle, is under the cursor.
const VERTEX_PICK: f64 = 8.0;
const MIDDLE_PICK: f64 = 7.0;
/// Handles drawn while at most this many vertices are on screen (the hovered one always).
const MAX_HANDLES: usize = 3000;

#[derive(Default)]
pub struct RoiEdit {
    pub on: bool,
    /// the polygon being edited (the ROI's, in active pixels; even-odd rings)
    pub poly: Polygon,
    /// the vertex being dragged, on the canvas `panel`
    pub drag: Option<(usize, usize)>,
    panel: Option<egui::Id>,
    /// the handle under the cursor, on that canvas
    pub hover: Option<(egui::Id, Handle)>,
    /// the ROI it was taken from (roi_gen): taken again when the ROI changes otherwise (undo, another ROI)
    seen: u64,
    /// the map canvases seen while editing (the self-test points at them)
    pub rects: Vec<(egui::Id, Rect)>,
    /// the self-test's pointer: its events in place of the mouse's while it drives the map
    pub test_pointer: bool,
    pub events: Vec<egui::Event>,
}

/// Save ROI as vector: the file, whether it opens as a vector layer, what happened.
#[derive(Default)]
pub struct RoiSaveUi {
    pub open: bool,
    pub path: String,
    pub open_layer: bool,
    pub status: String,
}

/// Vertex i of ring r moved to p.
pub fn move_vertex(poly: &mut Polygon, r: usize, i: usize, p: [f64; 2]) {
    if let Some(v) = poly.get_mut(r).and_then(|ring| ring.get_mut(i * 2..i * 2 + 2)) {
        v.copy_from_slice(&p);
    }
}

/// A vertex at p after vertex i of ring r: its place.
pub fn insert_vertex(poly: &mut Polygon, r: usize, i: usize, p: [f64; 2]) -> usize {
    let ring = &mut poly[r];
    let at = (i + 1).min(ring.len() / 2);
    ring.splice(at * 2..at * 2, p);
    at
}

/// Vertex i of ring r removed, unless the ring would keep fewer than three.
pub fn remove_vertex(poly: &mut Polygon, r: usize, i: usize) -> bool {
    match poly.get_mut(r) {
        Some(ring) if ring.len() / 2 > 3 && i < ring.len() / 2 => {
            ring.drain(i * 2..i * 2 + 2);
            true
        }
        _ => false,
    }
}

/// The middle of the edge from vertex i of `ring` to the next.
fn middle(ring: &[f64], i: usize) -> [f64; 2] {
    let n = ring.len() / 2;
    let j = (i + 1) % n;
    [(ring[i * 2] + ring[j * 2]) / 2.0, (ring[i * 2 + 1] + ring[j * 2 + 1]) / 2.0]
}

impl State {
    /// The ROI's rings in the active layer's pixels: its polygon, or its rectangle's corners.
    pub(crate) fn roi_shape(&self) -> Option<Polygon> {
        let r = self.series.roi_rect?;
        if !self.series.roi_poly.is_empty() {
            return Some(self.series.roi_poly.clone());
        }
        let [x0, y0, x1, y1] = r.map(|v| v as f64);
        Some(vec![vec![x0, y0, x1, y0, x1, y1, x0, y1]])
    }

    /// Edit ROI vertices on or off (Shift+R): on only with an ROI.
    pub fn toggle_roi_edit(&mut self) {
        let on = !self.roi_edit.on;
        self.set_roi_edit(on);
    }

    pub fn set_roi_edit(&mut self, on: bool) {
        let shape = if on { self.roi_shape() } else { None };
        let e = &mut self.roi_edit;
        (e.drag, e.panel, e.hover) = (None, None, None);
        match shape {
            Some(p) => {
                (e.on, e.poly, e.seen) = (true, p, self.series.roi_gen);
                // drawing a polygon or a transect stops
                self.series.poly = Default::default();
                self.transect_draft.mode = false;
            }
            None => (e.on, e.poly) = (false, Vec::new()),
        }
    }

    /// The ROI changed otherwise (undone, drawn again, cleared): its shape taken again, or the editing ends.
    fn follow_roi_for_edit(&mut self) {
        let e = &self.roi_edit;
        if !e.on || e.drag.is_some() || e.seen == self.series.roi_gen {
            return;
        }
        match self.roi_shape() {
            Some(p) => (self.roi_edit.poly, self.roi_edit.seen) = (p, self.series.roi_gen),
            None => self.set_roi_edit(false),
        }
    }

    /// The polygon edited becomes the ROI (a step of Undo); false (and the ROI's shape back) if it covers no pixel's
    /// centre. Nothing when it did not change.
    pub fn commit_roi_edit(&mut self) -> bool {
        if self.roi_shape().as_ref() == Some(&self.roi_edit.poly) {
            return true;
        }
        let ok = self.set_roi_polygon(self.roi_edit.poly.clone());
        if !ok && let Some(p) = self.roi_shape() {
            self.roi_edit.poly = p;
        }
        self.roi_edit.seen = self.series.roi_gen;
        ok
    }

    /// Vertex i of ring r removed from the ROI (a ring keeps three).
    pub fn remove_roi_vertex(&mut self, r: usize, i: usize) -> bool {
        remove_vertex(&mut self.roi_edit.poly, r, i) && self.commit_roi_edit()
    }

    /// The handle at the active layer's point p: a vertex within 8 screen points, else an edge's middle within 7
    /// (edges longer than 24 points).
    pub(crate) fn roi_handle_at(&self, p: [f64; 2]) -> Option<Handle> {
        let k = self.screen_per_px();
        let d = |a: [f64; 2]| (a[0] - p[0]).hypot(a[1] - p[1]) * k;
        let mut best: Option<(f64, Handle)> = None;
        for (r, ring) in self.roi_edit.poly.iter().enumerate() {
            for (i, v) in ring.as_chunks::<2>().0.iter().enumerate() {
                let dv = d(*v);
                if dv <= VERTEX_PICK && best.is_none_or(|b| dv < b.0) {
                    best = Some((dv, Handle::Vertex(r, i)));
                }
            }
        }
        if best.is_some() {
            return best.map(|b| b.1);
        }
        for (r, ring) in self.roi_edit.poly.iter().enumerate() {
            let n = ring.len() / 2;
            for i in 0..n {
                let j = (i + 1) % n;
                let len = (ring[j * 2] - ring[i * 2]).hypot(ring[j * 2 + 1] - ring[i * 2 + 1]) * k;
                let dm = d(middle(ring, i));
                if len > 24.0 && dm <= MIDDLE_PICK && best.is_none_or(|b| dm < b.0) {
                    best = Some((dm, Handle::Middle(r, i)));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// While editing: a press on a vertex drags it, on an edge's middle drags a new vertex; released, the polygon is
    /// the ROI; a right click on a vertex removes it. Returns whether the pointer was taken (a drag, or over a
    /// handle: no pan, pin or feature picked).
    pub(crate) fn roi_edit_input(&mut self, ui: &Ui, resp: &Response, p: Option<[f64; 2]>) -> bool {
        self.follow_roi_for_edit();
        if !self.roi_edit.on {
            return false;
        }
        let id = resp.id;
        match self.roi_edit.rects.iter_mut().find(|r| r.0 == id) {
            Some(r) => r.1 = resp.rect,
            None => self.roi_edit.rects.push((id, resp.rect)),
        }
        if let Some((r, i)) = self.roi_edit.drag
            && self.roi_edit.panel == Some(id)
        {
            if resp.is_pointer_button_down_on() {
                if let Some(p) = p {
                    move_vertex(&mut self.roi_edit.poly, r, i, p);
                }
                ui.ctx().request_repaint();
            } else {
                (self.roi_edit.drag, self.roi_edit.panel) = (None, None);
                self.commit_roi_edit();
            }
            return true;
        }
        if self.roi_edit.hover.is_some_and(|h| h.0 == id) {
            self.roi_edit.hover = None;
        }
        if !resp.hovered() {
            return false;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace)) {
            return true; // a vertex's (roi_edit_shortcuts), not the last pin's
        }
        let Some(p) = p else { return false };
        let h = self.roi_handle_at(p);
        if let Some(h) = h {
            self.roi_edit.hover = Some((id, h));
        }
        let (pressed, shift) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift));
        if pressed && !shift {
            let e = &mut self.roi_edit;
            match h {
                Some(Handle::Vertex(r, i)) => (e.drag, e.panel) = (Some((r, i)), Some(id)),
                Some(Handle::Middle(r, i)) => {
                    let m = middle(&e.poly[r], i);
                    let at = insert_vertex(&mut e.poly, r, i, m);
                    (e.drag, e.panel, e.hover) = (Some((r, at)), Some(id), Some((id, Handle::Vertex(r, at))));
                }
                None => return false,
            }
            return true;
        }
        if resp.secondary_clicked()
            && let Some(Handle::Vertex(r, i)) = h
        {
            self.remove_roi_vertex(r, i);
            self.roi_edit.hover = None;
            return true;
        }
        h.is_some()
    }

    /// Shift+R toggles the editing; while editing, Delete / Backspace remove the vertex under the cursor, Esc or
    /// Enter end it.
    pub(crate) fn roi_edit_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        let (toggle, del, end) = ctx.input(|i| {
            (
                i.key_pressed(Key::R) && i.modifiers.shift && !i.modifiers.command && !i.modifiers.alt,
                i.key_pressed(Key::Delete) || i.key_pressed(Key::Backspace),
                i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter),
            )
        });
        if toggle {
            self.toggle_roi_edit();
            return;
        }
        if !self.roi_edit.on || self.roi_edit.drag.is_some() {
            return;
        }
        if del && let Some((_, Handle::Vertex(r, i))) = self.roi_edit.hover {
            self.remove_roi_vertex(r, i);
            self.roi_edit.hover = None;
        }
        if end {
            self.set_roi_edit(false);
        }
    }

    /// The polygon being edited over a map canvas: its outline, a square at each vertex and a dot in the middle of
    /// each edge (while not too many are on screen), the one under the cursor larger, and a hint.
    pub(crate) fn draw_roi_edit_marks(&self, painter: &egui::Painter, rect: Rect, at: &dyn Fn(f64, f64) -> Pos2) {
        let e = &self.roi_edit;
        if !e.on {
            return;
        }
        let roi = crate::series::ROI_COLOR;
        let dark = Color32::from_black_alpha(200);
        let view = rect.expand(10.0);
        let on_screen: usize =
            e.poly.iter().map(|r| r.as_chunks::<2>().0.iter().filter(|v| view.contains(at(v[0], v[1]))).count()).sum();
        let hover = e.hover.map(|h| h.1);
        for (r, ring) in e.poly.iter().enumerate() {
            let pts: Vec<Pos2> = ring.as_chunks::<2>().0.iter().map(|v| at(v[0], v[1])).collect();
            if pts.len() < 2 {
                continue;
            }
            painter.add(Shape::closed_line(pts.clone(), Stroke::new(3.5, dark)));
            painter.add(Shape::closed_line(pts.clone(), Stroke::new(1.5, Color32::WHITE)));
            let n = pts.len();
            for i in 0..n {
                let (a, b) = (pts[i], pts[(i + 1) % n]);
                let hot_v = hover == Some(Handle::Vertex(r, i)) || e.drag == Some((r, i));
                let hot_m = hover == Some(Handle::Middle(r, i));
                if on_screen > MAX_HANDLES && !hot_v && !hot_m {
                    continue;
                }
                if view.contains(a) {
                    let s = if hot_v { 11.0 } else { 7.0 };
                    let sq = Rect::from_center_size(a, vec2(s, s));
                    painter.rect_filled(sq.expand(1.5), 1.0, dark);
                    painter.rect_filled(sq, 1.0, if hot_v { roi } else { Color32::WHITE });
                }
                let m = Pos2::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                if (b - a).length() > 24.0 && view.contains(m) {
                    painter.circle_filled(m, if hot_m { 5.5 } else { 3.5 }, dark);
                    painter.circle_filled(m, if hot_m { 4.0 } else { 2.0 }, if hot_m { roi } else { Color32::WHITE });
                }
            }
        }
        if let Some(h) = painter.ctx().pointer_hover_pos().filter(|p| rect.contains(*p))
            && e.drag.is_none()
        {
            let hint = match hover {
                Some(Handle::Vertex(..)) => "drag: move the vertex; right click / Delete: remove it",
                Some(Handle::Middle(..)) => "drag: a new vertex here",
                None => "Editing the ROI: drag a vertex or an edge's middle (Shift+R / Esc: done)",
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

    /// The self-test's pointer events in place of the mouse's (raw_input_hook).
    pub(crate) fn roi_edit_test_input(&mut self, raw: &mut egui::RawInput) {
        let e = &mut self.roi_edit;
        if !e.test_pointer {
            return;
        }
        raw.events.retain(|e| {
            !matches!(
                e,
                egui::Event::PointerMoved(_)
                    | egui::Event::MouseMoved(_)
                    | egui::Event::PointerButton { .. }
                    | egui::Event::PointerGone
            )
        });
        raw.events.append(&mut e.events);
    }

    /// Pixels of the active layer whose centres are in the ROI (counted on a grid of at most 16 M cells over its box,
    /// scaled up beyond).
    pub(crate) fn roi_pixels(&self) -> usize {
        let Some([x0, y0, x1, y1]) = self.series.roi_rect else { return 0 };
        let (w, h) = (x1 - x0, y1 - y0);
        if self.series.roi_poly.is_empty() {
            return w * h;
        }
        let s = (16.0e6 / (w as f64 * h as f64)).sqrt().min(1.0);
        let (bw, bh) = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
        let inside = plane::mask(&self.series.roi_poly, x0 as f64, y0 as f64, w as f64, h as f64, bw, bh)
            .iter()
            .filter(|&&m| m)
            .count();
        if s < 1.0 { (inside as f64 * (w * h) as f64 / (bw * bh) as f64).round() as usize } else { inside }
    }

    /// The ROI written to `path` (.gpkg or .geojson) in the active layer's CRS, a feature with its name, the layer's
    /// and its pixels; opened as a vector layer if `open`. Its pixels.
    pub fn save_roi_vector(&mut self, path: &str, open: bool, ctx: &egui::Context) -> Result<usize, String> {
        let shape = self.roi_shape().ok_or("there is no ROI")?;
        let info = self.session.as_ref().map(|s| s.info.clone()).ok_or("open a series")?;
        if info.geo_transform.is_none() {
            return Err("the active layer has no georeferencing".into());
        }
        let mut rings = shape;
        plane::transform(&mut rings, |x, y| info.pixel_to_geo(x, y));
        let pixels = self.roi_pixels();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let fields = [
            ("name", FieldValue::Text("ROI".into())),
            ("layer", FieldValue::Text(layer)),
            ("pixels", FieldValue::Integer(pixels as i64)),
        ];
        write_polygon(path, "roi", &info.crs_wkt, &rings, &fields)?;
        if open {
            self.add_vector_files(vec![path.to_string()], ctx);
        }
        Ok(pixels)
    }

    /// Save ROI as vector...: its file in the export folder, named after the active layer.
    pub fn open_roi_save(&mut self) {
        if self.series.roi_rect.is_none() {
            return;
        }
        let s = &mut self.roi_save;
        if s.path.is_empty() || !std::path::Path::new(&s.path).parent().is_some_and(|p| p.is_dir()) {
            let layer = self.layers.get(self.active).map_or("janus".to_string(), |l| l.name.clone());
            let name = crate::exports::sanitize(&format!("{layer}_roi.gpkg"));
            let path = std::path::Path::new(&self.export_folder()).join(name).to_string_lossy().into_owned();
            self.roi_save.path = path;
            self.roi_save.open_layer = true;
        }
        self.roi_save.open = true;
        self.roi_save.status.clear();
    }

    pub(crate) fn roi_save_window(&mut self, ctx: &egui::Context) {
        if !self.roi_save.open {
            return;
        }
        if self.series.roi_rect.is_none() || self.session.is_none() {
            self.roi_save.open = false;
            return;
        }
        let pixels = self.roi_pixels();
        let kind = if self.series.roi_poly.is_empty() {
            "a rectangle".to_string()
        } else {
            let n: usize = self.series.roi_poly.iter().map(|r| r.len() / 2).sum();
            format!("a polygon of {n} vertices")
        };
        // the CRS by its name and code ("WGS 84 EPSG:4326"), either alone if the other is missing
        let crs = self
            .session
            .as_ref()
            .map_or(String::new(), |s| format!("{} {}", s.info.crs_name, s.info.crs_authority).trim().to_string());
        let (mut go, mut cancel) = (false, false);
        let mut target = String::new();
        egui::Modal::new(egui::Id::new("save roi")).show(ctx, |ui| {
            let s = &mut self.roi_save;
            ui.heading("Save ROI as vector");
            ui.weak(format!(
                "The ROI, {kind} over {pixels} pixels, in the active layer's CRS ({}).",
                if crs.is_empty() { "its own" } else { &crs }
            ));
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut s.path)
                        .desired_width(ui.text_style_height(&egui::TextStyle::Body) * 30.0),
                );
                if ui.button("Browse...").clicked() {
                    let p = std::path::PathBuf::from(&s.path);
                    let mut d =
                        rfd::FileDialog::new().add_filter("GeoPackage", &["gpkg"]).add_filter("GeoJSON", &["geojson"]);
                    if let Some(n) = p.file_name() {
                        d = d.set_file_name(n.to_string_lossy());
                    }
                    if let Some(dir) = p.parent().filter(|d| d.is_dir()) {
                        d = d.set_directory(dir);
                    }
                    if let Some(f) = d.save_file() {
                        s.path = f.to_string_lossy().into_owned();
                    }
                }
            });
            target = s.path.trim().trim_matches('"').to_string();
            if !target.is_empty() && std::path::Path::new(&target).extension().is_none() {
                target += ".gpkg";
            }
            let ext = std::path::Path::new(&target)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            let folder_ok = !target.is_empty() && std::path::Path::new(&target).parent().is_some_and(|p| p.is_dir());
            let ext_ok = matches!(ext.as_str(), "gpkg" | "geojson" | "json");
            if !folder_ok {
                ui.colored_label(theme::warning(), "Type a file name in an existing folder, or Browse...");
            } else if !ext_ok {
                ui.colored_label(theme::warning(), "A GeoPackage (.gpkg) or GeoJSON (.geojson)");
            } else if std::path::Path::new(&target).exists() {
                ui.colored_label(theme::warning(), "The file exists and will be replaced.");
            }
            ui.checkbox(&mut s.open_layer, "Open it as a vector layer")
                .on_hover_text("Alt + click on its polygon makes the same ROI again");
            if !s.status.is_empty() {
                ui.colored_label(theme::warning(), &s.status);
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                go =
                    ui.add_enabled(folder_ok && ext_ok, egui::Button::new("Save").min_size(vec2(120.0, 0.0))).clicked();
                cancel = ui.add(egui::Button::new("Cancel").min_size(vec2(120.0, 0.0))).clicked()
                    || ui.input(|i| i.key_pressed(egui::Key::Escape));
            });
        });
        if go {
            self.remember_export_folder(&target);
            let open = self.roi_save.open_layer;
            match self.save_roi_vector(&target, open, ctx) {
                Ok(_) => self.roi_save.open = false,
                Err(e) => self.roi_save.status = e,
            }
        }
        if cancel {
            self.roi_save.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vertices_moved_inserted_removed() {
        let mut p: Polygon = vec![vec![0.0, 0.0, 10.0, 0.0, 10.0, 10.0, 0.0, 10.0]];
        move_vertex(&mut p, 0, 2, [12.0, 14.0]);
        assert_eq!(p[0][4..6], [12.0, 14.0]);
        // after the last vertex: the edge back to the first
        assert_eq!(middle(&p[0], 3), [0.0, 5.0]);
        let at = insert_vertex(&mut p, 0, 3, [-2.0, 5.0]);
        assert_eq!(at, 4);
        assert_eq!(p[0], vec![0.0, 0.0, 10.0, 0.0, 12.0, 14.0, 0.0, 10.0, -2.0, 5.0]);
        assert!(remove_vertex(&mut p, 0, 4));
        assert!(remove_vertex(&mut p, 0, 0));
        assert_eq!(p[0], vec![10.0, 0.0, 12.0, 14.0, 0.0, 10.0]);
        assert!(!remove_vertex(&mut p, 0, 0), "a ring keeps three vertices");
        assert!(!remove_vertex(&mut p, 1, 0));
        move_vertex(&mut p, 3, 0, [1.0, 1.0]); // no such ring: nothing
    }
}
