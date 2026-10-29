//! Polygonize... (IDEIAS phase 41): a run's map or a series' date shown (a class or change map) as polygons in a
//! GeoPackage, each with its value, class, date, area in hectares, perimeter and the mean of another map of the same
//! grid; a task (Tools -> Tasks) whose GeoPackage opens as a vector layer when done.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::polygonize::{PolyOptions, polygonize};

use crate::app::State;
use crate::contents::Sel;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

/// The map polygonized: its file, band, name, class names, whether its values are years, its grid's size.
struct Source {
    path: String,
    band: usize,
    name: String,
    classes: Vec<(i64, String)>,
    years: bool,
    size: (usize, usize),
}

#[derive(Default)]
pub struct PolyUi {
    pub show: bool,
    pub source: Option<Sel>,
    pub eight: bool,
    pub min_area_ha: f64,
    pub simplify_m: f64,
    /// the map whose mean each polygon gets (a run's map of the same size)
    pub mean_of: Option<u32>,
    pub name: String,
    pub status: String,
    /// runs under way: their status and GeoPackage, opened when done
    pending: Vec<(Arc<Mutex<JobStatus>>, String)>,
}

impl State {
    fn poly_source(&self, sel: Sel) -> Option<Source> {
        match sel {
            Sel::Result(id) => {
                let r = self.zeit.results.iter().find(|r| r.id == id)?;
                Some(Source {
                    path: r.path.clone(),
                    band: 1,
                    name: r.name.clone(),
                    classes: r.classes.iter().enumerate().map(|(k, n)| (k as i64 + 1, n.clone())).collect(),
                    years: r.unit == "year" || r.unit == "years",
                    size: (r.w, r.h),
                })
            }
            Sel::Series(id) => {
                // the active layer's session is the state's
                let i = if self.session.as_ref().is_some_and(|s| s.info.id == id) {
                    self.active
                } else {
                    self.layers.iter().position(|l| l.session.as_ref().is_some_and(|s| s.info.id == id))?
                };
                let (info, t) = if i == self.active {
                    (self.session.as_ref().map(|s| s.info.clone())?, self.display.t)
                } else {
                    (self.layers[i].session.as_ref().map(|s| s.info.clone())?, self.layers[i].disp.t)
                };
                let l = info.layers.get(t.min(info.t().saturating_sub(1)))?;
                if info.multidim || info.remote {
                    return None;
                }
                let name = if info.t() > 1 {
                    format!("{} [{}]", self.layers[i].name, l.label)
                } else {
                    self.layers[i].name.clone()
                };
                Some(Source {
                    path: l.path.clone(),
                    band: l.band.max(1) as usize,
                    name,
                    classes: info.class_names.iter().map(|(k, n)| (*k as i64, n.clone())).collect(),
                    years: false,
                    size: (info.width, info.height),
                })
            }
            Sel::Vector(_) => None,
        }
    }

    /// Opens the window on a layer's map.
    pub(crate) fn open_polygonize(&mut self, sel: Sel) {
        if self.poly.source != Some(sel) {
            self.poly.mean_of = None;
            self.poly.name.clear();
        }
        self.poly.source = Some(sel);
        self.poly.show = true;
    }

    pub(crate) fn polygonize_window(&mut self, ctx: &egui::Context) {
        self.pump_polygonize(ctx);
        if !self.poly.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Polygonize", ctx, 440.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.polygonize_ui(ui)));
        self.poly.show = open;
    }

    /// A run whose GeoPackage is not opened yet.
    pub(crate) fn polygonize_pending(&self) -> bool {
        !self.poly.pending.is_empty()
    }

    /// The GeoPackages of the runs done opened as vector layers.
    fn pump_polygonize(&mut self, ctx: &egui::Context) {
        let mut open = vec![];
        self.poly.pending.retain(|(st, path)| match st.lock().unwrap().state {
            JobState::Running => true,
            JobState::Done => {
                open.push(path.clone());
                false
            }
            _ => false,
        });
        if !open.is_empty() {
            self.add_vector_files(open, ctx);
        }
    }

