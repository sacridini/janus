//! Layers (src/app_layers.cpp): several series open at once and the Layers panel.
//!
//! Map space is the active layer's pixel grid. Other layers are placed through their geotransforms: straight (an
//! affine map) with the same CRS and no rotation, else reprojected (janus-geo: OGR/PROJ on the CPU, a warp grid in
//! the shader). The active layer's state lives in the State's members (session, display, classes, composite) and is
//! swapped in and out of its SeriesLayer when the active layer changes, as the C++'s display state is.

use egui::Ui;
use janus_cube::CubeInfo;
use janus_geo::Reprojection;
use janus_gpu::{GpuTexture, Warp};

use crate::app::State;
use crate::classes::LayerClasses;
use crate::composite::BandComposite;
use crate::display::Display;
use crate::session::Session;
use crate::theme;

/// Where a layer is drawn on the map: this layer's pixels -> the active layer's pixels, x' = ax + bx x,
/// y' = ay + by y, or through a reprojection.
pub struct Placement {
    pub ax: f64,
    pub bx: f64,
    pub ay: f64,
    pub by: f64,
    /// can be placed on the map (straight, or reprojected)
    pub aligned: bool,
    /// why not, or what it costs
    pub note: String,
    /// another CRS than the active layer's, or a rotated grid; its warp grid on the GPU
    pub reproj: Option<(Reprojection, Option<GpuTexture>)>,
}

impl Default for Placement {
    fn default() -> Self {
        Placement { ax: 0.0, bx: 1.0, ay: 0.0, by: 1.0, aligned: true, note: String::new(), reproj: None }
    }
}

impl Placement {
    /// A pixel of the layer -> a pixel of the active layer.
    pub fn to_active(&self, lx: f64, ly: f64) -> Option<(f64, f64)> {
        if !self.aligned {
            return None;
        }
        match &self.reproj {
            Some((r, _)) => r.to_active(lx, ly),
            None => Some((self.ax + self.bx * lx, self.ay + self.by * ly)),
        }
    }

    /// A pixel of the active layer -> the layer's continuous pixel coordinates (no rounding, no bounds).
    pub fn to_layer(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if !self.aligned {
            return None;
        }
        match &self.reproj {
            Some((r, _)) => r.to_layer(x, y),
            None if self.bx == 0.0 || self.by == 0.0 => None,
            None => Some(((x - self.ax) / self.bx, (y - self.ay) / self.by)),
        }
    }

    /// A pixel of the active layer -> the layer's pixel under it (None outside the layer).
    pub fn pixel(&self, info: &CubeInfo, x: f64, y: f64) -> Option<(usize, usize)> {
        let (fx, fy) = self.to_layer(x, y)?;
        (fx >= 0.0 && fy >= 0.0 && fx < info.width as f64 && fy < info.height as f64)
            .then_some((fx as usize, fy as usize))
    }

