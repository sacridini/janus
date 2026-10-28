//! File > Build series... and File > Save series as... (IDEIAS phase 46). Building: loose files (picked, or a
//! folder's) planned by janus_zeit::series_io::plan (their dates read from the names, a date's single-band files
//! stacked as its bands, its tiles mosaicked, one grid), the plan shown, then a folder of VRTs or COGs opened as a
//! series. Saving: the active series (the quantity shown with its mask, or every band) over the whole layer, the
//! visible area or the ROI as a multiband COG, a NetCDF or a Zarr with a time axis, opened back when done.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::series_io::{BuildFormat, BuildPlan, SaveFormat, SaveOptions, build, plan, save_series};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

type Planning = Arc<Mutex<Option<Result<BuildPlan, String>>>>;

#[derive(Default)]
pub struct BuildUi {
    pub show: bool,
    pub files: Vec<String>,
    pub plan: Option<BuildPlan>,
    planning: Option<Planning>,
    pub name: String,
    pub cog: bool,
    pub status: String,
}

#[derive(Default)]
pub struct SaveUi {
    pub show: bool,
    pub format: SaveFormat,
    pub all_bands: bool,
    /// 0 the whole layer, 1 the visible area, 2 the ROI (run_window's scopes)
    pub scope: usize,
    pub path: String,
    pub dont_open: bool,
    pub status: String,
}

impl SaveUi {
    fn ext(&self) -> &'static str {
        match self.format {
            SaveFormat::Cog => "tif",
            SaveFormat::NetCdf => "nc",
            SaveFormat::Zarr => "zarr",
        }
    }
}

/// A new running task's status.
fn running() -> Arc<Mutex<JobStatus>> {
    Arc::new(Mutex::new(JobStatus {
        state: JobState::Running,
        progress: 0.0,
        message: "starting".into(),
        error: String::new(),
        result: None,
        seconds: 0.0,
    }))
}

