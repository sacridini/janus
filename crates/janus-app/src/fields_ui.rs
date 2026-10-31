//! Tools > Classification > Field delineation: the fields of the active Sentinel-2 series as parcels (janus_ml's
//! fields and fieldnet: the Fields of The World / WRI Trazo network on two median composites of the season, then
//! the parcels found in its probabilities), a task whose class map opens as a run's layers and whose parcels open
//! as a vector layer (a GeoPackage: each parcel's area, perimeter and mean interior probability).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use egui_plot::{Line, Plot, PlotPoints, Polygon};
use janus_cube::CubeInfo;
use janus_ml::fieldnet::{self, CHECKPOINTS, FieldNet};
use janus_ml::fields::{self, FieldBands, Parcels, Season, Tiling};
use janus_ml::fields_train::{
    self as ft, ChipInputs, FieldArch, FieldHeader, FieldModel, FieldNetwork, FieldTrainConfig,
};
use janus_ml::model::shared_device;
use janus_zeit::polygonize::{PolyOptions, polygonize};

use crate::app::State;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

pub struct FieldsUi {
    pub show: bool,
    /// CHECKPOINTS' index, then the field models' (`files`)
    pub model: usize,
    /// the field models trained (the app's field_models folder, and files opened): path, header
    pub files: Vec<(std::path::PathBuf, FieldHeader)>,
    scanned: bool,
    /// a temporal model's year (its months from the year before's); 0: the one the series covers best
    pub year: i32,
    /// training on fields drawn: the vector layer (id) of the fields, the epochs, the new model's name
    pub train_vector: u32,
    pub train_epochs: usize,
    pub train_name: String,
    /// trainings under way: their status and model file, listed when done
    training: Vec<(Arc<Mutex<JobStatus>>, std::path::PathBuf)>,
    /// 0 whole image, 1 visible area, 2 ROI
    pub scope: usize,
    /// the season found from the series' NDVI (else the windows' dates as chosen)
    pub auto: bool,
    /// the windows' half width (days) around the season's start and end
    pub half: i64,
    /// the windows chosen: first and last date of A and of B (indices)
    pub a: (usize, usize),
    pub b: (usize, usize),
    pub method: Parcels,
    /// percent from which an interior pixel seeds the watershed
    pub core: u8,
    pub min_area_ha: f64,
    pub simplify_m: f64,
    pub gpu: bool,
    pub patch: usize,
    pub status: String,
    /// the season last found (series id): its NDVI and windows, shown
    pub found: Option<(u64, fields::SeasonFound)>,
    /// runs under way: their status and GeoPackage, opened when done
    pending: Vec<(Arc<Mutex<JobStatus>>, String)>,
}

impl Default for FieldsUi {
    fn default() -> Self {
        FieldsUi {
            show: false,
            model: 0,
            files: vec![],
            scanned: false,
            year: 0,
            train_vector: 0,
            train_epochs: 30,
            train_name: "my fields".into(),
            training: vec![],
            scope: 1,
            auto: true,
            half: 21,
            a: (0, 0),
            b: (0, 0),
            method: Parcels::Watershed,
            core: 50,
            min_area_ha: 0.05,
            simplify_m: 15.0,
            gpu: true,
            patch: 256,
            status: String::new(),
            found: None,
            pending: vec![],
        }
    }
}

