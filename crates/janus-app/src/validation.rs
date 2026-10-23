//! Validation in the interface (src/app_validation.cpp; janus-validation holds the statistics and the project
//! file): the sampling design over any map (strata, sample size, allocation, the expected errors), the sample drawn
//! into a project (a GeoPackage) and interpreted point by point (blind to the maps assessed), and the accuracy and
//! error-adjusted areas of every map assessed with the sample, recomputed as labels come (microseconds: the maps'
//! values at the points and their areas are read once, in the background). Also the pixel's square on the map,
//! interpreters' agreement and adjudication, the report and the Python that gives the same numbers. The tabs are in
//! validation_tabs.rs, the Chips panel in chips.rs.

#![allow(clippy::needless_range_loop)] // parallel arrays indexed together, as the C++

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Color32, Pos2, Stroke, Ui};
use janus_geo::vectors::{Feature, Kind, Placer, VectorData};
use janus_validation as val;
use val::{AssessedMap, Assessment, DateAgreement, Est, Label, MapKind, MapRule, Prepared, Progress, Project};

use crate::app::State;
use crate::vectors::VectorLayer;

/// The label set of a project not made from a class map (land change).
pub(crate) const EVENT_LABELS: [&str; 9] = [
    "Stable forest",
    "Deforestation",
    "Degradation",
    "Regeneration",
    "Agriculture",
    "Pasture",
    "Water",
    "Cloud / no data",
    "Other",
];

/// Points' colours: labelled by this interpreter, by others only, disagreeing, adjudicated; right, wrong, no
/// reference (the selected map).
const MINE: Color32 = Color32::from_rgb(80, 200, 120);
const OTHERS: Color32 = Color32::from_rgb(90, 160, 255);
const CONFLICT: Color32 = Color32::from_rgb(240, 70, 60);
const SETTLED: Color32 = Color32::from_rgb(190, 120, 255);
const RIGHT: Color32 = Color32::from_rgb(80, 200, 120);
const WRONG: Color32 = Color32::from_rgb(240, 70, 60);
const NO_REF: Color32 = Color32::from_rgb(150, 150, 150);
/// the sample's points on the map
const SAMPLE_COLOUR: Color32 = Color32::from_rgb(255, 217, 51);

/// An area with thousands separators: "12,345", "12.3", "0.0123".
pub(crate) fn fmt_area(v: f64) -> String {
    if !v.is_finite() {
        return "-".into();
    }
    let s = if v.abs() >= 100.0 {
        format!("{v:.0}")
    } else if v.abs() >= 1.0 {
        format!("{v:.1}")
    } else {
        val::fmt_g(v, 3)
    };
    let end = s.find('.').unwrap_or(s.len());
    let start = usize::from(s.starts_with('-'));
    let mut out = s.clone();
    let mut k = end as i64 - 3;
    while k > start as i64 {
        out.insert(k as usize, ',');
        k -= 3;
    }
    out
}

/// "0.912 +- 0.034" ("-" without an estimate).
pub(crate) fn fmt_pm(e: &Est) -> String {
    if !e.est.is_finite() {
        return "-".into();
    }
    format!("{:.3} +- {:.3}", e.est, e.hi - e.est)
}

pub(crate) fn html_esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '&' => o.push_str("&amp;"),
            '"' => o.push_str("&quot;"),
            c => o.push(c),
        }
    }
    o
}

/// The change of a prepared map as a binary map: its user's and producer's accuracy and the area of change (the
/// classes the rule calls change; for the reference, those classes and, for maps of events, every period).
#[derive(Clone, Debug, Default)]
pub(crate) struct BinaryChange {
    pub defined: bool,
    pub users: Est,
    pub producers: Est,
    pub area: Est,
    pub mapped: f64,
}

pub(crate) fn binary_change(r: &MapRule, p: &Prepared) -> BinaryChange {
    let mut b = BinaryChange::default();
    let (nc, n) = (p.input.classes.len(), p.input.stratum.len());
    let mut is_change = vec![false; nc];
    for c in 0..nc {
        let k = p.class_keys[c];
        is_change[c] = if k != val::NONE {
            r.is_change(k)
        } else {
            r.kind == MapKind::Events && p.input.classes[c] != val::NO_CHANGE
        };
        b.defined |= is_change[c];
        if is_change[c] && c < p.input.mapped.len() {
            b.mapped += p.input.mapped[c];
        }
    }
    if !b.defined {
        return b;
    }
    let st: Vec<Option<usize>> = (0..n)
        .map(|i| p.input.stratum[i].filter(|_| p.input.map[i].is_some() && p.input.reference[i].is_some()))
        .collect();
    let e = val::Estimator::new(&p.input.stratum_area, &st, p.input.confidence);
    let (mut m, mut rf, mut both) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    for i in 0..n {
        if st[i].is_none() {
            continue;
        }
        m[i] = if is_change[p.input.map[i].unwrap()] { 1.0 } else { 0.0 };
        rf[i] = if is_change[p.input.reference[i].unwrap()] { 1.0 } else { 0.0 };
        both[i] = m[i] * rf[i];
    }
    b.users = e.ratio(&both, &m);
    b.producers = e.ratio(&both, &rf);
    let a = e.mean(&rf);
    let total: f64 = p.input.stratum_area.iter().map(|v| v.max(0.0)).sum();
    b.area = Est { est: a.est * total, se: a.se * total, lo: a.lo * total, hi: a.hi * total };
    b
}

/// Work on another thread, its result taken once.
pub(crate) struct Job<T>(Option<Receiver<T>>);

impl<T> Default for Job<T> {
    fn default() -> Self {
        Job(None)
    }
}

impl<T: Send + 'static> Job<T> {
    pub fn start(f: impl FnOnce() -> T + Send + 'static, ctx: &egui::Context) -> Job<T> {
        let (tx, rx) = channel();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(f());
            c.request_repaint();
        });
        Job(Some(rx))
    }

    pub fn busy(&self) -> bool {
        self.0.is_some()
    }

    /// The result, once there is one.
    pub fn poll(&mut self) -> Option<T> {
        match self.0.as_ref()?.try_recv() {
            Ok(v) => {
                self.0 = None;
                Some(v)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.0 = None;
                None
            }
        }
    }

    /// Waits for the result.
    pub fn wait(&mut self) -> Option<T> {
        self.0.take()?.recv().ok()
    }
}

