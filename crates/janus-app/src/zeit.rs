//! Zeit's tools in Janus (src/app_zeit.cpp), native: the Tools menu, a window per tool made from its descriptor,
//! the tool fitted on the chart's series (the cursor, the pins, the ROI mean, the other layers') on a worker
//! thread, its model drawn over each series and its numbers in the Statistics table.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use egui::{Color32, ComboBox, Ui};
use egui_plot::{Line, PlotPoints, PlotUi, Points, VLine};
use janus_cube::{CubeInfo, CubeReader};
use janus_zeit::pixel::Shown;
use janus_zeit::{Params, PixelInput, PixelResult, Tool, manifest, run_pixel};
use serde_json::{Value, json};

use crate::app::State;
use crate::theme;

/// Which series of a layer a fit is of.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Which {
    Hover,
    Pin(u32),
    Roi,
}

/// A fit: of a tool (its place in the manifest) on a series of a layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FitKey {
    pub cube: u64,
    pub which: Which,
    pub tool: u16,
}

/// The settings a fit was made with: every tool's (the bands), the tool's own (its parameters, the map's date).
type Version = (u64, u64);

/// A fit asked for or done: of the series at (x, y) with the tool's settings of `version`.
pub struct Fit {
    x: usize,
    y: usize,
    version: Version,
    pub result: Option<Result<PixelResult, String>>,
}

/// A tool's window: open, its parameters, where a raster run goes (0 whole image, 1 visible area, 2 ROI).
#[derive(Default)]
pub struct ToolUi {
    pub open: bool,
    pub params: BTreeMap<String, Value>,
    pub scope: usize,
    /// bumped when its parameters change: its fits are made again
    pub version: u64,
    /// its colour and line on the chart (the first free when it is fitted)
    pub slot: usize,
    /// the run's time measured on a sample (for the series, parameters and bands of `est_key`), the measure under
    /// way, and when they changed last (measured once they stay for a moment)
    est: Option<Result<janus_zeit::job::Estimate, String>>,
    est_key: String,
    est_job: Option<Receiver<Result<janus_zeit::job::Estimate, String>>>,
    est_changed: Option<Instant>,
    /// Run again: the run's own window on its series (scope 3), and what to know when it was on another series
    pub again: Option<(u64, [usize; 4])>,
    pub again_note: String,
    /// a preview again when a parameter changes (if it takes a few seconds), and when one was asked so
    pub auto_preview: bool,
    pub preview_asked: Option<Instant>,
    /// Only where...: a layer on the series' grid (its cube id) whose pixels 0 or without a value are left out
    pub mask: Option<u64>,
    /// the threads its runs use, set in its window (None: the Settings')
    pub threads: Option<usize>,
}

/// A preview's side at most, in pixels (without an ROI: at the centre of the view).
pub const PREVIEW_SIDE: usize = 256;
/// Auto-preview runs again only when a preview is estimated to take at most this long.
const AUTO_PREVIEW_SECONDS: f64 = 3.0;

/// The tool's run on a small window before the whole scene: its folder (in the work folder, replaced by the next
/// preview), the series and window it ran on, its tool's parameters' version.
pub struct Preview {
    pub tool: String,
    pub dir: String,
    pub cube: u64,
    pub window: [usize; 4],
    pub version: u64,
}

/// The bands a multiband fit reads for its pixel (in the worker).
struct BandRead {
    info: Arc<CubeInfo>,
    roles: Vec<(String, i32)>,
    qa_band: i32,
}

struct Request {
    key: FitKey,
    x: usize,
    y: usize,
    version: Version,
    tool: String,
    params: Params,
    input: PixelInput,
    bands: Option<BandRead>,
}

struct Reply {
    key: FitKey,
    x: usize,
    y: usize,
    version: Version,
    result: Result<PixelResult, String>,
    ms: f64,
}

/// A result's copy at full resolution: its path, values, width and height.
type FullCopy = (String, Vec<f32>, usize, usize);

#[derive(Default)]
pub struct ZeitUi {
    pub tools: HashMap<String, ToolUi>,
    /// the tools fitted on the chart, in the order they were turned on (a tool opened is fitted)
    pub pixel_tools: Vec<String>,
    /// bumped when the bands change: every fit is made again
    pub version: u64,
    pub fits: HashMap<FitKey, Fit>,
    /// the active layer's bands for each role (multiband tools), and the cube they were guessed for
    pub roles: BTreeMap<String, i32>,
    roles_for: u64,
    /// the map's date a tool on it (CCDC's synthetic image) was fitted for
    map_t: Option<usize>,
    tx: Option<Sender<Request>>,
    rx: Option<Receiver<Reply>>,
    /// the last fit's time (Performance)
    pub last_fit_ms: f64,
    /// the raster runs, their result maps, the Tasks window
    pub jobs: Vec<Job>,
    /// the maps the runs made: layers of their own, kept sorted by their place in the drawing order (`z`)
    pub results: Vec<ResultLayer>,
    /// the last result's number
    pub next_result: u32,
    pub show_tasks: bool,
    /// Tools > Tasks > Log: a run's log, followed
    pub log: LogView,
    /// where runs write (None: the app's results folder)
    pub results_dir: Option<std::path::PathBuf>,
    /// series outputs to open as layers, and the layer to keep active meanwhile
    pub series_to_open: Vec<(String, bool)>,
    pub keep_active: Option<usize>,
    /// the series a run wrote, being opened, goes hidden
    pub open_hidden: bool,
    /// the last preview of a tool (its maps are result layers marked preview)
    pub preview: Option<Preview>,
    /// result maps read (on threads)
    loaded: Arc<std::sync::Mutex<Vec<Result<ResultLayer, String>>>>,
    /// shown results' copies at full resolution read (path, data, width, height)
    upgraded: Arc<std::sync::Mutex<Vec<FullCopy>>>,
    pending_loads: usize,
}

/// The worker: the newest request of each series, in turn; a multiband pixel's bands read first.
fn worker(rx: Receiver<Request>, tx: Sender<Reply>, ctx: egui::Context) {
    let mut readers: HashMap<u64, CubeReader> = HashMap::new();
    while let Ok(first) = rx.recv() {
        let mut queue = vec![first];
        queue.extend(rx.try_iter());
        // the newest of each series only
        let mut newest: Vec<Request> = Vec::new();
        for r in queue {
            newest.retain(|q| q.key != r.key);
            newest.push(r);
        }
        for mut r in newest {
            let t0 = Instant::now();
            if let Some(b) = &r.bands {
                let reader = readers.entry(b.info.id).or_insert_with(|| CubeReader::new(b.info.clone()));
                let n = b.info.t();
                let mut read = |band: i32, raw: bool| -> Vec<f64> {
                    (0..n)
                        .map(|t| reader.read_band_pixel(t, band, r.x, r.y, raw).map_or(f64::NAN, f64::from))
                        .map(|v| if v.is_finite() { v } else { f64::NAN })
                        .collect()
                };
                r.input.bands = b.roles.iter().map(|(role, band)| (role.clone(), read(*band, false))).collect();
                if b.qa_band > 0 {
                    r.input.qa = Some(read(b.qa_band, true));
                }
            }
            let result = run_pixel(&r.tool, &r.params, &r.input);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if tx.send(Reply { key: r.key, x: r.x, y: r.y, version: r.version, result, ms }).is_err() {
                return;
            }
            ctx.request_repaint();
        }
    }
}

/// Why a tool cannot run on a series, if it cannot (the C++'s toolApplicability).
pub fn applicability(tool: &Tool, info: &CubeInfo) -> Option<String> {
    if tool.input.as_deref() == Some("layers") {
        return None; // runs on result maps, whatever the series
    }
    if janus_embed::embedding_meta(info).is {
        // as Zeit itself, which refuses such cubes
        return Some(
            "a layer of embeddings has no physical unit nor seasonal signal (see the Embeddings panel)".into(),
        );
    }
    if info.spectral != (tool.requires.time == "spectrum") {
        return Some(if info.spectral {
            "a spectrum (one band per wavelength), not a series in time".into()
        } else {
            "needs a scene of one band per wavelength (a hyperspectral image)".into()
        });
    }
    if tool.requires.all_bands == Some(true) && info.bands_per_date < 2 {
        return Some("needs one file per date with several bands (e.g. surface reflectance)".into());
    }
    let bands = tool.requires.bands.clone().unwrap_or_default();
    if !bands.is_empty() && info.bands_per_date < bands.len() {
        return Some(format!(
            "needs one file per date with several bands: {} (e.g. Landsat surface reflectance)",
            bands.join(", ")
        ));
    }
    let no_dates = "needs dates (none were found in the file names or band descriptions)";
    if !bands.is_empty() && !info.time_is_date {
        return Some(no_dates.into());
    }
    let n = info.t();
    if (n as i32) < tool.requires.min_dates {
        return Some(format!("needs at least {} dates (this series has {n})", tool.requires.min_dates));
    }
    if tool.requires.time == "annual" {
        if !info.time_is_date {
            return Some(no_dates.into());
        }
        let mut years: Vec<i64> = (0..n).map(|t| info.decimal_year(t).floor() as i64).collect();
        years.sort_unstable();
        years.dedup();
        if years.len() != n {
            return Some("needs an annual series (one date per year)".into());
        }
    }
    let min_per_year = tool.requires.min_per_year.unwrap_or(0);
    if tool.requires.time == "regular" || min_per_year > 0 {
        if !info.time_is_date {
            return Some(no_dates.into());
        }
        // the bridge's rule: observations per year from the median spacing
        let mut gaps: Vec<f64> = (1..n).map(|t| info.decimal_year(t) - info.decimal_year(t - 1)).collect();
        gaps.sort_by(f64::total_cmp);
        let med = gaps.get(gaps.len() / 2).copied().unwrap_or(0.0);
        if med <= 0.0 {
            return Some("needs increasing dates".into());
        }
        let per_year = ((1.0 / med).round() as i32).max(1);
        if per_year < min_per_year {
            return Some(format!("needs at least {min_per_year} observations per year (this series has {per_year})"));
        }
        // months (28-31 days) or 16-day composites wrapping at the year's end pass
        if tool.requires.time == "regular" && (gaps[0] < 0.4 * med || gaps[gaps.len() - 1] > 1.6 * med) {
            return Some("needs evenly spaced dates (missing dates must be no-data bands, not skipped)".into());
        }
    }
    None
}

/// The required and optional band roles of a tool.
fn tool_roles(tool: &Tool) -> (Vec<String>, Vec<String>) {
    (tool.requires.bands.clone().unwrap_or_default(), tool.requires.optional_bands.clone().unwrap_or_default())
}

/// Whether a tool reads bands of each date besides the shown series (multiband tools).
fn reads_bands(tool: &Tool) -> bool {
    tool.requires.all_bands == Some(true) || tool.requires.bands.as_ref().is_some_and(|b| !b.is_empty())
}

/// The bands a multiband tool reads, (role, band of each date): every band (b001, b002...) for a tool on all of
/// them, else its roles' mapped bands.
fn tool_bands(tool: &Tool, info: &CubeInfo, roles: &BTreeMap<String, i32>) -> Vec<(String, i32)> {
    if tool.requires.all_bands == Some(true) {
        return (1..=info.bands_per_date as i32).map(|b| (format!("b{b:03}"), b)).collect();
    }
    let (needed, optional) = tool_roles(tool);
    needed.iter().chain(&optional).filter_map(|r| roles.get(r).map(|&b| (r.clone(), b))).collect()
}

/// What the shown series is in band roles (multiband series).
fn shown_of(info: &CubeInfo, roles: &BTreeMap<String, i32>) -> Option<Shown> {
    if info.bands_per_date <= 1 {
        return None;
    }
    let role_of = |b: i32| roles.iter().find(|(_, v)| **v == b).map(|(r, _)| r.clone());
    let sel = &info.sel;
    Some(if info.expr.is_some() {
        // an expression: neither a band nor a normalized difference of the roles
        Shown { band: None, nd: None }
    } else if sel.nd_band > 0 {
        Shown { band: None, nd: role_of(sel.band).zip(role_of(sel.nd_band)).map(|(a, b)| vec![a, b]) }
    } else {
        Shown { band: role_of(sel.band), nd: None }
    })
}

/// A series of a layer as a pixel run gets it.
fn pixel_input(info: &CubeInfo, values: &[f32], t: usize, roles: &BTreeMap<String, i32>) -> PixelInput {
    // a spectrum's axis: the wavelengths (nm), as the chart draws it
    let x = |i: usize| if info.spectral { info.layers[i].time } else { info.decimal_year(i) };
    PixelInput {
        years: (0..info.t()).map(x).collect(),
        days: info.time_is_date.then(|| (0..info.t()).map(|i| info.unix_day(i)).collect()),
        values: values.iter().map(|&v| if v.is_finite() { f64::from(v) } else { f64::NAN }).collect(),
        shown: shown_of(info, roles),
        map_year: Some(x(t.min(info.t().saturating_sub(1)))),
        qa_rule: (info.sel.qa_band > 0).then(|| info.sel.qa_rule.id().to_string()),
        ..Default::default()
    }
}

/// A tool's place in the manifest (a fit's key, a panel's tab).
pub fn tool_index(id: &str) -> u16 {
    manifest().tools.iter().position(|t| t.id == id).unwrap_or(0) as u16
}

