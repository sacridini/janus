//! Classification (IDEIAS.md, "IA"; janus-ml): Tools > Classification. Samples from what Janus already has (the
//! interpretation's labels, a validation project's references, a vector layer's class column, pins given a class,
//! grouped by class here), their series read at full resolution; a Random Forest, a TempCNN or an L-TAE trained on
//! them in the background (Tools > Tasks), its curves and the validation samples' confusion matrix in the window; the model kept in a .janusmodel file and
//! applied to the whole image, the visible area or the ROI, its class map, probability and uncertainty opening as
//! result layers.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{ComboBox, RichText, Ui};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use janus_cube::CubeInfo;
use janus_geo::vectors::Kind;
use janus_ml::forest::ForestHyper;
use janus_ml::model::shared_device;
use janus_ml::presto::{PrestoHyper, PrestoInputs};
use janus_ml::{Arch, Calendar, Class, Epoch, Header, InputBand, Model, Net, Norm, TrainConfig};
use serde_json::{Value, json};

use crate::app::State;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

pub(crate) const ARCHS: [&str; 5] = [
    "TempCNN (Pelletier et al. 2019)",
    "L-TAE (Garnot & Landrieu 2020)",
    "Presto (Tseng et al. 2023, pretrained)",
    "U-TAE (Garnot & Landrieu 2021, patches)",
    "Random Forest (Breiman 2001)",
];
/// The architecture that segments patches (dense labels)
const UTAE: usize = 3;
/// The forest (no network: no epochs, no GPU)
pub(crate) const FOREST: usize = 4;
/// The order in the list of models: the forest first
pub(crate) const ARCH_ORDER: [usize; 5] = [FOREST, 0, 1, 2, UTAE];
const PATCHES: [usize; 3] = [32, 64, 128];
const SOURCES: [&str; 5] = [
    "Interpretation labels",
    "Validation project (reference)",
    "Vector layer (class column)",
    "Pins (a class each)",
    "A class map (every pixel a label; U-TAE)",
];
const POLYGONS: [&str; 3] = ["Its centre", "Each pixel inside", "The mean of its pixels"];

/// A place samples come from: its class and its pixels (in the active layer), one sample each or their mean.
#[derive(Clone, Debug, Default)]
pub struct Site {
    pub class: String,
    /// what it is ("Pin 3", "Feature 12"), and its pin
    pub label: String,
    pub pin: Option<u32>,
    pub pixels: Vec<(usize, usize)>,
    pub mean: bool,
}

/// The samples and their classes (or why there are none).
type SitesResult = Result<(Vec<Site>, Vec<String>), String>;

/// A training under way or done.
#[derive(Default)]
pub struct TrainRun {
    pub arch: String,
    /// a forest's: `epochs` are its trees grown
    pub forest: bool,
    pub epochs_asked: usize,
    pub epochs: Vec<Epoch>,
    pub done: bool,
    pub error: String,
    pub model_path: String,
    pub header: Option<Header>,
    pub message: String,
}

pub struct MlUi {
    pub open: bool,
    pub arch: usize,
    pub source: usize,
    /// the vector layer and its class field (source 2)
    pub vector: u32,
    pub field: usize,
    pub polygons: usize,
    /// at most this many pixels of a polygon
    pub max_pixels: usize,
    pub shown: bool,
    pub bands: BTreeSet<i32>,
    pub pin_classes: BTreeMap<u32, String>,
    /// the pins' classes in their order (their colours), the class a click's pin is given ("" none), the pins
    /// coloured by class, the class being typed
    pub pin_class_list: Vec<String>,
    pub drop_class: String,
    pub pins_by_class: bool,
    new_class: String,
    pub forest: ForestHyper,
    pub cfg: TrainConfig,
    cfg_arch: usize,
    pub min_valid: f64,
    /// Presto: the year of its monthly composites (0: the series' last), or the series' own dates
    pub presto_year: i32,
    pub presto_dates: bool,
    /// U-TAE: the patches' side, starting from PASTIS's weights, the class map giving the labels (its file)
    pub patch: usize,
    pub pastis: bool,
    pub label_map: String,
    /// U-TAE: only the labels inside the ROI
    pub in_roi: bool,
    pub gpu: bool,
    pub scope: usize,
    /// the class map's post-processing when a model is applied
    pub post: janus_ml::post::PostProcess,
    /// also the model's area of applicability (when it kept its samples)
    pub aoa: bool,
    pub run: Option<Arc<Mutex<TrainRun>>>,
    /// the model to apply: its file and header
    pub model: Option<(String, Header)>,
    /// the window's tab (Samples, Method, Apply)
    pub tab: usize,
    /// the kind of method (the forest and networks by `arch`, TWDTW, embeddings, SOM)
    pub kind: crate::methods::Kind,
    /// its estimate on the samples (TWDTW, embeddings)
    pub method_check: crate::methods::MethodCheck,
    /// the samples of the source, and when they were made
    pub(crate) sites: Option<(Instant, SitesResult)>,
    /// the samples' quality, checked as they are gathered
    pub check: crate::sample_check::SampleCheck,
}

impl Default for MlUi {
    fn default() -> Self {
        MlUi {
            open: false,
            arch: 0,
            source: 0,
            vector: 0,
            field: 0,
            polygons: 0,
            max_pixels: 50,
            shown: true,
            bands: BTreeSet::new(),
            pin_classes: BTreeMap::new(),
            pin_class_list: vec![],
            drop_class: String::new(),
            pins_by_class: true,
            new_class: String::new(),
            forest: ForestHyper::default(),
            cfg: TrainConfig::tempcnn(),
            cfg_arch: 0,
            min_valid: 50.0,
            presto_year: 0,
            presto_dates: false,
            patch: 64,
            pastis: false,
            label_map: String::new(),
            in_roi: false,
            gpu: true,
            scope: 0,
            post: Default::default(),
            aoa: true,
            run: None,
            model: None,
            tab: 0,
            kind: Default::default(),
            method_check: Default::default(),
            sites: None,
            check: Default::default(),
        }
    }
}

impl MlUi {
    pub fn arch(&self) -> Arch {
        match self.arch {
            1 => Arch::LTae(Default::default()),
            2 => Arch::Presto(PrestoHyper::default()),
            UTAE => Arch::Utae(janus_ml::utae::UtaeHyper {
                patch: self.patch,
                overlap: self.patch / 4,
                pastis: self.pastis,
                ..Default::default()
            }),
            FOREST => Arch::RandomForest(self.forest.clone()),
            _ => Arch::TempCnn(Default::default()),
        }
    }

    /// The year of Presto's monthly composites (None: the series' own dates).
    pub fn presto_year(&self, info: &CubeInfo) -> Option<i32> {
        if self.presto_dates {
            return None;
        }
        Some(if self.presto_year > 0 {
            self.presto_year
        } else {
            info.decimal_year(info.t().saturating_sub(1)).floor() as i32
        })
    }

    pub fn training(&self) -> bool {
        self.run.as_ref().is_some_and(|r| !r.lock().unwrap().done)
    }
}

/// A job's work, a panic in it (a GPU out of memory, in Burn) its error: the job then fails with a message instead
/// of running for ever.
fn guarded<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let what = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        // Burn's messages run over many lines: the first and the cause at the bottom
        let lines: Vec<&str> = what.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let cause = lines.iter().rev().find(|l| !l.starts_with("Caused by") && !l.contains("backtrace"));
        Err(match (lines.first(), cause) {
            (Some(a), Some(b)) if a != b => format!("failed: {a} ({b})"),
            (Some(a), _) => format!("failed: {a}"),
            _ => "failed (a panic)".into(),
        })
    })
}

/// Each class's samples (a pixel each, or a place's mean).
fn counts_of(sites: &[Site], classes: &[String]) -> Vec<(String, usize)> {
    classes
        .iter()
        .map(|c| {
            let n = sites.iter().filter(|s| &s.class == c).map(|s| if s.mean { 1 } else { s.pixels.len() }).sum();
            (c.clone(), n)
        })
        .collect()
}

/// Class k's colour: the Paired colormap's, as the class map shows it.
pub fn class_color(k: usize) -> [u8; 3] {
    let maps = janus_gpu::colormaps::all();
    let cm = &maps[janus_gpu::colormaps::by_name("Paired").unwrap_or(0)];
    let n = cm.keys().count().max(1);
    let [r, g, b, _] = cm.sample(((k % n) as f32 + 0.5) / n as f32);
    [r, g, b]
}

/// The pixels of a placed feature: its anchor, or those whose centres are inside (at most `max`, spread evenly).
fn feature_pixels(
    poly: &janus_geo::plane::Polygon,
    anchor: [f64; 2],
    kind: Kind,
    every: bool,
    max: usize,
    info: &CubeInfo,
) -> Vec<(usize, usize)> {
    let inside = |x: f64, y: f64| x >= 0.0 && y >= 0.0 && (x as usize) < info.width && (y as usize) < info.height;
    if kind == Kind::Area
        && every
        && let Some(b) = janus_geo::plane::bounds(poly)
    {
        let (w, h) = (info.width as f64, info.height as f64);
        let (x0, y0) = (b[0].floor().clamp(0.0, w) as usize, b[1].floor().clamp(0.0, h) as usize);
        let (x1, y1) = (b[2].ceil().clamp(0.0, w) as usize, b[3].ceil().clamp(0.0, h) as usize);
        if x1 > x0 && y1 > y0 && (x1 - x0) * (y1 - y0) <= 4_000_000 {
            let (pw, ph) = (x1 - x0, y1 - y0);
            let m = janus_geo::plane::mask(poly, x0 as f64, y0 as f64, pw as f64, ph as f64, pw, ph);
            let all: Vec<(usize, usize)> = (0..pw * ph).filter(|&i| m[i]).map(|i| (x0 + i % pw, y0 + i / pw)).collect();
            if !all.is_empty() {
                let step = all.len().div_ceil(max.max(1));
                return all.into_iter().step_by(step.max(1)).collect();
            }
        }
    }
    if anchor[0].is_finite() && inside(anchor[0], anchor[1]) {
        return vec![(anchor[0] as usize, anchor[1] as usize)];
    }
    vec![]
}

