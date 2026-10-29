//! Rasterize on the series' grid... (IDEIAS phase 44): a vector layer burned on the active series' grid (its CRS,
//! origin, pixel and size) by janus_zeit::rasterize: 1 inside (a mask), the feature's number or an attribute (a
//! text as classes with their names); the pixels' centres or every pixel touched. A task whose GeoTIFF opens as a
//! layer, usable as a Zeit tool's mask (Only where... under its scope).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::rasterize::{Burn, RasterizeOptions, rasterize};
use janus_zeit::warp::Grid;

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

#[derive(Default)]
pub struct RasterizeUi {
    pub show: bool,
    /// the vector layer (its id)
    pub vector: Option<u32>,
    /// 0 a mask (1 / 0), 1 the feature's number, 2 an attribute
    pub burn: usize,
    pub field: String,
    pub all_touched: bool,
    pub name: String,
    pub status: String,
}

impl State {
    pub(crate) fn open_rasterize(&mut self, vector: u32) {
        if self.raster.vector != Some(vector) {
            self.raster.field.clear();
            self.raster.name.clear();
        }
        self.raster.vector = Some(vector);
        self.raster.show = true;
    }

    pub(crate) fn rasterize_window(&mut self, ctx: &egui::Context) {
        if !self.raster.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Rasterize on the series' grid", ctx, 420.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.rasterize_ui(ui)));
        self.raster.show = open;
    }

    /// The active series' grid.
    fn series_grid(&self) -> Option<Grid> {
        let info = &self.session.as_ref()?.info;
        Some(Grid { wkt: info.crs_wkt.clone(), gt: info.geo_transform?, w: info.width, h: info.height })
    }

