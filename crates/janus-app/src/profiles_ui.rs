//! Profiles of every line... in a line layer's menu: each line (each part of a multi-line) sampled along its length
//! about a pixel apart, as the transect samples it, at every date of the active series or the dates chosen
//! (janus_geo::profiles: read by windows around the samples, a date per thread). A task of Tools > Tasks writing a
//! long CSV (a row per sample and date: the line's FID and the fields chosen, the part, the sample, its distance
//! along the line, its place in the series' CRS and pixel, the date and the value) and, if asked, a wide one (a row
//! per sample, a column per date).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_geo::profiles::{ProfileOptions, profiles};

use crate::app::State;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

#[derive(Default)]
pub struct ProfilesUi {
    pub show: bool,
    /// the vector layer (its id)
    pub vector: Option<u32>,
    /// every date, else from .. to (places in the series)
    pub all_dates: bool,
    pub from: usize,
    pub to: usize,
    /// the fields written with each line (their places)
    pub fields: Vec<usize>,
    /// a wide table too
    pub wide: bool,
    pub name: String,
    pub status: String,
    /// the threads its runs use (None: the Settings')
    pub threads: Option<usize>,
    /// the last run: its status and folder
    pub last: Option<(Arc<Mutex<JobStatus>>, String)>,
}

impl State {
    pub(crate) fn open_profiles(&mut self, vector: u32) {
        if self.profiles.vector != Some(vector) {
            (self.profiles.fields, self.profiles.name) = (Vec::new(), String::new());
            self.profiles.all_dates = true;
        }
        self.profiles.vector = Some(vector);
        self.profiles.show = true;
    }

    pub(crate) fn profiles_window(&mut self, ctx: &egui::Context) {
        self.pump_profiles();
        if !self.profiles.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Profiles of every line", ctx, 440.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.profiles_form(ui)));
        self.profiles.show = open;
    }