impl State {
    /// The samples of the chosen source, as places in the active layer, and the classes in their order.
    pub(crate) fn ml_sites(&self) -> Result<(Vec<Site>, Vec<String>), String> {
        let info = &self.session.as_ref().ok_or("no series is open")?.info;
        let ui = &self.ml;
        let dense = ui.arch == UTAE;
        let every = ui.polygons > 0 || dense;
        let mean = ui.polygons == 2 && !dense;
        let max = if dense {
            usize::MAX / 2
        } else if mean {
            5000
        } else {
            ui.max_pixels.max(1)
        };
        let mut sites = vec![];
        let mut order: Vec<String> = vec![];
        let of_layer =
            |id: u32, class: &dyn Fn(usize) -> Option<String>, sites: &mut Vec<Site>| -> Result<(), String> {
                let v = self.vectors.iter().find(|v| v.id == id).ok_or("its vector layer is not open")?;
                if v.px.is_empty() && !v.data.features.is_empty() {
                    return Err(format!(
                        "{} is not placed on the active layer{}",
                        v.name,
                        if v.why.is_empty() { String::new() } else { format!(": {}", v.why) }
                    ));
                }
                for (i, f) in v.data.features.iter().enumerate() {
                    let Some(c) = class(i).filter(|c| !c.is_empty()) else { continue };
                    let (poly, anchor) =
                        (v.px.get(i).cloned().unwrap_or_default(), v.anchor.get(i).copied().unwrap_or([f64::NAN; 2]));
                    let pixels = feature_pixels(&poly, anchor, f.kind, every, max, info);
                    if !pixels.is_empty() {
                        let label = format!("Feature {}", i + 1);
                        sites.push(Site { class: c, label, pin: None, pixels, mean: mean && f.kind == Kind::Area });
                    }
                }
                Ok(())
            };
        match ui.source {
            0 => {
                let it = &self.labels;
                let id = it.vector.ok_or("no interpretation is going on (Interpretation panel)")?;
                order = it.classes.clone();
                of_layer(id, &|i| it.entries.get(i).map(|l| l.label.clone()), &mut sites)?;
            }
            1 => {
                let p = self.val.project.as_ref().ok_or("no validation project is open")?;
                order = p.label_set.clone();
                of_layer(
                    self.val.vector_id,
                    &|i| janus_validation::resolve(&p.labels, i).0.map(|l| l.label.clone()),
                    &mut sites,
                )?;
            }
            2 => {
                let v = self.vectors.iter().find(|v| v.id == ui.vector).ok_or("choose a vector layer")?;
                let field = ui.field;
                if field >= v.data.fields.len() {
                    return Err("choose its class column".into());
                }
                // the table read on a thread while the window shows the layer (pump_attributes)
                if !v.table_ready() {
                    return Err(match &v.attrs.table {
                        crate::attributes::Table::Failed(e) => format!("its attributes could not be read: {e}"),
                        _ => format!("reading the attributes of {}...", v.name),
                    });
                }
                of_layer(ui.vector, &|i| v.row(i).map(|r| r[field].trim().to_string()), &mut sites)?;
            }
            4 => {
                if !dense {
                    return Err("a class map gives a label to every pixel: for U-TAE, which learns from patches".into());
                }
                let r = self.zeit.results.iter().find(|r| r.path == ui.label_map).ok_or("choose the class map")?;
                return Ok((vec![], r.classes.clone()));
            }
            _ => {
                order = ui.pin_class_list.clone();
                for p in &self.series.pins {
                    if let Some(c) = ui.pin_classes.get(&p.id).filter(|c| !c.trim().is_empty()) {
                        sites.push(Site {
                            class: c.trim().to_string(),
                            label: format!("Pin {}", p.id),
                            pin: Some(p.id),
                            pixels: vec![(p.x, p.y)],
                            mean: false,
                        });
                    }
                }
            }
        }
        // the classes: the label set's order, then the others as they come
        let present: BTreeSet<&str> = sites.iter().map(|s| s.class.as_str()).collect();
        let mut classes: Vec<String> = order.into_iter().filter(|c| present.contains(c.as_str())).collect();
        for s in &sites {
            if !classes.contains(&s.class) {
                classes.push(s.class.clone());
            }
        }
        Ok((sites, classes))
    }

    /// The values the model reads on each date.
    fn ml_inputs(&self, info: &CubeInfo) -> Vec<InputBand> {
        let mut v = vec![];
        if self.ml.shown || info.bands_per_date <= 1 {
            v.push(InputBand::shown(info));
        }
        for &b in &self.ml.bands {
            if info.bands_per_date > 1 && b >= 1 && b as usize <= info.bands_per_date {
                v.push(InputBand::source(info, b));
            }
        }
        v
    }

    pub(crate) fn ml_menu(&mut self, ui: &mut Ui) {
        if ui
            .add(egui::Button::selectable(
                self.ml.open,
                "Classification (Random Forest, TWDTW, embeddings, SOM, networks)...",
            ))
            .clicked()
        {
            self.ml.open = !self.ml.open;
            ui.close();
        }
    }

    pub(crate) fn ml_window(&mut self, ctx: &egui::Context) {
        if !self.ml.open || self.docked(crate::app::Tab::DeepLearning) {
            return;
        }
        let mut open = true;
        // wide enough for the samples' two columns where the screen allows
        let width = (ctx.content_rect().width() * 0.5).clamp(440.0, 860.0);
        crate::panels::fitted_window("Classification", ctx, width)
            .id(egui::Id::new("ml window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.ml_ui(ui)));
        self.ml.open = open;
        if self.ml.training() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    pub(crate) fn ml_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.colored_label(crate::theme::error(), "Open a series first.");
            return;
        };
        // the samples checked as they are gathered (not a class map's: every pixel a label)
        let read = self.ml_inputs(&info);
        if let Ok((sites, classes)) = self.ml_refresh_sites(false)
            && self.ml.source != 4
            && !read.is_empty()
        {
            self.drive_sample_check(&info, &read, &sites, &classes, ui.ctx());
        }
        self.drive_method_check(&info, ui.ctx());
        let to_look = self
            .ml
            .check
            .report
            .as_deref()
            .map_or(0, |r| crate::sample_check::warnings(r).iter().filter(|w| w.level > 0).count());
        ui.horizontal(|ui| {
            let training = self.ml.training();
            for (i, name) in ["Samples", "Method", "Apply"].into_iter().enumerate() {
                let text = match i {
                    0 if to_look > 0 => RichText::new(format!("{name} ({to_look})")),
                    1 if training => RichText::new(format!("{name} (training...)")),
                    _ => RichText::new(name),
                };
                ui.selectable_value(&mut self.ml.tab, i, text.strong());
            }
        });
        ui.separator();
        use crate::methods::Kind;
        match (self.ml.tab, self.ml.kind) {
            (0, _) => self.ml_samples_tab(ui, &info),
            (1, kind) => {
                self.method_choice_ui(ui);
                match kind {
                    Kind::Learned => self.ml_model_tab(ui, &info),
                    Kind::Twdtw => self.twdtw_method_ui(ui, &info),
                    Kind::Embeddings => self.embeddings_method_ui(ui),
                    Kind::Som => self.som_method_ui(ui, &info),
                }
            }
            (_, Kind::Learned) => self.ml_rest_ui(ui, &info),
            (_, Kind::Embeddings) => self.embeddings_apply_ui(ui),
            (_, kind) => self.zeit_method_apply_ui(ui, &info, kind.tool().unwrap_or_default()),
        }
    }

    /// The samples of the source, again when asked (a choice changed) or a second old; the pins' every frame.
    fn ml_refresh_sites(&mut self, changed: bool) -> Result<(Vec<Site>, Vec<String>), String> {
        if changed || self.ml.source == 3 || self.ml.sites.as_ref().is_none_or(|c| c.0.elapsed().as_secs_f64() > 1.0) {
            let r = self.ml_sites();
            self.ml.sites = Some((Instant::now(), r));
        }
        self.ml.sites.as_ref().map_or(Err(String::new()), |c| c.1.clone())
    }

    /// Where the samples come from: the source and its choices; whether they changed.
    fn ml_source_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) -> bool {
        let mut changed = false;
        ComboBox::from_id_salt("ml source").width(ui.available_width()).selected_text(SOURCES[self.ml.source]).show_ui(
            ui,
            |ui| {
                for (i, s) in SOURCES.iter().enumerate() {
                    changed |= ui.selectable_value(&mut self.ml.source, i, *s).changed();
                }
            },
        );
        match self.ml.source {
            2 => {
                let names: Vec<(u32, String, Vec<String>)> =
                    self.vectors.iter().map(|v| (v.id, v.name.clone(), v.data.fields.clone())).collect();
                if names.is_empty() {
                    ui.weak("Open a vector file with a column of classes (points or polygons).");
                }
                let cur = names.iter().find(|n| n.0 == self.ml.vector);
                ComboBox::from_id_salt("ml vector")
                    .selected_text(cur.map_or("(choose a layer)", |n| n.1.as_str()))
                    .show_ui(ui, |ui| {
                        for n in &names {
                            changed |= ui.selectable_value(&mut self.ml.vector, n.0, &n.1).changed();
                        }
                    });
                if let Some(n) = names.iter().find(|n| n.0 == self.ml.vector) {
                    ComboBox::from_id_salt("ml field")
                        .selected_text(n.2.get(self.ml.field).map_or("(choose its class column)", |s| s.as_str()))
                        .show_ui(ui, |ui| {
                            for (k, f) in n.2.iter().enumerate() {
                                changed |= ui.selectable_value(&mut self.ml.field, k, f).changed();
                            }
                        });
                }
            }
            4 => {
                let maps: Vec<(String, String, usize)> = self
                    .zeit
                    .results
                    .iter()
                    .filter(|r| !r.classes.is_empty() && r.cube == info.id)
                    .map(|r| (r.path.clone(), r.name.clone(), r.classes.len()))
                    .collect();
                if maps.is_empty() {
                    ui.weak("Open a class map first (a classification's result: TempCNN's, SOM's, TWDTW's...).");
                }
                let cur = maps.iter().find(|m| m.0 == self.ml.label_map);
                ComboBox::from_id_salt("ml label map")
                    .selected_text(cur.map_or("(choose a class map)", |m| m.1.as_str()))
                    .show_ui(ui, |ui| {
                        for m in &maps {
                            changed |= ui
                                .selectable_value(
                                    &mut self.ml.label_map,
                                    m.0.clone(),
                                    format!("{} ({} classes)", m.1, m.2),
                                )
                                .changed();
                        }
                    });
            }
            3 => changed |= self.pin_classes_ui(ui),
            _ => {}
        }
        if self.ml.arch == UTAE {
            ui.weak("U-TAE learns from patches: every pixel of a polygon is a label, the others none.");
        } else if self.ml.source != 3 {
            ui.horizontal(|ui| {
                ui.label("A polygon gives");
                ComboBox::from_id_salt("ml polygons").selected_text(POLYGONS[self.ml.polygons]).show_ui(ui, |ui| {
                    for (i, s) in POLYGONS.iter().enumerate() {
                        changed |= ui.selectable_value(&mut self.ml.polygons, i, *s).changed();
                    }
                });
            });
            if self.ml.polygons == 1 {
                changed |= ui
                    .add(
                        egui::Slider::new(&mut self.ml.max_pixels, 1..=1000)
                            .logarithmic(true)
                            .text("pixels at most each"),
                    )
                    .changed();
            }
        }
        changed
    }