    /// Where the layer's pixels [x0, x1) × [y0, y1) are drawn in map space (the active layer's pixels) and the warp
    /// that goes with it (none for a layer on the same CRS and grid lines). None if nothing of it can be drawn.
    pub fn quad(&self, info: &CubeInfo, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<([f64; 4], Option<Warp>)> {
        if !self.aligned {
            return None;
        }
        let Some((r, grid)) = &self.reproj else {
            let (a, b) = (self.to_active(x0, y0)?, self.to_active(x1, y1)?);
            return Some(([a.0, a.1, b.0, b.1], None));
        };
        let grid = grid.as_ref().filter(|_| r.has_domain())?;
        let d = r.domain;
        let mut q = d;
        let (w, h) = (info.width as f64, info.height as f64);
        if x0 > 0.0 || y0 > 0.0 || x1 < w || y1 < h {
            // part of the layer (a tile, an image): the bounding box of its edges, plus a margin for their
            // curvature between the points; the shader discards what falls outside it
            let b = r.footprint(x0, y0, x1, y1)?;
            let (mx, my) = ((b[2] - b[0]) * 0.02 + 1.0, (b[3] - b[1]) * 0.02 + 1.0);
            q = [(b[0] - mx).max(d[0]), (b[1] - my).max(d[1]), (b[2] + mx).min(d[2]), (b[3] + my).min(d[3])];
            if q[2] <= q[0] || q[3] <= q[1] {
                return None;
            }
        }
        let (dw, dh) = (d[2] - d[0], d[3] - d[1]);
        let warp = Warp {
            grid: grid.clone(),
            quad: [
                ((q[0] - d[0]) / dw) as f32,
                ((q[1] - d[1]) / dh) as f32,
                ((q[2] - q[0]) / dw) as f32,
                ((q[3] - q[1]) / dh) as f32,
            ],
            src: [(x0 / w) as f32, (y0 / h) as f32, ((x1 - x0) / w) as f32, ((y1 - y0) / h) as f32],
        };
        Some((q, Some(warp)))
    }
}

impl Placement {
    /// The map-space rectangle in the layer's own pixels (what to read of it) and the zoom there: target pixels per
    /// layer pixel, at `px_per_map_px`.
    pub fn view(&self, x0: f64, y0: f64, x1: f64, y1: f64, px_per_map_px: f64) -> janus_session::ViewRect {
        use janus_session::ViewRect;
        let Some((r, _)) = &self.reproj else {
            return ViewRect {
                x0: (x0 - self.ax) / self.bx,
                y0: (y0 - self.ay) / self.by,
                x1: (x1 - self.ax) / self.bx,
                y1: (y1 - self.ay) / self.by,
                scale: px_per_map_px * self.bx,
            };
        };
        // reprojected: the bounding box of the view's edges in the layer's pixels
        let p = r.polygon(x0, y0, x1, y1, 8);
        let k = r.layer_px_per_map_px((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        if p.is_empty() || k.is_nan() || k <= 0.0 {
            return ViewRect { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.0, scale: 0.0 }; // nothing to read
        }
        let mut v = ViewRect {
            x0: f64::INFINITY,
            y0: f64::INFINITY,
            x1: f64::NEG_INFINITY,
            y1: f64::NEG_INFINITY,
            scale: px_per_map_px / k,
        };
        for q in p.as_chunks::<2>().0 {
            v.x0 = v.x0.min(q[0]);
            v.y0 = v.y0.min(q[1]);
            v.x1 = v.x1.max(q[0]);
            v.y1 = v.y1.max(q[1]);
        }
        v
    }
}

/// An open series. The active one's session, display state, classes and composite are the State's (here None and
/// defaults); the others keep theirs here.
pub struct SeriesLayer {
    pub session: Option<Session>,
    pub inputs: Vec<String>,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub disp: Display,
    pub classes: LayerClasses,
    pub composite: Option<BandComposite>,
    pub place: Placement,
    /// its series at the active layer's cursor, pins and ROI ("All visible layers")
    pub series: crate::series::OtherSeries,
    /// a layer of embeddings (or a spectrum's analysis): its views
    pub emb: Option<Box<crate::embeddings::EmbeddingLayer>>,
}

/// The map's options a layer does not keep (the C++'s App members): the season, the temporal composites' options,
/// playback.
type MapOptions = (crate::season::Season, i32, i32, i32, bool);

fn map_options(d: &Display) -> MapOptions {
    (d.season, d.reducer, d.window, d.days, d.playing)
}

fn set_map_options(d: &mut Display, o: MapOptions) {
    (d.season, d.reducer, d.window, d.days, d.playing) = o;
}

fn same_crs(a: &CubeInfo, b: &CubeInfo) -> bool {
    if !a.crs_authority.is_empty() || !b.crs_authority.is_empty() {
        return a.crs_authority == b.crs_authority;
    }
    a.crs_name == b.crs_name
}

/// Dates with dates, wavelengths with wavelengths: a layer's date follows the active one's by time.
pub fn matchable_axis(a: &CubeInfo, b: &CubeInfo) -> bool {
    (a.time_is_date && b.time_is_date) || (a.spectral && b.spectral)
}

impl State {
    /// Layer i's session (the active one's is the State's).
    pub fn layer_session(&self, i: usize) -> Option<&Session> {
        if i == self.active { self.session.as_ref() } else { self.layers.get(i)?.session.as_ref() }
    }

    /// A layer's name: its file or folder (a URL, a NetCDF / Zarr variable), numbered when another has it.
    pub fn layer_name(&self, info: &CubeInfo, inputs: &[String]) -> String {
        let one = inputs.len() == 1;
        let p = std::path::PathBuf::from(if one { inputs[0].clone() } else { info.first_path.clone() });
        let p = if one { p } else { p.parent().map(|d| d.to_path_buf()).unwrap_or(p) };
        let mut n =
            p.file_stem().or_else(|| p.file_name()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        if one && (janus_cube::is_remote_path(&inputs[0]) || janus_cube::dataset_file(&inputs[0]) != inputs[0]) {
            n = janus_cube::display_name(&inputs[0]);
        }
        if n.is_empty() {
            n = p.to_string_lossy().into_owned();
        }
        // two folders called "ndvi"
        let dup = self.layers.iter().filter(|l| l.name == n || l.name.starts_with(&format!("{n} ("))).count();
        if dup > 0 { format!("{n} ({})", dup + 1) } else { n }
    }

    /// Every layer placed on the active layer's grid: straight, reprojected (kept while both stay the same), or
    /// shown only when active.
    pub(crate) fn update_alignment(&mut self) {
        let (Some(s), Some(gpu)) = (&self.session, &self.gpu) else { return };
        let a = &s.info;
        for (i, l) in self.layers.iter_mut().enumerate() {
            let p = &mut l.place;
            p.note.clear();
            (p.ax, p.ay, p.bx, p.by) = (0.0, 0.0, 1.0, 1.0);
            if i == self.active {
                p.reproj = None;
                p.aligned = true;
                continue;
            }
            let Some(b) = l.session.as_ref().map(|s| s.info.clone()) else { continue };
            let (Some(ga), Some(gb)) = (a.geo_transform, b.geo_transform) else {
                // without georeferencing, only identical grids can be overlaid
                p.reproj = None;
                p.aligned = a.width == b.width && a.height == b.height;
                if !p.aligned {
                    p.note = "no georeferencing and a different size: shown only when active".into();
                }
                continue;
            };
            let rotated = ga[2] != 0.0 || ga[4] != 0.0 || gb[2] != 0.0 || gb[4] != 0.0;
            if !rotated && same_crs(a, &b) {
                // the fast path: an affine map
                p.reproj = None;
                p.aligned = true;
                p.bx = gb[1] / ga[1];
                p.by = gb[5] / ga[5];
                p.ax = (gb[0] - ga[0]) / ga[1];
                p.ay = (gb[3] - ga[3]) / ga[5];
                continue;
            }
            // another CRS or a rotated grid: reprojected
            if p.reproj.as_ref().is_none_or(|(r, _)| r.active_id != a.id || r.layer_id != b.id) {
                match Reprojection::new(a, &b, 0.6) {
                    Ok(r) => {
                        let grid =
                            r.has_domain().then(|| gpu.create_warp_grid(r.grid_w as u32, r.grid_h as u32, &r.grid));
                        p.reproj = Some((r, grid));
                    }
                    Err(why) => {
                        p.reproj = None;
                        p.aligned = false;
                        let crs = if b.crs_authority.is_empty() { &b.crs_name } else { &b.crs_authority };
                        let what = if rotated { "rotated grid".to_string() } else { format!("different CRS ({crs})") };
                        p.note = format!("{what}, {why}: shown only when active");
                        continue;
                    }
                }
            }
            p.aligned = true;
        }
    }

    /// Every non-active layer shows the date nearest to the active layer's date.
    pub(crate) fn sync_layer_times(&mut self) {
        let Some(s) = &self.session else { return };
        let a = &s.info;
        let t = self.display.t;
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(ls) = l.session.as_ref().filter(|_| i != self.active) else { continue };
            let b = &ls.info;
            let lt = if matchable_axis(a, b) { b.nearest_layer(a.layers[t].time) } else { t.min(b.t() - 1) };
            if lt != l.disp.t {
                l.disp.t = lt;
                self.map.dirty = true;
            }
            ls.s.overview.set_focus(l.disp.t);
        }
    }

    /// Makes layer i the active one: its grid becomes the map's; the view and the pins are carried to it.
    pub fn set_active(&mut self, i: usize, ctx: &egui::Context) {
        if i >= self.layers.len() {
            return;
        }
        let carry = self.session.is_some() && i != self.active;
        if carry {
            self.update_alignment(); // the new layer may have just been opened
        }
        // the view (its centre and zoom) and the pins live in map space: carried to the new grid through the
        // transformation that places the new layer on the current map (straight or reprojected)
        let mut keep_view = None;
        let mut pins_at: Vec<Option<(f64, f64)>> = Vec::new();
        if carry
            && let Some(s) = &self.session
            && let Some(n) = self.layers[i].session.as_ref()
            && self.layers[i].place.aligned
        {
            let ov = &s.s.overview;
            let (fx, fy) = (s.info.width as f64 / ov.w as f64, s.info.height as f64 / ov.h as f64);
            let place = &self.layers[i].place;
            if self.map.touched && self.map.scale > 0.0 {
                let (mx, my) = (self.map.center[0] * fx, self.map.center[1] * fy);
                let k = match &place.reproj {
                    Some((r, _)) => r.layer_px_per_map_px(mx, my),
                    None => 1.0 / (place.bx * place.by).abs().sqrt(),
                };
                if let Some((lx, ly)) = place.to_layer(mx, my).filter(|_| k > 0.0) {
                    let nov = &n.s.overview;
                    let (nfx, nfy) = (n.info.width as f64 / nov.w as f64, n.info.height as f64 / nov.h as f64);
                    // screen points per source pixel of the active layer, then of the new one
                    let scale_src = self.map.scale / fx / k;
                    keep_view = Some(([lx / nfx, ly / nfy], scale_src * nfx));
                }
            }
            pins_at = self.series.pins.iter().map(|p| place.to_layer(p.x as f64 + 0.5, p.y as f64 + 0.5)).collect();
        }
        // the active layer's state into its entry, the new one's out of its own; the season, the composites'
        // options and playback are the map's, not a layer's
        let old = self.active;
        if self.session.is_some() && old < self.layers.len() {
            let l = &mut self.layers[old];
            l.session = self.session.take();
            l.disp = std::mem::take(&mut self.display);
            l.classes = std::mem::take(&mut self.classes);
            l.composite = self.composite.take();
        }
        let options = map_options(&self.layers[old.min(self.layers.len() - 1)].disp);
        self.active = i;
        let l = &mut self.layers[i];
        self.session = l.session.take();
        self.display = std::mem::take(&mut l.disp);
        self.classes = std::mem::take(&mut l.classes);
        self.composite = l.composite.take();
        if carry {
            set_map_options(&mut self.display, options);
        }
        self.update_alignment();

        // the view
        match keep_view {
            Some((center, scale)) => {
                self.map.set_view(center, scale);
                self.map.touched = true;
            }
            None => self.map.fit(),
        }
        // pins: on the new grid (those outside it dropped), read again
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        let old_pins = std::mem::take(&mut self.series.pins);
        let mut carried = Vec::new();
        for (k, p) in old_pins.into_iter().enumerate() {
            let (px, py) = match pins_at.get(k) {
                Some(Some((lx, ly))) => {
                    if !(*lx >= 0.0 && *ly >= 0.0 && *lx < info.width as f64 && *ly < info.height as f64) {
                        continue;
                    }
                    (*lx as usize, *ly as usize)
                }
                Some(None) => continue,
                None => (p.x, p.y), // unrelated grids: they keep their pixel
            };
            if px >= info.width || py >= info.height {
                continue;
            }
            carried.push((p.id, p.color, px, py));
        }
        // the cursor and the ROI belong to the previous grid
        self.rebuild_pins(carried);
        self.clear_roi();
        self.sync_layer_times();
        self.map.dirty = true;
        let n = self.layers.len();
        let title = format!(
            "Janus - {} ({}){}",
            self.layers[i].name,
            info.description,
            if n > 1 { format!("  [{n} layers]") } else { String::new() }
        );
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    /// The same files with another band selection: the layer keeps its place, name, visibility, opacity, display
    /// mode, date, pins and view; the stretches go back to automatic since the values changed.
    pub(crate) fn replace_layer_session(
        &mut self,
        i: usize,
        info: CubeInfo,
        o: &crate::session::Opening,
        ctx: &egui::Context,
    ) {
        let s = Session::new(info, o, &self.session_settings(), ctx);
        let n = s.info.t();
        let carry = |old: &Display| Display {
            mode: old.mode,
            cmap: old.cmap,
            t: old.t.min(n - 1),
            rgb: [0, n / 2, n - 1],
            diff_ref: old.diff_ref.min(n as i32 - 1),
            per_date_range: old.per_date_range,
            detail: old.detail,
            fps: old.fps,
            season: old.season,
            reducer: old.reducer,
            window: old.window,
            days: old.days,
            ..Default::default()
        };
        self.layers[i].emb = crate::embeddings::EmbeddingLayer::new(&s.info);
        if i == self.active {
            self.display = carry(&self.display);
            self.session = Some(s);
            self.classes = Default::default();
            self.clear_roi();
            let at = self.series.pins.iter().map(|p| (p.id, p.color, p.x, p.y)).collect();
            self.rebuild_pins(at);
        } else {
            let l = &mut self.layers[i];
            l.disp = carry(&l.disp);
            l.session = Some(s);
            l.classes = Default::default();
            l.series = Default::default();
        }
        self.update_alignment();
        self.sync_layer_times();
        self.map.dirty = true;
    }

    /// Layers i and j change places in the drawing order (the active one stays active).
    pub fn swap_layers(&mut self, i: usize, j: usize) {
        self.layers.swap(i, j);
        if self.active == i {
            self.active = j;
        } else if self.active == j {
            self.active = i;
        }
        self.map.dirty = true;
    }

    /// Closes layer i.
    pub fn remove_layer(&mut self, i: usize, ctx: &egui::Context) {
        if i >= self.layers.len() {
            return;
        }
        let was_active = i == self.active;
        let options = map_options(&self.display);
        if was_active {
            self.session = None;
            self.display = Display::default();
            self.classes = Default::default();
            self.composite = None;
        }
        self.layers.remove(i);
        if self.layers.is_empty() {
            self.active = 0;
            self.series = Default::default();
            ctx.send_viewport_cmd(egui::ViewportCommand::Title("Janus".into()));
            return;
        }
        if was_active {
            // nothing to save: the removed layer's state is gone
            self.active = usize::MAX;
            self.set_active(i.min(self.layers.len() - 1), ctx);
            set_map_options(&mut self.display, options);
        } else {
            if i < self.active {
                self.active -= 1;
            }
            self.update_alignment();
            self.map.dirty = true;
        }
    }

    /// Every layer closed.
    pub fn close_all(&mut self, ctx: &egui::Context) {
        self.session = None;
        self.layers.clear();
        self.active = 0;
        self.display = Display::default();
        self.classes = Default::default();
        self.composite = None;
        self.series = Default::default();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title("Janus".into()));
    }

    /// A non-active layer into the map's frame, with its own display state, through its placement (`to_map`: a
    /// map-space box, the active layer's pixels, to the target's pixels).
    pub(crate) fn draw_layer(
        l: &mut SeriesLayer,
        gpu: &janus_gpu::Gpu,
        f: &mut janus_gpu::MapFrame,
        to_map: &dyn Fn([f64; 4]) -> [f32; 4],
    ) {
        let Some(s) = &l.session else { return };
        let info = &s.info;
        let d = &l.disp;
        if let Some(c) = l.composite.as_mut().filter(|c| c.on) {
            // three bands as RGB: the images read while it was active
            let place = &l.place;
            c.draw(d.t, info.t(), |tex, w| {
                if let Some((q, warp)) = place.quad(info, w[0], w[1], w[2], w[3]) {
                    gpu.draw_image(f, tex, to_map(q), l.opacity, warp.as_ref(), true);
                }
            });
            return;
        }
        let mut params = d.layer_params(s);
        if l.classes.on() && params.mode == janus_gpu::Mode::Value {
            params.class_lut = l.classes.lut.clone();
        }
        let cmap = d.cmap[params.mode as usize];
        if let (true, Some(cube)) = (d.ready(s, &params), &s.cube)
            && let Some((q, warp)) = l.place.quad(info, 0.0, 0.0, info.width as f64, info.height as f64)
        {
            params.warp = warp;
            gpu.draw_cube(f, cube, to_map(q), &params, cmap, l.opacity);
        }
    }

    /// The non-active layers' background work: their dates to the GPU, their statistics, their classes.
    pub(crate) fn pump_layers(&mut self, ctx: &egui::Context) {
        let Some(gpu) = self.gpu.as_mut() else { return };
        for (i, l) in self.layers.iter_mut().enumerate() {
            if i == self.active {
                continue;
            }
            let Some(s) = l.session.as_mut() else { continue };
            if s.pump(gpu, ctx) {
                self.map.dirty = true;
            }
            let (changed, became_on) = l.classes.update(s, gpu);
            if changed {
                self.map.dirty = true;
            }
            if became_on {
                l.disp.mode = janus_gpu::Mode::Value;
            }
        }
    }

    /// The Layers panel: every layer, top of the list drawn on top.
    pub(crate) fn layers_ui(&mut self, ui: &mut Ui) {
        if self.layers.is_empty() {
            ui.weak("No layers. Open a series (File, drag and drop\nor the command line).");
            self.vectors_ui(ui);
            return;
        }
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            if ui.button("Add layer...").on_hover_text("Open another series as a new layer (files)").clicked()
                && let Some(files) = rfd::FileDialog::new().pick_files()
            {
                self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, &ctx);
            }
            if ui
                .button("Add folder...")
                .on_hover_text("Open another series as a new layer (a folder, 1 file per date)")
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.open_inputs(vec![dir.to_string_lossy().into_owned()], true, &ctx);
            }
        });
        ui.weak("Top of the list = drawn on top. Click a name to make it active.");
        ui.separator();
        self.vectors_ui(ui); // drawn over every raster layer
        let (mut up, mut down, mut remove, mut activate) = (None, None, None, None);
        let n = self.layers.len();
        let active = self.active;
        for i in (0..n).rev() {
            let is_active = i == active;
            // the active layer's session is the State's
            let info = if is_active {
                self.session.as_ref().map(|s| s.info.clone())
            } else {
                self.layers[i].session.as_ref().map(|s| s.info.clone())
            };
            let Some(info) = info else { continue };
            let l = &mut self.layers[i];
            ui.push_id(i, |ui| {
                // the maps its runs made: layers of their own above it, shown or hidden apart from it
                if State::results_ui(ui, &mut self.zeit.results, info.id, &l.name, &mut self.map.dirty) {
                    ui.separator();
                }
                ui.horizontal(|ui| {
                    if ui
                        .checkbox(&mut l.visible, "")
                        .on_hover_text(if l.visible { "Hide this layer" } else { "Show this layer" })
                        .changed()
                    {
                        self.map.dirty = true;
                    }
                    let label = format!("{}{}", if is_active { "> " } else { "  " }, l.name);
                    let text = if is_active {
                        egui::RichText::new(label).color(theme::accent())
                    } else {
                        egui::RichText::new(label)
                    };
                    let w = (ui.available_width() - 76.0).max(40.0);
                    // the name left-aligned over the row's width (as a menu's items)
                    let h = ui.spacing().interact_size.y;
                    let r = ui
                        .allocate_ui_with_layout(
                            egui::vec2(w, h),
                            egui::Layout::top_down_justified(egui::Align::LEFT),
                            |ui| ui.add(egui::Button::selectable(is_active, text)),
                        )
                        .inner
                        .on_hover_text(format!(
                            "{}\n{}, {} x {} px, {} dates ({} .. {})\nCRS: {}",
                            if l.inputs.len() == 1 { &l.inputs[0] } else { &info.first_path },
                            info.description,
                            info.width,
                            info.height,
                            info.t(),
                            info.layers[0].label,
                            info.layers[info.t() - 1].label,
                            if info.crs_authority.is_empty() { &info.crs_name } else { &info.crs_authority }
                        ));
                    if r.clicked() && !is_active {
                        activate = Some(i);
                    }
                    if ui.add_enabled(i + 1 < n, egui::Button::new("^").small()).clicked() {
                        up = Some(i);
                    }
                    if ui.add_enabled(i > 0, egui::Button::new("v").small()).clicked() {
                        down = Some(i);
                    }
                    if ui.small_button("x").on_hover_text("Close this layer").clicked() {
                        remove = Some(i);
                    }
                });
                ui.indent("layer", |ui| {
                    if !is_active && let Some(s) = &l.session {
                        ui.weak(format!("date {}", info.layers[l.disp.t.min(info.t() - 1)].label));
                        let ov = &s.s.overview;
                        if !ov.complete() {
                            ui.weak(format!("loading {}/{}", ov.layers_done(), info.t()));
                        }
                    }
                    if !l.place.note.is_empty() {
                        ui.colored_label(theme::warning(), &l.place.note);
                    }
                    if let Some((r, _)) = l.place.reproj.as_ref().filter(|_| !is_active) {
                        let text = if r.same_crs {
                            "rotated grid, placed on the map's".to_string()
                        } else {
                            format!("reprojected from {}", r.crs_text)
                        };
                        ui.weak(text).on_hover_text(format!(
                            "Drawn through a grid of {} x {} points (interpolation error up to {} of\nits pixels, made \
                             in {:.0} ms); cursor, pins and ROI use the exact\ntransformation (PROJ). Nearest \
                             neighbour: the values are not changed.",
                            r.grid_w,
                            r.grid_h,
                            janus_cube::multidim::format_g(r.grid_error, 2),
                            r.build_ms
                        ));
                    }
                    if ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2)).changed()
                    {
                        self.map.dirty = true;
                    }
                });
                ui.separator();
            });
        }
        if let Some(i) = up {
            self.swap_layers(i, i + 1);
        }
        if let Some(i) = down {
            self.swap_layers(i, i - 1);
        }
        if let Some(i) = activate {
            self.set_active(i, &ctx);
        }
        if let Some(i) = remove {
            self.remove_layer(i, &ctx);
        }
    }
}

