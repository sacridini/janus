//! Tools > Data preparation > Harmonize sensors... (Zeit's bandpass_adjust): a series' Sentinel-2 dates taken to
//! Landsat 8's OLI by HLS's bandpass adjustment (and Landsat 7 and 4-5 by Roy et al. 2016's lines, when asked), so
//! that a series mixing sensors has no step where the sensor changes; with another open series on its grid, the two
//! joined in time. Written as a folder of COGs (a task) that opens as a new series.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Ui, vec2};
use janus_cube::CubeInfo;
use janus_zeit::harmonize::{EtmLines, Found, HarmonizeOptions, Sensor, band_roles, date_sensor, harmonize, same_grid};

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

const ETM: [(&str, &str); 3] = [
    (
        "as they are",
        "Landsat Collection 2's surface reflectance is generally used across sensors without an adjustment\n\
         (Earth Engine's FAQ on cross-sensor harmonization)",
    ),
    (
        "to OLI, Roy et al. 2016 (RMA)",
        "The reduced major axis lines of Roy et al. (2016, table 2): fitted on pre-collection data",
    ),
    (
        "to OLI, Roy et al. 2016 (OLS)",
        "The least squares lines of Roy et al. (2016, table 2): fitted on pre-collection data",
    ),
];

#[derive(Default)]
pub struct HarmonizeUi {
    pub show: bool,
    /// the series joined to the active one (its info's id)
    pub join: Option<u64>,
    /// every date's sensor, when the scenes do not say it: of the active series, of the joined one
    pub given: Option<Sensor>,
    pub join_given: Option<Sensor>,
    /// 0 as they are, 1 RMA, 2 OLS
    pub etm: usize,
    pub roi: bool,
    pub name: String,
    pub status: String,
    /// the threads its runs use, set in its window (None: the Settings')
    pub threads: Option<usize>,
    /// each series' dates' sensors as read from their scenes (by the info's id): reading them opens every file
    found: HashMap<u64, Vec<Found>>,
}

impl HarmonizeUi {
    fn etm_lines(&self) -> Option<EtmLines> {
        match self.etm {
            1 => Some(EtmLines::Rma),
            2 => Some(EtmLines::Ols),
            _ => None,
        }
    }

    fn found(&mut self, info: &CubeInfo) -> &[Found] {
        self.found.entry(info.id).or_insert_with(|| (0..info.t()).map(|t| date_sensor(info, t)).collect())
    }
}

/// "Sentinel-2A 12, Landsat 8-9 (OLI) 30, unknown 2", and whether it is HLS, and how many are unknown.
fn summary(found: &[Found]) -> (String, bool, usize) {
    let mut parts: Vec<String> = Vec::new();
    for s in Sensor::ALL {
        let n = found.iter().filter(|f| **f == Found::Sensor(s)).count();
        if n > 0 {
            parts.push(format!("{} {n}", s.label()));
        }
    }
    let unknown = found.iter().filter(|f| **f == Found::Unknown).count();
    if unknown > 0 {
        parts.push(format!("unknown {unknown}"));
    }
    (parts.join(", "), found.contains(&Found::Hls), unknown)
}

/// The sensor of every date: read from the scenes, or one chosen.
fn sensor_combo(ui: &mut Ui, id: &str, given: &mut Option<Sensor>) {
    let shown = given.map_or("from the scenes", |s| s.label());
    egui::ComboBox::from_id_salt(id).selected_text(shown).show_ui(ui, |ui| {
        ui.selectable_value(given, None, "from the scenes")
            .on_hover_text("Each date's sensor from its STAC item (a series built from a search) or its file's name");
        for s in Sensor::ALL {
            ui.selectable_value(given, Some(s), s.label());
        }
    });
}

impl State {
    pub(crate) fn harmonize_window(&mut self, ctx: &egui::Context) {
        if !self.harmonize.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Harmonize sensors", ctx, 480.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.harmonize_ui(ui)));
        self.harmonize.show = open;
    }

    /// The other open series with dates: (info, name, on the active one's grid).
    fn harmonize_others(&self, info: &CubeInfo) -> Vec<(Arc<CubeInfo>, String, bool)> {
        (0..self.layers.len())
            .filter(|&i| i != self.active)
            .filter_map(|i| {
                let other = self.layer_session(i)?.info.clone();
                (other.time_is_date && !self.layers[i].canvas).then(|| {
                    let grid = same_grid(info, &other);
                    (other, self.layers[i].name.clone(), grid)
                })
            })
            .collect()
    }

