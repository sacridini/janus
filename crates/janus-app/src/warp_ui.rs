//! Tools > Reproject, resample, clip... (IDEIAS phase 43): the active series' files (or a run's map) through
//! gdalwarp (janus_zeit::warp: its code, so the command's result bit for bit) to another CRS, pixel size or another
//! layer's grid, on the visible area, the ROI or cut by a vector layer's polygons; COGs, GeoTIFFs or virtual
//! (warped VRTs), in a folder that opens as a new series when done (a task).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::warp::{Format, Grid, RESAMPLING, WarpOptions, warp_files};

use crate::app::State;
use crate::contents::Sel;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const TARGETS: [&str; 4] = ["Keep the CRS", "A CRS", "The CRS of a layer", "The grid of a layer"];
const AREAS: [&str; 4] = ["Whole layer", "Visible area", "ROI", "Polygons of a vector layer"];
const TYPES: [&str; 8] = ["as it is", "Byte", "Int16", "UInt16", "Int32", "UInt32", "Float32", "Float64"];
const FORMATS: [(&str, &str); 3] = [
    ("COG", "Cloud-optimized GeoTIFFs (DEFLATE, overviews)"),
    ("GeoTIFF", "Tiled GeoTIFFs (DEFLATE)"),
    ("Virtual", "Warped VRTs: nothing written, ready at once, the pixels computed as they are read"),
];

pub struct WarpUi {
    pub show: bool,
    /// a run's map (its id), else the active series
    pub source: Option<u32>,
    pub target: usize,
    pub srs: String,
    /// the layer whose CRS or grid is the target
    pub layer: Option<Sel>,
    pub pixel_on: bool,
    pub pixel: f64,
    pub tap: bool,
    /// an index of RESAMPLING; None: mode for classes, else near
    pub resampling: Option<usize>,
    pub area: usize,
    pub vector: Option<u32>,
    pub nodata_on: bool,
    pub nodata: f64,
    pub data_type: usize,
    pub format: usize,
    pub name: String,
    pub status: String,
}

impl Default for WarpUi {
    fn default() -> Self {
        WarpUi {
            show: false,
            source: None,
            target: 1,
            srs: "EPSG:4326".into(),
            layer: None,
            pixel_on: false,
            pixel: 30.0,
            tap: false,
            resampling: None,
            area: 0,
            vector: None,
            nodata_on: false,
            nodata: 0.0,
            data_type: 0,
            format: 0,
            name: String::new(),
            status: String::new(),
        }
    }
}

impl State {
    /// A layer's grid (a series' or a run's map's).
    fn layer_grid(&self, sel: Sel) -> Option<Grid> {
        match sel {
            Sel::Series(id) => {
                let i = (0..self.layers.len()).find(|&i| self.layer_session(i).is_some_and(|s| s.info.id == id))?;
                let info = &self.layer_session(i)?.info;
                Some(Grid { wkt: info.crs_wkt.clone(), gt: info.geo_transform?, w: info.width, h: info.height })
            }
            Sel::Result(id) => {
                let r = self.zeit.results.iter().find(|r| r.id == id)?;
                let ds = gdal::Dataset::open(&r.path).ok()?;
                let (w, h) = ds.raster_size();
                Some(Grid { wkt: ds.projection(), gt: ds.geo_transform().ok()?, w, h })
            }
            Sel::Vector(_) => None,
        }
    }

    /// The layers whose CRS or grid can be the target.
    fn grid_layers(&self) -> Vec<(Sel, String)> {
        let mut v: Vec<(Sel, String)> = (0..self.layers.len())
            .filter_map(|i| Some((Sel::Series(self.layer_session(i)?.info.id), self.layers[i].name.clone())))
            .collect();
        v.extend(self.zeit.results.iter().filter(|r| !r.preview).map(|r| (Sel::Result(r.id), r.name.clone())));
        v
    }

    /// Whether the source holds classes.
    fn warp_categorical(&self) -> bool {
        match self.warp.source {
            Some(id) => self.zeit.results.iter().find(|r| r.id == id).is_some_and(|r| !r.classes.is_empty()),
            None => self.classes.on() || self.session.as_ref().is_some_and(|s| s.info.file_categorical),
        }
    }

