//! Validation > Alerts (IDEIAS: alertas oficiais): a map of change dates (a run's map in years: LandTrendr, CCDC,
//! BFAST, CODED...) against official alerts over the area both cover and a window of dates: INPE's DETER
//! (TerraBrasilis' WFS, Amazon or Cerrado, fetched for the map's area into a GeoPackage of the cache that opens as
//! a vector layer), any vector layer of dated polygons, or alert rasters (UMD's GLAD-L and GLAD-S2, Wageningen's
//! RADD, GFW's integrated alerts: read from their public tiles where they are keyless, or from GeoTIFFs on disk).
//! The comparison (janus_validation::alerts) runs in a task of Tools > Tasks; its agreement map (both, only the
//! map, only the alerts) and, for rasters, the alerts' dates join the layers; the numbers, the lags' histogram
//! and the classes are in the tab, and in CSVs.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{RichText, Ui};
use egui_plot::{Bar, BarChart, Plot};
use janus_geo::ogc::{FeatureRequest, Service};
use janus_validation::alerts::{
    self, AGREEMENT_CLASSES, AlertsOnGrid, CompareOptions, Comparison, DateCode, Product, RasterFile, parse_day, ymd,
};

use crate::app::State;
use crate::validation::fmt_area;
use crate::zeit::{Job, JobLog, JobState, JobStatus, ResultLayer};

/// A WFS (or OGC API) layer of dated polygons with a class: DETER's, or another server's.
#[derive(Clone, Debug, PartialEq)]
pub struct WfsAlerts {
    pub name: String,
    pub service: Service,
    pub url: String,
    pub layer: String,
    pub date_field: String,
    pub class_field: String,
}

/// INPE's DETER on TerraBrasilis (public, no key): the Legal Amazon and the Cerrado.
pub fn deter_presets() -> [WfsAlerts; 2] {
    let p = |name: &str, ws: &str, layer: &str| WfsAlerts {
        name: name.into(),
        service: Service::Wfs,
        url: format!("https://terrabrasilis.dpi.inpe.br/geoserver/{ws}/ows"),
        layer: format!("{ws}:{layer}"),
        date_field: "view_date".into(),
        class_field: "classname".into(),
    };
    [p("DETER (Amazon)", "deter-amz", "deter_amz"), p("DETER (Cerrado)", "deter-cerrado-nb", "deter_cerrado")]
}

/// Where the alerts come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AlertFrom {
    /// DETER (a preset), or the server set by --alerts url=... / the self-test
    #[default]
    Server,
    /// a vector layer open
    Vector,
    /// alert rasters
    Rasters,
}

/// What a comparison was asked for (kept for its result's names).
#[derive(Clone, Debug)]
enum Alerts {
    Vector { path: String, layer: String, date_field: String, class_field: String, name: String },
    Rasters { product: Product, files: Vec<RasterFile> },
}

/// A comparison done.
pub struct Done {
    pub cmp: Comparison,
    pub classes: Vec<String>,
    pub map_name: String,
    pub alerts_name: String,
    pub alerts_source: String,
    pub skipped: usize,
    pub folder: PathBuf,
    pub agreement: String,
    /// rasters: the alerts' dates on the map's grid
    pub alert_dates: Option<String>,
    /// the map's path (its layer: the grid the new maps take)
    pub map_path: String,
    alerts: Arc<AlertsOnGrid>,
}

type Pending = (Arc<Mutex<JobStatus>>, Arc<Mutex<Option<Done>>>);

pub struct AlertsUi {
    pub from_kind: AlertFrom,
    /// the DETER preset (index into deter_presets)
    pub preset: usize,
    /// another server (--alerts url=..., the self-test): used instead of the preset
    pub server: Option<WfsAlerts>,
    /// the map of change (its path)
    pub map: String,
    /// how a map from disk gives its dates (a run's maps: decimal years)
    pub map_code: DateCode,
    /// a map of dates chosen as a file (File...), listed with the runs' and the layers'
    pub map_file: Option<String>,
    /// a vector layer (its path) and its fields
    pub vector: String,
    pub date_field: String,
    pub class_field: String,
    pub product: Product,
    pub files: Vec<RasterFile>,
    /// the window of dates (YYYY-MM-DD; empty: open)
    pub from: String,
    pub to: String,
    pub bin_days: i32,
    pub status: String,
    pub error: String,
    /// a DETER fetch awaited (its GeoPackage), then compared
    waiting: Option<(String, Alerts)>,
    pending: Option<Pending>,
    pub done: Option<Arc<Done>>,
    /// --alerts applied
    pub cli_done: bool,
}

impl Default for AlertsUi {
    fn default() -> Self {
        AlertsUi {
            from_kind: AlertFrom::Server,
            preset: 0,
            server: None,
            map: String::new(),
            map_code: DateCode::Auto,
            map_file: None,
            vector: String::new(),
            date_field: String::new(),
            class_field: String::new(),
            product: Product::GladS2,
            files: vec![],
            from: String::new(),
            to: String::new(),
            bin_days: 30,
            status: String::new(),
            error: String::new(),
            waiting: None,
            pending: None,
            done: None,
            cli_done: false,
        }
    }
}

impl AlertsUi {
    pub fn busy(&self) -> bool {
        self.waiting.is_some() || self.pending.is_some()
    }

    fn server_now(&self) -> WfsAlerts {
        self.server.clone().unwrap_or_else(|| deter_presets()[self.preset.min(1)].clone())
    }
}