    /// The Samples tab: where they come from, then their quality as they are gathered (a summary, the classes, what
    /// to look at, each class's typical series, the samples), in two columns when the window is wide.
    fn ml_samples_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let changed = self.ml_source_ui(ui, info);
        let (sites, classes) = match self.ml_refresh_sites(changed) {
            Ok(s) => s,
            Err(e) => {
                ui.colored_label(crate::theme::error(), e);
                return;
            }
        };
        if self.ml.source == 4 {
            if let Some(r) = self.zeit.results.iter().find(|r| r.path == self.ml.label_map) {
                ui.label(format!("{} classes: {}", r.classes.len(), r.classes.join(", ")));
            }
            return;
        }
        if sites.is_empty() && (self.ml.source != 3 || self.series.pins.is_empty()) {
            ui.weak("No labelled samples yet.");
            return;
        }
        let counts = counts_of(&sites, &classes);
        crate::panels::section(ui, "Quality");
        self.samples_summary_ui(ui, &counts);
        let screen = ui.ctx().content_rect().height();
        let (table_h, sig_h) = ((screen * 0.3).clamp(140.0, 420.0), (screen * 0.17).clamp(100.0, 190.0));
        if ui.available_width() >= 640.0 {
            ui.columns(2, |c| {
                crate::panels::section(&mut c[0], "Classes");
                self.classes_table_ui(&mut c[0], &counts);
                crate::panels::section(&mut c[0], "Typical series (mean, p10-p90)");
                self.signatures_ui(&mut c[0], sig_h);
                crate::panels::section(&mut c[1], "To look at");
                self.warnings_ui(&mut c[1]);
                crate::panels::section(&mut c[1], "Samples");
                self.samples_table_ui(&mut c[1], &sites, &classes, table_h);
            });
        } else {
            crate::panels::section(ui, "Classes");
            self.classes_table_ui(ui, &counts);
            crate::panels::section(ui, "To look at");
            self.warnings_ui(ui);
            egui::CollapsingHeader::new("Typical series (mean, p10-p90)")
                .id_salt("ml signatures")
                .default_open(false)
                .show(ui, |ui| self.signatures_ui(ui, sig_h));
            crate::panels::section(ui, "Samples");
            self.samples_table_ui(ui, &sites, &classes, table_h);
        }
    }

    /// The Model tab: the samples in a line, the values read, the model and its training.
    fn ml_model_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let mut can_train = false;
        match self.ml_refresh_sites(false) {
            Ok((sites, classes)) if !sites.is_empty() => {
                let counts = counts_of(&sites, &classes);
                let total: usize = counts.iter().map(|x| x.1).sum();
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("{total} samples in {} classes", counts.len()));
                    if let Some(r) = self.ml.check.current() {
                        let look = crate::sample_check::warnings(r).iter().filter(|w| w.level > 0).count();
                        if r.accuracy.is_finite() {
                            ui.weak(format!("estimated accuracy {:.0} %", r.accuracy * 100.0));
                        }
                        if look > 0 && ui.link(format!("{look} to look at")).clicked() {
                            self.ml.tab = 0;
                        }
                    }
                });
                can_train = counts.len() >= 2 && total >= 10;
                if !can_train {
                    ui.colored_label(crate::theme::error(), "At least two classes and ten samples are needed.");
                }
            }
            Ok((_, classes)) if self.ml.source == 4 && self.ml.arch == UTAE => {
                ui.label(format!("{} classes: {}", classes.len(), classes.join(", ")));
                can_train = classes.len() >= 2;
            }
            Ok(_) => {
                if ui.link("No labelled samples yet: gather them in Samples.").clicked() {
                    self.ml.tab = 0;
                }
            }
            Err(e) => {
                ui.colored_label(crate::theme::error(), e);
            }
        }

        crate::panels::section(ui, "Values read on each date");
        if self.ml.arch == 2 {
            can_train &= self.presto_inputs_ui(ui, info);
        } else if self.ml.arch == UTAE
            && pastis_bands(info).is_some()
            && {
                ui.checkbox(&mut self.ml.pastis, "Start from the authors' PASTIS weights (Sentinel-2 crops, 10 bands)")
                .on_hover_text("U-TAE trained by its authors on PASTIS (France, 20 crop types): its weights as the starting point, its last layer made for your classes. Downloaded once (13 MB).");
                self.ml.pastis
            }
        {
            let b = pastis_bands(info).unwrap_or_default();
            ui.weak(format!("Its 10 bands: {}", b.iter().map(|x| x.name.clone()).collect::<Vec<_>>().join(", ")));
        } else if info.bands_per_date > 1 {
            ui.checkbox(&mut self.ml.shown, format!("The series shown ({})", InputBand::shown(info).name));
            ui.horizontal_wrapped(|ui| {
                for b in 1..=info.bands_per_date as i32 {
                    let name = info.band_names.get(b as usize - 1).cloned().unwrap_or_else(|| format!("band {b}"));
                    let mut on = self.ml.bands.contains(&b);
                    if ui.checkbox(&mut on, name).changed() {
                        if on {
                            self.ml.bands.insert(b);
                        } else {
                            self.ml.bands.remove(&b);
                        }
                    }
                }
            });
        } else {
            ui.label(format!("The series shown ({})", InputBand::shown(info).name));
        }
        let inputs = if self.ml.arch == UTAE && self.ml.pastis {
            pastis_bands(info).unwrap_or_default()
        } else {
            self.ml_inputs(info)
        };
        if self.ml.arch == UTAE {
            ui.horizontal(|ui| {
                ui.label("Patches of");
                ComboBox::from_id_salt("ml patch").selected_text(format!("{} px", self.ml.patch)).show_ui(ui, |ui| {
                    for p in PATCHES {
                        ui.selectable_value(&mut self.ml.patch, p, format!("{p} px"));
                    }
                });
            });
            ui.weak("The image is classified in windows of this side, overlapping by a quarter (their mean).");
            if self.series.roi_rect.is_some() {
                ui.checkbox(&mut self.ml.in_roi, "Only the labels inside the ROI");
            }
        }
        if self.ml.arch != 2 && self.ml.arch != UTAE {
            if inputs.is_empty() {
                can_train = false;
                ui.colored_label(crate::theme::error(), "Choose at least one value.");
            }
            ui.weak(format!(
                "{} dates: the model applies to series of the same {} dates (and, for TempCNN and the forest, their \
                 order).",
                info.t(),
                info.t()
            ));
        }

        crate::panels::section(ui, "Settings");
        if self.ml.cfg_arch != self.ml.arch {
            let seed = self.ml.cfg.seed;
            self.ml.cfg = TrainConfig { seed, ..TrainConfig::for_arch(&self.ml.arch()) };
            self.ml.cfg_arch = self.ml.arch;
        }
        if self.ml.arch == FOREST {
            self.forest_settings_ui(ui, info, inputs.len());
        } else {
            self.net_settings_ui(ui);
        }
        let training = self.ml.training();
        if ui
            .add_enabled(
                can_train && !training,
                egui::Button::new("Train").min_size(egui::vec2(ui.available_width(), 0.0)),
            )
            .clicked()
        {
            self.ml_train(info, inputs.clone(), ui.ctx());
        }
        self.ml_progress_ui(ui);
    }
    /// The networks' training settings (sits').
    fn net_settings_ui(&mut self, ui: &mut Ui) {
        egui::Grid::new("ml cfg").num_columns(2).show(ui, |ui| {
            let c = &mut self.ml.cfg;
            ui.label("Epochs at most");
            ui.add(egui::DragValue::new(&mut c.epochs).range(1..=2000));
            ui.end_row();
            ui.label("Batch size");
            ui.add(egui::DragValue::new(&mut c.batch_size).range(8..=4096));
            ui.end_row();
            ui.label("Learning rate");
            ui.add(egui::DragValue::new(&mut c.lr).speed(1e-5).range(1e-6..=0.1).max_decimals(6));
            ui.end_row();
            ui.label("Weight decay");
            ui.add(egui::DragValue::new(&mut c.weight_decay).speed(1e-6).range(0.0..=0.1).max_decimals(7));
            ui.end_row();
            ui.label("Validation share");
            ui.add(egui::Slider::new(&mut c.validation_split, 0.05..=0.5).fixed_decimals(2));
            ui.end_row();
            ui.label("Patience (epochs)").on_hover_text(
                "Training stops when the validation loss has not improved by min. delta for this many epochs",
            );
            ui.add(egui::DragValue::new(&mut c.patience).range(1..=500));
            ui.end_row();
            ui.label("Random seed");
            ui.add(egui::DragValue::new(&mut c.seed).range(0..=1_000_000));
            ui.end_row();
            ui.label("Min dates with a value (%)")
                .on_hover_text("Gaps are filled linearly in time; pixels with values on fewer dates are left out");
            ui.add(egui::Slider::new(&mut self.ml.min_valid, 1.0..=100.0).fixed_decimals(0));
            ui.end_row();
        });
        ui.checkbox(&mut self.ml.cfg.class_weights, "Weigh the classes (rare classes count more)");
        let gpu_name = if self.ml.gpu { "the GPU (wgpu), else the CPU" } else { "the CPU" };
        ui.checkbox(&mut self.ml.gpu, "Use the GPU").on_hover_text(format!("Trains and classifies on {gpu_name}"));
        if ui.small_button("sits' defaults").clicked() {
            self.ml.cfg = TrainConfig::for_arch(&self.ml.arch());
        }
    }

    /// The forest's settings (ranger's and scikit-learn's defaults).
    fn forest_settings_ui(&mut self, ui: &mut Ui, info: &CubeInfo, values: usize) {
        let p = info.t() * values.max(1);
        egui::Grid::new("ml forest").num_columns(2).show(ui, |ui| {
            let (h, c) = (&mut self.ml.forest, &mut self.ml.cfg);
            ui.label("Trees").on_hover_text("More trees vote more steadily; the accuracy levels off (see its curve)");
            ui.add(egui::DragValue::new(&mut h.trees).range(10..=5000).speed(5.0));
            ui.end_row();
            ui.label("Values tried at each split").on_hover_text(format!(
                "Of the {p} values of a sample ({} dates x {} values): drawn at random at each split, the best of \
                 them by the Gini impurity (mtry). 0: the square root of their number, {}",
                info.t(),
                values.max(1),
                ForestHyper { mtry: 0, ..h.clone() }.mtry_of(p)
            ));
            ui.add(egui::DragValue::new(&mut h.mtry).range(0..=p.max(1)).custom_formatter(|v, _| {
                if v == 0.0 { format!("auto ({})", ForestHyper::default().mtry_of(p)) } else { format!("{v}") }
            }));
            ui.end_row();
            ui.label("Samples in a leaf at least");
            ui.add(egui::DragValue::new(&mut h.min_leaf).range(1..=100));
            ui.end_row();
            ui.label("Depth at most").on_hover_text("0: the trees grow until their leaves are pure");
            ui.add(
                egui::DragValue::new(&mut h.max_depth)
                    .range(0..=100)
                    .custom_formatter(|v, _| if v == 0.0 { "no limit".into() } else { format!("{v}") }),
            );
            ui.end_row();
            ui.label("Validation share").on_hover_text(
                "Held out of the training, by whole places (a polygon's pixels, a pin) when a place has several \
                 samples: the accuracy on places the forest never saw. The out-of-bag accuracy is the other estimate.",
            );
            ui.add(egui::Slider::new(&mut c.validation_split, 0.05..=0.5).fixed_decimals(2));
            ui.end_row();
            ui.label("Random seed");
            ui.add(egui::DragValue::new(&mut c.seed).range(0..=1_000_000));
            ui.end_row();
            ui.label("Min dates with a value (%)")
                .on_hover_text("Gaps are filled linearly in time; pixels with values on fewer dates are left out");
            ui.add(egui::Slider::new(&mut self.ml.min_valid, 1.0..=100.0).fixed_decimals(0));
            ui.end_row();
        });
        ui.weak("Grown on the CPU, a tree a thread; the same forest whatever the threads.");
        if ui.small_button("ranger's defaults").clicked() {
            self.ml.forest = ForestHyper::default();
            self.ml.cfg.validation_split = 0.2;
        }
    }

    /// The pins' classes: the list (their colours), the class a click's pin is given, the pins without one given
    /// one at once (the samples' table lists every pin). Whether the samples changed.
    fn pin_classes_ui(&mut self, ui: &mut Ui) -> bool {
        let mut changed = false;
        if self.series.pins.is_empty() {
            ui.weak("Drop pins on places you know (a click on the map), each of the class chosen below.");
        }
        // the classes: those given, then any a pin has that the list lacks (an older project's)
        for c in self.ml.pin_classes.values() {
            if !self.ml.pin_class_list.contains(c) {
                self.ml.pin_class_list.push(c.clone());
            }
        }
        let list = self.ml.pin_class_list.clone();
        let color_of = |k: usize| {
            let [r, g, b] = class_color(k);
            egui::Color32::from_rgb(r, g, b)
        };
        let count = |c: &str| self.ml.pin_classes.values().filter(|v| v.as_str() == c).count();
        let mut remove = None;
        ui.horizontal_wrapped(|ui| {
            ui.label("Classes:");
            for (k, c) in list.iter().enumerate() {
                ui.colored_label(color_of(k), "\u{25cf}");
                ui.label(format!("{c} ({})", count(c)));
                if ui
                    .small_button("x")
                    .on_hover_text(format!("Remove the class {c} (its pins keep no class)"))
                    .clicked()
                {
                    remove = Some(c.clone());
                }
            }
            let r =
                ui.add(egui::TextEdit::singleline(&mut self.ml.new_class).hint_text("new class").desired_width(90.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let name = self.ml.new_class.trim().to_string();
            if (ui.add_enabled(!name.is_empty(), egui::Button::new("Add")).clicked() || enter) && !name.is_empty() {
                if !self.ml.pin_class_list.contains(&name) {
                    self.ml.pin_class_list.push(name.clone());
                }
                // the class just made is the one new pins get
                self.ml.drop_class = name;
                self.ml.new_class.clear();
            }
        });
        if let Some(c) = remove {
            let ids: Vec<u32> = self.ml.pin_classes.iter().filter(|e| *e.1 == c).map(|e| *e.0).collect();
            for id in ids {
                self.ml.pin_classes.remove(&id);
            }
            self.ml.pin_class_list.retain(|k| *k != c);
            if self.ml.drop_class == c {
                self.ml.drop_class.clear();
            }
            self.color_pins();
            changed = true;
        }
        let list = self.ml.pin_class_list.clone();
        let choose = |ui: &mut Ui, salt: egui::Id, cur: &str, none: &str| -> Option<String> {
            let mut pick = None;
            ComboBox::from_id_salt(salt).selected_text(if cur.is_empty() { none } else { cur }).show_ui(ui, |ui| {
                if ui.selectable_label(cur.is_empty(), none).clicked() {
                    pick = Some(String::new());
                }
                for c in &list {
                    if ui.selectable_label(cur == c, c).clicked() {
                        pick = Some(c.clone());
                    }
                }
            });
            pick
        };
        ui.horizontal(|ui| {
            ui.label("New pins:").on_hover_text("A click on the map drops a pin of this class");
            let cur = self.ml.drop_class.clone();
            if let Some(c) = choose(ui, egui::Id::new("ml drop class"), &cur, "no class") {
                self.ml.drop_class = c;
            }
            if ui.checkbox(&mut self.ml.pins_by_class, "Coloured by class").changed() {
                self.color_pins();
            }
        });
        // the pins without a class, given one at once (the table below lists them all)
        let unclassed: Vec<u32> =
            self.series.pins.iter().map(|p| p.id).filter(|id| !self.ml.pin_classes.contains_key(id)).collect();
        let mut moves: Vec<(u32, String)> = vec![];
        if !unclassed.is_empty() && !list.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} pins without a class:", unclassed.len()));
                if let Some(to) =
                    choose(ui, egui::Id::new("ml unclassed to"), "", "(give them a class)").filter(|t| !t.is_empty())
                {
                    moves.extend(unclassed.iter().map(|&id| (id, to.clone())));
                }
            });
        }
        for (id, to) in moves {
            self.set_pin_class(id, &to);
            changed = true;
        }
        changed
    }

    /// Presto's embeddings, then the model to apply.
    fn ml_rest_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        if self.ml.arch == 2 {
            crate::panels::section(ui, "Presto embeddings");
            ui.weak(
                "Presto's encoding of every pixel (128 values) for the year: an embedding layer, viewed in the \
                 Embeddings panel like TESSERA's and AlphaEarth's (principal components, similarity, change).",
            );
            if ui
                .add_enabled(
                    info.time_is_date,
                    egui::Button::new("Make the embeddings").min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .on_hover_text("Of the scope chosen below")
                .clicked()
            {
                self.ml_embed(ui.ctx());
            }
        }

        crate::panels::section(ui, "Apply");
        ui.horizontal(|ui| {
            if ui.button("Open a model...").clicked()
                && let Some(p) = rfd::FileDialog::new().add_filter("Janus model", &["janusmodel"]).pick_file()
            {
                let p = p.to_string_lossy().into_owned();
                match Model::read_header(&p) {
                    Ok(h) => self.ml.model = Some((p, h)),
                    Err(e) => self.error_popup = Some(e),
                }
            }
            if let Some((p, _)) = &self.ml.model
                && ui.button("Save as...").clicked()
                && let Some(to) = rfd::FileDialog::new()
                    .add_filter("Janus model", &["janusmodel"])
                    .set_file_name("model.janusmodel")
                    .save_file()
                && let Err(e) = std::fs::copy(p, &to)
            {
                self.error_popup = Some(format!("{}: {e}", to.display()));
            }
        });
        let Some((path, header)) = self.ml.model.clone() else {
            ui.weak("Train a model, or open one.");
            return;
        };
        ui.label(format!(
            "{}: {} classes, {} values on {} dates ({})",
            header.arch.name(),
            header.classes.len(),
            header.inputs.len(),
            header.calendar.len(),
            std::path::Path::new(&path).file_name().map_or(String::new(), |f| f.to_string_lossy().into_owned())
        ))
        .on_hover_text(&path);
        let fits = check_fits(&header, info);
        if let Err(e) = &fits {
            ui.colored_label(crate::theme::error(), format!("Not for this series: {e}"));
        }
        const SCOPES: [&str; 3] = ["Whole image", "Visible area", "ROI"];
        if self.ml.scope == 2 && self.series.roi_rect.is_none() {
            self.ml.scope = 0;
        }
        let roi = self.series.roi_rect.is_some();
        ComboBox::from_id_salt("ml scope").width(ui.available_width()).selected_text(SCOPES[self.ml.scope]).show_ui(
            ui,
            |ui| {
                for (i, s) in SCOPES.iter().enumerate() {
                    if ui.add_enabled(i != 2 || roi, egui::Button::selectable(self.ml.scope == i, *s)).clicked() {
                        self.ml.scope = i;
                    }
                }
            },
        );
        self.ml_post_ui(ui);
        let kept = header.applicability.as_ref();
        ui.add_enabled_ui(kept.is_some(), |ui| {
            ui.checkbox(&mut self.ml.aoa, "Area of applicability").on_hover_text(match kept {
                Some(a) => format!(
                    "Where the model can be trusted (Meyer & Pebesma 2021, Zeit's aoa, CAST's): each pixel's \
                     dissimilarity index to the training samples, as the network reads them (their distance to the \
                     nearest sample over the samples' mean distance), and the area of applicability, where it is not \
                     above the samples' own (to the nearest sample of another place): outside it the class is an \
                     extrapolation the validation says nothing of. This model: {}.",
                    a.describe()
                ),
                None => "This model kept no samples for its area of applicability: Random Forest, TempCNN and L-TAE \
                         models trained by this version of Janus or later keep them."
                    .into(),
            });
        });
        if ui
            .add_enabled(fits.is_ok(), egui::Button::new("Apply").min_size(egui::vec2(ui.available_width(), 0.0)))
            .clicked()
        {
            self.ml_apply(&path, &header, ui.ctx());
        }
        ui.weak(
            "Runs in the background; see Tools > Tasks. The class map, its probability and uncertainty open as layers.",
        );
    }

    /// The training's curves, then its validation (confusion matrix, accuracies).
    fn ml_progress_ui(&mut self, ui: &mut Ui) {
        let Some(run) = self.ml.run.clone() else { return };
        let r = run.lock().unwrap();
        if !r.error.is_empty() {
            ui.colored_label(crate::theme::error(), &r.error);
        } else if !r.message.is_empty() {
            ui.weak(&r.message);
        }
        if r.epochs.is_empty() {
            return;
        }
        let line = |name: &str, f: &dyn Fn(&Epoch) -> f64| {
            Line::new(name, PlotPoints::from_iter(r.epochs.iter().map(|e| [e.epoch as f64, f(e)])))
        };
        if r.forest {
            let grown = r.epochs.last().map_or(0, |e| e.epoch);
            ui.label(format!("{}: {grown} trees of {}", r.arch, r.epochs_asked));
            Plot::new("ml forest accuracy")
                .height(120.0)
                .legend(Legend::default())
                .allow_scroll(false)
                .include_y(1.0)
                .x_axis_label("trees")
                .show(ui, |p| {
                    p.line(line("out-of-bag accuracy", &|e| e.train_acc));
                    p.line(line("validation accuracy", &|e| e.val_acc));
                });
        } else {
            ui.label(format!("{}: epoch {} of at most {}", r.arch, r.epochs.len(), r.epochs_asked));
            Plot::new("ml loss").height(130.0).legend(Legend::default()).allow_scroll(false).show(ui, |p| {
                p.line(line("training loss", &|e| e.train_loss));
                p.line(line("validation loss", &|e| e.val_loss));
            });
            Plot::new("ml accuracy").height(110.0).legend(Legend::default()).allow_scroll(false).include_y(1.0).show(
                ui,
                |p| {
                    p.line(line("training accuracy", &|e| e.train_acc));
                    p.line(line("validation accuracy", &|e| e.val_acc));
                },
            );
        }
        let Some(h) = &r.header else { return };
        let v = &h.training["validation"];
        let confusion: Vec<Vec<u64>> = v["confusion"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|row| {
                        row.as_array().map(|x| x.iter().map(|c| c.as_u64().unwrap_or(0)).collect()).unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        if confusion.is_empty() {
            return;
        }
        if r.forest {
            let oob = h.training["oob"]["overall_accuracy"].as_f64().unwrap_or(f64::NAN);
            let by =
                if h.training["samples"]["split"] == "places" { "whole places held out" } else { "samples held out" };
            ui.label(format!(
                "Validation: overall accuracy {:.1} % on {} samples ({by}); out of bag {:.1} %",
                v["overall_accuracy"].as_f64().unwrap_or(f64::NAN) * 100.0,
                h.training["samples"]["validation"],
                oob * 100.0
            ));
        } else {
            ui.label(format!(
                "Validation: overall accuracy {:.1} % on {} samples (the model of epoch {})",
                v["overall_accuracy"].as_f64().unwrap_or(f64::NAN) * 100.0,
                h.training["samples"]["validation"],
                h.training["best_epoch"]
            ));
        }
        let pct =
            |x: &Value| x.as_f64().filter(|v| v.is_finite()).map_or("-".to_string(), |v| format!("{:.0}%", v * 100.0));
        egui::Grid::new("ml confusion").striped(true).show(ui, |ui| {
            ui.weak("reference \\ model");
            for c in &h.classes {
                ui.weak(&c.name);
            }
            ui.weak("producer's");
            ui.end_row();
            for (i, row) in confusion.iter().enumerate() {
                ui.label(&h.classes[i].name);
                for (j, n) in row.iter().enumerate() {
                    if i == j {
                        ui.strong(n.to_string())
                    } else {
                        ui.label(n.to_string())
                    };
                }
                ui.label(pct(&v["producers_accuracy"][i]));
                ui.end_row();
            }
            ui.weak("user's");
            for j in 0..h.classes.len() {
                ui.label(pct(&v["users_accuracy"][j]));
            }
            ui.end_row();
        });
        if r.forest {
            importance_ui(ui, h);
        }
    }

    /// Starts a training on the samples (Tools > Tasks follows it; the window shows its curves).
    fn ml_train(&mut self, info: &Arc<CubeInfo>, inputs: Vec<InputBand>, ctx: &egui::Context) {
        let (sites, classes) = match self.ml_sites() {
            Ok(s) => s,
            Err(e) => {
                self.error_popup = Some(e);
                return;
            }
        };
        let mut arch = self.ml.arch();
        if let Arch::Presto(h) = &mut arch {
            h.inputs.year = self.ml.presto_year(info);
        }
        // U-TAE's labels from a class map: its file and where it lies on the series
        let label_map = (self.ml.arch == UTAE && self.ml.source == 4)
            .then(|| {
                self.zeit
                    .results
                    .iter()
                    .find(|r| r.path == self.ml.label_map)
                    .map(|r| (r.path.clone(), r.x0, r.y0, r.w, r.h))
            })
            .flatten();
        let roi = (self.ml.arch == UTAE && self.ml.in_roi).then_some(self.series.roi_rect).flatten();
        let cfg = self.ml.cfg.clone();
        let (min_valid, gpu) = (self.ml.min_valid, self.ml.gpu);
        let layer_inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(info, &layer_inputs);
        let dir = self.results_dir().join("models");
        let _ = std::fs::create_dir_all(&dir);
        let stem = format!("{name}_{}_{}", arch.name().to_lowercase().replace('-', ""), crate::zeit::timestamp());
        let model_path = dir.join(format!("{stem}.janusmodel")).to_string_lossy().into_owned();
        let log = JobLog { path: dir.join(format!("{stem}.log")), t0: Instant::now() };
        let forest = matches!(arch, Arch::RandomForest(_));
        let run = Arc::new(Mutex::new(TrainRun {
            arch: arch.name().into(),
            forest,
            epochs_asked: if let Arch::RandomForest(h) = &arch { h.trees } else { cfg.epochs },
            message: "reading the samples' series".into(),
            ..Default::default()
        }));
        self.ml.run = Some(run.clone());
        self.ml.tab = 1;
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "reading the samples' series".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let n_sites = sites.len();
        let title = format!("{} training - {n_sites} places, {} classes", arch.name(), classes.len());
        log.write(&format!(
            "=== {} {title}\nseries: {}\ninputs: {}\nconfig: {}\nmodel: {model_path}\n",
            crate::zeit::timestamp_text(),
            info.description,
            inputs.iter().map(|b| b.name.clone()).collect::<Vec<_>>().join(", "),
            match &arch {
                Arch::RandomForest(h) => serde_json::to_string(&json!({
                    "forest": h, "validation_split": cfg.validation_split, "seed": cfg.seed
                }))
                .unwrap_or_default(),
                _ => serde_json::to_string(&cfg).unwrap_or_default(),
            }
        ));
        self.zeit.jobs.push(Job {
            title,
            tool: arch.name().into(),
            cube: info.id,
            output_dir: dir.to_string_lossy().into_owned(),
            log_path: log.path.to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel: cancel.clone(),
            loaded: false,
        });
        self.zeit.show_tasks = true;
        let (info, c) = (info.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("ml-train".into())
            .spawn(move || {
                let t0 = Instant::now();
                let set = |f: f64, m: &str| {
                    let mut s = status.lock().unwrap();
                    s.progress = f;
                    s.message = m.to_string();
                    run.lock().unwrap().message = m.to_string();
                    c.request_repaint();
                };
                let r = guarded(|| {
                    if let Arch::Utae(h) = &arch {
                        utae_job(
                            &info,
                            &inputs,
                            &sites,
                            label_map.as_ref(),
                            roi,
                            &classes,
                            h,
                            &cfg,
                            gpu,
                            &model_path,
                            &log,
                            &run,
                            &set,
                            &cancel,
                        )
                    } else {
                        train_job(
                            &info,
                            &inputs,
                            &sites,
                            &classes,
                            &arch,
                            &cfg,
                            min_valid,
                            gpu,
                            &model_path,
                            &log,
                            &run,
                            &set,
                            &cancel,
                        )
                    }
                });
                let mut s = status.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                let mut rr = run.lock().unwrap();
                rr.done = true;
                match r {
                    Ok(header) => {
                        s.state = JobState::Done;
                        s.progress = 1.0;
                        rr.model_path = model_path.clone();
                        rr.header = Some(header);
                        log.line("done");
                    }
                    Err(e) if e == "cancelled" => {
                        s.state = JobState::Cancelled;
                        rr.message = "cancelled".into();
                        log.line("cancelled");
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        s.state = JobState::Failed;
                        s.error = e.clone();
                        rr.error = e;
                    }
                }
                c.request_repaint();
            })
            .ok();
    }

    /// Trains on the chosen samples and values now (the Train button's; the UI test's).
    pub(crate) fn ml_train_now(&mut self, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let inputs = self.ml_inputs(&info);
        self.ml_train(&info, inputs, ctx);
    }

    /// Applies the model to the chosen scope now (the Apply button's; the UI test's).
    pub(crate) fn ml_apply_now(&mut self, ctx: &egui::Context) {
        if let Some((path, header)) = self.ml.model.clone() {
            self.ml_apply(&path, &header, ctx);
        }
    }

    /// A finished training's model becomes the one to apply.
    pub(crate) fn ml_embed_now(&mut self, ctx: &egui::Context) {
        self.ml_embed(ctx);
    }

    pub(crate) fn pump_ml(&mut self) {
        let Some(run) = self.ml.run.clone() else { return };
        let r = run.lock().unwrap();
        if r.done
            && let Some(h) = &r.header
            && self.ml.model.as_ref().is_none_or(|m| m.0 != r.model_path)
        {
            self.ml.model = Some((r.model_path.clone(), h.clone()));
        }
    }

    /// The class map's post-processing after the model: sits' Bayesian smoothing of the probabilities and a minimum
    /// mapping unit (Zeit's smooth_probs and mmu_sieve).
    fn ml_post_ui(&mut self, ui: &mut Ui) {
        let post = &mut self.ml.post;
        ui.checkbox(&mut post.smooth, "Smooth the probabilities (Bayesian)").on_hover_text(
            "sits' Bayesian smoothing (sits_smooth, Zeit's smooth_probs): each class's probability is pulled towards \
             its neighbours', the more where they agree; a confident pixel keeps its class, the noise among uncertain \
             ones goes. The class is then the most probable after smoothing.",
        );
        if post.smooth {
            ui.horizontal(|ui| {
                ui.label("Window");
                let mut win = post.window;
                if ui.add(egui::DragValue::new(&mut win).range(3..=33).speed(0.1).suffix(" px")).changed() {
                    // odd: on from the even value it passed, the way it went
                    post.window = if !win.is_multiple_of(2) {
                        win
                    } else if win > post.window {
                        win + 1
                    } else {
                        win - 1
                    };
                }
                ui.label("Smoothness");
                ui.add(egui::DragValue::new(&mut post.smoothness).range(1.0..=1000.0).speed(0.5))
                    .on_hover_text("The prior's variance: higher smooths more (sits' default 20).");
                ui.label("Neighbours");
                ui.add(egui::DragValue::new(&mut post.neigh_fraction).range(0.05..=1.0).speed(0.01)).on_hover_text(
                    "The fraction of the most probable neighbours whose mean is the prior (sits' default 0.5).",
                );
            });
        }
        ui.horizontal(|ui| {
            ui.label("Minimum mapping unit");
            ui.add(egui::DragValue::new(&mut post.min_size).range(0..=100000).speed(0.2).suffix(" px")).on_hover_text(
                "Patches of fewer pixels (8-connected) are merged into the neighbouring class with which they share \
                 the longest border (Zeit's mmu_sieve); 0: off. 11 Landsat pixels are about 1 ha.",
            );
        });
    }

    /// Applies the model to the scope (a task; its maps open as result layers).
    fn ml_apply(&mut self, path: &str, header: &Header, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let (window, scope_name) = self.run_window(self.ml.scope);
        let layer_inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(&info, &layer_inputs);
        let arch = header.arch.name();
        let out = self.results_dir().join(format!(
            "{name}_{}_{}",
            arch.to_lowercase().replace('-', ""),
            crate::zeit::timestamp()
        ));
        if let Err(e) = std::fs::create_dir_all(&out) {
            self.error_popup = Some(format!("could not create {}: {e}", out.display()));
            return;
        }
        let title =
            format!("{arch} classification - {scope_name} ({} x {} px)", window[2] - window[0], window[3] - window[1]);
        let log = JobLog { path: out.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} {title}\nmodel: {path}\nwindow: {window:?}\noutput: {}\n{}",
            crate::zeit::timestamp_text(),
            out.display(),
            if self.ml.post.on() { format!("post-processing: {}\n", self.ml.post.describe()) } else { String::new() }
        ));
        if let (true, Some(a)) = (self.ml.aoa, &header.applicability) {
            log.line(&format!("area of applicability: {}", a.describe()));
        }
        {
            use crate::provenance::{Provenance, SeriesRecord};
            let mut p = Provenance::start("model", arch, &format!("{arch} classification"));
            let lname = self.layers.get(self.active).map_or(name.clone(), |l| l.name.clone());
            p.series = Some(SeriesRecord::of(&info, &lname, &layer_inputs));
            (p.scope, p.scope_index, p.window) = (scope_name.to_string(), self.ml.scope, Some(window));
            let post = &self.ml.post;
            for (k, v) in [
                ("model", serde_json::json!(path)),
                ("classes", serde_json::json!(header.classes.iter().map(|c| c.name.clone()).collect::<Vec<_>>())),
                ("smooth", post.smooth.into()),
                ("smooth_window", post.window.into()),
                ("smoothness", post.smoothness.into()),
                ("neigh_fraction", post.neigh_fraction.into()),
                ("min_size", post.min_size.into()),
                ("aoa", (self.ml.aoa && header.applicability.is_some()).into()),
            ] {
                p.extra.insert(k.into(), v);
            }
            p.python_note = "a model applied: its file is the record of the run (no Python)".into();
            self.begin_provenance(&out, p);
        }
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "loading the model".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        self.zeit.jobs.push(Job {
            title,
            tool: arch.into(),
            cube: info.id,
            output_dir: out.to_string_lossy().into_owned(),
            log_path: log.path.to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel: cancel.clone(),
            loaded: false,
        });
        self.zeit.show_tasks = true;
        let (path, gpu, c) = (path.to_string(), self.ml.gpu, ctx.clone());
        let spec = janus_ml::apply::ApplySpec {
            window,
            output_dir: out.to_string_lossy().into_owned(),
            stem: arch.to_lowercase().replace('-', ""),
            batch: 0,
            post: self.ml.post.clone(),
            aoa: self.ml.aoa && header.applicability.is_some(),
        };
        std::thread::Builder::new()
            .name("ml-apply".into())
            .spawn(move || {
                let t0 = Instant::now();
                let dev = shared_device(gpu);
                log.line(&format!("device: {}", dev.1));
                let progress = |f: f64, m: &str| {
                    let mut s = status.lock().unwrap();
                    s.progress = f;
                    s.message = m.to_string();
                    c.request_repaint();
                };
                let r = guarded(|| {
                    Model::load(&path, &dev.0)
                        .and_then(|m| janus_ml::apply::apply(&spec, &info, &m, &progress, &cancel))
                });
                let mut s = status.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(a) => {
                        log.line(&format!("{} pixels classified", a.classified));
                        for o in &a.outputs {
                            log.line(&format!(
                                "output: {} -> {}",
                                o["name"].as_str().unwrap_or(""),
                                o["path"].as_str().unwrap_or("")
                            ));
                        }
                        log.line("done");
                        s.state = JobState::Done;
                        s.progress = 1.0;
                        s.result = Some(janus_zeit::job::JobResult { outputs: a.outputs, window: a.window });
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        s.state = JobState::Failed;
                        s.error = e;
                    }
                }
                c.request_repaint();
            })
            .ok();
    }
}

