//! Labels of a vector layer's features on the map, from a column of its table (field names, municipalities, CAR
//! ids...): the layer's style (its menu's Labels; kept in the project and copied with Copy style), drawn over the
//! map panels and in what the map exports (figures, animations).
//!
//! - Where: a polygon's label at its point inside (janus_geo's interior point, computed once when read); a line's
//!   at the middle of its length; a point's beside it.
//! - Which: a polygon or a line only when its box on screen is at least twice the label's height; no two labels
//!   overlap: the larger features first (a polygon's box, a line's length), each label skipped if it would cover
//!   one already placed (greedy, as a GIS's labeling engine without moving them), at most MAX_LABELS a view.
//! - Fast at a million features: the column read on a thread (OGR skipping the geometries and the other fields),
//!   the label points and sizes made on a thread once per placement, in a grid of cells over the layer, each cell's
//!   features sorted from the largest: a view takes from the cells it sees the features large enough, and stops
//!   at the first too small.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex};

use egui::{Color32, FontFamily, FontId, Pos2, Rect, Ui, vec2};
use janus_geo::plane::Polygon;
use janus_geo::vectors::Kind;
use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::vectors::VectorLayer;

/// Labels placed in a view, at most.
pub const MAX_LABELS: usize = 1500;
/// Features tried in a view, at most (the largest).
const MAX_TRIES: usize = 6000;

/// A layer's labels: the column (none: ""), their size in points, colour and halo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LabelStyle {
    pub field: String,
    pub size: f32,
    pub color: [u8; 4],
    pub halo: bool,
    pub halo_color: [u8; 4],
}

impl Default for LabelStyle {
    fn default() -> Self {
        LabelStyle {
            field: String::new(),
            size: 12.0,
            color: [255, 255, 255, 255],
            halo: true,
            halo_color: [0, 0, 0, 220],
        }
    }
}

impl LabelStyle {
    pub fn on(&self) -> bool {
        !self.field.is_empty()
    }

