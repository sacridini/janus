//! Analysis > Areas (IDEIAS phase 42): the area of each class at each date of the active series, or of each value
//! of a run's map (a map of change dates by year), counted at full resolution on the ROI or the whole layer in a
//! task (janus_zeit::areas: each pixel's true area), drawn as bars in ha, km² or % with a table and a CSV.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, Ui, vec2};
use egui_plot::{Bar, BarChart, Legend, Plot};
use janus_zeit::areas::{AreaOptions, AreaSource, AreaTable, areas};

use crate::app::State;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const UNITS: [&str; 3] = ["ha", "km²", "%"];

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

/// What a table counted: its title, the values' names and colours, by year.
#[derive(Clone, Default)]
pub struct AreaMeta {
    pub title: String,
    pub names: HashMap<i64, String>,
    pub colors: HashMap<i64, [u8; 3]>,
    pub by_year: bool,
    pub folder: String,
    pub seconds: f64,
}

type Pending = (Arc<Mutex<JobStatus>>, Arc<Mutex<Option<AreaTable>>>, AreaMeta);
/// The maps to count, what they are, the window.
type Counted = (Vec<AreaSource>, AreaMeta, Option<[usize; 4]>);

#[derive(Default)]
pub struct AreaUi {
    /// a run's map (its id), else the active series
    pub source: Option<u32>,
    pub roi: bool,
    pub unit: usize,
    /// the table's date (None: the one shown)
    pub date: Option<usize>,
    pending: Option<Pending>,
    pub table: Option<(Arc<AreaTable>, AreaMeta)>,
    pub status: String,
}

impl AreaMeta {
    fn name(&self, v: i64) -> String {
        self.names.get(&v).cloned().unwrap_or_else(|| v.to_string())
    }

    fn color(&self, k: usize, v: i64) -> Color32 {
        let c = self.colors.get(&v).copied().unwrap_or(PALETTE[k % PALETTE.len()]);
        Color32::from_rgb(c[0], c[1], c[2])
    }
}

impl State {
    /// Analysis > Areas in front, on a run's map (None: the active series).
    pub(crate) fn open_areas(&mut self, source: Option<u32>) {
        self.areas.source = source;
        self.analysis.show = true;
        self.analysis.tab = 4;
        self.analysis.focus = true;
    }

    /// The maps to count, the options' floor and the names and colours of their values; Err: why not.
    fn area_sources(&self) -> Result<Counted, String> {
        let roi = if self.areas.roi { self.series.roi_rect } else { None };
        if let Some(id) = self.areas.source {
            let r = self.zeit.results.iter().find(|r| r.id == id).ok_or("the map was closed")?;
            let by_year = r.unit == "year" || r.unit == "years";
            let mut meta = AreaMeta { title: r.name.clone(), by_year, ..Default::default() };
            for (k, n) in r.classes.iter().enumerate() {
                meta.names.insert(k as i64 + 1, n.clone());
            }
            // the ROI (the series' pixels) in the map's window
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
            return Ok((vec![AreaSource { path: r.path.clone(), band: 1, label: r.name.clone() }], meta, window));
        }
        let s = self.session.as_ref().ok_or("open a series")?;
        let info = &s.info;
        if info.multidim || info.remote {
            return Err("a series read over the network or from a NetCDF / Zarr variable: not counted".into());
        }
        let name = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let mut meta = AreaMeta { title: name, ..Default::default() };
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
        Ok((sources, meta, roi))
    }