/// The colours of the tools fitted on the chart, each with its line (solid, dashed, dotted...): none a series'
/// (the pins' are vivid, the ROI's yellow, the date's orange), over a dark outline.
const FIT_COLOURS: [Color32; 6] = [
    Color32::from_rgb(242, 38, 242),
    Color32::from_rgb(236, 236, 236),
    Color32::from_rgb(0, 214, 255),
    Color32::from_rgb(196, 255, 0),
    Color32::from_rgb(255, 120, 60),
    Color32::from_rgb(150, 120, 255),
];

/// A fitted tool's look on the chart: its colour and its line.
#[derive(Clone, Copy)]
pub struct FitStyle {
    pub color: Color32,
    pub line: egui_plot::LineStyle,
}

impl FitStyle {
    pub fn of(slot: usize) -> Self {
        let line = match slot % 4 {
            0 => egui_plot::LineStyle::Solid,
            1 => egui_plot::LineStyle::Dashed { length: 9.0 },
            2 => egui_plot::LineStyle::Dotted { spacing: 6.0 },
            _ => egui_plot::LineStyle::dashed_dense(),
        };
        FitStyle { color: theme::on_plot(FIT_COLOURS[slot % FIT_COLOURS.len()]), line }
    }
}

/// A fit of a series by one of the tools on the chart (None: asked for, not done yet).
pub struct ToolFit<'a> {
    pub tool: &'static Tool,
    pub style: FitStyle,
    pub result: Option<&'a Result<PixelResult, String>>,
}

impl ZeitUi {
    /// Whether the tool is fitted on the chart.
    pub fn fitted(&self, id: &str) -> bool {
        self.pixel_tools.iter().any(|t| t == id)
    }

    /// The tool fitted on the chart or not; fitted, it takes the first colour no other fitted tool has.
    pub fn set_fitted(&mut self, id: &str, on: bool) {
        if on == self.fitted(id) {
            return;
        }
        if on {
            let used: Vec<usize> = self.pixel_tools.iter().filter_map(|t| self.tools.get(t)).map(|u| u.slot).collect();
            let slot = (0..).find(|k| !used.contains(k)).unwrap_or(0);
            let u = self.tools.entry(id.to_string()).or_default();
            u.slot = slot;
            u.version += 1;
            self.pixel_tools.push(id.to_string());
        } else {
            self.pixel_tools.retain(|t| t != id);
            let k = tool_index(id);
            self.fits.retain(|key, _| key.tool != k);
        }
    }

    /// Only this tool on the chart (the tours, the tests, --zeit).
    pub fn fit_only(&mut self, id: Option<&str>) {
        for t in self.pixel_tools.clone() {
            self.set_fitted(&t, false);
        }
        if let Some(id) = id {
            self.set_fitted(id, true);
        }
    }

    /// The settings a tool's fits are made with now.
    fn version_of(&self, id: &str) -> Version {
        (self.version, self.tools.get(id).map_or(0, |u| u.version))
    }

    /// The tools on the chart, in order, with their looks (a tool of the raster only is never on it).
    pub fn chart_tools(&self) -> Vec<(&'static Tool, FitStyle)> {
        self.pixel_tools
            .iter()
            .filter_map(|id| manifest().tool(id))
            .filter(|t| t.modes.iter().any(|m| m == "pixel"))
            .map(|t| (t, FitStyle::of(self.tools.get(&t.id).map_or(0, |u| u.slot))))
            .collect()
    }

    /// A tool's fit of a layer's series, if it is of that place and of the tool's current settings.
    pub fn fit_of(
        &self,
        tool: &str,
        cube: u64,
        which: Which,
        x: usize,
        y: usize,
    ) -> Option<&Result<PixelResult, String>> {
        if !self.fitted(tool) {
            return None;
        }
        let f = self.fits.get(&FitKey { cube, which, tool: tool_index(tool) })?;
        (f.x == x && f.y == y && f.version == self.version_of(tool)).then_some(f.result.as_ref()).flatten()
    }

    /// Every chart tool's fit of a layer's series (the tools' order).
    pub fn fits_of(&self, cube: u64, which: Which, x: usize, y: usize) -> Vec<ToolFit<'_>> {
        self.chart_tools()
            .into_iter()
            .map(|(tool, style)| ToolFit { tool, style, result: self.fit_of(&tool.id, cube, which, x, y) })
            .collect()
    }

    /// Whether a fit of that series is wanted (the tool's settings or the place changed).
    fn wanted(&self, key: FitKey, x: usize, y: usize, version: Version) -> bool {
        self.fits.get(&key).is_none_or(|f| f.x != x || f.y != y || f.version != version)
    }

    fn send(&mut self, ctx: &egui::Context, r: Request) {
        if self.tx.is_none() {
            let (tx, rx) = channel();
            let (rtx, rrx) = channel();
            let c = ctx.clone();
            std::thread::Builder::new().name("zeit-fits".into()).spawn(move || worker(rx, rtx, c)).ok();
            self.tx = Some(tx);
            self.rx = Some(rrx);
        }
        self.fits.insert(r.key, Fit { x: r.x, y: r.y, version: r.version, result: None });
        if let Some(tx) = &self.tx {
            let _ = tx.send(r);
        }
    }

    /// The tool's parameters (its defaults filled in).
    pub fn params(&mut self, tool: &Tool) -> &mut BTreeMap<String, Value> {
        let ui = self.tools.entry(tool.id.clone()).or_default();
        if ui.params.is_empty() {
            ui.params = tool.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
        }
        &mut ui.params
    }
}

impl State {
    /// The active layer's band roles, guessed again for another series.
    fn active_roles(&mut self) -> BTreeMap<String, i32> {
        let Some(s) = &self.session else { return BTreeMap::new() };
        if self.zeit.roles_for != s.info.id {
            self.zeit.roles_for = s.info.id;
            self.zeit.roles =
                janus_cube::roles::guess_band_roles(&s.info).into_iter().map(|(r, b)| (r.to_string(), b)).collect();
        }
        self.zeit.roles.clone()
    }

    /// Every frame: the fits done, and those wanted asked for.
    pub(crate) fn pump_zeit(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.zeit.rx {
            for r in rx.try_iter().collect::<Vec<_>>() {
                if let Some(f) = self.zeit.fits.get_mut(&r.key)
                    && f.x == r.x
                    && f.y == r.y
                    && f.version == r.version
                {
                    f.result = Some(r.result);
                    self.zeit.last_fit_ms = r.ms;
                }
            }
        }
        // tools on the map's date are fitted again when it changes
        if self.zeit.map_t != Some(self.display.t) {
            self.zeit.map_t = Some(self.display.t);
            for id in self.zeit.pixel_tools.clone() {
                if manifest().tool(&id).is_some_and(|t| t.map_date == Some(true)) {
                    self.zeit.tools.entry(id).or_default().version += 1;
                }
            }
        }
        for id in self.zeit.pixel_tools.clone() {
            let Some(tool) = manifest().tool(&id) else { continue };
            for r in self.fit_requests(tool) {
                self.zeit.send(ctx, r);
            }
        }
    }

    /// A tool's fits wanted: of the cursor, the pins and the ROI, and of the other layers' on the chart (none if
    /// the series does not suit it).
    fn fit_requests(&mut self, tool: &'static Tool) -> Vec<Request> {
        let Some(s) = &self.session else { return vec![] };
        let info = s.info.clone();
        if !tool.modes.iter().any(|m| m == "pixel") || applicability(tool, &info).is_some() {
            return vec![];
        }
        let roles = self.active_roles();
        let (needed, _) = tool_roles(tool);
        let multiband = reads_bands(tool);
        if needed.iter().any(|r| !roles.contains_key(r)) {
            return vec![];
        }
        let id = tool.id.clone();
        let params = Params::with_defaults(&id, self.zeit.params(tool)).unwrap_or_default();
        let t = self.display.t;
        let bands = || {
            multiband.then(|| BandRead {
                info: info.clone(),
                roles: tool_bands(tool, &info, &roles),
                qa_band: info.sel.qa_band,
            })
        };
        let mut requests = vec![];
        let version = self.zeit.version_of(&id);
        let k = tool_index(&id);
        let mut ask = |zeit: &ZeitUi,
                       cube: &CubeInfo,
                       which: Which,
                       view: &crate::series::SeriesView,
                       t: usize,
                       b: Option<BandRead>| {
            let key = FitKey { cube: cube.id, which, tool: k };
            if view.exact && zeit.wanted(key, view.x, view.y, version) {
                requests.push(Request {
                    key,
                    x: view.x,
                    y: view.y,
                    version,
                    tool: id.clone(),
                    params: params.clone(),
                    input: pixel_input(cube, &view.values, t, &roles),
                    bands: b,
                });
            }
        };
        if let Some(h) = &self.series.hover {
            ask(&self.zeit, &info, Which::Hover, h, t, bands());
        }
        for p in &self.series.pins {
            ask(&self.zeit, &info, Which::Pin(p.id), p, t, bands());
        }
        // the other visible layers on the chart: single-band tools, on their own dates
        if self.series.chart_layers == 1 && !multiband {
            for (i, l) in self.layers.iter().enumerate() {
                let Some(ls) = l.session.as_ref().filter(|_| i != self.active && l.visible) else { continue };
                if applicability(tool, &ls.info).is_some() {
                    continue;
                }
                if let Some(h) = &l.series.hover {
                    ask(&self.zeit, &ls.info, Which::Hover, h, l.disp.t, None);
                }
                for p in &l.series.pins {
                    ask(&self.zeit, &ls.info, Which::Pin(p.id), p, l.disp.t, None);
                }
            }
        }
        // the ROI: the mean of the shown series (single-band tools)
        let roi = &self.series.roi;
        if !multiband && roi.complete() && !roi.mean.is_empty() {
            let key = FitKey { cube: info.id, which: Which::Roi, tool: k };
            let roi_gen = self.series.roi_gen as usize;
            if self.zeit.wanted(key, roi_gen, 0, version) {
                requests.push(Request {
                    key,
                    x: roi_gen,
                    y: 0,
                    version,
                    tool: id.clone(),
                    params: params.clone(),
                    input: pixel_input(&info, &roi.mean, t, &roles),
                    bands: None,
                });
            }
        }
        requests
    }