    fn profiles_form(&mut self, ui: &mut Ui) {
        let Some(v) = self.profiles.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id)) else {
            ui.weak("Choose Profiles of every line... in a line layer's menu.");
            return;
        };
        let (vname, fields, lines) = (v.name.clone(), v.data.fields.clone(), v.data.lines);
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.weak("Open a raster series: the lines are sampled on its grid.");
            return;
        };
        let series = self.layers.get(self.active).map_or("the active series".into(), |l| l.name.clone());
        let labels: Vec<String> = info.layers.iter().map(|l| l.label.clone()).collect();
        ui.label(format!("{vname}: {lines} lines, on {series}"));
        let p = &mut self.profiles;
        p.fields.retain(|&k| k < fields.len());
        p.from = p.from.min(labels.len().saturating_sub(1));
        p.to =
            if p.to == 0 && !labels.is_empty() { labels.len() - 1 } else { p.to.min(labels.len().saturating_sub(1)) };
        egui::Grid::new("profiles grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Dates");
            ui.vertical(|ui| {
                ui.radio_value(&mut p.all_dates, true, format!("every date ({})", labels.len()));
                ui.horizontal(|ui| {
                    ui.radio_value(&mut p.all_dates, false, "from");
                    ui.add_enabled_ui(!p.all_dates && labels.len() > 1, |ui| {
                        for (salt, d) in [("profiles from", &mut p.from), ("profiles to", &mut p.to)] {
                            egui::ComboBox::from_id_salt(salt).selected_text(&labels[*d]).show_ui(ui, |ui| {
                                for (k, l) in labels.iter().enumerate() {
                                    ui.selectable_value(d, k, l);
                                }
                            });
                        }
                    });
                });
            });
            ui.end_row();
            ui.label("Fields");
            ui.vertical(|ui| {
                if fields.is_empty() {
                    ui.weak("none in the file (the FID names each line)");
                }
                egui::ScrollArea::vertical().max_height(crate::fit::scaled(ui, 120.0)).show(ui, |ui| {
                    for (k, f) in fields.iter().enumerate() {
                        let mut on = p.fields.contains(&k);
                        if ui.checkbox(&mut on, f).changed() {
                            if on {
                                p.fields.push(k);
                                p.fields.sort_unstable();
                            } else {
                                p.fields.retain(|&x| x != k);
                            }
                        }
                    }
                });
            });
            ui.end_row();
            ui.label("Tables");
            ui.vertical(|ui| {
                ui.weak("a row per sample and date (long)");
                ui.checkbox(&mut p.wide, "and a row per sample, a column per date (wide)");
            });
            ui.end_row();
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut p.name)
                    .hint_text(format!("{vname} profiles"))
                    .desired_width(crate::fit::scaled(ui, 220.0)),
            );
            ui.end_row();
        });
        ui.weak(
            "Each line sampled about a pixel apart along its length, as the transect samples it (distances in \
             metres, or the CRS's units); samples off the series are left out. The values the chart shows.",
        );
        let mut own = self.profiles.threads;
        self.threads_ui(ui, &mut own);
        self.profiles.threads = own;
        ui.add_space(4.0);
        let deferred = self.session.as_ref().is_some_and(|s| s.s.defer_random_reads());
        if deferred {
            ui.colored_label(crate::theme::warning(), "Building the overview from an HDD: available once it ends.");
        }
        let busy = self.profiles.last.as_ref().is_some_and(|(st, _)| st.lock().unwrap().state == JobState::Running);
        if ui.add_enabled(!busy && !deferred, egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.profiles_run(ui.ctx());
        }
        if !self.profiles.status.is_empty() {
            ui.weak(&self.profiles.status);
        }
    }

    /// The last run's end in the window's status.
    fn pump_profiles(&mut self) {
        let Some((st, folder)) = &self.profiles.last else { return };
        let s = st.lock().unwrap();
        let text = match s.state {
            JobState::Running => return,
            JobState::Done => format!("Done in {:.1} s: {}", s.seconds, s.message),
            JobState::Failed => format!("Failed: {}", s.error),
            _ => "Cancelled".into(),
        };
        let text = if s.state == JobState::Done { format!("{text} (in {folder})") } else { text };
        drop(s);
        if self.profiles.status != text {
            self.profiles.status = text;
        }
    }

    /// The profiles of the chosen layer's lines as a task; its folder (None: not started, the status says why).
    pub(crate) fn profiles_run(&mut self, ctx: &egui::Context) -> Option<String> {
        let v = self.profiles.vector.and_then(|id| self.vectors.iter().find(|v| v.id == id))?;
        let (data, vname) = (v.data.clone(), v.name.clone());
        let Some(s) = self.session.as_ref() else {
            self.profiles.status = "open a series".into();
            return None;
        };
        if s.info.geo_transform.is_none() {
            self.profiles.status = "the series has no georeference".into();
            return None;
        }
        let (info, cache) = (s.info.clone(), s.s.full_res.clone());
        let p = &self.profiles;
        let dates: Vec<usize> = if p.all_dates || info.t() < 2 {
            Vec::new()
        } else {
            let (a, b) = (p.from.min(p.to), p.from.max(p.to).min(info.t() - 1));
            (a..=b).collect()
        };
        let opts =
            ProfileOptions { dates, fields: p.fields.clone(), threads: p.threads.unwrap_or(self.processing_threads()) };
        let wide = p.wide;
        let name = if p.name.trim().is_empty() { format!("{vname} profiles") } else { p.name.trim().into() };
        let stem = janus_cube::stac_vrt::sanitize(&name);
        let folder = self.results_dir().join("profiles").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.profiles.status = format!("could not create {}: {e}", folder.display());
            return None;
        }
        let series = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Profiles of every line: {name}\n{}\nvector: {} ({})\nseries: {series}, {} dates, first {}\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            data.path,
            data.layer,
            info.t(),
            info.layers.first().map_or("", |l| l.path.as_str()),
            folder.display()
        ));
        {
            let mut pr = crate::provenance::Provenance::start("profiles", "profiles", "Profiles of every line");
            pr.scope = "every line of the layer".into();
            pr.threads = Some(opts.threads as i64);
            for (k, v) in [
                ("vector", serde_json::json!(data.path)),
                ("layer", serde_json::json!(data.layer)),
                ("dates", serde_json::json!(opts.dates)),
                ("fields", serde_json::json!(opts.fields.iter().map(|&k| &data.fields[k]).collect::<Vec<_>>())),
                ("wide", serde_json::json!(wide)),
            ] {
                pr.params.insert(k.into(), v);
            }
            pr.python_note =
                "shapely: line.interpolate(d) every pixel's length along it; rasterio: each date's value at the \
                 pixels"
                    .into();
            self.begin_provenance(&folder, pr);
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
        let (st, cn, c, dir) = (status.clone(), cancel.clone(), ctx.clone(), folder.clone());
        std::thread::Builder::new()
            .name("profiles".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f * 0.95, "reading the dates".into());
                    }
                    c.request_repaint();
                };
                let r = profiles(&data, &info, Some(&*cache), &opts, &progress, &cn).and_then(|p| {
                    st.lock().unwrap().message = "writing the tables".into();
                    let w = |file: &str, text: String| {
                        std::fs::write(dir.join(file), text).map_err(|e| format!("{file}: {e}"))
                    };
                    w("profiles.csv", p.long_csv())?;
                    if wide {
                        w("profiles_wide.csv", p.wide_csv())?;
                    }
                    Ok(p)
                });
                let seconds = t0.elapsed().as_secs_f64();
                let mut s = st.lock().unwrap();
                s.seconds = seconds;
                match r {
                    Ok(p) => {
                        let text = format!(
                            "{} lines, {} samples, {} dates ({} pixels in {} windows a date); {} lines off the series",
                            p.lines.len(),
                            p.samples(),
                            p.dates.len(),
                            p.pixels,
                            p.windows,
                            p.off
                        );
                        log.line(&format!("{text}; {seconds:.1} s"));
                        log.line(&format!("written {}", dir.display()));
                        log.line("done");
                        (s.state, s.progress, s.message) = (JobState::Done, 1.0, text);
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
        let folder_text = folder.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: format!("Profiles of {vname}"),
            tool: "profiles".into(),
            cube: 0,
            output_dir: folder_text.clone(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.profiles.status = format!("\"{name}\" under way (Tools > Tasks)");
        self.profiles.last = Some((status, folder_text.clone()));
        Some(folder_text)
    }
}
