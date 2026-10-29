//! Tools > Regularize series... (IDEIAS phase 40): the active series as one composite per period of a fixed step
//! (the median, the medoid across its bands, the clearest date), its empty periods filled up to a gap, written as a
//! folder of COGs (a task) that opens as a new series when done.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::regularize::{Composite, Fill, RegOptions, Step, periods, regularize};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const STEPS: [&str; 5] = ["8 days", "16 days", "Monthly", "Yearly", "Days..."];
const COMPOSITES: [(&str, &str); 3] = [
    ("Median", "Each band's median of the period's values (zeit.regularize_time_series 'median')"),
    (
        "Medoid",
        "The observation nearest the bands' medians, so a composite's bands come from one date \
         (zeit.regularize_time_series 'medoid'); with one band, the value nearest the median",
    ),
    (
        "Clearest date",
        "The value of the period's date with the most clear pixels that has one (the next clearest else)",
    ),
];
const FILLS: [&str; 3] = ["None", "Linear", "Whittaker"];

pub struct RegUi {
    pub show: bool,
    pub step: usize,
    pub days: u32,
    pub composite: usize,
    pub fill: usize,
    pub lambda: f64,
    /// a gap is filled when its values around are at most this many days apart (0: any)
    pub max_gap: f64,
    pub all_bands: bool,
    pub roi: bool,
    pub name: String,
    pub status: String,
}

impl Default for RegUi {
    fn default() -> Self {
        RegUi {
            show: false,
            step: 1,
            days: 10,
            composite: 0,
            fill: 0,
            lambda: 10.0,
            max_gap: 0.0,
            all_bands: false,
            roi: false,
            name: String::new(),
            status: String::new(),
        }
    }
}

impl RegUi {
    pub fn options(&self, threads: usize) -> RegOptions {
        RegOptions {
            step: match self.step {
                0 => Step::Days(8),
                1 => Step::Days(16),
                2 => Step::Month,
                3 => Step::Year,
                _ => Step::Days(self.days.max(1)),
            },
            composite: [Composite::Median, Composite::Medoid, Composite::Clearest][self.composite.min(2)],
            fill: match self.fill {
                1 => Fill::Linear,
                2 => Fill::Whittaker(self.lambda),
                _ => Fill::None,
            },
            max_gap_days: self.max_gap.max(0.0),
            all_bands: self.all_bands,
            window: None,
            stem: "reg".into(),
            threads,
        }
    }

    fn step_text(&self) -> String {
        match self.step {
            4 => format!("{} days", self.days.max(1)),
            s => STEPS[s].to_lowercase(),
        }
    }
}

impl State {
    /// The processing threads (Settings, --threads).
    pub(crate) fn processing_threads(&self) -> usize {
        match self.opts.threads.unwrap_or(self.settings.s.threads) {
            0 => janus_session::default_processing_threads(),
            n => n,
        }
    }

    pub(crate) fn regularize_window(&mut self, ctx: &egui::Context) {
        if !self.reg.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Regularize series", ctx, 460.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.regularize_ui(ui)));
        self.reg.show = open;
    }