/// A map of the project: its values at the points and its areas (read in the background), and its assessment.
pub(crate) struct MapData {
    pub values: Vec<f64>,
    pub mapped: BTreeMap<i32, f64>,
    pub values_ready: bool,
    pub mapped_ready: bool,
    values_job: Job<Result<Vec<f64>, String>>,
    mapped_job: Job<Result<BTreeMap<i32, f64>, String>>,
    pub prog: Arc<Progress>,
    pub error: String,
    /// of the cached assessment
    pub version: u64,
    pub prep: Prepared,
    pub a: Assessment,
    pub change: BinaryChange,
    pub dates: DateAgreement,
    pub tolerance: f32,
    /// translations guessed for its classes
    pub guessed: bool,
}

impl MapData {
    pub fn ready(&self) -> bool {
        self.values_ready && self.mapped_ready
    }

    pub fn mapped_busy(&self) -> bool {
        self.mapped_job.busy()
    }
}

/// The strata computed for a design, or why not.
pub(crate) type StrataResult = (Result<val::Strata, String>, String);

/// A sample drawn: a new project, or points added to the open one.
pub(crate) struct DrawResult {
    pub p: Project,
    pub error: String,
    pub more: bool,
    pub added: Vec<val::Point>,
}

/// A write to the project, one at a time (its error, "" if none).
pub(crate) type Task = Box<dyn FnOnce() -> String + Send>;

/// The estimates as the labels came: the overall accuracy or a class's area at ~40 steps.
#[derive(Default)]
pub(crate) struct Conv {
    pub key: u64,
    pub n: Vec<f64>,
    pub est: Vec<f64>,
    pub lo: Vec<f64>,
    pub hi: Vec<f64>,
}

pub struct ValidationUi {
    /// the Validation panel, the Chips window
    pub show: bool,
    pub show_chips: bool,

    // --- Design ---
    /// the maps to stratify, each one's bins and change classes as typed
    pub maps: Vec<MapRule>,
    pub(crate) bins_text: Vec<String>,
    pub(crate) change_text: Vec<String>,
    pub buffer: i32,
    /// 0 every pixel, 1 the ROI, 2 a vector layer's polygons
    pub limit: usize,
    pub(crate) limit_vector: u32,
    pub(crate) strata_prog: Arc<Progress>,
    pub(crate) strata_job: Job<StrataResult>,
    /// the last computed, its raster and the spec it was computed for
    pub strata: val::Strata,
    pub strata_path: String,
    pub(crate) strata_for: String,
    pub(crate) preview_count: u32,
    pub alloc: usize,
    pub(crate) target_se: f32,
    pub min_per: i32,
    pub(crate) n_auto: bool,
    pub(crate) n: i32,
    pub seed: i32,
    pub confidence: f32,
    pub design_error: String,
    pub(crate) draw_job: Job<DrawResult>,
    /// rasters to open as layers (one at a time), layers to close (by their file)
    pub to_open: VecDeque<String>,
    pub(crate) to_close: VecDeque<String>,
    pub(crate) more_stratum: usize,
    pub(crate) more_n: i32,

    // --- Project ---
    pub project: Option<Project>,
    pub vector_id: u32,
    pub(crate) data: Vec<MapData>,
    pub selected: usize,
    /// labels, translations or maps changed
    pub version: u64,
    pub blind: bool,
    /// files of layers the blind mode hid (shown again after)
    pub(crate) hidden: Vec<String>,
    /// 0 state, 1 stratum, 2 the selected map right or wrong
    pub color_by: usize,
    /// opened before any series: interpreted once one is
    pub(crate) interpret_when_open: bool,
    pub(crate) colors_for: u64,

    // saving: labels a second after the last change, the project's tables when they change; one write at a time
    pub(crate) pending: BTreeMap<usize, Label>,
    pub(crate) pending_as: String,
    pub meta_dirty: bool,
    pub(crate) tasks: VecDeque<Task>,
    pub(crate) writer: Job<String>,
    pub write_error: String,
    pub message: String,

    // --- Accuracy ---
    /// 0 points, 1 proportions of area
    pub(crate) matrix_mode: usize,
    pub(crate) tolerance: f32,
    pub(crate) conv_class: i32,
    pub(crate) conv: Conv,
    pub(crate) by_year: (u64, Vec<(i32, val::ClassArea)>),
    /// a tab to show (0 Design, 1 Accuracy, 2 Maps, 3 Interpreters, 4 Report)
    pub select_tab: Option<usize>,
    /// the guides' screenshots: the Accuracy tab's sections open (bits, in order; None: as left)
    pub(crate) sections: Option<u32>,
    pub(crate) tab: usize,
    /// the Maps tab's bins as typed, for the map selected
    pub(crate) bins_m: (Option<usize>, String),

    // --- the pixel's square on the map ---
    placer: Option<Placer>,
    placer_key: u64,
    placer_vector: u32,

    // --- Chips (chips.rs) ---
    pub(crate) chips: crate::chips::Chips,
}

impl Default for ValidationUi {
    fn default() -> Self {
        ValidationUi {
            show: false,
            show_chips: false,
            maps: Vec::new(),
            bins_text: Vec::new(),
            change_text: Vec::new(),
            buffer: 0,
            limit: 0,
            limit_vector: 0,
            strata_prog: Arc::new(Progress::default()),
            strata_job: Job::default(),
            strata: val::Strata::default(),
            strata_path: String::new(),
            strata_for: String::new(),
            preview_count: 0,
            alloc: 0,
            target_se: 0.01,
            min_per: 50,
            n_auto: true,
            n: 0,
            seed: 42,
            confidence: 0.95,
            design_error: String::new(),
            draw_job: Job::default(),
            to_open: VecDeque::new(),
            to_close: VecDeque::new(),
            more_stratum: 0,
            more_n: 25,
            project: None,
            vector_id: 0,
            data: Vec::new(),
            selected: 0,
            version: 1,
            blind: true,
            hidden: Vec::new(),
            color_by: 0,
            interpret_when_open: false,
            colors_for: 0,
            pending: BTreeMap::new(),
            pending_as: String::new(),
            meta_dirty: false,
            tasks: VecDeque::new(),
            writer: Job::default(),
            write_error: String::new(),
            message: String::new(),
            matrix_mode: 0,
            tolerance: 1.0,
            conv_class: -1,
            conv: Conv::default(),
            by_year: (0, Vec::new()),
            select_tab: None,
            sections: None,
            tab: 0,
            bins_m: (None, String::new()),
            placer: None,
            placer_key: 0,
            placer_vector: 0,
            chips: Default::default(),
        }
    }
}