    /// The chart tools' fits of the ROI, those of the current ROI.
    pub(crate) fn roi_fits(&self) -> Vec<ToolFit<'_>> {
        let Some(s) = self.session.as_ref() else { return vec![] };
        self.zeit.fits_of(s.info.id, Which::Roi, self.series.roi_gen as usize, 0)
    }

    /// The Tools menu: a submenu per category, its tools beside it (each opens its window); deep learning with
    /// the classifications.
    pub(crate) fn tools_menu(&mut self, ui: &mut Ui) {
        let m = manifest();
        let mut categories: Vec<&str> = vec![];
        for t in &m.tools {
            if !categories.contains(&t.category.as_str()) {
                categories.push(&t.category);
            }
        }
        for c in categories {
            let tools: Vec<&Tool> = m.tools.iter().filter(|t| t.category == c).collect();
            // a category with a tool open is marked
            let any_open = tools.iter().any(|t| self.zeit.tools.get(&t.id).is_some_and(|u| u.open))
                || c == "Classification" && (self.ml.open || self.fields.show);
            let label = format!("{c}{}", if any_open { "  \u{25cf}" } else { "" });
            ui.menu_button(label, |ui| {
                for t in tools {
                    // TWDTW and SOM: methods of the Classification window
                    let method = crate::methods::Kind::of_tool(&t.id);
                    let open = match method {
                        Some(k) => self.ml.open && self.ml.kind == k,
                        None => self.zeit.tools.get(&t.id).is_some_and(|u| u.open),
                    };
                    let mut b = ui.add(egui::Button::selectable(open, &t.name));
                    if !t.description.is_empty() {
                        b = b.on_hover_text(&t.description);
                    }
                    if b.clicked() {
                        match method {
                            Some(k) => {
                                (self.ml.open, self.ml.kind, self.ml.tab) = (!open, k, 1);
                            }
                            None => self.set_tool_open(t, !open),
                        }
                        ui.close();
                    }
                }
                if c == "Classification" {
                    ui.separator();
                    self.ml_menu(ui);
                    self.fields_menu(ui);
                }
            });
        }
        ui.separator();
        if ui
            .add(egui::Button::selectable(self.calc.show, "Raster calculator..."))
            .on_hover_text(
                "An expression over the open layers: bands, indices, a series with an image,\n\
                 series matched by date, reductions over time (max, median, count...), rules",
            )
            .clicked()
        {
            self.calc.show = !self.calc.show;
            ui.close();
        }
        if ui
            .add(egui::Button::selectable(self.reg.show, "Regularize series..."))
            .on_hover_text(
                "The active series at a fixed step (8 or 16 days, monthly, yearly): a composite per period\n\
                 (median, medoid, clearest date), the empty periods filled; a new series of COGs",
            )
            .clicked()
        {
            self.reg.show = !self.reg.show;
            ui.close();
        }
        if ui
            .add(egui::Button::selectable(self.speckle.show, "Speckle filter..."))
            .on_hover_text(
                "A radar series (Sentinel-1's VV and VH) through Quegan's multitemporal filter or Lee's:\n\
                 less speckle, a new series of COGs",
            )
            .clicked()
        {
            self.speckle.show = !self.speckle.show;
            ui.close();
        }
        if ui
            .add(egui::Button::selectable(self.warp.show, "Reproject, resample, clip..."))
            .on_hover_text(
                "The active series or a run's map through gdalwarp: another CRS, pixel size or a layer's grid,\n\
                 the visible area, the ROI or a vector layer's polygons; COGs, GeoTIFFs or virtual (VRTs)",
            )
            .clicked()
        {
            self.warp.show = !self.warp.show;
            ui.close();
        }
        if ui.add(egui::Button::selectable(self.zeit.show_tasks, "Tasks")).clicked() {
            self.zeit.show_tasks = !self.zeit.show_tasks;
            ui.close();
        }
        ui.weak(format!("Zeit {} (native, zeit-core)", janus_zeit::ZEIT_VERSION));
    }

    /// A tool's window opened or closed: opened, a tool of the chart is fitted on it (if the series suits it);
    /// closed, its fits go.
    pub(crate) fn set_tool_open(&mut self, tool: &Tool, open: bool) {
        self.zeit.tools.entry(tool.id.clone()).or_default().open = open;
        // closed: its preview goes with it
        if !open && self.zeit.preview.as_ref().is_some_and(|p| p.tool == tool.id) {
            self.drop_preview();
        }
        if tool.modes.iter().any(|m| m == "pixel") {
            self.zeit.set_fitted(&tool.id, open);
        }
    }

    /// The open tools' windows (those in the dock drawn there).
    pub(crate) fn tool_windows(&mut self, ctx: &egui::Context) {
        let open: Vec<String> = self.zeit.tools.iter().filter(|(_, u)| u.open).map(|(id, _)| id.clone()).collect();
        for id in open {
            let Some(tool) = manifest().tool(&id) else { continue };
            if self.docked(crate::shots::tool_tab(&id)) {
                continue;
            }
            let mut keep = true;
            crate::panels::fitted_window(&tool.name, ctx, 440.0)
                .id(egui::Id::new(("zeit tool", &id)))
                .open(&mut keep)
                .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.tool_window(ui, tool)));
            if !keep {
                self.set_tool_open(tool, false);
            }
        }
    }

    pub(crate) fn tool_window(&mut self, ui: &mut Ui, tool: &'static Tool) {
        ui.label(&tool.description);
        let info = self.session.as_ref().map(|s| s.info.clone());
        let roles = self.active_roles();
        let (needed, optional) = tool_roles(tool);
        let mut why = match &info {
            None => Some("no series is open".to_string()),
            Some(i) => applicability(tool, i),
        };
        if why.is_none() {
            let missing: Vec<&String> = needed.iter().filter(|r| !roles.contains_key(*r)).collect();
            if !missing.is_empty() {
                why = Some(format!(
                    "choose the band of: {}",
                    missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                ));
            }
        }
        if let Some(w) = &why {
            ui.colored_label(theme::error(), format!("Not available for this series: {w}"));
        }
        let mut changed = false;

        // the bands of each role (multiband tools)
        if let Some(info) = info.as_ref().filter(|i| !needed.is_empty() && i.bands_per_date > 1) {
            crate::panels::section(ui, "Bands");
            for role in needed.iter().chain(&optional) {
                let is_optional = optional.contains(role);
                let cur = roles.get(role).copied().unwrap_or(0);
                let label = |b: i32| {
                    if b == 0 {
                        (if is_optional { "(not used)" } else { "(choose)" }).to_string()
                    } else {
                        format!("{b}: {}", info.band_names.get(b as usize - 1).map_or("", |s| s.as_str()))
                    }
                };
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt(("role", role)).width(220.0).selected_text(label(cur)).show_ui(ui, |ui| {
                        for b in 0..=info.bands_per_date as i32 {
                            if ui.selectable_label(b == cur, label(b)).clicked() {
                                if b == 0 {
                                    self.zeit.roles.remove(role);
                                } else {
                                    self.zeit.roles.insert(role.clone(), b);
                                }
                                // every tool reads these bands
                                self.zeit.version += 1;
                            }
                        }
                    });
                    ui.label(if is_optional { format!("{role} (optional)") } else { role.clone() });
                });
            }
            if info.sel.qa_band > 0 {
                ui.weak(format!(
                    "Quality: {} ({})",
                    info.band_names.get(info.sel.qa_band as usize - 1).map_or("", |s| s.as_str()),
                    info.sel.qa_rule.label()
                ));
            } else {
                ui.weak("No quality band: set one in the Display panel to screen clouds.");
            }
        }

        crate::panels::section(ui, "Parameters");
        changed |= self.params_ui(ui, tool);
        if ui.button("Reset to defaults").clicked() {
            *self.zeit.params(tool) = tool.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
            changed = true;
        }
        if changed {
            let u = self.zeit.tools.entry(tool.id.clone()).or_default();
            u.version += 1;
            if u.auto_preview {
                u.preview_asked = Some(Instant::now());
            }
        }

        if tool.modes.iter().any(|m| m == "pixel") {
            crate::panels::section(ui, "On the chart");
            // on while the series suits the tool (kept on: it fits again on a series that does)
            let fitted = self.zeit.fitted(&tool.id);
            let mut on = fitted && why.is_none();
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(why.is_none(), egui::Checkbox::new(&mut on, "Fit the cursor, pins and ROI series"))
                    .changed()
                {
                    self.zeit.set_fitted(&tool.id, on);
                }
                // its line on the chart, as in the legend
                if let Some((_, style)) = self.zeit.chart_tools().into_iter().find(|(t, _)| t.id == tool.id) {
                    fit_swatch(ui, style).on_hover_text("Its model on the chart (the legend names it)");
                }
            });
            ui.weak(
                "Runs on every exact series; the model is drawn over it in its own\ncolour and line, named in the \
                 chart's legend, and its numbers are\nadded to the Statistics table. Other tools can be fitted too.",
            );
        }
        if tool.modes.iter().any(|m| m == "raster") && self.session.is_some() {
            self.raster_run_ui(ui, tool, why.is_some());
        }
    }

    /// The parameters' form without its lists of series (TWDTW's patterns: the Classification window makes them).
    pub(crate) fn tool_params_ui(&mut self, ui: &mut Ui, tool: &Tool) -> bool {
        self.params_form(ui, tool, false)
    }

    /// The parameters' form, from the descriptor. Returns whether a value changed.
    fn params_ui(&mut self, ui: &mut Ui, tool: &Tool) -> bool {
        self.params_form(ui, tool, true)
    }

    fn params_form(&mut self, ui: &mut Ui, tool: &Tool, lists: bool) -> bool {
        let mut changed = false;
        let pins: Vec<(u32, usize, usize, bool, Vec<f32>)> =
            self.series.pins.iter().map(|p| (p.id, p.x, p.y, p.exact, p.values.clone())).collect();
        let roi = self.series.roi.complete().then(|| self.series.roi.mean.clone()).filter(|m| !m.is_empty());
        let info = self.session.as_ref().map(|s| s.info.clone());
        let results: Vec<(String, String, String)> = self.zeit_result_maps();
        let shown_t = self.display.t;
        let values = self.zeit.params(tool);
        egui::Grid::new(("params", &tool.id)).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
            for p in &tool.params {
                let v = values.entry(p.id.clone()).or_insert_with(|| p.default.clone());
                let help = |r: egui::Response| if p.help.is_empty() { r } else { r.on_hover_text(&p.help) };
                match p.kind.as_str() {
                    "int" => {
                        help(ui.label(&p.label));
                        let mut x = v.as_i64().unwrap_or(0);
                        let mut drag = egui::DragValue::new(&mut x);
                        if let (Some(lo), Some(hi)) = (p.min, p.max) {
                            drag = drag.range(lo as i64..=hi as i64);
                        }
                        if help(ui.add(drag)).changed() {
                            *v = json!(x);
                            changed = true;
                        }
                    }
                    "float" => {
                        help(ui.label(&p.label));
                        let mut x = v.as_f64().unwrap_or(0.0);
                        let mut drag = egui::DragValue::new(&mut x).speed(0.01).max_decimals(6);
                        if let (Some(lo), Some(hi)) = (p.min, p.max) {
                            drag = drag.range(lo..=hi).speed(((hi - lo) / 500.0).clamp(1e-4, 10.0));
                        }
                        if help(ui.add(drag)).changed() {
                            *v = json!(x);
                            changed = true;
                        }
                    }
                    "bool" => {
                        ui.label("");
                        let mut x = v.as_bool().unwrap_or(false);
                        if help(ui.checkbox(&mut x, &p.label)).changed() {
                            *v = json!(x);
                            changed = true;
                        }
                    }
                    "enum" => {
                        help(ui.label(&p.label));
                        let options = p.options.clone().unwrap_or_default();
                        let labels = p.labels.clone().unwrap_or_default();
                        let cur = v.as_str().unwrap_or("").to_string();
                        let k = options.iter().position(|o| *o == cur).unwrap_or(0);
                        let shown = |i: usize| labels.get(i).or(options.get(i)).cloned().unwrap_or_default();
                        ComboBox::from_id_salt(("enum", &p.id)).width(220.0).selected_text(shown(k)).show_ui(
                            ui,
                            |ui| {
                                for (i, option) in options.iter().enumerate() {
                                    if ui.selectable_label(i == k, shown(i)).clicked() {
                                        *v = json!(option);
                                        changed = true;
                                    }
                                }
                            },
                        );
                    }
                    "layers" => {
                        ui.end_row();
                        changed |= layers_param_ui(ui, p, v, &results);
                    }
                    // lists of spectra: below the grid, the window's width
                    "patterns" | "endmembers" => continue,
                    _ => {}
                }
                ui.end_row();
            }
        });
        for p in tool.params.iter().filter(|p| lists && (p.kind == "patterns" || p.kind == "endmembers")) {
            let v = values.entry(p.id.clone()).or_insert_with(|| p.default.clone());
            changed |= if p.kind == "patterns" {
                patterns_ui(ui, p, v, &pins, roi.as_deref(), info.as_deref())
            } else {
                endmembers_ui(ui, p, v, &pins, roi.as_deref(), info.as_ref(), shown_t)
            };
        }
        changed
    }
}

/// A "patterns" parameter (TWDTW's classes): reference series from the pins or the ROI mean, each named.
/// Value: [{"name", "from", "years", "days", "values"}].
fn patterns_ui(
    ui: &mut Ui,
    p: &janus_zeit::Param,
    v: &mut Value,
    pins: &[(u32, usize, usize, bool, Vec<f32>)],
    roi: Option<&[f32]>,
    info: Option<&CubeInfo>,
) -> bool {
    if !v.is_array() {
        *v = json!([]);
    }
    let mut changed = false;
    ui.vertical(|ui| {
        ui.label(&p.label).on_hover_text(&p.help);
        let list = v.as_array_mut().unwrap();
        let mut remove = None;
        for (k, pat) in list.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let mut name = pat["name"].as_str().unwrap_or("").to_string();
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(160.0)).changed() {
                    pat["name"] = json!(name);
                    changed = true;
                }
                let n = pat["values"].as_array().map_or(0, Vec::len);
                ui.weak(format!("{}, {n} dates", pat["from"].as_str().unwrap_or("")));
                if ui.small_button("x").clicked() {
                    remove = Some(k);
                }
            });
        }
        if let Some(k) = remove {
            list.remove(k);
            changed = true;
        }
        if list.is_empty() {
            ui.weak("No patterns yet: drop pins on known places, then add them here.");
        }
        let Some(info) = info else { return };
        let mut add = |from: String, values: &[f32]| {
            let n = list.len() + 1;
            list.push(json!({
                "name": format!("Class {n}"),
                "from": from,
                "years": (0..info.t()).map(|t| info.decimal_year(t)).collect::<Vec<_>>(),
                "days": if info.time_is_date { json!((0..info.t()).map(|t| info.unix_day(t)).collect::<Vec<_>>()) } else { Value::Null },
                "values": values.iter().map(|&x| if x.is_finite() { json!(f64::from(x)) } else { Value::Null }).collect::<Vec<_>>(),
            }));
            changed = true;
        };
        ComboBox::from_id_salt(("add pattern", &p.id)).width(260.0).selected_text("Add a pattern from...").show_ui(ui, |ui| {
            for (id, x, y, exact, values) in pins {
                let label = format!("Pin {id} ({x}, {y}){}", if *exact { "" } else { " - reading..." });
                if ui.add_enabled(*exact, egui::Button::selectable(false, label)).clicked() {
                    add(format!("pin {id}"), values);
                }
            }
            if ui.add_enabled(roi.is_some(), egui::Button::selectable(false, "ROI mean")).clicked()
                && let Some(r) = roi
            {
                add("ROI".into(), r);
            }
            if pins.is_empty() && roi.is_none() {
                ui.weak("Click the map to drop pins first.");
            }
        });
    });
    changed
}