    fn regularize_ui(&mut self, ui: &mut Ui) {
        let Some(s) = self.session.as_ref() else {
            ui.weak("Open a series with dates first.");
            return;
        };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        if !info.time_is_date || info.t() < 2 {
            ui.weak(format!("\"{layer}\" has no dates to regularize."));
            return;
        }
        let r = &mut self.reg;
        ui.label(format!(
            "{layer}: {} dates, {} to {}",
            info.t(),
            info.layers[0].label,
            info.layers[info.t() - 1].label
        ));
        egui::Grid::new("reg grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Step");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("reg step").selected_text(STEPS[r.step]).show_ui(ui, |ui| {
                    for (i, s) in STEPS.iter().enumerate() {
                        ui.selectable_value(&mut r.step, i, *s);
                    }
                });
                if r.step == 4 {
                    ui.add(egui::DragValue::new(&mut r.days).range(1..=3650).suffix(" days"));
                }
            });
            ui.end_row();
            ui.label("A period gives");
            egui::ComboBox::from_id_salt("reg composite").selected_text(COMPOSITES[r.composite].0).show_ui(ui, |ui| {
                for (i, (s, h)) in COMPOSITES.iter().enumerate() {
                    ui.selectable_value(&mut r.composite, i, *s).on_hover_text(*h);
                }
            });
            ui.end_row();
            ui.label("Bands");
            ui.horizontal(|ui| {
                ui.radio_value(&mut r.all_bands, false, format!("as shown ({})", info.selection_text()));
                ui.add_enabled_ui(info.bands_per_date > 1, |ui| {
                    ui.radio_value(&mut r.all_bands, true, format!("all {}", info.bands_per_date))
                        .on_hover_text("Every band of the dates, the quality mask applied (the medoid across them)");
                });
            });
            ui.end_row();
            ui.label("Empty periods");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("reg fill").selected_text(FILLS[r.fill]).show_ui(ui, |ui| {
                    ui.selectable_value(&mut r.fill, 0, "None").on_hover_text("Left without a value");
                    ui.selectable_value(&mut r.fill, 1, "Linear")
                        .on_hover_text("Linear in time between the values around");
                    ui.selectable_value(&mut r.fill, 2, "Whittaker")
                        .on_hover_text("From a Whittaker curve through the values (the values kept)");
                });
                if r.fill == 2 {
                    ui.add(egui::DragValue::new(&mut r.lambda).range(0.01..=1e6).speed(0.5).prefix("λ "));
                }
            });
            ui.end_row();
            if r.fill > 0 {
                ui.label("Longest gap");
                ui.add(egui::DragValue::new(&mut r.max_gap).range(0.0..=3650.0).suffix(" days")).on_hover_text(
                    "A gap is filled when the values around it are at most this many days apart (0: any)",
                );
                ui.end_row();
            }
            ui.label("Area");
            ui.horizontal(|ui| {
                ui.radio_value(&mut r.roi, false, "the whole layer");
                ui.add_enabled_ui(self.series.roi_rect.is_some(), |ui| {
                    ui.radio_value(&mut r.roi, true, "the ROI");
                });
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut r.name).hint_text(format!("{layer} regular")).desired_width(220.0));
            ui.end_row();
        });
        let opts = self.reg.options(1);
        let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let ps = periods(&times, opts.step);
        let empty = ps.iter().filter(|p| p.dates.is_empty()).count();
        ui.weak(format!(
            "{} periods ({}){}: one COG each, in a folder that opens as a series",
            ps.len(),
            self.reg.step_text(),
            if empty > 0 { format!(", {empty} without a date") } else { String::new() }
        ));
        ui.add_space(4.0);
        if ui.add(egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.regularize_run(ui.ctx());
        }
        if !self.reg.status.is_empty() {
            ui.weak(&self.reg.status);
        }
    }

    /// The regularization as a task; its folder opened as a series when done.
    pub(crate) fn regularize_run(&mut self, ctx: &egui::Context) {
        let Some(s) = self.session.as_ref() else { return };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let mut opts = self.reg.options(self.processing_threads());
        if self.reg.roi {
            opts.window = self.series.roi_rect;
        }
        let name =
            if self.reg.name.trim().is_empty() { format!("{layer} regular") } else { self.reg.name.trim().into() };
        opts.stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("regularize").join(format!("{}_{}", opts.stem, crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return;
        }
        let title = format!("Regularize {layer}: {}", self.reg.step_text());
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Regularize series: {name}\n{}\nseries: {layer} ({} dates)\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            info.t(),
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("regularize", "regularize", "Regularize series");
            p.series = Some(crate::provenance::SeriesRecord::of(&info, &layer, &inputs));
            p.window = opts.window;
            p.threads = Some(opts.threads as i64);
            p.scope = if opts.window.is_some() { "the ROI".into() } else { "the whole layer".into() };
            for (k, v) in [
                ("step", serde_json::json!(self.reg.step_text())),
                ("composite", serde_json::json!(COMPOSITES[self.reg.composite].0)),
                ("bands", serde_json::json!(if opts.all_bands { "all" } else { "as shown" })),
                ("fill", serde_json::json!(FILLS[self.reg.fill])),
                ("lambda", serde_json::json!(self.reg.lambda)),
                ("max_gap_days", serde_json::json!(opts.max_gap_days)),
            ] {
                p.params.insert(k.into(), v);
            }
            p.python_note = "zeit.regularize_time_series(cube, freq, method) for the median and the medoid".into();
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
            .name("regularize".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = regularize(&info, &opts, &out, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(r) => {
                        log.line(&format!(
                            "{} regular dates ({} periods without a date), {:.1} s",
                            r.files.len(),
                            r.empty,
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
            tool: "regularize".into(),
            cube: 0,
            output_dir: folder_s.clone(),
            log_path: log_path_of(&folder),
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
        self.reg.status = format!("\"{name}\" under way: it opens as a series when ready ({})", folder.display());
    }
}

fn log_path_of(folder: &std::path::Path) -> String {
    folder.join("job.log").to_string_lossy().into_owned()
}