/// Whether a model applies to a series (its dates and values), and why not.
pub fn check_fits(h: &Header, info: &CubeInfo) -> Result<(), String> {
    match &h.arch {
        // Presto: the year's dates; U-TAE: any dates (their positions are the series' own)
        Arch::Presto(p) => {
            p.inputs.steps(info)?;
        }
        Arch::Utae(_) => {}
        _ if info.t() != h.calendar.len() => {
            return Err(format!("the model was trained on {} dates; this series has {}", h.calendar.len(), info.t()));
        }
        _ => {}
    }
    for b in &h.inputs {
        b.find_in(info)?;
    }
    Ok(())
}

/// The samples' series (each pixel a sample, or each place's mean), gaps filled and scaled: the training samples
/// and their classes, the scaling, the count of each class and each sample's place.
pub fn sample_series(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    sites: &[Site],
    classes: &[String],
    min_valid: f64,
) -> (janus_ml::Series, Vec<u32>, Norm, Vec<usize>, Vec<usize>) {
    let pixels: Vec<(usize, usize)> = sites.iter().flat_map(|s| s.pixels.iter().copied()).collect();
    let raw = janus_ml::data::read_points(info, inputs, &pixels);
    let (mut s, y, place) = assemble(&raw, sites, classes, &Calendar::of(info).years, min_valid);
    let mut per_class = vec![0usize; classes.len()];
    for &k in &y {
        per_class[k as usize] += 1;
    }
    let norm = Norm::fit(&s);
    norm.apply(&mut s);
    (s, y, norm, per_class, place)
}