/// Pairs GLAD's files: a name with "date" is the date file of the file named the same without it; GLAD-L's year
/// from the digits after "alert" (alert21 -> 2021) or any 4-digit year in the name.
pub fn pair_files(product: Product, paths: &[String]) -> Vec<RasterFile> {
    let name = |p: &str| {
        std::path::Path::new(p).file_name().map_or(p.to_string(), |f| f.to_string_lossy().to_ascii_lowercase())
    };
    let year_of = |p: &str| -> i32 {
        let n = name(p);
        let digits: Vec<&str> = n.split(|c: char| !c.is_ascii_digit()).filter(|s| !s.is_empty()).collect();
        if let Some(y) = digits.iter().find(|d| d.len() == 4).and_then(|d| d.parse().ok()) {
            return y;
        }
        n.find("alert")
            .and_then(|k| {
                let rest = n[k + 5..].trim_start_matches(|c: char| !c.is_ascii_digit());
                rest.get(..2).and_then(|d| d.parse::<i32>().ok())
            })
            .map_or(0, |y| 2000 + y)
    };
    if !product.two_files() {
        return paths.iter().map(|p| RasterFile { conf: p.clone(), date: String::new(), year: 0 }).collect();
    }
    let (dates, confs): (Vec<&String>, Vec<&String>) = paths.iter().partition(|p| name(p).contains("date"));
    confs
        .iter()
        .map(|c| {
            let key = name(c).replace("alert", "");
            let date = dates
                .iter()
                .find(|d| name(d).replace("alertdate", "").replace("date", "") == key)
                .or_else(|| (dates.len() == 1 && confs.len() == 1).then(|| &dates[0]))
                .map_or(String::new(), |d| d.to_string());
            RasterFile { conf: c.to_string(), date, year: year_of(c) }
        })
        .collect()
}

impl State {
    /// The maps of change dates (path, name): runs' maps of dates in decimal years, then the single maps open as
    /// layers from disk and the file chosen (their dates read as `map_code` says). Unit "year" is a date (a year
    /// of detection, a break date); "years" a length of time (LandTrendr's duration, years to recovery, the spread
    /// of the agreement's years), left out.
    pub(crate) fn alert_maps(&self) -> Vec<(String, String)> {
        let ours = self.results_dir().join("alerts");
        let mut maps: Vec<(String, String)> = self
            .zeit
            .results
            .iter()
            .filter(|r| r.unit == "year" && !r.preview && !r.path.is_empty())
            .filter(|r| !std::path::Path::new(&r.path).starts_with(&ours))
            .map(|r| (r.path.clone(), r.name.clone()))
            .collect();
        for k in 0..self.layers.len() {
            let Some(s) = self.layer_session(k) else { continue };
            let i = &s.info;
            if self.layers[k].canvas || i.t() != 1 || i.remote || i.multidim || i.layers[0].band > 1 {
                continue;
            }
            let p = &i.layers[0].path;
            if !std::path::Path::new(p).starts_with(&ours) && !maps.iter().any(|m| &m.0 == p) {
                maps.push((p.clone(), format!("{} (layer)", self.layers[k].name)));
            }
        }
        if let Some(f) = &self.alerts.map_file
            && !maps.iter().any(|m| &m.0 == f)
        {
            let name = std::path::Path::new(f).file_name().map_or(f.clone(), |n| n.to_string_lossy().into_owned());
            maps.push((f.clone(), format!("{name} (file)")));
        }
        maps
    }

    /// How the map's values give its dates: a run's map in decimal years, else as chosen (detected by default).
    fn alert_map_code(&self, map: &str) -> DateCode {
        if self.zeit.results.iter().any(|r| r.path == map) { DateCode::DecimalYear } else { self.alerts.map_code }
    }

    /// Validation > Alerts in front.
    pub(crate) fn open_alerts(&mut self) {
        self.val.show = true;
        self.val.select_tab = Some(5);
    }

    /// Compares the map with the alerts chosen: DETER fetched first (a task) when it is the source.
    pub(crate) fn alerts_compare(&mut self, ctx: &egui::Context) -> Result<(), String> {
        let a = &self.alerts;
        if a.busy() {
            return Err("a comparison is under way".into());
        }
        if a.map.is_empty() {
            return Err("choose a map of change dates".into());
        }
        let window = window_of(&a.from, &a.to)?;
        let map = a.map.clone();
        match a.from_kind {
            AlertFrom::Server => {
                let s = a.server_now();
                let grid = janus_validation::read_grid(&map)?;
                let bbox = alerts::lonlat_bounds(&grid)?;
                let req = FeatureRequest {
                    service: s.service,
                    url: s.url.clone(),
                    layer: s.layer.clone(),
                    bbox: Some(bbox),
                    date_field: s.date_field.clone(),
                    from: if window.0 > i32::MIN + 1 { ymd(window.0) } else { String::new() },
                    to: if window.1 < i32::MAX { ymd(window.1) } else { String::new() },
                    filter: String::new(),
                    max_features: 0,
                };
                let path = req.cache_path(&crate::web_layers::cache_dir()).to_string_lossy().into_owned();
                let what = Alerts::Vector {
                    path: path.clone(),
                    layer: req.out_layer(),
                    date_field: s.date_field.clone(),
                    class_field: s.class_field.clone(),
                    name: s.name.clone(),
                };
                let open = self.web.sources.iter().any(|w| w.path == path) && std::path::Path::new(&path).exists();
                if open {
                    self.alerts_start(what, ctx);
                } else {
                    self.web_fetch(req, s.name.clone(), None, ctx);
                    self.alerts.status = format!("Fetching {} for the map's area (Tools > Tasks)...", s.name);
                    self.alerts.waiting = Some((path, what));
                }
            }
            AlertFrom::Vector => {
                let v =
                    self.vectors.iter().find(|v| v.data.path == a.vector).ok_or("choose a vector layer of alerts")?;
                if a.date_field.is_empty() {
                    return Err("choose the alerts' date field".into());
                }
                let what = Alerts::Vector {
                    path: v.data.path.clone(),
                    layer: v.data.layer.clone(),
                    date_field: a.date_field.clone(),
                    class_field: a.class_field.clone(),
                    name: v.name.clone(),
                };
                self.alerts_start(what, ctx);
            }
            AlertFrom::Rasters => {
                if a.files.is_empty() {
                    return Err("add the alerts' files (or download them for the map's area)".into());
                }
                if a.product.two_files() && a.files.iter().any(|f| f.date.is_empty()) {
                    return Err(format!("{}: each alert file needs its file of dates", a.product.name()));
                }
                let what = Alerts::Rasters { product: a.product, files: a.files.clone() };
                self.alerts_start(what, ctx);
            }
        }
        Ok(())
    }

