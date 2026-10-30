//! Zonal statistics over time (IDEIAS phase 45): Zonal statistics over time... in a vector layer's menu puts each of
//! its features (a zone) through every date of the active series or a run's map (janus_zeit::zonal): the values'
//! statistics by date, the area of each class by date and the transitions between two dates, or a change map's area
//! by year. A task writing a long and a wide CSV and a GeoPackage of the features with the columns added (opened as
//! a vector layer); Analysis > Zonal draws a zone's series (the attribute table's pick followed).

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, Ui, vec2};
use egui_plot::{Bar, BarChart, Legend, Line, Plot, PlotPoints, Polygon};
use janus_zeit::areas::AreaSource;
use janus_zeit::warp::Grid;
use janus_zeit::zonal::{STATS, ZonalKind, ZonalOptions, ZonalTable, write_gpkg, zonal};

use crate::app::State;
use crate::contents::Sel;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const KINDS: [(ZonalKind, &str); 3] = [
    (ZonalKind::Continuous, "values: their statistics"),
    (ZonalKind::Classes, "classes: their areas"),
    (ZonalKind::Years, "change dates: the area by year"),
];

/// Colours of values without a class colour (Tableau 10).
const PALETTE: [[u8; 3]; 10] = [
    [31, 119, 180],
    [255, 127, 14],
    [44, 160, 44],
    [214, 39, 40],
    [148, 103, 189],
    [140, 86, 75],
    [227, 119, 194],
    [127, 127, 127],
    [188, 189, 34],
    [23, 190, 207],
];

/// At most this many columns in the GeoPackage (GDAL's limit is 2000).
const MAX_COLUMNS: usize = 1990;

/// What a table was made of: its title, the vector layer, each zone's name, the values' names and colours.
#[derive(Clone, Default)]
pub struct ZonalMeta {
    pub title: String,
    pub vector: u32,
    pub names: HashMap<i64, String>,
    pub colors: HashMap<i64, [u8; 3]>,
    pub folder: String,
    pub seconds: f64,
}

impl ZonalMeta {
    fn name(&self, v: i64) -> String {
        self.names.get(&v).cloned().unwrap_or_else(|| v.to_string())
    }

    fn color(&self, k: usize, v: i64) -> Color32 {
        let c = self.colors.get(&v).copied().unwrap_or(PALETTE[k % PALETTE.len()]);
        Color32::from_rgb(c[0], c[1], c[2])
    }
}

type Pending = (Arc<Mutex<JobStatus>>, Arc<Mutex<Option<ZonalTable>>>, ZonalMeta);
/// The maps, their grid, the names and colours of their values, the title and the window.
type Sources = (Vec<AreaSource>, Grid, ZonalMeta, Option<[usize; 4]>);

#[derive(Default)]
pub struct ZonalUi {
    pub show: bool,
    /// the vector layer (its id)
    pub vector: Option<u32>,
    /// a run's map (its id), else the active series
    pub source: Option<u32>,
    pub kind: ZonalKind,
    /// what the kind was guessed for (the map, the series)
    guessed: Option<(Option<u32>, u64)>,
    /// the zones' names: an attribute (empty: the FID)
    pub id_field: String,
    pub all_touched: bool,
    pub roi: bool,
    /// classes: the transitions between dates from and to
    pub transitions: bool,
    pub from: usize,
    pub to: usize,
    /// the statistic of the wide table and the GeoPackage (STATS)
    pub stat: usize,
    pub name: String,
    pub status: String,
    pending: Option<Pending>,
    pub table: Option<(Arc<ZonalTable>, ZonalMeta)>,
    /// the zone drawn, and the attribute table's pick it followed
    pub zone: usize,
    seen_pick: Option<(u32, usize)>,
    /// GeoPackages to open when their task is done
    gpkgs: Vec<(Arc<Mutex<JobStatus>>, String)>,
    /// the threads its runs use, set in its window (None: the Settings')
    pub threads: Option<usize>,
}

impl State {
    pub(crate) fn open_zonal(&mut self, vector: u32) {
        if self.zonal.vector != Some(vector) {
            self.zonal.id_field.clear();
            self.zonal.name.clear();
        }
        if self.zonal.stat == 0 {
            self.zonal.stat = 1;
        }
        self.zonal.vector = Some(vector);
        self.zonal.show = true;
    }