    pub(crate) fn areas_ui(&mut self, ui: &mut Ui) {
        self.pump_areas();
        let maps: Vec<(u32, String)> =
            self.zeit.results.iter().filter(|r| !r.preview).map(|r| (r.id, r.name.clone())).collect();
        if self.areas.source.is_some_and(|id| !maps.iter().any(|m| m.0 == id)) {
            self.areas.source = None;
        }
        let series = self.layers.get(self.active).map_or("the active series".into(), |l| l.name.clone());
        ui.horizontal(|ui| {
            ui.label("Map");
            let shown = self
                .areas
                .source
                .and_then(|id| maps.iter().find(|m| m.0 == id))
                .map_or(format!("{series} (each date)"), |m| m.1.clone());
            egui::ComboBox::from_id_salt("areas source").selected_text(shown).width(260.0).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.areas.source, None, format!("{series} (each date)"));
                for (id, n) in &maps {
                    ui.selectable_value(&mut self.areas.source, Some(*id), n);
                }
            });
            ui.separator();
            ui.radio_value(&mut self.areas.roi, false, "whole layer");
            ui.add_enabled_ui(self.series.roi_rect.is_some(), |ui| {
                ui.radio_value(&mut self.areas.roi, true, "ROI");
            });
            ui.separator();
            let busy = self.areas.pending.is_some();
            if ui
                .add_enabled(!busy, egui::Button::new("▶ Count"))
                .on_hover_text("Counted at full resolution in a task (Tools > Tasks), each pixel with its true area")
                .clicked()
            {
                self.areas_run(ui.ctx());
            }
        });
        if !self.areas.status.is_empty() {
            ui.weak(&self.areas.status);
        }
        let Some((t, meta)) = self.areas.table.clone() else {
            ui.weak(
                "The area of each class at every date of the series, or of each value of a run's map (a map of \
                 change dates: by year), counted at full resolution: Count.",
            );
            return;
        };
        ui.separator();
        ui.horizontal(|ui| {
            ui.strong(&meta.title);
            for (k, u) in UNITS.iter().enumerate() {
                ui.radio_value(&mut self.areas.unit, k, *u);
            }
            ui.separator();
            if ui.button("Copy CSV").clicked() {
                ui.ctx().copy_text(t.csv(&|v| meta.name(v)));
            }
            if ui.button("Save CSV...").clicked()
                && let Some(p) =
                    rfd::FileDialog::new().add_filter("CSV", &["csv"]).set_file_name("areas.csv").save_file()
                && let Err(e) = std::fs::write(&p, t.csv(&|v| meta.name(v)))
            {
                self.error_popup = Some(format!("{}: {e}", p.display()));
            }
        });
        ui.weak(format!("Pixel areas: {}. {:.1} s; the CSV in {}.", t.note, meta.seconds, meta.folder));
        let unit = self.areas.unit;
        let value = |s: usize, k: usize| {
            let a = t.m2[s][k];
            match unit {
                0 => a / 1e4,
                1 => a / 1e6,
                _ => {
                    let total = t.total(s);
                    if total > 0.0 { 100.0 * a / total } else { 0.0 }
                }
            }
        };
        let h = (ui.available_height() * 0.55).clamp(140.0, 320.0);
        let n_src = t.labels.len();
        if n_src == 1 {
            // one map: a bar per value (a year, a class)
            let ticks: Vec<String> = t.values.iter().map(|&v| meta.name(v)).collect();
            Plot::new("areas one")
                .height(h)
                .y_axis_label(UNITS[unit])
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
                    let bars: Vec<Bar> = (0..t.values.len())
                        .map(|k| {
                            Bar::new(k as f64, value(0, k))
                                .width(0.8)
                                .fill(meta.color(k, t.values[k]))
                                .name(meta.name(t.values[k]))
                        })
                        .collect();
                    pu.bar_chart(BarChart::new("area", bars));
                });
        } else {
            // a bar per date, the classes stacked
            let ticks = t.labels.clone();
            Plot::new("areas dates")
                .height(h)
                .y_axis_label(UNITS[unit])
                .legend(Legend::default().position(egui_plot::Corner::RightTop).background_alpha(0.6))
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
                    let mut below: Vec<BarChart> = vec![];
                    for (k, &v) in t.values.iter().enumerate() {
                        let c = meta.color(k, v);
                        let bars: Vec<Bar> =
                            (0..n_src).map(|s| Bar::new(s as f64, value(s, k)).width(0.8).fill(c)).collect();
                        let refs: Vec<&BarChart> = below.iter().collect();
                        let chart = BarChart::new(meta.name(v), bars).color(c).stack_on(&refs);
                        below.push(chart);
                    }
                    for c in below {
                        pu.bar_chart(c);
                    }
                });
        }
        // the table of a date
        let s = if n_src == 1 {
            0
        } else {
            let s = self.areas.date.unwrap_or(self.display.t).min(n_src - 1);
            let mut pick = s;
            egui::ComboBox::from_label("date").selected_text(&t.labels[s]).show_ui(ui, |ui| {
                for (k, l) in t.labels.iter().enumerate() {
                    ui.selectable_value(&mut pick, k, l);
                }
            });
            if pick != s {
                self.areas.date = Some(pick);
            }
            pick
        };
        let total = t.total(s);
        egui::ScrollArea::vertical().id_salt("areas table").auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("areas grid").num_columns(5).striped(true).spacing(vec2(14.0, 3.0)).show(ui, |ui| {
                for h in [if meta.by_year { "year" } else { "class" }, "pixels", "ha", "km²", "%"] {
                    ui.strong(h);
                }
                ui.end_row();
                for (k, &v) in t.values.iter().enumerate() {
                    let a = t.m2[s][k];
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, meta.color(k, v));
                        ui.label(meta.name(v));
                    });
                    ui.label(t.pixels[s][k].to_string());
                    ui.label(format!("{:.2}", a / 1e4));
                    ui.label(format!("{:.4}", a / 1e6));
                    ui.label(format!("{:.2}", if total > 0.0 { 100.0 * a / total } else { 0.0 }));
                    ui.end_row();
                }
                ui.strong("total");
                ui.strong(t.pixels[s].iter().sum::<u64>().to_string());
                ui.strong(format!("{:.2}", total / 1e4));
                ui.strong(format!("{:.4}", total / 1e6));
                ui.strong("100");
                ui.end_row();
            });
        });
    }

    /// A count finished: its table shown.
    pub(crate) fn pump_areas(&mut self) {
        let Some((st, out, _)) = &self.areas.pending else { return };
        let (state, seconds, error) = {
            let s = st.lock().unwrap();
            (s.state, s.seconds, s.error.clone())
        };
        match state {
            JobState::Running => {}
            JobState::Done => {
                let table = out.lock().unwrap().take();
                let (_, _, mut meta) = self.areas.pending.take().unwrap();
                meta.seconds = seconds;
                if let Some(t) = table {
                    self.areas.status.clear();
                    self.areas.table = Some((Arc::new(t), meta));
                }
            }
            JobState::Failed => {
                self.areas.status = format!("Failed: {error}");
                self.areas.pending = None;
            }
            _ => {
                self.areas.status = "Cancelled".into();
                self.areas.pending = None;
            }
        }
    }

    pub(crate) fn areas_run(&mut self, ctx: &egui::Context) {
        let (sources, meta, window) = match self.area_sources() {
            Ok(x) => x,
            Err(e) => {
                self.areas.status = e;
                return;
            }
        };
        let stem = janus_cube::stac_vrt::sanitize(&meta.title);
        let folder = self.results_dir().join("areas").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.areas.status = format!("could not create {}: {e}", folder.display());
            return;
        }
        let opts = AreaOptions { window, floor: meta.by_year, threads: self.processing_threads() };
        let title = format!("Areas of {}", meta.title);
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Areas: {}\n{}\n{} map(s), first {}\noptions: {opts:?}\noutput: {}\n",
            meta.title,
            crate::zeit::timestamp_text(),
            sources.len(),
            sources[0].path,
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("areas", "areas", "Areas");
            p.scope = if window.is_some() { "the ROI".into() } else { "the whole layer".into() };
            p.window = window;
            p.threads = Some(opts.threads as i64);
            p.params.insert("by_year".into(), serde_json::json!(opts.floor));
            p.params.insert("maps".into(), serde_json::json!(sources.iter().map(|s| &s.path).collect::<Vec<_>>()));
            p.python_note = "terra::expanse(rast(maps), unit = \"m\", byValue = TRUE) in R".into();
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
        let (st, o, cn, c, m, dir) =
            (status.clone(), out.clone(), cancel.clone(), ctx.clone(), meta.clone(), folder.clone());
        std::thread::Builder::new()
            .name("areas".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, msg: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, msg.to_string());
                    }
                    c.request_repaint();
                };
                let r = areas(&sources, &opts, &progress, &cn);
                let seconds = t0.elapsed().as_secs_f64();
                let state = match r {
                    Ok(t) => {
                        let csv = dir.join("areas.csv");
                        let _ = std::fs::write(&csv, t.csv(&|v| m.name(v)));
                        log.line(&format!(
                            "{} map(s), {} values; {}; {seconds:.1} s",
                            t.labels.len(),
                            t.values.len(),
                            t.note
                        ));
                        log.line(&format!("written {}", csv.display()));
                        log.line("done");
                        *o.lock().unwrap() = Some(t);
                        Ok(())
                    }
                    Err(e) => Err(e),
                };
                let mut s = st.lock().unwrap();
                s.seconds = seconds;
                match state {
                    Ok(()) => (s.state, s.progress) = (JobState::Done, 1.0),
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
            tool: "areas".into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.areas.status = format!("Counting \"{}\" at full resolution...", meta.title);
        self.areas.pending = Some((status, out, AreaMeta { folder: folder.to_string_lossy().into_owned(), ..meta }));
    }
}