impl ValidationUi {
    /// No write under way or waiting.
    pub fn idle(&self) -> bool {
        !self.writer.busy() && self.tasks.is_empty() && self.pending.is_empty()
    }

    /// The points whose interpreters disagree.
    pub fn conflicts(&self) -> usize {
        let Some(p) = &self.project else { return 0 };
        (0..p.points.len()).filter(|&i| val::resolve(&p.labels, i).1).count()
    }
}

/// The sample as a vector layer's data: a point per sample, its id the only field (the stratum stays out of sight:
/// blind interpretation).
fn samples_data(p: &Project) -> VectorData {
    VectorData {
        path: p.path.clone(),
        layer: "samples".into(),
        crs_wkt: p.strata.grid.wkt.clone(),
        crs_name: gdal::spatial_ref::SpatialRef::from_wkt(&p.strata.grid.wkt)
            .ok()
            .and_then(|s| s.name())
            .unwrap_or_default(),
        fields: vec!["sample_id".into()],
        features: p
            .points
            .iter()
            .map(|q| Feature {
                fid: i64::from(q.id),
                kind: Kind::Point,
                parts: vec![vec![q.x, q.y]],
                cx: q.x,
                cy: q.y,
                values: vec![q.id.to_string()],
            })
            .collect(),
        points: p.points.len(),
        vertices: p.points.len(),
        ..Default::default()
    }
}

/// A map to stratify or to assess, from what is open.
pub(crate) struct Source {
    pub label: String,
    pub rule: MapRule,
}

/// Three equal intervals of [lo, hi].
fn thirds(lo: f64, hi: f64) -> Vec<f64> {
    let hi = if hi > lo { hi } else { lo + 1.0 };
    vec![lo, lo + (hi - lo) / 3.0, lo + 2.0 * (hi - lo) / 3.0, hi]
}

/// A raster file as a map: classes (integers, with the band's category names), events (years of change) or three
/// intervals of its range.
pub(crate) fn file_rule(path: &str) -> MapRule {
    let mut r = MapRule {
        path: path.to_string(),
        name: std::path::Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        kind: MapKind::Intervals,
        ..Default::default()
    };
    let Ok(ds) = gdal::Dataset::open(path) else { return r };
    let Ok(b) = ds.rasterband(1) else { return r };
    use gdal::raster::GdalDataType::*;
    if !matches!(b.band_type(), Float32 | Float64) {
        r.kind = MapKind::Classes;
    }
    // SAFETY: a live band; GDAL owns the list.
    unsafe {
        let cats = gdal_sys::GDALGetRasterCategoryNames(b.c_rasterband());
        if !cats.is_null() {
            let mut k = 0;
            while !(*cats.add(k)).is_null() {
                let s = std::ffi::CStr::from_ptr(*cats.add(k)).to_string_lossy().into_owned();
                if !s.is_empty() {
                    r.names.insert(k as i32, s);
                }
                k += 1;
            }
        }
    }
    if r.kind == MapKind::Intervals {
        let (mut mn, mut mx) = (f64::NAN, f64::NAN);
        if let Ok(s) = b.compute_raster_min_max(true) {
            (mn, mx) = (s.min, s.max);
        }
        let lower = r.name.to_ascii_lowercase();
        if lower.contains("yod") || lower.contains("year") || (mn >= 0.0 && mx > 1900.0 && mx < 2200.0) {
            r.kind = MapKind::Events;
        } else if mx > mn {
            r.bins = thirds(mn, mx);
            r.bins[3] = mx.next_up(); // the maximum inside the last interval
        }
    }
    r
}

impl State {
    /// The name labels are kept under: Settings', else the login.
    pub fn interpreter_name(&self) -> String {
        if !self.settings.s.interpreter.is_empty() {
            return self.settings.s.interpreter.clone();
        }
        std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok()
            .filter(|u| !u.is_empty())
            .unwrap_or_else(|| "interpreter".into())
    }

    pub fn validation_project_open(&self) -> bool {
        self.val.project.is_some()
    }

    pub fn is_project_vector(&self, id: u32) -> bool {
        self.val.project.is_some() && id != 0 && self.val.vector_id == id
    }

    /// The date of layer i shown (the active one's is the map's).
    pub(crate) fn date_of_layer(&self, i: usize) -> usize {
        let l = &self.layers[i];
        let info =
            if i == self.active { self.session.as_ref().map(|s| &s.info) } else { l.session.as_ref().map(|s| &s.info) };
        let n = info.map_or(1, |i| i.t()).max(1);
        (if i == self.active { self.display.t } else { l.disp.t }).min(n - 1)
    }

    /// The maps that can be stratified or assessed: Zeit's results and every layer at its date.
    pub(crate) fn validation_sources(&self) -> Vec<Source> {
        let mut out = Vec::new();
        for r in &self.zeit.results {
            let mut rule = MapRule { path: r.path.clone(), name: r.name.clone(), ..Default::default() };
            if !r.classes.is_empty() {
                rule.kind = MapKind::Classes;
                for (k, c) in r.classes.iter().enumerate() {
                    rule.names.insert(k as i32 + 1, c.clone());
                }
            } else if r.unit == "year" || r.unit == "years" {
                rule.kind = MapKind::Events;
            } else {
                rule.kind = MapKind::Intervals;
                rule.bins = thirds(f64::from(r.lo), f64::from(r.hi));
            }
            out.push(Source { label: r.name.clone(), rule });
        }
        for (i, l) in self.layers.iter().enumerate() {
            let info = if i == self.active {
                self.session.as_ref().map(|s| &s.info)
            } else {
                l.session.as_ref().map(|s| &s.info)
            };
            let Some(info) = info else { continue };
            if info.spectral || info.geo_transform.is_none() {
                continue;
            }
            let t = self.date_of_layer(i);
            let label = format!("{} @ {}", l.name, info.layers[t].label);
            let mut rule = MapRule {
                path: info.layers[t].path.clone(),
                band: info.layers[t].band,
                name: label.clone(),
                ..Default::default()
            };
            let classes = if i == self.active { &self.classes } else { &l.classes };
            if classes.state == crate::classes::ClassState::On {
                rule.kind = MapKind::Classes;
                for c in &classes.list {
                    rule.names.insert(c.value, c.name.clone());
                }
            } else {
                rule.kind = MapKind::Intervals;
                let m = janus_gpu::Mode::Value as usize;
                let rg = if i == self.active { &self.display.ranges[m] } else { &l.disp.ranges[m] };
                rule.bins = thirds(f64::from(rg.lo), f64::from(rg.hi));
            }
            out.push(Source { label, rule });
        }
        out
    }