/// The samples from the sites' pixels' raw series (`raw`, the sites' pixels in order): each pixel a sample, or each
/// place's mean (of each date's values over the pixels with one); gaps filled linearly in time, the samples with
/// values on fewer than `min_valid` % of the dates left out. The series (not scaled), the classes and each sample's
/// place (its site).
pub fn assemble(
    raw: &janus_ml::Series,
    sites: &[Site],
    classes: &[String],
    years: &[f64],
    min_valid: f64,
) -> (janus_ml::Series, Vec<u32>, Vec<usize>) {
    let (t, b) = (raw.t, raw.b);
    let mut values = vec![];
    let mut y = vec![];
    let mut place = vec![];
    let mut at = 0;
    for (si, s) in sites.iter().enumerate() {
        let k = classes.iter().position(|c| c == &s.class).unwrap_or(0) as u32;
        let n = s.pixels.len();
        if s.mean {
            let mut m = vec![f32::NAN; t * b];
            for (j, v) in m.iter_mut().enumerate() {
                let (mut sum, mut cnt) = (0f64, 0);
                for i in at..at + n {
                    let x = raw.values[i * t * b + j];
                    if x.is_finite() {
                        sum += x as f64;
                        cnt += 1;
                    }
                }
                if cnt > 0 {
                    *v = (sum / cnt as f64) as f32;
                }
            }
            values.extend(m);
            y.push(k);
            place.push(si);
        } else {
            values.extend_from_slice(&raw.values[at * t * b..(at + n) * t * b]);
            y.extend(std::iter::repeat_n(k, n));
            place.extend(std::iter::repeat_n(si, n));
        }
        at += n;
    }
    let mut s = janus_ml::Series { n: y.len(), t, b, values };
    let keep = janus_ml::data::fill_gaps(&mut s, years, min_valid);
    let idx: Vec<usize> = (0..s.n).filter(|&i| keep[i]).collect();
    let s = s.select(&idx);
    let y: Vec<u32> = idx.iter().map(|&i| y[i]).collect();
    let place: Vec<usize> = idx.iter().map(|&i| place[i]).collect();
    (s, y, place)
}

