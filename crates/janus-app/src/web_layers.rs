//! Web layers (IDEIAS: camadas OGC): File > Add web layer... (also the Layers panel's + Add and the palette). A
//! server's URL (or a preset: INPE's TerraBrasilis, IBGE), its service found or chosen (WMS, WMTS, WFS, OGC API -
//! Features), its layers listed with their titles and abstracts and a filter, one chosen (janus_geo::ogc).
//!
//! A map (WMS, WMTS) becomes a layer of the drawing order drawn as the basemap is (janus_geo::basemap: tiles in Web
//! Mercator around the active layer, GDAL's WMS driver, a disk cache), translucent if asked, its time tied to the
//! series' date if it has one, its credit on the map and in the exports. Features (WFS, OGC API) are fetched for
//! the view (or the ROI, the active layer, the whole layer) in a task of Tools > Tasks, paged, up to a cap (said
//! when reached), into a GeoPackage in the cache (the same request is read again at once) that opens as a vector
//! layer: every vector tool works on it, and Export as... keeps it. Both are kept in projects (the request, fetched
//! again when its GeoPackage is gone).

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{RichText, Ui, vec2};
use janus_geo::basemap::Basemap;
use janus_geo::ogc::{self, Capabilities, FeatureRequest, Fetched, MapLayerSpec, Service};
use janus_gpu::{Gpu, GpuTexture, MapFrame, Warp};
use janus_session::ViewRect;
use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

/// A GeoPackage fetched this long ago is read again rather than fetched (s).
const CACHE_AGE_S: u64 = 12 * 3600;

/// The web layers' folder in the cache.
pub fn cache_dir() -> PathBuf {
    janus_cache::overview::default_cache_dir().join("web")
}

/// Which time a map with a time dimension shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeMode {
    /// the server's default
    #[default]
    Default,
    /// the series' date (the last listed time not after it)
    SeriesDate,
    /// from the first time to the series' date (accumulated: every alert up to it)
    UpToSeriesDate,
    /// a time typed
    Fixed,
}

impl TimeMode {
    const ALL: [TimeMode; 4] = [TimeMode::Default, TimeMode::SeriesDate, TimeMode::UpToSeriesDate, TimeMode::Fixed];

    fn name(self) -> &'static str {
        match self {
            TimeMode::Default => "The server's default",
            TimeMode::SeriesDate => "The series' date",
            TimeMode::UpToSeriesDate => "Up to the series' date",
            TimeMode::Fixed => "This time:",
        }
    }
}

/// A map from a server (WMS, WMTS) in the drawing order.
pub struct WebMap {
    pub id: u32,
    pub name: String,
    pub spec: MapLayerSpec,
    /// the layer's time dimension as listed, and which time is shown
    pub times: Vec<String>,
    pub time_mode: TimeMode,
    pub fixed_time: String,
    pub visible: bool,
    pub opacity: f32,
    /// its place in the drawing order (order.rs)
    pub z: u64,
    pub b: Option<Basemap<GpuTexture>>,
    /// why it is not drawn
    pub why: String,
    /// the layer it could not be placed on (not tried again for it)
    failed_for: u64,
}

impl WebMap {
    /// The TIME asked for at the series' date `date` (YYYY-MM-DD, "" none).
    fn time_for(&self, date: &str) -> String {
        match self.time_mode {
            TimeMode::Default => String::new(),
            TimeMode::Fixed => self.fixed_time.trim().to_string(),
            _ if date.is_empty() => String::new(),
            TimeMode::SeriesDate => {
                if self.times.is_empty() {
                    date.to_string()
                } else {
                    ogc::time_at(&self.times, date).unwrap_or_default()
                }
            }
            TimeMode::UpToSeriesDate => {
                let first = self.times.first().map_or("1900-01-01", |t| t.split('/').next().unwrap_or(t));
                let first = first.get(..10).unwrap_or(first);
                if first > date { String::new() } else { format!("{first}/{date}") }
            }
        }
    }

    /// Its tiles' GPU resources go now, the rest (its pool, which may wait for a request) in a thread.
    fn retire(&mut self) {
        if let Some(mut b) = self.b.take() {
            b.release_gpu();
            std::thread::spawn(move || drop(b));
        }
    }

    pub fn attribution(&self) -> String {
        ogc::map_source(&self.spec).attribution
    }
}

/// A vector layer's server: the request it was fetched by.
#[derive(Clone, Debug)]
pub struct WebSource {
    pub path: String,
    pub req: FeatureRequest,
    pub name: String,
    pub features: usize,
    pub capped: bool,
    /// the vector layer was given its name
    pub(crate) named: bool,
}

/// A fetch under way: its task, its request, where it goes, the result.
struct PendingFetch {
    status: Arc<Mutex<JobStatus>>,
    result: Arc<Mutex<Option<Fetched>>>,
    req: FeatureRequest,
    name: String,
}

/// A layer's fields (name, type) as read from its server.
type FieldsRead = Result<Vec<(String, String)>, String>;

/// The dialog's state.
#[derive(Default)]
pub struct WebUi {
    pub show: bool,
    pub url: String,
    /// None: found from the URL
    pub service: Option<Service>,
    pub caps: Option<Arc<Capabilities>>,
    loading: Option<Receiver<Result<Capabilities, String>>>,
    pub error: String,
    pub filter: String,
    /// the layers shown by the filter (indices), and the filter they are for
    shown: Vec<usize>,
    shown_for: (String, usize),
    pub picked: Option<usize>,
    // a map's options
    pub style: String,
    pub format: String,
    pub transparent: bool,
    pub time_mode: TimeMode,
    pub fixed_time: String,
    pub opacity: f32,
    // features' options: 0 the visible area, 1 the ROI, 2 the active layer, 3 the whole layer
    pub area: usize,
    pub max_features: usize,
    pub date_field: String,
    pub from: String,
    pub to: String,
    pub where_sql: String,
    /// the picked layer's fields (read in the background)
    fields: Option<(String, Receiver<FieldsRead>)>,
    pub field_list: Vec<(String, String)>,
    pub status: String,
}

#[derive(Default)]
pub struct WebState {
    pub ui: WebUi,
    pub maps: Vec<WebMap>,
    next_id: u32,
    pending: Vec<PendingFetch>,
    pub sources: Vec<WebSource>,
    /// --web-layer applied
    pub cli_done: bool,
    /// what the WMS maps answered at the last click (the Map info window)
    pub info: Option<WebInfo>,
}

/// A WMS map's GetFeatureInfo answer (None: still asked).
pub type InfoAnswer = Option<Result<Vec<ogc::FeatureInfo>, String>>;