    pub fn color32(&self) -> Color32 {
        let [r, g, b, a] = self.color;
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    pub fn halo32(&self) -> Color32 {
        let [r, g, b, a] = self.halo_color;
        Color32::from_rgba_unmultiplied(r, g, b, a)
    }

    /// As fields of a style's text (style.rs): ";labels=...;lsize=...;lcolor=...;lhalo=..." (nothing when off).
    pub fn to_text(&self) -> String {
        if !self.on() {
            return String::new();
        }
        let hex = |c: &[u8; 4]| c.iter().map(|v| format!("{v:02x}")).collect::<String>();
        format!(
            ";labels={};lsize={};lcolor={};lhalo={}",
            crate::style::esc(&self.field),
            self.size,
            hex(&self.color),
            if self.halo { hex(&self.halo_color) } else { "-".into() }
        )
    }

    /// Read back from a style's fields (`get`: a field's value).
    pub fn from_fields(get: &dyn Fn(&str) -> Option<String>) -> LabelStyle {
        let d = LabelStyle::default();
        let Some(field) = get("labels") else { return d };
        let unhex = |s: &str| -> Option<[u8; 4]> {
            let mut o = [0u8; 4];
            for (k, v) in o.iter_mut().enumerate() {
                *v = u8::from_str_radix(s.get(2 * k..2 * k + 2)?, 16).ok()?;
            }
            (s.len() == 8).then_some(o)
        };
        let halo = get("lhalo");
        LabelStyle {
            field: crate::style::unesc(&field),
            size: get("lsize").and_then(|s| s.parse().ok()).unwrap_or(d.size).clamp(6.0, 48.0),
            color: get("lcolor").as_deref().and_then(unhex).unwrap_or(d.color),
            halo: halo.as_deref() != Some("-"),
            halo_color: halo.as_deref().and_then(unhex).unwrap_or(d.halo_color),
        }
    }
}

/// Each feature's label point (active pixels; NaN: none) and size (a polygon's box's larger side, a line's length;
/// points 0), and a grid of cells over the points, each cell's features from the largest.
pub struct Prepared {
    /// the placement it is for
    key: u64,
    at: Vec<[f32; 2]>,
    size: Vec<f32>,
    point: Vec<bool>,
    bounds: [f64; 4],
    cells: usize,
    grid: Vec<Vec<u32>>,
}

/// A line's point at the middle of its length (its parts one after the other).
pub fn line_middle(parts: &Polygon) -> Option<([f64; 2], f64)> {
    let len = |r: &[f64]| r.windows(4).step_by(2).map(|s| (s[2] - s[0]).hypot(s[3] - s[1])).sum::<f64>();
    let total: f64 = parts.iter().map(|r| len(r)).sum();
    let mut half = total / 2.0;
    for r in parts.iter().filter(|r| r.len() >= 4) {
        for s in r.windows(4).step_by(2) {
            let l = (s[2] - s[0]).hypot(s[3] - s[1]);
            if l >= half && l > 0.0 {
                let k = half / l;
                return Some(([s[0] + k * (s[2] - s[0]), s[1] + k * (s[3] - s[1])], total));
            }
            half -= l;
        }
    }
    parts.iter().find(|r| r.len() >= 2).map(|r| ([r[0], r[1]], total))
}

impl Prepared {
    /// From a layer as placed: its features' parts, boxes and points in active pixels, their kinds.
    pub fn new(px: &[Polygon], boxes: &[[f32; 4]], anchor: &[[f64; 2]], kinds: &[Kind], key: u64) -> Prepared {
        let n = px.len();
        let (mut at, mut size, mut point) = (vec![[f32::NAN; 2]; n], vec![0f32; n], vec![false; n]);
        for i in 0..n {
            let b = boxes.get(i).copied().unwrap_or([1.0, 1.0, 0.0, 0.0]);
            if b[0] > b[2] {
                continue;
            }
            match kinds.get(i).copied().unwrap_or_default() {
                Kind::Area => {
                    let a = anchor.get(i).copied().unwrap_or([f64::NAN; 2]);
                    at[i] = [a[0] as f32, a[1] as f32];
                    size[i] = (b[2] - b[0]).max(b[3] - b[1]);
                }
                Kind::Line => {
                    if let Some((m, len)) = line_middle(&px[i]) {
                        at[i] = [m[0] as f32, m[1] as f32];
                        size[i] = len as f32;
                    }
                }
                Kind::Point => {
                    at[i] = [b[0], b[1]];
                    point[i] = true;
                }
                Kind::None => {}
            }
        }
        let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for a in at.iter().filter(|a| a[0].is_finite() && a[1].is_finite()) {
            let (x, y) = (f64::from(a[0]), f64::from(a[1]));
            bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
        }
        // about 16 features a cell, 256 x 256 cells at most
        let cells = ((n as f64 / 16.0).sqrt() as usize).clamp(1, 256);
        let mut grid: Vec<Vec<u32>> = vec![Vec::new(); cells * cells];
        let mut p = Prepared { key, at, size, point, bounds, cells, grid: Vec::new() };
        for i in 0..n {
            let a = p.at[i];
            if a[0].is_finite() && a[1].is_finite() {
                let (cx, cy) = p.cell(f64::from(a[0]), f64::from(a[1]));
                grid[cy * cells + cx].push(i as u32);
            }
        }
        for c in &mut grid {
            c.sort_by(|&a, &b| {
                let (a, b) = (a as usize, b as usize);
                p.point[a].cmp(&p.point[b]).then(p.size[b].total_cmp(&p.size[a])).then(a.cmp(&b))
            });
        }
        p.grid = grid;
        p
    }

    fn cell(&self, x: f64, y: f64) -> (usize, usize) {
        let b = self.bounds;
        let n = self.cells as f64;
        let fx = ((x - b[0]) / (b[2] - b[0]).max(1e-9) * n).floor().clamp(0.0, n - 1.0);
        let fy = ((y - b[1]) / (b[3] - b[1]).max(1e-9) * n).floor().clamp(0.0, n - 1.0);
        (fx as usize, fy as usize)
    }