/// An "endmembers" parameter (spectral unmixing): the spectra of pure covers, each named. On a hyperspectral scene
/// a pin's series is its spectrum (the ROI mean too); on a series of multiband dates, the pin's bands at the date
/// shown. Value: [{"name", "from", "values"}].
fn endmembers_ui(
    ui: &mut Ui,
    p: &janus_zeit::Param,
    v: &mut Value,
    pins: &[(u32, usize, usize, bool, Vec<f32>)],
    roi: Option<&[f32]>,
    info: Option<&Arc<CubeInfo>>,
    t: usize,
) -> bool {
    if !v.is_array() {
        *v = json!([]);
    }
    let mut changed = false;
    ui.vertical(|ui| {
        ui.label(&p.label).on_hover_text(&p.help);
        let list = v.as_array_mut().unwrap();
        let mut remove = None;
        for (k, e) in list.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                let mut name = e["name"].as_str().unwrap_or("").to_string();
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(160.0)).changed() {
                    e["name"] = json!(name);
                    changed = true;
                }
                let n = e["values"].as_array().map_or(0, Vec::len);
                ui.weak(format!("{}, {n} bands", e["from"].as_str().unwrap_or("")));
                if ui.small_button("x").clicked() {
                    remove = Some(k);
                }
            });
        }
        if let Some(k) = remove {
            list.remove(k);
            changed = true;
        }
        if list.is_empty() {
            ui.weak("No endmembers yet: drop pins on pure covers (vegetation, soil, water...), then add them here.");
        }
        let Some(info) = info else { return };
        let t = t.min(info.t().saturating_sub(1));
        let mut add = |from: String, values: Vec<f64>| {
            let n = list.len() + 1;
            list.push(json!({
                "name": format!("Endmember {n}"),
                "from": from,
                "values": values.iter().map(|&x| if x.is_finite() { json!(x) } else { Value::Null }).collect::<Vec<_>>(),
            }));
            changed = true;
        };
        let date = info.layers.get(t).map(|l| l.label.clone()).unwrap_or_default();
        let text = if info.spectral { "Add an endmember from...".to_string() } else { format!("Add one from {date}...") };
        ComboBox::from_id_salt(("add endmember", &p.id)).width(260.0).selected_text(text).show_ui(ui, |ui| {
            for (id, x, y, exact, values) in pins {
                let label = format!("Pin {id} ({x}, {y}){}", if *exact { "" } else { " - reading..." });
                if ui.add_enabled(*exact, egui::Button::selectable(false, label)).clicked() {
                    if info.spectral {
                        add(format!("pin {id}"), values.iter().map(|&v| f64::from(v)).collect());
                    } else {
                        // the pin's bands at the date shown; the quality band none (left out of the unmixing)
                        let mut reader = CubeReader::new(info.clone());
                        let bands = (1..=info.bands_per_date as i32)
                            .map(|b| {
                                if b == info.sel.qa_band {
                                    return f64::NAN;
                                }
                                reader.read_band_pixel(t, b, *x, *y, false).map_or(f64::NAN, f64::from)
                            })
                            .collect();
                        add(format!("pin {id}, {date}"), bands);
                    }
                }
            }
            if info.spectral
                && ui.add_enabled(roi.is_some(), egui::Button::selectable(false, "ROI mean")).clicked()
                && let Some(r) = roi
            {
                add("ROI".into(), r.iter().map(|&v| f64::from(v)).collect());
            }
            if pins.is_empty() {
                ui.weak("Click the map to drop pins first.");
            }
        });
    });
    changed
}

/// A "layers" parameter (the maps Agreement compares): result maps of the parameter's unit, in the order checked
/// (the first is the grid). Value: [{"name", "path"}]. `results`: (name, path, unit) of the result maps.
fn layers_param_ui(ui: &mut Ui, p: &janus_zeit::Param, v: &mut Value, results: &[(String, String, String)]) -> bool {
    if !v.is_array() {
        *v = json!([]);
    }
    let mut changed = false;
    let list = v.as_array_mut().unwrap();
    // maps removed from the Layers panel
    let before = list.len();
    list.retain(|m| results.iter().any(|r| Some(r.1.as_str()) == m["path"].as_str()));
    changed |= list.len() != before;
    ui.vertical(|ui| {
        ui.label(&p.label).on_hover_text(&p.help);
        let mut shown = 0;
        for (name, path, unit) in results {
            if p.unit.as_deref().is_some_and(|u| u != unit) {
                continue;
            }
            let at = list.iter().position(|m| m["path"].as_str() == Some(path.as_str()));
            let mut on = at.is_some();
            let label = format!("{name}{}", if at == Some(0) { "  (grid)" } else { "" });
            if ui.checkbox(&mut on, label).on_hover_text(path).changed() {
                match at {
                    Some(k) => {
                        list.remove(k);
                    }
                    None => list.push(json!({"name": name, "path": path})),
                }
                changed = true;
            }
            shown += 1;
        }
        if shown == 0 {
            ui.weak(if p.unit.as_deref() == Some("year") {
                "No map of dates yet: run LandTrendr, CCDC, BFAST or CODED first."
            } else {
                "No map of this kind yet: run a tool first."
            });
        }
    });
    changed
}

/// The fitted models' overlays on a series, each under its tool's name in the legend (a click there hides that
/// tool's models), in its colour and line over a dark outline: never mistaken for data. A series hidden in the
/// legend hides its models.
pub(crate) fn draw_fits(
    plot: &mut PlotUi,
    series_hidden: bool,
    fits: &[ToolFit<'_>],
    shown: &crate::chart::AsShown,
    info: &CubeInfo,
) {
    if series_hidden {
        return;
    }
    for f in fits {
        let Some(Ok(r)) = f.result else { continue };
        draw_fit(plot, &f.tool.name, f.style, r, shown, info);
    }
}

fn draw_fit(
    plot: &mut PlotUi,
    name: &str,
    style: FitStyle,
    r: &PixelResult,
    shown: &crate::chart::AsShown,
    info: &CubeInfo,
) {
    let col = style.color;
    let outline = Color32::from_rgba_unmultiplied(13, 13, 20, 230);
    // the legend takes the colour of the first item of a name: the tool's, not its outline's
    let mut named = false;
    for o in &r.overlays {
        if o.kind == "vlines" {
            for &x in &o.x {
                let x = info.x_from_decimal_year(x);
                if !named {
                    plot.vline(VLine::new(name, x).color(col).width(1.5));
                    named = true;
                }
                plot.vline(VLine::new(name, x).color(outline).width(3.5));
                plot.vline(VLine::new(name, x).color(col.gamma_multiply(0.85)).width(1.5).style(style.line));
            }
            continue;
        }
        let pts: Vec<[f64; 2]> =
            o.x.iter()
                .zip(&o.y)
                .map(|(&x, y)| {
                    let x = info.x_from_decimal_year(x);
                    [x, y.map_or(f64::NAN, |y| shown.at(y, x))]
                })
                .collect();
        if !named && let Some(p) = pts.iter().find(|p| p[1].is_finite()) {
            plot.line(Line::new(name, PlotPoints::from(vec![*p])).color(col).width(2.0));
            named = true;
        }
        if pts.is_empty() {
            continue;
        }
        let alpha = o.alpha.unwrap_or(1.0) as f32;
        let width = o.weight.unwrap_or(2.5) as f32;
        if o.kind == "markers" {
            let valid: Vec<[f64; 2]> = pts.into_iter().filter(|p| p[1].is_finite()).collect();
            plot.points(
                Points::new(name, PlotPoints::from(valid)).color(col).radius(3.0).shape(egui_plot::MarkerShape::Square),
            );
        } else {
            // a gap where the model has no value
            for piece in pts.split(|p| !p[1].is_finite()).filter(|p| !p.is_empty()) {
                plot.line(
                    Line::new(name, PlotPoints::from(piece.to_vec()))
                        .color(outline.gamma_multiply(alpha))
                        .width(width + 2.5),
                );
                plot.line(
                    Line::new(name, PlotPoints::from(piece.to_vec()))
                        .color(col.gamma_multiply(alpha))
                        .width(width)
                        .style(if o.style.as_deref() == Some("dashed") {
                            egui_plot::LineStyle::dashed_loose()
                        } else {
                            style.line
                        }),
                );
            }
        }
    }
}

/// A fitted tool's line, as the chart draws it, 28 points wide.
pub(crate) fn fit_swatch(ui: &mut Ui, style: FitStyle) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(28.0, 12.0), egui::Sense::hover());
    let (a, b) = (rect.left_center(), rect.right_center());
    let p = ui.painter();
    p.line_segment([a, b], egui::Stroke::new(4.0, Color32::from_rgba_unmultiplied(13, 13, 20, 230)));
    match style.line {
        egui_plot::LineStyle::Solid => {
            p.line_segment([a, b], egui::Stroke::new(2.0, style.color));
        }
        egui_plot::LineStyle::Dotted { spacing } => {
            p.extend(egui::Shape::dotted_line(&[a, b], style.color, spacing * 0.6, 1.2));
        }
        egui_plot::LineStyle::Dashed { length } => {
            p.extend(egui::Shape::dashed_line(
                &[a, b],
                egui::Stroke::new(2.0, style.color),
                length * 0.6,
                length * 0.4,
            ));
        }
    }
    resp
}

/// The rows a column's fit adds to the Statistics table, by name.
pub(crate) fn fit_rows(fit: Option<&Result<PixelResult, String>>) -> Vec<(String, String)> {
    match fit {
        Some(Ok(r)) => r.rows.iter().map(|[a, b]| (a.clone(), b.clone())).collect(),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Raster runs: their tasks and their result maps
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done,
    Failed,
    Cancelled,
}

/// What a running job tells the window.
pub struct JobStatus {
    pub state: JobState,
    pub progress: f64,
    pub message: String,
    pub error: String,
    pub result: Option<janus_zeit::job::JobResult>,
    pub seconds: f64,
}

/// A raster run of a tool (Tools > Tasks).
pub struct Job {
    pub title: String,
    pub tool: String,
    /// the series (layer) it ran on: its results go under it
    pub cube: u64,
    pub output_dir: String,
    /// job.log in the output folder
    pub log_path: String,
    pub started: Instant,
    pub status: Arc<std::sync::Mutex<JobStatus>>,
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    /// its outputs were taken (as result layers or series)
    pub loaded: bool,
}

/// A run's log (job.log beside its job.json): what runs, its progress, its outputs or its error, each line after
/// the seconds since the run started.
pub(crate) struct JobLog {
    pub(crate) path: std::path::PathBuf,
    pub(crate) t0: Instant,
}

impl JobLog {
    pub(crate) fn write(&self, text: &str) {
        use std::io::Write as _;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            let _ = f.write_all(text.as_bytes());
        }
    }

    pub(crate) fn line(&self, text: &str) {
        self.write(&format!("[{:7.1} s] {text}\n", self.t0.elapsed().as_secs_f64()));
    }
}

/// The Log window: the file shown and its end, read as it grows.
#[derive(Default)]
pub struct LogView {
    pub show: bool,
    /// a file of its own (not a run's log): kept while shown
    pub external: bool,
    pub path: String,
    shown: String,
    text: String,
    /// the bytes of the file read
    size: u64,
    /// only the end of the file is shown
    cut: bool,
    checked: Option<Instant>,
}

impl LogView {
    /// What it shows of its file.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The file it shows, once read.
    pub fn shown(&self) -> &str {
        &self.shown
    }
}

impl State {
    /// The Log window on a file of its own (the tests; a run's log is picked in Tools > Tasks).
    pub(crate) fn show_log_file(&mut self, path: &str) {
        let v = &mut self.zeit.log;
        (v.path, v.show, v.external) = (path.to_string(), true, true);
    }
}

/// A map a run wrote (src/results.hpp): a layer of its own, on the grid of the series it was computed on (placed on
/// the map by that grid, whether the series is still open, shown or not).
pub struct ResultLayer {
    /// its number (the Layers panel's selection, the Pixel values' focus)
    pub id: u32,
    pub cube: u64,
    /// the series' grid (x0, y0, w and h are in its pixels), its name, and its placement on the active layer's
    pub grid: Option<Arc<CubeInfo>>,
    pub source: String,
    pub place: crate::layers::Placement,
    /// its place in the drawing order (order.rs)
    pub z: u64,
    /// its run's folder and its place among the run's outputs (the first one drawn on top of the others)
    pub run: String,
    pub output: usize,
    /// e.g. "LandTrendr: Year of detection"
    pub name: String,
    pub path: String,
    pub unit: String,
    pub cmap: usize,
    /// the window in the series' pixels
    pub x0: usize,
    pub y0: usize,
    pub w: usize,
    pub h: usize,
    /// a copy (at most 4096 px a side): tw x th, NaN = no value
    pub tw: usize,
    pub th: usize,
    pub data: Vec<f32>,
    pub tex: Option<janus_gpu::GpuTexture>,
    pub lo: f32,
    pub hi: f32,
    pub visible: bool,
    pub opacity: f32,
    /// a class map: value k is classes[k - 1]
    pub classes: Vec<String>,
    /// each class's typical series (x in decimal years), drawn for the class under the cursor
    pub class_series: Vec<(Vec<f64>, Vec<f64>)>,
    /// its copy at full resolution being read (it is shown, and the first copy was reduced)
    pub upgrading: bool,
    /// a preview's map (replaced by the next preview)
    pub preview: bool,
}

impl ResultLayer {
    /// The value at a pixel of its series (NaN outside or without one).
    pub fn value_at(&self, x: usize, y: usize) -> f32 {
        if x < self.x0 || y < self.y0 || x >= self.x0 + self.w || y >= self.y0 + self.h || self.data.is_empty() {
            return f32::NAN;
        }
        let px = ((x - self.x0) as f64 * self.tw as f64 / self.w as f64) as usize;
        let py = ((y - self.y0) as f64 * self.th as f64 / self.h as f64) as usize;
        self.data[py.min(self.th - 1) * self.tw + px.min(self.tw - 1)]
    }

    /// A map of segments (SNIC's labels): each segment one colour of a qualitative colormap.
    pub fn is_segments(&self) -> bool {
        self.unit == "segment"
    }