    /// The comparison in a task.
    fn alerts_start(&mut self, what: Alerts, ctx: &egui::Context) {
        let a = &mut self.alerts;
        a.error.clear();
        let Ok(window) = window_of(&a.from, &a.to) else { return };
        let opts = CompareOptions { from: window.0, to: window.1, bin_days: a.bin_days.max(1) };
        let map = a.map.clone();
        let code = self.alert_map_code(&map);
        let map_name = self.alert_maps().into_iter().find(|m| m.0 == map).map_or(map.clone(), |m| m.1);
        let alerts_name = match &what {
            Alerts::Vector { name, .. } => name.clone(),
            Alerts::Rasters { product, .. } => product.name().to_string(),
        };
        let stem = janus_cube::stac_vrt::sanitize(&alerts_name);
        let folder = self.results_dir().join("alerts").join(format!("{stem}_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&folder) {
            self.alerts.error = format!("could not create {}: {e}", folder.display());
            return;
        }
        let threads = self.processing_threads();
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Alerts: {alerts_name} against {map_name}\n{}\nmap: {map}\nalerts: {what:?}\nwindow: {} to {}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            ymd(opts.from),
            ymd(opts.to),
            folder.display()
        ));
        {
            let mut p = crate::provenance::Provenance::start("alerts", "alerts", "Alerts against a map of change");
            p.params.insert("map".into(), serde_json::json!(map));
            p.params.insert("map_dates".into(), serde_json::json!(code.name()));
            p.params.insert("alerts".into(), serde_json::json!(format!("{what:?}")));
            p.params.insert("from".into(), serde_json::json!(ymd(opts.from)));
            p.params.insert("to".into(), serde_json::json!(ymd(opts.to)));
            p.python_note = "tools/bench_alerts_vs_python.py: geopandas, rasterio and numpy, the same numbers".into();
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
        let (st, o, cn, c, dir) = (status.clone(), out.clone(), cancel.clone(), ctx.clone(), folder.clone());
        let (mn, an) = (map_name.clone(), alerts_name.clone());
        std::thread::Builder::new()
            .name("alerts".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, msg: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, msg.to_string());
                    }
                    c.request_repaint();
                };
                let r = run(&map, code, &what, &opts, threads, &dir, &progress, &cn, &log);
                let seconds = t0.elapsed().as_secs_f64();
                let mut s = st.lock().unwrap();
                s.seconds = seconds;
                match r {
                    Ok((cmp, on_grid, agreement, alert_dates)) => {
                        let k = &cmp;
                        log.line(&format!(
                            "{} {}: both {}, only the map {}, only the alerts {}; {} of {} alerts detected; {:.1} s",
                            fmt_area(k.covered_ha),
                            k.unit,
                            fmt_area(k.both_ha),
                            fmt_area(k.only_map_ha),
                            fmt_area(k.only_alerts_ha),
                            k.alerts_detected,
                            k.alerts,
                            seconds
                        ));
                        log.line("done");
                        *o.lock().unwrap() = Some(Done {
                            classes: on_grid.classes.clone(),
                            alerts_source: on_grid.source.clone(),
                            skipped: on_grid.skipped,
                            map_name: mn,
                            alerts_name: an,
                            folder: dir,
                            agreement,
                            alert_dates,
                            map_path: map,
                            cmp,
                            alerts: Arc::new(on_grid),
                        });
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
            title: format!("Compare {alerts_name} with {map_name}"),
            tool: "alerts".into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true,
        });
        self.alerts.status = format!("Comparing {alerts_name} with {map_name} (Tools > Tasks)...");
        self.alerts.pending = Some((status, out));
    }

    /// Once a frame: the DETER fetch awaited, the comparison done (its maps as layers), --alerts.
    pub(crate) fn pump_alerts(&mut self, ctx: &egui::Context) {
        if self.alerts.waiting.is_some() && !self.web.fetching() {
            let (path, what) = self.alerts.waiting.take().unwrap();
            if self.web.sources.iter().any(|s| s.path == path) {
                self.alerts_start(what, ctx);
            } else {
                self.alerts.status.clear();
                self.alerts.error = if self.web.ui.error.is_empty() {
                    "the alerts could not be fetched".into()
                } else {
                    self.web.ui.error.clone()
                };
            }
        }
        if let Some((st, out)) = &self.alerts.pending {
            let (state, error) = {
                let s = st.lock().unwrap();
                (s.state, s.error.clone())
            };
            match state {
                JobState::Running => {}
                JobState::Done => {
                    let done = out.lock().unwrap().take();
                    self.alerts.pending = None;
                    if let Some(d) = done {
                        self.alerts.status.clear();
                        self.alerts_layers(&d, ctx);
                        self.alerts.done = Some(Arc::new(d));
                    }
                }
                JobState::Failed => {
                    self.alerts.status.clear();
                    self.alerts.error = error;
                    self.alerts.pending = None;
                }
                _ => {
                    self.alerts.status = "Cancelled".into();
                    self.alerts.pending = None;
                }
            }
        }
        // --alerts: once the series is open and a map of change dates is there (a Zeit run's, loaded)
        if !self.alerts.cli_done
            && !self.opts.alerts.is_empty()
            && self.session.as_ref().is_some_and(|s| s.ready_ms.is_some())
            && self.frame > 10
            && (!self.opts.zeit_run || self.zeit.runs_settled())
            && self.zeit.pending_loads == 0
            && (!self.alert_maps().is_empty() || self.opts.alerts.iter().any(|a| a.contains("mapfile=")))
        {
            self.alerts.cli_done = true;
            for arg in self.opts.alerts.clone() {
                if let Err(e) = self.alerts_option(&arg, ctx) {
                    eprintln!("--alerts {arg}: {e}");
                    self.alerts.error = e;
                }
            }
        }
    }

    /// The comparison's maps as layers on the map's grid (the previous comparison's removed): the agreement and,
    /// for rasters, the alerts' dates in the map's colours.
    fn alerts_layers(&mut self, d: &Done, ctx: &egui::Context) {
        let prev: Vec<String> = self.alerts.done.iter().map(|p| p.folder.to_string_lossy().into_owned()).collect();
        self.zeit.results.retain(|r| !prev.contains(&r.run));
        let Some(base) = self.zeit.results.iter().find(|r| r.path == d.map_path) else {
            // a map from disk: the agreement opens as a layer of its own (its classes named in the file)
            if self.opening.is_none() {
                self.open_inputs(vec![d.agreement.clone()], true, ctx);
            }
            return;
        };
        let make = |name: String, path: &str, unit: &str, output: usize| {
            let mut r = ResultLayer::for_test(vec![], 0, 0, unit.into());
            (r.cube, r.grid, r.source) = (base.cube, base.grid.clone(), base.source.clone());
            (r.x0, r.y0, r.w, r.h) = (base.x0, base.y0, base.w, base.h);
            (r.run, r.output, r.name, r.path) = (d.folder.to_string_lossy().into_owned(), output, name, path.into());
            r.visible = true;
            r
        };
        let mut layers = Vec::new();
        if let Some(p) = &d.alert_dates {
            let mut r = make(format!("{}: date", d.alerts_name), p, "year", 1);
            (r.cmap, r.lo, r.hi) = (base.cmap, base.lo, base.hi);
            r.visible = false;
            layers.push((r, false));
        }
        let mut r = make(format!("Agreement: {} and {}", d.alerts_name, d.map_name), &d.agreement, "", 0);
        r.classes = AGREEMENT_CLASSES.iter().map(|s| s.to_string()).collect();
        r.cmap = janus_gpu::colormaps::by_name("Agreement").unwrap_or(r.cmap);
        layers.push((r, true));
        for (r, auto) in layers {
            self.load_result_layer(r, auto, ctx);
        }
    }

    /// --alerts SOURCE[,KEY=VALUE...]: a comparison once a map of change is there (screenshots). SOURCE: deter-amz,
    /// deter-cerrado, glad-l, glad-s2, radd, integrated; keys: from, to, map (a part of its name), mapfile (a map
    /// of dates from disk), dates (its encoding: auto, decimal, yearday, ymd, days), files (paths
    /// separated by ';'), download=1 (the rasters of the map's area), url, layer, date, class (another WFS), tab=0
    /// (the panel left as it is).
    pub(crate) fn alerts_option(&mut self, arg: &str, ctx: &egui::Context) -> Result<(), String> {
        let mut parts = arg.split(',');
        let source = parts.next().unwrap_or("").trim().to_ascii_lowercase();
        let kv: Vec<(String, String)> = parts
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let get = |k: &str| kv.iter().find(|x| x.0 == k).map(|x| x.1.clone());
        if let Some(f) = get("mapfile") {
            self.alerts.map_file = Some(f);
        }
        if let Some(d) = get("dates") {
            self.alerts.map_code = match d.to_ascii_lowercase().as_str() {
                "auto" => DateCode::Auto,
                "decimal" | "years" => DateCode::DecimalYear,
                "yearday" | "yyyyddd" => DateCode::YearDay,
                "ymd" | "yyyymmdd" => DateCode::Ymd,
                "days" => DateCode::Days1970,
                _ => return Err(format!("dates={d}: auto, decimal, yearday, ymd or days")),
            };
        }
        let maps = self.alert_maps();
        let map = match (get("map"), get("mapfile")) {
            (Some(m), _) => maps.iter().find(|x| x.1.to_lowercase().contains(&m.to_lowercase())),
            (None, Some(f)) => maps.iter().find(|x| x.0 == f),
            (None, None) => maps.first(),
        };
        let a = &mut self.alerts;
        a.map = map.ok_or("no map of change dates with that name")?.0.clone();
        a.from = get("from").unwrap_or_default();
        a.to = get("to").unwrap_or_default();
        match source.as_str() {
            "deter-amz" | "deter" | "deter-cerrado" => {
                (a.from_kind, a.preset) = (AlertFrom::Server, usize::from(source == "deter-cerrado"));
            }
            "wfs" | "oapif" => {
                a.from_kind = AlertFrom::Server;
                a.server = Some(WfsAlerts {
                    name: get("name").unwrap_or("Alerts".into()),
                    service: Service::from_id(&source).unwrap_or(Service::Wfs),
                    url: get("url").ok_or("url= is needed")?,
                    layer: get("layer").ok_or("layer= is needed")?,
                    date_field: get("date").unwrap_or("view_date".into()),
                    class_field: get("class").unwrap_or_default(),
                });
            }
            s => {
                let product = Product::from_id(s).ok_or(format!("{s}: not a source of alerts"))?;
                (a.from_kind, a.product) = (AlertFrom::Rasters, product);
                if let Some(f) = get("files") {
                    let paths: Vec<String> =
                        f.split(';').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect();
                    a.files = pair_files(product, &paths);
                } else if get("download").is_some_and(|v| v == "1") {
                    let grid = janus_validation::read_grid(&a.map)?;
                    let (from, to) = window_of(&a.from, &a.to)?;
                    a.files = alerts::remote_files(product, alerts::lonlat_bounds(&grid)?, from, to)?;
                }
            }
        }
        if get("tab").is_none_or(|v| v != "0") {
            // the comparison in front: the run's tool window (--zeit) and Tasks closed
            for t in self.zeit.tools.values_mut() {
                t.open = false;
            }
            self.zeit.show_tasks = false;
            self.open_alerts();
        }
        self.alerts_compare(ctx)
    }

    /// Ready for a screenshot: --alerts applied, its comparison done and its maps loaded.
    pub(crate) fn alerts_settled(&self) -> bool {
        self.opts.alerts.is_empty()
            || self.alerts.cli_done
                && !self.alerts.busy()
                && self.zeit.pending_loads == 0
                && self.web_ready()
                && self.vector_loads.is_empty()
    }

    /// The Alerts tab.
    pub(crate) fn alerts_ui(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("A map of change dates against official alerts, over the area both cover").strong());
        ui.weak(
            "Where and when they agree: the area of change in both, in one only; the alerts the map\n\
             detects and the map's change the alerts confirm; the lag between their dates.",
        );
        ui.add_space(4.0);
        let maps = self.alert_maps();
        if !maps.iter().any(|m| m.0 == self.alerts.map) {
            self.alerts.map = maps.first().map(|m| m.0.clone()).unwrap_or_default();
        }
        let busy = self.alerts.busy();
        egui::Grid::new("alerts_in").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Map of change");
            ui.horizontal(|ui| {
                if maps.is_empty() {
                    ui.weak("No map of dates yet: run LandTrendr, CCDC, BFAST or CODED, or choose a file.");
                } else {
                    let shown = maps.iter().find(|m| m.0 == self.alerts.map).map_or("", |m| m.1.as_str());
                    egui::ComboBox::from_id_salt("alerts_map").selected_text(shown).width(300.0).show_ui(ui, |ui| {
                        for (p, n) in &maps {
                            ui.selectable_value(&mut self.alerts.map, p.clone(), n).on_hover_text(p);
                        }
                    });
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("File..."))
                    .on_hover_text("A map of change dates from disk (band 1; 0 or no data: no change)")
                    .clicked()
                    && let Some(f) = rfd::FileDialog::new().add_filter("Raster", &["tif", "tiff", "vrt", "img"]).pick_file()
                {
                    let f = f.to_string_lossy().into_owned();
                    self.alerts.map = f.clone();
                    self.alerts.map_file = Some(f);
                }
            });
            ui.end_row();
            if !self.alerts.map.is_empty() && !self.zeit.results.iter().any(|r| r.path == self.alerts.map) {
                ui.label("Its dates as");
                let a = &mut self.alerts;
                egui::ComboBox::from_id_salt("alerts_code").selected_text(a.map_code.name()).show_ui(ui, |ui| {
                    for c in DateCode::ALL {
                        ui.selectable_value(&mut a.map_code, c, c.name());
                    }
                })
                .response
                .on_hover_text(
                    "How the map's values give its dates. Detected: from its largest value (up to 3000 decimal                      years, 1000001-3000366 a year and day, 10000101-30001231 YYYYMMDD, else days since 1970)",
                );
                ui.end_row();
            }
            ui.label("Alerts");
            ui.horizontal(|ui| {
                let a = &mut self.alerts;
                ui.radio_value(&mut a.from_kind, AlertFrom::Server, "DETER (INPE)")
                    .on_hover_text("INPE's daily deforestation and degradation alerts, polygons with a date and a class,\nfetched from TerraBrasilis (WFS) for the map's area");
                ui.radio_value(&mut a.from_kind, AlertFrom::Vector, "A vector layer")
                    .on_hover_text("Polygons with a date (and a class) already open: alerts from a file, or a web layer");
                ui.radio_value(&mut a.from_kind, AlertFrom::Rasters, "Alert rasters")
                    .on_hover_text("GLAD-L, GLAD-S2, RADD or GFW's integrated alerts: rasters with an encoded date and confidence");
            });
            ui.end_row();
            match self.alerts.from_kind {
                AlertFrom::Server => {
                    ui.label("");
                    ui.horizontal(|ui| {
                        let a = &mut self.alerts;
                        if let Some(s) = &a.server {
                            ui.label(format!("{} ({})", s.name, s.layer)).on_hover_text(&s.url);
                            if ui.small_button("DETER").on_hover_text("Back to INPE's DETER").clicked() {
                                a.server = None;
                            }
                        } else {
                            for (k, p) in deter_presets().iter().enumerate() {
                                ui.radio_value(&mut a.preset, k, &p.name).on_hover_text(format!("{}\n{}", p.url, p.layer));
                            }
                        }
                    });
                    ui.end_row();
                }
                AlertFrom::Vector => self.alerts_vector_rows(ui),
                AlertFrom::Rasters => self.alerts_raster_rows(ui),
            }
            ui.label("Dates");
            ui.horizontal(|ui| {
                let a = &mut self.alerts;
                ui.label("from");
                ui.add(egui::TextEdit::singleline(&mut a.from).hint_text("YYYY-MM-DD").desired_width(90.0));
                ui.label("to");
                ui.add(egui::TextEdit::singleline(&mut a.to).hint_text("YYYY-MM-DD").desired_width(90.0));
                ui.weak("(empty: open)").on_hover_text(
                    "A map pixel counts as change when its date is in the window, an alert when its date is:\n\
                     each is compared over the same window",
                );
            });
            ui.end_row();
            ui.label("Lags' bins");
            ui.add(egui::DragValue::new(&mut self.alerts.bin_days).range(1..=365).suffix(" days"));
            ui.end_row();
        });
        ui.horizontal(|ui| {
            let ok = !busy && !maps.is_empty();
            if ui
                .add_enabled(ok, egui::Button::new("Compare"))
                .on_hover_text("In a task (Tools > Tasks); the agreement map joins the layers")
                .clicked()
                && let Err(e) = self.alerts_compare(ui.ctx())
            {
                self.alerts.error = e;
            }
            if busy {
                ui.spinner();
            }
            if !self.alerts.status.is_empty() {
                ui.weak(&self.alerts.status);
            }
        });
        if !self.alerts.error.is_empty() {
            ui.colored_label(crate::theme::error(), &self.alerts.error);
        }
        if let Some(d) = self.alerts.done.clone() {
            ui.separator();
            self.alerts_result_ui(ui, &d);
        }
    }

    fn alerts_vector_rows(&mut self, ui: &mut Ui) {
        let layers: Vec<(String, String, Vec<String>)> = self
            .vectors
            .iter()
            .filter(|v| v.data.areas > 0)
            .map(|v| (v.data.path.clone(), v.name.clone(), v.data.fields.clone()))
            .collect();
        ui.label("Layer");
        if layers.is_empty() {
            ui.weak("No polygons open: File > Add vector layer..., or Add web layer...");
            ui.end_row();
            return;
        }
        let a = &mut self.alerts;
        if !layers.iter().any(|l| l.0 == a.vector) {
            a.vector = layers[0].0.clone();
            (a.date_field, a.class_field) = (String::new(), String::new());
        }
        let k = layers.iter().position(|l| l.0 == a.vector).unwrap_or(0);
        egui::ComboBox::from_id_salt("alerts_vec").selected_text(&layers[k].1).width(300.0).show_ui(ui, |ui| {
            for (p, n, _) in &layers {
                ui.selectable_value(&mut a.vector, p.clone(), n).on_hover_text(p);
            }
        });
        ui.end_row();
        let fields = &layers[k].2;
        // DETER's names guessed
        if a.date_field.is_empty() {
            a.date_field = fields
                .iter()
                .find(|f| f.eq_ignore_ascii_case("view_date"))
                .or_else(|| fields.iter().find(|f| f.to_lowercase().contains("date")))
                .cloned()
                .unwrap_or_default();
            a.class_field = fields.iter().find(|f| f.eq_ignore_ascii_case("classname")).cloned().unwrap_or_default();
        }
        ui.label("Fields");
        ui.horizontal(|ui| {
            ui.label("date");
            egui::ComboBox::from_id_salt("alerts_date").selected_text(&a.date_field).show_ui(ui, |ui| {
                for f in fields {
                    ui.selectable_value(&mut a.date_field, f.clone(), f);
                }
            });
            ui.label("class");
            let shown = if a.class_field.is_empty() { "(none)" } else { a.class_field.as_str() };
            egui::ComboBox::from_id_salt("alerts_class").selected_text(shown).show_ui(ui, |ui| {
                ui.selectable_value(&mut a.class_field, String::new(), "(none)");
                for f in fields {
                    ui.selectable_value(&mut a.class_field, f.clone(), f);
                }
            });
        });
        ui.end_row();
    }

    fn alerts_raster_rows(&mut self, ui: &mut Ui) {
        let a = &mut self.alerts;
        ui.label("Product");
        let before = a.product;
        egui::ComboBox::from_id_salt("alerts_product").selected_text(a.product.name()).width(300.0).show_ui(ui, |ui| {
            for p in Product::ALL {
                ui.selectable_value(&mut a.product, p, p.name()).on_hover_text(alerts::product_help(p));
            }
        });
        if a.product != before {
            a.files.clear();
        }
        ui.end_row();
        ui.label("Files");
        ui.vertical(|ui| {
            for f in &a.files {
                let name = |p: &str| {
                    std::path::Path::new(p).file_name().map_or(p.to_string(), |f| f.to_string_lossy().into_owned())
                };
                // GLAD-S2's two files share their name (alert/T.tif, alertDate/T.tif): their folders too
                let with_dir = |p: &str| {
                    let p = std::path::Path::new(p);
                    match p.parent().and_then(|d| d.file_name()) {
                        Some(d) => format!("{}/{}", d.to_string_lossy(), name(&p.to_string_lossy())),
                        None => name(&p.to_string_lossy()),
                    }
                };
                let same = !f.date.is_empty() && name(&f.conf) == name(&f.date);
                let mut t = if same { with_dir(&f.conf) } else { name(&f.conf) };
                if !f.date.is_empty() {
                    t += &format!(" + {}", if same { with_dir(&f.date) } else { name(&f.date) });
                }
                if f.year > 0 && a.product == Product::GladL {
                    t += &format!(" ({})", f.year);
                }
                ui.label(t).on_hover_text(format!("{}\n{}", f.conf, f.date));
            }
            ui.horizontal(|ui| {
                if ui
                    .button("Add files...")
                    .on_hover_text(if a.product.two_files() {
                        "GLAD's GeoTIFFs: each alert file with its file of dates (alertDate...)"
                    } else {
                        "GeoTIFFs of the encoded alerts (GFW's date_conf)"
                    })
                    .clicked()
                    && let Some(fs) = rfd::FileDialog::new().add_filter("GeoTIFF", &["tif", "tiff"]).pick_files()
                {
                    let mut paths: Vec<String> =
                        a.files.iter().flat_map(|f| [f.conf.clone(), f.date.clone()]).collect();
                    paths.retain(|p| !p.is_empty());
                    paths.extend(fs.iter().map(|p| p.to_string_lossy().into_owned()));
                    paths.dedup();
                    a.files = pair_files(a.product, &paths);
                }
                if !a.files.is_empty() && ui.button("Clear").clicked() {
                    a.files.clear();
                }
                let can = alerts::remote_supported(a.product);
                if ui
                    .add_enabled(can.is_ok(), egui::Button::new("Of the map's area"))
                    .on_hover_text(can.clone().unwrap_or_else(|e| e))
                    .on_disabled_hover_text(can.err().unwrap_or_default())
                    .clicked()
                {
                    let r = janus_validation::read_grid(&a.map).and_then(|g| {
                        let (from, to) = window_of(&a.from, &a.to)?;
                        alerts::remote_files(a.product, alerts::lonlat_bounds(&g)?, from, to)
                    });
                    match r {
                        Ok(f) => a.files = f,
                        Err(e) => a.error = e,
                    }
                }
            });
        });
        ui.end_row();
    }

    fn alerts_result_ui(&mut self, ui: &mut Ui, d: &Done) {
        let c = &d.cmp;
        let pct = |v: f64| if v.is_finite() { format!("{:.1} %", 100.0 * v) } else { "-".into() };
        let unit = c.unit.clone();
        ui.label(RichText::new(format!("{} against {}", d.alerts_name, d.map_name)).strong())
            .on_hover_text(&d.alerts_source);
        ui.weak(format!(
            "{} {unit} both cover ({} pixels), {} to {}",
            fmt_area(c.covered_ha),
            c.covered_pixels,
            if c.from > i32::MIN + 1 { ymd(c.from) } else { "the first date".into() },
            if c.to < i32::MAX { ymd(c.to) } else { "the last".into() },
        ));
        egui::Grid::new("alerts_areas").num_columns(3).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
            ui.label("");
            ui.label(RichText::new(&unit).strong());
            ui.label(RichText::new("pixels").strong());
            ui.end_row();
            for (name, ha, px) in [
                ("Change in the map", c.map_ha(), c.both + c.only_map),
                ("Change in the alerts", c.alerts_ha(), c.both + c.only_alerts),
                ("In both", c.both_ha, c.both),
                ("Only in the map", c.only_map_ha, c.only_map),
                ("Only in the alerts", c.only_alerts_ha, c.only_alerts),
            ] {
                ui.label(name);
                ui.label(fmt_area(ha));
                ui.label(px.to_string());
                ui.end_row();
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Agreement").strong());
        egui::Grid::new("alerts_rates").num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
            ui.label("Alerts the map detects").on_hover_text(
                "Pixel by pixel: the share of the alerts' area the map changed. By alert: an alert is detected \
                 when the map changed at one of its pixels at least (in the window)",
            );
            ui.label(format!(
                "{} of their area; {} of {} alerts ({})",
                pct(c.alerts_detected_share()),
                c.alerts_detected,
                c.alerts,
                pct(c.alerts_detected_count_share())
            ));
            ui.end_row();
            ui.label("The detected alerts' area")
                .on_hover_text("The area of the alerts detected (all their pixels), over all the alerts' area");
            ui.label(format!("{} of the alerts' area", pct(c.alerts_detected_area_share())));
            ui.end_row();
            ui.label("Map change the alerts confirm").on_hover_text(
                "A patch of the map's change (8-connected) is confirmed when one of its pixels is an alert's",
            );
            ui.label(format!(
                "{} of its area; {} of {} patches ({})",
                pct(c.map_confirmed_share()),
                c.map_patches_confirmed,
                c.map_patches,
                pct(c.map_confirmed_count_share())
            ));
            ui.end_row();
            ui.label("Dice (F1)").on_hover_text("2 x both / (map + alerts), by area");
            ui.label(if c.dice().is_finite() { format!("{:.3}", c.dice()) } else { "-".into() });
            ui.end_row();
        });
        let mut notes = Vec::new();
        if c.alerts_too_small > 0 {
            notes.push(format!(
                "{} alert(s) of the window hold no pixel's centre (smaller than a pixel, or under earlier alerts)",
                c.alerts_too_small
            ));
        }
        if d.skipped > 0 {
            notes.push(format!("{} feature(s) without a date or a geometry left out", d.skipped));
        }
        for n in notes {
            ui.weak(n);
        }
        ui.add_space(4.0);
        let l = &c.lag;
        ui.label(RichText::new("Lag: the map's date minus the alert's").strong());
        if l.n == 0 {
            ui.weak("No pixel changed in both.");
        } else {
            ui.label(format!(
                "Over {} pixels: median {:.0} days (25-75 %: {:.0} to {:.0}; 10-90 %: {:.0} to {:.0}), mean {:.1}",
                l.n, l.median, l.q25, l.q75, l.q10, l.q90, l.mean
            ));
            ui.weak(format!(
                "Within 30 days: {}, 90 days: {}, a year: {} (either way)",
                pct(l.within_30),
                pct(l.within_90),
                pct(l.within_365)
            ));
            if c.map_years_only {
                ui.colored_label(
                    crate::theme::warning(),
                    "The map gives whole years: its dates are taken as 1 January, so its lags are within a year at best.",
                );
            }
            let fill = crate::theme::accent();
            Plot::new("alerts_lags")
                .height(150.0)
                .x_axis_label("days (the map later: positive)")
                .y_axis_label("pixels")
                .allow_drag(false)
                .allow_scroll(false)
                .show(ui, |pu| {
                    let bars: Vec<Bar> = c
                        .histogram
                        .iter()
                        .map(|&(a, b, n)| {
                            Bar::new(f64::from(a + b) / 2.0, n as f64).width(f64::from(b - a) * 0.95).fill(fill)
                        })
                        .collect();
                    pu.bar_chart(BarChart::new("lags", bars).color(fill));
                });
        }
        if d.classes.len() > 1 || !d.classes.first().is_none_or(|n| n == "(no class)") {
            ui.add_space(4.0);
            ui.label(RichText::new("By class").strong());
            egui::Grid::new("alerts_classes").num_columns(6).striped(true).spacing([12.0, 3.0]).show(ui, |ui| {
                for h in ["Class", "Alerts", "Detected", &format!("Area ({unit})"), "Detected area", "Median lag"] {
                    ui.label(RichText::new(h).strong());
                }
                ui.end_row();
                // (the classes without an alert in the window are in the CSV only)
                for k in c.classes.iter().filter(|k| k.alerts > 0) {
                    ui.label(&k.name);
                    ui.label(k.alerts.to_string());
                    ui.label(format!("{} ({})", k.detected, pct(k.detected as f64 / k.alerts as f64)));
                    ui.label(fmt_area(k.ha));
                    ui.label(format!("{} ({})", fmt_area(k.detected_ha), pct(k.detected_ha / k.ha)));
                    ui.label(if k.lag_median.is_finite() { format!("{:.0} d", k.lag_median) } else { "-".into() });
                    ui.end_row();
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui
                .button("Export CSV...")
                .on_hover_text(
                    "The summary, the classes, every alert and the lags' histogram as CSVs, and the agreement map",
                )
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.alerts_export(d, &dir);
            }
            ui.weak(format!("{:.2} s", c.seconds)).on_hover_text(d.folder.to_string_lossy());
        });
    }

    /// The CSVs and the agreement map written to `dir`.
    pub(crate) fn alerts_export(&mut self, d: &Done, dir: &std::path::Path) {
        let stem = janus_cube::stac_vrt::sanitize(&d.alerts_name).to_lowercase();
        match d.cmp.export(dir, &stem, &d.map_name, &d.alerts) {
            Ok(files) => self.alerts.status = format!("Written {} files to {}", files.len(), dir.display()),
            Err(e) => self.alerts.error = e,
        }
    }
}