    pub(crate) fn zonal_window(&mut self, ctx: &egui::Context) {
        self.pump_zonal(ctx);
        if !self.zonal.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Zonal statistics over time", ctx, 460.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.zonal_form(ui)));
        self.zonal.show = open;
    }

    /// The kind of the map chosen: a map of years, classes, else values.
    pub(crate) fn zonal_guess(&self) -> ZonalKind {
        match self.zonal.source.and_then(|id| self.zeit.results.iter().find(|r| r.id == id)) {
            Some(r) if r.unit == "year" || r.unit == "years" => ZonalKind::Years,
            Some(r) if !r.classes.is_empty() => ZonalKind::Classes,
            Some(_) => ZonalKind::Continuous,
            None if self.classes.on() || self.session.as_ref().is_some_and(|s| s.info.file_categorical) => {
                ZonalKind::Classes
            }
            None => ZonalKind::Continuous,
        }
    }

    /// The maps to go through; Err: why not.
    fn zonal_sources(&self) -> Result<Sources, String> {
        let roi = if self.zonal.roi { self.series.roi_rect } else { None };
        if let Some(id) = self.zonal.source {
            let r = self.zeit.results.iter().find(|r| r.id == id).ok_or("the map was closed")?;
            let grid = self.layer_grid(Sel::Result(id)).ok_or("the map has no georeference")?;
            let mut meta = ZonalMeta { title: r.name.clone(), ..Default::default() };
            for (k, n) in r.classes.iter().enumerate() {
                meta.names.insert(k as i64 + 1, n.clone());
            }
            // the ROI (the series' pixels) in the map's pixels
            let window = match roi {
                Some([a, b, c, d]) => {
                    let (x0, y0) = (a.max(r.x0) - r.x0, b.max(r.y0) - r.y0);
                    let (x1, y1) = (c.min(r.x0 + r.w).saturating_sub(r.x0), d.min(r.y0 + r.h).saturating_sub(r.y0));
                    if x1 <= x0 || y1 <= y0 {
                        return Err("the ROI is off the map".into());
                    }
                    Some([x0, y0, x1, y1])
                }
                None => None,
            };
            return Ok((vec![AreaSource { path: r.path.clone(), band: 1, label: r.name.clone() }], grid, meta, window));
        }
        let s = self.session.as_ref().ok_or("open a series")?;
        let info = &s.info;
        if info.multidim || info.remote {
            return Err("a series read over the network or from a NetCDF / Zarr variable: not read".into());
        }
        let grid = Grid {
            wkt: info.crs_wkt.clone(),
            gt: info.geo_transform.ok_or("the series has no georeference")?,
            w: info.width,
            h: info.height,
        };
        let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let mut meta = ZonalMeta { title: name, ..Default::default() };
        for (v, n) in &info.class_names {
            meta.names.insert(i64::from(*v), n.clone());
        }
        for e in &self.classes.list {
            meta.names.insert(i64::from(e.value), e.name.clone());
            meta.colors.insert(i64::from(e.value), e.color);
        }
        // a stack's bands are different variables: the one shown
        let dates: Vec<usize> =
            if info.stack { vec![self.display.t.min(info.t() - 1)] } else { (0..info.t()).collect() };
        let sources = dates
            .iter()
            .map(|&t| {
                let l = &info.layers[t];
                AreaSource { path: l.path.clone(), band: l.band.max(1) as usize, label: l.label.clone() }
            })
            .collect();
        Ok((sources, grid, meta, roi))
    }