    /// What its texture holds: the values, or for a map of segments each label's colour (1..=the colormap's
    /// colours), scattered by a multiplicative hash so that neighbouring segments seldom share one.
    pub(crate) fn texture_values(&self) -> std::borrow::Cow<'_, [f32]> {
        if !self.is_segments() {
            return std::borrow::Cow::Borrowed(&self.data);
        }
        let k = janus_gpu::colormaps::all()[self.cmap].keys().count().max(1) as u32;
        let colour = |v: f32| ((v as u32).wrapping_mul(2_654_435_761) >> 16) % k + 1;
        self.data.iter().map(|&v| if v.is_finite() && v >= 0.0 { colour(v) as f32 } else { f32::NAN }).collect()
    }

    /// The automatic range: min..max for years and durations, 2-98 % otherwise; a class map's colours one
    /// per class, a map of segments' one per colour.
    pub(crate) fn auto_range(&mut self) {
        if self.is_segments() {
            let colormaps = janus_gpu::colormaps::all();
            if !colormaps[self.cmap].qualitative
                && let Some(paired) = janus_gpu::colormaps::by_name("Paired")
            {
                self.cmap = paired;
            }
            self.lo = 0.5;
            self.hi = colormaps[self.cmap].keys().count() as f32 + 0.5;
            return;
        }
        if !self.classes.is_empty() {
            let colormaps = janus_gpu::colormaps::all();
            if self.classes.len() > colormaps[self.cmap].keys().count()
                && let Some(paired) = janus_gpu::colormaps::by_name("Paired")
            {
                self.cmap = paired;
            }
            self.lo = 0.5;
            self.hi = colormaps[self.cmap].keys().count() as f32 + 0.5;
            return;
        }
        let v: Vec<f32> = self.data.iter().copied().filter(|x| x.is_finite()).collect();
        if v.is_empty() {
            (self.lo, self.hi) = (0.0, 1.0);
        } else if self.unit == "year" || self.unit == "years" {
            let (mn, mx) = v.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            (self.lo, self.hi) = (mn, mx.max(mn + 1.0));
        } else {
            (self.lo, self.hi) = janus_cube::stats::sample_percentiles(&v, 2_000_000, 0.02, 0.98);
            // a diverging colormap across 0: its middle colour at 0
            let diverging = matches!(janus_gpu::colormaps::all()[self.cmap].name, "RdBu" | "BrBG" | "PiYG");
            if diverging && self.lo < 0.0 && self.hi > 0.0 {
                let m = self.hi.max(-self.lo);
                (self.lo, self.hi) = (-m, m);
            }
        }
    }
}

/// Band 1 of a result raster, downsampled (nearest) so its longer side is at most `max_dim`.
pub(crate) fn load_result(path: &str, max_dim: usize) -> Result<(Vec<f32>, usize, usize), String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    let (w, h) = ds.raster_size();
    let s = (max_dim as f64 / w.max(h) as f64).min(1.0);
    let (tw, th) = (((w as f64 * s).round() as usize).max(1), ((h as f64 * s).round() as usize).max(1));
    let band = ds.rasterband(1).map_err(|e| e.to_string())?;
    let nd = band.no_data_value().filter(|v| !v.is_nan());
    let buf = band
        .read_as::<f32>((0, 0), (w, h), (tw, th), Some(gdal::raster::ResampleAlg::NearestNeighbour))
        .map_err(|e| format!("{path}: {e}"))?;
    let mut data = buf.data().to_vec();
    if let Some(nd) = nd {
        for v in data.iter_mut() {
            if *v == nd as f32 {
                *v = f32::NAN;
            }
        }
    }
    Ok((data, tw, th))
}

/// Now as "YYYY-MM-DD HH:MM:SS UTC", for the logs.
pub(crate) fn timestamp_text() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = janus_zeit::numeric::civil(now.div_euclid(86400));
    let s = now.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
}

pub(crate) fn timestamp() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64;
    let (y, m, d) = janus_zeit::numeric::civil(now.div_euclid(86400));
    let s = now.rem_euclid(86400);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// A file or folder name from the series' inputs.
pub(crate) fn series_name(info: &CubeInfo, inputs: &[String]) -> String {
    let p = std::path::PathBuf::from(if inputs.len() == 1 { inputs[0].clone() } else { info.first_path.clone() });
    let p = if inputs.len() == 1 { p } else { p.parent().map(|p| p.to_path_buf()).unwrap_or(p) };
    let n = p.file_stem().or(p.file_name()).map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let n = if n.is_empty() { "series".into() } else { n };
    n.chars().map(|c| if matches!(c, ' ' | ':' | '*' | '?') { '_' } else { c }).collect()
}

fn format_duration(s: f64) -> String {
    if s < 1.0 {
        "under a second".into()
    } else if s < 90.0 {
        format!("~{s:.0} s")
    } else if s < 5400.0 {
        format!("~{:.0} min", s / 60.0)
    } else {
        format!("~{:.1} h", s / 3600.0)
    }
}

impl State {
    /// The result maps a "layers" parameter can choose: (name, path, unit).
    pub(crate) fn zeit_result_maps(&self) -> Vec<(String, String, String)> {
        self.zeit.results.iter().map(|r| (r.name.clone(), r.path.clone(), r.unit.clone())).collect()
    }

    /// The pixel window of a run's scope: the whole image, the visible area or the ROI.
    pub(crate) fn run_window(&self, scope: usize) -> ([usize; 4], &'static str) {
        let Some(s) = &self.session else { return ([0, 0, 0, 0], "") };
        let (w, h) = (s.info.width, s.info.height);
        if scope == 1
            && let Some(v) = self.map.last_view
        {
            let x0 = (v.x0.floor().max(0.0) as usize).min(w - 1);
            let y0 = (v.y0.floor().max(0.0) as usize).min(h - 1);
            let x1 = (v.x1.ceil() as usize).clamp(x0 + 1, w);
            let y1 = (v.y1.ceil() as usize).clamp(y0 + 1, h);
            return ([x0, y0, x1, y1], "visible area");
        }
        if scope == 2
            && let Some([x0, y0, x1, y1]) = self.series.roi_rect
        {
            return ([x0, y0, x1, y1], if self.series.roi_poly.is_empty() { "ROI" } else { "ROI's bounding box" });
        }
        ([0, 0, w, h], "whole image")
    }

    /// A tool's window of a run and its scope's name: run_window's, or (scope 3) the window of a run run again.
    pub(crate) fn tool_run_window(&self, tool: &Tool, scope: usize) -> ([usize; 4], String) {
        let cube = self.session.as_ref().map_or(0, |s| s.info.id);
        if scope == 3
            && let Some((_, w)) = self.zeit.tools.get(&tool.id).and_then(|u| u.again).filter(|a| a.0 == cube)
        {
            return (w, "the run's window".into());
        }
        let (w, name) = self.run_window(scope);
        (w, name.into())
    }

    /// A raster run of the tool (scope, output folder, Run).
    pub(crate) fn raster_run_ui(&mut self, ui: &mut Ui, tool: &'static Tool, unavailable: bool) {
        if self.session.is_none() {
            return;
        }
        if tool.input.as_deref() == Some("layers") {
            crate::panels::section(ui, "Run");
            let maps = tool
                .params
                .iter()
                .filter(|p| p.kind == "layers")
                .map(|p| self.zeit.params(tool).get(&p.id).and_then(Value::as_array).map_or(0, Vec::len))
                .max()
                .unwrap_or(0);
            ui.weak("On the grid of the first map checked; the results go under its series.");
            self.output_folder_ui(ui);
            if ui
                .add_enabled(maps >= 2, egui::Button::new("Run").min_size(egui::vec2(ui.available_width(), 0.0)))
                .clicked()
            {
                self.run_tool(tool, &ui.ctx().clone());
            }
            if maps < 2 {
                ui.weak("Check at least two maps above.");
            }
            return;
        }
        crate::panels::section(ui, "Run on the raster");
        let roi = self.series.roi_rect.is_some();
        let cube = self.session.as_ref().map_or(0, |s| s.info.id);
        let ui_state = self.zeit.tools.entry(tool.id.clone()).or_default();
        // the window of a run run again, while its series is the active one
        let again = ui_state.again.filter(|a| a.0 == cube).map(|a| a.1);
        let mut scopes = vec!["Whole image".to_string(), "Visible area".into(), "ROI".into()];
        if let Some([x0, y0, x1, y1]) = again {
            scopes.push(format!("The run's window ({} x {} px at {x0}, {y0})", x1 - x0, y1 - y0));
        }
        if ui_state.scope == 2 && !roi || ui_state.scope == 3 && again.is_none() {
            ui_state.scope = 0;
        }
        ComboBox::from_id_salt(("scope", &tool.id))
            .width(ui.available_width())
            .selected_text(&scopes[ui_state.scope])
            .show_ui(ui, |ui| {
                for (i, s) in scopes.iter().enumerate() {
                    if ui.add_enabled(i != 2 || roi, egui::Button::selectable(ui_state.scope == i, s)).clicked() {
                        ui_state.scope = i;
                    }
                }
            });
        if !ui_state.again_note.is_empty() {
            ui.colored_label(theme::warning(), &ui_state.again_note);
        }
        let scope = ui_state.scope;
        if tool.input.as_deref() != Some("layers") {
            self.mask_ui(ui, &tool.id);
        }
        self.output_folder_ui(ui);
        let win = self.tool_run_window(tool, scope).0;
        let px = (win[2] - win[0]) as f64 * (win[3] - win[1]) as f64;
        if !unavailable {
            self.update_estimate(tool, ui.ctx());
            let est = self.zeit.tools.get(&tool.id).and_then(|u| u.est.as_ref());
            match est {
                Some(Ok(e)) => {
                    ui.label(format!(
                        "Estimated: {} for {:.3} M px",
                        format_duration(e.seconds(win[2] - win[0], win[3] - win[1])),
                        px / 1e6
                    ))
                    .on_hover_text(
                        "Computing time measured on a sample of the series, on the processing threads.
Reading the data adds to it: much more for files in strips or on spinning disks.",
                    );
                }
                Some(Err(e)) => {
                    ui.weak(format!("No estimate: {e}"));
                }
                None => {
                    ui.weak("Estimating the run time...");
                }
            }
        }
        let mut own = self.zeit.tools.get(&tool.id).and_then(|u| u.threads);
        let threads = self.threads_ui(ui, &mut own);
        self.zeit.tools.entry(tool.id.clone()).or_default().threads = own;
        if ui
            .add_enabled(!unavailable, egui::Button::new("Run").min_size(egui::vec2(ui.available_width(), 0.0)))
            .clicked()
        {
            self.run_tool(tool, &ui.ctx().clone());
        }
        ui.weak(format!("Runs in the background on {threads} threads; see Tools > Tasks."));
        self.preview_ui(ui, tool, unavailable);
    }