    fn polygonize_ui(&mut self, ui: &mut Ui) {
        let Some(src) = self.poly.source.and_then(|s| self.poly_source(s)) else {
            ui.weak("Choose Polygonize... in the menu of a run's map or of a series in the Layers panel.");
            return;
        };
        let same: Vec<(u32, String)> = self
            .zeit
            .results
            .iter()
            .filter(|r| (r.w, r.h) == src.size && r.path != src.path && !r.preview)
            .map(|r| (r.id, r.name.clone()))
            .collect();
        let p = &mut self.poly;
        ui.label(format!("{}: {} x {} px", src.name, src.size.0, src.size.1));
        if !src.classes.is_empty() {
            ui.weak(format!("{} classes named", src.classes.len()));
        }
        egui::Grid::new("poly grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Neighbours");
            ui.horizontal(|ui| {
                ui.radio_value(&mut p.eight, false, "4 (sides)");
                ui.radio_value(&mut p.eight, true, "8 (and corners)");
            });
            ui.end_row();
            ui.label("Minimum area");
            ui.add(egui::DragValue::new(&mut p.min_area_ha).range(0.0..=1e6).speed(0.1).suffix(" ha")).on_hover_text(
                "Patches smaller than this merged into the neighbour they share the longest border with \
                 (Zeit's minimum mapping unit), or left out without one (0: every patch)",
            );
            ui.end_row();
            ui.label("Simplify");
            ui.add(egui::DragValue::new(&mut p.simplify_m).range(0.0..=1e5).speed(1.0).suffix(" m"))
                .on_hover_text("The outlines simplified to this tolerance, their topology kept (0: the pixels' edges)");
            ui.end_row();
            ui.label("Mean of");
            let shown = p.mean_of.and_then(|id| same.iter().find(|s| s.0 == id)).map_or("none".into(), |s| s.1.clone());
            ui.add_enabled_ui(!same.is_empty(), |ui| {
                egui::ComboBox::from_id_salt("poly mean").selected_text(shown).width(220.0).show_ui(ui, |ui| {
                    ui.selectable_value(&mut p.mean_of, None, "none");
                    for (id, n) in &same {
                        ui.selectable_value(&mut p.mean_of, Some(*id), n);
                    }
                });
            })
            .response
            .on_hover_text("Each polygon gets the mean of this map's values in it (a map of the same grid)");
            ui.end_row();
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut p.name)
                    .hint_text(format!("{} polygons", src.name))
                    .desired_width(220.0),
            );
            ui.end_row();
        });
        ui.weak(format!(
            "Columns: value{}{}, area_ha (geodesic), perimeter_m, pixels{}; a GeoPackage opened as a vector layer",
            if src.classes.is_empty() { "" } else { ", class" },
            if src.years { ", date" } else { "" },
            if self.poly.mean_of.is_some() { ", mean_..." } else { "" }
        ));
        ui.add_space(4.0);
        if ui.add(egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            let _ = self.polygonize_run(src, ui.ctx());
        }
        if !self.poly.status.is_empty() {
            ui.weak(&self.poly.status);
        }
    }

    /// Runs on the window's map; its GeoPackage's path.
    pub(crate) fn polygonize_start(&mut self, ctx: &egui::Context) -> Option<String> {
        let src = self.poly.source.and_then(|s| self.poly_source(s))?;
        self.polygonize_run(src, ctx)
    }

    fn polygonize_run(&mut self, src: Source, ctx: &egui::Context) -> Option<String> {
        let name = if self.poly.name.trim().is_empty() {
            format!("{} polygons", src.name)
        } else {
            self.poly.name.trim().into()
        };
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("polygonize").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return None;
        }
        let gpkg = folder.join(format!("{stem}.gpkg"));
        let mean_of = self
            .poly
            .mean_of
            .and_then(|id| self.zeit.results.iter().find(|r| r.id == id))
            .map(|r| (r.name.clone(), r.path.clone(), 1));
        let opts = PolyOptions {
            band: src.band,
            eight: self.poly.eight,
            min_area_ha: self.poly.min_area_ha,
            simplify_m: self.poly.simplify_m,
            classes: src.classes.clone(),
            years: src.years,
            mean_of,
            layer: stem.clone(),
        };
        let title = format!("Polygonize {}", src.name);
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Polygonize: {name}\n{}\nmap: {} (band {})\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            src.path,
            src.band,
            gpkg.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("polygonize", "polygonize", "Polygonize");
            p.scope = format!("{} (band {})", src.path, src.band);
            for (k, v) in [
                ("connectivity", serde_json::json!(if opts.eight { 8 } else { 4 })),
                ("min_area_ha", serde_json::json!(opts.min_area_ha)),
                ("simplify_m", serde_json::json!(opts.simplify_m)),
                ("mean_of", serde_json::json!(opts.mean_of.as_ref().map(|m| m.1.clone()))),
            ] {
                p.params.insert(k.into(), v);
            }
            p.python_note = "gdal_polygonize / rasterio.features.shapes, then the areas with pyproj's Geod".into();
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
        let (st, cn, c, out, path) = (status.clone(), cancel.clone(), ctx.clone(), gpkg.clone(), src.path.clone());
        std::thread::Builder::new()
            .name("polygonize".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = polygonize(&path, &opts, &out, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(r) => {
                        log.line(&format!(
                            "{} polygons, {:.2} ha{}, {:.1} s",
                            r.polygons,
                            r.area_ha,
                            if r.dropped > 0 {
                                format!(" ({} patches under the minimum area with no neighbour left out)", r.dropped)
                            } else {
                                String::new()
                            },
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
        self.zeit.jobs.push(Job {
            title,
            tool: "polygonize".into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        let gpkg = gpkg.to_string_lossy().into_owned();
        self.poly.pending.push((status, gpkg.clone()));
        self.poly.status = format!("\"{name}\" under way: it opens as a vector layer when ready ({gpkg})");
        Some(gpkg)
    }
}