    fn zonal_form(&mut self, ui: &mut Ui) {
        let Some(v) = self.zonal.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)) else {
            ui.weak("Choose Zonal statistics over time... in a vector layer's menu.");
            return;
        };
        let (vname, fields, features) = (v.name.clone(), v.data.fields.clone(), v.data.features.len());
        let maps: Vec<(u32, String)> =
            self.zeit.results.iter().filter(|r| !r.preview).map(|r| (r.id, r.name.clone())).collect();
        if self.zonal.source.is_some_and(|id| !maps.iter().any(|m| m.0 == id)) {
            self.zonal.source = None;
        }
        let series_id = self.session.as_ref().map_or(0, |s| s.info.id);
        if self.zonal.guessed != Some((self.zonal.source, series_id)) {
            self.zonal.kind = self.zonal_guess();
            self.zonal.guessed = Some((self.zonal.source, series_id));
        }
        let labels: Vec<String> = match self.zonal.source {
            Some(_) => vec![],
            None => self.session.as_ref().map_or(vec![], |s| s.info.layers.iter().map(|l| l.label.clone()).collect()),
        };
        let series = self.layers.get(self.active).map_or("the active series".into(), |l| l.name.clone());
        ui.label(format!("{vname}: {features} features, each a zone"));
        let has_roi = self.series.roi_rect.is_some();
        let z = &mut self.zonal;
        if !z.id_field.is_empty() && !fields.contains(&z.id_field) {
            z.id_field.clear();
        }
        egui::Grid::new("zonal grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Map");
            let shown = z
                .source
                .and_then(|id| maps.iter().find(|m| m.0 == id))
                .map_or(format!("{series} (each date)"), |m| m.1.clone());
            egui::ComboBox::from_id_salt("zonal source").selected_text(shown).width(280.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut z.source, None, format!("{series} (each date)"));
                for (id, n) in &maps {
                    ui.selectable_value(&mut z.source, Some(*id), n);
                }
            });
            ui.end_row();
            ui.label("What");
            ui.vertical(|ui| {
                for (k, n) in KINDS {
                    ui.radio_value(&mut z.kind, k, n);
                }
            });
            ui.end_row();
            match z.kind {
                ZonalKind::Continuous => {
                    ui.label("Columns");
                    egui::ComboBox::from_id_salt("zonal stat")
                        .selected_text(STATS[z.stat])
                        .show_ui(ui, |ui| {
                            for (k, s) in STATS.iter().enumerate() {
                                ui.selectable_value(&mut z.stat, k, *s);
                            }
                        })
                        .response
                        .on_hover_text("The statistic of the wide table and of the polygons' columns, one per date");
                    ui.end_row();
                }
                ZonalKind::Classes if labels.len() > 1 => {
                    ui.label("Transitions");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut z.transitions, "from");
                        z.from = z.from.min(labels.len() - 1);
                        z.to = if z.to == 0 { labels.len() - 1 } else { z.to.min(labels.len() - 1) };
                        for (salt, d) in [("zonal from", &mut z.from), ("zonal to", &mut z.to)] {
                            egui::ComboBox::from_id_salt(salt).selected_text(&labels[*d]).show_ui(ui, |ui| {
                                for (k, l) in labels.iter().enumerate() {
                                    ui.selectable_value(d, k, l);
                                }
                            });
                        }
                    });
                    ui.end_row();
                }
                _ => {}
            }
            ui.label("Zones named by");
            let shown = if z.id_field.is_empty() { "the FID".to_string() } else { z.id_field.clone() };
            egui::ComboBox::from_id_salt("zonal id").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut z.id_field, String::new(), "the FID");
                for f in &fields {
                    ui.selectable_value(&mut z.id_field, f.clone(), f);
                }
            });
            ui.end_row();
            ui.label("Pixels");
            ui.horizontal(|ui| {
                ui.radio_value(&mut z.all_touched, false, "their centre inside");
                ui.radio_value(&mut z.all_touched, true, "every one touched");
            });
            ui.end_row();
            ui.label("Where");
            ui.horizontal(|ui| {
                ui.radio_value(&mut z.roi, false, "every feature");
                ui.add_enabled_ui(has_roi, |ui| {
                    ui.radio_value(&mut z.roi, true, "within the ROI");
                });
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut z.name).hint_text(format!("{vname} zonal")).desired_width(220.0));
            ui.end_row();
        });
        ui.weak(
            "A pixel in several features counts in each. Writes a long and a wide CSV and the features with a \
             column per date (a GeoPackage, opened as a layer); Analysis > Zonal draws a feature's series.",
        );
        let mut own = self.zonal.threads;
        self.threads_ui(ui, &mut own);
        self.zonal.threads = own;
        ui.add_space(4.0);
        let busy = self.zonal.pending.is_some();
        if ui.add_enabled(!busy, egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.zonal_run(ui.ctx());
        }
        if !self.zonal.status.is_empty() {
            ui.weak(&self.zonal.status);
        }
    }

    /// A run done: its table shown in Analysis > Zonal, its GeoPackage opened.
    pub(crate) fn pump_zonal(&mut self, ctx: &egui::Context) {
        let mut open = vec![];
        self.zonal.gpkgs.retain(|(st, path)| match st.lock().unwrap().state {
            JobState::Running => true,
            JobState::Done => {
                if std::path::Path::new(path).exists() {
                    open.push(path.clone());
                }
                false
            }
            _ => false,
        });
        if !open.is_empty() {
            self.add_vector_files(open, ctx);
        }
        let Some((st, out, _)) = &self.zonal.pending else { return };
        let (state, seconds, error) = {
            let s = st.lock().unwrap();
            (s.state, s.seconds, s.error.clone())
        };
        match state {
            JobState::Running => {}
            JobState::Done => {
                let table = out.lock().unwrap().take();
                let (_, _, mut meta) = self.zonal.pending.take().unwrap();
                meta.seconds = seconds;
                if let Some(t) = table {
                    self.zonal.status = format!("Done in {seconds:.1} s: Analysis > Zonal draws a feature's series.");
                    self.zonal.zone = (0..t.pixels.len()).find(|&z| t.pixels[z] > 0).unwrap_or(0);
                    self.zonal.seen_pick = None;
                    self.zonal.table = Some((Arc::new(t), meta));
                    self.analysis.show = true;
                    self.analysis.tab = 5;
                }
            }
            JobState::Failed => {
                self.zonal.status = format!("Failed: {error}");
                self.zonal.pending = None;
            }
            _ => {
                self.zonal.status = "Cancelled".into();
                self.zonal.pending = None;
            }
        }
    }

    pub(crate) fn zonal_run(&mut self, ctx: &egui::Context) {
        let Some(v) = self.zonal.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)) else { return };
        let (path, layer, vname, vid) = (v.data.path.clone(), v.data.layer.clone(), v.name.clone(), v.id);
        let (sources, grid, mut meta, window) = match self.zonal_sources() {
            Ok(x) => x,
            Err(e) => {
                self.zonal.status = e;
                return;
            }
        };
        let z = &self.zonal;
        let name = if z.name.trim().is_empty() { format!("{vname} zonal") } else { z.name.trim().into() };
        meta.title = format!("{vname} on {}", meta.title);
        meta.vector = vid;
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("zonal").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.zonal.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let transitions = (z.kind == ZonalKind::Classes && z.transitions && sources.len() > 1)
            .then(|| (z.from.min(sources.len() - 1), z.to.min(sources.len() - 1)));
        let opts = ZonalOptions {
            layer: layer.clone(),
            id_field: z.id_field.clone(),
            all_touched: z.all_touched,
            kind: z.kind,
            window,
            transitions,
            threads: self.zonal.threads.unwrap_or(self.processing_threads()),
        };
        let stat = z.stat;
        let gpkg = folder.join(format!("{stem}.gpkg"));
        let title = format!("Zonal statistics of {vname}");
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Zonal statistics: {name}\n{}\nvector: {path} ({layer})\n{} map(s), first {}\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            sources.len(),
            sources[0].path,
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("zonal", "zonal", "Zonal statistics over time");
            p.scope = if window.is_some() { "the features within the ROI".into() } else { "every feature".into() };
            p.window = window;
            p.threads = Some(opts.threads as i64);
            for (k, v) in [
                ("vector", serde_json::json!(path)),
                ("layer", serde_json::json!(layer)),
                ("kind", serde_json::json!(format!("{:?}", opts.kind))),
                ("all_touched", serde_json::json!(opts.all_touched)),
                ("transitions", serde_json::json!(opts.transitions)),
                ("maps", serde_json::json!(sources.iter().map(|s| &s.path).collect::<Vec<_>>())),
            ] {
                p.params.insert(k.into(), v);
            }
            p.python_note = "rasterstats.zonal_stats(vector, map, stats=..., categorical=...) per date".into();
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
        let out = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, o, cn, c, dir, g) =
            (status.clone(), out.clone(), cancel.clone(), ctx.clone(), folder.clone(), gpkg.clone());
        let names = meta.names.clone();
        let (vpath, vlayer) = (path.clone(), layer.clone());
        std::thread::Builder::new()
            .name("zonal".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, msg: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, msg.to_string());
                    }
                    c.request_repaint();
                };
                let name_of = |v: i64| names.get(&v).cloned().unwrap_or_else(|| v.to_string());
                let r = zonal(&vpath, &grid, &sources, &opts, &progress, &cn).and_then(|t| {
                    let w = |file: &str, text: String| {
                        std::fs::write(dir.join(file), text).map_err(|e| format!("{file}: {e}"))
                    };
                    w("zonal_long.csv", t.long_csv(&name_of))?;
                    let (cols, rows) = t.wide(stat, &name_of);
                    let mut wide = format!("id,fid,pixels,{}\n", cols.join(","));
                    for (z, row) in rows.iter().enumerate() {
                        let cells: Vec<String> =
                            row.iter().map(|x| if x.is_nan() { String::new() } else { format!("{x}") }).collect();
                        wide += &format!(
                            "\"{}\",{},{},{}\n",
                            t.ids[z].replace('"', "'"),
                            t.fids[z],
                            t.pixels[z],
                            cells.join(",")
                        );
                    }
                    w("zonal_wide.csv", wide)?;
                    if !t.transitions.is_empty() {
                        w("transitions.csv", t.transitions_csv(&name_of))?;
                    }
                    if cols.len() <= MAX_COLUMNS {
                        progress(0.99, "writing the GeoPackage");
                        write_gpkg(&vpath, &vlayer, &g, &cols, &rows)?;
                    } else {
                        log.line(&format!("{} columns: no GeoPackage (at most {MAX_COLUMNS})", cols.len()));
                    }
                    Ok(t)
                });
                let seconds = t0.elapsed().as_secs_f64();
                let mut s = st.lock().unwrap();
                s.seconds = seconds;
                match r {
                    Ok(t) => {
                        log.line(&format!(
                            "{} zones ({} with pixels), {} map(s) of the window {:?}, {} pixels in more than one zone{}; \
                             {seconds:.1} s",
                            t.fids.len(),
                            t.pixels.iter().filter(|&&p| p > 0).count(),
                            t.labels.len(),
                            t.window,
                            t.shared,
                            if t.note.is_empty() { String::new() } else { format!("; {}", t.note) }
                        ));
                        log.line(&format!("written {}", dir.display()));
                        log.line("done");
                        *o.lock().unwrap() = Some(t);
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
            tool: "zonal".into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.zonal.gpkgs.push((status.clone(), gpkg.to_string_lossy().into_owned()));
        self.zonal.status = format!("\"{name}\" under way (Tools > Tasks)");
        self.zonal.pending = Some((status, out, ZonalMeta { folder: folder.to_string_lossy().into_owned(), ..meta }));
    }

    /// Analysis > Zonal: the last run's zone drawn over time.
    pub(crate) fn zonal_tab(&mut self, ui: &mut Ui) {
        self.pump_zonal(&ui.ctx().clone());
        let Some((t, meta)) = self.zonal.table.clone() else {
            ui.weak(
                "Choose Zonal statistics over time... in a vector layer's menu: each feature's statistics, or the \
                 area of each class, at every date of the series.",
            );
            return;
        };
        let zones = t.fids.len();
        if zones == 0 {
            ui.weak("No feature.");
            return;
        }
        // the attribute table's pick followed
        let at = &self.attr_table;
        if at.layer == meta.vector
            && let Some(p) = at.picked
            && self.zonal.seen_pick != Some((at.layer, p))
        {
            self.zonal.seen_pick = Some((at.layer, p));
            let fid =
                self.vectors.iter().find(|v| v.id == meta.vector).and_then(|v| v.data.features.get(p)).map(|f| f.fid);
            if let Some(z) = fid.and_then(|f| t.fids.iter().position(|&x| x == f)) {
                self.zonal.zone = z;
            }
        }
        let z = self.zonal.zone.min(zones - 1);
        let id = |z: usize| t.ids[z].clone();
        ui.horizontal(|ui| {
            ui.strong(&meta.title);
            ui.separator();
            ui.label("Feature");
            let mut pick = z;
            egui::ComboBox::from_id_salt("zonal zone").selected_text(id(z)).width(180.0).show_ui(ui, |ui| {
                for k in 0..zones {
                    ui.selectable_value(&mut pick, k, format!("{} ({} px)", id(k), t.pixels[k]));
                }
            });
            self.zonal.zone = pick;
            ui.weak(format!("{} pixels", t.pixels[z]));
        });
        ui.weak(format!(
            "{:.1} s; the tables and the GeoPackage in {}.{}",
            meta.seconds,
            meta.folder,
            if t.note.is_empty() { String::new() } else { format!(" Pixel areas: {}.", t.note) }
        ));
        let ticks = t.labels.clone();
        let fmt = move |m: egui_plot::GridMark, _: &std::ops::RangeInclusive<f64>| {
            let i = m.value.round();
            if (m.value - i).abs() < 1e-6 && i >= 0.0 {
                ticks.get(i as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        };
        let h = (ui.available_height() - 8.0).clamp(140.0, 360.0);
        let dates = t.labels.len();
        match t.kind {
            ZonalKind::Continuous => {
                let at = |s: usize, k: usize| t.stats[s][z][k];
                let pts = |k: usize| -> Vec<[f64; 2]> {
                    (0..dates).filter(|&s| !at(s, k).is_nan()).map(|s| [s as f64, at(s, k)]).collect()
                };
                Plot::new("zonal values")
                    .height(h)
                    .legend(Legend::default().position(egui_plot::Corner::RightTop).background_alpha(0.6))
                    .x_axis_formatter(fmt)
                    .allow_scroll(false)
                    .show(ui, |pu| {
                        let blue = Color32::from_rgb(31, 119, 180);
                        // a band a quad per two dates with values (egui fills a polygon as convex)
                        for (lo, hi, name, a) in [(4, 8, "p10 to p90", 40), (5, 7, "p25 to p75", 70)] {
                            let s: Vec<usize> =
                                (0..dates).filter(|&s| !at(s, lo).is_nan() && !at(s, hi).is_nan()).collect();
                            for p in s.windows(2) {
                                let (x0, x1) = (p[0] as f64, p[1] as f64);
                                let quad = vec![
                                    [x0, at(p[0], lo)],
                                    [x1, at(p[1], lo)],
                                    [x1, at(p[1], hi)],
                                    [x0, at(p[0], hi)],
                                ];
                                pu.polygon(
                                    Polygon::new(name, PlotPoints::from(quad))
                                        .fill_color(Color32::from_rgba_unmultiplied(31, 119, 180, a))
                                        .stroke(egui::Stroke::NONE),
                                );
                            }
                        }
                        pu.line(Line::new("mean", PlotPoints::from(pts(1))).color(blue).width(2.0));
                        pu.line(
                            Line::new("median", PlotPoints::from(pts(6)))
                                .color(Color32::from_rgb(255, 127, 14))
                                .style(egui_plot::LineStyle::dashed_dense()),
                        );
                    });
            }
            ZonalKind::Classes | ZonalKind::Years => {
                // the zone's counts by map and value (ha)
                let ha = |s: usize, v: i64| -> f64 {
                    t.counts[s].iter().filter(|c| c.0 as usize == z && c.1 == v).map(|c| c.3 / 1e4).sum()
                };
                let values: Vec<i64> =
                    t.values.iter().copied().filter(|&v| (0..dates).any(|s| ha(s, v) > 0.0)).collect();
                if dates == 1 {
                    let ticks: Vec<String> = values.iter().map(|&v| meta.name(v)).collect();
                    Plot::new("zonal one")
                        .height(h)
                        .y_axis_label("ha")
                        .x_axis_formatter(move |m, _| {
                            let i = m.value.round();
                            if (m.value - i).abs() < 1e-6 && i >= 0.0 {
                                ticks.get(i as usize).cloned().unwrap_or_default()
                            } else {
                                String::new()
                            }
                        })
                        .allow_scroll(false)
                        .show(ui, |pu| {
                            let bars: Vec<Bar> = values
                                .iter()
                                .enumerate()
                                .map(|(k, &v)| {
                                    Bar::new(k as f64, ha(0, v)).width(0.8).fill(meta.color(k, v)).name(meta.name(v))
                                })
                                .collect();
                            pu.bar_chart(BarChart::new("area", bars));
                        });
                } else {
                    Plot::new("zonal dates")
                        .height(h)
                        .y_axis_label("ha")
                        .legend(Legend::default().position(egui_plot::Corner::RightTop).background_alpha(0.6))
                        .x_axis_formatter(fmt)
                        .allow_scroll(false)
                        .show(ui, |pu| {
                            let mut below: Vec<BarChart> = vec![];
                            for (k, &v) in values.iter().enumerate() {
                                let c = meta.color(k, v);
                                let bars: Vec<Bar> =
                                    (0..dates).map(|s| Bar::new(s as f64, ha(s, v)).width(0.8).fill(c)).collect();
                                let refs: Vec<&BarChart> = below.iter().collect();
                                below.push(BarChart::new(meta.name(v), bars).color(c).stack_on(&refs));
                            }
                            for c in below {
                                pu.bar_chart(c);
                            }
                        });
                }
            }
        }
    }
}