    /// Preview and Auto-preview: the tool on the ROI or a small window at the centre of the view, its maps as
    /// layers marked preview (replaced by the next one); again when a parameter changes, if that takes seconds.
    fn preview_ui(&mut self, ui: &mut Ui, tool: &'static Tool, unavailable: bool) {
        let Some([x0, y0, x1, y1]) = self.preview_area() else { return };
        let (w, h) = (x1 - x0, y1 - y0);
        let running = self.zeit.preview.as_ref().is_some_and(|p| {
            p.tool == tool.id
                && self
                    .zeit
                    .jobs
                    .iter()
                    .any(|j| j.output_dir == p.dir && j.status.lock().unwrap().state == JobState::Running)
        });
        let u = self.zeit.tools.entry(tool.id.clone()).or_default();
        let est = match &u.est {
            Some(Ok(e)) => Some(e.seconds(w, h)),
            _ => None,
        };
        let mut preview = false;
        ui.horizontal(|ui| {
            let b = ui.add_enabled(!unavailable, egui::Button::new(if running { "Preview..." } else { "Preview" }));
            preview = b
                .on_hover_text(format!(
                    "The tool on {} ({w} x {h} px) before the whole scene: its maps as layers marked preview,                      its window dashed on the map, replaced by the next preview",
                    if self.series.roi_rect.is_some() { "the ROI" } else { "the centre of the view" }
                ))
                .clicked();
            ui.checkbox(&mut u.auto_preview, "Auto-preview").on_hover_text(
                "A preview again whenever a parameter changes, when one takes a few seconds or less",
            );
        });
        if let Some(s) = est {
            ui.weak(format!("Preview: {} for {w} x {h} px", format_duration(s)));
        }
        let stale = self.zeit.preview.as_ref().is_some_and(|p| p.tool == tool.id && p.version != u.version);
        if stale && !running {
            ui.colored_label(theme::warning(), "The parameters changed since the preview on the map.");
        }
        // a parameter changed: once they rest for a moment and the last preview ended, if it is quick
        let quick = est.is_some_and(|s| s <= AUTO_PREVIEW_SECONDS);
        let asked = u.preview_asked.is_some_and(|t| t.elapsed().as_secs_f64() >= 0.4);
        if u.preview_asked.is_some() && !asked {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(450));
        }
        if asked && !running && !unavailable && u.auto_preview && quick {
            u.preview_asked = None;
            preview = true;
        } else if asked && (!u.auto_preview || est.is_some() && !quick) {
            u.preview_asked = None;
        }
        if preview && !unavailable {
            self.run_preview(tool, &ui.ctx().clone());
        }
    }

    fn output_folder_ui(&mut self, ui: &mut Ui) {
        let dir = self.results_dir();
        ui.horizontal(|ui| {
            ui.weak(format!("Output: {}", dir.display()));
            if ui.small_button("Change...").clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.zeit.results_dir = Some(d);
            }
        });
    }

    pub(crate) fn results_dir(&self) -> std::path::PathBuf {
        self.zeit.results_dir.clone().unwrap_or_else(|| crate::settings::app_data_dir().join("results"))
    }

    /// A run's spec on the active series (or, for a tool on maps, on its first map's grid): the spec without its
    /// output folder, the series its results go under, what it runs on, and the series' name.
    fn job_spec(&mut self, tool: &'static Tool) -> Result<(janus_zeit::job::JobSpec, u64, String, String), String> {
        let info = self.session.as_ref().ok_or("no series is open")?.info.clone();
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let name = series_name(&info, &inputs);
        let params: BTreeMap<String, Value> = self.zeit.params(tool).clone();
        let scope = self.zeit.tools.get(&tool.id).map_or(0, |u| u.scope);
        let work = crate::settings::app_data_dir().join("work");
        let _ = std::fs::create_dir_all(&work);
        let threads = self.zeit.tools.get(&tool.id).and_then(|u| u.threads).unwrap_or(self.processing_threads());
        let mut spec = base_spec(&info, tool, &params, self.display.t, threads);
        if self.classes.on() {
            // a categorical layer: its classes' names and colours on the class series a run writes
            spec.classes = self
                .classes
                .list
                .iter()
                .map(|c| janus_zeit::job::ClassDef { value: c.value, name: c.name.clone(), color: c.color })
                .collect();
        }
        let (mut window, mut scope_name) = self.tool_run_window(tool, scope);
        let mut cube = info.id;
        if tool.input.as_deref() == Some("layers") {
            // on result maps: the outputs on the first one's grid, under its series
            let first = tool
                .params
                .iter()
                .filter(|p| p.kind == "layers")
                .find_map(|p| params.get(&p.id)?.as_array()?.first()?.get("path")?.as_str().map(str::to_string));
            let Some(r) = first.and_then(|p| self.zeit.results.iter().find(|r| r.path == p)) else {
                return Err("Choose the maps to run on first.".into());
            };
            cube = r.cube;
            window = [r.x0, r.y0, r.x0 + r.w, r.y0 + r.h];
            let n: usize = tool
                .params
                .iter()
                .filter(|p| p.kind == "layers")
                .map(|p| params.get(&p.id).and_then(Value::as_array).map_or(0, Vec::len))
                .sum();
            scope_name = format!("{n} maps");
        } else {
            let roles = self.active_roles();
            series_inputs(&info, tool, &roles, &work.join(format!("{name}_{}", info.id)), &mut spec)?;
        }
        spec.window = Some(window);
        if tool.input.as_deref() != Some("layers")
            && let Some((path, mask)) =
                self.zeit.tools.get(&tool.id).and_then(|u| u.mask).and_then(|m| self.mask_path(m))
        {
            spec.mask = Some(path);
            scope_name = format!("{scope_name}, only where {mask} is not 0");
        }
        Ok((spec, cube, scope_name, name))
    }

    /// A run's provenance (provenance.rs) as it starts, and its Python (rerun.py) when the tool has one.
    fn zeit_provenance(
        &mut self,
        tool: &'static Tool,
        spec: &janus_zeit::job::JobSpec,
        scope_name: &str,
        name: &str,
        out: &std::path::Path,
    ) {
        use crate::provenance::{Provenance, SCRIPT, SeriesRecord};
        let params = self.zeit.params(tool).clone();
        let mut p = Provenance::start("zeit", &tool.id, &tool.name);
        p.set_params(&params);
        p.scope = scope_name.to_string();
        p.scope_index = self.zeit.tools.get(&tool.id).map_or(0, |u| u.scope);
        p.window = spec.window;
        p.threads = spec.threads;
        let info = self.session.as_ref().map(|s| s.info.clone());
        match info.filter(|_| tool.input.as_deref() != Some("layers")) {
            Some(info) => {
                let l = self.layers.get(self.active);
                let inputs = l.map(|l| l.inputs.clone()).unwrap_or_default();
                let lname = l.map_or(name.to_string(), |l| l.name.clone());
                p.series = Some(SeriesRecord::of(&info, &lname, &inputs));
                let roles = self.active_roles();
                if !roles.is_empty() && reads_bands(tool) {
                    p.extra.insert("bands".into(), json!(roles));
                }
                let window = spec.window.unwrap_or([0, 0, info.width, info.height]);
                let to = out.join("python").to_string_lossy().into_owned();
                match crate::python::run_code(&info, tool, &params, &roles, window, spec.threads, &to) {
                    Ok(code) if std::fs::write(out.join(SCRIPT), &code).is_ok() => p.python = SCRIPT.into(),
                    Ok(_) => p.python_note = format!("{SCRIPT} could not be written"),
                    Err(e) => p.python_note = e,
                }
            }
            None => p.python_note = "a run on result maps: no Python of it".into(),
        }
        self.begin_provenance(out, p);
    }

    /// Starts a raster run of the tool.
    pub(crate) fn run_tool(&mut self, tool: &'static Tool, ctx: &egui::Context) {
        self.start_run(tool, ctx, None);
    }

    /// A run of the tool on its scope, or (a preview) on `preview`'s window into its folder.
    fn start_run(&mut self, tool: &'static Tool, ctx: &egui::Context, preview: Option<([usize; 4], &std::path::Path)>) {
        let (mut spec, cube, mut scope_name, name) = match self.job_spec(tool) {
            Ok(s) => s,
            Err(e) => {
                self.error_popup = Some(e);
                return;
            }
        };
        let out = match preview {
            Some((w, dir)) => {
                (spec.window, scope_name) = (Some(w), "preview".into());
                dir.to_path_buf()
            }
            None => self.results_dir().join(format!("{name}_{}_{}", tool.id, timestamp())),
        };
        if let Err(e) = std::fs::create_dir_all(&out) {
            self.error_popup = Some(format!("could not create {}: {e}", out.display()));
            return;
        }
        let window = spec.window.unwrap_or_default();
        spec.output_dir = out.to_string_lossy().to_string();
        let title = format!("{} - {scope_name} ({} x {} px)", tool.name, window[2] - window[0], window[3] - window[1]);
        // the spec (what the bridge was given) and the log beside the maps
        let spec_path = out.join("job.json");
        let _ = std::fs::write(&spec_path, serde_json::to_string_pretty(&spec).unwrap_or_default());
        if preview.is_none() {
            self.zeit_provenance(tool, &spec, &scope_name, &name, &out);
        }
        let log = JobLog { path: out.join("job.log"), t0: Instant::now() };
        let mut params = serde_json::to_string(&spec.params).unwrap_or_default();
        if params.len() > 4000 {
            // reference series (patterns)
            let mut k = 4000;
            while !params.is_char_boundary(k) {
                k -= 1;
            }
            params = format!("{}...", &params[..k]);
        }
        log.write(&format!(
            "=== {} job: {title}\ntool: {}\nparams: {params}\nwindow: {window:?}\nthreads: {}\nspec: {}\noutput: {}\n",
            timestamp_text(),
            tool.id,
            spec.threads.filter(|&n| n > 0).map_or("not limited".into(), |n| n.to_string()),
            spec_path.display(),
            out.display()
        ));
        let log_path = log.path.to_string_lossy().to_string();
        let status = Arc::new(std::sync::Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (st, cn) = (status.clone(), cancel.clone());
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("zeit-job".into())
            .spawn(move || {
                let t0 = Instant::now();
                // progress to the log every 5 s or 10%
                let logged = std::sync::Mutex::new((-1.0, -1.0));
                let progress = |f: f64, m: &str| {
                    let mut s = st.lock().unwrap();
                    s.progress = f;
                    s.message = m.to_string();
                    drop(s);
                    let now = t0.elapsed().as_secs_f64();
                    let mut l = logged.lock().unwrap();
                    if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                        log.line(&format!("{:3.0}%  {m}", f * 100.0));
                        *l = (f, now);
                    }
                    ctx.request_repaint();
                };
                let r = janus_zeit::job::run_job(&spec, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(res) => {
                        for o in &res.outputs {
                            let name = o["name"].as_str().or(o["id"].as_str()).unwrap_or("");
                            log.line(&format!("output: {name} -> {}", o["path"].as_str().unwrap_or("")));
                        }
                        log.line("done");
                        s.state = JobState::Done;
                        s.progress = 1.0;
                        s.result = Some(res);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        log.line("failed");
                        s.state = JobState::Failed;
                        s.error = e;
                    }
                }
                ctx.request_repaint();
            })
            .ok();
        self.zeit.jobs.push(Job {
            title,
            tool: tool.id.clone(),
            cube,
            output_dir: out.to_string_lossy().to_string(),
            log_path,
            started: Instant::now(),
            status,
            cancel,
            loaded: false,
        });
        // a preview's progress is on the map
        self.zeit.show_tasks |= preview.is_none();
    }

    /// Where a preview runs: the ROI (its bounding box), else at most PREVIEW_SIDE px a side at the centre of the
    /// view, on the active layer's grid.
    pub(crate) fn preview_area(&self) -> Option<[usize; 4]> {
        let s = self.session.as_ref()?;
        if let Some(r) = self.series.roi_rect {
            return Some(r);
        }
        let (w, h) = (s.info.width, s.info.height);
        let (cx, cy) =
            self.map.last_view.map_or((w as f64 / 2.0, h as f64 / 2.0), |v| ((v.x0 + v.x1) / 2.0, (v.y0 + v.y1) / 2.0));
        let side = |n: usize, c: f64| {
            let k = n.min(PREVIEW_SIDE);
            let a = (c - k as f64 / 2.0).round().clamp(0.0, (n - k) as f64) as usize;
            (a, a + k)
        };
        let ((x0, x1), (y0, y1)) = (side(w, cx), side(h, cy));
        Some([x0, y0, x1, y1])
    }

    /// The tool run on the preview's area (preview_area) into a folder of its own; its maps replace the last
    /// preview's (theirs and their folder gone).
    pub(crate) fn run_preview(&mut self, tool: &'static Tool, ctx: &egui::Context) {
        let Some(window) = self.preview_area() else { return };
        self.drop_preview();
        let dir = crate::settings::app_data_dir().join("work").join("preview").join(format!(
            "{}_{}_{}",
            tool.id,
            timestamp(),
            self.frame
        ));
        let version = self.zeit.tools.get(&tool.id).map_or(0, |u| u.version);
        let cube = self.session.as_ref().map_or(0, |s| s.info.id);
        self.start_run(tool, ctx, Some((window, &dir)));
        if self.zeit.jobs.last().is_some_and(|j| std::path::Path::new(&j.output_dir) == dir) {
            self.zeit.preview =
                Some(Preview { tool: tool.id.clone(), dir: dir.to_string_lossy().into_owned(), cube, window, version });
        }
    }

    /// The last preview gone: its run cancelled if running, its maps, its task and its folder.
    pub(crate) fn drop_preview(&mut self) {
        let Some(p) = self.zeit.preview.take() else { return };
        for j in self.zeit.jobs.iter().filter(|j| j.output_dir == p.dir) {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.zeit.jobs.retain(|j| j.output_dir != p.dir);
        self.zeit.results.retain(|r| r.run != p.dir);
        self.map.dirty = true;
        // a run being cancelled may still write for a moment
        std::thread::spawn(move || {
            for _ in 0..50 {
                if std::fs::remove_dir_all(&p.dir).is_ok() || !std::path::Path::new(&p.dir).exists() {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        });
    }

    /// The preview's window on the map, dashed (with "Preview..." while it runs), over the series it was made on.
    pub(crate) fn draw_preview_mark(&self, painter: &egui::Painter, at: &dyn Fn(f64, f64) -> egui::Pos2) {
        let Some(p) = &self.zeit.preview else { return };
        if self.session.as_ref().is_none_or(|s| s.info.id != p.cube) {
            return;
        }
        let [x0, y0, x1, y1] = p.window.map(|v| v as f64);
        let pts = [at(x0, y0), at(x1, y0), at(x1, y1), at(x0, y1), at(x0, y0)];
        let colour = theme::accent();
        painter.extend(egui::Shape::dashed_line(
            &pts,
            egui::Stroke::new(3.5, Color32::from_black_alpha(150)),
            7.0,
            5.0,
        ));
        painter.extend(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.6, colour), 7.0, 5.0));
        let running =
            self.zeit.jobs.iter().any(|j| j.output_dir == p.dir && j.status.lock().unwrap().state == JobState::Running);
        let label = if running { "Preview..." } else { "preview" };
        let font = egui::FontId::proportional(12.0);
        painter.text(pts[0] + egui::vec2(4.0, 2.0), egui::Align2::LEFT_TOP, label, font, colour);
    }

    /// Finished runs: their maps loaded as result layers (on a thread), their series opened as layers.
    pub(crate) fn pump_zeit_jobs(&mut self, ctx: &egui::Context) {
        let mut vectors = vec![];
        for j in &mut self.zeit.jobs {
            if j.loaded {
                continue;
            }
            let mut st = j.status.lock().unwrap();
            if st.state != JobState::Done {
                continue;
            }
            j.loaded = true;
            let Some(res) = st.result.take() else { continue };
            drop(st);
            let tool_name = manifest().tool(&j.tool).map_or(j.tool.clone(), |t| t.name.clone());
            let [x0, y0, x1, y1] = res.window;
            // the series' grid: its maps placed by it even once it is closed
            let (session, active) = (&self.session, self.active);
            let grid = self.layers.iter().enumerate().find_map(|(i, l)| {
                let s = if i == active { session.as_ref() } else { l.session.as_ref() };
                s.filter(|s| s.info.id == j.cube).map(|s| (s.info.clone(), l.name.clone()))
            });
            let source = grid.as_ref().map_or(String::new(), |(_, n)| n.clone());
            let grid = grid.map(|(g, _)| g);
            let preview = self.zeit.preview.as_ref().is_some_and(|p| p.dir == j.output_dir);
            let mut first = true;
            for (output, o) in res.outputs.iter().enumerate() {
                let path = o["path"].as_str().unwrap_or("").to_string();
                if o["vector"].as_bool() == Some(true) {
                    // SNIC's polygons: a vector layer
                    vectors.push(path);
                    continue;
                }
                if o["series"].as_bool() == Some(true) {
                    // e.g. the NDFI of every date: a new layer (hidden: the Gi* under the hot spots' patterns)
                    self.zeit.series_to_open.push((path, o["hidden"].as_bool() == Some(true)));
                    continue;
                }
                let colormap = o["colormap"].as_str().unwrap_or("Viridis");
                let class_series = o["class_series"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .map(|c| {
                                let f = |k: &str| -> Vec<f64> {
                                    c[k].as_array()
                                        .map(|v| v.iter().map(|x| x.as_f64().unwrap_or(f64::NAN)).collect())
                                        .unwrap_or_default()
                                };
                                (f("x"), f("y"))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut layer = ResultLayer {
                    id: 0,
                    cube: j.cube,
                    grid: grid.clone(),
                    source: source.clone(),
                    place: Default::default(),
                    z: 0,
                    run: j.output_dir.clone(),
                    output,
                    name: format!(
                        "{}{tool_name}: {}",
                        if preview { "Preview: " } else { "" },
                        o["name"].as_str().unwrap_or("")
                    ),
                    path: path.clone(),
                    unit: o["unit"].as_str().unwrap_or("").to_string(),
                    cmap: janus_gpu::colormaps::by_name(colormap).unwrap_or(janus_gpu::colormaps::VIRIDIS),
                    x0,
                    y0,
                    w: x1 - x0,
                    h: y1 - y0,
                    tw: 0,
                    th: 0,
                    data: vec![],
                    tex: None,
                    lo: 0.0,
                    hi: 1.0,
                    visible: first, // the first output (e.g. the year of detection) shown
                    opacity: 1.0,
                    classes: o["classes"]
                        .as_array()
                        .map(|a| a.iter().map(|c| c.as_str().unwrap_or("").to_string()).collect())
                        .unwrap_or_default(),
                    class_series,
                    upgrading: false,
                    preview,
                };
                first = false;
                let loaded = self.zeit.loaded.clone();
                self.zeit.pending_loads += 1;
                let c = ctx.clone();
                std::thread::spawn(move || {
                    let r = load_result(&layer.path, 4096).map(|(data, tw, th)| {
                        (layer.data, layer.tw, layer.th) = (data, tw, th);
                        layer.auto_range();
                        layer
                    });
                    loaded.lock().unwrap().push(r);
                    c.request_repaint();
                });
            }
        }
        if !vectors.is_empty() {
            self.add_vector_files(vectors, ctx);
        }
        // loaded results: on the GPU, layers of their own (if a raster is still open to place them on)
        let done: Vec<_> = std::mem::take(&mut *self.zeit.loaded.lock().unwrap());
        for r in done {
            self.zeit.pending_loads -= 1;
            match r {
                Err(e) => self.error_popup = Some(e),
                Ok(mut layer) => {
                    if self.session.is_none() || layer.grid.is_none() {
                        continue;
                    }
                    if let Some(gpu) = &self.gpu {
                        layer.tex = Some(gpu.create_tile(layer.tw as u32, layer.th as u32, &layer.texture_values()));
                    }
                    self.push_result(layer);
                }
            }
        }
        self.upgrade_results(ctx);
        // series outputs join as layers, one at a time; the series being studied stays active
        if !self.zeit.series_to_open.is_empty() && self.opening.is_none() && !self.layers.is_empty() {
            let (path, hidden) = self.zeit.series_to_open.remove(0);
            self.zeit.keep_active = Some(self.active);
            self.zeit.open_hidden = hidden;
            self.open_inputs(vec![path], true, ctx);
        }
    }

    /// A result map read again from its file (a project opened): loaded on a thread as a run's maps are, its
    /// colours' range kept (`auto`: computed from its values).
    pub(crate) fn load_result_layer(&mut self, mut layer: ResultLayer, auto: bool, ctx: &egui::Context) {
        let loaded = self.zeit.loaded.clone();
        self.zeit.pending_loads += 1;
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = load_result(&layer.path, 4096).map(|(data, tw, th)| {
                (layer.data, layer.tw, layer.th) = (data, tw, th);
                if auto {
                    layer.auto_range();
                }
                layer
            });
            loaded.lock().unwrap().push(r);
            c.request_repaint();
        });
    }

    /// A result is first read reduced (at most 4096 px a side, quick); once shown, its copy at full resolution (up
    /// to the GPU's largest texture) is read in the background, or its pixels would not fall on the series'.
    fn upgrade_results(&mut self, ctx: &egui::Context) {
        let max = self.render_state.device.limits().max_texture_dimension_2d as usize;
        for r in self.zeit.results.iter_mut() {
            if r.visible && !r.upgrading && (r.tw < r.w || r.th < r.h) && r.w.max(r.h) <= max && !r.data.is_empty() {
                r.upgrading = true;
                let (path, done, c) = (r.path.clone(), self.zeit.upgraded.clone(), ctx.clone());
                std::thread::spawn(move || {
                    if let Ok((data, tw, th)) = load_result(&path, max) {
                        done.lock().unwrap().push((path, data, tw, th));
                        c.request_repaint();
                    }
                });
            }
        }
        let done: Vec<_> = std::mem::take(&mut *self.zeit.upgraded.lock().unwrap());
        for (path, data, tw, th) in done {
            if let Some(r) = self.zeit.results.iter_mut().find(|r| r.path == path) {
                (r.data, r.tw, r.th) = (data, tw, th);
                if let Some(gpu) = &self.gpu {
                    r.tex = Some(gpu.create_tile(tw as u32, th as u32, &r.texture_values()));
                }
                self.map.dirty = true;
            }
        }
    }

    /// Tools > Tasks: the raster runs, their progress, cancel, their folder.
    pub(crate) fn tasks_window(&mut self, ctx: &egui::Context) {
        if !self.zeit.show_tasks || self.docked(crate::app::Tab::Tasks) {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Tasks", ctx, 560.0)
            .open(&mut open)
            .max_width(900.0)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.tasks_ui(ui)));
        self.zeit.show_tasks = open;
    }

    /// The runs: their progress, log, folder.
    pub(crate) fn tasks_ui(&mut self, ui: &mut Ui) {
        let mut remove = None;
        let mut show_log = None;
        {
            if self.zeit.jobs.is_empty() {
                ui.weak("No tasks yet. Run a tool from the Tools menu.");
            }
            for (i, j) in self.zeit.jobs.iter().enumerate() {
                ui.push_id(i, |ui| {
                    let s = j.status.lock().unwrap();
                    ui.label(&j.title);
                    let el = if s.state == JobState::Running { j.started.elapsed().as_secs_f64() } else { s.seconds };
                    let name = match s.state {
                        JobState::Running => "running",
                        JobState::Done => "done",
                        JobState::Cancelled => "cancelled",
                        JobState::Failed => "failed",
                    };
                    let bw = ui.text_style_height(&egui::TextStyle::Body) * 4.5;
                    let bar = egui::ProgressBar::new(s.progress as f32)
                        .text(format!("{name}  {:.0}%  {el:.0} s", s.progress * 100.0));
                    // the buttons from the right (Log, then Cancel or Folder and Remove), the bar in what they leave
                    crate::panels::progress_row(ui, bar, |ui| {
                        if s.state == JobState::Running {
                            if ui.add(egui::Button::new("Cancel").min_size(egui::vec2(bw, 0.0))).clicked() {
                                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        } else {
                            if ui.add(egui::Button::new("Remove").min_size(egui::vec2(bw, 0.0))).clicked() {
                                remove = Some(i);
                            }
                            if ui.add(egui::Button::new("Folder").min_size(egui::vec2(bw, 0.0))).clicked() {
                                crate::platform::open_in_explorer(&j.output_dir);
                            }
                        }
                        let log = ui.add(egui::Button::new("Log").min_size(egui::vec2(bw, 0.0)));
                        if log.on_hover_text(&j.log_path).clicked() {
                            show_log = Some(j.log_path.clone());
                        }
                    });
                    if s.state == JobState::Running && !s.message.is_empty() {
                        ui.weak(&s.message);
                    }
                    if !s.error.is_empty() {
                        ui.colored_label(theme::error(), &s.error);
                    }
                    ui.separator();
                });
            }
        }
        if let Some(i) = remove {
            self.zeit.jobs.remove(i);
        }
        if let Some(path) = show_log {
            (self.zeit.log.path, self.zeit.log.show) = (path, true);
        }
    }

    /// Tasks > Log: a run's log, its end, followed as it grows (looked at 4 times a second).
    pub(crate) fn log_window(&mut self, ctx: &egui::Context) {
        if !self.zeit.log.show {
            return;
        }
        // what can be shown: every run's log, newest first
        let sources: Vec<(String, String)> =
            self.zeit.jobs.iter().rev().map(|j| (j.title.clone(), j.log_path.clone())).collect();
        let v = &mut self.zeit.log;
        if !v.external && !sources.iter().any(|s| s.1 == v.path) {
            match sources.first() {
                Some(s) => v.path = s.1.clone(),
                None => {
                    v.show = false;
                    return;
                }
            }
        }
        const KEEP: u64 = 2 << 20; // bytes kept (the end of a long log)
        if v.path != v.shown || v.checked.is_none_or(|t| t.elapsed().as_secs_f64() >= 0.25) {
            v.checked = Some(Instant::now());
            let size = std::fs::metadata(&v.path).map_or(0, |m| m.len());
            if v.path != v.shown || size < v.size {
                // another file, or this one started again
                v.shown = v.path.clone();
                v.text.clear();
                v.size = size.saturating_sub(KEEP);
                v.cut = v.size > 0;
            }
            if size > v.size {
                use std::io::{Read as _, Seek as _};
                let mut add = Vec::new();
                if let Ok(mut f) = std::fs::File::open(&v.path)
                    && f.seek(std::io::SeekFrom::Start(v.size)).is_ok()
                {
                    let _ = f.take(size - v.size).read_to_end(&mut add);
                }
                v.size += add.len() as u64;
                v.text.push_str(&String::from_utf8_lossy(&add).replace('\r', ""));
                if v.text.len() as u64 > 2 * KEEP {
                    // the end only, from a line start
                    let mut from = v.text.len() - KEEP as usize;
                    while !v.text.is_char_boundary(from) {
                        from += 1;
                    }
                    let from = v.text[from..].find('\n').map_or(from, |k| from + k + 1);
                    v.text.drain(..from);
                    v.cut = true;
                }
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(250));
        let mut open = true;
        egui::Window::new("Log").open(&mut open).default_size([720.0, 400.0]).show(ctx, |ui| {
            ui.horizontal(|ui| {
                let preview = sources.iter().find(|s| s.1 == v.path).map_or("", |s| s.0.as_str());
                egui::ComboBox::from_id_salt("log-source")
                    .width(ui.text_style_height(&egui::TextStyle::Body) * 26.0)
                    .selected_text(preview)
                    .show_ui(ui, |ui| {
                        for (title, p) in &sources {
                            if ui.selectable_label(*p == v.path, title).on_hover_text(p).clicked() {
                                v.path = p.clone();
                            }
                        }
                    });
                if ui.button("Copy").on_hover_text("Copies the log shown").clicked() {
                    ui.ctx().copy_text(v.text.clone());
                }
                if ui.button("Open file").clicked() {
                    crate::platform::open_in_explorer(&v.path);
                }
                if ui.button("Folder").clicked()
                    && let Some(d) = std::path::Path::new(&v.path).parent()
                {
                    crate::platform::open_in_explorer(&d.to_string_lossy());
                }
            });
            ui.weak(format!("{}{}", v.path, if v.cut { "  (its end)" } else { "" }));
            egui::Frame::group(ui.style()).show(ui, |ui| {
                if v.text.is_empty() {
                    ui.weak("(no log yet)");
                    return;
                }
                // errors in the error colour, warnings in the warning's, a run's first line ("=== date ...") in
                // the accent
                let lines: Vec<&str> = v.text.lines().collect();
                let row = ui.text_style_height(&egui::TextStyle::Body);
                egui::ScrollArea::both().auto_shrink(false).stick_to_bottom(true).show_rows(
                    ui,
                    row,
                    lines.len(),
                    |ui, range| {
                        for line in &lines[range] {
                            let has = |k: &str| line.contains(k);
                            let err = ["Traceback", "Error", "Exception", "error:", "] failed"].into_iter().any(has);
                            let colour = if err {
                                Some(theme::error())
                            } else if has("Warning") || has("warning:") {
                                Some(theme::warning())
                            } else if line.starts_with("=== ") {
                                Some(theme::accent())
                            } else {
                                None
                            };
                            let text = egui::RichText::new(*line);
                            ui.add(egui::Label::new(colour.map_or(text.clone(), |c| text.color(c))).extend());
                        }
                    },
                );
            });
        });
        self.zeit.log.show = open;
    }
}

/// A result map's settings (a row of the Layers panel unfolded): its colormap and range, or its classes' legend.
pub(crate) fn result_details_ui(ui: &mut Ui, r: &mut ResultLayer, dirty: &mut bool) {
    if r.is_segments() {
        ui.weak("Each segment a colour (hover: its number)");
    } else if r.classes.is_empty() {
        let maps = janus_gpu::colormaps::all();
        ComboBox::from_id_salt("rcmap").selected_text(maps[r.cmap].name).show_ui(ui, |ui| {
            for (k, m) in maps.iter().enumerate() {
                if ui.selectable_label(k == r.cmap, m.name).clicked() {
                    r.cmap = k;
                    *dirty = true;
                }
            }
        });
        ui.horizontal(|ui| {
            let speed = ((r.hi - r.lo) / 300.0).max(1e-6);
            *dirty |= ui.add(egui::DragValue::new(&mut r.lo).speed(speed).max_decimals(4)).changed();
            *dirty |= ui.add(egui::DragValue::new(&mut r.hi).speed(speed).max_decimals(4)).changed();
            if ui.button("Auto").clicked() {
                r.auto_range();
                *dirty = true;
            }
        });
    } else {
        // a class map's legend
        let cm = &janus_gpu::colormaps::all()[r.cmap];
        for (k, c) in r.classes.iter().enumerate() {
            ui.horizontal(|ui| {
                let [cr, cg, cb, _] = cm.sample((k as f32 + 0.5) / cm.keys().count() as f32);
                let (rect, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(cr, cg, cb));
                ui.label(c);
            });
        }
    }
    ui.horizontal(|ui| {
        ui.weak("opacity");
        *dirty |= ui.add(egui::Slider::new(&mut r.opacity, 0.0..=1.0).fixed_decimals(2)).changed();
    });
}

/// A result map, if shown, through its own placement on the map (its series' grid on the active layer's).
pub(crate) fn draw_result(
    gpu: &janus_gpu::Gpu,
    f: &mut janus_gpu::MapFrame,
    r: &ResultLayer,
    to_map: &dyn Fn([f64; 4]) -> [f32; 4],
) {
    let (Some(tex), Some(info)) = (&r.tex, &r.grid) else { return };
    if !r.visible || !r.place.aligned {
        return;
    }
    let (x0, y0) = (r.x0 as f64, r.y0 as f64);
    if let Some((q, warp)) = r.place.quad(info, x0, y0, x0 + r.w as f64, y0 + r.h as f64) {
        gpu.draw_overlay(f, tex, to_map(q), [r.lo, r.hi], r.cmap, r.opacity, warp.as_ref());
    }
}

/// A series' visible result maps over it, through its placement on the map (a map panel of that series).
pub(crate) fn draw_results(
    gpu: &janus_gpu::Gpu,
    f: &mut janus_gpu::MapFrame,
    results: &[ResultLayer],
    info: &CubeInfo,
    place: &crate::layers::Placement,
    to_map: &dyn Fn([f64; 4]) -> [f32; 4],
) {
    for r in results.iter().filter(|r| r.cube == info.id && r.visible) {
        let Some(tex) = &r.tex else { continue };
        let (x0, y0) = (r.x0 as f64, r.y0 as f64);
        if let Some((q, warp)) = place.quad(info, x0, y0, x0 + r.w as f64, y0 + r.h as f64) {
            gpu.draw_overlay(f, tex, to_map(q), [r.lo, r.hi], r.cmap, r.opacity, warp.as_ref());
        }
    }
}

impl ZeitUi {
    /// Result maps or series of a run still being read.
    pub fn loads_pending(&self) -> bool {
        self.pending_loads > 0 || !self.loaded.lock().unwrap().is_empty() || !self.series_to_open.is_empty()
    }

    /// Every run done and its maps loaded (or failed).
    pub fn runs_settled(&self) -> bool {
        !self.jobs.is_empty()
            && self.jobs.iter().all(|j| {
                j.loaded
                    || j.status.lock().unwrap().state != JobState::Running
                        && j.status.lock().unwrap().state != JobState::Done
            })
            && self.loaded.lock().unwrap().is_empty()
            && self.pending_loads == 0
    }
}

impl State {
    /// --zeit-run: the --zeit tool run once the series is loaded (on the ROI's window if there is one).
    pub(crate) fn scripted_zeit_run(&mut self, ctx: &egui::Context) {
        if !self.opts.zeit_run || !self.zeit.jobs.is_empty() {
            return;
        }
        // the first of several
        let first = self.opts.zeit_tool.as_deref().and_then(|ids| ids.split(',').next());
        let Some(tool) = first.and_then(|id| manifest().tool(id.trim())) else { return };
        if !self.session.as_ref().is_some_and(|s| s.ready_ms.is_some()) {
            return;
        }
        if self.series.roi_rect.is_some() {
            self.zeit.tools.entry(tool.id.clone()).or_default().scope = 2;
        }
        self.run_tool(tool, ctx);
    }
}

impl State {
    /// The run's time measured again once the series, the parameters or the bands stay for 0.6 s.
    fn update_estimate(&mut self, tool: &'static Tool, ctx: &egui::Context) {
        let roles = self.active_roles();
        let Some(id) = self.session.as_ref().map(|s| s.info.id) else { return };
        let threads = self.zeit.tools.get(&tool.id).and_then(|u| u.threads);
        let key =
            format!("{id} {} {roles:?} {threads:?}", serde_json::to_string(self.zeit.params(tool)).unwrap_or_default());
        let u = self.zeit.tools.entry(tool.id.clone()).or_default();
        if let Some(rx) = &u.est_job {
            if let Ok(r) = rx.try_recv() {
                u.est = Some(r);
                u.est_job = None;
            }
            return;
        }
        if key == u.est_key {
            return;
        }
        let now = Instant::now();
        match u.est_changed {
            None => {
                u.est_changed = Some(now);
                ctx.request_repaint_after(std::time::Duration::from_millis(650));
                return;
            }
            Some(t) if now - t < std::time::Duration::from_millis(600) => {
                ctx.request_repaint_after(std::time::Duration::from_millis(650));
                return;
            }
            _ => {}
        }
        u.est_changed = None;
        u.est_key = key;
        u.est = None;
        // on the whole image
        let scope = std::mem::take(&mut u.scope);
        let spec = self.job_spec(tool);
        let u = self.zeit.tools.entry(tool.id.clone()).or_default();
        u.scope = scope;
        match spec {
            Err(e) => u.est = Some(Err(e)),
            Ok((spec, ..)) => {
                let (tx, rx) = channel();
                let c = ctx.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(janus_zeit::job::estimate(&spec));
                    c.request_repaint();
                });
                u.est_job = Some(rx);
            }
        }
    }

    /// The typical series of the class under the cursor, from the topmost visible class map that has them (a
    /// SOM's clusters): its class colour over a dark outline, a model and not data.
    pub(crate) fn draw_class_series(&self, plot: &mut PlotUi, shown: &crate::chart::AsShown) {
        let (Some(s), Some(h)) = (&self.session, &self.series.hover) else { return };
        let info = &s.info;
        for r in self.zeit.results.iter().rev() {
            if r.cube != info.id || !r.visible || r.class_series.is_empty() {
                continue;
            }
            let v = r.value_at(h.x, h.y);
            let k = if v.is_nan() { 0 } else { v.round() as usize };
            let Some((x, y)) = k.checked_sub(1).and_then(|i| r.class_series.get(i)) else { continue };
            let pts: Vec<[f64; 2]> = x
                .iter()
                .zip(y)
                .map(|(&x, &y)| {
                    let x = info.x_from_decimal_year(x);
                    [x, shown.at(y, x)]
                })
                .collect();
            let name =
                format!("{}: typical series", r.classes.get(k - 1).cloned().unwrap_or_else(|| format!("class {k}")));
            let cm = &janus_gpu::colormaps::all()[r.cmap];
            let n = cm.keys().count().max(1);
            let [cr, cg, cb, _] = cm.sample(((k - 1) % n) as f32 / (n - 1).max(1) as f32);
            plot.line(
                Line::new(&name, PlotPoints::from(pts.clone()))
                    .color(Color32::from_rgba_unmultiplied(13, 13, 20, 230))
                    .width(5.0),
            );
            plot.line(
                Line::new(&name, PlotPoints::from(pts)).color(theme::on_plot(Color32::from_rgb(cr, cg, cb))).width(2.5),
            );
            return; // one map's only
        }
    }
}

