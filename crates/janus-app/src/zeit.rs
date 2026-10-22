//! Zeit's tools in Janus (src/app_zeit.cpp), native: the Tools menu, a window per tool made from its descriptor,
//! the tool fitted on the chart's series (the cursor, the pins, the ROI mean, the other layers') on a worker
//! thread, its model drawn over each series and its numbers in the Statistics table.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use egui::{Color32, ComboBox, Ui};
use egui_plot::{Line, PlotPoints, PlotUi, Points, VLine};
use janus_cube::stats::SeriesStats;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FitKey {
    pub cube: u64,
    pub which: Which,
}

/// A fit asked for or done: of the series at (x, y) with the tool's settings of `version`.
pub struct Fit {
    x: usize,
    y: usize,
    version: u64,
    pub result: Option<Result<PixelResult, String>>,
}

/// A tool's window: open, its parameters, where a raster run goes (0 whole image, 1 visible area, 2 ROI).
#[derive(Default)]
pub struct ToolUi {
    pub open: bool,
    pub params: BTreeMap<String, Value>,
    pub scope: usize,
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
    version: u64,
    tool: String,
    params: Params,
    input: PixelInput,
    bands: Option<BandRead>,
}

struct Reply {
    key: FitKey,
    x: usize,
    y: usize,
    version: u64,
    result: Result<PixelResult, String>,
    ms: f64,
}