    fn harmonize_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.weak("Open a Sentinel-2 or Landsat series first (File > Search STAC catalog).");
            return;
        };
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        if !info.time_is_date {
            ui.weak(format!("\"{layer}\" has no dates."));
            return;
        }
        let others = self.harmonize_others(&info);
        if self.harmonize.join.is_some_and(|id| !others.iter().any(|o| o.0.id == id && o.2)) {
            self.harmonize.join = None;
        }
        let joined = self.harmonize.join.and_then(|id| others.iter().find(|o| o.0.id == id)).cloned();
        let roi = self.series.roi_rect.is_some();
        let r = &mut self.harmonize;
        let (found, hls, unknown) = summary(r.found(&info));
        let (roles, broad) = band_roles(&info);
        ui.label(format!(
            "{layer}: {} dates of {} band(s) ({})",
            info.t(),
            info.bands_per_date.max(1),
            info.band_names.join(", ")
        ));
        let mut ready = !hls && (unknown == 0 || r.given.is_some()) && !roles.is_empty();
        egui::Grid::new("harmonize grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
            ui.label("Sensors");
            ui.vertical(|ui| {
                ui.weak(if found.is_empty() { "none found".into() } else { found.clone() });
                sensor_combo(ui, "harmonize sensor", &mut r.given);
            });
            ui.end_row();
            ui.label("Join with");
            ui.vertical(|ui| {
                let shown = joined.as_ref().map_or("nothing (this series alone)".to_string(), |o| o.1.clone());
                egui::ComboBox::from_id_salt("harmonize join")
                    .selected_text(shown)
                    .width(crate::fit::scaled(ui, 260.0))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut r.join, None, "nothing (this series alone)");
                        for (o, name, grid) in &others {
                            let label = if *grid { name.clone() } else { format!("{name} (another grid)") };
                            ui.add_enabled_ui(*grid, |ui| {
                                ui.selectable_value(&mut r.join, Some(o.id), label).on_disabled_hover_text(
                                    "Reproject it onto this series' grid first (Tools > Data preparation >\n\
                                     Reproject, resample, clip, to a layer's grid)",
                                );
                            });
                        }
                    })
                    .response
                    .on_hover_text(
                        "Another series on this one's grid (a Landsat search with a Sentinel-2 one, say):\n\
                         both adjusted and joined in time, the reflectance bands they share named by role",
                    );
                if let Some((o, _, _)) = &joined {
                    let (f, h, u) = summary(r.found(o));
                    ui.weak(if f.is_empty() { "none found".into() } else { f });
                    sensor_combo(ui, "harmonize join sensor", &mut r.join_given);
                    if h {
                        ui.colored_label(crate::theme::warning(), "it is HLS: already harmonized");
                    }
                    if u > 0 && r.join_given.is_none() {
                        ui.colored_label(crate::theme::warning(), format!("{u} of its dates of no known sensor"));
                    }
                    ready &= !h && (u == 0 || r.join_given.is_some()) && !band_roles(o).0.is_empty();
                }
            });
            ui.end_row();
            ui.label("Landsat 7 and 4-5");
            egui::ComboBox::from_id_salt("harmonize etm").selected_text(ETM[r.etm].0).show_ui(ui, |ui| {
                for (i, (s, h)) in ETM.iter().enumerate() {
                    ui.selectable_value(&mut r.etm, i, *s).on_hover_text(*h);
                }
            });
            ui.end_row();
            ui.label("Area");
            ui.horizontal(|ui| {
                ui.radio_value(&mut r.roi, false, "the whole layer");
                ui.add_enabled_ui(roi, |ui| {
                    ui.radio_value(&mut r.roi, true, "the ROI");
                });
            });
            ui.end_row();
            ui.label("Name");
            ui.add(
                egui::TextEdit::singleline(&mut r.name)
                    .hint_text(format!("{layer} harmonized"))
                    .desired_width(crate::fit::scaled(ui, 220.0)),
            );
            ui.end_row();
        });
        if hls {
            ui.colored_label(
                crate::theme::warning(),
                "This series is HLS: its Sentinel-2 dates are already adjusted to OLI.",
            );
        } else if unknown > 0 && r.given.is_none() {
            ui.colored_label(
                crate::theme::warning(),
                format!("{unknown} of {} dates have no sensor in their scene or file name: say which it is", info.t()),
            );
        }
        if roles.is_empty() {
            ui.colored_label(
                crate::theme::warning(),
                "No band is one HLS adjusts: name them blue, green, red, NIR, SWIR 1, SWIR 2",
            );
        } else if broad {
            ui.colored_label(
                crate::theme::warning(),
                "The NIR is Sentinel-2's broad one (B08), which HLS has no line for: it stays as it is.\n\
                 Search with B8A (the narrow NIR) to adjust it.",
            );
        }
        ui.weak(
            "Sentinel-2 to Landsat 8's OLI by HLS's bandpass adjustment (Claverie et al. 2018), each unit's \
             lines; Landsat 8 and 9 as they are. Only the bandpass step of HLS, not its atmospheric correction \
             or BRDF normalization. A COG per date, in a folder that opens as a series.",
        );
        let mut own = self.harmonize.threads;
        self.threads_ui(ui, &mut own);
        self.harmonize.threads = own;
        ui.add_space(4.0);
        if ui.add_enabled(ready, egui::Button::new("▶ Run").min_size(vec2(110.0, 0.0))).clicked() {
            self.harmonize_run(ui.ctx());
        }
        if !self.harmonize.status.is_empty() {
            ui.weak(&self.harmonize.status);
        }
    }

    /// The harmonization as a task; its folder opened as a series when done.
    pub(crate) fn harmonize_run(&mut self, ctx: &egui::Context) {
        let Some(s) = self.session.as_ref() else { return };
        let info = s.info.clone();
        let layer = self.layers.get(self.active).map_or(String::new(), |l| l.name.clone());
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let joined = self
            .harmonize
            .join
            .and_then(|id| self.harmonize_others(&info).into_iter().find(|o| o.0.id == id && o.2))
            .map(|o| (o.0, o.1));
        let opts = HarmonizeOptions {
            etm: self.harmonize.etm_lines(),
            window: if self.harmonize.roi { self.series.roi_rect } else { None },
            stem: String::new(),
            threads: self.harmonize.threads.unwrap_or(self.processing_threads()),
        };
        let name = if self.harmonize.name.trim().is_empty() {
            format!("{layer} harmonized")
        } else {
            self.harmonize.name.trim().into()
        };
        let opts = HarmonizeOptions { stem: janus_cube::stac_vrt::sanitize(&name), ..opts };
        let folder = self.results_dir().join("harmonize").join(format!("{}_{}", opts.stem, crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.error_popup = Some(format!("could not create {}: {e}", folder.display()));
            return;
        }
        let (given, join_given) = (self.harmonize.given, self.harmonize.join_given);
        let title = match &joined {
            Some((_, j)) => format!("Harmonize {layer} + {j}"),
            None => format!("Harmonize {layer}"),
        };
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Harmonize sensors: {name}\n{}\nseries: {layer} ({} dates)\njoined: {}\noptions: {opts:?}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            info.t(),
            joined.as_ref().map_or("none".into(), |(i, n)| format!("{n} ({} dates)", i.t())),
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("harmonize", "harmonize", "Harmonize sensors");
            p.series = Some(crate::provenance::SeriesRecord::of(&info, &layer, &inputs));
            p.window = opts.window;
            p.threads = Some(opts.threads as i64);
            p.scope = if opts.window.is_some() { "the ROI".into() } else { "the whole layer".into() };
            p.params.insert("landsat_7_4_5".into(), serde_json::json!(ETM[self.harmonize.etm].0));
            p.params.insert("sensor".into(), serde_json::json!(given.map_or("from the scenes", |s| s.label())));
            if let Some((_, j)) = &joined {
                p.params.insert("joined".into(), serde_json::json!(j));
                p.params.insert(
                    "joined_sensor".into(),
                    serde_json::json!(join_given.map_or("from the scenes", |s| s.label())),
                );
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
        let (info2, join2, opts2) = (info.clone(), joined.as_ref().map(|j| j.0.clone()), opts.clone());
        std::thread::Builder::new()
            .name("harmonize".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                progress(0.0, "reading the dates' sensors");
                let r = (|| {
                    let mut sources = vec![janus_zeit::harmonize::source(&info2, given)?];
                    if let Some(j) = &join2 {
                        sources.push(janus_zeit::harmonize::source(j, join_given)?);
                    }
                    harmonize(&sources, &opts2, &out, &progress, &cn)
                })();
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(h) => {
                        for n in &h.notes {
                            log.line(n);
                        }
                        log.line(&format!("bands: {}", h.bands.join(", ")));
                        log.line(&format!("{} dates written, {:.1} s", h.files.len(), s.seconds));
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
            tool: "harmonize".into(),
            cube: 0,
            output_dir: folder_s.clone(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        // one series: shown as it was (its index), with the same bands; joined, the bands are by role
        let sel = (joined.is_none() && info.sel.expr.is_some()).then_some(info.sel);
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
        self.harmonize.status = format!("\"{name}\" under way: it opens as a series when ready ({})", folder.display());
    }
}