/// A run's spec on a series, without its inputs, window and output folder.
pub(crate) fn base_spec(
    info: &CubeInfo,
    tool: &Tool,
    params: &BTreeMap<String, Value>,
    t: usize,
    threads: usize,
) -> janus_zeit::job::JobSpec {
    janus_zeit::job::JobSpec {
        tool: tool.id.clone(),
        params: params.clone(),
        years: (0..info.t()).map(|t| info.decimal_year(t)).collect(),
        days: info.time_is_date.then(|| (0..info.t()).map(|t| info.unix_day(t)).collect()),
        labels: Some(info.layers.iter().map(|l| l.label.clone()).collect()),
        map_year: Some(info.decimal_year(t.min(info.t().saturating_sub(1)))),
        threads: (threads > 0).then_some(threads as i64),
        ..Default::default()
    }
}

/// The series as VRTs at `stem`* (the shown band, its normalized difference's other band, the quality band, each
/// mapped role's band), as the C++ wrote them for the bridge, into the spec.
pub(crate) fn series_inputs(
    info: &CubeInfo,
    tool: &Tool,
    roles: &BTreeMap<String, i32>,
    stem: &std::path::Path,
    spec: &mut janus_zeit::job::JobSpec,
) -> Result<(), String> {
    let vrt = |suffix: &str, band: i32, raw: bool| -> Result<String, String> {
        let path = format!("{}{suffix}.vrt", stem.display());
        janus_cube::vrt::write_cube_vrt(info, &path, band, raw).map(|_| path)
    };
    spec.input = vrt("", 0, false)?;
    spec.shown = shown_of(info, roles);
    if info.sel.nd_band > 0 {
        spec.nd_input = Some(vrt(&format!("_b{}", info.sel.nd_band), info.sel.nd_band, false)?);
    }
    if info.sel.qa_band > 0 {
        spec.qa_input = Some(vrt("_qa", info.sel.qa_band, true)?);
        spec.qa_rule = Some(info.sel.qa_rule.id().to_string());
    }
    for (role, b) in tool_bands(tool, info, roles) {
        spec.bands.insert(role.clone(), vrt(&format!("_{role}"), b, false)?);
        if tool.requires.all_bands == Some(true) {
            spec.band_names
                .insert(role, info.band_names.get(b as usize - 1).cloned().unwrap_or_else(|| format!("band {b}")));
        }
    }
    if tool.id == "snic" && spec.params.get("features").and_then(Value::as_str) == Some("bands") {
        // SNIC on every band of every date
        for b in 1..=info.bands_per_date.max(1) {
            let role = format!("b{b:03}");
            spec.bands.insert(role.clone(), vrt(&format!("_{role}"), b as i32, false)?);
            spec.band_names.insert(role, info.band_names.get(b - 1).cloned().unwrap_or_else(|| format!("band {b}")));
        }
    }
    Ok(())
}