/// A network: a checkpoint of WRI's or FTW's, or a field model trained.
// a few in a list, cloned when chosen: not worth a box
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Choice {
    Checkpoint(&'static fieldnet::Checkpoint),
    File(std::path::PathBuf, FieldHeader),
}

impl Choice {
    fn id(&self) -> String {
        match self {
            Choice::Checkpoint(c) => c.id.into(),
            Choice::File(p, _) => p.file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
        }
    }

    fn source(&self) -> String {
        match self {
            Choice::Checkpoint(c) => c.url.into(),
            Choice::File(p, _) => p.display().to_string(),
        }
    }

    /// The upsampling the network was trained with (the checkpoints': their authors'; fine-tuned: none).
    fn resize(&self) -> usize {
        match self {
            Choice::Checkpoint(c) => c.resize,
            Choice::File(..) => 1,
        }
    }

    fn temporal(&self) -> bool {
        matches!(self, Choice::File(_, h) if matches!(h.arch, FieldArch::Utae { .. }))
    }
}

impl FieldsUi {
    /// The network chosen.
    fn choice(&self) -> Choice {
        match CHECKPOINTS.get(self.model) {
            Some(c) => Choice::Checkpoint(c),
            None => match self.files.get(self.model - CHECKPOINTS.len()) {
                Some((p, h)) => Choice::File(p.clone(), h.clone()),
                None => Choice::Checkpoint(&CHECKPOINTS[0]),
            },
        }
    }
}

/// The years of a series' dates.
fn series_years(info: &CubeInfo) -> Vec<i32> {
    let mut v: Vec<i32> = (0..info.t()).map(|k| janus_cube::dates::civil_from_days(info.unix_day(k)).0).collect();
    v.dedup();
    v
}

/// A temporal model's months for the year (from its first month of the year before).
fn months_of(info: &CubeInfo, choice: &Choice, year: i32) -> Vec<Vec<usize>> {
    match choice {
        Choice::File(_, FieldHeader { arch: FieldArch::Utae { months, first_month, .. }, .. }) => {
            fields::month_dates(info, (year - 1, *first_month), *months)
        }
        _ => vec![],
    }
}

/// The year whose months the series covers best (the latest of equals).
fn best_year(info: &CubeInfo, choice: &Choice) -> i32 {
    let mut years = series_years(info);
    if let Some(&last) = years.last() {
        years.push(last + 1);
    }
    let covered = |y: i32| months_of(info, choice, y).iter().filter(|m| !m.is_empty()).count();
    years.iter().copied().max_by_key(|&y| (covered(y), y)).unwrap_or(2024)
}

fn status_new() -> Arc<Mutex<JobStatus>> {
    Arc::new(Mutex::new(JobStatus {
        state: JobState::Running,
        progress: 0.0,
        message: "starting".into(),
        error: String::new(),
        result: None,
        seconds: 0.0,
    }))
}

/// The folder of the field models trained in Janus.
pub fn models_dir() -> std::path::PathBuf {
    crate::settings::app_data_dir().join("field_models")
}

/// The run's choices, taken to its thread.
struct Spec {
    info: Arc<CubeInfo>,
    window: [usize; 4],
    model: Choice,
    year: i32,
    season: Option<Season>,
    half: i64,
    method: Parcels,
    core: u8,
    min_area_ha: f64,
    simplify_m: f64,
    gpu: bool,
    patch: usize,
    dir: std::path::PathBuf,
    stem: String,
}

impl State {
    pub(crate) fn fields_menu(&mut self, ui: &mut Ui) {
        if ui
            .add(egui::Button::selectable(self.fields.show, "Field delineation (parcels)..."))
            .on_hover_text(
                "The fields of a Sentinel-2 series as parcels: WRI's Trazo or Fields of The World's network\n\
                 on two composites of the season (planting, harvest), the parcels as a vector layer",
            )
            .clicked()
        {
            self.fields.show = !self.fields.show;
            ui.close();
        }
    }

    pub(crate) fn fields_window(&mut self, ctx: &egui::Context) {
        self.pump_fields(ctx);
        if !self.fields.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Field delineation", ctx, 480.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.fields_ui(ui)));
        self.fields.show = open;
    }

    /// A run whose GeoPackage is not opened yet.
    pub(crate) fn fields_pending(&self) -> bool {
        !self.fields.pending.is_empty()
    }

    fn pump_fields(&mut self, ctx: &egui::Context) {
        let mut open = vec![];
        self.fields.pending.retain(|(st, path)| match st.lock().unwrap().state {
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
        // trainings done: their models listed and chosen
        let f = &mut self.fields;
        let mut done = vec![];
        f.training.retain(|(st, path)| match st.lock().unwrap().state {
            JobState::Running => true,
            JobState::Done => {
                done.push(path.clone());
                false
            }
            _ => false,
        });
        for p in done {
            match FieldModel::read_header(&p) {
                Ok(h) => {
                    f.files.retain(|(q, _)| *q != p);
                    f.files.push((p.clone(), h));
                    f.model = CHECKPOINTS.len() + f.files.len() - 1;
                    f.status = format!("trained: {} (chosen as the model)", p.display());
                }
                Err(e) => f.status = e,
            }
        }
        if !f.scanned {
            f.scanned = true;
            if let Ok(rd) = std::fs::read_dir(models_dir()) {
                let mut found: Vec<std::path::PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| p.extension().is_some_and(|x| x == "janusfields"))
                    .collect();
                found.sort();
                for p in found {
                    if let Ok(h) = FieldModel::read_header(&p)
                        && !f.files.iter().any(|(q, _)| *q == p)
                    {
                        f.files.push((p, h));
                    }
                }
            }
        }
    }

    /// A training under way.
    pub(crate) fn fields_training(&self) -> bool {
        !self.fields.training.is_empty()
    }

    fn fields_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.weak("Open a Sentinel-2 series (red, green, blue and NIR bands) first.");
            return;
        };
        let bands = match FieldBands::of(&info) {
            Ok(b) => b,
            Err(e) => {
                ui.colored_label(ui.visuals().warn_fg_color, e);
                return;
            }
        };
        ui.label(format!(
            "Reads {} of the series' {} dates",
            bands.bands.iter().map(|b| b.name.as_str()).collect::<Vec<_>>().join(", "),
            info.t()
        ));
        let t = info.t();
        let label = |k: usize| info.layers.get(k).map_or(String::new(), |l| l.label.clone());
        let f = &mut self.fields;
        if f.a == (0, 0) && f.b == (0, 0) && t > 1 {
            (f.a, f.b) = ((0, (t / 4).min(t - 1)), ((t * 3 / 4).min(t - 1), t - 1));
        }
        egui::Grid::new("fields grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Model");
            let choice = f.choice();
            let title = |k: usize| -> String {
                match CHECKPOINTS.get(k) {
                    Some(c) => c.title.into(),
                    None => f.files.get(k - CHECKPOINTS.len()).map_or(String::new(), |(p, _)| {
                        format!("{} (trained)", p.file_stem().unwrap_or_default().to_string_lossy())
                    }),
                }
            };
            let titles: Vec<String> = (0..CHECKPOINTS.len() + f.files.len()).map(title).collect();
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("fields model")
                    .selected_text(&titles[f.model.min(titles.len() - 1)])
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for (k, t) in titles.iter().enumerate() {
                            let about = match CHECKPOINTS.get(k) {
                                Some(c) => c.about.to_string(),
                                None => f.files[k - CHECKPOINTS.len()].1.about.clone(),
                            };
                            ui.selectable_value(&mut f.model, k, t).on_hover_text(about);
                        }
                    });
                if ui.button("Open...").on_hover_text("A field model file (.janusfields) trained in Janus").clicked()
                    && let Some(p) = rfd::FileDialog::new().add_filter("Janus field model", &["janusfields"]).pick_file()
                {
                    match FieldModel::read_header(&p) {
                        Ok(h) => {
                            f.files.retain(|(q, _)| *q != p);
                            f.files.push((p, h));
                            f.model = CHECKPOINTS.len() + f.files.len() - 1;
                        }
                        Err(e) => f.status = e,
                    }
                }
            });
            ui.end_row();
            ui.label("");
            let about = match &choice {
                Choice::Checkpoint(ck) => format!("{} Downloaded once ({} MB). {}", ck.about, ck.mb, ck.cite),
                Choice::File(p, h) => format!("{} ({})", h.about, p.display()),
            };
            // wrapped to the window's width rather than stretching it
            ui.vertical(|ui| {
                ui.set_max_width(360.0);
                ui.add(egui::Label::new(egui::RichText::new(about).weak().small()).wrap());
            });
            ui.end_row();
            if choice.temporal() {
                ui.label("Year");
                let best = best_year(&info, &choice);
                egui::ComboBox::from_id_salt("fields year")
                    .selected_text(if f.year == 0 { format!("{best} (the best covered)") } else { f.year.to_string() })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut f.year, 0, format!("{best} (the best covered)"));
                        for y in series_years(&info) {
                            ui.selectable_value(&mut f.year, y, y.to_string());
                        }
                    })
                    .response
                    .on_hover_text("The temporal model reads a year's months (from July of the year before): that year's fields");
                ui.end_row();
            }
            ui.label("Area");
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.scope, 1, "visible area");
                ui.radio_value(&mut f.scope, 2, "ROI");
                ui.radio_value(&mut f.scope, 0, "whole image");
            });
            ui.end_row();
            if !choice.temporal() {
            ui.label("Season");
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.auto, true, "found in the series")
                    .on_hover_text("The region's NDVI date by date: windows about its green-up (A) and its fall (B)");
                ui.radio_value(&mut f.auto, false, "chosen");
            });
            ui.end_row();
            if f.auto {
                ui.label("Windows");
                ui.add(egui::DragValue::new(&mut f.half).range(5..=90).prefix("± ").suffix(" days"))
                    .on_hover_text("Each window's dates: this many days either side of the season's start and end");
                ui.end_row();
            } else {
                for (name, w) in [("Window A", &mut f.a), ("Window B", &mut f.b)] {
                    ui.label(name);
                    ui.horizontal(|ui| {
                        for (k, v) in [&mut w.0, &mut w.1].into_iter().enumerate() {
                            egui::ComboBox::from_id_salt(format!("fields {name} {k}"))
                                .selected_text(label(*v))
                                .width(110.0)
                                .show_ui(ui, |ui| {
                                    for d in 0..t {
                                        ui.selectable_value(v, d, label(d));
                                    }
                                });
                            if k == 0 {
                                ui.label("to");
                            }
                        }
                    });
                    ui.end_row();
                }
            }
            }
            ui.label("Parcels");
            ui.horizontal(|ui| {
                ui.radio_value(&mut f.method, Parcels::Watershed, "watershed")
                    .on_hover_text("From each field's interior over the boundary's probability: the boundary pixels go to the fields on either side, so neighbouring parcels touch");
                ui.radio_value(&mut f.method, Parcels::Interiors, "interiors (FTW)")
                    .on_hover_text("ftw-tools' polygonize: each set of connected interior pixels is a parcel");
            });
            ui.end_row();
            if f.method == Parcels::Watershed {
                ui.label("Seeds");
                ui.add(egui::DragValue::new(&mut f.core).range(10..=95).suffix(" %")).on_hover_text(if choice.temporal() {
                    "An interior pixel at least this far to its field's middle (and its four neighbours interior) seeds a parcel"
                } else {
                    "An interior pixel at least this sure (and its four neighbours interior) seeds a parcel"
                });
                ui.end_row();
            }
            ui.label("Minimum area");
            ui.add(egui::DragValue::new(&mut f.min_area_ha).range(0.0..=1000.0).speed(0.01).suffix(" ha"))
                .on_hover_text("Smaller parcels merged into a neighbour, or left out without one (ftw-tools: 500 m²)");
            ui.end_row();
            ui.label("Simplify");
            ui.add(egui::DragValue::new(&mut f.simplify_m).range(0.0..=200.0).suffix(" m"))
                .on_hover_text("The outlines simplified to this tolerance (ftw-tools: 15 m)");
            ui.end_row();
            ui.label("Tiles");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("fields patch").selected_text(format!("{} px", f.patch)).show_ui(ui, |ui| {
                    for p in [128, 256, 512] {
                        ui.selectable_value(&mut f.patch, p, format!("{p} px"));
                    }
                });
                ui.checkbox(&mut f.gpu, "GPU");
            });
            ui.end_row();
        });
        // the season last found, its NDVI and windows
        if let Some((id, s)) = &self.fields.found
            && *id == info.id
        {
            season_plot(ui, &info, s);
        }
        let temporal = self.fields.choice().temporal();
        ui.horizontal(|ui| {
            if self.fields.auto && !temporal && ui.button("Find the season").clicked() {
                let (window, _) = self.run_window(self.fields.scope);
                match fields::find_season(&info, &bands, window, None, self.fields.half) {
                    Ok(s) => {
                        self.fields.status = season_text(&s, &info);
                        self.fields.found = Some((info.id, s));
                    }
                    Err(e) => self.fields.status = e,
                }
            }
            if ui.add(egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
                let _ = self.fields_run(ui.ctx());
            }
        });
        if !self.fields.status.is_empty() {
            ui.weak(&self.fields.status);
        }
        ui.separator();
        egui::CollapsingHeader::new("Train on your fields")
            .id_salt("fields train")
            .show(ui, |ui| self.fields_train_ui(ui));
    }

    /// Training on fields drawn: the model chosen fine-tuned on the polygons of a vector layer inside the ROI.
    fn fields_train_ui(&mut self, ui: &mut Ui) {
        ui.add(
            egui::Label::new(
                egui::RichText::new(
                    "Draw (or open) the fields of an area as polygons, every field in it, and make that area the \
                     ROI: the model chosen learns them (outside the ROI nothing is taught). A few hundred fields \
                     help most; the new model is kept in Janus's field models and chosen above.",
                )
                .weak(),
            )
            .wrap(),
        );
        let areas: Vec<(u32, String, usize)> = self
            .vectors
            .iter()
            .map(|v| {
                let n = v.data.features.iter().filter(|f| f.kind == janus_geo::vectors::Kind::Area).count();
                (v.id, v.name.clone(), n)
            })
            .filter(|v| v.2 > 0)
            .collect();
        let f = &mut self.fields;
        if !areas.iter().any(|a| a.0 == f.train_vector)
            && let Some(a) = areas.first()
        {
            f.train_vector = a.0;
        }
        egui::Grid::new("fields train grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Fields");
            let sel = areas
                .iter()
                .find(|a| a.0 == f.train_vector)
                .map_or("(no polygons open)".to_string(), |a| format!("{} ({} polygons)", a.1, a.2));
            egui::ComboBox::from_id_salt("fields train vector").selected_text(sel).width(260.0).show_ui(ui, |ui| {
                for a in &areas {
                    ui.selectable_value(&mut f.train_vector, a.0, format!("{} ({} polygons)", a.1, a.2));
                }
            });
            ui.end_row();
            ui.label("Epochs");
            ui.add(egui::DragValue::new(&mut f.train_epochs).range(1..=300));
            ui.end_row();
            ui.label("Name");
            ui.text_edit_singleline(&mut f.train_name);
            ui.end_row();
        });
        let roi = self.series.roi_rect.is_some();
        let ok = roi && !areas.is_empty();
        if !roi {
            ui.colored_label(ui.visuals().warn_fg_color, "No ROI: the area whose fields are all drawn");
        }
        if ui.add_enabled(ok, egui::Button::new("▶ Train")).clicked() {
            let _ = self.fields_train_run(ui.ctx());
        }
    }

    /// A task of the tool "fields" on its thread: `work` with the task's log, progress and cancel.
    #[allow(clippy::too_many_arguments)]
    fn fields_job(
        &mut self,
        ctx: &egui::Context,
        title: String,
        info: &CubeInfo,
        dir: &std::path::Path,
        status: Arc<Mutex<JobStatus>>,
        log: JobLog,
        work: impl FnOnce(
            &JobLog,
            &(dyn Fn(f64, &str) + Sync),
            &AtomicBool,
        ) -> Result<(Vec<serde_json::Value>, [usize; 4]), String>
        + Send
        + 'static,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.zeit.jobs.push(Job {
            title,
            tool: "fields".into(),
            cube: info.id,
            output_dir: dir.to_string_lossy().into_owned(),
            log_path: log.path.to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel: cancel.clone(),
            loaded: false,
        });
        self.zeit.show_tasks = true;
        let (st, c) = (status, ctx.clone());
        std::thread::Builder::new()
            .name("fields".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = crate::ml::guarded(|| work(&log, &progress, &cancel));
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok((outputs, window)) => {
                        log.line("done");
                        (s.state, s.progress) = (JobState::Done, 1.0);
                        s.result = Some(janus_zeit::job::JobResult { outputs, window });
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
    }

    /// Starts a training: the model chosen fine-tuned on the fields drawn (the polygons of the vector layer chosen)
    /// inside the ROI; the new model's path.
    pub(crate) fn fields_train_run(&mut self, ctx: &egui::Context) -> Option<std::path::PathBuf> {
        let info = self.session.as_ref()?.info.clone();
        let Some([x0, y0, x1, y1]) = self.series.roi_rect else {
            self.fields.status = "no ROI: make the area whose fields are all drawn the ROI".into();
            return None;
        };
        let (w, h) = (x1 - x0, y1 - y0);
        // the fields' layer placed on the series' grid now if the map has not yet
        if let Some(i) = self.vectors.iter().position(|v| v.id == self.fields.train_vector)
            && self.vectors[i].placed_for != self.vector_place_key()
        {
            self.place_vector(i);
        }
        let Some(v) = self.vectors.iter().find(|v| v.id == self.fields.train_vector) else {
            self.fields.status = "no vector layer of fields".into();
            return None;
        };
        // the fields drawn as a parcel number per pixel of the ROI's box, the ROI's polygon what is known
        let mut labels = vec![0u32; w * h];
        let mut drawn = 0;
        for (i, f) in v.data.features.iter().enumerate() {
            if f.kind != janus_geo::vectors::Kind::Area || i >= v.px.len() || i >= v.boxes.len() {
                continue;
            }
            let b = v.boxes[i];
            if b[0] > b[2] {
                continue;
            }
            let bx0 = (b[0].floor().max(x0 as f32) as usize).min(x1);
            let by0 = (b[1].floor().max(y0 as f32) as usize).min(y1);
            let bx1 = (b[2].ceil().max(0.0) as usize).clamp(bx0, x1);
            let by1 = (b[3].ceil().max(0.0) as usize).clamp(by0, y1);
            if bx1 <= bx0 || by1 <= by0 {
                continue;
            }
            let (bw, bh) = (bx1 - bx0, by1 - by0);
            let poly: Vec<_> = v.px[i].iter().filter(|r| r.len() >= 6).cloned().collect();
            let m = janus_geo::plane::mask(&poly, bx0 as f64, by0 as f64, bw as f64, bh as f64, bw, bh);
            let mut any = false;
            for yy in 0..bh {
                for xx in 0..bw {
                    let at = (by0 - y0 + yy) * w + bx0 - x0 + xx;
                    if m[yy * bw + xx] && labels[at] == 0 {
                        labels[at] = i as u32 + 1;
                        any = true;
                    }
                }
            }
            drawn += any as usize;
        }
        if drawn == 0 {
            self.fields.status = "no field drawn inside the ROI".into();
            return None;
        }
        let known = if self.series.roi_poly.is_empty() {
            vec![true; w * h]
        } else {
            janus_geo::plane::mask(&self.series.roi_poly, x0 as f64, y0 as f64, w as f64, h as f64, w, h)
        };
        let vname = v.name.clone();
        let f = &self.fields;
        let model = f.choice();
        let year = if f.year == 0 { best_year(&info, &model) } else { f.year };
        let season = (!f.auto).then(|| Season {
            a: (f.a.0.min(f.a.1)..=f.a.0.max(f.a.1)).collect(),
            b: (f.b.0.min(f.b.1)..=f.b.0.max(f.b.1)).collect(),
        });
        let safe: String =
            f.train_name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
        let safe = if safe.trim_matches('_').is_empty() { "fields".to_string() } else { safe };
        let _ = std::fs::create_dir_all(models_dir());
        let mut out = models_dir().join(format!("{safe}.janusfields"));
        if out.exists() {
            out = models_dir().join(format!("{safe}_{}.janusfields", crate::zeit::timestamp()));
        }
        let (epochs, gpu, half) = (f.train_epochs, f.gpu, f.half);
        let dir = self.results_dir().join(format!("{safe}_fields_training_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error_popup = Some(format!("could not create {}: {e}", dir.display()));
            return None;
        }
        let log = JobLog { path: dir.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Field model training: {safe}\n{}\nstarting from: {} ({})\nfields: {vname} ({drawn} drawn in the ROI, \
             {w} x {h} px)\nepochs: {epochs}\nmodel: {}\n",
            crate::zeit::timestamp_text(),
            model.id(),
            model.source(),
            out.display()
        ));
        let about_fields = format!("{vname} ({drawn} fields)");
        let title = format!("Field model training ({}) - {drawn} fields", model.id());
        let status = status_new();
        let window = [x0, y0, x1, y1];
        let (o, inf) = (out.clone(), info.clone());
        self.fields_job(ctx, title, &info, &dir, status.clone(), log, move |log, progress, cancel| {
            let info = &inf;
            let mut bands = FieldBands::of(info)?;
            let dev = shared_device(gpu);
            let inputs = if model.temporal() {
                let months = months_of(info, &model, year);
                let all: Vec<usize> = months.iter().flatten().copied().collect();
                if all.is_empty() {
                    return Err(format!("the series has no date in the months the model reads for {year}"));
                }
                bands.set_units(fields::units_high(info, &bands, window, &all));
                ChipInputs::Months(months)
            } else {
                let season = match &season {
                    Some(s) => {
                        bands.set_units(fields::units_high(info, &bands, window, &s.a));
                        s.clone()
                    }
                    None => {
                        let found = fields::find_season(info, &bands, window, None, half)?;
                        log.line(&season_text(&found, info));
                        bands.set_units(found.high);
                        found.season
                    }
                };
                ChipInputs::Season(season)
            };
            let side = if model.temporal() { 128 } else { 256 };
            let chips = ft::chips_of_window(info, &bands, &inputs, window, &labels, &known, side, &|f, m| progress(0.1 * f, m), cancel)?;
            log.line(&format!("{} chips of {} px", chips.len(), chips.first().map_or(0, |c| c.side)));
            if chips.len() < 2 {
                return Err(format!(
                    "{} chip of the ROI has fields drawn: make the ROI (and its fields) larger, two chips of {side} px at least",
                    chips.len()
                ));
            }
            let cfg = FieldTrainConfig {
                epochs,
                batch: 2,
                crop: side,
                lr: if model.temporal() { 3e-4 } else { 1e-4 },
                patience: (epochs / 3).max(5),
                validation: 0.2,
                crops: 4,
                ..Default::default()
            };
            let n = epochs.max(1) as f64;
            let mut on_epoch = |e: &janus_ml::train::Epoch| {
                log.line(&format!(
                    "epoch {}: loss {:.4}, accuracy {:.3}; validation loss {:.4}, accuracy {:.3} ({:.0} s)",
                    e.epoch, e.train_loss, e.train_acc, e.val_loss, e.val_acc, e.seconds
                ));
                progress(
                    0.1 + 0.88 * e.epoch as f64 / n,
                    &format!("epoch {}/{epochs}: validation accuracy {:.3}", e.epoch, e.val_acc),
                );
            };
            let (header, net) = match &model {
                Choice::File(p, h @ FieldHeader { arch: FieldArch::Utae { months, standard, .. }, .. }) => {
                    let days = fields::month_positions(*months);
                    let start = |d: &janus_ml::Device| match FieldModel::load(p, d)?.net {
                        FieldNetwork::Utae(n) => Ok(n),
                        _ => Err("not a U-TAE".to_string()),
                    };
                    let (net, trained) =
                        ft::train_field_utae(&chips, &start, &days, standard, &cfg, &dev.0, &mut on_epoch, cancel)?;
                    let header = FieldHeader {
                        arch: h.arch.clone(),
                        about: format!("{} fine-tuned on {about_fields}", h.about),
                        training: serde_json::json!({"config": cfg, "result": trained, "from": p}),
                    };
                    (header, FieldNetwork::Utae(net))
                }
                _ => {
                    let (start_path, base): (std::path::PathBuf, String) = match &model {
                        Choice::Checkpoint(ck) => (
                            fieldnet::weights(&crate::settings::app_data_dir().join("weights"), ck, &|_| {}, cancel)?,
                            ck.id.into(),
                        ),
                        Choice::File(p, h) => (
                            p.clone(),
                            match &h.arch {
                                FieldArch::FieldNet { base } => base.clone(),
                                _ => String::new(),
                            },
                        ),
                    };
                    let from_file = matches!(model, Choice::File(..));
                    let start = |d: &janus_ml::Device| {
                        if from_file {
                            match FieldModel::load(&start_path, d)?.net {
                                FieldNetwork::FieldNet(n) => Ok(n),
                                _ => Err("not a FieldNet".to_string()),
                            }
                        } else {
                            FieldNet::new(fieldnet::INPUTS, fieldnet::CLASSES, d).load_ftw(&start_path)
                        }
                    };
                    let (net, trained) = ft::train_fieldnet(&chips, &start, &cfg, &dev.0, &mut on_epoch, cancel)?;
                    let header = FieldHeader {
                        arch: FieldArch::FieldNet { base },
                        about: format!("{} fine-tuned on {about_fields}", model.id()),
                        training: serde_json::json!({"config": cfg, "result": trained}),
                    };
                    (header, FieldNetwork::FieldNet(net))
                }
            };
            FieldModel { header, net }.save(&o)?;
            log.line(&format!("kept {}", o.display()));
            Ok((vec![], window))
        });
        self.fields.training.push((status, out.clone()));
        self.fields.status = format!("training: the model is chosen when ready ({})", out.display());
        Some(out)
    }

    /// Starts a run on the active series with the window's choices; its GeoPackage's path.
    pub(crate) fn fields_run(&mut self, ctx: &egui::Context) -> Option<String> {
        let info = self.session.as_ref()?.info.clone();
        let (window, scope_name) = self.run_window(self.fields.scope);
        let f = &self.fields;
        let model = f.choice();
        let season = (!f.auto).then(|| Season {
            a: (f.a.0.min(f.a.1)..=f.a.0.max(f.a.1)).collect(),
            b: (f.b.0.min(f.b.1)..=f.b.0.max(f.b.1)).collect(),
        });
        let layer_inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(&info, &layer_inputs);
        let dir = self.results_dir().join(format!("{name}_fields_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error_popup = Some(format!("could not create {}: {e}", dir.display()));
            return None;
        }
        let year = if f.year == 0 { best_year(&info, &model) } else { f.year };
        let spec = Spec {
            info: info.clone(),
            window,
            model: model.clone(),
            year,
            season,
            half: f.half,
            method: f.method,
            core: f.core,
            min_area_ha: f.min_area_ha,
            simplify_m: f.simplify_m,
            gpu: f.gpu,
            patch: f.patch,
            dir: dir.clone(),
            stem: "fields".into(),
        };
        let gpkg = dir.join("fields_parcels.gpkg");
        let title = format!(
            "Field delineation ({}) - {scope_name} ({} x {} px)",
            model.id(),
            window[2] - window[0],
            window[3] - window[1]
        );
        let log = JobLog { path: dir.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Field delineation: {name}\n{}\nmodel: {} ({})\nwindow: {window:?} ({scope_name})\nparcels: {:?}, \
             seeds {} %, minimum area {} ha, simplify {} m\noutput: {}\n",
            crate::zeit::timestamp_text(),
            model.id(),
            model.source(),
            spec.method,
            spec.core,
            spec.min_area_ha,
            spec.simplify_m,
            dir.display()
        ));
        {
            use crate::provenance::{Provenance, SeriesRecord};
            let mut p = Provenance::start("fields", "fields", "Field delineation");
            let lname = self.layers.get(self.active).map_or(name.clone(), |l| l.name.clone());
            p.series = Some(SeriesRecord::of(&info, &lname, &layer_inputs));
            (p.scope, p.scope_index, p.window) = (scope_name.to_string(), self.fields.scope, Some(window));
            for (k, v) in [
                ("model", serde_json::json!(model.id())),
                ("weights", serde_json::json!(model.source())),
                (
                    "season",
                    serde_json::json!(if model.temporal() {
                        format!("the months of {year}")
                    } else if self.fields.auto {
                        "found (NDVI)".into()
                    } else {
                        "chosen".into()
                    }),
                ),
                ("half_days", serde_json::json!(spec.half)),
                ("parcels", serde_json::json!(format!("{:?}", spec.method))),
                ("seed_percent", serde_json::json!(spec.core)),
                ("min_area_ha", serde_json::json!(spec.min_area_ha)),
                ("simplify_m", serde_json::json!(spec.simplify_m)),
            ] {
                p.extra.insert(k.into(), v);
            }
            p.python_note = "ftw-tools: ftw inference run (the checkpoint), then ftw inference polygonize".into();
            self.begin_provenance(&dir, p);
        }
        let (st, out) = (status_new(), gpkg.clone());
        self.fields_job(ctx, title, &info, &dir, st.clone(), log, move |log, progress, cancel| {
            run(&spec, &out, log, progress, cancel)
        });
        let status = st;
        let gpkg = gpkg.to_string_lossy().into_owned();
        self.fields.pending.push((status, gpkg.clone()));
        self.fields.status = format!("under way: the parcels open as a vector layer when ready ({gpkg})");
        Some(gpkg)
    }
}