/// GetFeatureInfo at a click on the map: what each visible WMS map that answers it says is there.
pub struct WebInfo {
    /// the longitude and latitude clicked
    pub at: (f64, f64),
    /// per map asked: its name and its answer (None: still asked)
    pub answers: Vec<(String, InfoAnswer)>,
    rx: Receiver<(usize, Result<Vec<ogc::FeatureInfo>, String>)>,
}

impl WebInfo {
    /// Every map has answered.
    pub fn done(&self) -> bool {
        self.answers.iter().all(|a| a.1.is_some())
    }
}

impl WebUi {
    /// No capabilities being read.
    pub fn loading_done(&self) -> bool {
        self.loading.is_none()
    }
}

impl WebState {
    pub fn fetching(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn source_of(&self, path: &str) -> Option<&WebSource> {
        self.sources.iter().find(|s| s.path == path)
    }

    /// The credits of the maps shown.
    pub fn credits(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for m in self.maps.iter().filter(|m| m.visible && m.b.as_ref().is_some_and(|b| b.has_map())) {
            let a = m.attribution();
            if !out.contains(&a) {
                out.push(a);
            }
        }
        out
    }
}

/// Unix seconds as YYYY-MM-DD.
fn ymd(secs: f64) -> String {
    let days = (secs / 86400.0).floor() as i64;
    // Howard Hinnant's civil_from_days
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Kept in a project: a map layer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebMapEntry {
    pub service: String,
    pub url: String,
    pub version: String,
    pub layer: String,
    pub style: String,
    pub format: String,
    pub transparent: bool,
    pub srs: String,
    pub tiles: String,
    pub max_zoom: i32,
    pub title: String,
    pub attribution: String,
    /// GetFeatureInfo's format ("" not queried) and URL
    pub info_format: String,
    pub info_url: String,
    pub times: Vec<String>,
    pub time_mode: TimeMode,
    pub fixed_time: String,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
}

/// Kept in a project with a vector layer fetched from a server: its request.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct WebFeatureEntry {
    pub service: String,
    pub url: String,
    pub layer: String,
    pub bbox: Option<[f64; 4]>,
    pub date_field: String,
    pub from: String,
    pub to: String,
    pub filter: String,
    pub max_features: usize,
}

impl WebFeatureEntry {
    pub fn of(r: &FeatureRequest) -> Self {
        WebFeatureEntry {
            service: r.service.id().into(),
            url: r.url.clone(),
            layer: r.layer.clone(),
            bbox: r.bbox,
            date_field: r.date_field.clone(),
            from: r.from.clone(),
            to: r.to.clone(),
            filter: r.filter.clone(),
            max_features: r.max_features,
        }
    }

    pub fn request(&self) -> FeatureRequest {
        FeatureRequest {
            service: Service::from_id(&self.service).unwrap_or(Service::Wfs),
            url: self.url.clone(),
            layer: self.layer.clone(),
            bbox: self.bbox,
            date_field: self.date_field.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            filter: self.filter.clone(),
            max_features: self.max_features,
            capabilities: None,
        }
    }
}

/// Draws a web map's tiles of the view (in the active layer's source pixels) at its opacity, each place once.
pub(crate) fn draw_web_map(
    gpu: &Gpu,
    f: &mut MapFrame,
    w: &mut WebMap,
    view: &ViewRect,
    to_px: &dyn Fn([f64; 4]) -> [f32; 4],
) {
    if !w.visible {
        return;
    }
    let Some(b) = w.b.as_mut().filter(|b| b.has_map()) else { return };
    let v = [view.x0, view.y0, view.x1, view.y1];
    b.update(&v, view.scale);
    let alpha = w.opacity;
    b.for_each_visible_once(&v, view.scale, |tex, grid, p| {
        let warp = Warp { grid: grid.clone(), quad: p.warp_quad, src: p.warp_src };
        gpu.draw_image(f, tex, to_px(p.quad), alpha, Some(&warp), false);
    });
}

impl State {
    /// File > Add web layer...
    pub fn open_web_dialog(&mut self) {
        let u = &mut self.web.ui;
        u.show = true;
        if u.max_features == 0 {
            u.max_features = ogc::DEFAULT_MAX_FEATURES;
            u.opacity = 1.0;
            u.transparent = true;
        }
    }

    /// The series' date shown (YYYY-MM-DD), "" when its steps are not dates.
    fn web_series_date(&self) -> String {
        let Some(s) = &self.session else { return String::new() };
        if !s.info.time_is_date {
            return String::new();
        }
        s.info.layers.get(self.display.t).map_or(String::new(), |l| ymd(l.time))
    }

    /// Reads a server's capabilities in the background.
    pub fn web_connect(&mut self, ctx: &egui::Context) {
        let u = &mut self.web.ui;
        let (url, service) = (u.url.trim().to_string(), u.service);
        let (tx, rx) = channel();
        let c = ctx.clone();
        std::thread::Builder::new()
            .name("ogc-capabilities".into())
            .spawn(move || {
                let _ = tx.send(ogc::capabilities(&url, service));
                c.request_repaint();
            })
            .ok();
        u.loading = Some(rx);
        u.caps = None;
        u.picked = None;
        u.error.clear();
        u.status.clear();
        u.shown_for = (String::new(), usize::MAX);
    }

    /// The box of the features asked for, in longitude and latitude (None: the whole layer).
    fn web_area(&self) -> Result<Option<[f64; 4]>, String> {
        match self.web.ui.area {
            0 => self.embedding_bounds(0).map(|b| Some(b.0)),
            1 => self.embedding_bounds(1).map(|b| Some(b.0)),
            2 => {
                let s = self.session.as_ref().ok_or("open a series first")?;
                let (w, h) = (s.info.width as f64, s.info.height as f64);
                lonlat_box(&s.info, [0.0, 0.0, w, h]).map(Some)
            }
            _ => Ok(None),
        }
    }

    /// The dialog's request for the layer picked.
    pub(crate) fn web_request(&self) -> Result<(FeatureRequest, String), String> {
        let u = &self.web.ui;
        let caps = u.caps.as_ref().ok_or("connect to a server first")?;
        let l = u.picked.and_then(|k| caps.layers.get(k)).ok_or("choose a layer")?;
        let req = FeatureRequest {
            service: caps.service,
            url: caps.url.clone(),
            layer: l.name.clone(),
            bbox: self.web_area()?,
            date_field: u.date_field.trim().to_string(),
            from: u.from.trim().to_string(),
            to: u.to.trim().to_string(),
            filter: u.where_sql.trim().to_string(),
            max_features: u.max_features,
            capabilities: caps.raw_caps(),
        };
        Ok((req, l.label().to_string()))
    }