#[derive(Default)]
pub struct ZeitUi {
    pub tools: HashMap<String, ToolUi>,
    /// the tool fitted on the chart
    pub pixel_tool: Option<String>,
    /// bumped when the tool, its parameters or its bands change: the fits are made again
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
    if info.spectral {
        return Some("a spectrum (one band per wavelength), not a series in time".into());
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

/// What the shown series is in band roles (multiband series).
fn shown_of(info: &CubeInfo, roles: &BTreeMap<String, i32>) -> Option<Shown> {
    if info.bands_per_date <= 1 {
        return None;
    }
    let role_of = |b: i32| roles.iter().find(|(_, v)| **v == b).map(|(r, _)| r.clone());
    let sel = &info.sel;
    Some(if sel.nd_band > 0 {
        Shown { band: None, nd: role_of(sel.band).zip(role_of(sel.nd_band)).map(|(a, b)| vec![a, b]) }
    } else {
        Shown { band: role_of(sel.band), nd: None }
    })
}

/// A series of a layer as a pixel run gets it.
fn pixel_input(info: &CubeInfo, values: &[f32], t: usize, roles: &BTreeMap<String, i32>) -> PixelInput {
    PixelInput {
        years: (0..info.t()).map(|i| info.decimal_year(i)).collect(),
        days: info.time_is_date.then(|| (0..info.t()).map(|i| info.unix_day(i)).collect()),
        values: values.iter().map(|&v| if v.is_finite() { f64::from(v) } else { f64::NAN }).collect(),
        shown: shown_of(info, roles),
        map_year: Some(info.decimal_year(t.min(info.t().saturating_sub(1)))),
        qa_rule: (info.sel.qa_band > 0).then(|| info.sel.qa_rule.id().to_string()),
        ..Default::default()
    }
}

impl ZeitUi {
    /// A layer's fit of a series, if it is of that place and of the tool's current settings.
    pub fn fit_of(&self, cube: u64, which: Which, x: usize, y: usize) -> Option<&Result<PixelResult, String>> {
        self.pixel_tool.as_ref()?;
        let f = self.fits.get(&FitKey { cube, which })?;
        (f.x == x && f.y == y && f.version == self.version).then_some(f.result.as_ref()).flatten()
    }

    /// Whether a fit of that series is wanted (the tool's settings or the place changed).
    fn wanted(&self, key: FitKey, x: usize, y: usize) -> bool {
        self.fits.get(&key).is_none_or(|f| f.x != x || f.y != y || f.version != self.version)
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
        let Some(id) = self.zeit.pixel_tool.clone() else { return };
        let Some(tool) = manifest().tool(&id) else { return };
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        if applicability(tool, &info).is_some() {
            return;
        }
        let roles = self.active_roles();
        let (needed, optional) = tool_roles(tool);
        let multiband = !needed.is_empty();
        if multiband && needed.iter().any(|r| !roles.contains_key(r)) {
            return;
        }
        // a tool on the map's date is fitted again when it changes
        if tool.map_date == Some(true) && self.zeit.map_t != Some(self.display.t) {
            self.zeit.map_t = Some(self.display.t);
            self.zeit.version += 1;
        }
        let params = Params::with_defaults(&id, self.zeit.params(tool)).unwrap_or_default();
        let t = self.display.t;
        let bands = || {
            multiband.then(|| BandRead {
                info: info.clone(),
                roles: needed.iter().chain(&optional).filter_map(|r| roles.get(r).map(|b| (r.clone(), *b))).collect(),
                qa_band: info.sel.qa_band,
            })
        };
        let mut requests = vec![];
        let version = self.zeit.version;
        let mut ask = |zeit: &ZeitUi,
                       cube: &CubeInfo,
                       which: Which,
                       view: &crate::series::SeriesView,
                       t: usize,
                       b: Option<BandRead>| {
            let key = FitKey { cube: cube.id, which };
            if view.exact && zeit.wanted(key, view.x, view.y) {
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
            let key = FitKey { cube: info.id, which: Which::Roi };
            let roi_gen = self.series.roi_gen as usize;
            if self.zeit.wanted(key, roi_gen, 0) {
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
        for r in requests {
            self.zeit.send(ctx, r);
        }
    }

    /// The ROI's fit, if it is of the current ROI.
    pub(crate) fn roi_fit(&self) -> Option<&Result<PixelResult, String>> {
        let s = self.session.as_ref()?;
        self.zeit.fit_of(s.info.id, Which::Roi, self.series.roi_gen as usize, 0)
    }

    /// The Tools menu: Zeit's tools by category (each opens its window).
    pub(crate) fn tools_menu(&mut self, ui: &mut Ui) {
        let m = manifest();
        let mut categories: Vec<&str> = vec![];
        for t in &m.tools {
            if !categories.contains(&t.category.as_str()) {
                categories.push(&t.category);
            }
        }
        for c in categories {
            ui.label(egui::RichText::new(c).weak().small());
            for t in m.tools.iter().filter(|t| t.category == c) {
                let open = self.zeit.tools.get(&t.id).is_some_and(|u| u.open);
                if ui.add(egui::Button::selectable(open, &t.name)).clicked() {
                    self.zeit.tools.entry(t.id.clone()).or_default().open = !open;
                    ui.close();
                }
            }
            ui.separator();
        }
        ui.weak(format!("Zeit {} (native, zeit-core)", janus_zeit::ZEIT_VERSION));
    }

    /// The open tools' windows.
    pub(crate) fn tool_windows(&mut self, ctx: &egui::Context) {
        let open: Vec<String> = self.zeit.tools.iter().filter(|(_, u)| u.open).map(|(id, _)| id.clone()).collect();
        for id in open {
            let Some(tool) = manifest().tool(&id) else { continue };
            let mut keep = true;
            egui::Window::new(&tool.name)
                .id(egui::Id::new(("zeit tool", &id)))
                .open(&mut keep)
                .default_size([440.0, 560.0])
                .scroll(true)
                .show(ctx, |ui| self.tool_window(ui, tool));
            if !keep {
                self.zeit.tools.entry(id).or_default().open = false;
            }
        }
    }

    fn tool_window(&mut self, ui: &mut Ui, tool: &'static Tool) {
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
                                changed = true;
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
        if changed && self.zeit.pixel_tool.as_deref() == Some(&tool.id) {
            self.zeit.version += 1;
        }

        if tool.modes.iter().any(|m| m == "pixel") {
            crate::panels::section(ui, "On the chart");
            let mut on = self.zeit.pixel_tool.as_deref() == Some(&tool.id);
            if ui
                .add_enabled(why.is_none(), egui::Checkbox::new(&mut on, "Fit the cursor, pins and ROI series"))
                .changed()
            {
                self.zeit.pixel_tool = on.then(|| tool.id.clone());
                self.zeit.version += 1;
            }
            ui.weak("Runs on every exact series; the model is drawn over it\nand its numbers are added to the Statistics table.");
        }
        if tool.modes.iter().any(|m| m == "raster") {
            self.raster_run_ui(ui, tool, why.is_some());
        }
    }

    /// The parameters' form, from the descriptor. Returns whether a value changed.
    fn params_ui(&mut self, ui: &mut Ui, tool: &Tool) -> bool {
        let mut changed = false;
        let pins: Vec<(u32, usize, usize, bool, Vec<f32>)> =
            self.series.pins.iter().map(|p| (p.id, p.x, p.y, p.exact, p.values.clone())).collect();
        let roi = self.series.roi.complete().then(|| self.series.roi.mean.clone()).filter(|m| !m.is_empty());
        let info = self.session.as_ref().map(|s| s.info.clone());
        let results: Vec<(String, String, String)> = self.zeit_result_maps();
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
                    "patterns" => {
                        ui.end_row();
                        changed |= patterns_ui(ui, p, v, &pins, roi.as_deref(), info.as_deref());
                    }
                    "layers" => {
                        ui.end_row();
                        changed |= layers_param_ui(ui, p, v, &results);
                    }
                    _ => {}
                }
                ui.end_row();
            }
        });
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

/// The fitted model's overlays on the chart, under the series' legend entry (hiding the series hides its
/// model). Every model in the same colour, used by no series, over a dark outline: never mistaken for data.
pub(crate) fn draw_fit(
    plot: &mut PlotUi,
    name: &str,
    fit: Option<&Result<PixelResult, String>>,
    st: &SeriesStats,
    values: usize,
    info: &CubeInfo,
) {
    let Some(Ok(r)) = fit else { return };
    let col = theme::on_plot(Color32::from_rgb(242, 38, 242));
    let outline = Color32::from_rgba_unmultiplied(13, 13, 20, 230);
    let shown = |v: f64| match values {
        1 => v - st.mean,
        2 => {
            if st.std > 0.0 {
                (v - st.mean) / st.std
            } else {
                0.0
            }
        }
        _ => v,
    };
    for o in &r.overlays {
        if o.kind == "vlines" {
            for &x in &o.x {
                let x = info.x_from_decimal_year(x);
                plot.vline(VLine::new(name, x).color(outline).width(3.5));
                plot.vline(VLine::new(name, x).color(col.gamma_multiply(0.85)).width(1.5));
            }
            continue;
        }
        let pts: Vec<[f64; 2]> =
            o.x.iter().zip(&o.y).map(|(&x, y)| [info.x_from_decimal_year(x), y.map_or(f64::NAN, shown)]).collect();
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
                    Line::new(name, PlotPoints::from(piece.to_vec())).color(col.gamma_multiply(alpha)).width(width),
                );
            }
        }
    }
}

/// The rows a column's fit adds to the Statistics table, by name.
pub(crate) fn fit_rows(fit: Option<&Result<PixelResult, String>>) -> Vec<(String, String)> {
    match fit {
        Some(Ok(r)) => r.rows.iter().map(|[a, b]| (a.clone(), b.clone())).collect(),
        _ => vec![],
    }
}

impl State {
    /// The result maps a "layers" parameter can choose: (name, path, unit).
    pub(crate) fn zeit_result_maps(&self) -> Vec<(String, String, String)> {
        vec![]
    }

    /// A raster run of the tool (scope, output folder, estimate, Run).
    fn raster_run_ui(&mut self, ui: &mut Ui, _tool: &Tool, _unavailable: bool) {
        crate::panels::section(ui, "Run on the raster");
        ui.weak("Raster runs: next.");
    }
}
