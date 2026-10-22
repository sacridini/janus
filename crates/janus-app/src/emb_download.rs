//! Embeddings of the visible area or the ROI downloaded as a layer (src/app_embeddings.cpp: embeddingBounds,
//! startEmbeddingDownload, uiEmbeddingDownload): AlphaEarth and TESSERA read natively (janus_embed::download); the
//! job in Tools > Tasks with its log, its progress on the map, the layer added on top (the series being studied stays
//! active) when it is ready.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, Pos2, Rect, Ui, vec2};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_embed::download::{DownloadSpec, Source, download};

use crate::app::State;
use crate::map::body_font;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

struct SourceInfo {
    source: Source,
    label: &'static str,
    help: &'static str,
}

const SOURCES: [SourceInfo; 2] = [
    SourceInfo {
        source: Source::AlphaEarth,
        label: "AlphaEarth Foundations (64 dims)",
        help: "Google DeepMind's Satellite Embedding: 64 dimensions per 10 m pixel and year, from\n\
               optical, radar, lidar and climate data; read from its free copy on Source Cooperative.\n\
               The first download fetches its file index (78 MB, refreshed monthly).",
    },
    SourceInfo {
        source: Source::Tessera,
        label: "TESSERA (128 dims)",
        help: "TESSERA (University of Cambridge): 128 dimensions per 10 m pixel and year, from\n\
               Sentinel-1 and Sentinel-2 time series; read from its free Zarr store on Source Cooperative.",
    },
];
const RESOLUTIONS: [u32; 5] = [10, 20, 30, 60, 100];
/// what a download may write
const MAX_BYTES: f64 = 8.0 * (1u64 << 30) as f64;

/// The download's choices.
pub struct DownloadUi {
    pub source: usize,
    pub y0: i32,
    pub y1: i32,
    pub res: usize,
    /// 0 the visible area, 1 the ROI
    pub area: usize,
}

impl Default for DownloadUi {
    fn default() -> Self {
        DownloadUi { source: 0, y0: 2023, y1: 2024, res: 0, area: 0 }
    }
}

/// A download under way: its job (also in Tasks), its folder, whether it was opened.
pub struct EmbDownload {
    pub title: String,
    pub status: Arc<Mutex<JobStatus>>,
    pub started: Instant,
    pub folder: String,
    pub opened: bool,
    pub ended: Option<Instant>,
}

/// Zeit's cache of embeddings (the AlphaEarth index is shared with it).
fn cache_dir() -> std::path::PathBuf {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(std::path::PathBuf::from);
    home.map_or_else(
        || crate::settings::app_data_dir().join("embeddings-cache"),
        |h| h.join(".cache").join("zeit").join("embeddings"),
    )
}

impl State {
    /// The active layer's visible area (area 0) or ROI (area 1) in longitude and latitude, and its size in metres.
    pub(crate) fn embedding_bounds(&self, area: usize) -> Result<([f64; 4], f64, f64), String> {
        let s = self.session.as_ref().ok_or("open a series first")?;
        let info = &s.info;
        if info.crs_wkt.is_empty() || info.geo_transform.is_none() {
            return Err("the active layer has no CRS: the area cannot be placed on the Earth".into());
        }
        let win = if area == 1 {
            let r = self.series.roi_rect.ok_or("draw an ROI first (Shift + drag on the map)")?;
            r.map(|v| v as f64)
        } else {
            let w = self.embedding_visible_window_active().ok_or("nothing of the layer is visible")?;
            w.map(|v| v as f64)
        };
        let mut src = SpatialRef::from_wkt(&info.crs_wkt)
            .map_err(|e| format!("the active layer's CRS is not understood ({e})"))?;
        let mut dst = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
        src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let ct = CoordTransform::new(&src, &dst)
            .map_err(|_| "no transformation from the active layer's CRS to longitude and latitude")?;
        let mut ll = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let n = 16; // points along each edge: a reprojected rectangle bulges
        for i in 0..=n {
            let f = i as f64 / n as f64;
            for (px, py) in [
                (win[0], win[1] + f * (win[3] - win[1])),
                (win[2], win[1] + f * (win[3] - win[1])),
                (win[0] + f * (win[2] - win[0]), win[1]),
                (win[0] + f * (win[2] - win[0]), win[3]),
            ] {
                let Some((gx, gy)) = info.pixel_to_geo(px, py) else { continue };
                let (mut x, mut y, mut z) = ([gx], [gy], [0.0]);
                if ct.transform_coords(&mut x, &mut y, &mut z).is_err() || !x[0].is_finite() || !y[0].is_finite() {
                    continue;
                }
                ll = [ll[0].min(x[0]), ll[1].min(y[0]), ll[2].max(x[0]), ll[3].max(y[0])];
            }
        }
        if ll[2] <= ll[0] || ll[3] <= ll[1] {
            return Err("the area could not be transformed to longitude and latitude".into());
        }
        let lat = 0.5 * (ll[1] + ll[3]) * std::f64::consts::PI / 180.0;
        Ok((ll, (ll[2] - ll[0]) * 111_320.0 * lat.cos(), (ll[3] - ll[1]) * 110_574.0))
    }

