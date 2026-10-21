//! The basemap in the interface (src/app_basemap.cpp): its section of the Layers panel (source, on/off, opacity, a
//! custom XYZ URL), the Basemap's life (made after the first frames, only once a source is picked and a series is
//! open; a source change retires the old one in the background, since a request may still be running), drawing it
//! first in the map's target and its attribution on the map. Each session and each series opened start without
//! it, drawn only when asked for.

use std::sync::Arc;

use egui::{Color32, FontId, Pos2, Rect, Ui, vec2};
use janus_geo::basemap::{self, Basemap, BasemapSource};
use janus_gpu::{Gpu, GpuTexture, MapFrame, Warp};
use janus_session::ViewRect;

use crate::app::State;

/// The setting: the source picked and how it is drawn.
pub struct BasemapUi {
    /// "none", a preset's id or "custom"
    pub source: String,
    pub on: bool,
    pub opacity: f32,
    /// the custom source
    pub url: String,
    pub max_zoom: i32,
    pub tile_size: i32,
    pub attribution: String,
    /// the fields being edited, applied when left
    url_edit: String,
    attr_edit: String,
}

impl Default for BasemapUi {
    fn default() -> Self {
        BasemapUi {
            source: "none".into(),
            on: false,
            opacity: 1.0,
            url: String::new(),
            max_zoom: 19,
            tile_size: 256,
            attribution: String::new(),
            url_edit: String::new(),
            attr_edit: String::new(),
        }
    }
}

#[derive(Default)]
pub struct BasemapState {
    pub ui: BasemapUi,
    pub b: Option<Basemap<GpuTexture>>,
    /// why it is not drawn
    pub why: String,
    /// the layer it could not be placed on (not tried again for it)
    failed_for: u64,
}

/// "Tiles: host" for a custom source without an attribution of its own.
fn host_of(url: &str) -> &str {
    let Some(a) = url.find("://") else { return url };
    let rest = &url[a + 3..];
    &rest[..rest.find(['/', '?']).unwrap_or(rest.len())]
}

impl BasemapUi {
    pub fn source(&self) -> BasemapSource {
        if let Some(p) = basemap::presets().into_iter().find(|p| p.id == self.source) {
            return p;
        }
        BasemapSource {
            id: "custom".into(),
            name: "Custom XYZ".into(),
            url: self.url.clone(),
            max_zoom: self.max_zoom,
            tile_size: self.tile_size,
            attribution: if self.attribution.is_empty() {
                format!("Tiles: {}", host_of(&self.url))
            } else {
                self.attribution.clone()
            },
            terms: "The provider's terms (a key in the URL is sent with every request)".into(),
            connections: 4,
            cache_days: 7,
        }
    }
}

impl BasemapState {
    pub fn shown(&self) -> bool {
        self.ui.on && self.ui.source != "none" && self.b.as_ref().is_some_and(|b| b.has_map())
    }

    /// The tiles of the view under the layers (`view`: in the active layer's source pixels, at target pixels per
    /// source pixel; `to_px`: a map-space box to the target's pixels), faded towards `bg` by the opacity.
    pub fn draw(
        &mut self,
        gpu: &Gpu,
        f: &mut MapFrame,
        view: &ViewRect,
        bg: [f32; 4],
        to_px: impl Fn(&[f64; 4]) -> [f32; 4],
    ) {
        if !self.shown() {
            return;
        }
        let b = self.b.as_mut().unwrap();
        let v = [view.x0, view.y0, view.x1, view.y1];
        b.update(&v, view.scale);
        let mut any = false;
        b.for_each_visible(&v, view.scale, |tex, grid, p| {
            let warp = Warp { grid: grid.clone(), quad: p.warp_quad, src: p.warp_src };
            gpu.draw_image(f, tex, to_px(&p.quad), 1.0, Some(&warp), false);
            any = true;
        });
        // opacity: the tiles (of several levels, overlapping) drawn opaque, then faded towards the background, so
        // each place is blended once
        if any && self.ui.opacity < 1.0 {
            let [w, h] = f.size;
            gpu.fill_rect(f, [0.0, 0.0, w as f32, h as f32], [bg[0], bg[1], bg[2], 1.0 - self.ui.opacity]);
        }
    }

    /// The old Basemap's GPU resources go now; the rest (its pool, which may wait for a request) in a thread.
    fn retire(&mut self) -> bool {
        let Some(mut b) = self.b.take() else { return false };
        b.release_gpu();
        std::thread::spawn(move || drop(b));
        true
    }
}

impl State {
    /// S: satellite imagery under the layers, or hidden again. The source picked is kept if it is imagery (Esri,
    /// EOX, custom); none or OpenStreetMap become Esri.
    pub fn toggle_satellite(&mut self) {
        let bm = &mut self.basemap;
        let imagery = bm.ui.source != "none" && bm.ui.source != "osm";
        if bm.ui.on && imagery {
            bm.ui.on = false;
        } else {
            if !imagery {
                bm.ui.source = "esri".into();
            }
            bm.ui.on = true;
            bm.failed_for = 0;
        }
        self.map.dirty = true;
    }