impl State {
    pub(crate) fn series_windows(&mut self, ctx: &egui::Context) {
        self.pump_build_plan();
        if self.build.show {
            let mut open = true;
            crate::panels::fitted_window("Build series", ctx, 480.0)
                .open(&mut open)
                .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.build_ui(ui)));
            self.build.show = open;
        }
        if self.save.show {
            let mut open = true;
            crate::panels::fitted_window("Save series as", ctx, 440.0)
                .open(&mut open)
                .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.save_ui(ui)));
            self.save.show = open;
        }
    }

    /// The files to build from, planned in the background.
    pub(crate) fn build_plan_start(&mut self, files: Vec<String>) {
        let files = janus_cube::inputs::expand_inputs(&files);
        if self.build.name.trim().is_empty()
            && let Some(dir) = files.first().and_then(|f| std::path::Path::new(f).parent()).and_then(|d| d.file_name())
        {
            self.build.name = dir.to_string_lossy().into_owned();
        }
        self.build.files = files.clone();
        self.build.plan = None;
        let out: Planning = Arc::new(Mutex::new(None));
        let o = out.clone();
        std::thread::Builder::new()
            .name("build plan".into())
            .spawn(move || {
                *o.lock().unwrap() = Some(plan(&files));
            })
            .ok();
        self.build.planning = Some(out);
        self.build.status = format!("reading {} files...", self.build.files.len());
    }

    fn pump_build_plan(&mut self) {
        let Some(p) = &self.build.planning else { return };
        let Some(r) = p.lock().unwrap().take() else { return };
        self.build.planning = None;
        match r {
            Ok(p) => {
                self.build.status.clear();
                self.build.plan = Some(p);
            }
            Err(e) => self.build.status = e,
        }
    }

    fn build_ui(&mut self, ui: &mut Ui) {
        ui.label(
            "Loose files grouped by the date in their names: a date's files of one band stacked as its bands, \
                  its tiles (files of other extents) mosaicked, every date on one grid.",
        );
        ui.horizontal(|ui| {
            if ui.button("Pick files...").clicked()
                && let Some(f) = rfd::FileDialog::new().pick_files()
            {
                self.build_plan_start(f.iter().map(|p| p.to_string_lossy().into_owned()).collect());
            }
            if ui.button("Pick a folder...").clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.build_plan_start(vec![d.to_string_lossy().into_owned()]);
            }
        });
        if !self.build.status.is_empty() {
            ui.weak(&self.build.status);
        }
        let Some(p) = &self.build.plan else { return };
        ui.separator();
        match &p.grid {
            Some(g) => {
                let tiles = p.dates.iter().flat_map(|d| d.bands.iter().map(Vec::len)).max().unwrap_or(1);
                ui.strong(format!(
                    "{} dates, {} band(s) a date{}: {} x {} px of {} m",
                    p.dates.len(),
                    p.bands.len(),
                    if tiles > 1 { format!(", up to {tiles} tiles mosaicked") } else { String::new() },
                    g.w,
                    g.h,
                    g.gt[1]
                ));
                ui.weak(format!(
                    "Bands: {}{}",
                    p.bands.join(", "),
                    if p.multiband { " (each file's own)" } else { " (a file each)" }
                ));
            }
            None => {
                ui.colored_label(crate::theme::warning(), "No date to build.");
            }
        }
        egui::ScrollArea::vertical().id_salt("build dates").max_height(160.0).show(ui, |ui| {
            egui::Grid::new("build grid").num_columns(2).striped(true).spacing(vec2(14.0, 2.0)).show(ui, |ui| {
                for d in &p.dates {
                    ui.label(&d.label);
                    let files: Vec<String> = d
                        .bands
                        .iter()
                        .map(|f| {
                            let n = std::path::Path::new(&f[0])
                                .file_name()
                                .map_or(String::new(), |n| n.to_string_lossy().into_owned());
                            if f.len() > 1 { format!("{n} (+{} tiles)", f.len() - 1) } else { n }
                        })
                        .collect();
                    ui.weak(files.join(", "));
                    ui.end_row();
                }
            });
        });
        if !p.skipped.is_empty() {
            egui::CollapsingHeader::new(format!("{} file(s) left out", p.skipped.len())).show(ui, |ui| {
                for (f, why) in &p.skipped {
                    let n = std::path::Path::new(f).file_name().map_or(f.clone(), |n| n.to_string_lossy().into_owned());
                    ui.weak(format!("{n}: {why}"));
                }
            });
        }
        let ok = p.grid.is_some();
        ui.separator();
        let b = &mut self.build;
        egui::Grid::new("build options").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("As");
            ui.horizontal(|ui| {
                ui.radio_value(&mut b.cog, false, "VRTs (at once, nothing copied)");
                ui.radio_value(&mut b.cog, true, "COGs");
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut b.name).hint_text("series").desired_width(220.0));
            ui.end_row();
        });
        if ui.add_enabled(ok, egui::Button::new("▶ Build").min_size(vec2(110.0, 0.0))).clicked() {
            self.build_run(ui.ctx());
        }
    }

    /// Builds the plan as a task; its folder opened as a series when done.
    pub(crate) fn build_run(&mut self, ctx: &egui::Context) {
        let Some(p) = self.build.plan.clone() else { return };
        let name = if self.build.name.trim().is_empty() { "series".to_string() } else { self.build.name.trim().into() };
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("series").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.build.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let format = if self.build.cog { BuildFormat::Cog } else { BuildFormat::Vrt };
        let threads = self.processing_threads();
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Build series: {name}\n{}\n{} files: {} dates of {} band(s) ({}), {} left out\nas {format:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            self.build.files.len(),
            p.dates.len(),
            p.bands.len(),
            p.bands.join(", "),
            p.skipped.len(),
            folder.display()
        ));
        for (f, why) in &p.skipped {
            log.line(&format!("left out {f}: {why}"));
        }
        {
            let mut pr = crate::provenance::Provenance::start("build_series", "build_series", "Build series");
            pr.scope = "the files' extent at their finest pixel".into();
            for (k, v) in [
                ("files", serde_json::json!(self.build.files)),
                ("bands", serde_json::json!(p.bands)),
                ("format", serde_json::json!(format!("{format:?}"))),
            ] {
                pr.params.insert(k.into(), v);
            }
            pr.python_note = "gdalbuildvrt -separate -te -tr per date (gdal.BuildVRT)".into();
            self.begin_provenance(&folder, pr);
        }
        let status = running();
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, out) = (status.clone(), cancel.clone(), ctx.clone(), folder.clone());
        std::thread::Builder::new()
            .name("build series".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = build(&p, &out, &stem, format, threads, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(files) => {
                        log.line(&format!("{} dates written, {:.1} s", files.len(), s.seconds));
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
        let title = format!("Build series {name}");
        let folder_s = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "build_series".into(),
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
            keep_active: false,
            sel: None,
            opened: false,
            ended: None,
        });
        self.build.status = format!("\"{name}\" under way: it opens as a series when ready");
    }

    fn save_ui(&mut self, ui: &mut Ui) {
        let Some(s) = self.session.as_ref() else {
            ui.weak("Open a series.");
            return;
        };
        let info = &s.info;
        let shown = info.selection_text();
        let bands = info.bands_per_date;
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let has_roi = self.series.roi_rect.is_some();
        ui.label(format!("{layer}: {} {}", info.t(), info.steps_noun()));
        let sv = &mut self.save;
        egui::Grid::new("save grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Format");
            ui.vertical(|ui| {
                ui.radio_value(&mut sv.format, SaveFormat::Cog, "COG: a band per date (its date in the band's name)");
                ui.radio_value(&mut sv.format, SaveFormat::NetCdf, "NetCDF (CF), with a time axis");
                ui.radio_value(&mut sv.format, SaveFormat::Zarr, "Zarr, with a time axis");
            });
            ui.end_row();
            ui.label("Values");
            ui.vertical(|ui| {
                ui.radio_value(&mut sv.all_bands, false, format!("{shown} (as shown, its mask applied)"));
                ui.add_enabled_ui(sv.format != SaveFormat::Cog && bands > 1, |ui| {
                    ui.radio_value(&mut sv.all_bands, true, format!("every band ({bands}), a variable each"));
                });
            });
            ui.end_row();
            ui.label("Area");
            ui.horizontal(|ui| {
                ui.radio_value(&mut sv.scope, 0, "whole layer");
                ui.radio_value(&mut sv.scope, 1, "visible area");
                ui.add_enabled_ui(has_roi, |ui| {
                    ui.radio_value(&mut sv.scope, 2, "ROI");
                });
            });
            ui.end_row();
        });
        if sv.format == SaveFormat::Cog {
            sv.all_bands = false;
        }
        ui.weak("Reprojected or clipped by polygons: Tools > Reproject, resample, clip first.");
        ui.checkbox(&mut sv.dont_open, "Don't open it when saved");
        ui.add_space(4.0);
        if ui.add(egui::Button::new("Save as...").min_size(vec2(110.0, 0.0))).clicked() {
            let ext = sv.ext();
            let file = format!("{}.{ext}", janus_cube::stac_vrt::sanitize(&layer));
            if let Some(p) = rfd::FileDialog::new().add_filter(ext, &[ext]).set_file_name(file).save_file() {
                self.save.path = p.with_extension(ext).to_string_lossy().into_owned();
                self.save_run(ui.ctx());
            }
        }
        if !self.save.status.is_empty() {
            ui.weak(&self.save.status);
        }
    }

    /// Saves the active series to save.path as a task; opened when done.
    pub(crate) fn save_run(&mut self, ctx: &egui::Context) {
        let Some(s) = self.session.as_ref() else { return };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let sv = &self.save;
        let (window, scope) = self.run_window(sv.scope);
        let whole = window == [0, 0, info.width, info.height];
        let opts = SaveOptions {
            format: sv.format,
            window: (!whole).then_some(window),
            all_bands: sv.all_bands,
            name: info.selection_text(),
            threads: self.processing_threads(),
        };
        let out = std::path::PathBuf::from(&sv.path);
        let stem = janus_cube::stac_vrt::sanitize(&layer);
        let folder = self.results_dir().join("save").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.save.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Save series: {layer}\n{}\n{} dates, {scope}\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            info.t(),
            out.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("save_series", "save_series", "Save series as");
            p.series = Some(crate::provenance::SeriesRecord::of(&info, &layer, &inputs));
            p.window = opts.window;
            p.scope = scope.into();
            p.params.insert("format".into(), serde_json::json!(format!("{:?}", opts.format)));
            p.params.insert("all_bands".into(), serde_json::json!(opts.all_bands));
            p.params.insert("output".into(), serde_json::json!(out.to_string_lossy()));
            p.python_note = "xarray: the series as a DataArray (time, y, x), .to_netcdf() / .to_zarr()".into();
            self.begin_provenance(&folder, p);
        }
        let status = running();
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, o) = (status.clone(), cancel.clone(), ctx.clone(), out.clone());
        std::thread::Builder::new()
            .name("save series".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = save_series(&info, &opts, &o, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(r) => {
                        log.line(&format!(
                            "{} dates, variable(s) {}, {:.1} s",
                            r.dates,
                            r.variables.join(", "),
                            s.seconds
                        ));
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
        let title =
            format!("Save {layer} as {}", out.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()));
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "save_series".into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        if !self.save.dont_open {
            self.emb_downloads.push(EmbDownload {
                title,
                status,
                started: Instant::now(),
                folder: out.to_string_lossy().into_owned(),
                name: Some(format!("{layer} saved")),
                keep_active: true,
                sel: None,
                opened: false,
                ended: None,
            });
        }
        self.save.status = format!("saving to {}...", out.display());
    }
}