    /// Fetches features into a GeoPackage (the cache's, or `out`) in a task; it opens as a vector layer when done.
    pub fn web_fetch(&mut self, req: FeatureRequest, name: String, out: Option<PathBuf>, ctx: &egui::Context) {
        let dir = cache_dir();
        let _ = std::fs::create_dir_all(dir.join("logs"));
        let stamp = crate::zeit::timestamp();
        let log =
            JobLog { path: dir.join("logs").join(format!("{}_{stamp}.log", req.out_layer())), t0: Instant::now() };
        log.write(&format!("=== Fetch {name}\n{}\n{}\n", crate::zeit::timestamp_text(), req.describe()));
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let result = Arc::new(Mutex::new(None));
        let (st, cn, c, res, r2, lp) =
            (status.clone(), cancel.clone(), ctx.clone(), result.clone(), req.clone(), log.path.clone());
        std::thread::Builder::new()
            .name("ogc-features".into())
            .spawn(move || {
                let log = JobLog { path: lp, t0: Instant::now() };
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let t0 = Instant::now();
                let r = match &out {
                    Some(p) => ogc::fetch_features(&r2, p, &progress, &cn),
                    None => ogc::fetch_cached(&r2, &dir, CACHE_AGE_S, &progress, &cn),
                };
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(f) => {
                        log.line(&format!(
                            "{} features{}{} into {} ({:.1} s)",
                            f.features,
                            if f.capped { ", capped" } else { "" },
                            if f.cached { ", from the cache" } else { "" },
                            f.path.display(),
                            s.seconds
                        ));
                        s.message = format!("{} features{}", f.features, if f.capped { " (capped)" } else { "" });
                        (s.state, s.progress) = (JobState::Done, 1.0);
                        *res.lock().unwrap() = Some(f);
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
            title: format!("Fetch {name}"),
            tool: "web".into(),
            cube: 0,
            output_dir: cache_dir().to_string_lossy().into_owned(),
            log_path: log.path.to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.web.pending.push(PendingFetch { status, result, req, name: name.clone() });
        self.web.ui.status = format!("Fetching \"{name}\" (Tools > Tasks): it opens as a vector layer when ready");
    }

    /// A map layer added on top of the drawing order.
    pub fn add_web_map(
        &mut self,
        spec: MapLayerSpec,
        times: Vec<String>,
        mode: TimeMode,
        fixed: String,
        opacity: f32,
    ) -> u32 {
        self.web.next_id += 1;
        let id = self.web.next_id;
        let z = self.take_z();
        let name = if spec.title.is_empty() { spec.layer.clone() } else { spec.title.clone() };
        self.web.maps.push(WebMap {
            id,
            name,
            spec,
            times,
            time_mode: mode,
            fixed_time: fixed,
            visible: true,
            opacity: opacity.clamp(0.0, 1.0),
            z,
            b: None,
            why: String::new(),
            failed_for: 0,
        });
        self.map.dirty = true;
        id
    }

    /// The dialog's map layer for the layer picked.
    pub(crate) fn web_add_picked_map(&mut self) -> Result<u32, String> {
        let u = &self.web.ui;
        let caps = u.caps.as_ref().ok_or("connect to a server first")?;
        let l = u.picked.and_then(|k| caps.layers.get(k)).ok_or("choose a layer")?.clone();
        let (tiles, max_zoom) = match caps.service {
            Service::Wmts => l.tiles.clone().ok_or("this layer has no Web Mercator tiles: use the server's WMS")?,
            _ => (String::new(), 0),
        };
        let format = if !u.format.is_empty() {
            u.format.clone()
        } else if caps.service == Service::Wmts {
            l.formats.iter().find(|f| f.contains("png")).or(l.formats.first()).cloned().unwrap_or("image/png".into())
        } else {
            caps.formats
                .iter()
                .find(|f| *f == "image/png")
                .or(caps.formats.first())
                .cloned()
                .unwrap_or("image/png".into())
        };
        let spec = MapLayerSpec {
            service: caps.service,
            url: caps.url.clone(),
            version: if caps.version.starts_with("1.3") { "1.3.0".into() } else { "1.1.1".into() },
            layer: l.name.clone(),
            style: if u.style.is_empty() { l.styles.first().cloned().unwrap_or_default() } else { u.style.clone() },
            format,
            transparent: u.transparent,
            time: String::new(),
            srs: l.web_mercator_code().into(),
            tiles,
            max_zoom,
            title: l.label().to_string(),
            attribution: if l.attribution.is_empty() {
                ogc::preset_attribution(&caps.url).map(String::from).unwrap_or_default()
            } else {
                l.attribution.clone()
            },
            info_format: if caps.service == Service::Wms && l.queryable {
                ogc::pick_info_format(&caps.info_formats)
            } else {
                String::new()
            },
            info_url: caps.info_url.clone(),
        };
        let (mode, fixed, op) = (u.time_mode, u.fixed_time.clone(), u.opacity);
        Ok(self.add_web_map(spec, l.times.clone(), mode, fixed, op))
    }

    pub fn remove_web_map(&mut self, k: usize) {
        if k < self.web.maps.len() {
            let mut m = self.web.maps.remove(k);
            m.retire();
            self.map.dirty = true;
        }
    }

    /// A click on the map at the active layer's source pixel (x, y): the visible WMS maps that answer GetFeatureInfo
    /// asked what is there, each in a thread (the Map info window); false when no map is asked.
    pub(crate) fn web_info_at(&mut self, x: f64, y: f64, ctx: &egui::Context) -> bool {
        let maps: Vec<(String, MapLayerSpec)> = self
            .web
            .maps
            .iter()
            .filter(|m| m.visible && m.spec.service == Service::Wms && !m.spec.info_format.is_empty())
            .map(|m| (m.name.clone(), m.spec.clone()))
            .collect();
        if maps.is_empty() {
            return false;
        }
        let (Some(info), Some(g)) = (self.session.as_ref().map(|s| s.info.clone()), self.grid()) else { return false };
        let scale = self.map.scale;
        let mut lon_lat = |px: f64, py: f64| {
            let geo = info.pixel_to_geo(px, py)?;
            self.status_lon_lat(geo)
        };
        let Some(at) = lon_lat(x, y) else { return false };
        // the Web Mercator metres of a screen pixel there (the server's tolerance is in pixels of its image)
        let m0 = ogc::mercator(at.0, at.1);
        let dist = |p: Option<(f64, f64)>| {
            let m = p.map(|(lo, la)| ogc::mercator(lo, la))?;
            Some(((m.0 - m0.0).powi(2) + (m.1 - m0.1).powi(2)).sqrt())
        };
        let per_px = match (dist(lon_lat(x + 1.0, y)), dist(lon_lat(x, y + 1.0))) {
            (Some(a), Some(b)) => (a + b) / 2.0,
            (Some(a), None) | (None, Some(a)) => a,
            _ => return false,
        };
        let metres = per_px * g.fx / scale.max(1e-12);
        let (tx, rx) = channel();
        for (k, (_, spec)) in maps.iter().enumerate() {
            let (tx, spec, c) = (tx.clone(), spec.clone(), ctx.clone());
            std::thread::Builder::new()
                .name("ogc-info".into())
                .spawn(move || {
                    let _ = tx.send((k, ogc::feature_info(&spec, at.0, at.1, metres)));
                    c.request_repaint();
                })
                .ok();
        }
        self.web.info = Some(WebInfo { at, answers: maps.into_iter().map(|(n, _)| (n, None)).collect(), rx });
        true
    }

    /// The Map info window: what the WMS maps answered at the last click.
    fn web_info_window(&mut self, ctx: &egui::Context) {
        let Some(inf) = &self.web.info else { return };
        let mut open = true;
        egui::Window::new("Map info").open(&mut open).resizable(true).default_width(320.0).show(ctx, |ui| {
            ui.label(RichText::new(format!("{:.5}, {:.5}", inf.at.1, inf.at.0)).weak())
                .on_hover_text("The latitude and longitude clicked: what the web maps (WMS) say is there");
            egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                for (k, (name, a)) in inf.answers.iter().enumerate() {
                    ui.strong(name);
                    match a {
                        None => {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("asking the server...");
                            });
                        }
                        Some(Err(e)) => {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        }
                        Some(Ok(v)) if v.is_empty() => {
                            ui.label(RichText::new("nothing here").weak());
                        }
                        Some(Ok(v)) => {
                            for (i, f) in v.iter().enumerate() {
                                if !f.id.is_empty() {
                                    ui.label(RichText::new(&f.id).weak());
                                }
                                egui::Grid::new(("web info", k, i)).num_columns(2).striped(true).show(ui, |ui| {
                                    for (key, val) in &f.fields {
                                        ui.label(key);
                                        ui.add(egui::Label::new(val).wrap());
                                        ui.end_row();
                                    }
                                });
                            }
                        }
                    }
                    ui.add_space(6.0);
                }
            });
        });
        if !open {
            self.web.info = None;
        }
    }