    /// Once per frame. Nothing is made before the third frame (the window shows first), nor without a series.
    pub(crate) fn pump_basemap(&mut self, ctx: &egui::Context) {
        let bm = &mut self.basemap;
        if bm.ui.source == "none" {
            if bm.retire() {
                self.map.dirty = true;
            }
            bm.why.clear();
            return;
        }
        let (Some(s), Some(gpu)) = (&self.session, &self.gpu) else { return };
        if !bm.ui.on || self.frame < 3 {
            return; // off: nothing asked for, the tiles kept
        }
        if bm.ui.source == "custom"
            && let Some(why) = basemap::url_problem(&bm.ui.url)
        {
            if bm.retire() {
                self.map.dirty = true;
            }
            bm.why = why.into();
            return;
        }
        let src = bm.ui.source();
        if bm.b.as_ref().is_some_and(|b| !b.source().same_tiles(&src)) {
            bm.retire();
        }
        if bm.b.is_none() {
            let dir = janus_cache::overview::default_cache_dir().join("basemap");
            let c = ctx.clone();
            bm.b = Some(Basemap::new(&src, &dir, Arc::new(move || c.request_repaint())));
            bm.failed_for = 0;
            self.map.dirty = true;
        }
        let b = bm.b.as_mut().unwrap();
        if bm.failed_for != s.info.id {
            let had = b.has_map();
            match b.set_map(&s.info, |w, h, rg| gpu.create_warp_grid(w as u32, h as u32, rg)) {
                Ok(()) => {
                    bm.why.clear();
                    if !had {
                        self.map.dirty = true;
                    }
                }
                Err(why) => {
                    bm.why = why;
                    bm.failed_for = s.info.id; // not tried again for this layer
                }
            }
        }
        b.tick();
        if b.upload_ready(16, |rgba, side| gpu.create_image(side, side, rgba)) {
            self.map.dirty = true;
        }
    }

    /// Bottom right of the map: the attribution, always, and a short state (offline, a server error); wrapped to
    /// the map's width.
    pub(crate) fn basemap_note(&self, painter: &egui::Painter, rect: Rect) {
        if !self.basemap.shown() {
            return;
        }
        let b = self.basemap.b.as_ref().unwrap();
        let mut text = b.source().attribution.clone();
        if b.offline() {
            text = format!("Basemap unavailable (offline)  |  {text}");
        } else {
            let e = b.error();
            if !e.is_empty() {
                text = format!("Basemap: {e}  |  {text}");
            }
        }
        let wrap = (rect.width() - 260.0).max(80.0); // the colour bar is bottom left
        let galley = painter.layout(text, FontId::proportional(11.0), Color32::from_gray(220), wrap);
        let pad = 3.0;
        let at = rect.max - galley.size() - vec2(pad + 2.0, pad + 2.0);
        painter.rect_filled(
            Rect::from_min_size(at - vec2(pad, pad), galley.size() + vec2(2.0 * pad, 2.0 * pad)),
            2.0,
            Color32::from_black_alpha(140),
        );
        painter.galley(Pos2::new(at.x, at.y), galley, Color32::from_gray(220));
    }

