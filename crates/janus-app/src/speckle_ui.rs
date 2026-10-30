//! Tools > Speckle filter... (IDEIAS phase 47): a radar series (Sentinel-1's VV and VH) through Lee's filter, a
//! window of each date, or Quegan's multitemporal one, every date's window means put together; on the bands'
//! linear values, written as a folder of COGs (a task) that opens as a new series shown as the one filtered.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_zeit::speckle::{Filter, SpeckleOptions, speckle};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const FILTERS: [(&str, &str); 2] = [
    (
        "Quegan (multitemporal)",
        "Each date's window mean times the mean, over the dates, of each date's value over its own window mean\n\
         (Quegan & Yu 2001): the speckle averaged through time, each date's changes kept",
    ),
    (
        "Lee (spatial)",
        "Each date alone: the window's mean, plus the pixel's departure from it weighted by how much more the\n\
         window varies than the speckle does (Lee 1980): smooth areas averaged, edges kept",
    ),
];

pub struct SpeckleUi {
    pub show: bool,
    /// 0 Quegan, 1 Lee
    pub filter: usize,
    pub size: usize,
    /// the looks of Lee's filter (Sentinel-1 IW GRDH: 4.4)
    pub looks: f64,
    pub roi: bool,
    pub name: String,
    pub status: String,
    /// the threads its runs use, set in its window (None: the Settings')
    pub threads: Option<usize>,
}

impl Default for SpeckleUi {
    fn default() -> Self {
        SpeckleUi {
            show: false,
            filter: 0,
            size: 7,
            looks: 4.4,
            roi: false,
            name: String::new(),
            status: String::new(),
            threads: None,
        }
    }
}

impl SpeckleUi {
    pub fn options(&self, threads: usize) -> SpeckleOptions {
        SpeckleOptions {
            filter: if self.filter == 1 { Filter::Lee { looks: self.looks.max(0.1) } } else { Filter::Quegan },
            size: self.size | 1,
            window: None,
            stem: "speckle".into(),
            threads,
        }
    }
}

impl State {
    pub(crate) fn speckle_window(&mut self, ctx: &egui::Context) {
        if !self.speckle.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Speckle filter", ctx, 440.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.speckle_ui(ui)));
        self.speckle.show = open;
    }

    fn speckle_ui(&mut self, ui: &mut Ui) {
        let Some(s) = self.session.as_ref() else {
            ui.weak("Open a radar series first (File > Search STAC catalog > Sentinel-1 RTC).");
            return;
        };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        if !info.time_is_date {
            ui.weak(format!("\"{layer}\" has no dates."));
            return;
        }
        let r = &mut self.speckle;
        ui.label(format!(
            "{layer}: {} dates of {} band(s) ({})",
            info.t(),
            info.bands_per_date.max(1),
            info.band_names.join(", ")
        ));
        egui::Grid::new("speckle grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Filter");
            egui::ComboBox::from_id_salt("speckle filter").selected_text(FILTERS[r.filter].0).show_ui(ui, |ui| {
                for (i, (s, h)) in FILTERS.iter().enumerate() {
                    ui.selectable_value(&mut r.filter, i, *s).on_hover_text(*h);
                }
            });
            ui.end_row();
            ui.label("Window");
            egui::ComboBox::from_id_salt("speckle size").selected_text(format!("{0} x {0} pixels", r.size)).show_ui(
                ui,
                |ui| {
                    for k in [3, 5, 7, 9, 11, 15] {
                        ui.selectable_value(&mut r.size, k, format!("{k} x {k} pixels"));
                    }
                },
            );
            ui.end_row();
            if r.filter == 1 {
                ui.label("Looks");
                ui.add(egui::DragValue::new(&mut r.looks).range(0.5..=100.0).speed(0.1).fixed_decimals(1))
                    .on_hover_text(
                        "The speckle's number of looks (its variation is 1 / sqrt(looks)):\n\
                         Sentinel-1 IW GRDH has 4.4; fewer smooths less",
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
            ui.add(egui::TextEdit::singleline(&mut r.name).hint_text(format!("{layer} filtered")).desired_width(220.0));
            ui.end_row();
        });
        let quegan = self.speckle.filter == 0;
        if quegan && info.t() < 2 {
            ui.colored_label(crate::theme::warning(), "Quegan's filter needs 2 dates or more");
        }
        ui.weak(
            "Every band of every date, on the linear values (not dB): a COG per date, in a folder that opens \
             as a series shown as this one is.",
        );
        let mut own = self.speckle.threads;
        self.threads_ui(ui, &mut own);
        self.speckle.threads = own;
        ui.add_space(4.0);
        if ui.add_enabled(!quegan || info.t() >= 2, egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.speckle_run(ui.ctx());
        }
        if !self.speckle.status.is_empty() {
            ui.weak(&self.speckle.status);
        }
    }

    /// The filter as a task; its folder opened as a series (shown as the one filtered) when done.
    pub(crate) fn speckle_run(&mut self, ctx: &egui::Context) {
        let Some(s) = self.session.as_ref() else { return };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let mut opts = self.speckle.options(self.speckle.threads.unwrap_or(self.processing_threads()));
        if self.speckle.roi {
            opts.window = self.series.roi_rect;
        }
        let name = if self.speckle.name.trim().is_empty() {
            format!("{layer} filtered")
        } else {
            self.speckle.name.trim().into()
        };
        opts.stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("speckle").join(format!("{}_{}", opts.stem, crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return;
        }
        let what = format!("{} {1} x {1}", opts.filter.name(), opts.size);
        let title = format!("Speckle {layer}: {what}");
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Speckle filter: {name}\n{}\nseries: {layer} ({} dates)\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            info.t(),
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("speckle", "speckle", "Speckle filter");
            p.series = Some(crate::provenance::SeriesRecord::of(&info, &layer, &inputs));
            p.window = opts.window;
            p.threads = Some(opts.threads as i64);
            p.scope = if opts.window.is_some() { "the ROI".into() } else { "the whole layer".into() };
            p.params.insert("filter".into(), serde_json::json!(opts.filter.name()));
            p.params.insert("window".into(), serde_json::json!(opts.size));
            if let Filter::Lee { looks } = opts.filter {
                p.params.insert("looks".into(), serde_json::json!(looks));
            }
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
        let (info2, opts2) = (info.clone(), opts.clone());
        std::thread::Builder::new()
            .name("speckle".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = speckle(&info2, &opts2, &out, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(files) => {
                        log.line(&format!("{} dates filtered, {:.1} s", files.len(), s.seconds));
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
            tool: "speckle".into(),
            cube: 0,
            output_dir: folder_s.clone(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        // the filtered series shown as the one filtered (its dB, an index), with the same bands
        let sel = info.sel.expr.is_some().then_some(info.sel);
        self.emb_downloads.push(EmbDownload {
            title,
            status,
            started: Instant::now(),
            folder: folder_s,
            name: Some(name.clone()),
            keep_active: false,
            sel,
            opened: false,
            ended: None,
        });
        self.speckle.status = format!("\"{name}\" under way: it opens as a series when ready ({})", folder.display());
    }
}