    /// Once per frame: the dialog's capabilities, the fetches done (their vector layers), each map's tiles.
    pub(crate) fn pump_web(&mut self, ctx: &egui::Context) {
        // GetFeatureInfo's answers
        if let Some(inf) = &mut self.web.info {
            while let Ok((k, r)) = inf.rx.try_recv() {
                if let Some(a) = inf.answers.get_mut(k) {
                    a.1 = Some(r);
                }
            }
        }
        // the capabilities read
        let u = &mut self.web.ui;
        if let Some(rx) = &u.loading {
            match rx.try_recv() {
                Ok(Ok(c)) => {
                    u.status = format!(
                        "{} {}: {} layers ({:.1} s, {} KB)",
                        c.service.name(),
                        c.version,
                        c.layers.len(),
                        c.ms / 1000.0,
                        c.bytes / 1024
                    );
                    u.picked = (c.layers.len() == 1).then_some(0);
                    u.caps = Some(Arc::new(c));
                    u.loading = None;
                }
                Ok(Err(e)) => {
                    u.error = e;
                    u.loading = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => u.loading = None,
            }
        }
        if let Some((_, rx)) = &u.fields {
            match rx.try_recv() {
                Ok(r) => {
                    u.field_list = r.unwrap_or_default();
                    u.fields = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
                Err(_) => u.fields = None,
            }
        }
        // the fetches done: their GeoPackages opened as vector layers
        let mut open = Vec::new();
        let mut notices = Vec::new();
        self.web.pending.retain(|p| {
            let st = p.status.lock().unwrap();
            match st.state {
                JobState::Running => true,
                JobState::Done => {
                    if let Some(f) = p.result.lock().unwrap().take() {
                        open.push((f, p.req.clone(), p.name.clone()));
                    }
                    false
                }
                JobState::Failed => {
                    notices.push((format!("{} could not be fetched", p.name), st.error.clone(), true));
                    false
                }
                _ => false,
            }
        });
        for (f, req, name) in open {
            let path = f.path.to_string_lossy().into_owned();
            if f.capped {
                notices.push((
                    format!("{name}: the first {} features only", f.features),
                    format!(
                        "The server has more features there than the cap ({}): zoom in, narrow the dates or raise \
                         the cap (Add web layer...)",
                        req.cap()
                    ),
                    false,
                ));
            }
            if f.features == 0 {
                notices.push((format!("{name}: no features there"), req.describe(), false));
            }
            self.web.ui.status = format!(
                "\"{name}\": {} features{}{}",
                f.features,
                if f.capped { " (capped)" } else { "" },
                if f.cached { ", from the cache" } else { "" }
            );
            self.web.sources.retain(|s| s.path != path);
            self.web.sources.push(WebSource {
                path: path.clone(),
                req,
                name,
                features: f.features,
                capped: f.capped,
                named: f.features == 0, // no layer to name
            });
            if f.features > 0 {
                self.add_vector_files(vec![path], ctx);
            }
        }
        for (t, text, failed) in notices {
            if failed {
                self.web.ui.error = format!("{t}: {text}");
            }
            self.notify(&t, &text, failed);
        }
        // the vector layers read: their names
        for s in self.web.sources.iter_mut().filter(|s| !s.named) {
            if let Some(v) = self.vectors.iter_mut().find(|v| v.data.path == s.path) {
                v.name = if s.capped { format!("{} (first {})", s.name, s.features) } else { s.name.clone() };
                s.named = true;
            }
        }
        self.pump_web_maps(ctx);
        // --web-layer: once the series is open and the view known (its area is the features')
        if !self.web.cli_done
            && !self.opts.web_layers.is_empty()
            && self.session.as_ref().is_some_and(|s| s.ready_ms.is_some())
            && self.map.last_view.is_some()
            && self.frame > 10
        {
            self.web.cli_done = true;
            for arg in self.opts.web_layers.clone() {
                if let Err(e) = self.web_layer_option(&arg, ctx) {
                    eprintln!("--web-layer {arg}: {e}");
                }
            }
        }
    }

    fn pump_web_maps(&mut self, ctx: &egui::Context) {
        if self.web.maps.is_empty() {
            return;
        }
        let date = self.web_series_date();
        let (Some(s), Some(gpu)) = (&self.session, &self.gpu) else { return };
        if self.frame < 3 {
            return;
        }
        let info = s.info.clone();
        let last_view = self.map.last_view.map(|v| [v.x0, v.y0, v.x1, v.y1]);
        let mut dirty = false;
        for w in self.web.maps.iter_mut() {
            if !w.visible {
                continue; // hidden: nothing asked for, the tiles kept
            }
            let t = w.time_for(&date);
            if t != w.spec.time {
                w.spec.time = t;
                w.retire();
                w.failed_for = 0;
            }
            if w.b.is_none() {
                let c = ctx.clone();
                w.b = Some(Basemap::new(
                    &ogc::map_source(&w.spec),
                    &cache_dir().join("tiles"),
                    Arc::new(move || c.request_repaint()),
                ));
                w.failed_for = 0;
                dirty = true;
            }
            let b = w.b.as_mut().unwrap();
            let around = around_view(last_view, &info, b.around());
            if w.failed_for != info.id {
                let had = b.has_map();
                match b.set_map(&info, around, |gw, gh, rg| gpu.create_warp_grid(gw as u32, gh as u32, rg)) {
                    Ok(()) => {
                        w.why.clear();
                        dirty |= !had;
                    }
                    Err(why) => {
                        w.why = why;
                        w.failed_for = info.id;
                    }
                }
            }
            b.tick();
            dirty |= b.upload_ready(16, |rgba, side| gpu.create_image(side, side, rgba));
        }
        if dirty {
            self.map.dirty = true;
        }
    }

    /// File > Add web layer...'s window.
    pub(crate) fn web_window(&mut self, ctx: &egui::Context) {
        self.pump_web(ctx);
        self.web_info_window(ctx);
        if !self.web.ui.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Add web layer", ctx, 560.0)
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.web_ui(ui)));
        self.web.ui.show = open;
    }