/// The window from its texts (empty: open).
fn window_of(from: &str, to: &str) -> Result<(i32, i32), String> {
    let f = if from.trim().is_empty() {
        i32::MIN + 1
    } else {
        parse_day(from).ok_or(format!("{from}: not a date (YYYY-MM-DD)"))?
    };
    let t =
        if to.trim().is_empty() { i32::MAX } else { parse_day(to).ok_or(format!("{to}: not a date (YYYY-MM-DD)"))? };
    if t < f {
        return Err("the window ends before it starts".into());
    }
    Ok((f, t))
}

/// The comparison: the map read, the alerts placed on its grid, compared; the agreement (and the alerts' dates)
/// written in `dir`.
#[allow(clippy::too_many_arguments)]
fn run(
    map: &str,
    code: DateCode,
    what: &Alerts,
    o: &CompareOptions,
    threads: usize,
    dir: &std::path::Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
    log: &JobLog,
) -> Result<(Comparison, AlertsOnGrid, String, Option<String>), String> {
    progress(0.0, "reading the map");
    let (grid, change, used) = alerts::read_change_map_as(map, code, threads)?;
    log.line(&format!("the map's dates: {}", used.name()));
    if !change.iter().any(|v| v.is_finite()) {
        return Err(format!("no dates in the map read as {}", used.name()));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let on_grid = match what {
        Alerts::Vector { path, layer, date_field, class_field, .. } => {
            progress(0.2, "placing the alerts on the map's grid");
            alerts::alerts_from_vector(path, layer, date_field, class_field, &grid)?
        }
        Alerts::Rasters { product, files } => {
            let n = files.len();
            alerts::alerts_from_rasters(*product, files, &grid, threads, &|k| {
                progress(0.1 + 0.6 * k as f64 / n.max(1) as f64, &format!("reading file {} of {n}", k + 1));
            })?
        }
    };
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    progress(0.8, "comparing");
    let cmp = alerts::compare(&change, &on_grid, o);
    let agreement = dir.join("agreement.tif").to_string_lossy().into_owned();
    alerts::write_u8(&agreement, &grid, &cmp.agreement, &AGREEMENT_CLASSES)?;
    let dates = if on_grid.polygons() {
        None
    } else {
        let p = dir.join("alert_dates.tif").to_string_lossy().into_owned();
        alerts::write_f32(&p, &grid, &on_grid.years())?;
        Some(p)
    };
    cmp.export(dir, "alerts", map, &on_grid)?;
    Ok((cmp, on_grid, agreement, dates))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_glad_files() {
        let p = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let f = pair_files(Product::GladS2, &p(&["d/050W_10S_alert.tif", "d/050W_10S_alertDate.tif"]));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].date, "d/050W_10S_alertDate.tif");
        let f = pair_files(
            Product::GladL,
            &p(&["a/alert21_10S_070W.tif", "a/alertDate21_10S_070W.tif", "a/alert22_10S_070W.tif"]),
        );
        assert_eq!(f.len(), 2);
        assert_eq!((f[0].year, f[0].date.as_str()), (2021, "a/alertDate21_10S_070W.tif"));
        assert_eq!((f[1].year, f[1].date.as_str()), (2022, ""));
        let f = pair_files(Product::Radd, &p(&["x.tif", "y.tif"]));
        assert_eq!(f.len(), 2);
        assert_eq!(window_of("", "").unwrap(), (i32::MIN + 1, i32::MAX));
        assert!(window_of("2021-01-01", "2020-01-01").is_err());
    }
}
