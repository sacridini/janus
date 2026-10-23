//! The map panel (src/app.cpp: uiMap, mapInput, drawMapMarks, renderMap): every layer drawn by the GPU into the
//! panel's target, shown as an image; the wheel zooms (smoothed, as the C++), a drag moves, a click drops a pin, the
//! right button removes one, Shift + drag draws an ROI; the cursor's pixel, the pins, the ROI, the colour bar and
//! the status are drawn over it. The map panels and the swipe (views.rs) share its view and its input.

use egui::{Align2, Color32, FontId, Pos2, Rect, Response, Sense, Stroke, Ui, vec2};
use janus_gpu::Mode;
use janus_session::ViewRect;

use crate::app::State;
use crate::display::range_label;
use crate::theme;
use janus_cube::multidim::format_g;

/// The closest zoom: screen points per source pixel (the C++'s 64; four times that, to look at a pixel's
/// neighbours closely).
pub(crate) const MAX_ZOOM: f64 = 256.0;

/// The C++'s wheel zoom: an exponential approach with this time constant (s).
const ZOOM_TAU: f64 = 0.06;

#[derive(Default)]
pub struct MapView {
    /// the overview pixel at the panel's centre
    pub center: [f64; 2],
    /// screen points per overview pixel
    pub scale: f64,
    target_scale: f64,
    /// where the wheel was turned: the screen point and the image point under it, kept together while zooming
    anchor: Option<(Pos2, [f64; 2])>,
    pub fit_pending: bool,
    /// moved or zoomed since the last fit (a layer made active keeps the view only then)
    pub touched: bool,
    /// the map's size on the screen (points), last frame
    pub canvas: [f32; 2],
    /// what the target holds: the mode, dates, range, colormap, the image's place and the tiles on the GPU
    drawn: Option<DrawKey>,
    pub dirty: bool,
    /// the view last drawn, in source pixels at target pixels per source pixel (what an RGB composite reads)
    pub last_view: Option<ViewRect>,
    /// the detail tiles' level drawn (0 = full resolution), None: the overview
    pub detail_level: Option<u32>,
    /// the main map was drawn again this frame: the panels and the swipe follow it
    pub redrawn: bool,
    /// the canvas the pointer is over this frame (0: the main map, else a panel's id), and the last frame's
    pub pointer_panel: Option<u32>,
    pub prev_pointer_panel: Option<u32>,
    /// an export's background (None: the map's)
    pub export_bg: Option<[f32; 4]>,
    /// an export without the vector layers
    pub export_no_vectors: bool,
    /// the active layer's CRS's area of use in its source pixels (west, north, east, south), for the layer of
    /// that id (None: none known)
    crs_extent: Option<(u64, Option<[f64; 4]>)>,
}

type DrawKey = (Mode, [i32; 3], [u32; 2], usize, [u32; 4], usize, Option<usize>, u64);

/// The map's background where there is no data (the C++'s, app.cpp renderMap).
pub(crate) const MAP_BACKGROUND: [f32; 4] = [0.10, 0.10, 0.115, 1.0];

impl MapView {
    pub fn fit(&mut self) {
        self.fit_pending = true;
        self.touched = false;
    }

    /// The map centred on an overview pixel at `scale` screen points per overview pixel.
    pub fn set_view(&mut self, center: [f64; 2], scale: f64) {
        self.center = center;
        self.scale = scale;
        self.target_scale = scale;
        self.anchor = None;
        self.fit_pending = false;
        self.dirty = true;
    }

    /// A point of the overview on a canvas centred like the main map.
    pub fn screen_of(&self, rect: Rect, ix: f64, iy: f64) -> Pos2 {
        Pos2::new(
            rect.center().x + ((ix - self.center[0]) * self.scale) as f32,
            rect.center().y + ((iy - self.center[1]) * self.scale) as f32,
        )
    }

    /// The overview's point under a screen point.
    pub fn image_at(&self, rect: Rect, p: Pos2) -> [f64; 2] {
        [
            self.center[0] + f64::from(p.x - rect.center().x) / self.scale,
            self.center[1] + f64::from(p.y - rect.center().y) / self.scale,
        ]
    }
}

/// The area of use of a series' CRS (PROJ's database: longitudes and latitudes), in the series' source pixels
/// (x0, y0, x1, y1): its edges and a grid inside it transformed to the CRS (a projection curves them), then to the
/// pixels by the inverse of the geotransform. None without georeferencing, or a CRS without an area.
pub(crate) fn crs_extent_px(info: &janus_cube::CubeInfo) -> Option<[f64; 4]> {
    use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
    let g = info.geo_transform?;
    let det = g[1] * g[5] - g[2] * g[4];
    if info.crs_wkt.is_empty() || det == 0.0 {
        return None;
    }
    let mut srs = SpatialRef::from_wkt(&info.crs_wkt).ok()?;
    let (mut w, mut s, mut e, mut n) = (0.0, 0.0, 0.0, 0.0);
    let ok =
        unsafe { gdal_sys::OSRGetAreaOfUse(srs.to_c_hsrs(), &mut w, &mut s, &mut e, &mut n, std::ptr::null_mut()) };
    if ok == 0 || w < -180.5 || e > 180.5 {
        return None;
    }
    if e < w {
        e += 360.0; // across the antimeridian
    }
    let mut wgs = SpatialRef::from_epsg(4326).ok()?;
    srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    wgs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&wgs, &srs).ok()?;
    let k = 24;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for i in 0..=k {
        for j in 0..=k {
            let lon = w + (e - w) * i as f64 / k as f64;
            let lat = s + (n - s) * j as f64 / k as f64;
            let (mut x, mut y, mut z) = ([if lon > 180.0 { lon - 360.0 } else { lon }], [lat], [0.0]);
            if ct.transform_coords(&mut x, &mut y, &mut z).is_err() || !x[0].is_finite() || !y[0].is_finite() {
                continue;
            }
            let (dx, dy) = (x[0] - g[0], y[0] - g[3]);
            let px = (g[5] * dx - g[2] * dy) / det;
            let py = (-g[4] * dx + g[1] * dy) / det;
            b = [b[0].min(px), b[1].min(py), b[2].max(px), b[3].max(py)];
        }
    }
    (b[0] < b[2] && b[1] < b[3]).then_some(b)
}