    fn web_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let presets = ogc::presets();
        let mut connect = false;
        {
            let u = &mut self.web.ui;
            ui.horizontal_wrapped(|ui| {
                ui.label("Server");
                egui::ComboBox::from_id_salt("web preset")
                    .selected_text("Presets")
                    .width(crate::fit::scaled(ui, 120.0))
                    .show_ui(ui, |ui| {
                        for p in &presets {
                            if ui
                                .selectable_label(u.url == p.url && u.service == p.service, p.name)
                                .on_hover_text(format!("{}\n{}", p.url, p.terms))
                                .clicked()
                            {
                                u.url = p.url.into();
                                u.service = p.service;
                                connect = true;
                            }
                        }
                    });
                egui::ComboBox::from_id_salt("web service")
                    .selected_text(u.service.map_or("Auto", Service::name))
                    .width(crate::fit::scaled(ui, 150.0))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut u.service, None, "Auto")
                            .on_hover_text("Found from the URL, else OGC API, WFS and WMS tried in turn");
                        for s in Service::ALL {
                            ui.selectable_value(&mut u.service, Some(s), s.name());
                        }
                    });
            });
            let r = ui.add(
                egui::TextEdit::singleline(&mut u.url)
                    .hint_text("https://.../geoserver/ows, .../wms, an OGC API's landing page")
                    .desired_width(f32::INFINITY),
            );
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                connect = true;
            }
            ui.horizontal(|ui| {
                let busy = u.loading.is_some();
                if ui.add_enabled(!busy && !u.url.trim().is_empty(), egui::Button::new("Connect")).clicked() {
                    connect = true;
                }
                if busy {
                    ui.spinner();
                    ui.weak("reading the server's capabilities...");
                }
            });
        }
        if connect {
            self.web_connect(&ctx);
        }
        let warn = crate::theme::warning();
        if !self.web.ui.error.is_empty() {
            ui.colored_label(warn, &self.web.ui.error);
        }
        let Some(caps) = self.web.ui.caps.clone() else {
            if !self.web.ui.status.is_empty() {
                ui.weak(&self.web.ui.status);
            }
            ui.weak("Maps (WMS, WMTS) are drawn like the basemap; features (WFS, OGC API) open as a vector layer.");
            return;
        };
        let (service, nlayers) = (caps.service, caps.layers.len());
        ui.separator();
        {
            let u = &mut self.web.ui;
            let title = if caps.title.is_empty() { caps.url.clone() } else { caps.title.clone() };
            ui.label(RichText::new(format!("{title} ({})", service.name())).strong());
            if nlayers > 8 || !u.filter.is_empty() {
                ui.add(
                    egui::TextEdit::singleline(&mut u.filter)
                        .hint_text("Filter by name, title or abstract")
                        .desired_width(f32::INFINITY),
                );
            }
            if u.shown_for != (u.filter.clone(), nlayers) {
                u.shown = (0..nlayers).filter(|&k| caps.layers[k].matches(&u.filter)).collect();
                u.shown_for = (u.filter.clone(), nlayers);
            }
            let row_h = ui.text_style_height(&egui::TextStyle::Body) + ui.spacing().item_spacing.y + 2.0;
            let shown = u.shown.clone();
            let mut pick = None;
            egui::ScrollArea::vertical()
                .id_salt("web layers")
                .max_height(crate::fit::scaled(ui, 200.0))
                .auto_shrink([false, true])
                .show_rows(ui, row_h, shown.len(), |ui, range| {
                    for &k in &shown[range] {
                        let l = &caps.layers[k];
                        let text = if l.title.is_empty() || l.title == l.name {
                            l.name.clone()
                        } else {
                            format!("{}  ({})", l.title, l.name)
                        };
                        let r = ui.add(egui::Button::selectable(u.picked == Some(k), text).truncate());
                        let r = if l.abstract_text.is_empty() { r } else { r.on_hover_text(&l.abstract_text) };
                        if r.clicked() {
                            pick = Some(k);
                        }
                    }
                });
            ui.weak(format!("{} of {nlayers} layers", shown.len()));
            if let Some(k) = pick
                && u.picked != Some(k)
            {
                u.picked = Some(k);
                u.style.clear();
                u.format.clear();
                u.field_list.clear();
                u.date_field.clear();
                if !service.is_map() {
                    let (tx, rx) = channel();
                    let (url, name, c) = (caps.url.clone(), caps.layers[k].name.clone(), ctx.clone());
                    let raw = caps.raw_caps();
                    std::thread::Builder::new()
                        .name("ogc-fields".into())
                        .spawn(move || {
                            let _ = tx.send(ogc::layer_fields_with(service, &url, &name, raw));
                            c.request_repaint();
                        })
                        .ok();
                    u.fields = Some((caps.layers[k].name.clone(), rx));
                }
            }
        }
        let Some(k) = self.web.ui.picked else {
            ui.weak(&self.web.ui.status);
            return;
        };
        let caps = self.web.ui.caps.clone().unwrap();
        let l = caps.layers[k].clone();
        let formats = if service == Service::Wmts { l.formats.clone() } else { caps.formats.clone() };
        ui.separator();
        ui.label(RichText::new(l.label()).strong());
        if !l.abstract_text.is_empty() {
            ui.add(egui::Label::new(RichText::new(&l.abstract_text).weak()).wrap());
        }
        if let Some(b) = l.bbox {
            ui.weak(format!("Extent: {:.3}, {:.3} to {:.3}, {:.3} (longitude, latitude)", b[0], b[1], b[2], b[3]));
        }
        let has_series = self.session.is_some();
        if service.is_map() {
            let u = &mut self.web.ui;
            egui::Grid::new("web map grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                if l.styles.len() > 1 {
                    ui.label("Style");
                    let cur = if u.style.is_empty() { l.styles[0].clone() } else { u.style.clone() };
                    egui::ComboBox::from_id_salt("web style").selected_text(&cur).show_ui(ui, |ui| {
                        for s in &l.styles {
                            ui.selectable_value(&mut u.style, s.clone(), s);
                        }
                    });
                    ui.end_row();
                }
                if formats.len() > 1 {
                    ui.label("Format");
                    let cur = if u.format.is_empty() { "image/png".to_string() } else { u.format.clone() };
                    egui::ComboBox::from_id_salt("web format").selected_text(&cur).show_ui(ui, |ui| {
                        for f in formats.iter().filter(|f| f.starts_with("image/")) {
                            ui.selectable_value(&mut u.format, f.clone(), f);
                        }
                    });
                    ui.end_row();
                }
                if service == Service::Wms {
                    ui.label("");
                    ui.checkbox(&mut u.transparent, "Transparent")
                        .on_hover_text("Where the map has nothing, the layers under it show (PNG)");
                    ui.end_row();
                }
                ui.label("Opacity");
                ui.add(egui::Slider::new(&mut u.opacity, 0.0..=1.0).fixed_decimals(2));
                ui.end_row();
                if !l.times.is_empty() || !l.time_default.is_empty() {
                    ui.label("Time");
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("web time").selected_text(u.time_mode.name()).show_ui(ui, |ui| {
                            for m in TimeMode::ALL {
                                ui.selectable_value(&mut u.time_mode, m, m.name());
                            }
                        });
                        if u.time_mode == TimeMode::Fixed {
                            ui.add(
                                egui::TextEdit::singleline(&mut u.fixed_time)
                                    .hint_text("2023-08-01")
                                    .desired_width(crate::fit::scaled(ui, 160.0)),
                            );
                        }
                    })
                    .response
                    .on_hover_text(format!(
                        "{} times listed ({} ... {}), default {}",
                        l.times.len(),
                        l.times.first().map_or("", |s| s.as_str()),
                        l.times.last().map_or("", |s| s.as_str()),
                        if l.time_default.is_empty() { "none" } else { &l.time_default }
                    ));
                    ui.end_row();
                }
            });
            let ok = service != Service::Wmts || l.tiles.is_some();
            if !ok {
                ui.colored_label(warn, "No Web Mercator tile matrix set for this layer: use the server's WMS.");
            }
            if !has_series {
                ui.weak("Drawn over a georeferenced series once one is open.");
            }
            if ui.add_enabled(ok, egui::Button::new("Add map layer").min_size(vec2(130.0, 0.0))).clicked() {
                match self.web_add_picked_map() {
                    Ok(_) => self.web.ui.status = format!("\"{}\" added on top of the layers", l.label()),
                    Err(e) => self.web.ui.error = e,
                }
            }
        } else {
            let has_roi = self.series.roi_rect.is_some();
            let u = &mut self.web.ui;
            egui::Grid::new("web features grid").num_columns(2).spacing(vec2(10.0, 6.0)).show(ui, |ui| {
                ui.label("Where");
                let areas = ["The visible area", "The ROI", "The active layer", "The whole layer"];
                egui::ComboBox::from_id_salt("web area")
                    .selected_text(areas[u.area.min(3)])
                    .show_ui(ui, |ui| {
                        for (a, name) in areas.iter().enumerate() {
                            let ok = a == 3 || (has_series && (a != 1 || has_roi));
                            ui.add_enabled_ui(ok, |ui| ui.selectable_value(&mut u.area, a, *name));
                        }
                    })
                    .response
                    .on_hover_text(
                        "Only the features in this box are asked for (BBOX); the whole layer can be very large",
                    );
                ui.end_row();
                ui.label("At most");
                ui.add(egui::DragValue::new(&mut u.max_features).range(1..=2_000_000).speed(100.0).suffix(" features"))
                    .on_hover_text("Fetching stops there (a notice says so): zoom in or narrow the dates for the rest");
                ui.end_row();
                ui.label("Dates");
                ui.horizontal_wrapped(|ui| {
                    let dates: Vec<&(String, String)> =
                        u.field_list.iter().filter(|f| f.1.contains("Date") || f.0.contains("date")).collect();
                    let cur = if u.date_field.is_empty() { "no date filter".to_string() } else { u.date_field.clone() };
                    egui::ComboBox::from_id_salt("web date field").selected_text(cur).show_ui(ui, |ui| {
                        ui.selectable_value(&mut u.date_field, String::new(), "no date filter");
                        for f in &dates {
                            ui.selectable_value(&mut u.date_field, f.0.clone(), format!("{} ({})", f.0, f.1));
                        }
                    });
                    if !u.date_field.is_empty() {
                        ui.add(
                            egui::TextEdit::singleline(&mut u.from)
                                .hint_text("from 2023-01-01")
                                .desired_width(crate::fit::scaled(ui, 95.0)),
                        );
                        ui.add(
                            egui::TextEdit::singleline(&mut u.to)
                                .hint_text("to 2023-12-31")
                                .desired_width(crate::fit::scaled(ui, 95.0)),
                        );
                    }
                });
                ui.end_row();
                ui.label("Filter");
                ui.add(
                    egui::TextEdit::singleline(&mut u.where_sql)
                        .hint_text("classname = 'DESMATAMENTO_CR' (optional)")
                        .desired_width(crate::fit::scaled(ui, 300.0)),
                )
                .on_hover_text("An attribute filter (OGR SQL), sent to the server when it can take it");
                ui.end_row();
            });
            if u.fields.is_some() {
                ui.weak("reading the layer's fields...");
            } else if !u.field_list.is_empty() {
                let names: Vec<&str> = u.field_list.iter().map(|f| f.0.as_str()).collect();
                ui.add(egui::Label::new(RichText::new(format!("Fields: {}", names.join(", "))).weak()).wrap());
            }
            if ui.add(egui::Button::new("Fetch features").min_size(vec2(130.0, 0.0))).clicked() {
                match self.web_request() {
                    Ok((req, name)) => self.web_fetch(req, name, None, &ctx),
                    Err(e) => self.web.ui.error = e,
                }
            }
        }
        if !self.web.ui.status.is_empty() {
            ui.weak(&self.web.ui.status);
        }
        let terms = presets.iter().find(|p| ogc::host_of(p.url) == ogc::host_of(&caps.url)).map(|p| p.terms);
        if let Some(t) = terms {
            ui.weak(t);
        }
    }

    /// A web map's row in the Layers panel.
    pub(crate) fn web_row(
        &mut self,
        ui: &mut Ui,
        k: usize,
        place: (usize, usize, usize),
        acts: &mut Vec<crate::contents::Act>,
    ) {
        use crate::contents::{Row, Swatch, row};
        let pos = place.0;
        let w = &mut self.web.maps[k];
        if !self.contents.shows(&w.name) {
            return;
        }
        let (id, indent) = (w.id, self.contents.indent);
        let loading = w.b.as_ref().map_or(0, |b| b.inflight());
        let warn = if !w.why.is_empty() {
            Some(format!("not drawn: {}", w.why))
        } else {
            w.b.as_ref()
                .map(|b| b.error())
                .filter(|e| !e.is_empty())
                .or_else(|| w.b.as_ref().filter(|b| b.offline()).map(|_| "the server cannot be reached".to_string()))
        };
        let detail = format!(
            "{} {}{}",
            w.spec.service.name(),
            ogc::host_of(&w.spec.url),
            if !w.spec.time.is_empty() {
                format!(", {}", w.spec.time)
            } else if loading > 0 {
                format!(", loading {loading}")
            } else {
                String::new()
            }
        );
        let mut remove = false;
        ui.push_id(("web map", id), |ui| {
            let r = Row {
                name: &w.name,
                detail,
                badge: String::new(),
                active: false,
                selected: false,
                warn,
                indent,
                swatch: Swatch::Basemap,
                fold: None,
            };
            let out = row(ui, Some(&mut w.visible), &r);
            if out.visible_changed {
                self.map.dirty = true;
            }
            let tip = |ui: &mut Ui| {
                ui.label(format!("{} layer {}", w.spec.service.name(), w.spec.layer));
                ui.label(&w.spec.url);
                ui.weak(w.attribution());
                if !w.times.is_empty() {
                    ui.label(format!(
                        "Time: {} ({} listed)",
                        if w.spec.time.is_empty() { "the server's default" } else { &w.spec.time },
                        w.times.len()
                    ));
                }
            };
            let out_row = out.row.on_hover_ui(tip);
            crate::contents::drag_to_reorder(ui, &out_row, pos, acts);
            let mut menu = |ui: &mut Ui| {
                if ui.add(egui::Slider::new(&mut w.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2)).changed() {
                    self.map.dirty = true;
                }
                if !w.times.is_empty() {
                    ui.menu_button("Time", |ui| {
                        for m in TimeMode::ALL {
                            if ui.selectable_value(&mut w.time_mode, m, m.name()).changed() {
                                self.map.dirty = true;
                            }
                        }
                        if w.time_mode == TimeMode::Fixed {
                            ui.add(egui::TextEdit::singleline(&mut w.fixed_time).hint_text("2023-08-01"));
                        }
                    });
                }
                if ui
                    .button("Reload its tiles")
                    .on_hover_text("Its tiles asked for again (the disk cache kept)")
                    .clicked()
                {
                    w.retire();
                    w.failed_for = 0;
                    ui.close();
                }
                crate::contents::move_menu(ui, place, acts);
                ui.separator();
                if ui.button("Remove").clicked() {
                    remove = true;
                    ui.close();
                }
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
        });
        if remove {
            self.remove_web_map(k);
        }
    }

    /// A project's vector layer fetched from a server: its request kept (saved again with it), and fetched again
    /// into `path` when the file is gone.
    pub(crate) fn restore_web_source(
        &mut self,
        path: &str,
        e: &WebFeatureEntry,
        name: &str,
        fetch: bool,
        ctx: &egui::Context,
    ) {
        let req = e.request();
        self.web.sources.retain(|s| s.path != path);
        self.web.sources.push(WebSource {
            path: path.to_string(),
            req: req.clone(),
            name: name.to_string(),
            features: 0,
            capped: false,
            named: true,
        });
        if fetch {
            self.web_fetch(req, name.to_string(), Some(PathBuf::from(path)), ctx);
        }
    }

    /// The project's map layers (their keys "w0"...), to restore.
    pub(crate) fn web_map_entries(&self) -> Vec<WebMapEntry> {
        self.web
            .maps
            .iter()
            .map(|m| WebMapEntry {
                service: m.spec.service.id().into(),
                url: m.spec.url.clone(),
                version: m.spec.version.clone(),
                layer: m.spec.layer.clone(),
                style: m.spec.style.clone(),
                format: m.spec.format.clone(),
                transparent: m.spec.transparent,
                srs: m.spec.srs.clone(),
                tiles: m.spec.tiles.clone(),
                max_zoom: m.spec.max_zoom,
                title: m.spec.title.clone(),
                attribution: m.spec.attribution.clone(),
                info_format: m.spec.info_format.clone(),
                info_url: m.spec.info_url.clone(),
                times: m.times.clone(),
                time_mode: m.time_mode,
                fixed_time: m.fixed_time.clone(),
                name: m.name.clone(),
                visible: m.visible,
                opacity: m.opacity,
            })
            .collect()
    }

    /// A project's map layers added (each on top); their places in `maps`.
    pub(crate) fn restore_web_maps(&mut self, entries: &[WebMapEntry]) -> Vec<usize> {
        let mut out = Vec::new();
        for e in entries {
            let spec = MapLayerSpec {
                service: Service::from_id(&e.service).unwrap_or(Service::Wms),
                url: e.url.clone(),
                version: e.version.clone(),
                layer: e.layer.clone(),
                style: e.style.clone(),
                format: e.format.clone(),
                transparent: e.transparent,
                time: String::new(),
                srs: e.srs.clone(),
                tiles: e.tiles.clone(),
                max_zoom: e.max_zoom,
                title: e.title.clone(),
                attribution: e.attribution.clone(),
                info_format: e.info_format.clone(),
                info_url: e.info_url.clone(),
            };
            let id = self.add_web_map(spec, e.times.clone(), e.time_mode, e.fixed_time.clone(), e.opacity);
            let k = self.web.maps.len() - 1;
            let m = &mut self.web.maps[k];
            debug_assert_eq!(m.id, id);
            (m.name, m.visible) = (e.name.clone(), e.visible);
            out.push(k);
        }
        out
    }

    /// --web-layer URL,LAYER[,SERVICE[,KEY=VALUE...]]: a web layer added once the series is open (screenshots):
    /// a map at once, features of the visible area fetched (time=, from=, to=, field=, max=, opacity=).
    pub(crate) fn web_layer_option(&mut self, arg: &str, ctx: &egui::Context) -> Result<(), String> {
        let mut parts = arg.split(',');
        let url = parts.next().unwrap_or("").trim().to_string();
        let layer = parts.next().unwrap_or("").trim().to_string();
        let mut service = None;
        let mut kv = Vec::new();
        for p in parts {
            match p.split_once('=') {
                Some((k, v)) => kv.push((k.trim().to_ascii_lowercase(), v.trim().to_string())),
                None => service = Service::from_id(p.trim()),
            }
        }
        let get = |k: &str| kv.iter().find(|x| x.0 == k).map(|x| x.1.clone());
        let caps = ogc::capabilities(&url, service)?;
        let k = caps
            .layers
            .iter()
            .position(|l| l.name == layer || l.name.rsplit(':').next() == Some(&layer))
            .ok_or(format!("{layer}: not on the server"))?;
        let u = &mut self.web.ui;
        u.picked = Some(k);
        u.caps = Some(Arc::new(caps.clone()));
        u.opacity = get("opacity").and_then(|v| v.parse().ok()).unwrap_or(1.0);
        u.transparent = true;
        u.max_features = get("max").and_then(|v| v.parse().ok()).unwrap_or(ogc::DEFAULT_MAX_FEATURES);
        if let Some(t) = get("time") {
            (u.time_mode, u.fixed_time) = match t.as_str() {
                "series" => (TimeMode::SeriesDate, String::new()),
                "upto" => (TimeMode::UpToSeriesDate, String::new()),
                _ => (TimeMode::Fixed, t),
            };
        }
        u.date_field = get("field").unwrap_or_default();
        u.from = get("from").unwrap_or_default();
        u.to = get("to").unwrap_or_default();
        u.area = get("area").and_then(|v| v.parse().ok()).unwrap_or(0);
        // the dialog as it was filled (screenshots): dialog=1, the list filtered by filter=
        (u.url, u.service) = (url, service);
        u.show = get("dialog").is_some_and(|v| v == "1");
        u.filter = get("filter").unwrap_or_default();
        u.status.clear();
        if !caps.service.is_map() {
            u.field_list = ogc::layer_fields_with(caps.service, &caps.url, &caps.layers[k].name, caps.raw_caps())
                .unwrap_or_default();
        }
        if caps.service.is_map() {
            self.web_add_picked_map().map(|_| ())
        } else {
            let (req, name) = self.web_request()?;
            self.web_fetch(req, name, None, ctx);
            Ok(())
        }
    }

    /// Ready for a screenshot: no fetch under way, every map's tiles of the view read.
    pub(crate) fn web_ready(&self) -> bool {
        let key = self.vector_place_key();
        self.web.pending.is_empty()
            && self.web.sources.iter().all(|s| {
                s.named
                    && self
                        .vectors
                        .iter()
                        .filter(|v| v.data.path == s.path)
                        .all(|v| v.placed_for == key || !v.why.is_empty())
            })
            && self.web.maps.iter().all(|m| {
                !m.visible
                    || !m.why.is_empty()
                    || m.b.as_ref().is_some_and(|b| b.has_map() && b.loads() > 0 && b.inflight() == 0)
            })
    }
}