#[allow(clippy::too_many_arguments)]
fn train_job(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    sites: &[Site],
    classes: &[String],
    arch: &Arch,
    cfg: &TrainConfig,
    min_valid: f64,
    gpu: bool,
    model_path: &str,
    log: &JobLog,
    run: &Arc<Mutex<TrainRun>>,
    set: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Header, String> {
    let mut arch = arch.clone();
    let mut inputs = inputs.to_vec();
    let (x, y, norm, per_class, places) = if let Arch::Presto(h) = &mut arch {
        set(0.0, "Presto's weights");
        let weights = janus_ml::presto::weights(&crate::settings::app_data_dir().join("weights"), &|_| {}, cancel)?;
        let (x, y, per_class, pi) = presto_samples(info, sites, classes, h.inputs.year)?;
        log.line(&format!(
            "Presto's inputs: {}; reflectance x {}{}",
            pi.read_bands().iter().map(|b| b.name.clone()).collect::<Vec<_>>().join(", "),
            pi.reflectance_scale,
            pi.year.map_or(", the series' dates".to_string(), |y| format!(", monthly composites of {y}"))
        ));
        inputs = pi.read_bands();
        *h = PrestoHyper { inputs: pi, hidden: vec![], weights: Some(weights) };
        (x, y, Norm::default(), per_class, None)
    } else {
        let (x, y, norm, per_class, places) = sample_series(info, &inputs, sites, classes, min_valid);
        (x, y, norm, per_class, Some(places))
    };
    let arch = &arch;
    let left = sites.iter().map(|s| if s.mean { 1 } else { s.pixels.len() }).sum::<usize>() - x.n;
    log.line(&format!(
        "{} samples ({left} left out: too few dates with a value): {}",
        x.n,
        classes.iter().zip(&per_class).map(|(c, n)| format!("{c} {n}")).collect::<Vec<_>>().join(", ")
    ));
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if left > 0 && (x.n < 10 || per_class.iter().filter(|&&n| n > 0).count() < 2) {
        return Err(format!(
            "{left} of {} samples left out: they have a value on fewer than {min_valid:.0} % of the dates (clouds,              the quality band); lower 'Min dates with a value (%)'",
            left + x.n
        ));
    }
    let calendar = Calendar::of(info);
    if let Arch::RandomForest(h) = arch {
        return forest_job(
            info,
            inputs,
            classes,
            h,
            cfg,
            min_valid,
            (x, y, norm, per_class, places.unwrap_or_default()),
            calendar,
            model_path,
            log,
            run,
            set,
            cancel,
        );
    }
    let dev = shared_device(gpu);
    log.line(&format!("device: {}", dev.1));
    set(0.02, &format!("training on {} samples ({})", x.n, dev.1));
    let epochs = cfg.epochs;
    let mut on_epoch = |e: &Epoch| {
        log.line(&format!(
            "epoch {:3}: training loss {:.4} accuracy {:.3}, validation loss {:.4} accuracy {:.3} ({:.1} s)",
            e.epoch, e.train_loss, e.train_acc, e.val_loss, e.val_acc, e.seconds
        ));
        run.lock().unwrap().epochs.push(e.clone());
        set(
            0.02 + 0.97 * e.epoch as f64 / epochs as f64,
            &format!("epoch {}: validation accuracy {:.1} %", e.epoch, e.val_acc * 100.0),
        );
    };
    let trained = janus_ml::train::train(&x, &y, classes.len(), arch, &calendar, cfg, &dev.0, &mut on_epoch, cancel)?;
    let (oa, _, _) = trained.accuracies();
    log.line(&format!(
        "best epoch {} of {}{}; validation overall accuracy {:.4}",
        trained.best_epoch,
        trained.history.len(),
        if trained.stopped_early { " (stopped early)" } else { "" },
        oa
    ));
    let mut training = trained.summary(cfg, &per_class);
    training["device"] = json!(dev.1);
    // the samples kept for the area of applicability (the pixel networks: Presto's inputs are its encoder's)
    let applicability = match places {
        Some(places) => match janus_ml::aoa::Applicability::fit(&x, Some(&places), &y, -1) {
            Ok(a) => {
                log.line(&format!("area of applicability: {}", a.describe()));
                Some(a)
            }
            Err(e) => {
                log.line(&format!("no area of applicability: {e}"));
                None
            }
        },
        None => None,
    };
    let header = Header {
        format: 1,
        janus: env!("CARGO_PKG_VERSION").into(),
        arch: arch.clone(),
        inputs,
        calendar,
        norm,
        standard: None,
        classes: classes.iter().enumerate().map(|(k, c)| Class { name: c.clone(), color: class_color(k) }).collect(),
        min_valid,
        series: info.description.clone(),
        training,
        applicability,
        created: crate::zeit::timestamp_text(),
    };
    let model = Model { header: header.clone(), net: trained.net, device: dev.0.clone() };
    model.save(model_path)?;
    log.line(&format!("model: {model_path}"));
    Ok(header)
}

/// The samples as the forest reads them: the series, their classes, the scaling, the count of each class and each
/// sample's place.
pub(crate) type Samples = (janus_ml::Series, Vec<u32>, Norm, Vec<usize>, Vec<usize>);

/// Grows a Random Forest on the samples (its validation by whole places), keeps it in the model file with its area
/// of applicability (the values weighed by their importance).
#[allow(clippy::too_many_arguments)]
pub(crate) fn forest_job(
    info: &Arc<CubeInfo>,
    inputs: Vec<InputBand>,
    classes: &[String],
    h: &ForestHyper,
    cfg: &TrainConfig,
    min_valid: f64,
    (x, y, norm, per_class, places): Samples,
    calendar: Calendar,
    model_path: &str,
    log: &JobLog,
    run: &Arc<Mutex<TrainRun>>,
    set: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Header, String> {
    let k = classes.len();
    let (tr, va, how) = janus_ml::forest::split_by_place(&y, Some(&places), k, cfg.validation_split, cfg.seed);
    log.line(&format!(
        "validation: {} samples ({}), training: {}; {} values a sample, {} tried at each split",
        va.len(),
        if how == "places" { "whole places" } else { "samples" },
        tr.len(),
        x.t * x.b,
        h.mtry_of(x.t * x.b)
    ));
    set(0.02, &format!("growing {} trees on {} samples", h.trees, tr.len()));
    let trees = h.trees.max(1);
    let mut on_trees = |e: &Epoch| {
        log.line(&format!(
            "{:4} trees: out-of-bag accuracy {:.3}, validation accuracy {:.3} ({:.1} s)",
            e.epoch, e.train_acc, e.val_acc, e.seconds
        ));
        run.lock().unwrap().epochs.push(e.clone());
        set(
            0.02 + 0.97 * e.epoch as f64 / trees as f64,
            &format!("{} trees: out-of-bag accuracy {:.1} %", e.epoch, e.train_acc * 100.0),
        );
    };
    let mut grown = janus_ml::forest::train(&x, &y, k, h, &tr, &va, cfg.seed, &mut on_trees, cancel)?;
    grown.split = how.into();
    let (oa, _, _) = grown.accuracies();
    log.line(&format!("validation overall accuracy {oa:.4}"));
    let mut training = grown.summary(h, cfg.validation_split, cfg.seed, &per_class);
    training["device"] = json!("CPU");
    let applicability =
        match janus_ml::aoa::Applicability::fit_weighted(&x, Some(&places), &y, Some(&grown.importance), -1) {
            Ok(a) => {
                log.line(&format!("area of applicability: {}", a.describe()));
                Some(a)
            }
            Err(e) => {
                log.line(&format!("no area of applicability: {e}"));
                None
            }
        };
    let header = Header {
        format: 1,
        janus: env!("CARGO_PKG_VERSION").into(),
        arch: Arch::RandomForest(h.clone()),
        inputs,
        calendar,
        norm,
        standard: None,
        classes: classes.iter().enumerate().map(|(k, c)| Class { name: c.clone(), color: class_color(k) }).collect(),
        min_valid,
        series: info.description.clone(),
        training,
        applicability,
        created: crate::zeit::timestamp_text(),
    };
    let model =
        Model { header: header.clone(), net: Net::forest(grown.forest), device: shared_device(false).0.clone() };
    model.save(model_path)?;
    log.line(&format!("model: {model_path}"));
    Ok(header)
}

/// A forest's importance of each date (its values' summed), and of each value read when there are several.
fn importance_ui(ui: &mut Ui, h: &Header) {
    use egui_plot::{Bar, BarChart};
    let Some(imp) = h.training["importance"].as_array() else { return };
    let imp: Vec<f64> = imp.iter().map(|v| v.as_f64().unwrap_or(0.0)).collect();
    let (t, b) = (h.calendar.len(), h.inputs.len().max(1));
    if imp.len() != t * b || t == 0 {
        return;
    }
    let by_date: Vec<f64> = (0..t).map(|i| imp[i * b..(i + 1) * b].iter().sum()).collect();
    let labels = h.calendar.labels.clone();
    ui.label("Importance of each date (the mean decrease of the impurity, summing to 1)").on_hover_text(
        "How much the forest's splits on each date's values separate the classes (scikit-learn's \
         feature_importances_): the dates that tell the classes apart. Also the weights of the area of applicability.",
    );
    let accent = ui.visuals().selection.bg_fill;
    Plot::new("ml forest importance")
        .height(110.0)
        .allow_scroll(false)
        .include_y(0.0)
        .x_axis_formatter(move |m, _| {
            let i = m.value.round();
            if (m.value - i).abs() < 1e-6 && i >= 0.0 {
                labels.get(i as usize).cloned().unwrap_or_default()
            } else {
                String::new()
            }
        })
        .show(ui, |p| {
            let bars: Vec<Bar> = by_date
                .iter()
                .enumerate()
                .map(|(i, &v)| Bar::new(i as f64, v).width(0.8).name(&h.calendar.labels[i]))
                .collect();
            p.bar_chart(BarChart::new("importance", bars).color(accent));
        });
    if b > 1 {
        let by_value: Vec<String> = (0..b)
            .map(|j| {
                let s: f64 = (0..t).map(|i| imp[i * b + j]).sum();
                format!("{} {:.0} %", h.inputs[j].name, s * 100.0)
            })
            .collect();
        ui.weak(format!("By value: {}", by_value.join(", ")));
    }
}

/// Each sample's raw series (each pixel, or a place's mean), its class and a pixel of it (for its place).
fn raw_samples(
    info: &Arc<CubeInfo>,
    read: &[InputBand],
    sites: &[Site],
    classes: &[String],
) -> (janus_ml::Series, Vec<u32>, Vec<(f64, f64)>) {
    let pixels: Vec<(usize, usize)> = sites.iter().flat_map(|s| s.pixels.iter().copied()).collect();
    let raw = janus_ml::data::read_points(info, read, &pixels);
    let (t, b) = (raw.t, raw.b);
    let (mut values, mut y, mut at_px) = (vec![], vec![], vec![]);
    let mut at = 0;
    for s in sites {
        let k = classes.iter().position(|c| c == &s.class).unwrap_or(0) as u32;
        let n = s.pixels.len();
        if s.mean {
            // the mean of each date's values over the pixels with one
            for j in 0..t * b {
                let (mut sum, mut cnt) = (0f64, 0);
                for i in at..at + n {
                    let x = raw.values[i * t * b + j];
                    if x.is_finite() {
                        sum += x as f64;
                        cnt += 1;
                    }
                }
                values.push(if cnt > 0 { (sum / cnt as f64) as f32 } else { f32::NAN });
            }
            y.push(k);
            at_px.push((s.pixels[0].0 as f64, s.pixels[0].1 as f64));
        } else {
            values.extend_from_slice(&raw.values[at * t * b..(at + n) * t * b]);
            y.extend(std::iter::repeat_n(k, n));
            at_px.extend(s.pixels.iter().map(|&(x, y)| (x as f64, y as f64)));
        }
        at += n;
    }
    (janus_ml::Series { n: y.len(), t, b, values }, y, at_px)
}

/// Each band's 2 % and 98 % quantiles in raw values (1-based bands of `read`), by band number.
fn band_ranges(info: &CubeInfo, read: &[InputBand], raw: &janus_ml::Series) -> Vec<(f32, f32)> {
    let mut ranges = vec![(0f32, 1f32); info.bands_per_date.max(1)];
    for (k, b) in read.iter().enumerate() {
        let mut v: Vec<f32> =
            raw.values.iter().skip(k).step_by(raw.b.max(1)).copied().filter(|x| x.is_finite()).collect();
        if v.is_empty() || b.band < 1 {
            continue;
        }
        v.sort_by(f32::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
        ranges[b.band as usize - 1] = (q(0.02), q(0.98));
    }
    ranges
}

/// Presto's samples: its inputs found in the series (the units from the samples' values), each sample's input.
fn presto_samples(
    info: &Arc<CubeInfo>,
    sites: &[Site],
    classes: &[String],
    year: Option<i32>,
) -> Result<(janus_ml::Series, Vec<u32>, Vec<usize>, PrestoInputs), String> {
    let first = PrestoInputs::of(info, &vec![(0.0, 1.0); info.bands_per_date.max(1)], year)?;
    let read = first.read_bands();
    let (raw, y, pixels) = raw_samples(info, &read, sites, classes);
    let pi = PrestoInputs::of(info, &band_ranges(info, &read, &raw), year)?;
    let (x, keep) = janus_ml::presto::classifier_input(info, &pi, &raw, &pixels)?;
    let idx: Vec<usize> = (0..x.n).filter(|&i| keep[i]).collect();
    let x = x.select(&idx);
    let y: Vec<u32> = idx.iter().map(|&i| y[i]).collect();
    let mut per_class = vec![0usize; classes.len()];
    for &k in &y {
        per_class[k as usize] += 1;
    }
    Ok((x, y, per_class, pi))
}

impl State {
    /// Presto's inputs found in the series, and the dates it is given; whether it can be trained.
    fn presto_inputs_ui(&mut self, ui: &mut Ui, info: &CubeInfo) -> bool {
        let roles = janus_cube::roles::guess_band_roles(info);
        let fallback = |input: &str| {
            let role = match input {
                "B2" => "blue",
                "B3" => "green",
                "B4" => "red",
                "B8" => "nir",
                "B11" => "swir1",
                "B12" => "swir2",
                _ => "",
            };
            roles.get(role).map(|&b| b as usize - 1)
        };
        let found = janus_ml::presto::match_names(&info.band_names, &fallback);
        let matched: Vec<String> = found
            .iter()
            .enumerate()
            .filter_map(|(k, b)| {
                b.map(|b| {
                    format!("{} <- {}", janus_ml::presto::INPUTS[k], info.band_names.get(b).map_or("?", |s| s.as_str()))
                })
            })
            .collect();
        let mut ok = true;
        if matched.is_empty() {
            ui.colored_label(
                crate::theme::error(),
                "None of the bands is one of Presto's inputs: name them as Sentinel-2 (blue, green, red, nir, B8A, \
                 swir1...), Sentinel-1 (VV, VH), ERA5 or SRTM bands.",
            );
            ok = false;
        } else {
            ui.label(format!("Presto's inputs: {}", matched.join(", ")));
            ui.weak("The units are found from the values (reflectance 0-1 or x 10000; dB or linear backscatter).");
        }
        if !info.time_is_date {
            ui.colored_label(crate::theme::error(), "Presto needs the series' dates (it encodes their months).");
            return false;
        }
        ui.checkbox(
            &mut self.ml.presto_dates,
            format!("The series' own dates (at most {})", janus_ml::presto::MAX_DATES),
        );
        if self.ml.presto_dates {
            if info.t() > janus_ml::presto::MAX_DATES {
                ui.colored_label(
                    crate::theme::error(),
                    format!("This series has {} dates: use the monthly composites of a year.", info.t()),
                );
                ok = false;
            }
        } else {
            let (first, last) = (info.decimal_year(0).floor() as i32, info.decimal_year(info.t() - 1).floor() as i32);
            let mut y = self.ml.presto_year(info).unwrap_or(last);
            ui.horizontal(|ui| {
                ui.label("Monthly composites of");
                if ui.add(egui::DragValue::new(&mut y).range(first..=last)).changed() {
                    self.ml.presto_year = y;
                }
            });
            ui.weak("Each month: the median of its observations; months without one are missing (Presto takes that).");
        }
        ok
    }

    /// Presto's embeddings of the scope: a task, its file opening as an embedding layer.
    fn ml_embed(&mut self, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let year = self.ml.presto_year(&info);
        let (window, scope_name) = self.run_window(self.ml.scope);
        let layer_inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(&info, &layer_inputs);
        let out = self.results_dir().join(format!("{name}_presto_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&out) {
            self.error_popup = Some(format!("could not create {}: {e}", out.display()));
            return;
        }
        let file = out.join(format!("presto_{}.tif", year.map_or("dates".to_string(), |y| y.to_string())));
        let title =
            format!("Presto embeddings - {scope_name} ({} x {} px)", window[2] - window[0], window[3] - window[1]);
        let log = JobLog { path: out.join("job.log"), t0: Instant::now() };
        log.write(&format!("=== {} {title}\noutput: {}\n", crate::zeit::timestamp_text(), file.display()));
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "Presto's weights".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        self.zeit.jobs.push(Job {
            title,
            tool: "Presto".into(),
            cube: info.id,
            output_dir: out.to_string_lossy().into_owned(),
            log_path: log.path.to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel: cancel.clone(),
            loaded: false,
        });
        self.zeit.show_tasks = true;
        let (gpu, c) = (self.ml.gpu, ctx.clone());
        std::thread::Builder::new()
            .name("presto".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    let mut s = status.lock().unwrap();
                    s.progress = f;
                    s.message = m.to_string();
                    c.request_repaint();
                };
                let r = guarded(|| -> Result<usize, String> {
                    let weights = janus_ml::presto::weights(&crate::settings::app_data_dir().join("weights"), &|_| {}, &cancel)?;
                    // the units from a sample of the window
                    let first = PrestoInputs::of(&info, &vec![(0.0, 1.0); info.bands_per_date.max(1)], year)?;
                    let read = first.read_bands();
                    let [x0, y0, x1, y1] = window;
                    let pts: Vec<(usize, usize)> = (0..2000)
                        .map(|i| (x0 + (i * 7919) % (x1 - x0).max(1), y0 + (i * 104729) % (y1 - y0).max(1)))
                        .collect();
                    let raw = janus_ml::data::read_points(&info, &read, &pts);
                    let pi = PrestoInputs::of(&info, &band_ranges(&info, &read, &raw), year)?;
                    log.line(&format!(
                        "Presto's inputs: {}; reflectance x {}",
                        pi.read_bands().iter().map(|b| b.name.clone()).collect::<Vec<_>>().join(", "),
                        pi.reflectance_scale
                    ));
                    let dev = shared_device(gpu);
                    log.line(&format!("device: {}", dev.1));
                    janus_ml::presto::embed_window(&info, &pi, window, &weights, &file, &dev.0, &progress, &cancel)
                });
                let mut s = status.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(n) => {
                        log.line(&format!("{n} pixels encoded; done"));
                        s.state = JobState::Done;
                        s.progress = 1.0;
                        let o = json!({"id": "embeddings", "name": "Presto embeddings", "series": true, "path": file.to_string_lossy()});
                        s.result = Some(janus_zeit::job::JobResult { outputs: vec![o], window });
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        s.state = JobState::Failed;
                        s.error = e;
                    }
                }
                c.request_repaint();
            })
            .ok();
    }
}

/// The series' 10 bands of PASTIS (Sentinel-2's B2...B12, by name), in PASTIS's order, when it has them all.
fn pastis_bands(info: &CubeInfo) -> Option<Vec<InputBand>> {
    let found = janus_ml::presto::match_names(&info.band_names, &|_| None);
    // Presto's inputs 2..12 are PASTIS's bands, in its order
    found[2..12].iter().map(|b| b.map(|k| InputBand::source(info, k as i32 + 1))).collect()
}

/// A U-TAE trained on dense labels: the labelled places' pixels (or a class map's), patches over them, each
/// band standardized; from the authors' PASTIS weights if asked.
#[allow(clippy::too_many_arguments)]
fn utae_job(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    sites: &[Site],
    label_map: Option<&(String, usize, usize, usize, usize)>,
    roi: Option<[usize; 4]>,
    classes: &[String],
    h: &janus_ml::utae::UtaeHyper,
    cfg: &TrainConfig,
    gpu: bool,
    model_path: &str,
    log: &JobLog,
    run: &Arc<Mutex<TrainRun>>,
    set: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Header, String> {
    use janus_ml::dense::{NONE, Standard, read_patches, train_utae};
    let (w, hh) = (info.width, info.height);
    let mut labels = vec![NONE; w * hh];
    if let Some((path, x0, y0, mw, mh)) = label_map {
        // a class map: value k is class k (1-based)
        let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
        let (fw, fh) = ds.raster_size();
        let band = ds.rasterband(1).map_err(|e| e.to_string())?;
        let v = band.read_as::<f32>((0, 0), (fw, fh), (*mw, *mh), None).map_err(|e| format!("{path}: {e}"))?;
        for (i, &c) in v.data().iter().enumerate() {
            let (x, y) = (x0 + i % mw, y0 + i / mw);
            if c.is_finite() && c >= 1.0 && (c as usize) <= classes.len() && x < w && y < hh {
                labels[y * w + x] = c as u8 - 1;
            }
        }
    } else {
        for s in sites {
            let k = classes.iter().position(|c| c == &s.class).unwrap_or(0) as u8;
            for &(x, y) in &s.pixels {
                labels[y * w + x] = k;
            }
        }
    }
    if let Some([x0, y0, x1, y1]) = roi {
        for (i, l) in labels.iter_mut().enumerate() {
            let (x, y) = (i % w, i / w);
            if x < x0 || x >= x1 || y < y0 || y >= y1 {
                *l = NONE;
            }
        }
    }
    let n_labels = labels.iter().filter(|&&l| l != NONE).count();
    let mut per_class = vec![0usize; classes.len()];
    for &l in labels.iter().filter(|&&l| l != NONE) {
        per_class[l as usize] += 1;
    }
    log.line(&format!(
        "{n_labels} labelled pixels: {}",
        classes.iter().zip(&per_class).map(|(c, n)| format!("{c} {n}")).collect::<Vec<_>>().join(", ")
    ));
    let pastis = if h.pastis {
        set(0.0, "U-TAE's PASTIS weights");
        Some(janus_ml::utae::pastis_weights(&crate::settings::app_data_dir().join("weights"), &|_| {}, cancel)?)
    } else {
        None
    };
    set(0.01, "reading the patches");
    let mut patches =
        read_patches(info, inputs, &labels, h.patch, &|f| set(0.01 + 0.04 * f, "reading the patches"), cancel)?;
    let standard = Standard::fit(&patches);
    standard.apply(&mut patches.x, patches.c, h.patch * h.patch);
    log.line(&format!("{} patches of {} px", patches.n, h.patch));
    let dev = shared_device(gpu);
    log.line(&format!("device: {}", dev.1));
    set(0.05, &format!("training on {} patches ({})", patches.n, dev.1));
    let calendar = Calendar::of(info);
    let epochs = cfg.epochs;
    let mut on_epoch = |e: &Epoch| {
        log.line(&format!(
            "epoch {:3}: training loss {:.4} accuracy {:.3}, validation loss {:.4} accuracy {:.3} ({:.1} s)",
            e.epoch, e.train_loss, e.train_acc, e.val_loss, e.val_acc, e.seconds
        ));
        run.lock().unwrap().epochs.push(e.clone());
        set(
            0.05 + 0.94 * e.epoch as f64 / epochs as f64,
            &format!("epoch {}: validation accuracy {:.1} %", e.epoch, e.val_acc * 100.0),
        );
    };
    let trained = train_utae(
        &patches,
        classes.len(),
        h,
        &calendar.day_offsets(),
        cfg,
        pastis.as_ref(),
        &dev.0,
        &mut on_epoch,
        cancel,
    )?;
    let (oa, _, _) = trained.accuracies();
    log.line(&format!(
        "best epoch {} of {}; validation overall accuracy {:.4} (pixels)",
        trained.best_epoch,
        trained.history.len(),
        oa
    ));
    let mut training = trained.summary(cfg, &per_class);
    training["device"] = json!(dev.1);
    training["samples"]["patches"] = json!(patches.n);
    let header = Header {
        format: 1,
        janus: env!("CARGO_PKG_VERSION").into(),
        arch: Arch::Utae(h.clone()),
        inputs: inputs.to_vec(),
        calendar,
        norm: Norm::default(),
        standard: Some(standard),
        classes: classes.iter().enumerate().map(|(k, c)| Class { name: c.clone(), color: class_color(k) }).collect(),
        min_valid: 0.0,
        series: info.description.clone(),
        training,
        applicability: None,
        created: crate::zeit::timestamp_text(),
    };
    let model = Model { header: header.clone(), net: trained.net, device: dev.0.clone() };
    model.save(model_path)?;
    log.line(&format!("model: {model_path}"));
    Ok(header)
}

impl MlUi {
    /// The method's estimate settled (or none is made for it).
    pub fn method_settled(&self) -> bool {
        !matches!(self.kind, crate::methods::Kind::Twdtw | crate::methods::Kind::Embeddings)
            || self.method_check.settled()
    }
}