    pub(crate) fn warp_window(&mut self, ctx: &egui::Context) {
        if !self.warp.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Reproject, resample, clip", ctx, 480.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.warp_ui(ui)));
        self.warp.show = open;
    }

    fn warp_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.weak("Open a series first.");
            return;
        };
        let series = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let maps: Vec<(u32, String)> =
            self.zeit.results.iter().filter(|r| !r.preview).map(|r| (r.id, r.name.clone())).collect();
        if self.warp.source.is_some_and(|id| !maps.iter().any(|m| m.0 == id)) {
            self.warp.source = None;
        }
        let layers = self.grid_layers();
        let vectors: Vec<(u32, String)> = self.vectors.iter().map(|v| (v.id, v.name.clone())).collect();
        let categorical = self.warp_categorical();
        let roi = self.series.roi_rect.is_some();
        let w = &mut self.warp;
        egui::Grid::new("warp grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Layer");
            let shown = w
                .source
                .and_then(|id| maps.iter().find(|m| m.0 == id))
                .map_or(format!("{series} ({} files)", files_of(&info).len()), |m| m.1.clone());
            egui::ComboBox::from_id_salt("warp source").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut w.source, None, format!("{series} ({} files)", files_of(&info).len()));
                for (id, n) in &maps {
                    ui.selectable_value(&mut w.source, Some(*id), n);
                }
            });
            ui.end_row();
            ui.label("To");
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("warp target").selected_text(TARGETS[w.target]).show_ui(ui, |ui| {
                    for (k, t) in TARGETS.iter().enumerate() {
                        ui.selectable_value(&mut w.target, k, *t);
                    }
                });
                match w.target {
                    1 => {
                        ui.add(egui::TextEdit::singleline(&mut w.srs).hint_text("EPSG:4326").desired_width(220.0))
                            .on_hover_text("Any CRS GDAL reads: EPSG:31983, ESRI:102033, a PROJ string, a WKT");
                    }
                    2 | 3 => {
                        let shown = w
                            .layer
                            .and_then(|s| layers.iter().find(|l| l.0 == s))
                            .map_or("choose a layer".into(), |l| l.1.clone());
                        egui::ComboBox::from_id_salt("warp layer").selected_text(shown).width(240.0).show_ui(
                            ui,
                            |ui| {
                                for (s, n) in &layers {
                                    ui.selectable_value(&mut w.layer, Some(*s), n);
                                }
                            },
                        );
                        if w.target == 3 {
                            ui.weak("its CRS, origin, pixel size and extent: the maps aligned pixel to pixel");
                        }
                    }
                    _ => {}
                }
            });
            ui.end_row();
            if w.target != 3 {
                ui.label("Pixel size");
                ui.horizontal(|ui| {
                    ui.checkbox(&mut w.pixel_on, "");
                    ui.add_enabled(w.pixel_on, egui::DragValue::new(&mut w.pixel).range(1e-9..=1e7).speed(0.5))
                        .on_hover_text("In the target CRS's units (metres, or degrees in longitudes and latitudes)");
                    ui.add_enabled(w.pixel_on, egui::Checkbox::new(&mut w.tap, "aligned"))
                        .on_hover_text("The pixels' edges on multiples of the size (-tap)");
                    if !w.pixel_on {
                        ui.weak("as gdalwarp chooses");
                    }
                });
                ui.end_row();
            }
            ui.label("Resampling");
            let r = w.resampling.unwrap_or(if categorical { 4 } else { 0 });
            egui::ComboBox::from_id_salt("warp resampling")
                .selected_text(RESAMPLING[r])
                .show_ui(ui, |ui| {
                    for (k, m) in RESAMPLING.iter().enumerate() {
                        if ui.selectable_label(k == r, *m).clicked() {
                            w.resampling = Some(k);
                        }
                    }
                })
                .response
                .on_hover_text("mode by default for classes (the most frequent class), near otherwise");
            ui.end_row();
            ui.label("Area");
            ui.vertical(|ui| {
                egui::ComboBox::from_id_salt("warp area").selected_text(AREAS[w.area]).show_ui(ui, |ui| {
                    for (k, a) in AREAS.iter().enumerate() {
                        ui.add_enabled_ui(k != 2 || roi, |ui| ui.selectable_value(&mut w.area, k, *a));
                    }
                });
                if w.area == 3 {
                    let shown = w
                        .vector
                        .and_then(|id| vectors.iter().find(|v| v.0 == id))
                        .map_or("choose one".into(), |v| v.1.clone());
                    egui::ComboBox::from_id_salt("warp vector").selected_text(shown).width(240.0).show_ui(ui, |ui| {
                        for (id, n) in &vectors {
                            ui.selectable_value(&mut w.vector, Some(*id), n);
                        }
                    });
                    ui.weak("the pixels outside its polygons without a value, the result cropped to them");
                }
                if w.target == 3 && (w.area == 1 || w.area == 2) {
                    ui.weak("the layer's grid sets the extent");
                }
            });
            ui.end_row();
            ui.label("No data");
            ui.horizontal(|ui| {
                ui.checkbox(&mut w.nodata_on, "");
                ui.add_enabled(w.nodata_on, egui::DragValue::new(&mut w.nodata).speed(1.0));
                if !w.nodata_on {
                    ui.weak("the source's");
                }
            });
            ui.end_row();
            ui.label("Type");
            egui::ComboBox::from_id_salt("warp type").selected_text(TYPES[w.data_type]).show_ui(ui, |ui| {
                for (k, t) in TYPES.iter().enumerate() {
                    ui.selectable_value(&mut w.data_type, k, *t);
                }
            });
            ui.end_row();
            ui.label("Format");
            ui.horizontal(|ui| {
                for (k, (f, h)) in FORMATS.iter().enumerate() {
                    ui.radio_value(&mut w.format, k, *f).on_hover_text(*h);
                }
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut w.name).hint_text(format!("{series} warped")).desired_width(220.0));
            ui.end_row();
        });
        match self.warp_options() {
            Ok(o) => {
                if let Ok(a) = o.args() {
                    ui.weak(format!("gdalwarp {}", a.join(" ")).replace(&info.crs_wkt, "<the layer's CRS>"));
                }
                ui.add_space(4.0);
                if ui.add(egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
                    self.warp_run(ui.ctx());
                }
            }
            Err(e) => {
                ui.colored_label(crate::theme::warning(), e);
            }
        }
        if !self.warp.status.is_empty() {
            ui.weak(&self.warp.status);
        }
    }

    /// The window's options for gdalwarp; Err: what is missing.
    pub(crate) fn warp_options(&self) -> Result<WarpOptions, String> {
        let w = &self.warp;
        let info = &self.session.as_ref().ok_or("open a series")?.info;
        let mut o = WarpOptions {
            resampling: RESAMPLING[w.resampling.unwrap_or(if self.warp_categorical() { 4 } else { 0 })].into(),
            dst_nodata: w.nodata_on.then_some(w.nodata),
            data_type: (w.data_type > 0).then(|| TYPES[w.data_type].to_string()),
            format: [Format::Cog, Format::GTiff, Format::Vrt][w.format.min(2)],
            threads: self.processing_threads(),
            ..Default::default()
        };
        match w.target {
            1 => {
                if w.srs.trim().is_empty() {
                    return Err("Give the CRS (EPSG:4326, ...)".into());
                }
                gdal::spatial_ref::SpatialRef::from_definition(w.srs.trim())
                    .map_err(|_| format!("{}: not a CRS GDAL knows", w.srs.trim()))?;
                o.t_srs = Some(w.srs.trim().into());
            }
            2 | 3 => {
                let g = w.layer.and_then(|s| self.layer_grid(s)).ok_or("Choose the layer")?;
                if g.wkt.is_empty() {
                    return Err("That layer has no CRS".into());
                }
                if w.target == 3 {
                    o.grid = Some(g);
                } else {
                    o.t_srs = Some(g.wkt);
                }
            }
            _ => {}
        }
        if w.pixel_on && w.target != 3 {
            o.pixel = Some((w.pixel, w.pixel));
            o.tap = w.tap;
        }
        if (w.area == 1 || w.area == 2) && w.target != 3 {
            let gt = info.geo_transform.ok_or("The series has no georeference")?;
            if w.area == 2 && self.series.roi_rect.is_none() {
                return Err("Draw an ROI first".into());
            }
            let ([x0, y0, x1, y1], _) = self.run_window(w.area);
            let at = |x: usize, y: usize| {
                (gt[0] + x as f64 * gt[1] + y as f64 * gt[2], gt[3] + x as f64 * gt[4] + y as f64 * gt[5])
            };
            let pts = [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1)];
            let (xs, ys): (Vec<f64>, Vec<f64>) = pts.iter().copied().unzip();
            let f = |v: &[f64], m: fn(f64, f64) -> f64, s: f64| v.iter().fold(s, |a, &b| m(a, b));
            o.extent = Some((
                [
                    f(&xs, f64::min, f64::MAX),
                    f(&ys, f64::min, f64::MAX),
                    f(&xs, f64::max, f64::MIN),
                    f(&ys, f64::max, f64::MIN),
                ],
                info.crs_wkt.clone(),
            ));
        }
        if w.area == 3 {
            let v =
                w.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)).ok_or("Choose the vector layer")?;
            o.cutline = Some((v.data.path.clone(), v.data.layer.clone()));
        }
        Ok(o)
    }

    pub(crate) fn warp_run(&mut self, ctx: &egui::Context) {
        let opts = match self.warp_options() {
            Ok(o) => o,
            Err(e) => {
                self.warp.status = e;
                return;
            }
        };
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let series = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let (files, what) = match self.warp.source.and_then(|id| self.zeit.results.iter().find(|r| r.id == id)) {
            Some(r) => (vec![r.path.clone()], r.name.clone()),
            None => (files_of(&info), series.clone()),
        };
        let name =
            if self.warp.name.trim().is_empty() { format!("{what} warped") } else { self.warp.name.trim().into() };
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("warp").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.warp.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let args = opts.args().unwrap_or_default().join(" ");
        let title = format!("Warp {what}");
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Reproject, resample, clip: {name}\n{}\n{} file(s), first {}\ngdalwarp {args} <file> <output>\noutput: {}\n",
            crate::zeit::timestamp_text(),
            files.len(),
            files.first().map_or("", |f| f.as_str()),
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("warp", "warp", "Reproject, resample, clip");
            if self.warp.source.is_none() {
                let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
                p.series = Some(crate::provenance::SeriesRecord::of(&info, &series, &inputs));
            }
            p.threads = Some(opts.threads as i64);
            p.scope = AREAS[self.warp.area].to_lowercase();
            p.params.insert("gdalwarp".into(), serde_json::json!(args));
            p.params.insert("files".into(), serde_json::json!(files));
            p.python_note = format!("gdalwarp {args} <file> <output>, file by file");
            self.begin_provenance(&folder, p);
        }
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, out) = (status.clone(), cancel.clone(), ctx.clone(), folder.clone());
        std::thread::Builder::new()
            .name("warp".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = warp_files(&files, &out, &opts, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(written) => {
                        log.line(&format!("{} file(s) written, {:.1} s", written.len(), s.seconds));
                        log.line("done");
                        (s.state, s.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        (s.state, s.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        let folder_s = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "warp".into(),
            cube: 0,
            output_dir: folder_s.clone(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.emb_downloads.push(EmbDownload {
            title,
            status,
            started: Instant::now(),
            folder: folder_s,
            name: Some(name.clone()),
            keep_active: true,
            opened: false,
            ended: None,
        });
        self.warp.status = format!("\"{name}\" under way: it opens as a series when ready ({})", folder.display());
    }
}

/// A series' files, once each, in date order.
fn files_of(info: &janus_cube::CubeInfo) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for l in &info.layers {
        if !v.contains(&l.path) {
            v.push(l.path.clone());
        }
    }
    v
}