/// The roles of a series' bands (guessed from their names).
pub(crate) fn guess_roles(info: &CubeInfo) -> BTreeMap<String, i32> {
    janus_cube::roles::guess_band_roles(info).into_iter().map(|(r, b)| (r.to_string(), b)).collect()
}

/// A series of a series' pixel as a pixel run gets it, its bands read (multiband tools).
pub(crate) fn read_pixel_input(
    info: &Arc<CubeInfo>,
    tool: &Tool,
    roles: &BTreeMap<String, i32>,
    values: &[f32],
    x: usize,
    y: usize,
    t: usize,
) -> PixelInput {
    let mut input = pixel_input(info, values, t, roles);
    if reads_bands(tool) {
        let mut reader = CubeReader::new(info.clone());
        let n = info.t();
        let mut read = |band: i32, raw: bool| -> Vec<f64> {
            (0..n)
                .map(|t| reader.read_band_pixel(t, band, x, y, raw).map_or(f64::NAN, f64::from))
                .map(|v| if v.is_finite() { v } else { f64::NAN })
                .collect()
        };
        input.bands = tool_bands(tool, info, roles).into_iter().map(|(r, b)| (r, read(b, false))).collect();
        if info.sel.qa_band > 0 {
            input.qa = Some(read(info.sel.qa_band, true));
        }
    }
    input
}

impl ResultLayer {
    /// A result map read for a check (no series, no texture).
    pub(crate) fn for_test(data: Vec<f32>, tw: usize, th: usize, unit: String) -> ResultLayer {
        ResultLayer {
            id: 0,
            cube: 0,
            grid: None,
            source: String::new(),
            place: Default::default(),
            z: 0,
            run: String::new(),
            output: 0,
            name: String::new(),
            path: String::new(),
            unit,
            cmap: janus_gpu::colormaps::VIRIDIS,
            x0: 0,
            y0: 0,
            w: tw,
            h: th,
            tw,
            th,
            data,
            tex: None,
            lo: 0.0,
            hi: 1.0,
            visible: false,
            opacity: 1.0,
            classes: vec![],
            class_series: vec![],
            upgrading: false,
            preview: false,
        }
    }
}