    /// The features whose label point is in `view` (active pixels) large enough at `k` screen points per pixel
    /// (`min`: screen points; points always), the largest first, MAX_TRIES at most.
    pub fn candidates(&self, view: [f64; 4], k: f64, min: f64) -> Vec<usize> {
        if self.grid.is_empty() || view[0] > self.bounds[2] || view[2] < self.bounds[0] {
            return Vec::new();
        }
        if view[1] > self.bounds[3] || view[3] < self.bounds[1] {
            return Vec::new();
        }
        let (x0, y0) = self.cell(view[0], view[1]);
        let (x1, y1) = self.cell(view[2], view[3]);
        let min_px = (min / k) as f32;
        let mut out: Vec<usize> = Vec::new();
        for cy in y0..=y1 {
            for cx in x0..=x1 {
                // the cell's features the largest first, its points after them
                let cell = &self.grid[cy * self.cells + cx];
                let points = cell.partition_point(|&i| !self.point[i as usize]);
                let large = cell[..points].partition_point(|&i| self.size[i as usize] >= min_px);
                for &i in cell[..large].iter().chain(&cell[points..]) {
                    let i = i as usize;
                    let a = self.at[i];
                    let (x, y) = (f64::from(a[0]), f64::from(a[1]));
                    if x >= view[0] && x <= view[2] && y >= view[1] && y <= view[3] {
                        out.push(i);
                    }
                }
            }
        }
        let order = |a: &usize, b: &usize| self.size[*b].total_cmp(&self.size[*a]).then(a.cmp(b));
        if out.len() > MAX_TRIES {
            out.select_nth_unstable_by(MAX_TRIES, order);
            out.truncate(MAX_TRIES);
        }
        out.sort_by(order);
        out
    }

    /// Feature i's label point (active pixels) and whether it is a point's (its label beside it).
    pub fn at(&self, i: usize) -> ([f64; 2], bool) {
        let a = self.at[i];
        ([f64::from(a[0]), f64::from(a[1])], self.point[i])
    }
}

/// The rectangles placed so far, in screen cells of 64 points: a new one overlapping none is kept.
pub struct Occupied {
    cells: std::collections::HashMap<(i32, i32), Vec<Rect>>,
}

impl Occupied {
    pub fn new() -> Self {
        Occupied { cells: Default::default() }
    }

    fn keys(r: Rect) -> impl Iterator<Item = (i32, i32)> {
        let c = |v: f32| (v / 64.0).floor() as i32;
        let (x0, y0, x1, y1) = (c(r.min.x), c(r.min.y), c(r.max.x), c(r.max.y));
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| (x, y)))
    }

    /// `r` kept if it overlaps none kept before.
    pub fn take(&mut self, r: Rect) -> bool {
        if Self::keys(r).any(|k| self.cells.get(&k).is_some_and(|v| v.iter().any(|o| o.intersects(r)))) {
            return false;
        }
        for k in Self::keys(r) {
            self.cells.entry(k).or_default().push(r);
        }
        true
    }
}

impl Default for Occupied {
    fn default() -> Self {
        Self::new()
    }
}

/// The labels of `prep` placed in a view: `view` the active pixels seen, `k` screen units per active pixel, `to`
/// a point's place on screen, `measure` a text's size on screen, `size` the label's height on screen, `occupied`
/// what labels (of layers above) already cover. Each placed: the feature and its label's rectangle.
#[allow(clippy::too_many_arguments)]
pub fn place(
    prep: &Prepared,
    values: &[String],
    view: [f64; 4],
    k: f64,
    size: f32,
    to: &dyn Fn(f64, f64) -> Pos2,
    measure: &mut dyn FnMut(&str) -> egui::Vec2,
    occupied: &mut Occupied,
) -> Vec<(usize, Rect)> {
    let mut out = Vec::new();
    for i in prep.candidates(view, k, 2.0 * f64::from(size)) {
        let Some(text) = values.get(i).filter(|t| !t.trim().is_empty()) else { continue };
        let (a, point) = prep.at(i);
        let c = to(a[0], a[1]);
        let s = measure(text);
        // a point's label to its right, past the point's disc; the others centred on their point
        let min = if point { Pos2::new(c.x + 0.5 * size + 3.0, c.y - s.y / 2.0) } else { c - s / 2.0 };
        let r = Rect::from_min_size(min, s).expand(1.0);
        if occupied.take(r) {
            out.push((i, r.shrink(1.0)));
            if out.len() >= MAX_LABELS {
                break;
            }
        }
    }
    out
}