    /// Map k of the project: its values at the points read, and its areas (those of a map of the strata came with
    /// them).
    pub(crate) fn validation_add_map_data(&mut self, k: usize, ctx: &egui::Context) {
        let threads = self.session_settings().processing_threads();
        let v = &mut self.val;
        let Some(p) = &v.project else { return };
        let m = &p.maps[k];
        let mut d = MapData {
            values: Vec::new(),
            mapped: BTreeMap::new(),
            values_ready: false,
            mapped_ready: false,
            values_job: Job::default(),
            mapped_job: Job::default(),
            prog: Arc::new(Progress::with_threads(threads)),
            error: String::new(),
            version: 0,
            prep: Prepared::default(),
            a: Assessment::default(),
            change: BinaryChange::default(),
            dates: DateAgreement::default(),
            tolerance: -1.0,
            guessed: false,
        };
        for (s, r) in p.spec.maps.iter().enumerate() {
            if s < p.strata.mapped.len()
                && r.path == m.rule.path
                && r.band == m.rule.band
                && r.kind == m.rule.kind
                && r.bins == m.rule.bins
            {
                d.mapped = p.strata.mapped[s].clone();
                d.mapped_ready = true;
            }
        }
        let xy: Vec<[f64; 2]> = p.points.iter().map(|q| [q.x, q.y]).collect();
        let (rule, wkt) = (m.rule.clone(), p.strata.grid.wkt.clone());
        d.values_job = Job::start(move || val::values_at(&rule, &wkt, &xy), ctx);
        if !d.mapped_ready {
            let (rule, path, strata, prog) = (m.rule.clone(), p.strata_path.clone(), p.strata.clone(), d.prog.clone());
            d.mapped_job = Job::start(move || val::mapped_areas(&rule, &path, &strata, Some(&prog)), ctx);
        }
        if v.data.len() <= k {
            v.data.resize_with(k + 1, || MapData {
                values: Vec::new(),
                mapped: BTreeMap::new(),
                values_ready: false,
                mapped_ready: false,
                values_job: Job::default(),
                mapped_job: Job::default(),
                prog: Arc::new(Progress::default()),
                error: String::new(),
                version: 0,
                prep: Prepared::default(),
                a: Assessment::default(),
                change: BinaryChange::default(),
                dates: DateAgreement::default(),
                tolerance: -1.0,
                guessed: false,
            });
        }
        v.data[k] = d;
    }