    /// The Layers panel's section, below the layers (it is drawn under all of them).
    pub(crate) fn basemap_ui(&mut self, ui: &mut Ui) {
        ui.separator();
        ui.strong("Basemap");
        let presets = basemap::presets();
        let bm = &mut self.basemap;
        let current = match bm.ui.source.as_str() {
            "none" => "None".to_string(),
            "custom" => "Custom XYZ URL".to_string(),
            id => presets.iter().find(|p| p.id == id).map_or("None".into(), |p| p.name.clone()),
        };
        let mut dirty = false;
        ui.horizontal(|ui| {
            ui.add_enabled_ui(bm.ui.source != "none", |ui| {
                dirty |= ui
                    .checkbox(&mut bm.ui.on, "")
                    .on_hover_text(if bm.ui.on { "Hide the basemap" } else { "Show the basemap" })
                    .changed();
            });
            egui::ComboBox::from_id_salt("basemap source").selected_text(current).width(ui.available_width()).show_ui(
                ui,
                |ui| {
                    let mut pick = None;
                    if ui
                        .selectable_label(bm.ui.source == "none", "None")
                        .on_hover_text("Nothing under the layers; nothing is downloaded")
                        .clicked()
                    {
                        pick = Some("none".to_string());
                    }
                    for p in &presets {
                        if ui
                            .selectable_label(bm.ui.source == p.id, &p.name)
                            .on_hover_text(format!("{}\nUp to zoom {}. {}", p.attribution, p.max_zoom, p.terms))
                            .clicked()
                        {
                            pick = Some(p.id.clone());
                        }
                    }
                    if ui
                        .selectable_label(bm.ui.source == "custom", "Custom XYZ URL")
                        .on_hover_text(
                            "Your own tile service, e.g. with a key (Google Map Tiles API, MapTiler,\n\
                             Planet...): https://.../{z}/{x}/{y}.png?key=...",
                        )
                        .clicked()
                    {
                        pick = Some("custom".to_string());
                        bm.ui.url_edit = bm.ui.url.clone();
                        bm.ui.attr_edit = bm.ui.attribution.clone();
                    }
                    if let Some(id) = pick {
                        bm.ui.source = id;
                        bm.ui.on = true;
                        bm.failed_for = 0;
                        dirty = true;
                    }
                },
            );
        });
        if bm.ui.source == "none" {
            ui.weak("Satellite imagery or a map under the layers.");
            if dirty {
                self.map.dirty = true;
            }
            return;
        }
        dirty |= ui.add(egui::Slider::new(&mut bm.ui.opacity, 0.05..=1.0).text("opacity").fixed_decimals(2)).changed();
        if bm.ui.source == "custom" {
            // applied when the field is left or Enter is pressed (not at each key)
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut bm.ui.url_edit)
                        .hint_text("https://.../{z}/{x}/{y}.png")
                        .desired_width(f32::INFINITY),
                )
                .on_hover_text(
                    "XYZ tile URL: {z} zoom, {x} column, {y} row from the top ({-y}: from the\n\
                     bottom, TMS); {s} becomes 'a'. Keys go in the URL. Tiles in Web Mercator\n\
                     (EPSG:3857). Applied when you leave the field.",
                );
            if r.lost_focus() && bm.ui.url != bm.ui.url_edit {
                bm.ui.url = bm.ui.url_edit.clone();
                bm.failed_for = 0;
            }
            let r = ui
                .add(
                    egui::TextEdit::singleline(&mut bm.ui.attr_edit)
                        .hint_text("Attribution (shown on the map)")
                        .desired_width(f32::INFINITY),
                )
                .on_hover_text("The provider's required credit, drawn on the map and in exports");
            if r.lost_focus() {
                bm.ui.attribution = bm.ui.attr_edit.clone();
            }
            ui.horizontal(|ui| {
                ui.add(egui::Slider::new(&mut bm.ui.max_zoom, 0..=22).text("max zoom"))
                    .on_hover_text("Finest zoom level the service has (finer views enlarge its tiles)");
                egui::ComboBox::from_id_salt("tile size")
                    .selected_text(format!("{} px tiles", bm.ui.tile_size))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut bm.ui.tile_size, 256, "256 px tiles");
                        ui.selectable_value(&mut bm.ui.tile_size, 512, "512 px tiles");
                    });
            });
        }
        // state: why it is not drawn, offline, a server error, loading
        let warn = Color32::from_rgb(255, 179, 102);
        if self.session.is_none() {
            ui.weak("Drawn under a georeferenced series once one is open.");
        } else if !bm.why.is_empty() {
            ui.colored_label(warn, format!("Not drawn: {}", bm.why));
        } else if let Some(b) = bm.b.as_ref().filter(|_| bm.ui.on) {
            let e = b.error();
            if b.offline() {
                ui.weak("Basemap unavailable (offline)");
            } else if !e.is_empty() {
                ui.colored_label(warn, format!("Server: {e}"));
            } else if b.inflight() > 0 {
                let n = b.inflight();
                ui.weak(format!("Loading {n} tile{}...", if n > 1 { "s" } else { "" }));
            }
        }
        if bm.ui.source != "custom" || basemap::url_problem(&bm.ui.url).is_none() {
            let src = bm.ui.source();
            ui.weak(&src.attribution).on_hover_text(&src.terms);
        }
        if dirty {
            self.map.dirty = true;
        }
    }

    /// The Performance panel's section.
    pub(crate) fn basemap_perf_ui(&self, ui: &mut Ui) {
        let Some(b) = &self.basemap.b else { return };
        ui.separator();
        ui.strong("Basemap");
        ui.label(format!(
            "{}: {} tiles on GPU ({:.0} MB), {} loading",
            b.source().name,
            b.gpu_tiles(),
            b.gpu_bytes() as f64 / 1048576.0,
            b.inflight()
        ));
        ui.label(format!("Average tile: {:.1} ms ({} read)", b.avg_load_ms(), b.loads()));
        if let Some(ms) = b.first_tile_ms() {
            ui.label(format!("First tile: {ms:.0} ms after placing the map"));
        }
        if let Some(r) = b.map() {
            ui.label(format!(
                "Warp grid: {} x {} ({} px, {:.0} ms)",
                r.grid_w,
                r.grid_h,
                janus_cube::multidim::format_g(r.grid_error, 2),
                r.build_ms
            ));
        }
    }
}