/// The active layer's source pixels per overview pixel, and their counts.
pub(crate) struct Grid {
    pub ow: f64,
    pub oh: f64,
    pub w: f64,
    pub h: f64,
    pub fx: f64,
    pub fy: f64,
}

impl State {
    /// How far the map goes, in the active layer's overview pixels: its CRS's area of use, the series and every
    /// other layer placed on it (all of it reachable; the image itself when nothing else is known).
    pub(crate) fn map_extent(&mut self, g: &Grid) -> [f64; 4] {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return [0.0, 0.0, g.ow, g.oh] };
        if self.map.crs_extent.as_ref().is_none_or(|c| c.0 != info.id) {
            self.map.crs_extent = Some((info.id, crs_extent_px(&info)));
        }
        let mut e = [0.0, 0.0, g.w, g.h];
        let mut add = |x: f64, y: f64| {
            if x.is_finite() && y.is_finite() {
                e = [e[0].min(x), e[1].min(y), e[2].max(x), e[3].max(y)];
            }
        };
        if let Some(c) = self.map.crs_extent.as_ref().and_then(|c| c.1) {
            add(c[0], c[1]);
            add(c[2], c[3]);
        }
        for (i, l) in self.layers.iter().enumerate() {
            let Some(li) = l.session.as_ref().map(|s| &s.info).filter(|_| i != self.active) else { continue };
            let (lw, lh) = (li.width as f64, li.height as f64);
            for k in 0..=8 {
                let f = k as f64 / 8.0;
                for (x, y) in [(f * lw, 0.0), (f * lw, lh), (0.0, f * lh), (lw, f * lh)] {
                    if let Some((ax, ay)) = l.place.to_active(x, y) {
                        add(ax, ay);
                    }
                }
            }
        }
        [e[0] / g.fx, e[1] / g.fy, e[2] / g.fx, e[3] / g.fy]
    }

    pub(crate) fn grid(&self) -> Option<Grid> {
        let s = self.session.as_ref()?;
        let (ow, oh) = (s.s.overview.w as f64, s.s.overview.h as f64);
        let (w, h) = (s.info.width as f64, s.info.height as f64);
        Some(Grid { ow, oh, w, h, fx: w / ow, fy: h / oh })
    }

    /// The pointer over a map canvas (the main map's or a panel's: they share the view, centred alike): the wheel
    /// zooms about the cursor, a drag pans, Shift + drag draws an ROI, a click drops a pin, the right button
    /// removes the one under the cursor, Delete the last one. Returns the source pixel under the cursor.
    pub(crate) fn canvas_input(&mut self, ui: &Ui, rect: Rect, resp: &Response, main: bool) -> Option<(usize, usize)> {
        let g = self.grid()?;
        let (ow, oh, w, h, fx, fy) = (g.ow, g.oh, g.w, g.h, g.fx, g.fy);
        // the map goes as far as the CRS's area of use and every layer, not only the image
        let ext = self.map_extent(&g);
        let m = &mut self.map;
        if main {
            m.canvas = [rect.width(), rect.height()];
        }
        // the whole image with a margin, as the C++'s fitView (of the main map's canvas)
        let [cw, ch] = if m.canvas[0] > 0.0 { m.canvas } else { [rect.width(), rect.height()] };
        let fit = 0.98 * (f64::from(cw) / ow).min(f64::from(ch) / oh);
        if m.fit_pending || m.scale <= 0.0 {
            m.center = [ow / 2.0, oh / 2.0];
            m.scale = fit;
            m.target_scale = fit;
            m.anchor = None;
            m.fit_pending = false;
        }
        // up to MAX_ZOOM screen points per source pixel; down to the whole extent (F: the image)
        let max_scale = MAX_ZOOM * ow / w;
        let (ew, eh) = ((ext[2] - ext[0]).max(1e-9), (ext[3] - ext[1]).max(1e-9));
        let min_scale = (0.98 * (f64::from(cw) / ew).min(f64::from(ch) / eh)).min(fit);
        if resp.hovered() {
            // the wheel's own steps (in points), smoothed here as the C++ smooths them
            // macOS: a trackpad's two fingers (precise deltas, in points) pan, as the C++'s gestures do; a pinch
            // zooms; a mouse wheel zooms everywhere
            let trackpad_pans = cfg!(target_os = "macos");
            let (scroll, pinch, pan) = ui.input(|i| {
                let mut pan = egui::Vec2::ZERO;
                let scroll: f32 = i
                    .raw
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, modifiers, .. }
                            if !modifiers.ctrl && !modifiers.command =>
                        {
                            match unit {
                                egui::MouseWheelUnit::Point if trackpad_pans => {
                                    pan += *delta;
                                    None
                                }
                                egui::MouseWheelUnit::Point => Some(delta.y),
                                egui::MouseWheelUnit::Line => Some(delta.y * 50.0),
                                egui::MouseWheelUnit::Page => Some(delta.y * rect.height()),
                            }
                        }
                        _ => None,
                    })
                    .sum();
                (scroll, i.zoom_delta(), pan)
            });
            if pan != egui::Vec2::ZERO {
                m.center[0] -= f64::from(pan.x) / m.scale;
                m.center[1] -= f64::from(pan.y) / m.scale;
                m.touched = true;
                m.anchor = None;
            }
            if scroll != 0.0 || pinch != 1.0 {
                let factor = 1.25f64.powf(f64::from(scroll) / 50.0) * f64::from(pinch);
                m.touched = true;
                m.target_scale = (m.target_scale * factor).clamp(min_scale.min(max_scale), max_scale.max(fit));
                if let Some(p) = resp.hover_pos() {
                    m.anchor = Some((p - rect.center().to_vec2(), m.image_at(rect, p)));
                }
            }
        }
        if main {
            // the zoom's approach, once a frame (the anchor relative to the canvas' centre: any panel's wheel)
            let dt = f64::from(ui.input(|i| i.stable_dt)).min(0.1);
            if (m.target_scale / m.scale).ln().abs() > 1e-4 {
                let k = 1.0 - (-dt / ZOOM_TAU).exp();
                m.scale *= (m.target_scale / m.scale).powf(k);
                if let Some((sp, ip)) = m.anchor {
                    m.center = [ip[0] - f64::from(sp.x) / m.scale, ip[1] - f64::from(sp.y) / m.scale];
                }
                ui.ctx().request_repaint();
            } else if m.scale != m.target_scale {
                m.scale = m.target_scale;
                if m.scale <= min_scale * 1.0001 {
                    // zoomed out to the whole extent: it ends centred
                    m.center = [(ext[0] + ext[2]) / 2.0, (ext[1] + ext[3]) / 2.0];
                }
            }
        }
        // the swipe's divider takes the drag (no pan, pin or ROI)
        if main && self.swipe_input(ui, rect, resp) {
            return None;
        }
        let m = &mut self.map;
        let src_at = |m: &MapView, p: Pos2| -> [f64; 2] {
            let [ix, iy] = m.image_at(rect, p);
            [ix * fx, iy * fy]
        };
        let shift = ui.input(|i| i.modifiers.shift);
        // R: a polygon's vertices or a freehand outline (no pan, no pin)
        let at_pointer = resp.interact_pointer_pos().or(resp.hover_pos()).map(|p| src_at(m, p));
        let drawing = self.transect_input(ui, resp, at_pointer) || self.polygon_input(ui, resp, at_pointer);
        let poly_on = self.series.poly.on || drawing;
        let m = &mut self.map;
        // Shift + drag: an ROI rectangle (no pan, no pin)
        if !drawing
            && resp.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(p) = resp.interact_pointer_pos()
        {
            let a = src_at(m, p);
            self.series.roi_drag = Some((a, a));
        }
        if let Some((a, _)) = self.series.roi_drag.filter(|_| resp.is_pointer_button_down_on() || resp.drag_stopped()) {
            if let Some(p) = resp.interact_pointer_pos() {
                self.series.roi_drag = Some((a, src_at(m, p)));
            }
            if resp.drag_stopped() || !resp.is_pointer_button_down_on() {
                let (a, b) = self.series.roi_drag.take().unwrap();
                if (b[0] - a[0]).abs() >= 1.0 && (b[1] - a[1]).abs() >= 1.0 {
                    let r = [
                        a[0].min(b[0]).floor() as i64,
                        a[1].min(b[1]).floor() as i64,
                        a[0].max(b[0]).ceil() as i64,
                        a[1].max(b[1]).ceil() as i64,
                    ];
                    self.set_roi_rect(r);
                }
            }
            ui.ctx().request_repaint();
        } else if (!drawing && resp.dragged_by(egui::PointerButton::Primary))
            || resp.dragged_by(egui::PointerButton::Middle)
        {
            let d = resp.drag_delta();
            m.touched |= d != egui::Vec2::ZERO;
            m.center[0] -= f64::from(d.x) / m.scale;
            m.center[1] -= f64::from(d.y) / m.scale;
            m.anchor = None;
        }
        let m = &mut self.map;
        // the centre stays on the extent
        m.center = [m.center[0].clamp(ext[0], ext[2]), m.center[1].clamp(ext[1], ext[3])];
        // the pixel under the cursor
        let pointer = resp.hover_pos().map(|p| src_at(m, p));
        let inside =
            pointer.filter(|[x, y]| *x >= 0.0 && *y >= 0.0 && *x < w && *y < h).map(|[x, y]| (x as usize, y as usize));
        let view = (m.center, m.scale);
        if inside.is_some() && !self.opts.scripted() {
            self.set_hover(inside);
        }
        // a click drops a pin; the right button removes the one under the cursor; Delete the last one
        // the feature of a vector layer under the cursor; Alt + click on a polygon: it becomes the ROI, on a line:
        // the transect follows it
        let alt = ui.input(|i| i.modifiers.alt);
        if resp.hovered() {
            self.hover_vector = match pointer {
                Some([x, y]) if !self.vectors.is_empty() && !drawing => self.vector_feature_at(x, y, 6.0),
                _ => None,
            };
            self.vector_tooltip(resp);
        }
        if resp.clicked()
            && alt
            && let Some([x, y]) = pointer
        {
            if let Some((li, i)) = self.vector_feature_at(x, y, 6.0)
                && !self.feature_as_roi(li, i)
            {
                self.feature_as_transect(li, i);
            }
        } else if resp.clicked()
            && !shift
            && !poly_on
            && let Some((x, y)) = inside
        {
            self.add_pin(x, y);
        }
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let mv = MapView { center: view.0, scale: view.1, ..Default::default() };
            let near = self
                .series
                .pins
                .iter()
                .map(|pin| {
                    (pin.id, mv.screen_of(rect, (pin.x as f64 + 0.5) / fx, (pin.y as f64 + 0.5) / fy).distance(p))
                })
                .filter(|(_, d)| *d < 12.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((id, _)) = near {
                self.remove_pin(id);
            }
        }
        if resp.hovered()
            && !poly_on
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && let Some(id) = self.series.pins.last().map(|p| p.id)
        {
            self.remove_pin(id);
        }
        inside
    }

    /// Over a map canvas: the outline of the pixel under the cursor (when large enough), or a cross where the cursor
    /// is on another canvas; the pins; the ROI.
    pub(crate) fn map_marks(&self, painter: &egui::Painter, rect: Rect, inside: Option<(usize, usize)>, panel: u32) {
        let Some(g) = self.grid() else { return };
        let m = &self.map;
        let at = |x: f64, y: f64| m.screen_of(rect, x / g.fx, y / g.fy);
        if let Some((x, y)) = inside
            && m.scale * g.ow / g.w >= 6.0
        {
            let r = Rect::from_min_max(at(x as f64, y as f64), at(x as f64 + 1.0, y as f64 + 1.0));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(200)), egui::StrokeKind::Middle);
        }
        if inside.is_none()
            && let Some(h) = &self.series.hover
        {
            // the cursor is over another canvas: a cross at the same place here
            let c = at(h.x as f64 + 0.5, h.y as f64 + 0.5);
            if rect.contains(c) && self.map.prev_pointer_panel.is_some_and(|p| p != panel) {
                for (col, w) in [(Color32::from_black_alpha(200), 3.0), (Color32::from_white_alpha(230), 1.0)] {
                    let s = Stroke::new(w, col);
                    painter.line_segment([c - vec2(9.0, 0.0), c - vec2(3.0, 0.0)], s);
                    painter.line_segment([c + vec2(3.0, 0.0), c + vec2(9.0, 0.0)], s);
                    painter.line_segment([c - vec2(0.0, 9.0), c - vec2(0.0, 3.0)], s);
                    painter.line_segment([c + vec2(0.0, 3.0), c + vec2(0.0, 9.0)], s);
                }
            }
        }
        for pin in &self.series.pins {
            // a dark ring and a white one: visible over any colormap
            let c = at(pin.x as f64 + 0.5, pin.y as f64 + 0.5);
            painter.circle_stroke(c, 8.5, Stroke::new(3.0, Color32::from_black_alpha(220)));
            painter.circle_filled(c, 6.5, pin.color);
            painter.circle_stroke(c, 6.5, Stroke::new(1.5, Color32::WHITE));
            let galley = painter.layout_no_wrap(pin.id.to_string(), body_font(painter), pin.color);
            let tp = c + vec2(11.0, -18.0);
            painter.rect_filled(
                Rect::from_min_max(tp - vec2(3.0, 1.0), tp + galley.size() + vec2(3.0, 1.0)),
                3.0,
                Color32::from_black_alpha(190),
            );
            painter.galley(tp, galley, pin.color);
        }
        self.draw_vector_marks(painter, rect, &at); // under the pins and the ROI
        self.draw_validation_marks(painter, &at);
        self.draw_roi_marks(painter, rect, &at);
        self.draw_transect_marks(painter, rect, &at);
    }

    /// A colour bar at the bottom left of a canvas: the colormap and its range's ends.
    pub(crate) fn colour_bar(painter: &egui::Painter, rect: Rect, cmap: usize, lo: String, hi: String) {
        let cm = &janus_gpu::colormaps::all()[cmap];
        let b0 = rect.left_bottom() + vec2(10.0, -34.0);
        let bar = Rect::from_min_size(b0, vec2(220.0, 10.0));
        for i in 0..48 {
            let c = cm.sample((i as f32 + 0.5) / 48.0);
            let x0 = bar.min.x + bar.width() * i as f32 / 48.0;
            let x1 = bar.min.x + bar.width() * (i + 1) as f32 / 48.0;
            let r = Rect::from_min_max(Pos2::new(x0, bar.min.y), Pos2::new(x1, bar.max.y));
            painter.rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
        }
        painter.rect_stroke(bar, 0.0, Stroke::new(1.0, Color32::BLACK), egui::StrokeKind::Outside);
        State::shadowed(painter, b0 + vec2(0.0, 12.0), Align2::LEFT_TOP, &lo);
        State::shadowed(painter, bar.right_top() + vec2(0.0, 12.0), Align2::RIGHT_TOP, &hi);
    }

    /// A label over the map, light over a shadow: readable over any image.
    pub(crate) fn shadowed(painter: &egui::Painter, at: Pos2, align: Align2, text: &str) {
        let font = body_font(painter);
        painter.text(at + vec2(1.0, 1.0), align, text, font.clone(), Color32::from_black_alpha(200));
        painter.text(at, align, text, font, Color32::from_gray(230));
    }

    /// A title at the top left of a canvas, over a shadow.
    pub(crate) fn canvas_title(painter: &egui::Painter, rect: Rect, title: &str) {
        let font = body_font(painter);
        painter.text(rect.min + vec2(11.0, 9.0), Align2::LEFT_TOP, title, font.clone(), Color32::from_black_alpha(200));
        painter.text(rect.min + vec2(10.0, 8.0), Align2::LEFT_TOP, title, font, Color32::WHITE);
    }

    /// A map target's egui texture, made or updated when the target is (re)made at `size`.
    pub(crate) fn target_texture(&mut self, slot: i32, size: [u32; 2]) -> Option<(egui::TextureId, bool)> {
        let gpu = self.gpu.as_mut()?;
        let made = gpu.ensure_target(slot, size);
        let known = self.map_textures.get(&slot).copied();
        if made || known.is_none() {
            let t = gpu.target(slot)?;
            let rs = &self.render_state;
            let mut r = rs.renderer.write();
            let id = match known {
                Some(id) => {
                    r.update_egui_texture_from_wgpu_texture(&rs.device, &t.view, wgpu::FilterMode::Nearest, id);
                    id
                }
                None => r.register_native_texture(&rs.device, &t.view, wgpu::FilterMode::Nearest),
            };
            self.map_textures.insert(slot, id);
            return Some((id, true));
        }
        known.map(|id| (id, false))
    }

    pub fn map_ui(&mut self, ui: &mut Ui) {
        if self.session.is_none() {
            let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
            let msg = match (&self.opening, &self.error) {
                (Some(o), _) if o.inputs.len() == 1 => format!("Opening {}...", o.inputs[0]),
                (Some(o), _) => format!("Opening {} inputs...", o.inputs.len()),
                (_, Some(e)) => e.clone(),
                _ => "Open a time series: File > Open (Ctrl+O), drop files/a folder onto the window\nor call it from \
                      the command line: jn <folder | series.tif | files_*.tif>"
                    .to_string(),
            };
            let painter = ui.painter_at(rect);
            painter.text(rect.center(), Align2::CENTER_CENTER, msg, body_font(&painter), theme::weak_text());
            return;
        }
        // vector layers alone (on a canvas): no dates, no values
        let vectors_only = self.vectors_only();
        if !vectors_only {
            self.time_bar(ui);
            self.swipe_bar(ui);
        }
        // the canvas, above a status line
        let status_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y;
        let avail = ui.available_size();
        let (rect, resp) =
            ui.allocate_exact_size(vec2(avail.x.max(64.0), (avail.y - status_h).max(64.0)), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        if resp.hovered() {
            self.map.pointer_panel = Some(0);
        }
        let inside = self.canvas_input(ui, rect, &resp, true);
        let Some(g) = self.grid() else { return };

        // every layer drawn into the map's target, at the screen's pixels, the detail tiles over the active one
        let ppp = ui.ctx().pixels_per_point();
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        let Some((texture, remade)) = self.target_texture(0, size) else { return };
        if remade {
            self.map.dirty = true;
        }
        self.render_map(0, size, [rect.width(), rect.height()], ppp);
        let (ow, w) = (g.ow, g.w);
        painter.image(texture, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        self.draw_swipe(ui, &painter, rect, ppp);
        self.map_marks(&painter, rect, inside, 0);
        let m = &self.map;
        let s = self.session.as_ref().unwrap();

        // the title (date and mode), a wait, the overview's progress, the colour bar
        let (zoom, detail_level) = (m.scale * ow / w, m.detail_level);
        let info = &s.info;
        let d = &self.display;
        let n = info.t();
        let composite = self.composite.as_ref().filter(|c| c.on);
        let emb = self.embedding_map_info();
        let title = match (composite, &emb) {
            _ if vectors_only => "Vector layers".to_string(),
            (_, Some(e)) => e.0.clone(),
            (Some(c), None) => c.title(info, d.t),
            (None, None) => self.map_title(),
        };
        State::canvas_title(&painter, rect, &title);
        let font = body_font(&painter);
        let up = |t: usize| s.uploaded.get(t).copied().unwrap_or(false);
        let wait = if vectors_only {
            String::new()
        } else if let Some(e) = &emb {
            e.2.clone()
        } else if let Some(c) = composite {
            if !c.error.is_empty() {
                format!("RGB composite: {}", c.error)
            } else if c.ready(d.t, n) {
                String::new()
            } else {
                "Reading the RGB composite of this date...".into()
            }
        } else if d.mode.needs_stats() && s.stats.is_none() {
            "Computing statistics: waiting for the complete overview...".into()
        } else if d.mode == Mode::Diff && d.diff_ref_date(d.t, n).is_none() {
            "The first date has no previous date".into()
        } else if d.mode == Mode::Composite {
            let (t0, t1) = d.temporal_window(info, d.t);
            if (t0..=t1).all(up) { String::new() } else { "Loading the dates of the composite...".into() }
        } else if crate::display::time_dependent(d.mode)
            && (!up(if d.mode == Mode::Rgb { d.rgb[0] } else { d.t })
                || (d.mode == Mode::Diff && !d.diff_ref_date(d.t, n).is_some_and(up)))
        {
            "Loading this date...".into()
        } else {
            String::new()
        };
        if !wait.is_empty() {
            painter.text(rect.center(), Align2::CENTER_CENTER, wait, font.clone(), Color32::from_gray(220));
        }
        let ov = &s.s.overview;
        if !ov.complete() && !vectors_only {
            let what = if ov.from_cache() { " (cache)" } else { "" };
            let f = ov.layers_done() as f32 / n as f32;
            let p0 = rect.min + vec2(rect.width() - 250.0, 10.0);
            painter.rect_filled(Rect::from_min_size(p0, vec2(240.0, 18.0)), 3.0, Color32::from_black_alpha(160));
            painter.rect_filled(
                Rect::from_min_size(p0, vec2(240.0 * f, 18.0)),
                3.0,
                Color32::from_rgba_unmultiplied(70, 130, 220, 220),
            );
            painter.text(
                p0 + vec2(6.0, 2.0),
                Align2::LEFT_TOP,
                format!("Building overview {}/{n}{what}", ov.layers_done()),
                font.clone(),
                Color32::WHITE,
            );
        }
        self.draw_embedding_notes(&painter, rect);
        let mut bars = 0;
        if let Some((_, bar, _)) = &emb {
            if let Some((cmap, lo, hi)) = bar {
                State::colour_bar(&painter, rect, *cmap, format_g(f64::from(*lo), 3), format_g(f64::from(*hi), 3));
                bars = 1;
            }
        } else if d.mode != Mode::Rgb && composite.is_none() && !vectors_only {
            let range = d.ranges[d.mode as usize];
            let (lo, hi) = (range_label(d.mode, range.lo, info), range_label(d.mode, range.hi, info));
            State::colour_bar(&painter, rect, d.cmap[d.mode as usize], lo, hi);
            bars = 1;
        }
        // the topmost visible result map's, with its name, above it (app.cpp)
        let mut top = None;
        for (i, l) in self.layers.iter().enumerate() {
            let Some(id) = self.layer_session(i).map(|s| s.info.id) else { continue };
            if l.place.aligned {
                top = self.zeit.results.iter().rfind(|r| r.visible && r.cube == id).or(top);
            }
        }
        if let Some(r) = top {
            let up = vec2(0.0, -44.0 * bars as f32);
            let at = Rect::from_min_max(rect.min, rect.max + up);
            let (lo, hi) = (format_g(f64::from(r.lo), 4), format_g(f64::from(r.hi), 4));
            State::colour_bar(&painter, at, r.cmap, lo, hi);
            State::shadowed(&painter, at.left_bottom() + vec2(10.0, -50.0), Align2::LEFT_TOP, &r.name);
        }

        // the status line under the map: the zoom, its detail, the pixel and what the map shows there
        let mut status = format!("zoom {:.1}%", zoom * 100.0);
        status += &match detail_level {
            Some(l) => format!(" (detail 1:{})", 1u32 << l),
            None => format!(" (overview 1:{:.1})", ov.factor),
        };
        if let Some((x, y)) = inside.filter(|_| vectors_only) {
            // the canvas' pixels mean nothing: only where it is
            if let Some((gx, gy)) = info.pixel_to_geo(x as f64 + 0.5, y as f64 + 0.5) {
                status += &format!("  |  x {gx:.6}  y {gy:.6}");
            }
        } else if let Some((x, y)) = inside {
            status += &format!("  |  col {x}  row {y}");
            if let Some((gx, gy)) = info.pixel_to_geo(x as f64 + 0.5, y as f64 + 0.5) {
                status += &format!("  |  x {gx:.6}  y {gy:.6}");
            }
            let p = (y * ov.h / info.height).min(ov.h - 1) * ov.w + (x * ov.w / info.width).min(ov.w - 1);
            let hover = self.series.hover.as_ref().filter(|h| h.x == x && h.y == y && d.t < h.values.len());
            let approx = |h: &crate::series::SeriesView| if h.exact { "" } else { "(approx.)" };
            let g = |v: f64| format_g(v, 6);
            if let Some(c) = composite {
                status += &c.status_at(x as f64 + 0.5, y as f64 + 0.5, d.t, n);
            } else if d.mode == Mode::Composite {
                let (t0, t1) = d.temporal_window(info, d.t);
                status += &format!("  |  composite {} (overview)", g(f64::from(d.composite_at(s, p, t0, t1))));
            } else if let (true, Some(st)) = (d.mode.needs_stats(), &s.stats) {
                // the quantity shown (mean, trend...) at the overview pixel under the cursor
                let (a, b) = (st.s0[p], st.s1[p]);
                let v = match d.mode {
                    Mode::Mean => a[0],
                    Mode::Std => a[1],
                    Mode::Slope => a[2],
                    Mode::Min => b[0],
                    Mode::Max => b[1],
                    Mode::Amplitude => b[1] - b[0],
                    Mode::R2 => b[2],
                    _ => b[3],
                };
                if d.mode == Mode::Anomaly {
                    if let Some(h) = hover {
                        status += &format!("  |  anomaly {}", g(f64::from(h.values[d.t] - a[0])));
                    }
                } else if d.mode == Mode::DropDate {
                    let at = st.s2[p];
                    let when = if at.is_nan() { "none".to_string() } else { info.layers[at as usize].label.clone() };
                    status += &format!("  |  largest drop {when} ({})", format_g(f64::from(b[3]), 4));
                } else {
                    let unit = if d.mode == Mode::Slope { if info.time_is_date { "/yr" } else { "/step" } } else { "" };
                    status += &format!("  |  {} {}{unit}", crate::display::mode_name(d.mode), g(f64::from(v)));
                }
            } else if let (Mode::Diff, Some(h)) = (d.mode, hover) {
                if let Some(r) = d.diff_ref_date(d.t, n) {
                    status += &format!("  |  difference {} {}", g(f64::from(h.values[d.t] - h.values[r])), approx(h));
                }
            } else if let Some(h) = hover {
                if self.classes.on() {
                    status += &format!("  |  class {} {}", self.classes.name_of(h.values[d.t]), approx(h));
                } else {
                    status += &format!("  |  value {} {}", g(f64::from(h.values[d.t])), approx(h));
                }
            }
        }
        self.basemap_note(&painter, rect);
        if let Some((x, y)) = inside {
            status += &self.embedding_status_at(x, y);
        }
        if let Some(v) = self.vector_status(ui.input(|i| i.modifiers.alt)) {
            status += &v;
        }
        ui.weak(status);
    }

    /// Every layer drawn into the GPU target `slot` (C++ renderMap): `size` pixels showing the main map's view on
    /// a canvas of `canvas` points at `ppp` pixels per point, the basemap under them, the detail tiles over the
    /// active one. The main map's target (slot 0) is drawn again only when what it shows changed; another slot
    /// (the self-test's) always.
    pub(crate) fn render_map(&mut self, slot: i32, size: [u32; 2], canvas: [f32; 2], ppp: f32) {
        let Some(g) = self.grid() else { return };
        let (ow, oh, fx, fy) = (g.ow, g.oh, g.fx, g.fy);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(canvas[0], canvas[1]));
        // each layer's date of its embeddings
        let emb_dates: Vec<usize> = (0..self.layers.len()).map(|i| self.layer_date(i)).collect();
        let vector_sig = self.vector_gpu_sig();
        let Some(gpu) = self.gpu.as_mut() else { return };
        let m = &mut self.map;
        let Some(s) = self.session.as_mut() else { return };
        let to_px = |m: &MapView, x0: f64, y0: f64, x1: f64, y1: f64| -> [f32; 4] {
            let (a, b) = (m.screen_of(rect, x0, y0), m.screen_of(rect, x1, y1));
            [(a.x - rect.min.x) * ppp, (a.y - rect.min.y) * ppp, (b.x - rect.min.x) * ppp, (b.y - rect.min.y) * ppp]
        };
        let mut params = self.display.layer_params(s);
        if self.classes.on() && params.mode == Mode::Value {
            params.class_lut = self.classes.lut.clone();
        }
        let cmap = self.display.cmap[params.mode as usize];
        let mode = params.mode;
        let t = self.display.t;
        let ready = self.display.ready(s, &params);
        let vsig = vector_sig;
        // the detail tiles of the view: the visible part in source pixels, at screen pixels per source pixel
        let (v0, v1) = (m.image_at(rect, rect.min), m.image_at(rect, rect.max));
        let view = ViewRect {
            x0: v0[0] * fx,
            y0: v0[1] * fy,
            x1: v1[0] * fx,
            y1: v1[1] * fy,
            scale: m.scale * f64::from(ppp) / fx,
        };
        if slot == 0 {
            m.last_view = Some(view);
        }
        // an RGB composite reads its own detail
        let composite = self.composite.as_ref().is_some_and(|c| c.on);
        let detail = matches!(mode, Mode::Value | Mode::Diff) && !s.s.defer_random_reads() && !composite;
        let prefetch = self.display.playing.then(|| (t + 1) % s.info.t());
        let detail = detail && self.display.detail && (mode != Mode::Diff || ready);
        let mut level = None;
        if detail {
            level = s.tiles.update(s.s.fg(), t, &view, prefetch);
            if mode == Mode::Diff {
                s.tiles.update(s.s.fg(), params.layers[1] as usize, &view, None);
            }
        }
        let key = (
            mode,
            params.layers,
            params.range.map(f32::to_bits),
            cmap,
            to_px(m, 0.0, 0.0, ow, oh).map(f32::to_bits),
            s.tiles.gpu_tiles(),
            params.class_lut.as_ref().map(|l| std::sync::Arc::as_ptr(l) as usize),
            vsig,
        );
        if slot == 0 {
            m.detail_level = level;
            m.redrawn = false;
        }
        if slot != 0 || m.dirty || m.drawn.as_ref() != Some(&key) {
            let bg = m.export_bg.unwrap_or_else(theme::map_background);
            let mut f = gpu.begin_map(slot, size, bg);
            // the basemap under the layers, in map space (the active layer's source pixels)
            self.basemap.draw(gpu, &mut f, &view, bg, |q| to_px(m, q[0] / fx, q[1] / fy, q[2] / fx, q[3] / fy));
            let to_map = |q: [f64; 4]| to_px(m, q[0] / fx, q[1] / fy, q[2] / fx, q[3] / fy);
            // the vector layers in the series' order (order.rs): each drawn before the first series above it
            let vv = janus_gpu::VectorView {
                a: [m.scale * f64::from(ppp) / fx, m.scale * f64::from(ppp) / fy],
                centre: [m.center[0] * fx, m.center[1] * fy],
                t0: [f64::from(canvas[0] * ppp) / 2.0, f64::from(canvas[1] * ppp) / 2.0],
            };
            let margin = 8.0 / vv.a[0].abs().max(1e-12);
            let seen = [view.x0 - margin, view.y0 - margin, view.x1 + margin, view.y1 + margin];
            let mut vi = 0;
            let no_vectors = m.export_no_vectors;
            let vectors = &self.vectors;
            let mut vectors_under = |gpu: &mut janus_gpu::Gpu, f: &mut janus_gpu::MapFrame, z: u64| {
                while let Some(v) = vectors.get(vi).filter(|v| v.z < z) {
                    if let Some(g) =
                        v.gpu.ready.as_ref().filter(|_| v.visible && !no_vectors && v.gpu.draws(v.placed_for))
                    {
                        g.draw(gpu, f, &vv, seen, ppp, 0.0);
                    }
                    vi += 1;
                }
            };
            // every layer, bottom to top; the active one with its detail
            for li in 0..self.layers.len() {
                let t_emb = emb_dates.get(li).copied();
                let l = &mut self.layers[li];
                vectors_under(gpu, &mut f, l.z);
                if !l.place.aligned {
                    continue;
                }
                if !l.visible {
                    // a series hidden: the maps its runs made still shown, as layers of their own
                    let info = if li == self.active { Some(&s.info) } else { l.session.as_ref().map(|x| &x.info) };
                    if let Some(info) = info {
                        crate::zeit::draw_results(gpu, &mut f, &self.zeit.results, info, &l.place, &to_map);
                    }
                    continue;
                }
                if let (Some(e), Some(te)) = (l.emb.as_ref().filter(|e| e.drawn()), t_emb) {
                    e.draw(te, &l.place, l.opacity, gpu, &mut f, &to_map);
                    continue;
                }
                if li != self.active {
                    State::draw_layer(l, gpu, &mut f, &to_map);
                    if let Some(ls) = &l.session {
                        crate::zeit::draw_results(gpu, &mut f, &self.zeit.results, &ls.info, &l.place, &to_map);
                    }
                    continue;
                }
                let alpha = l.opacity;
                if let Some(c) = self.composite.as_mut().filter(|c| c.on) {
                    // three bands as RGB, in detail when zoomed in
                    State::draw_composite(c, gpu, &mut f, t, s.info.t(), alpha, |w| {
                        to_px(m, w[0] / fx, w[1] / fy, w[2] / fx, w[3] / fy)
                    });
                } else if let (true, Some(cube)) = (ready, &s.cube) {
                    let tiles: Vec<_> = if !detail {
                        Vec::new()
                    } else if mode == Mode::Diff {
                        s.tiles
                            .visible_pairs(t, params.layers[1] as usize, &view)
                            .into_iter()
                            .map(|(a, b)| (a, Some(b)))
                            .collect()
                    } else {
                        s.tiles.visible(t, &view).into_iter().map(|a| (a, None)).collect()
                    };
                    let place =
                        |x: f64, y: f64, sw: f64, sh: f64| to_px(m, x / fx, y / fy, (x + sw) / fx, (y + sh) / fy);
                    let full = to_px(m, 0.0, 0.0, ow, oh);
                    if alpha < 1.0 && !tiles.is_empty() {
                        // translucent: the overview only where no tile goes, or the tiles' squares show darker
                        let holes: Vec<[f32; 4]> =
                            tiles.iter().map(|(tl, _)| place(tl.x, tl.y, tl.sw, tl.sh)).collect();
                        for clip in janus_gpu::uncovered(full, &holes) {
                            f.clip = Some(clip);
                            gpu.draw_cube(&mut f, cube, full, &params, cmap, alpha);
                        }
                        f.clip = None;
                    } else {
                        gpu.draw_cube(&mut f, cube, full, &params, cmap, alpha);
                    }
                    for (tile, reference) in tiles {
                        params.tile2 = reference.map(|r| r.tex.clone());
                        gpu.draw_tile(
                            &mut f,
                            cube,
                            &tile.tex,
                            place(tile.x, tile.y, tile.sw, tile.sh),
                            &params,
                            cmap,
                            alpha,
                        );
                    }
                }
                crate::zeit::draw_results(gpu, &mut f, &self.zeit.results, &s.info, &l.place, &to_map);
            }
            // the vector layers over the top series
            vectors_under(gpu, &mut f, u64::MAX);
            gpu.end_map(f);
            if slot == 0 {
                m.drawn = Some(key);
                m.dirty = false;
                m.redrawn = true;
            }
        }
    }
}