/// `--selftest-reproject B F` (app_selftest.cpp, stages 40-43): B, a pin at (50, 100), then F (B reprojected to
/// UTM 22S at 15 m) added as a layer; B active again: F reprojected on its grid, the pin back at B's (50, 100); F
/// drawn alone on B's grid where PROJ puts each of its overview's texels.
#[derive(Default)]
pub struct ReprojectTest {
    stage: usize,
    t0: Option<std::time::Instant>,
}

impl State {
    pub fn reproject_selftest_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        let mut st = std::mem::take(&mut self.reproject_test);
        let r = self.reproject_selftest_stage(&mut st, ctx);
        self.reproject_test = st;
        r
    }

    fn reproject_selftest_stage(&mut self, st: &mut ReprojectTest, ctx: &egui::Context) -> Option<i32> {
        let t0 = *st.t0.get_or_insert_with(std::time::Instant::now);
        let fail = |what: &str| {
            println!("FAIL: {what}");
            Some(1)
        };
        let next = |st: &mut ReprojectTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
        };
        let complete = |s: &Session| s.s.overview.complete() && s.stats.is_some();
        match st.stage {
            0 => {
                let s = self.session.as_ref()?;
                if !complete(s) {
                    return None;
                }
                self.add_pin(50, 100);
                let f = self.opts.extra_layers.first()?.clone();
                self.open_inputs(vec![f], true, ctx);
                next(st, "B open, a pin at (50, 100); F (B reprojected to UTM 22S) added as a layer");
            }
            1 => {
                if self.opening.is_some()
                    || self.layers.len() != 2
                    || self.active != 1
                    || !complete(self.session.as_ref()?)
                {
                    return None;
                }
                if !self.layers[0].place.aligned || self.layers[0].place.reproj.is_none() {
                    return fail("F should be active, B reprojected on its grid");
                }
                let pin_f = self.series.pins.first().map(|p| (p.x, p.y));
                self.set_active(0, ctx);
                let f = &self.layers[1];
                let Some((r, Some(_))) = &f.place.reproj else {
                    return fail("F should be reprojected onto B's grid, its warp grid on the GPU");
                };
                // the grid (what the shader interpolates) against the exact transformation, and the round trip
                let (mut max_err, mut round_trip) = (0f64, 0f64);
                for k in 0..4000 {
                    let x = 300.0 * (k as f64 * 0.6180339887).fract();
                    let y = 200.0 * (k as f64 * 0.7548776662).fract();
                    let (Some((ex, ey)), Some((gx, gy))) = (r.to_layer(x, y), r.grid_at(x, y)) else {
                        return fail("points of B should transform to F");
                    };
                    let Some((bx, by)) = r.to_active(ex, ey) else { return fail("and back") };
                    max_err = max_err.max((gx - ex).hypot(gy - ey));
                    round_trip = round_trip.max((bx - x).hypot(by - y));
                }
                println!(
                    "    F on B's grid: {}, grid {} x {} over map x {:.0}..{:.0} y {:.0}..{:.0}, made in {:.1} ms;\n    \
                     grid vs PROJ: {:.2} F px at the cells' centres, {max_err:.2e} at 4000 points; round trip \
                     {round_trip:.1e} px",
                    r.crs_text,
                    r.grid_w,
                    r.grid_h,
                    r.domain[0],
                    r.domain[2],
                    r.domain[1],
                    r.domain[3],
                    r.build_ms,
                    r.grid_error
                );
                let pin = self.series.pins.first().map(|p| (p.x, p.y));
                println!("    pin carried B (50, 100) -> F {pin_f:?} -> B {pin:?}");
                if max_err > 0.01 || round_trip > 1e-6 {
                    return fail("the warp grid should be within 0.01 px of PROJ");
                }
                if pin != Some((50, 100)) {
                    return fail("the pin should come back to B's (50, 100)");
                }
                next(st, "B active again, F reprojected on its grid, the pin back at B's pixel");
            }
            2 => {
                // F alone on B's grid fitted to 400 × 300, in the same colours for every value
                let (w, h) = (400usize, 300usize);
                let (lo, hi) = (1.4f32, 6.0f32);
                let b = self.session.as_ref()?.info.clone();
                let scale = 0.98 * (w as f64 / b.width as f64).min(h as f64 / b.height as f64);
                let off = ((w as f64 - b.width as f64 * scale) * 0.5, (h as f64 - b.height as f64 * scale) * 0.5);
                let f = &mut self.layers[1];
                f.disp.mode = janus_gpu::Mode::Value;
                f.disp.ranges[0].lo = lo;
                f.disp.ranges[0].hi = hi;
                f.disp.ranges[0].manual = true;
                let cmap = f.disp.cmap[0];
                let gpu = self.gpu.as_mut()?;
                let to_map = |q: [f64; 4]| {
                    [
                        (off.0 + q[0] * scale) as f32,
                        (off.1 + q[1] * scale) as f32,
                        (off.0 + q[2] * scale) as f32,
                        (off.1 + q[3] * scale) as f32,
                    ]
                };
                let mut frame = gpu.begin_map(1, [w as u32, h as u32], crate::map::MAP_BACKGROUND);
                State::draw_layer(f, gpu, &mut frame, &to_map);
                gpu.end_map(frame);
                let (_, _, img) = gpu.read_target(1)?;
                gpu.release_target(1);
                // each sampled pixel against F's overview texel PROJ puts there
                let fs = f.session.as_ref()?;
                let (fi, ov) = (&fs.info, &fs.s.overview);
                let Some((r, _)) = &f.place.reproj else { return fail("F lost its reprojection") };
                let layer = ov.layer(f.disp.t)?;
                let lut = janus_gpu::colormaps::all()[cmap].lut();
                let (mut checked, mut edge, mut bad) = (0, 0, 0);
                for sy in (3..h).step_by(5) {
                    for sx in (3..w).step_by(5) {
                        let (mx, my) = ((sx as f64 + 0.5 - off.0) / scale, (sy as f64 + 0.5 - off.1) / scale);
                        let Some((lx, ly)) = r.to_layer(mx, my) else { continue };
                        let (ox, oy) = (lx / fi.width as f64 * ov.w as f64, ly / fi.height as f64 * ov.h as f64);
                        if !(ox >= 0.0 && oy >= 0.0 && ox < ov.w as f64 && oy < ov.h as f64) {
                            continue;
                        }
                        let (fx, fy) = (ox.fract(), oy.fract());
                        if !(0.02..=0.98).contains(&fx) || !(0.02..=0.98).contains(&fy) {
                            edge += 1; // on a texel's edge: either side
                            continue;
                        }
                        let v = layer[oy as usize * ov.w + ox as usize];
                        if v.is_nan() {
                            continue;
                        }
                        // between the colormap's two nearest colours (its texture is sampled bilinearly)
                        let k = ((v - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0;
                        let (a, c) = (lut[k.floor() as usize], lut[k.ceil() as usize]);
                        let p = &img[(sy * w + sx) * 4..][..3];
                        let ok = (0..3).all(|i| {
                            let (l, u) = (a[i].min(c[i]) as i32 - 3, a[i].max(c[i]) as i32 + 3);
                            (l..=u).contains(&i32::from(p[i]))
                        });
                        if !ok {
                            bad += 1;
                            if bad <= 3 {
                                println!("    ({sx}, {sy}): F's overview {v} -> {a:?}..{c:?}, map {p:?}");
                            }
                        }
                        checked += 1;
                    }
                }
                println!(
                    "    F alone (overview through the grid): {} pixels match PROJ's texel, {bad} wrong ({edge} on \
                     texel edges skipped)",
                    checked - bad
                );
                if bad > 0 || checked < 2000 {
                    return fail("F's overview should be drawn where PROJ puts it");
                }
                next(st, "F's overview through the warp grid where PROJ puts it");
                // A (B + 0.5 over the same grid lines) added under them, B active again
                let a = self.opts.extra_layers.get(1)?.clone();
                self.open_inputs(vec![a], true, ctx);
            }
            3 => {
                if self.opening.is_some() || self.layers.len() != 3 || !complete(self.layer_session(2)?) {
                    return None;
                }
                self.swap_layers(2, 1); // A in the middle, F on top
                self.swap_layers(1, 0); // A at the bottom
                self.set_active(1, ctx); // B
                // the cursor (the pin is there too) and an ROI, on every visible layer
                self.set_chart_layers(1);
                self.set_hover(Some((50, 100)));
                self.rest_hover();
                self.set_roi_rect([13, 21, 163, 191]);
                next(st, "A added; B active, cursor, pin and ROI on every layer");
            }
            4 => {
                let sr = &self.series;
                let (a, f) = (&self.layers[0], &self.layers[2]);
                let ready = |h: &Option<crate::series::SeriesView>| h.as_ref().is_some_and(|h| h.exact);
                let pins_ready = |p: &[crate::series::SeriesView]| p.first().is_some_and(|p| p.exact);
                if !ready(&sr.hover)
                    || !ready(&f.series.hover)
                    || !pins_ready(&sr.pins)
                    || !pins_ready(&f.series.pins)
                    || !sr.roi.complete()
                    || !a.series.roi.complete()
                {
                    return None;
                }
                let Some((r, _)) = &f.place.reproj else { return fail("F lost its reprojection") };
                let (ex, ey) = r.to_layer(50.5, 100.5)?;
                let (fh, bh) = (f.series.hover.as_ref()?, sr.hover.as_ref()?);
                println!(
                    "    cursor B (50, 100) -> F ({}, {}) (PROJ: {ex:.3}, {ey:.3}): {} in both",
                    fh.x, fh.y, fh.values[0]
                );
                if fh.x != ex as usize || fh.y != ey as usize {
                    return fail("F's cursor pixel should be PROJ's");
                }
                if fh.values != bh.values {
                    return fail("F's series under the cursor should be B's");
                }
                if f.series.pins[0].values != sr.pins[0].values {
                    return fail("F's series at the pin should be B's");
                }
                let (bd, ad) = (sr.roi.data.as_ref()?, a.series.roi.data.as_ref()?);
                let mut d_a = 0f32;
                for t in 0..bd.per_t.len() {
                    let (nb, na) = (bd.per_t[t].get()?.n, ad.per_t[t].get()?.n);
                    if na != nb {
                        return fail("A's ROI should have B's pixels (same grid lines)");
                    }
                    d_a = d_a.max((a.series.roi.mean[t] - sr.roi.mean[t] - 0.5).abs());
                }
                println!(
                    "    ROI 150 x 170 px of B: {} px on B and on A (|A - B - 0.5| {d_a:.2e})",
                    bd.per_t[0].get()?.n
                );
                if d_a > 1e-4 {
                    return fail("A's ROI should be B's + 0.5");
                }
                next(st, "F's cursor and pin series equal B's; the ROI on A and B agrees");
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                st.stage = 5;
                return Some(0);
            }
            _ => {}
        }
        None
    }
}