    fn rasterize_ui(&mut self, ui: &mut Ui) {
        let Some(v) = self.raster.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)) else {
            ui.weak("Choose Rasterize on the series' grid... in a vector layer's menu.");
            return;
        };
        let (vname, fields, features) = (v.name.clone(), v.data.fields.clone(), v.data.features.len());
        let Some(g) = self.series_grid() else {
            ui.weak("Open a series with a georeference: its grid is the target.");
            return;
        };
        let series = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        ui.label(format!("{vname} ({features} features) on {series}'s grid: {} x {} px", g.w, g.h));
        let r = &mut self.raster;
        if r.burn == 2 && !fields.contains(&r.field) {
            r.field = fields.first().cloned().unwrap_or_default();
        }
        egui::Grid::new("rasterize grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Burn");
            ui.vertical(|ui| {
                ui.radio_value(&mut r.burn, 0, "1 inside, 0 outside (a mask)");
                ui.radio_value(&mut r.burn, 1, "the feature's number (1, 2...)");
                ui.add_enabled_ui(!fields.is_empty(), |ui| {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut r.burn, 2, "an attribute");
                        egui::ComboBox::from_id_salt("rasterize field").selected_text(&r.field).show_ui(ui, |ui| {
                            for f in &fields {
                                ui.selectable_value(&mut r.field, f.clone(), f);
                            }
                        });
                    });
                })
                .response
                .on_hover_text("A number as it is; a text as classes (numbered by name, the names on the layer)");
            });
            ui.end_row();
            ui.label("Pixels");
            ui.horizontal(|ui| {
                ui.radio_value(&mut r.all_touched, false, "their centre inside");
                ui.radio_value(&mut r.all_touched, true, "every one touched");
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut r.name).hint_text(format!("{vname} raster")).desired_width(220.0));
            ui.end_row();
        });
        ui.weak("Opens as a layer; a Zeit tool can then run only where it is not 0 (Only where..., under its scope).");
        ui.add_space(4.0);
        if ui.add(egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.rasterize_run(ui.ctx());
        }
        if !self.raster.status.is_empty() {
            ui.weak(&self.raster.status);
        }
    }

    pub(crate) fn rasterize_run(&mut self, ctx: &egui::Context) {
        let Some(v) = self.raster.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)) else { return };
        let Some(grid) = self.series_grid() else { return };
        let (path, layer, vname) = (v.data.path.clone(), v.data.layer.clone(), v.name.clone());
        let r = &self.raster;
        let burn = match r.burn {
            0 => Burn::One,
            1 => Burn::Index,
            _ => Burn::Attribute(r.field.clone()),
        };
        let name = if r.name.trim().is_empty() { format!("{vname} raster") } else { r.name.trim().into() };
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("rasterize").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.raster.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let opts = RasterizeOptions { layer: layer.clone(), burn, all_touched: r.all_touched, grid };
        let out = folder.join(format!("{stem}.tif"));
        let title = format!("Rasterize {vname}");
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Rasterize: {name}\n{}\nvector: {path} ({layer})\nburn: {:?}, all touched: {}\ngrid: {} x {}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            opts.burn,
            opts.all_touched,
            opts.grid.w,
            opts.grid.h,
            out.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("rasterize", "rasterize", "Rasterize on the series' grid");
            p.scope = "the active series' grid".into();
            for (k, v) in [
                ("vector", serde_json::json!(path)),
                ("layer", serde_json::json!(layer)),
                ("burn", serde_json::json!(format!("{:?}", opts.burn))),
                ("all_touched", serde_json::json!(opts.all_touched)),
            ] {
                p.params.insert(k.into(), v);
            }
            p.python_note = "gdal_rasterize -a <attribute> (or -burn 1) -te -ts on the series' grid".into();
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
        let (st, cn, c) = (status.clone(), cancel.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("rasterize".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = rasterize(&path, &opts, &out, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(r) => {
                        log.line(&format!(
                            "{} of {} features burned ({}{}), {:.1} s",
                            r.burned,
                            r.features,
                            r.kind,
                            if r.classes.is_empty() { String::new() } else { format!(", {} classes", r.classes.len()) },
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
        let folder_s = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "rasterize".into(),
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
            sel: None,
            opened: false,
            ended: None,
        });
        self.raster.status = format!("\"{name}\" under way: it opens as a layer when ready");
    }
}

impl State {
    /// The layers that can be a tool's mask: on the active series' grid (another layer, its date shown).
    pub(crate) fn mask_layers(&self) -> Vec<(u64, String)> {
        let Some(a) = self.session.as_ref().map(|s| &s.info) else { return vec![] };
        (0..self.layers.len())
            .filter(|&i| i != self.active)
            .filter_map(|i| {
                let info = &self.layer_session(i)?.info;
                // within a millionth of a pixel (gdalwarp computes a grid's pixel size from its extent)
                let near = match (info.geo_transform, a.geo_transform) {
                    (Some(g), Some(h)) => {
                        let tol = 1e-6 * h[1].abs().max(h[5].abs());
                        g.iter().zip(&h).all(|(p, q)| (p - q).abs() <= tol)
                    }
                    (g, h) => g == h,
                };
                let same = (info.width, info.height) == (a.width, a.height) && near;
                same.then(|| (info.id, self.layers[i].name.clone()))
            })
            .collect()
    }

    /// A mask layer's file (its date shown, the band as a VRT when not the first) and name.
    pub(crate) fn mask_path(&self, cube: u64) -> Option<(String, String)> {
        let i = (0..self.layers.len())
            .find(|&i| i != self.active && self.layer_session(i).is_some_and(|s| s.info.id == cube))?;
        let info = &self.layer_session(i)?.info;
        let l = info.layers.get(self.layers[i].disp.t.min(info.t() - 1))?;
        let path =
            if l.band <= 1 { l.path.clone() } else { format!("vrt://{}?bands={}", l.path.replace('\\', "/"), l.band) };
        Some((path, self.layers[i].name.clone()))
    }

    /// A tool's Only where...: none, or a layer on the series' grid.
    pub(crate) fn mask_ui(&mut self, ui: &mut Ui, tool: &str) {
        let layers = self.mask_layers();
        let u = self.zeit.tools.entry(tool.to_string()).or_default();
        if u.mask.is_some_and(|m| !layers.iter().any(|l| l.0 == m)) {
            u.mask = None;
        }
        if layers.is_empty() && u.mask.is_none() {
            return;
        }
        let shown = u
            .mask
            .and_then(|m| layers.iter().find(|l| l.0 == m))
            .map_or("everywhere".into(), |l| format!("where {} is not 0", l.1));
        ui.horizontal(|ui| {
            ui.label("Only");
            egui::ComboBox::from_id_salt(("mask", tool)).selected_text(shown).width(ui.available_width()).show_ui(
                ui,
                |ui| {
                    ui.selectable_value(&mut u.mask, None, "everywhere");
                    for (id, n) in &layers {
                        ui.selectable_value(&mut u.mask, Some(*id), format!("where {n} is not 0"));
                    }
                },
            );
        })
        .response
        .on_hover_text(
            "A layer on the series' grid (a vector layer rasterized, a class map): its pixels 0 or without a value \
             left out, without a value in the outputs",
        );
    }
}