/// The interface's text size (the map's labels follow the font size setting, as in the C++).
pub(crate) fn body_font(painter: &egui::Painter) -> FontId {
    let size = painter.ctx().style_of(painter.ctx().theme()).text_styles[&egui::TextStyle::Body].size;
    FontId::proportional(size)
}

impl State {
    /// The time bar over the map: the date before and after (in the season), playback and its speed, the date.
    fn time_bar(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        let n = info.t();
        let d = &mut self.display;
        let (mut step, mut set) = (0i64, None);
        ui.horizontal(|ui| {
            if ui.button("<").clicked() {
                step = -1;
            }
            if ui.add_sized([60.0, 18.0], egui::Button::new(if d.playing { "Pause" } else { "Play" })).clicked() {
                d.playing = !d.playing;
            }
            if ui.button(">").clicked() {
                step = 1;
            }
            let mut t = d.t;
            let label = info.layers[d.t].label.clone();
            // room for the date, the speed and the count after it, at the font's size
            let k = ui.text_style_height(&egui::TextStyle::Body) / 13.0;
            ui.spacing_mut().slider_width = (ui.available_width() - 330.0 * k).max(120.0);
            if ui.add(egui::Slider::new(&mut t, 0..=n - 1).show_value(false)).changed() {
                set = Some(t);
            }
            ui.label(label);
            ui.spacing_mut().slider_width = 100.0 * k;
            let unit = if info.spectral { "bands/s" } else { "dates/s" };
            ui.add(
                egui::Slider::new(&mut d.fps, 0.5..=30.0)
                    .logarithmic(true)
                    .fixed_decimals(1)
                    .suffix(format!(" {unit}")),
            );
            ui.weak(format!("{}/{n}", d.t + 1));
        });
        if step != 0 {
            self.step_date(step);
        }
        if let Some(t) = set {
            self.set_date(t);
        }
    }

    /// What the map shows (app.cpp, the map's title): the dates of the RGB or the difference, the composite's
    /// window, the date and the mode, or the mode over the dates (and the season).
    pub(crate) fn map_title(&self) -> String {
        let (Some(s), d) = (&self.session, &self.display) else { return String::new() };
        let info = &s.info;
        let label = |t: usize| info.layers[t.min(info.t() - 1)].label.as_str();
        match d.mode {
            Mode::Rgb => format!("RGB: {} / {} / {}", label(d.rgb[0]), label(d.rgb[1]), label(d.rgb[2])),
            Mode::Diff => {
                format!("{} - {}  |  Difference", label(d.t), d.diff_ref_date(d.t, info.t()).map_or("(none)", label))
            }
            Mode::Composite => d.temporal_text(info, d.t),
            m if crate::display::time_dependent(m) => format!("{}  |  {}", label(d.t), crate::display::mode_name(m)),
            m => format!(
                "{}  |  {} {}{}",
                crate::display::mode_name(m),
                info.t(),
                if info.spectral { "bands" } else { "dates" },
                if d.season.active(info) { format!("  |  {}", d.season.text()) } else { String::new() }
            ),
        }
    }
}