    /// The sample's vector layer (placed on the active layer).
    fn add_sample_layer(&mut self) {
        if self.val.project.is_none() {
            return;
        }
        let z = self.take_z();
        let Some(p) = &self.val.project else { return };
        let id = self.next_vector_id;
        self.next_vector_id += 1;
        let name = format!(
            "Sample: {}",
            std::path::Path::new(&p.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
        );
        self.vectors.push(VectorLayer {
            id,
            data: Arc::new(samples_data(p)),
            name,
            color: SAMPLE_COLOUR,
            visible: true,
            px: Default::default(),
            boxes: Vec::new(),
            anchor: Vec::new(),
            placed_for: 0,
            why: String::new(),
            off_map: false,
            colors: Vec::new(),
            gpu: Default::default(),
            placing: None,
            z,
        });
        self.val.vector_id = id;
        self.val.colors_for = 0;
        let i = self.vectors.len() - 1;
        self.place_vector(i);
        self.map.dirty = true;
    }

    /// A project opened: its sample as a vector layer, its maps read, the interpretation started (blind).
    pub fn open_validation_project(&mut self, path: &str, ctx: &egui::Context) {
        let p = match val::load_project(path) {
            Ok(p) => p,
            Err(e) => {
                self.error_popup = Some(e);
                return;
            }
        };
        self.close_validation_project();
        let missing = !std::path::Path::new(&p.strata_path).exists();
        if missing {
            self.val.message = format!(
                "The strata raster is missing ({}): the areas of maps added now cannot be read.",
                p.strata_path
            );
        }
        self.val.project = Some(p);
        self.add_sample_layer();
        let p = self.val.project.as_mut().unwrap();
        if p.maps.is_empty() {
            p.maps = p.spec.maps.iter().map(|r| AssessedMap { rule: r.clone(), mapping: BTreeMap::new() }).collect();
        }
        let n = p.maps.len();
        self.val.data.clear();
        for k in 0..n {
            self.validation_add_map_data(k, ctx);
        }
        let v = &mut self.val;
        v.selected = 0;
        v.version += 1;
        v.blind = true;
        v.show = true;
        v.interpret_when_open = self.session.is_none();
        if self.session.is_some() {
            self.start_interpretation(self.val.vector_id);
        }
    }

    /// Every write done, the blind mode ended, the sample's layer removed.
    pub fn close_validation_project(&mut self) {
        if self.val.project.is_none() {
            return;
        }
        if self.labels.project {
            self.stop_interpretation(); // writes what is pending
        }
        self.validation_save_labels();
        if self.val.meta_dirty {
            let copy = self.val.project.clone().unwrap();
            self.val.tasks.push_back(Box::new(move || val::save_project_meta(&copy).err().unwrap_or_default()));
            self.val.meta_dirty = false;
        }
        // every write done before the project goes
        loop {
            if self.val.writer.busy()
                && let Some(e) = self.val.writer.wait()
                && !e.is_empty()
            {
                self.val.write_error = e;
            }
            let Some(t) = self.val.tasks.pop_front() else { break };
            let e = t();
            if !e.is_empty() {
                self.val.write_error = e;
            }
        }
        self.set_validation_blind(false);
        if let Some(i) = self.vector_index(self.val.vector_id) {
            self.remove_vector(i);
        }
        let v = &mut self.val;
        v.vector_id = 0;
        v.data.clear();
        v.project = None;
        v.placer = None;
        v.placer_vector = 0;
    }

    /// The interpretation's label set and this interpreter's labels, from the project.
    pub(crate) fn validation_entries(&mut self) {
        let Some(p) = &self.val.project else { return };
        self.labels.classes = if p.label_set.is_empty() {
            EVENT_LABELS.iter().map(|s| s.to_string()).collect()
        } else {
            p.label_set.clone()
        };
        self.labels.entries = vec![Label::default(); p.points.len()];
        if let Some(mine) = p.labels.get(&self.labels.who) {
            for (e, l) in self.labels.entries.iter_mut().zip(mine) {
                *e = l.clone();
            }
        }
    }

    /// A label given: into the project (saved a second later).
    pub(crate) fn validation_labels_changed(&mut self, feature: usize) {
        if self.val.project.is_none() || feature >= self.labels.entries.len() {
            return;
        }
        if !self.val.pending.is_empty() && self.val.pending_as != self.labels.who {
            self.validation_save_labels();
        }
        let who = self.labels.who.clone();
        let e = self.labels.entries[feature].clone();
        let v = &mut self.val;
        let p = v.project.as_mut().unwrap();
        let n = p.points.len();
        let mine = p.labels.entry(who.clone()).or_default();
        mine.resize(n, Label::default());
        mine[feature] = e.clone();
        v.pending.insert(feature, e);
        v.pending_as = who;
        v.version += 1;
    }

    /// The labels given since the last write, and the reference that follows for them.
    pub(crate) fn validation_save_labels(&mut self) {
        let v = &mut self.val;
        let Some(p) = &v.project else { return };
        if v.pending.is_empty() {
            return;
        }
        let rows: Vec<(usize, Label)> = std::mem::take(&mut v.pending).into_iter().collect();
        let finals: Vec<val::FinalRef> = rows.iter().map(|(i, _)| final_ref(p, *i)).collect();
        let (path, who) = (p.path.clone(), v.pending_as.clone());
        v.tasks.push_back(Box::new(move || val::save_labels(&path, &who, &rows, &finals).err().unwrap_or_default()));
    }

    /// The label set changed: kept in the project, the new labels' translations guessed for every map.
    pub(crate) fn validation_label_set_changed(&mut self) {
        let classes = self.labels.classes.clone();
        let v = &mut self.val;
        let Some(p) = v.project.as_mut() else { return };
        p.label_set = classes;
        for k in 0..p.maps.len() {
            let names: Vec<String> = v
                .data
                .get(k)
                .map(|d| {
                    val::map_classes(&p.maps[k].rule, &d.values, &d.mapped)
                        .into_iter()
                        .map(|c| p.maps[k].rule.class_name(c))
                        .collect()
                })
                .unwrap_or_default();
            for (l, t) in val::default_mapping(&p.maps[k].rule, &p.label_set, &names) {
                p.maps[k].mapping.entry(l).or_insert(t);
            }
        }
        v.meta_dirty = true;
        v.version += 1;
    }

    /// Blind: the maps assessed and the strata are not drawn while interpreting (hidden in the pump); off: shown
    /// again.
    pub fn set_validation_blind(&mut self, on: bool) {
        self.val.blind = on;
        if on {
            return;
        }
        for f in std::mem::take(&mut self.val.hidden) {
            for r in &mut self.zeit.results {
                if r.path == f {
                    r.visible = true;
                }
            }
            for l in &mut self.layers {
                if l.inputs.len() == 1 && l.inputs[0] == f {
                    l.visible = true;
                }
            }
        }
        self.map.dirty = true;
    }

    /// The points interpreters disagree on, gone through: the label given settles each.
    pub fn start_adjudication(&mut self) {
        let Some(p) = &self.val.project else { return };
        let conflicts: Vec<usize> = (0..p.points.len()).filter(|&i| val::resolve(&p.labels, i).1).collect();
        if self.labels.vector != Some(self.val.vector_id) {
            self.start_interpretation(self.val.vector_id);
        }
        if !self.labels.project {
            return;
        }
        if self.labels.dirty {
            self.save_labels();
        }
        self.labels.who = val::ADJUDICATED.into();
        self.validation_entries();
        self.labels.filter_name = format!("{} disagreement(s), adjudicating", conflicts.len());
        self.labels.filter = conflicts;
        if let Some(&first) = self.labels.filter.first() {
            self.interp_go(first);
        }
        self.labels.show = true;
    }

    /// Another copy's labels taken (the newer where both labelled a point); how many, or -1.
    pub fn validation_merge(&mut self, path: &str) -> (i64, String) {
        let Some(p) = self.val.project.as_mut() else { return (-1, String::new()) };
        let (id, other) = match val::read_labels(path, p.points.len()) {
            Ok(r) => r,
            Err(e) => return (-1, e),
        };
        if id != p.id {
            return (-1, "That file is not a copy of this project (another sample).".into());
        }
        let mut taken = 0;
        for (w, l) in other {
            let n = p.points.len();
            let mine = p.labels.entry(w.clone()).or_default();
            mine.resize(n, Label::default());
            let mut rows = Vec::new();
            for (i, x) in l.iter().enumerate() {
                if x.is_empty() {
                    continue;
                }
                if mine[i].is_empty() || x.updated > mine[i].updated {
                    mine[i] = x.clone();
                    rows.push((i, x.clone()));
                }
            }
            if rows.is_empty() {
                continue;
            }
            taken += rows.len();
            let finals: Vec<val::FinalRef> = rows.iter().map(|(i, _)| final_ref(p, *i)).collect();
            let pp = p.path.clone();
            self.val
                .tasks
                .push_back(Box::new(move || val::save_labels(&pp, &w, &rows, &finals).err().unwrap_or_default()));
        }
        if self.labels.project {
            self.validation_entries();
        }
        self.val.version += 1;
        (taken as i64, format!("{taken} label(s) merged (the newer one kept where both labelled a point)."))
    }

    /// Once a frame: strata and samples computed, rasters opened and closed, the maps read, translations guessed,
    /// writes, the blind mode, the points' colours.
    pub(crate) fn pump_validation(&mut self, ctx: &egui::Context) {
        // strata computed: shown as a layer (their raster has names and colours), the previous ones closed
        if let Some((r, path)) = self.val.strata_job.poll() {
            let v = &mut self.val;
            match r {
                Err(e) => v.design_error = e,
                Ok(mut s) => {
                    if !v.strata_path.is_empty() {
                        v.to_close.push_back(v.strata_path.clone());
                    }
                    for t in &mut s.strata {
                        t.expected_ua = if t.change || t.buffer { 0.7 } else { 0.9 };
                    }
                    v.strata = s;
                    v.strata_path = path.clone();
                    v.design_error.clear();
                    v.to_open.push_back(path);
                }
            }
        }
        // the sample drawn: a new project, or points added to the open one
        if let Some(j) = self.val.draw_job.poll() {
            if !j.error.is_empty() {
                self.val.design_error = j.error;
            } else if j.more && self.val.project.as_ref().is_some_and(|p| p.path == j.p.path) {
                let v = &mut self.val;
                let p = v.project.as_mut().unwrap();
                p.points.extend_from_slice(&j.added);
                p.strata.strata = j.p.strata.strata.clone();
                let n = p.points.len();
                for l in p.labels.values_mut() {
                    l.resize(n, Label::default());
                }
                let data = Arc::new(samples_data(p));
                v.message = format!("{} point(s) added to the sample.", j.added.len());
                v.version += 1;
                if let Some(i) = self.vector_index(self.val.vector_id) {
                    let l = &mut self.vectors[i];
                    l.data = data;
                    l.placed_for = 0;
                    l.colors.clear();
                }
                if self.labels.project {
                    self.labels.entries.resize(n, Label::default());
                }
                let maps = self.val.project.as_ref().map_or(0, |p| p.maps.len());
                for k in 0..maps {
                    self.validation_add_map_data(k, ctx);
                }
            } else {
                let path = j.p.path.clone();
                if self.val.project.is_some() {
                    self.close_validation_project();
                }
                // the preview's raster is replaced by the project's copy
                let v = &mut self.val;
                if !v.strata_path.is_empty() {
                    v.to_close.push_back(std::mem::take(&mut v.strata_path));
                }
                v.strata_for.clear();
                self.open_validation_project(&path, ctx);
                if let Some(p) = &self.val.project {
                    self.val.to_open.push_back(p.strata_path.clone());
                }
            }
        }
        // one raster opened at a time (the series being studied stays active); layers closed by file
        while let Some(f) = self.val.to_close.pop_front() {
            for i in (0..self.layers.len()).rev() {
                if self.layers[i].inputs.len() == 1 && self.layers[i].inputs[0] == f && i != self.active {
                    self.remove_layer(i, ctx);
                }
            }
        }
        if self.opening.is_none() && !self.layers.is_empty() && !self.val.to_open.is_empty() {
            let f = self.val.to_open.pop_front().unwrap();
            self.zeit.keep_active = Some(self.active);
            self.open_inputs(vec![f], true, ctx);
        }
        // the strata layers named as such
        for l in &mut self.layers {
            if l.inputs.len() == 1 && l.inputs[0].ends_with(".tif") {
                let f = &l.inputs[0];
                let is_strata =
                    *f == self.val.strata_path || self.val.project.as_ref().is_some_and(|p| p.strata_path == *f);
                if is_strata && !l.name.starts_with("Strata: ") {
                    let stem = std::path::Path::new(f).file_stem().map(|s| s.to_string_lossy().into_owned());
                    l.name = format!("Strata: {}", stem.unwrap_or_default());
                }
            }
        }
        if self.val.project.is_none() {
            return;
        }
        if self.vector_index(self.val.vector_id).is_none() {
            // removed from the Layers panel: the project's points come back
            self.add_sample_layer();
        }
        self.update_validation_placer();
        if let Some(t) = self.opts.validation_tab.take() {
            self.val.show = true;
            self.val.select_tab = Some(t);
        }
        if self.val.interpret_when_open && self.session.is_some() && self.opening.is_none() {
            self.val.interpret_when_open = false;
            self.start_interpretation(self.val.vector_id);
        }
        // the maps' values at the points and their areas
        let v = &mut self.val;
        for d in &mut v.data {
            if let Some(r) = d.values_job.poll() {
                match r {
                    Ok(x) => {
                        d.values = x;
                        d.values_ready = true;
                    }
                    Err(e) => d.error = e,
                }
                v.version += 1;
            }
            if let Some(r) = d.mapped_job.poll() {
                match r {
                    Ok(x) => {
                        d.mapped = x;
                        d.mapped_ready = true;
                    }
                    Err(e) => d.error = e,
                }
                v.version += 1;
            }
        }
        // translations for labels that have none yet (a map added, its classes known)
        let p = v.project.as_mut().unwrap();
        for k in 0..p.maps.len().min(v.data.len()) {
            let d = &mut v.data[k];
            if !d.ready() || d.guessed {
                continue;
            }
            d.guessed = true;
            let names: Vec<String> = val::map_classes(&p.maps[k].rule, &d.values, &d.mapped)
                .into_iter()
                .map(|c| p.maps[k].rule.class_name(c))
                .collect();
            let mut added = false;
            for (l, t) in val::default_mapping(&p.maps[k].rule, &p.label_set, &names) {
                if let std::collections::btree_map::Entry::Vacant(e) = p.maps[k].mapping.entry(l) {
                    e.insert(t);
                    added = true;
                }
            }
            if added {
                v.meta_dirty = true;
                v.version += 1;
            }
        }
        // writes, one at a time
        if let Some(e) = v.writer.poll() {
            v.write_error = e;
        }
        if v.meta_dirty && v.tasks.is_empty() {
            v.meta_dirty = false;
            let copy = v.project.clone().unwrap();
            v.tasks.push_back(Box::new(move || val::save_project_meta(&copy).err().unwrap_or_default()));
        }
        if !v.writer.busy()
            && let Some(t) = v.tasks.pop_front()
        {
            v.writer = Job::start(t, ctx);
        }
        // blind: the maps assessed and the strata are not drawn while interpreting
        let p = self.val.project.as_ref().unwrap();
        if self.val.blind && self.labels.project {
            let mut files: BTreeSet<String> = BTreeSet::from([p.strata_path.clone()]);
            files.extend(p.maps.iter().map(|m| m.rule.path.clone()));
            files.extend(p.spec.maps.iter().map(|r| r.path.clone()));
            let mut hide = Vec::new();
            for r in &mut self.zeit.results {
                if r.visible && files.contains(&r.path) {
                    r.visible = false;
                    hide.push(r.path.clone());
                }
            }
            for (i, l) in self.layers.iter_mut().enumerate() {
                if i == self.active || !l.visible || l.inputs.len() != 1 {
                    continue;
                }
                if files.contains(&l.inputs[0]) {
                    l.visible = false;
                    hide.push(l.inputs[0].clone());
                }
            }
            for f in hide {
                if !self.val.hidden.contains(&f) {
                    self.val.hidden.push(f);
                }
                self.map.dirty = true;
            }
        }
        // the points' colours: their state, their stratum, or right / wrong for the selected map
        let Some(vi) = self.vector_index(self.val.vector_id) else { return };
        let v = &self.val;
        let p = v.project.as_ref().unwrap();
        let by = if v.blind && self.labels.project { 0 } else { v.color_by };
        let key = (v.version.wrapping_mul(31) + by as u64).wrapping_mul(31)
            + v.selected as u64
            + ((self.labels.who.len() as u64) << 20);
        if key == v.colors_for && self.vectors[vi].colors.len() == p.points.len() {
            return;
        }
        let me = if self.labels.who.is_empty() { self.interpreter_name() } else { self.labels.who.clone() };
        let deep: Vec<[u8; 4]> = janus_gpu::colormaps::all()[0].keys().collect();
        let d = v.data.get(v.selected);
        let colors: Vec<Color32> = (0..p.points.len())
            .map(|i| {
                if by == 1 {
                    let s = p.strata.index_of_code(p.points[i].stratum).unwrap_or(0);
                    let c = deep[s % deep.len()];
                    return Color32::from_rgb(c[0], c[1], c[2]);
                }
                if by == 2
                    && let Some(d) = d.filter(|d| d.version == v.version && i < d.prep.input.reference.len())
                {
                    return match (d.prep.input.map[i], d.prep.input.reference[i]) {
                        (Some(m), Some(r)) if m == r => RIGHT,
                        (Some(_), Some(_)) => WRONG,
                        _ => NO_REF,
                    };
                }
                let (r, conflict) = val::resolve(&p.labels, i);
                let has = |w: &str| p.labels.get(w).and_then(|l| l.get(i)).is_some_and(|l| !l.label.is_empty());
                if conflict {
                    CONFLICT
                } else if has(val::ADJUDICATED) {
                    SETTLED
                } else if has(&me) {
                    MINE
                } else if r.is_some() {
                    OTHERS
                } else {
                    SAMPLE_COLOUR
                }
            })
            .collect();
        self.val.colors_for = key;
        self.vectors[vi].colors = colors;
        self.map.dirty = true;
    }

    /// The placer of the sample's points on the active layer (for the pixel's square), made again when the layer's
    /// grid changes.
    fn update_validation_placer(&mut self) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let Some(vi) = self.vector_index(self.val.vector_id) else { return };
        let key = self.vector_place_key();
        if self.val.placer.is_none() || self.val.placer_key != key || self.val.placer_vector != self.val.vector_id {
            self.val.placer = Placer::new(&self.vectors[vi].data, &info.crs_wkt, info.geo_transform).ok();
            self.val.placer_key = key;
            self.val.placer_vector = self.val.vector_id;
        }
    }