    /// The active layer's pixels on screen (x0, y0, x1, y1).
    pub(crate) fn embedding_visible_window_active(&self) -> Option<[usize; 4]> {
        let s = self.session.as_ref()?;
        let g = self.grid()?;
        let m = &self.map;
        let [cw, ch] = m.canvas.map(f64::from);
        let (ax0, ay0) = ((m.center[0] - cw / 2.0 / m.scale) * g.fx, (m.center[1] - ch / 2.0 / m.scale) * g.fy);
        let (ax1, ay1) = ((m.center[0] + cw / 2.0 / m.scale) * g.fx, (m.center[1] + ch / 2.0 / m.scale) * g.fy);
        let c = |v: f64, n: usize| v.clamp(0.0, n as f64) as usize;
        let w = [
            c(ax0.floor(), s.info.width),
            c(ay0.floor(), s.info.height),
            c(ax1.ceil(), s.info.width),
            c(ay1.ceil(), s.info.height),
        ];
        (w[2] > w[0] && w[3] > w[1]).then_some(w)
    }

    /// Starts the download of the panel's choices: a job of its own (Tools > Tasks), into the results folder.
    pub fn start_embedding_download(&mut self, ctx: &egui::Context) {
        let ui = &self.emb_download;
        let src = &SOURCES[ui.source.min(1)];
        let (bounds, w_m, h_m) = match self.embedding_bounds(ui.area) {
            Ok(b) => b,
            Err(e) => {
                self.error_popup = Some(e);
                return;
            }
        };
        let years: Vec<i32> = (ui.y0..=ui.y1).collect();
        let res = RESOLUTIONS[ui.res.min(RESOLUTIONS.len() - 1)];
        let id = src.source.id();
        let out = self.results_dir().join("embeddings").join(format!(
            "{id}_{}-{}_{}",
            ui.y0,
            ui.y1,
            crate::zeit::timestamp()
        ));
        if let Err(e) = std::fs::create_dir_all(&out) {
            self.error_popup = Some(format!("could not create {}: {e}", out.display()));
            return;
        }
        let name = if src.source == Source::Tessera { "TESSERA" } else { "AlphaEarth" };
        let title =
            format!("Embeddings {name} {}-{} ({:.1} x {:.1} km, {res} m)", ui.y0, ui.y1, w_m / 1000.0, h_m / 1000.0);
        let spec = DownloadSpec {
            source: src.source,
            years,
            bounds,
            res: (res != 10).then_some(f64::from(res)),
            out_dir: out.clone(),
            cache_dir: cache_dir(),
        };
        let log = JobLog { path: out.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} job: {title}\nsource: {id}\nyears: {:?}\nbounds: {bounds:?}\nres: {res} m\noutput: {}\n",
            crate::zeit::timestamp_text(),
            spec.years,
            out.display()
        ));
        let log_path = log.path.to_string_lossy().into_owned();
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
            .name("embeddings download".into())
            .spawn(move || {
                let t0 = Instant::now();
                // progress to the log every 5 s or 10%
                let logged = Mutex::new((-1.0, -1.0));
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    let now = t0.elapsed().as_secs_f64();
                    let mut l = logged.lock().unwrap();
                    if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                        log.line(&format!("{:3.0}%  {m}", f * 100.0));
                        *l = (f, now);
                    }
                    c.request_repaint();
                };
                let r = download(&spec, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(files) => {
                        for f in &files {
                            log.line(&format!("output: {}", f.display()));
                        }
                        log.line("done");
                        (s.state, s.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        log.line("failed");
                        (s.state, s.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        let folder = out.to_string_lossy().into_owned();
        // also listed in Tools > Tasks (with its log)
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "embeddings".into(),
            cube: 0,
            output_dir: folder.clone(),
            log_path,
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true, // nothing for the Zeit results to load
        });
        self.emb_downloads.push(EmbDownload {
            title: name.into(),
            status,
            started: Instant::now(),
            folder,
            opened: false,
            ended: None,
        });
    }

    /// Finished downloads: their folder added as a layer on top (the series being studied stays active); gone from
    /// the map's notes a while after (Tasks keeps them).
    pub(crate) fn pump_embedding_downloads(&mut self, ctx: &egui::Context) {
        let mut open = None;
        for d in &mut self.emb_downloads {
            let st = d.status.lock().unwrap().state;
            if st != JobState::Running && d.ended.is_none() {
                d.ended = Some(Instant::now());
            }
            if st == JobState::Done && !d.opened && open.is_none() && self.opening.is_none() {
                d.opened = true;
                open = Some(d.folder.clone());
            }
        }
        if let Some(folder) = open {
            if !self.layers.is_empty() {
                self.zeit.keep_active = Some(self.active);
            }
            self.open_inputs(vec![folder], true, ctx);
        }
        self.emb_downloads.retain(|d| {
            let running = d.status.lock().unwrap().state == JobState::Running;
            running || d.ended.is_none_or(|t| !d.opened && t.elapsed().as_secs_f64() < 20.0)
        });
        if self.emb_downloads.iter().any(|d| d.status.lock().unwrap().state == JobState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// The downloads' progress, top right of the map.
    pub(crate) fn draw_download_notes(&self, painter: &egui::Painter, rect: Rect, y0: f32) {
        let mut y = y0;
        let bw = 300.0;
        let font = body_font(painter);
        for d in &self.emb_downloads {
            let s = d.status.lock().unwrap();
            let el = d.started.elapsed().as_secs_f64();
            let (frac, text, col) = match s.state {
                JobState::Running => (
                    s.progress as f32,
                    format!("{}: {} ({el:.0} s)", d.title, if s.message.is_empty() { "starting" } else { &s.message }),
                    Color32::from_rgba_unmultiplied(60, 160, 110, 230),
                ),
                JobState::Failed => (
                    1.0,
                    format!("{} failed: {} (Tools > Tasks > Log)", d.title, s.error),
                    Color32::from_rgba_unmultiplied(170, 50, 50, 220),
                ),
                JobState::Done if !d.opened => {
                    (1.0, format!("{}: done, opening...", d.title), Color32::from_rgba_unmultiplied(60, 160, 110, 230))
                }
                _ => continue,
            };
            let p0 = Pos2::new(rect.right() - bw - 10.0, rect.top() + y);
            let r = Rect::from_min_size(p0, vec2(bw, 18.0));
            painter.rect_filled(r, 3.0, Color32::from_black_alpha(170));
            painter.rect_filled(Rect::from_min_size(p0, vec2(bw * frac.clamp(0.0, 1.0), 18.0)), 3.0, col);
            painter.with_clip_rect(r).text(
                p0 + vec2(6.0, 2.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                Color32::WHITE,
            );
            y += 24.0;
        }
    }

    /// The download's choices: the product, the years, the cells, the area; its size; the button.
    pub(crate) fn embedding_download_ui(&mut self, ui: &mut Ui) {
        let w = ui.available_width();
        let d = &mut self.emb_download;
        let cur = &SOURCES[d.source.min(1)];
        egui::ComboBox::from_id_salt(("emb dl source", ui.id()))
            .width(w)
            .selected_text(cur.label)
            .show_ui(ui, |ui| {
                for (i, s) in SOURCES.iter().enumerate() {
                    ui.selectable_value(&mut d.source, i, s.label).on_hover_text(s.help);
                }
            })
            .response
            .on_hover_text(cur.help);
        ui.horizontal(|ui| {
            ui.label("from");
            ui.add(egui::DragValue::new(&mut d.y0).range(2017..=2025));
            ui.label("to");
            ui.add(egui::DragValue::new(&mut d.y1).range(2017..=2025)).on_hover_text(
                "Years to download (one file each). Both products start in 2017;\n\
                 a year not published yet makes the download fail.",
            );
            d.y1 = d.y1.max(d.y0);
            let label = |i: usize| {
                if i == 0 { "10 m (native)".to_string() } else { format!("{} m (mean of the cells)", RESOLUTIONS[i]) }
            };
            egui::ComboBox::from_id_salt(("emb dl res", ui.id()))
                .selected_text(format!("{} m", RESOLUTIONS[d.res]))
                .show_ui(ui, |ui| {
                    for i in 0..RESOLUTIONS.len() {
                        ui.selectable_value(&mut d.res, i, label(i));
                    }
                })
                .response
                .on_hover_text(
                    "Cell size. Coarser cells are the mean of the 10 m embeddings under them\n\
                 (how embeddings are taken to a larger scale): fewer bytes for large areas.",
                );
        });
        ui.horizontal(|ui| {
            ui.radio_value(&mut d.area, 0, "Visible area");
            ui.radio_value(&mut d.area, 1, "ROI");
        });
        let (area, res, years, dims) = (d.area, RESOLUTIONS[d.res], (d.y1 - d.y0 + 1) as f64, cur.source.dims() as f64);
        let bounds = self.embedding_bounds(area);
        let mut block = None;
        match &bounds {
            Ok((ll, wm, hm)) => {
                let (px, py) = ((wm / f64::from(res)).ceil(), (hm / f64::from(res)).ceil());
                let bytes = px * py * dims * 2.0 * years; // Int16
                ui.weak(format!(
                    "{:.1} x {:.1} km, {px:.0} x {py:.0} px, {years:.0} year{}: ~{:.0} MB",
                    wm / 1000.0,
                    hm / 1000.0,
                    if years > 1.0 { "s" } else { "" },
                    bytes / 1048576.0
                ))
                .on_hover_text(format!(
                    "Int16 GeoTIFFs, one per year (compressed on disk; the download is about as large).\n\
                     {:.4}, {:.4} to {:.4}, {:.4} (longitude, latitude)",
                    ll[0], ll[1], ll[2], ll[3]
                ));
                if bytes > MAX_BYTES {
                    block = Some("too large: zoom in, use an ROI or coarser cells".to_string());
                }
            }
            Err(why) => {
                ui.colored_label(theme::warning(), why);
                block = Some(String::new());
            }
        }
        let go = ui
            .add_enabled(block.is_none(), egui::Button::new("Add embeddings").min_size(vec2(ui.available_width(), 0.0)))
            .on_hover_text(format!(
                "Downloads in the background (progress on the map and in Tools > Tasks);\n\
                 the layer is added on top when it is ready. Saved under {}",
                self.results_dir().join("embeddings").display()
            ))
            .clicked();
        if let Some(b) = block.filter(|b| !b.is_empty()) {
            ui.colored_label(theme::warning(), b);
        }
        if go {
            self.start_embedding_download(ui.ctx());
        }
    }
}