/// A column being read: its name, the values when read, the flag that cancels it.
type Reading = (String, Receiver<Result<Vec<String>, String>>, Arc<AtomicBool>);
/// Views' labels: what they were for, and them (feature, rectangle).
type ViewLabels = Vec<(u64, Vec<(usize, Rect)>)>;

/// A layer's labels: its style, the column's values (read on a thread), its label points (made on a thread for
/// each placement), the last views' labels.
#[derive(Default)]
pub struct Labels {
    pub style: LabelStyle,
    values: Option<(String, Arc<Vec<String>>)>,
    reading: Option<Reading>,
    prepared: Option<Arc<Prepared>>,
    preparing: Option<(u64, Receiver<Prepared>)>,
    pub error: String,
    /// --labels given to it
    cli: bool,
    /// the labels of the last views drawn (a map panel each): what they were for, and them
    cache: Mutex<ViewLabels>,
}

impl Drop for Labels {
    fn drop(&mut self) {
        if let Some((_, _, c)) = &self.reading {
            c.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

impl Labels {
    /// A new style (its column read again if it changed).
    pub fn set_style(&mut self, style: LabelStyle) {
        if style != self.style {
            self.style = style;
            self.error.clear();
            self.cache.lock().unwrap().clear();
        }
    }

    /// The column's values and the label points, when both are there for the layer's placement `key`.
    pub fn ready(&self, key: u64) -> Option<(&Prepared, &[String])> {
        let p = self.prepared.as_deref().filter(|p| p.key == key)?;
        let v = self.values.as_ref().filter(|v| v.0 == self.style.field)?;
        Some((p, v.1.as_slice()))
    }
}

impl VectorLayer {
    /// Its labels' column read and its label points made on threads, as its style and placement ask; whether the
    /// map should be drawn again.
    fn pump_labels(&mut self, ctx: &egui::Context) -> bool {
        let mut changed = false;
        let table = self.table();
        let l = &mut self.labels;
        if !l.style.on() {
            l.reading = None;
            return false;
        }
        // the column
        if let Some((field, rx, _)) = &l.reading {
            match rx.try_recv() {
                Ok(r) => {
                    let field = field.clone();
                    l.reading = None;
                    match r {
                        Ok(v) => {
                            l.values = Some((field, Arc::new(v)));
                            l.error.clear();
                        }
                        Err(e) => l.error = e,
                    }
                    changed = true;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => l.reading = None,
            }
        }
        let have = l.values.as_ref().is_some_and(|v| v.0 == l.style.field);
        let asked = l.reading.as_ref().is_some_and(|r| r.0 == l.style.field);
        if !have && !asked && l.error.is_empty() {
            match self.data.fields.iter().position(|f| *f == l.style.field) {
                None => l.error = format!("no column {} in this layer", l.style.field),
                Some(k) => {
                    // the table read already: its column
                    if let Some(t) = table {
                        let v = t.iter().map(|r| r.get(k).cloned().unwrap_or_default()).collect();
                        l.values = Some((l.style.field.clone(), Arc::new(v)));
                        changed = true;
                    } else {
                        let (tx, rx) = channel();
                        let cancel = Arc::new(AtomicBool::new(false));
                        let (data, c, ctx) = (self.data.clone(), cancel.clone(), ctx.clone());
                        std::thread::Builder::new()
                            .name("vector-labels".into())
                            .spawn(move || {
                                let _ = tx.send(janus_geo::vectors::read_field(&data, k, &c));
                                ctx.request_repaint();
                            })
                            .ok();
                        if let Some((_, _, c)) = l.reading.replace((l.style.field.clone(), rx, cancel)) {
                            c.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
        }
        // the label points, for this placement
        let key = self.placed_for;
        if let Some((k, rx)) = &l.preparing {
            match rx.try_recv() {
                Ok(p) => {
                    if *k == key {
                        l.prepared = Some(Arc::new(p));
                        changed = true;
                    }
                    l.preparing = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => l.preparing = None,
            }
        }
        let ready = l.prepared.as_ref().is_some_and(|p| p.key == key);
        let started = l.preparing.as_ref().is_some_and(|p| p.0 == key);
        if key != 0 && !self.px.is_empty() && !ready && !started {
            let (tx, rx) = channel();
            let (px, boxes, anchor) = (self.px.clone(), self.boxes.clone(), self.anchor.clone());
            let kinds: Vec<Kind> = self.data.features.iter().map(|f| f.kind).collect();
            let ctx = ctx.clone();
            std::thread::Builder::new()
                .name("vector-label-points".into())
                .spawn(move || {
                    let _ = tx.send(Prepared::new(&px, &boxes, &anchor, &kinds, key));
                    ctx.request_repaint();
                })
                .ok();
            l.preparing = Some((key, rx));
        }
        if changed {
            l.cache.lock().unwrap().clear();
        }
        changed
    }

    /// Its labels in a view (cached: the same view again takes them as placed), placed after `occupied`.
    #[allow(clippy::too_many_arguments)]
    pub fn labels_in(
        &self,
        view: [f64; 4],
        k: f64,
        size: f32,
        sig: u64,
        to: &dyn Fn(f64, f64) -> Pos2,
        measure: &mut dyn FnMut(&str) -> egui::Vec2,
        occupied: &mut Occupied,
    ) -> Vec<(usize, Rect)> {
        let l = &self.labels;
        let Some((prep, values)) = l.ready(self.placed_for) else { return Vec::new() };
        let mut cache = l.cache.lock().unwrap();
        if let Some((_, placed)) = cache.iter().find(|c| c.0 == sig) {
            // taken as they were: they cover their places for the layers below
            for (_, r) in placed {
                occupied.take(r.expand(1.0));
            }
            return placed.clone();
        }
        let placed = place(prep, values, view, k, size, to, measure, occupied);
        cache.retain(|c| c.0 != sig);
        cache.insert(0, (sig, placed.clone()));
        cache.truncate(4);
        placed
    }
}

/// A text with a halo: the halo's colour drawn around it (8 places a point away), then the text.
pub fn draw_text_halo(
    painter: &egui::Painter,
    at: Pos2,
    galley: Arc<egui::Galley>,
    color: Color32,
    halo: Option<Color32>,
) {
    if let Some(h) = halo {
        let d = 1.0;
        for (dx, dy) in [(-d, -d), (0.0, -d), (d, -d), (-d, 0.0), (d, 0.0), (-d, d), (0.0, d), (d, d)] {
            painter.galley_with_override_text_color(at + vec2(dx, dy), galley.clone(), h);
        }
    }
    painter.galley_with_override_text_color(at, galley, color);
}

/// A view's signature (the labels placed for it are kept).
fn view_sig(view: [f64; 4], k: f64, rect: Rect, style: &LabelStyle) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    view.map(f64::to_bits).hash(&mut h);
    k.to_bits().hash(&mut h);
    [rect.min.x, rect.min.y, rect.max.x, rect.max.y].map(f32::to_bits).hash(&mut h);
    style.size.to_bits().hash(&mut h);
    h.finish()
}

/// The Labels submenu of a vector layer's menu: the column, size, colour and halo; whether it changed.
pub fn labels_menu(ui: &mut Ui, v: &mut VectorLayer) -> bool {
    let mut changed = false;
    let st = &mut v.labels.style;
    let title = if st.on() { format!("Labels: {}", st.field) } else { "Labels".to_string() };
    ui.menu_button(title, |ui| {
        let none = "(none)".to_string();
        egui::ComboBox::from_id_salt("label column")
            .selected_text(if st.on() { st.field.clone() } else { none.clone() })
            .show_ui(ui, |ui| {
                changed |= ui.selectable_value(&mut st.field, String::new(), none).changed();
                for f in &v.data.fields {
                    changed |= ui.selectable_value(&mut st.field, f.clone(), f).changed();
                }
            })
            .response
            .on_hover_text("The column whose values label the features on the map");
        if v.data.fields.is_empty() {
            ui.weak("This layer has no columns.");
        }
        ui.add_enabled_ui(st.on(), |ui| {
            ui.horizontal(|ui| {
                ui.label("Size");
                changed |= ui.add(egui::DragValue::new(&mut st.size).range(6.0..=48.0).speed(0.2).suffix(" pt")).changed();
            });
            ui.horizontal(|ui| {
                ui.label("Colour");
                let mut c = st.color32();
                if ui.color_edit_button_srgba(&mut c).changed() {
                    st.color = c.to_srgba_unmultiplied();
                    changed = true;
                }
                changed |= ui.checkbox(&mut st.halo, "Halo").on_hover_text("An outline that keeps them legible").changed();
                if st.halo {
                    let mut h = st.halo32();
                    if ui.color_edit_button_srgba(&mut h).changed() {
                        st.halo_color = h.to_srgba_unmultiplied();
                        changed = true;
                    }
                }
            });
            ui.weak("A polygon's or a line's label when it is large enough on\nscreen; a label that would cover another is left out,\nthe larger features first.");
        });
        if v.labels.reading.is_some() {
            ui.weak("reading the column...");
        } else if !v.labels.error.is_empty() {
            ui.colored_label(crate::theme::warning(), &v.labels.error);
        }
    });
    if changed {
        v.labels.error.clear();
        v.labels.cache.lock().unwrap().clear();
    }
    changed
}

impl State {
    /// Every layer's labels read and made on threads as their styles ask.
    pub(crate) fn pump_vector_labels(&mut self, ctx: &egui::Context) {
        let mut dirty = false;
        // --labels FIELD[,SIZE]: every layer's, once
        if let Some(arg) = &self.opts.labels {
            let (field, size) = arg.split_once(',').unwrap_or((arg, ""));
            for v in self.vectors.iter_mut().filter(|v| !v.labels.cli) {
                v.labels.cli = true;
                let size = size.trim().parse().unwrap_or(12.0f32).clamp(6.0, 48.0);
                v.labels.set_style(LabelStyle { field: field.trim().into(), size, ..Default::default() });
            }
        }
        for v in &mut self.vectors {
            dirty |= v.pump_labels(ctx);
        }
        if dirty {
            self.map.dirty = true;
        }
    }

    /// Every vector file read, placed, drawn by the GPU and its labels ready (screenshots).
    pub(crate) fn vectors_settled(&self) -> bool {
        self.vector_loads.is_empty()
            && self.vectors.iter().all(|v| {
                let key = v.placed_for;
                let drawn = self.gpu.is_none() || v.px.is_empty() || v.gpu.draws(key);
                let labels = !v.labels.style.on() || !v.labels.error.is_empty() || v.labels.ready(key).is_some();
                v.placing.is_none() && (key != 0 || !v.why.is_empty()) && drawn && labels
            })
    }

    /// The vector layers' labels over a map canvas (over their features, the top layer's first: a label of a layer
    /// below that would cover one above is left out).
    pub(crate) fn draw_vector_labels(&self, painter: &egui::Painter, rect: Rect, at: &dyn Fn(f64, f64) -> Pos2) {
        for (li, i, r) in self.vector_labels_placed(painter, rect, at) {
            let v = &self.vectors[li];
            let st = &v.labels.style;
            let Some((_, values)) = v.labels.ready(v.placed_for) else { continue };
            let font = FontId::new(st.size, FontFamily::Proportional);
            let galley = painter.layout_no_wrap(values[i].clone(), font, st.color32());
            draw_text_halo(painter, r.min, galley, st.color32(), st.halo.then(|| st.halo32()));
        }
    }

    /// The labels a map canvas shows: each one's layer, feature and rectangle on screen.
    pub(crate) fn vector_labels_placed(
        &self,
        painter: &egui::Painter,
        rect: Rect,
        at: &dyn Fn(f64, f64) -> Pos2,
    ) -> Vec<(usize, usize, Rect)> {
        let mut out = Vec::new();
        let Some(g) = self.grid() else { return out };
        if !self.vectors.iter().any(|v| v.visible && v.labels.style.on()) {
            return out;
        }
        let k = self.screen_per_px();
        let clip = painter.clip_rect().intersect(rect);
        let [ax, ay] = self.map.image_at(rect, clip.min);
        let [bx, by] = self.map.image_at(rect, clip.max);
        let view = [ax * g.fx, ay * g.fy, bx * g.fx, by * g.fy];
        let mut occupied = Occupied::new();
        let mut order: Vec<usize> =
            (0..self.vectors.len()).filter(|&i| self.vectors[i].visible && self.vectors[i].labels.style.on()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(self.vectors[i].z));
        for li in order {
            let v = &self.vectors[li];
            let st = &v.labels.style;
            let font = FontId::new(st.size, FontFamily::Proportional);
            let mut measure = |s: &str| painter.layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size();
            let sig = view_sig(view, k, clip, st);
            let placed = v.labels_in(view, k, st.size, sig, at, &mut measure, &mut occupied);
            out.extend(placed.into_iter().map(|(i, r)| (li, i, r)));
        }
        out
    }

    /// The vector layers' labels on an exported image (`to` a point's place in its pixels, `ps` its pixels per
    /// point), as the map places them.
    pub(crate) fn draw_vector_labels_on(
        &self,
        cv: &mut crate::canvas::Canvas,
        to: &dyn Fn(f64, f64) -> (f32, f32),
        ps: f32,
    ) {
        use crate::canvas::{Canvas, Face};
        let Some(g) = self.grid() else { return };
        let (w, h) = (cv.w as f32, cv.h as f32);
        // the image's corners in active pixels, through `to` (a linear map)
        let (o, e) = (to(0.0, 0.0), to(g.w, g.h));
        let (sx, sy) = ((e.0 - o.0) / g.w as f32, (e.1 - o.1) / g.h as f32);
        if sx <= 0.0 || sy <= 0.0 {
            return;
        }
        let view = [f64::from(-o.0 / sx), f64::from(-o.1 / sy), f64::from((w - o.0) / sx), f64::from((h - o.1) / sy)];
        let k = f64::from(sx);
        let mut occupied = Occupied::new();
        let mut order: Vec<&VectorLayer> = self.vectors.iter().filter(|v| v.visible && v.labels.style.on()).collect();
        order.sort_by_key(|v| std::cmp::Reverse(v.z));
        let pos = |x: f64, y: f64| {
            let p = to(x, y);
            Pos2::new(p.0, p.1)
        };
        for v in order {
            let st = &v.labels.style;
            let Some((prep, values)) = v.labels.ready(v.placed_for) else { continue };
            let size = st.size * ps;
            let mut measure = |s: &str| vec2(Canvas::text_width(s, size, Face::Vector), size);
            let placed = place(prep, values, view, k, size, &pos, &mut measure, &mut occupied);
            let col = st.color32().to_srgba_unmultiplied();
            let halo = st.halo32().to_srgba_unmultiplied();
            for (i, r) in placed {
                if st.halo {
                    let d = ps.max(1.0);
                    for (dx, dy) in [(-d, -d), (0.0, -d), (d, -d), (-d, 0.0), (d, 0.0), (-d, d), (0.0, d), (d, d)] {
                        cv.text(r.min.x + dx, r.min.y + dy, &values[i], size, halo, Face::Vector);
                    }
                }
                cv.text(r.min.x, r.min.y, &values[i], size, col, Face::Vector);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<f64> {
        vec![x, y, x + s, y, x + s, y + s, x, y + s]
    }

    /// A layer's parts, boxes, label points and kinds.
    type Layer = (Vec<Polygon>, Vec<[f32; 4]>, Vec<[f64; 2]>, Vec<Kind>);

    /// Three polygons (a large one, a small one overlapping its label, a tiny one), a line, a point.
    fn layer() -> Layer {
        let px: Vec<Polygon> = vec![
            vec![square(0.0, 0.0, 100.0)],
            vec![square(45.0, 45.0, 30.0)],
            vec![square(300.0, 300.0, 1.0)],
            vec![vec![200.0, 0.0, 200.0, 30.0, 260.0, 30.0]],
            vec![vec![400.0, 50.0]],
        ];
        let boxes = px.iter().map(|p| janus_geo::plane::bounds(p).unwrap().map(|v| v as f32)).collect::<Vec<_>>();
        let anchor = vec![[50.0, 50.0], [60.0, 60.0], [300.5, 300.5], [f64::NAN; 2], [400.0, 50.0]];
        let kinds = vec![Kind::Area, Kind::Area, Kind::Area, Kind::Line, Kind::Point];
        (px, boxes, anchor, kinds)
    }

    #[test]
    fn line_middle_along_its_length() {
        let (m, len) = line_middle(&vec![vec![200.0, 0.0, 200.0, 30.0, 260.0, 30.0]]).unwrap();
        assert_eq!(len, 90.0);
        assert!((m[0] - 215.0).abs() < 1e-9 && (m[1] - 30.0).abs() < 1e-9, "{m:?}");
    }

    #[test]
    fn larger_first_no_overlap_large_enough() {
        let (px, boxes, anchor, kinds) = layer();
        let p = Prepared::new(&px, &boxes, &anchor, &kinds, 7);
        let values: Vec<String> = ["big", "small", "tiny", "road", "well"].map(String::from).to_vec();
        let to = |x: f64, y: f64| Pos2::new(x as f32, y as f32); // a screen point per pixel
        let mut measure = |s: &str| vec2(6.0 * s.len() as f32, 12.0);
        let mut occ = Occupied::new();
        let placed = place(&p, &values, [-10.0, -10.0, 500.0, 500.0], 1.0, 12.0, &to, &mut measure, &mut occ);
        let got: Vec<usize> = placed.iter().map(|p| p.0).collect();
        // the large polygon, then the line (90 long), not the smaller one (its label would cover the large one's),
        // nor the tiny one (under twice the label's height), the point
        assert_eq!(got, vec![0, 3, 4]);
        // the line's label at its middle, the point's to its right
        let road = placed[1].1;
        assert!((road.center().x - 215.0).abs() < 0.5 && (road.center().y - 30.0).abs() < 0.5);
        assert!(placed[2].1.min.x > 400.0);
        // zoomed in 4x, the large one's point alone in view
        let mut occ = Occupied::new();
        let to4 = |x: f64, y: f64| Pos2::new(4.0 * x as f32, 4.0 * y as f32);
        let placed = place(&p, &values, [44.0, 44.0, 58.0, 58.0], 4.0, 12.0, &to4, &mut measure, &mut occ);
        assert_eq!(placed.iter().map(|p| p.0).collect::<Vec<_>>(), vec![0]);
        // a view with the large one's point out of it: the smaller one's label
        let mut occ = Occupied::new();
        let placed = place(&p, &values, [55.0, 55.0, 70.0, 70.0], 4.0, 12.0, &to4, &mut measure, &mut occ);
        assert_eq!(placed.iter().map(|p| p.0).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn style_text_round_trip() {
        let s = LabelStyle {
            field: "nome; município".into(),
            size: 14.5,
            color: [250, 240, 10, 255],
            halo: false,
            halo_color: [0, 0, 0, 220],
        };
        let text = s.to_text();
        let fields: Vec<(String, String)> =
            text.split(';').filter_map(|p| p.split_once('=').map(|(a, b)| (a.to_string(), b.to_string()))).collect();
        let get = |k: &str| fields.iter().find(|f| f.0 == k).map(|f| f.1.clone());
        assert_eq!(LabelStyle::from_fields(&get), s);
        assert_eq!(LabelStyle::default().to_text(), "");
    }

    #[test]
    fn kept_in_the_project() {
        let e = crate::project::VectorEntry {
            labels: LabelStyle { field: "name".into(), size: 10.0, ..Default::default() },
            ..Default::default()
        };
        let text = serde_json::to_string(&e).unwrap();
        assert_eq!(serde_json::from_str::<crate::project::VectorEntry>(&text).unwrap(), e);
        // a project saved before labels: none
        let old: crate::project::VectorEntry = serde_json::from_str(r#"{"layer": "x", "visible": true}"#).unwrap();
        assert!(!old.labels.on());
    }
}