    /// The pixel of the point being interpreted (a ring when it is too small to see).
    pub(crate) fn draw_validation_marks(&self, painter: &egui::Painter, at: &dyn Fn(f64, f64) -> Pos2) {
        if self.val.project.is_none() || !self.labels.project || self.session.is_none() {
            return;
        }
        let Some(cur) = self.labels.current else { return };
        let (Some(pl), Some(p)) = (&self.val.placer, &self.val.project) else { return };
        let Some(q) = p.points.get(cur) else { return };
        let g = &p.strata.grid.gt;
        let mut c = [Pos2::ZERO; 4];
        for (k, (dx, dy)) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)].into_iter().enumerate() {
            let (col, row) = (f64::from(q.col) + dx, f64::from(q.row) + dy);
            let (x, y) = (g[0] + col * g[1] + row * g[2], g[3] + col * g[4] + row * g[5]);
            let Some((px, py)) = pl.to_pixel(x, y) else { return };
            c[k] = at(px, py);
        }
        let size = (c[2].x - c[0].x).abs().max((c[2].y - c[0].y).abs());
        if size < 6.0 {
            let m = Pos2::new((c[0].x + c[2].x) / 2.0, (c[0].y + c[2].y) / 2.0);
            painter.circle_stroke(m, 9.0, Stroke::new(4.0, Color32::from_black_alpha(200)));
            painter.circle_stroke(m, 9.0, Stroke::new(2.0, Color32::WHITE));
            return;
        }
        let quad = |w: f32, col: Color32| egui::Shape::closed_line(c.to_vec(), Stroke::new(w, col));
        painter.add(quad(4.0, Color32::from_black_alpha(200)));
        painter.add(quad(2.0, Color32::from_rgb(255, 255, 0)));
    }

    /// The Validation menu.
    pub(crate) fn validation_menu(&mut self, ui: &mut Ui) {
        let open = self.validation_project_open();
        if ui
            .button("New sampling design...")
            .on_hover_text(
                "Strata of a map (its classes, intervals of its values, its changes or the agreement\n\
                 of several maps), the sample size and its allocation, then a stratified random\n\
                 sample drawn into a project (Olofsson et al. 2014)",
            )
            .clicked()
        {
            self.val.show = true;
            self.val.select_tab = Some(0);
            ui.close();
        }
        if ui.button("Open project...").clicked() {
            ui.close();
            if let Some(f) = rfd::FileDialog::new().add_filter("GeoPackage", &["gpkg"]).pick_file() {
                self.open_validation_project(&f.to_string_lossy(), ui.ctx());
            }
        }
        if ui.add_enabled(open, egui::Button::new("Close project")).clicked() {
            self.close_validation_project();
            ui.close();
        }
        ui.separator();
        if ui.add(egui::Button::selectable(self.val.show, "Validation panel")).clicked() {
            self.val.show = !self.val.show;
            ui.close();
        }
        if ui.add_enabled(open && self.session.is_some(), egui::Button::new("Interpret the sample")).clicked() {
            self.start_interpretation(self.val.vector_id);
            self.labels.show = true;
            ui.close();
        }
        if ui
            .add(egui::Button::selectable(self.val.show_chips, "Chips"))
            .on_hover_text("The feature being interpreted at every date, around it")
            .clicked()
        {
            self.val.show_chips = !self.val.show_chips;
            ui.close();
        }
        ui.separator();
        if ui.add_enabled(open, egui::Button::new("Accuracy and areas")).clicked() {
            self.val.show = true;
            self.val.select_tab = Some(1);
            ui.close();
        }
        if ui.add_enabled(open, egui::Button::new("Export report...")).clicked() {
            ui.close();
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.export_validation_report(&dir.to_string_lossy(), ui.ctx());
            }
        }
        let has_maps = self.val.project.as_ref().is_some_and(|p| !p.maps.is_empty());
        if ui.add_enabled(open && has_maps, egui::Button::new("Copy Python (zeit.accuracy)")).clicked() {
            let p = self.val.project.as_ref().unwrap();
            let k = self.val.selected.min(p.maps.len() - 1);
            ui.ctx().copy_text(val::python_snippet(p, &p.maps[k], f64::from(self.val.confidence)));
            ui.close();
        }
    }

    /// The Validation panel: the project's state, then its tabs.
    pub(crate) fn validation_ui(&mut self, ui: &mut Ui) {
        if let Some(p) = &self.val.project {
            let (mut labelled, mut conflicts) = (0, 0);
            for i in 0..p.points.len() {
                let (r, c) = val::resolve(&p.labels, i);
                labelled += usize::from(r.is_some());
                conflicts += usize::from(c);
            }
            let file =
                std::path::Path::new(&p.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let path = p.path.clone();
            let n = p.points.len();
            let mut blind = self.val.blind;
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(crate::theme::accent(), file).on_hover_text(path);
                ui.weak(format!(
                    "{labelled} of {n} points with a reference{}",
                    if conflicts > 0 { format!(", {conflicts} disagreement(s)") } else { String::new() }
                ));
                ui.checkbox(&mut blind, "Blind").on_hover_text(
                    "While interpreting, the maps assessed and the strata are not drawn and the points\n\
                     are coloured by their state only: the reference is not swayed by the maps",
                );
            });
            if blind != self.val.blind {
                self.set_validation_blind(blind);
            }
            if !self.val.write_error.is_empty() {
                ui.colored_label(crate::theme::error(), format!("Not saved: {}", self.val.write_error));
            }
        }
        if !self.val.message.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.val.message);
                if ui.small_button("OK").clicked() {
                    self.val.message.clear();
                }
            });
        }
        if let Some(t) = self.val.select_tab.take() {
            self.val.tab = t;
        }
        ui.horizontal(|ui| {
            for (i, name) in ["Design", "Accuracy", "Maps", "Interpreters", "Report"].iter().enumerate() {
                ui.selectable_value(&mut self.val.tab, i, *name);
            }
        });
        ui.separator();
        match self.val.tab {
            0 => self.validation_design_ui(ui),
            1 => self.validation_accuracy_ui(ui),
            2 => self.validation_maps_ui(ui),
            3 => self.validation_interpreters_ui(ui),
            _ => self.validation_report_ui(ui),
        }
    }

    /// The Interpretation panel's part for a project: who labels, blind, chips, the others' labels of the point.
    pub(crate) fn interpretation_project_ui(&mut self, ui: &mut Ui) {
        let Some(p) = &self.val.project else { return };
        let adjudicating = self.labels.who == val::ADJUDICATED;
        let mut blind = self.val.blind;
        let (mut chips, mut todo, mut back) = (false, false, false);
        ui.horizontal_wrapped(|ui| {
            if adjudicating {
                ui.colored_label(crate::theme::warning(), "Adjudicating: your label settles the point");
            } else {
                ui.weak(format!("Labelling as {}", self.labels.who));
            }
            ui.checkbox(&mut blind, "Blind");
            chips = ui
                .small_button("Chips")
                .on_hover_text("The point at every date, around it (Validation > Chips)")
                .clicked();
            if !self.val.blind {
                let names = ["points by state", "points by stratum", "right / wrong (selected map)"];
                egui::ComboBox::from_id_salt("colorby").selected_text(names[self.val.color_by.min(2)]).show_ui(
                    ui,
                    |ui| {
                        for (k, n) in names.iter().enumerate() {
                            ui.selectable_value(&mut self.val.color_by, k, *n);
                        }
                    },
                );
            }
            if !adjudicating {
                todo = ui.small_button("Mine to do").clicked();
            } else {
                back = ui.small_button("Back to my labels").clicked();
            }
        });
        // others' labels of this point (all of them when adjudicating; after one's own label otherwise)
        if let Some(i) = self.labels.current.filter(|&i| i < p.points.len()) {
            let mine_given = !self.labels.entries.get(i).is_none_or(|e| e.label.is_empty());
            let others: Vec<(&String, &Label)> = p
                .labels
                .iter()
                .filter(|(w, l)| **w != self.labels.who && l.get(i).is_some_and(|x| !x.label.is_empty()))
                .map(|(w, l)| (w, &l[i]))
                .collect();
            if !others.is_empty() && (adjudicating || mine_given || !self.val.blind) {
                egui::Grid::new("others").striped(true).show(ui, |ui| {
                    for (w, l) in &others {
                        ui.weak(*w);
                        ui.label(&l.label);
                        ui.label(&l.date);
                        ui.weak(&l.notes);
                        ui.end_row();
                    }
                });
            } else if !others.is_empty() {
                ui.weak(format!("{} other interpreter(s) labelled this point (shown after your label)", others.len()));
            }
        }
        if blind != self.val.blind {
            self.set_validation_blind(blind);
        }
        if chips {
            self.val.show_chips = !self.val.show_chips;
        }
        if todo {
            let l = &mut self.labels;
            l.filter = (0..l.entries.len()).filter(|&i| l.entries[i].label.is_empty()).collect();
            l.filter_name = format!("{} not labelled by {}", l.filter.len(), l.who);
            if let Some(&f) = l.filter.first() {
                self.interp_go(f);
            }
        }
        if back {
            if self.labels.dirty {
                self.save_labels();
            }
            self.labels.who = self.interpreter_name();
            self.validation_entries();
            self.labels.filter.clear();
            self.labels.filter_name.clear();
        }
    }
}

/// The reference of point i as the samples layer keeps it.
pub(crate) fn final_ref(p: &Project, i: usize) -> val::FinalRef {
    let (r, conflict) = val::resolve(&p.labels, i);
    val::FinalRef {
        point: i,
        reference: r.cloned().unwrap_or_default(),
        conflict,
        interpreters: p
            .labels
            .iter()
            .filter(|(w, l)| *w != val::ADJUDICATED && l.get(i).is_some_and(|x| !x.label.is_empty()))
            .count() as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas() {
        assert_eq!(fmt_area(1234567.4), "1,234,567");
        assert_eq!(fmt_area(-12345.0), "-12,345");
        assert_eq!(fmt_area(12.34), "12.3");
        assert_eq!(fmt_area(0.012345), "0.0123");
        assert_eq!(fmt_area(f64::NAN), "-");
        assert_eq!(fmt_area(999.6), "1,000");
    }
}