/// The run: the weights, the season, the network over the window, the parcels, their polygons.
fn run(
    spec: &Spec,
    gpkg: &std::path::Path,
    log: &JobLog,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<(Vec<serde_json::Value>, [usize; 4]), String> {
    let info = &spec.info;
    let mut bands = FieldBands::of(info)?;
    let dev = shared_device(spec.gpu);
    let net = match &spec.model {
        Choice::Checkpoint(ck) => {
            progress(0.0, &format!("{}: the weights", ck.title));
            let weights = fieldnet::weights(
                &crate::settings::app_data_dir().join("weights"),
                ck,
                &|f| progress(0.05 * f, &format!("downloading {} ({:.0} %)", ck.title, 100.0 * f)),
                cancel,
            )?;
            log.line(&format!("device: {}, weights: {}", dev.1, weights.display()));
            FieldNetwork::FieldNet(FieldNet::new(fieldnet::INPUTS, fieldnet::CLASSES, &dev.0).load_ftw(&weights)?)
        }
        Choice::File(p, _) => {
            log.line(&format!("device: {}, model: {}", dev.1, p.display()));
            FieldModel::load(p, &dev.0)?.net
        }
    };
    let probs = match (&net, &spec.model) {
        (FieldNetwork::Utae(utae), Choice::File(_, FieldHeader { arch: FieldArch::Utae { standard, .. }, .. })) => {
            let months = months_of(info, &spec.model, spec.year);
            let all: Vec<usize> = months.iter().flatten().copied().collect();
            if all.is_empty() {
                return Err(format!("the series has no date in the months the model reads for {}", spec.year));
            }
            bands.set_units(fields::units_high(info, &bands, spec.window, &all));
            log.line(&format!(
                "the months of {}: {} of {} with dates ({} dates)",
                spec.year,
                months.iter().filter(|m| !m.is_empty()).count(),
                months.len(),
                all.len()
            ));
            let patch = spec.patch.min(256);
            fields::delineate_temporal(
                utae,
                standard,
                &dev.0,
                info,
                &bands,
                &months,
                spec.window,
                patch,
                2,
                &|f, m| progress(0.05 + 0.8 * f, m),
                cancel,
            )?
        }
        (FieldNetwork::FieldNet(net), _) => {
            let season = match &spec.season {
                Some(s) => {
                    bands.set_units(fields::units_high(info, &bands, spec.window, &s.a));
                    s.clone()
                }
                None => {
                    progress(0.05, "the season (the region's NDVI)");
                    let found = fields::find_season(info, &bands, spec.window, None, spec.half)?;
                    log.line(&season_text(&found, info));
                    bands.set_units(found.high);
                    found.season
                }
            };
            let dates = |d: &[usize]| d.iter().map(|&k| info.layers[k].label.clone()).collect::<Vec<_>>().join(", ");
            log.line(&format!("window A ({} dates): {}", season.a.len(), dates(&season.a)));
            log.line(&format!("window B ({} dates): {}", season.b.len(), dates(&season.b)));
            let tiling = Tiling {
                patch: spec.patch,
                pad: spec.patch.div_ceil(16),
                resize: spec.model.resize(),
                ..Tiling::default()
            };
            fields::delineate(
                net,
                &dev.0,
                info,
                &bands,
                &season,
                spec.window,
                &tiling,
                &|f, m| progress(0.05 + 0.8 * f, m),
                cancel,
            )?
        }
        _ => return Err("the model's file does not fit its network".into()),
    };
    log.line(&format!(
        "units: {} (x {} to digital numbers)",
        if bands.scale > 1.0 { "reflectance" } else { "digital numbers" },
        bands.scale
    ));
    progress(0.86, "the parcels");
    let labels = fields::parcels(&probs, spec.method, spec.core);
    log.line(&format!("{} parcels before the minimum area", labels.iter().copied().max().unwrap_or(0)));
    let outputs = fields::write(&spec.dir, &spec.stem, info, spec.window, &probs, &labels, &spec.model.id())?;
    progress(0.9, "the parcels' polygons");
    let opts = PolyOptions {
        band: 1,
        eight: false,
        min_area_ha: spec.min_area_ha,
        simplify_m: spec.simplify_m,
        classes: vec![],
        years: false,
        mean_of: Some((
            "interior".into(),
            spec.dir.join(format!("{}_interior.tif", spec.stem)).to_string_lossy().into_owned(),
            1,
        )),
        layer: "parcels".into(),
    };
    let parcels = spec.dir.join(format!("{}_parcels.tif", spec.stem));
    let r = polygonize(&parcels.to_string_lossy(), &opts, gpkg, &|f, m| progress(0.9 + 0.1 * f, m), cancel)?;
    log.line(&format!("{} parcels, {:.1} ha ({} under the minimum area left out)", r.polygons, r.area_ha, r.dropped));
    Ok((outputs, spec.window))
}

fn season_text(s: &fields::SeasonFound, info: &CubeInfo) -> String {
    let d = fields::day_text;
    let _ = info;
    format!(
        "season: green-up about {}, peak {}, fall about {}; window A {} dates, B {} dates",
        d(s.a_day),
        d(s.peak_day),
        d(s.b_day),
        s.season.a.len(),
        s.season.b.len()
    )
}

/// The region's NDVI over the dates, the two windows shaded.
fn season_plot(ui: &mut Ui, info: &CubeInfo, s: &fields::SeasonFound) {
    let x = |k: usize| info.decimal_year(k);
    let pts: Vec<[f64; 2]> =
        (0..s.ndvi.len()).filter(|&k| s.ndvi[k].is_finite()).map(|k| [x(k), s.ndvi[k] as f64]).collect();
    let span = |d: &[usize]| -> Option<(f64, f64)> { Some((x(*d.first()?), x(*d.last()?))) };
    Plot::new("fields season").height(120.0).allow_scroll(false).allow_drag(false).allow_zoom(false).show(ui, |p| {
        for (name, d, color) in [
            ("window A", &s.season.a, egui::Color32::from_rgba_unmultiplied(80, 160, 230, 60)),
            ("window B", &s.season.b, egui::Color32::from_rgba_unmultiplied(230, 140, 60, 60)),
        ] {
            if let Some((a, b)) = span(d) {
                let (a, b) = (a - 0.005, b + 0.005);
                p.polygon(
                    Polygon::new(name, PlotPoints::from(vec![[a, -0.1], [b, -0.1], [b, 1.0], [a, 1.0]]))
                        .fill_color(color)
                        .stroke(egui::Stroke::NONE),
                );
            }
        }
        p.line(Line::new("NDVI (the region's median)", PlotPoints::from(pts)));
    });
}