/// The map's area placed for tiles (basemap.rs' rule): None while the view is within the layer and its margin,
/// else a box about three times the view, kept while the view stays in it.
fn around_view(last: Option<[f64; 4]>, info: &janus_cube::CubeInfo, cur: Option<[f64; 4]>) -> Option<[f64; 4]> {
    let v = last?;
    let inside = |a: &[f64; 4]| v[0] >= a[0] && v[1] >= a[1] && v[2] <= a[2] && v[3] <= a[3];
    if inside(&Basemap::<GpuTexture>::default_around(info)) {
        return None;
    }
    let area = |a: &[f64; 4]| (a[2] - a[0]) * (a[3] - a[1]);
    if let Some(c) = cur
        && inside(&c)
        && area(&c) < 64.0 * area(&v)
    {
        return Some(c);
    }
    let (w, h) = (v[2] - v[0], v[3] - v[1]);
    Some([v[0] - w, v[1] - h, v[2] + w, v[3] + h])
}

/// A window of a layer's pixels as a box in longitude and latitude (its edges densified).
fn lonlat_box(info: &janus_cube::CubeInfo, win: [f64; 4]) -> Result<[f64; 4], String> {
    use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
    if info.crs_wkt.is_empty() || info.geo_transform.is_none() {
        return Err("the active layer has no CRS".into());
    }
    let mut src = SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| e.to_string())?;
    let mut dst = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&src, &dst).map_err(|_| "no transformation to longitude and latitude")?;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for i in 0..=16 {
        let f = f64::from(i) / 16.0;
        for (px, py) in [
            (win[0], win[1] + f * (win[3] - win[1])),
            (win[2], win[1] + f * (win[3] - win[1])),
            (win[0] + f * (win[2] - win[0]), win[1]),
            (win[0] + f * (win[2] - win[0]), win[3]),
        ] {
            let Some((gx, gy)) = info.pixel_to_geo(px, py) else { continue };
            let (mut x, mut y) = ([gx], [gy]);
            let mut z = [0.0];
            if ct.transform_coords(&mut x, &mut y, &mut z).is_ok() && x[0].is_finite() && y[0].is_finite() {
                b = [b[0].min(x[0]), b[1].min(y[0]), b[2].max(x[0]), b[3].max(y[0])];
            }
        }
    }
    if b[2] <= b[0] || b[3] <= b[1] {
        return Err("the area could not be placed in longitude and latitude".into());
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_and_times() {
        assert_eq!(ymd(0.0), "1970-01-01");
        assert_eq!(ymd(1_577_836_800.0), "2020-01-01");
        assert_eq!(ymd(1_582_934_400.0), "2020-02-29");
        let mut m = WebMap {
            id: 1,
            name: "x".into(),
            spec: MapLayerSpec::default(),
            times: vec!["2020-01-01T03:00:00.000Z".into(), "2020-02-01T03:00:00.000Z".into()],
            time_mode: TimeMode::SeriesDate,
            fixed_time: String::new(),
            visible: true,
            opacity: 1.0,
            z: 1,
            b: None,
            why: String::new(),
            failed_for: 0,
        };
        assert_eq!(m.time_for("2020-01-15"), "2020-01-01T03:00:00.000Z");
        assert_eq!(m.time_for("2019-01-15"), "");
        m.time_mode = TimeMode::UpToSeriesDate;
        assert_eq!(m.time_for("2020-03-10"), "2020-01-01/2020-03-10");
        m.time_mode = TimeMode::Default;
        assert_eq!(m.time_for("2020-03-10"), "");
    }
}
