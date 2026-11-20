//! Tune on samples (IDEIAS phase 62b): the window, opened from the window of a tool that gives change dates, that
//! searches the tool's parameters on reference points (janus_zeit::tune is the engine). The reference is the
//! calibration split of the open validation or interpretation project, or a vector layer with a date field (empty =
//! no change); the points' series are read once at full resolution, one set of parameters is timed for the estimate,
//! and the search runs as a task (Tools > Tasks) that can be cancelled. The result is a table of the sets, the
//! omission x commission scatter with its Pareto front, the partial rank correlation of each parameter, and the
//! k-fold check of the best set's optimism; selecting a set puts its parameters in the tool's window (the chart and
//! the pins fit with them at once) and "Use these parameters" keeps them, to be written in the provenance of the
//! runs that use them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, ComboBox, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Line, MarkerShape, Plot, PlotPoint, PlotPoints, Points};
use janus_cube::{CubeInfo, CubeReader};
use janus_geo::vectors::{Kind, Placer, VectorData, read_attributes};
use janus_validation as val;
use janus_zeit::python::{TunePointAt, TuneScriptSet, TuneScriptSpec};
use janus_zeit::tune::{self, Optimism, ScoreKind, ScoreOpts, SetKind, TuneConfig, TuneParam, TunePoint, TuneResult};
use janus_zeit::{PixelInput, Tool, manifest};
use rayon::prelude::*;
use serde_json::{Value, json};
use val::{CalibrationSplit, Project};

use crate::app::State;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus, format_duration, pixel_input, reads_bands, tool_bands};

/// The task's name in Tools > Tasks.
pub(crate) const JOB_TOOL: &str = "tune";

/// A section's title; a tour of the guide's screenshots (`shots_tune.rs`) can ask the window to scroll to it.
fn section(ui: &mut Ui, t: &TuneUi, title: &str) {
    crate::panels::section(ui, title);
    if t.scroll_to == Some(title) {
        ui.scroll_to_cursor(Some(egui::Align::TOP));
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The reference: dates, labels, weights (no window, no series: tested alone)
// ---------------------------------------------------------------------------------------------------------------

/// A reference point before it is placed on the series.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RefPoint {
    pub x: f64,
    pub y: f64,
    /// the year of the change (None: no change)
    pub reference: Option<f64>,
    /// the project's stratum (its place in the strata), for the stratified weight
    pub stratum: Option<usize>,
}

/// What became of the candidate points.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct RefStats {
    /// the candidates (calibration points, or the layer's features)
    pub candidates: usize,
    pub with_change: usize,
    pub no_change: usize,
    /// not labelled, in disagreement, or labelled with what the mapping leaves out
    pub left_out: usize,
    /// a change without a date
    pub no_date: usize,
    /// a date that is not one (a layer's field)
    pub bad_date: usize,
    pub first_bad: String,
    /// where the series has no pixel
    pub outside: usize,
}

/// A date as text to a decimal year (a calendar year is its integer part): "2018", "2018.4", "2018-05", "2018-05-12",
/// "2018/05/12", "2018-05-12T10:00:00" or "20180512". Empty (and "0") is no change: Ok(None).
pub(crate) fn parse_date(text: &str) -> Result<Option<f64>, String> {
    let t = text.trim();
    if t.is_empty() || t == "0" {
        return Ok(None);
    }
    let bad = || format!("'{t}' is not a date");
    let year_ok = |y: f64| (1900.0..2200.0).contains(&y);
    // a decimal year: one dot and no other separator
    if t.matches('.').count() == 1 && !t.contains(['-', '/', 'T', ' ', ':']) {
        let v: f64 = t.parse().map_err(|_| bad())?;
        return if year_ok(v) { Ok(Some(v)) } else { Err(bad()) };
    }
    let parts: Vec<&str> = t.split(['-', '/', '.', 'T', ' ', ':']).filter(|s| !s.is_empty()).collect();
    let digits = |s: &str| s.chars().all(|c| c.is_ascii_digit());
    let (y, m, d) = match parts.as_slice() {
        [p] if p.len() == 8 && digits(p) => (&p[..4], &p[4..6], &p[6..8]),
        [p] if p.len() == 4 && digits(p) => (*p, "1", "1"),
        [y, m] => (*y, *m, "1"),
        [y, m, d, ..] => (*y, *m, *d),
        _ => return Err(bad()),
    };
    if y.len() != 4 || !digits(y) || !digits(m) || !digits(d) {
        return Err(bad());
    }
    let (y, m, d): (f64, f64, f64) =
        (y.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?, d.parse().map_err(|_| bad())?);
    if !year_ok(y) || !(1.0..=12.0).contains(&m) || !(1.0..=31.0).contains(&d) {
        return Err(bad());
    }
    Ok(Some(y + (m - 1.0) / 12.0 + (d - 1.0) / 372.0))
}

/// What a vector layer's date field says of each feature (`dates`, as text): the year of the change, or none.
/// `places[i]`: the feature has a geometry. The features with a text that is not a date are left out (counted, the
/// first one named).
pub(crate) fn layer_reference(places: &[Option<(f64, f64)>], dates: &[String]) -> (Vec<RefPoint>, RefStats) {
    let mut stats = RefStats { candidates: places.len(), ..Default::default() };
    let mut out = Vec::new();
    for (p, d) in places.iter().zip(dates) {
        let Some((x, y)) = *p else {
            stats.left_out += 1;
            continue;
        };
        match parse_date(d) {
            Ok(reference) => {
                if reference.is_some() {
                    stats.with_change += 1;
                } else {
                    stats.no_change += 1;
                }
                out.push(RefPoint { x, y, reference, stratum: None });
            }
            Err(e) => {
                stats.bad_date += 1;
                if stats.first_bad.is_empty() {
                    stats.first_bad = e;
                }
            }
        }
    }
    (out, stats)
}

/// What a label means to the tuning, from the project's translation of its labels: "change", "no change", or (any
/// other text, none) left out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Meaning {
    Change,
    NoChange,
    LeftOut,
}

pub(crate) fn meaning(mapping: &BTreeMap<String, String>, label: &str) -> Meaning {
    match mapping.get(label).map(String::as_str) {
        Some(val::CHANGE) => Meaning::Change,
        Some(val::NO_CHANGE) => Meaning::NoChange,
        _ => Meaning::LeftOut,
    }
}

/// The translation of a project's labels to change / no change that the tuning starts from: the first map of events
/// it assesses, over the guess from the names of the labels.
pub(crate) fn default_mapping(p: &Project) -> BTreeMap<String, String> {
    val::reference_mapping(&p.maps, &p.label_set)
}

/// The reference of the project's points `idx` (indices into its points): the resolved label of each (the
/// adjudicated one, else the interpreters' when they agree) read with `mapping`; a point with a change label takes the
/// year of its earliest change (the label's date and its events'), one without a date is left out, and so is a point
/// without a label, in disagreement, or with a label the mapping leaves out.
pub(crate) fn project_reference(
    p: &Project,
    idx: &[usize],
    mapping: &BTreeMap<String, String>,
) -> (Vec<RefPoint>, RefStats) {
    let labels = val::for_mapping(&p.labels, &p.years, mapping);
    let mut stats = RefStats { candidates: idx.len(), ..Default::default() };
    let mut out = Vec::new();
    for &i in idx {
        let q = &p.points[i];
        let (Some(l), _) = val::resolve(&labels, i) else {
            stats.left_out += 1;
            continue;
        };
        let events = l.events.iter().map(|e| (e.label.as_str(), e.date.as_str()));
        let mut changes = std::iter::once((l.label.as_str(), l.date.as_str()))
            .chain(events)
            .filter(|(lab, _)| meaning(mapping, lab) == Meaning::Change);
        let reference = if let Some(first) = changes.next() {
            let years: Vec<i32> =
                std::iter::once(first).chain(changes).map(|(_, d)| val::year_of(d)).filter(|&y| y != 0).collect();
            let Some(&y) = years.iter().min() else {
                stats.no_date += 1;
                continue;
            };
            stats.with_change += 1;
            Some(f64::from(y))
        } else if meaning(mapping, &l.label) == Meaning::NoChange {
            stats.no_change += 1;
            None
        } else {
            stats.left_out += 1;
            continue;
        };
        out.push(RefPoint { x: q.x, y: q.y, reference, stratum: p.strata.index_of_code(q.stratum) });
    }
    (out, stats)
}

/// Why the accuracy points cannot be the reference: they estimate the accuracy.
pub(crate) fn accuracy_refusal(p: &Project) -> String {
    val::calibration_points(p).err().unwrap_or_else(|| {
        "the accuracy points cannot calibrate: they estimate the accuracy, and a tool tuned on them would make it \
         optimistic; use the calibration points, or another set of points"
            .into()
    })
}

/// The weight of each point when the scores are stratified: its stratum's area over the points of the stratum (what
/// each point stands for); a point without a stratum counts 1.
pub(crate) fn set_weights(points: &mut [TunePoint], area: &[f64]) {
    let mut n = vec![0usize; area.len()];
    for p in points.iter() {
        if let Some(h) = p.stratum.filter(|&h| h < n.len()) {
            n[h] += 1;
        }
    }
    for p in points.iter_mut() {
        p.weight = match p.stratum {
            Some(h) if h < n.len() => area[h] / n[h].max(1) as f64,
            _ => 1.0,
        };
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The state
// ---------------------------------------------------------------------------------------------------------------

/// Where the points come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Source {
    /// the open validation or interpretation project's calibration points
    #[default]
    Project,
    /// a vector layer (by its id) with a date field
    Layer(u32),
}

/// A parameter that can vary: whether it does, and its range (or the options of an enum) as the window edits it.
#[derive(Clone, Debug)]
pub(crate) struct VaryRow {
    pub id: String,
    pub label: String,
    pub on: bool,
    /// the file's range, to go back to
    pub spec: TuneParam,
    pub min: f64,
    pub max: f64,
    /// an enum: each option and whether the search picks it
    pub choices: Vec<(String, bool)>,
}

impl VaryRow {
    fn new(spec: &TuneParam, tool: &Tool) -> VaryRow {
        let label = tool.params.iter().find(|p| p.id == spec.param).map_or(spec.param.clone(), |p| p.label.clone());
        VaryRow::from_spec(spec, label)
    }

    fn from_spec(spec: &TuneParam, label: String) -> VaryRow {
        VaryRow {
            id: spec.param.clone(),
            label,
            on: spec.default_on,
            spec: spec.clone(),
            min: spec.min.unwrap_or(0.0),
            max: spec.max.unwrap_or(1.0),
            choices: spec.choices.iter().flatten().map(|c| (c.clone(), true)).collect(),
        }
    }

    /// The parameter as edited, for the search (None when it is as the file has it).
    pub(crate) fn edited(&self) -> Option<TuneParam> {
        let mut p = self.spec.clone();
        if p.choices.is_some() {
            let chosen: Vec<String> = self.choices.iter().filter(|c| c.1).map(|c| c.0.clone()).collect();
            // none chosen: all of them (nothing to search otherwise)
            p.choices =
                Some(if chosen.is_empty() { self.choices.iter().map(|c| c.0.clone()).collect() } else { chosen });
        } else {
            let lo = if p.scale == tune::Scale::Log { self.min.max(f64::MIN_POSITIVE) } else { self.min };
            (p.min, p.max) = (Some(lo), Some(self.max.max(lo)));
        }
        (p != self.spec).then_some(p)
    }

    /// The row's range or options back to the file's.
    fn reset(&mut self) {
        *self = VaryRow { on: self.on, ..VaryRow::from_spec(&self.spec, self.label.clone()) };
    }
}

/// The points read for a reference, with the series of each, and the time one set takes on them.
#[derive(Clone)]
pub(crate) struct Prepared {
    pub key: String,
    pub points: Arc<Vec<TunePoint>>,
    /// each point's column and row of the series' pixels, in the points' order (for the Python of the search)
    pub places: Arc<Vec<(usize, usize)>>,
    /// the series (`CubeInfo::id`) the points were read from
    pub series_id: u64,
    pub ctx: Arc<PixelInput>,
    /// seconds one set takes on all the points on one thread
    pub sec_per_set: f64,
    pub stats: RefStats,
    /// what the reference is: its text (the result's header), its record (the provenance's)
    pub text: String,
    pub desc: Value,
    pub read_seconds: f64,
}

/// A thread's answer.
type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// The reading of the points under way.
struct Reading {
    key: String,
    out: Slot<Prepared>,
}

/// The search under way.
struct Running {
    status: Arc<Mutex<JobStatus>>,
    progress: Arc<AtomicUsize>,
    cancel: Arc<AtomicBool>,
    total: usize,
    out: Arc<Mutex<Option<Done>>>,
}

/// A search's result with what is computed from it.
pub(crate) struct Done {
    pub cfg: TuneConfig,
    pub result: TuneResult,
    pub points: Arc<Vec<TunePoint>>,
    /// what `Prepared` says of the points' places and series, and the dates the search ran on
    pub places: Arc<Vec<(usize, usize)>>,
    pub series_id: u64,
    pub ctx: Arc<PixelInput>,
    /// best first
    pub ranked: Vec<usize>,
    pub pareto: Vec<usize>,
    /// the partial rank correlation of each varied parameter with the dated f1
    pub prcc: Vec<f64>,
    pub optimism: Option<Optimism>,
    pub reference: String,
    pub desc: Value,
    pub seconds: f64,
}

/// Parameters kept ("Use these parameters") and what to say of them in the provenance of the runs that use them.
pub(crate) struct Kept {
    tool: String,
    params: BTreeMap<String, Value>,
    entry: Value,
    /// the Python loop that redoes the search (written beside the provenance of the runs that use the parameters)
    script: Option<String>,
}

#[derive(Default)]
pub struct TuneUi {
    pub open: bool,
    pub(crate) tool: String,
    pub(crate) source: Source,
    /// the layer's date field
    pub(crate) field: String,
    /// the project's labels as change / no change / left out
    pub(crate) mapping: BTreeMap<String, String>,
    mapping_for: String,
    /// the accuracy points as the reference: refused
    pub(crate) use_accuracy: bool,
    pub(crate) split_fraction: f32,
    pub(crate) split_seed: u64,
    pub(crate) window: (i32, i32),
    window_for: u64,
    pub(crate) tolerance: i32,
    pub(crate) n_sets: usize,
    pub(crate) seed: u64,
    pub(crate) weighted: bool,
    pub(crate) rows: Vec<VaryRow>,
    pub(crate) prepared: Option<Prepared>,
    reading: Option<Reading>,
    /// the key of a reading that failed, and why
    pub(crate) failed: Option<(String, String)>,
    running: Option<Running>,
    pub(crate) done: Option<Done>,
    /// the set tried on the chart (an index of the result's sets)
    pub(crate) selected: Option<usize>,
    /// the tool's parameters before a set was tried (the ones to go back to)
    before: Option<BTreeMap<String, Value>>,
    pub(crate) kept: Option<Kept>,
    pub(crate) error: String,
    pub(crate) message: String,
    /// for the guide's screenshots (`--shots tune`): the section the window scrolls to, and where it is placed
    pub(crate) scroll_to: Option<&'static str>,
    pub(crate) pos: Option<egui::Pos2>,
}

impl TuneUi {
    /// What to add to the provenance of a run of `tool` with `params`: the calibration they came from (None when
    /// they were not kept from a tuning, or were edited since).
    pub(crate) fn calibration_entry(&self, tool: &str, params: &BTreeMap<String, Value>) -> Option<Value> {
        self.kept.as_ref().filter(|k| k.tool == tool && k.params == *params).map(|k| k.entry.clone())
    }

    /// The Python of the search the parameters were kept from (see `calibration_entry`), when it could be written.
    pub(crate) fn calibration_python(&self, tool: &str, params: &BTreeMap<String, Value>) -> Option<String> {
        self.kept.as_ref().filter(|k| k.tool == tool && k.params == *params).and_then(|k| k.script.clone())
    }

    pub(crate) fn running(&self) -> bool {
        self.running.is_some()
    }

    /// A window state that asks for `source`'s points (the accuracy ones when `use_accuracy`): for what stops it.
    pub(crate) fn asking(source: Source, use_accuracy: bool) -> TuneUi {
        TuneUi { source, use_accuracy, ..Default::default() }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The search's result
// ---------------------------------------------------------------------------------------------------------------

fn fmt_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() => n.to_string(),
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            let s = format!("{x:.4}");
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        }
        other => other.to_string(),
    }
}

fn pct(v: f64) -> String {
    if v.is_finite() { format!("{:.1}%", 100.0 * v) } else { "-".into() }
}

fn num3(v: f64) -> String {
    if v.is_finite() { format!("{v:.3}") } else { "-".into() }
}

pub(crate) fn scores_json(s: &tune::Scores) -> Value {
    json!({
        "omission": s.omission, "commission": s.commission, "f1": s.f1, "f1_dated": s.f1_dated,
        "date_agreement": s.date_agreement, "mean_abs_error": s.mean_abs_error, "true_positives": s.tp,
        "omitted": s.fn_, "false_changes": s.fp,
    })
}

impl Done {
    pub(crate) fn new(
        cfg: TuneConfig,
        result: TuneResult,
        points: Arc<Vec<TunePoint>>,
        p: &Prepared,
        seconds: f64,
    ) -> Done {
        let ranked = result.ranked();
        let pareto = result.pareto();
        let prcc = result.sensitivity(ScoreKind::F1Dated);
        let optimism = result.optimism(&points, 5, cfg.seed);
        Done {
            cfg,
            result,
            points,
            places: p.places.clone(),
            series_id: p.series_id,
            ctx: p.ctx.clone(),
            ranked,
            pareto,
            prcc,
            optimism,
            reference: p.text.clone(),
            desc: p.desc.clone(),
            seconds,
        }
    }

    /// The set's parameters by id.
    pub(crate) fn values(&self, k: usize) -> BTreeMap<String, Value> {
        self.result.vary.iter().cloned().zip(self.result.sets[k].values.iter().cloned()).collect()
    }

    /// A set's name in the table: "default", "current" or its rank.
    pub(crate) fn name(&self, k: usize) -> String {
        match self.result.sets[k].kind {
            SetKind::Default => "default".into(),
            SetKind::Current => "current".into(),
            SetKind::Sampled => format!("set {}", k + 1),
        }
    }

    /// What the provenance of a run that uses set `k` says of the calibration.
    pub(crate) fn entry(&self, k: usize) -> Value {
        let o = &self.result.opts;
        let set = &self.result.sets[k];
        let rank = self.ranked.iter().position(|&i| i == k).map(|r| r + 1);
        json!({
            "reference": self.desc,
            "points": self.points.len(),
            "changes_in_window": self.result.n_changes,
            "year_window": [o.window.0, o.window.1],
            "tolerance_years": o.tolerance,
            "weighted": o.weighted,
            "sets": self.cfg.n_sets,
            "seed": self.cfg.seed.to_string(),
            "varied": self.result.vary,
            "chosen": {
                "set": self.name(k), "rank": rank, "params": self.values(k),
            },
            "scores": scores_json(&set.scores),
            "held_out_f1": self.optimism.as_ref().map(|m| m.held_out.f1),
            "cancelled": self.result.cancelled,
        })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Reading the points (on a thread)
// ---------------------------------------------------------------------------------------------------------------

/// The candidates of a reference, once known (the thread reads the layer's dates).
pub(crate) enum Refs {
    Project { crs: Box<VectorData>, points: Vec<RefPoint>, stats: RefStats, areas: Vec<f64> },
    Layer { data: Arc<VectorData>, field: usize },
}

/// What the thread reads the points for.
pub(crate) struct Wanted {
    pub(crate) key: String,
    pub(crate) tool: &'static Tool,
    pub(crate) info: Arc<CubeInfo>,
    pub(crate) roles: BTreeMap<String, i32>,
    pub(crate) refs: Refs,
    pub(crate) text: String,
    pub(crate) desc: Value,
    pub(crate) base: BTreeMap<String, Value>,
    pub(crate) window: (i32, i32),
    pub(crate) threads: usize,
    pub(crate) map_t: usize,
}

/// The series of the point at pixel (x, y) as a pixel run gets it (the worker's reading in zeit.rs).
fn read_point(
    reader: &mut CubeReader,
    info: &CubeInfo,
    tool: &Tool,
    roles: &BTreeMap<String, i32>,
    x: usize,
    y: usize,
) -> TunePoint {
    let n = info.t();
    let finite = |v: Option<f32>| v.map_or(f64::NAN, |v| if v.is_finite() { f64::from(v) } else { f64::NAN });
    let values: Vec<f64> = (0..n).map(|t| finite(reader.read_pixel(t, x, y))).collect();
    let mut p = TunePoint { values, weight: 1.0, ..Default::default() };
    if reads_bands(tool) {
        for (role, band) in tool_bands(tool, info, roles) {
            let v: Vec<f64> = (0..n).map(|t| finite(reader.read_band_pixel(t, band, x, y, false))).collect();
            p.bands.insert(role, v);
        }
        if info.sel.qa_band > 0 {
            p.qa = Some((0..n).map(|t| finite(reader.read_band_pixel(t, info.sel.qa_band, x, y, true))).collect());
        }
    }
    p
}

pub(crate) fn read_points(w: Wanted) -> Result<Prepared, String> {
    let t0 = Instant::now();
    let (crs, refs, mut stats, areas) = match w.refs {
        Refs::Project { crs, points, stats, areas } => (*crs, points, stats, areas),
        Refs::Layer { data, field } => {
            let rows = if data.has_values() {
                None
            } else {
                Some(read_attributes(&data, &AtomicUsize::new(0), &AtomicBool::new(false))?)
            };
            let dates: Vec<String> = data
                .features
                .iter()
                .enumerate()
                .map(|(i, f)| match &rows {
                    Some(r) => r[i].get(field).cloned().unwrap_or_default(),
                    None => f.values.get(field).cloned().unwrap_or_default(),
                })
                .collect();
            let places: Vec<Option<(f64, f64)>> =
                data.features.iter().map(|f| (f.kind != Kind::None).then_some((f.cx, f.cy))).collect();
            let (points, stats) = layer_reference(&places, &dates);
            let crs = VectorData {
                crs_wkt: data.crs_wkt.clone(),
                crs_name: data.crs_name.clone(),
                lon_lat_assumed: data.lon_lat_assumed,
                ..Default::default()
            };
            (crs, points, stats, Vec::new())
        }
    };
    // onto the series' pixels
    let placer = Placer::to_pixels(&crs, &w.info.crs_wkt, w.info.geo_transform)?;
    let (mut xs, mut ys): (Vec<f64>, Vec<f64>) = refs.iter().map(|p| (p.x, p.y)).unzip();
    let ok = placer.to_pixel_many(&mut xs, &mut ys);
    let (width, height) = (w.info.width as f64, w.info.height as f64);
    let placed: Vec<(usize, usize, usize)> = (0..refs.len())
        .filter(|&i| ok[i] && xs[i] >= 0.0 && ys[i] >= 0.0 && xs[i] < width && ys[i] < height)
        .map(|i| (i, xs[i] as usize, ys[i] as usize))
        .collect();
    stats.outside = refs.len() - placed.len();
    if placed.is_empty() {
        return Err("none of the points falls on the series".into());
    }
    // the series, in parallel (a reader for each thread)
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(w.threads.max(1))
        .build()
        .map_err(|e| format!("cannot start the threads: {e}"))?;
    let roles = &w.roles;
    let (info, tool) = (&w.info, w.tool);
    let mut points: Vec<TunePoint> = pool.install(|| {
        placed
            .par_iter()
            .map_init(
                || CubeReader::new(info.clone()),
                |reader, &(i, x, y)| {
                    let mut p = read_point(reader, info, tool, roles, x, y);
                    (p.reference, p.stratum) = (refs[i].reference, refs[i].stratum);
                    p
                },
            )
            .collect()
    });
    if !areas.is_empty() {
        set_weights(&mut points, &areas);
    }
    let ctx = pixel_input(&w.info, &w.tool.id, &[], w.map_t, &w.roles);
    let mut cfg = TuneConfig::new(&w.tool.id, w.window);
    cfg.base = w.base;
    let sec_per_set = tune::time_one_set(&cfg, &ctx, &points)?;
    Ok(Prepared {
        key: w.key,
        points: Arc::new(points),
        places: Arc::new(placed.iter().map(|&(_, x, y)| (x, y)).collect()),
        series_id: w.info.id,
        ctx: Arc::new(ctx),
        sec_per_set,
        stats,
        text: w.text,
        desc: w.desc,
        read_seconds: t0.elapsed().as_secs_f64(),
    })
}

// ---------------------------------------------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------------------------------------------

impl State {
    /// Opens the window for a tool of the tune list (its window's button).
    pub(crate) fn open_tune(&mut self, tool: &Tool) {
        let mut t = std::mem::take(&mut self.tune);
        self.tune_open_for(&mut t, tool);
        self.tune = t;
    }

    fn tune_open_for(&mut self, t: &mut TuneUi, tool: &Tool) {
        let Some(spec) = tune::spec(&tool.id) else { return };
        if t.tool != tool.id || t.rows.is_empty() {
            t.rows = spec.params.iter().map(|p| VaryRow::new(p, tool)).collect();
            // another tool: its results are not this one's
            if t.tool != tool.id {
                self.tune_back(t);
                (t.done, t.selected, t.prepared, t.failed, t.reading, t.error) =
                    (None, None, None, None, None, String::new());
            }
            t.tool = tool.id.clone();
        }
        if t.n_sets == 0 {
            (t.n_sets, t.seed, t.tolerance, t.split_fraction, t.split_seed) = (200, 1, 1, 0.3, 42);
        }
        if let Some(s) = &self.session
            && t.window_for != s.info.id
        {
            t.window_for = s.info.id;
            let n = s.info.t();
            let y = |i: usize| s.info.decimal_year(i).floor() as i32;
            t.window = (y(0).min(y(n.saturating_sub(1))), y(0).max(y(n.saturating_sub(1))));
        }
        if !t.open {
            // a project if one is open, else the first layer of points
            let layer = self.vectors.iter().find(|v| !self.is_project_vector(v.id) && !v.data.features.is_empty());
            t.source = match (&self.val.project, layer) {
                (None, Some(v)) => Source::Layer(v.id),
                _ => Source::Project,
            };
        }
        t.open = true;
    }

    /// Closes the window: a set tried but not kept goes back to the parameters before it.
    fn tune_close(&mut self, t: &mut TuneUi) {
        self.tune_back(t);
        t.open = false;
    }

    /// The tool's parameters back to what they were before a set was tried.
    fn tune_back(&mut self, t: &mut TuneUi) {
        let Some(before) = t.before.take() else { return };
        t.selected = None;
        if let Some(tool) = manifest().tool(&t.tool) {
            *self.zeit.params(tool) = before;
            self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
        }
    }

    /// Tries set `k` of the result: its parameters go into the tool's window, so the chart and the pins fit with them.
    fn tune_select(&mut self, t: &mut TuneUi, done: &Done, k: usize) {
        let Some(tool) = manifest().tool(&t.tool) else { return };
        if k >= done.result.sets.len() {
            return;
        }
        let values = done.values(k);
        let params = self.zeit.params(tool);
        if t.before.is_none() {
            t.before = Some(params.clone());
        }
        params.extend(values);
        self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
        t.selected = Some(k);
    }

    /// Keeps the set tried as the tool's parameters.
    fn tune_keep(&mut self, t: &mut TuneUi, done: &Done) {
        let Some(k) = t.selected else { return };
        let Some(tool) = manifest().tool(&t.tool) else { return };
        let params = self.zeit.params(tool).clone();
        let mut entry = done.entry(k);
        let script = match self.tune_python(done) {
            Ok(code) => Some(code),
            Err(why) => {
                entry["python_note"] = json!(why);
                None
            }
        };
        t.kept = Some(Kept { tool: tool.id.clone(), params, entry, script });
        t.before = None;
        t.message =
            format!("The parameters of {} are kept; the runs that use them say so in their provenance.", done.name(k));
    }

    /// Why the Python of the search cannot be written now, if it cannot (what the button says when it is off).
    fn tune_python_blocker(&self, done: &Done) -> Option<String> {
        let Some(s) = &self.session else { return Some("Open the series the search ran on first.".into()) };
        if done.series_id != s.info.id {
            return Some("The search ran on another series than the one open.".into());
        }
        if done.places.len() != done.points.len() {
            return Some("The points' places on the series are not known.".into());
        }
        None
    }

    /// The Python that redoes the search of `done` with Zeit (`janus_zeit::python::tune_script`): the points' series
    /// read as a pin's, the sets written out, the scores and the table; or why it cannot be written.
    pub(crate) fn tune_python(&mut self, done: &Done) -> Result<String, String> {
        // (cheap checks first: `tune_python_blocker`)
        if let Some(why) = self.tune_python_blocker(done) {
            return Err(why);
        }
        let info = self.session.as_ref().map(|s| s.info.clone()).ok_or("open the series the search ran on first")?;
        let name = self.layers.get(self.active).map_or(info.description.clone(), |l| l.name.clone());
        let points: Vec<TunePointAt> = done
            .points
            .iter()
            .zip(done.places.iter())
            .map(|(p, &(col, row))| TunePointAt { col, row, reference: p.reference, weight: p.weight })
            .collect();
        let sets: Vec<TuneScriptSet> = (0..done.result.sets.len())
            .map(|k| TuneScriptSet {
                name: done.name(k),
                values: done.result.sets[k].values.clone(),
                f1_dated: Some(done.result.sets[k].scores.f1_dated).filter(|v| v.is_finite()),
            })
            .collect();
        let roles = self.active_roles();
        let path = |p: &str| crate::provenance::absolute(p);
        janus_zeit::python::tune_script(&TuneScriptSpec {
            info: &info,
            version: env!("CARGO_PKG_VERSION"),
            name: &name,
            tool: &done.cfg.tool,
            base: &done.cfg.base,
            input: &done.ctx,
            vary: &done.result.vary,
            sets: &sets,
            points: &points,
            opts: done.result.opts,
            reference: &done.reference,
            roles: &roles,
            path: &path,
        })
    }

    pub(crate) fn tune_select_now(&mut self, k: usize) {
        let mut t = std::mem::take(&mut self.tune);
        if let Some(d) = t.done.take() {
            self.tune_select(&mut t, &d, k);
            t.done = Some(d);
        }
        self.tune = t;
    }

    /// The calibration split of the open project drawn (the window's "Make a calibration split" button): for the
    /// guide's screenshots.
    pub(crate) fn tune_split_now(&mut self) {
        let (fraction, seed) = (f64::from(self.tune.split_fraction), self.tune.split_seed);
        if let Some(p) = self.val.project.as_mut() {
            p.calibration = Some(CalibrationSplit::draw(&p.points, fraction, seed));
            self.val.meta_dirty = true;
            self.val.version += 1;
        }
    }

    pub(crate) fn tune_keep_now(&mut self) {
        let mut t = std::mem::take(&mut self.tune);
        if let Some(d) = t.done.take() {
            self.tune_keep(&mut t, &d);
            t.done = Some(d);
        }
        self.tune = t;
    }

    /// Starts the search on the points read (the Run button).
    pub(crate) fn tune_run_now(&mut self, tool: &'static Tool, ctx: &egui::Context) {
        let mut t = std::mem::take(&mut self.tune);
        self.tune_start_run(&mut t, tool, ctx);
        self.tune = t;
    }

    pub(crate) fn tune_back_now(&mut self) {
        let mut t = std::mem::take(&mut self.tune);
        self.tune_back(&mut t);
        self.tune = t;
    }

    pub(crate) fn tune_close_now(&mut self) {
        let mut t = std::mem::take(&mut self.tune);
        self.tune_close(&mut t);
        self.tune = t;
    }

    /// What stops the reference from being used now, if anything.
    pub(crate) fn tune_problem(&self, t: &TuneUi) -> Option<String> {
        match t.source {
            Source::Project => {
                let Some(p) = &self.val.project else {
                    return Some("No validation or interpretation project is open.".into());
                };
                if t.use_accuracy {
                    return Some(accuracy_refusal(p));
                }
                val::calibration_points(p).err()
            }
            Source::Layer(id) => {
                let Some(i) = self.vector_index(id) else { return Some("Choose a vector layer.".into()) };
                if !self.vectors[i].data.fields.contains(&t.field) {
                    return Some("Choose the layer's date field.".into());
                }
                None
            }
        }
    }

    /// Everything the points read depends on, as one text: a reading is current while it is the same.
    pub(crate) fn tune_key(&self, t: &TuneUi, info: &CubeInfo, roles: &BTreeMap<String, i32>) -> String {
        let src = match t.source {
            Source::Project => self.val.project.as_ref().map_or("none".into(), |p| {
                let c = p.calibration.as_ref().map(|c| (c.fraction.to_bits(), c.seed, c.ids.len()));
                format!("p{}:{}:{c:?}:{}", p.id, self.val.version, json!(t.mapping))
            }),
            Source::Layer(id) => {
                let at = self.vector_index(id).map_or(0, |i| Arc::as_ptr(&self.vectors[i].data) as usize);
                format!("l{id}:{at}:{}", t.field)
            }
        };
        format!("{}|{}|{roles:?}|{src}", info.id, t.tool)
    }

    /// Starts reading the points (a thread) for the current reference.
    fn tune_start_read(&mut self, t: &mut TuneUi, tool: &'static Tool, key: String, ctx: &egui::Context) {
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        let roles = self.active_roles();
        let (refs, text, desc) = match t.source {
            Source::Project => {
                let Some(p) = &self.val.project else { return };
                let idx = match val::calibration_points(p) {
                    Ok(i) => i,
                    Err(e) => {
                        t.failed = Some((key, e));
                        return;
                    }
                };
                let (points, stats) = project_reference(p, &idx, &t.mapping);
                let name = std::path::Path::new(&p.path)
                    .file_name()
                    .map_or(p.path.clone(), |n| n.to_string_lossy().into_owned());
                let c = p.calibration.clone().unwrap_or_default();
                let crs = Box::new(VectorData { crs_wkt: p.strata.grid.wkt.clone(), ..Default::default() });
                let areas = if t.weighted { p.strata.strata.iter().map(|s| s.area).collect() } else { Vec::new() };
                let desc = json!({
                    "kind": "project", "file": p.path, "calibration_points": idx.len(),
                    "split": { "fraction": c.fraction, "seed": c.seed.to_string(), "points": c.ids.len() },
                    "labels": t.mapping,
                });
                (
                    Refs::Project { crs, points, stats, areas },
                    format!("the calibration points of {name} ({} of {})", idx.len(), p.points.len()),
                    desc,
                )
            }
            Source::Layer(id) => {
                let Some(i) = self.vector_index(id) else { return };
                let v = &self.vectors[i];
                let Some(field) = v.data.fields.iter().position(|f| *f == t.field) else { return };
                let desc = json!({ "kind": "layer", "layer": v.name, "file": v.data.path, "field": t.field });
                (Refs::Layer { data: v.data.clone(), field }, format!("{}, field {}", v.name, t.field), desc)
            }
        };
        let wanted = Wanted {
            key: key.clone(),
            tool,
            info,
            roles,
            refs,
            text,
            desc,
            base: self.zeit.params(tool).clone(),
            window: t.window,
            threads: self.processing_threads().max(1),
            map_t: self.display.t,
        };
        let out: Slot<Prepared> = Arc::new(Mutex::new(None));
        let (o, c) = (out.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("tune-read".into()).spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_points(wanted)))
                .unwrap_or_else(|_| Err("the points could not be read".into()));
            *o.lock().unwrap() = Some(r);
            c.request_repaint();
        });
        match spawned {
            Ok(_) => t.reading = Some(Reading { key, out }),
            Err(e) => t.failed = Some((key, format!("could not start the reading: {e}"))),
        }
    }

    /// The search's configuration as the window has it.
    pub(crate) fn tune_config(&mut self, t: &TuneUi, tool: &Tool) -> TuneConfig {
        let mut cfg = TuneConfig::new(&tool.id, t.window);
        cfg.base = self.zeit.params(tool).clone();
        cfg.vary = t.rows.iter().filter(|r| r.on).map(|r| r.id.clone()).collect();
        cfg.ranges = t.rows.iter().filter(|r| r.on).filter_map(|r| r.edited().map(|p| (r.id.clone(), p))).collect();
        cfg.n_sets = t.n_sets;
        cfg.seed = t.seed;
        cfg.threads = self.processing_threads().max(1);
        cfg.opts =
            ScoreOpts { window: t.window, tolerance: t.tolerance, weighted: t.weighted && t.source == Source::Project };
        cfg
    }

    /// Starts the search on the points read (a task in Tools > Tasks).
    fn tune_start_run(&mut self, t: &mut TuneUi, tool: &'static Tool, ctx: &egui::Context) {
        let Some(prep) = t.prepared.clone() else { return };
        self.tune_back(t);
        let cfg = self.tune_config(t, tool);
        let total = cfg.n_sets + 2;
        let stem = format!("{}_{}", tool.id, crate::zeit::timestamp());
        let folder = self.results_dir().join("tune").join(stem);
        if let Err(e) = std::fs::create_dir_all(&folder) {
            t.error = format!("could not create {}: {e}", folder.display());
            return;
        }
        let log = JobLog { path: folder.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== Tune {} on samples\n{}\nreference: {}\npoints: {} ({} with a change in {} to {})\nsets: {} (seed {}), \
             threads: {}\nvarying: {}\ntolerance: {} years, weighted: {}\noutput: {}\n",
            tool.name,
            crate::zeit::timestamp_text(),
            prep.text,
            prep.points.len(),
            tune::count_changes(&prep.points, t.window),
            t.window.0,
            t.window.1,
            cfg.n_sets,
            cfg.seed,
            cfg.threads,
            cfg.vary.join(", "),
            t.tolerance,
            cfg.opts.weighted,
            folder.display()
        ));
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let (progress, cancel) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicBool::new(false)));
        let out: Arc<Mutex<Option<Done>>> = Arc::new(Mutex::new(None));
        let (pts, pctx, prep2) = (prep.points.clone(), prep.ctx.clone(), prep.clone());
        let (st, o, pr, cn, c, cfg2) =
            (status.clone(), out.clone(), progress.clone(), cancel.clone(), ctx.clone(), cfg.clone());
        std::thread::Builder::new()
            .name("tune".into())
            .spawn(move || {
                let t0 = Instant::now();
                let r =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| tune::run(&cfg2, &pctx, &pts, &cn, &pr)))
                        .unwrap_or_else(|_| Err("the search stopped on an internal error".into()));
                let seconds = t0.elapsed().as_secs_f64();
                let mut s = st.lock().unwrap();
                s.seconds = seconds;
                match r {
                    Ok(res) => {
                        let cancelled = res.cancelled;
                        log.line(&format!(
                            "{} sets ran in {seconds:.1} s{}",
                            res.sets.len(),
                            if cancelled { " (cancelled)" } else { "" }
                        ));
                        if let Some(b) = res.best() {
                            let b = &res.sets[b];
                            log.line(&format!(
                                "best: {:?}, f1 dated {:.3}, f1 {:.3}",
                                b.values, b.scores.f1_dated, b.scores.f1
                            ));
                        }
                        *o.lock().unwrap() = Some(Done::new(cfg2.clone(), res, pts.clone(), &prep2, seconds));
                        if cancelled {
                            s.state = JobState::Cancelled;
                        } else {
                            (s.state, s.progress) = (JobState::Done, 1.0);
                        }
                        log.line(if cancelled { "cancelled" } else { "done" });
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
            title: format!("Tune {} on samples ({} points, {} sets)", tool.name, prep.points.len(), cfg.n_sets),
            tool: JOB_TOOL.into(),
            cube: 0,
            output_dir: folder.to_string_lossy().into_owned(),
            log_path: folder.join("job.log").to_string_lossy().into_owned(),
            started: Instant::now(),
            status: status.clone(),
            cancel: cancel.clone(),
            // there is no map to take
            loaded: true,
        });
        self.zeit.show_tasks = true;
        t.error.clear();
        t.message.clear();
        t.running = Some(Running { status, progress, cancel, total, out });
    }

    /// Every frame: the points read, the search's progress and its result.
    fn pump_tune(&mut self, t: &mut TuneUi, ctx: &egui::Context) {
        if let Some(r) = &t.reading {
            let done = r.out.lock().unwrap().take();
            if let Some(res) = done {
                let key = r.key.clone();
                t.reading = None;
                match res {
                    Ok(p) => (t.prepared, t.failed) = (Some(p), None),
                    Err(e) => t.failed = Some((key, e)),
                }
            }
        }
        let Some(r) = &t.running else { return };
        let finished = {
            let mut s = r.status.lock().unwrap();
            if s.state == JobState::Running {
                let d = r.progress.load(Ordering::Relaxed);
                s.progress = (d as f64 / r.total.max(1) as f64).min(0.99);
                s.message = format!("{d} of {} sets", r.total);
            }
            (s.state != JobState::Running).then(|| (s.state, s.error.clone()))
        };
        let Some((state, error)) = finished else {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
            return;
        };
        let done = r.out.lock().unwrap().take();
        t.running = None;
        match done {
            Some(d) => {
                t.selected = None;
                t.message = if d.result.cancelled {
                    format!("Cancelled: {} sets ran.", d.result.sets.len())
                } else {
                    String::new()
                };
                t.done = Some(d);
            }
            None => t.error = if state == JobState::Failed { error } else { "The search was cancelled.".into() },
        }
    }

    /// Once a frame: the window, and the tuning's threads and tasks whether it is open or not.
    pub(crate) fn tune_window(&mut self, ctx: &egui::Context) {
        let mut t = std::mem::take(&mut self.tune);
        self.pump_tune(&mut t, ctx);
        if t.open {
            if let Some(tool) = manifest().tool(&t.tool) {
                let mut keep = true;
                let title = format!("Tune on samples - {}", tool.name);
                let mut window =
                    crate::panels::fitted_window(title, ctx, 560.0).id(egui::Id::new("tune window")).open(&mut keep);
                if let Some(pos) = t.pos {
                    window = window.current_pos(pos);
                }
                window.show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.tune_body(ui, &mut t, tool, ctx)));
                if !keep {
                    self.tune_close(&mut t);
                }
            } else {
                t.open = false;
            }
        }
        self.tune = t;
    }

    fn tune_body(&mut self, ui: &mut Ui, t: &mut TuneUi, tool: &'static Tool, ctx: &egui::Context) {
        let Some(spec) = tune::spec(&tool.id) else { return };
        crate::welcome::guide_button(ui, "tune");
        if t.scroll_to == Some("top") {
            ui.scroll_to_cursor(Some(egui::Align::TOP));
        }
        ui.weak(&spec.why);
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.colored_label(theme::error(), "No series is open.");
            return;
        };
        self.tune_reference_ui(ui, t);
        self.tune_search_ui(ui, t);
        let roles = self.active_roles();
        let key = self.tune_key(t, &info, &roles);
        let problem = self.tune_problem(t);
        self.tune_run_ui(ui, t, tool, &key, problem.as_deref(), ctx);
        if let Some(done) = t.done.take() {
            self.tune_results_ui(ui, t, &done);
            if t.done.is_none() {
                t.done = Some(done);
            }
        }
    }

    // --- the reference ---

    fn tune_reference_ui(&mut self, ui: &mut Ui, t: &mut TuneUi) {
        section(ui, t, "Reference");
        let layers: Vec<(u32, String)> = self
            .vectors
            .iter()
            .filter(|v| !self.is_project_vector(v.id) && !v.data.features.is_empty())
            .map(|v| (v.id, v.name.clone()))
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut t.source, Source::Project, "Project's calibration points").on_hover_text(
                "The points of the open validation or interpretation project that were drawn to calibrate (the \
                 rest estimate the accuracy)",
            );
            let layer_on = matches!(t.source, Source::Layer(_));
            if ui
                .add_enabled(!layers.is_empty(), egui::Button::selectable(layer_on, "Vector layer with dates"))
                .on_hover_text("A layer of points with a field of the change's date; empty = no change")
                .clicked()
                && !layer_on
            {
                t.source = Source::Layer(layers[0].0);
            }
        });
        match t.source {
            Source::Project => self.tune_project_ui(ui, t),
            Source::Layer(id) => {
                let mut pick = id;
                let name = layers.iter().find(|l| l.0 == id).map_or("(none)".to_string(), |l| l.1.clone());
                ui.horizontal(|ui| {
                    ui.label("Layer");
                    ComboBox::from_id_salt("tune layer")
                        .width(crate::fit::scaled(ui, 200.0))
                        .selected_text(name)
                        .show_ui(ui, |ui| {
                            for (lid, n) in &layers {
                                ui.selectable_value(&mut pick, *lid, n);
                            }
                        });
                });
                t.source = Source::Layer(pick);
                let fields: Vec<String> =
                    self.vector_index(pick).map(|i| self.vectors[i].data.fields.clone()).unwrap_or_default();
                if !fields.contains(&t.field) || pick != id {
                    // the likeliest field: one named like a date
                    t.field = fields
                        .iter()
                        .find(|f| {
                            let l = f.to_lowercase();
                            ["date", "year", "yod", "change"].iter().any(|w| l.contains(w))
                        })
                        .or(fields.first())
                        .cloned()
                        .unwrap_or_default();
                }
                ui.horizontal(|ui| {
                    ui.label("Date field");
                    ComboBox::from_id_salt("tune field")
                        .width(crate::fit::scaled(ui, 160.0))
                        .selected_text(&t.field)
                        .show_ui(ui, |ui| {
                            for f in &fields {
                                ui.selectable_value(&mut t.field, f.clone(), f);
                            }
                        });
                });
                ui.weak("A year or a date (2018, 2018-05-12); empty = no change.");
            }
        }
    }

    fn tune_project_ui(&mut self, ui: &mut Ui, t: &mut TuneUi) {
        let Some(p) = self.val.project.as_mut() else {
            ui.weak("No validation or interpretation project is open.");
            return;
        };
        let want = format!("{}:{}", p.id, p.label_set.join("\u{1f}"));
        if t.mapping_for != want {
            t.mapping_for = want;
            t.mapping = default_mapping(p);
        }
        // the split
        let mut draw = false;
        match &p.calibration {
            Some(c) => {
                ui.label(format!(
                    "{} of {} points calibrate ({:.0}% of each stratum, seed {}); the accuracy uses the rest.",
                    c.ids.len(),
                    p.points.len(),
                    100.0 * c.fraction,
                    c.seed
                ));
            }
            None => {
                ui.colored_label(
                    theme::warning(),
                    "The project has no calibration split: the points that tune a tool must not estimate the accuracy \
                     too.",
                );
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Split");
            let mut pc = (t.split_fraction * 100.0).round() as i32;
            if ui.add(egui::DragValue::new(&mut pc).range(5..=90).suffix("%")).changed() {
                t.split_fraction = pc as f32 / 100.0;
            }
            ui.label("seed");
            let mut seed = t.split_seed as i64;
            if ui.add(egui::DragValue::new(&mut seed).range(0..=i64::MAX)).changed() {
                t.split_seed = seed.max(0) as u64;
            }
            let label = if p.calibration.is_some() { "Draw again" } else { "Make a calibration split" };
            draw = ui
                .button(label)
                .on_hover_text(
                    "A seeded draw inside each stratum marks the points that calibrate; the accuracy uses the rest \
                     with the same stratum weights",
                )
                .clicked();
        });
        if draw {
            p.calibration = Some(CalibrationSplit::draw(&p.points, f64::from(t.split_fraction), t.split_seed));
            self.val.meta_dirty = true;
            self.val.version += 1;
        }
        ui.checkbox(&mut t.use_accuracy, "Use the accuracy points")
            .on_hover_text("Refused: the points that estimate the accuracy cannot tune the parameters too");
        // the labels
        let labels = self.val.project.as_ref().map(|p| p.label_set.clone()).unwrap_or_default();
        if !labels.is_empty() {
            ui.label("What each label means:");
            egui::Grid::new("tune labels").num_columns(2).show(ui, |ui| {
                for l in &labels {
                    ui.label(l);
                    let cur = t.mapping.get(l).cloned().unwrap_or_default();
                    let name = |s: &str| match s {
                        val::CHANGE => "change",
                        val::NO_CHANGE => "no change",
                        _ => "left out",
                    };
                    ComboBox::from_id_salt(("tune label", l)).selected_text(name(&cur)).show_ui(ui, |ui| {
                        for v in [val::CHANGE, val::NO_CHANGE, ""] {
                            if ui.selectable_label(cur == v, name(v)).clicked() {
                                t.mapping.insert(l.clone(), v.to_string());
                            }
                        }
                    });
                    ui.end_row();
                }
            });
        }
    }

    // --- the search ---

    fn tune_search_ui(&mut self, ui: &mut Ui, t: &mut TuneUi) {
        section(ui, t, "Search");
        ui.horizontal_wrapped(|ui| {
            ui.label("Years");
            ui.add(egui::DragValue::new(&mut t.window.0).range(1900..=2200));
            ui.label("to");
            ui.add(egui::DragValue::new(&mut t.window.1).range(1900..=2200));
            t.window.1 = t.window.1.max(t.window.0);
            ui.label("Dates within +-");
            ui.add(egui::DragValue::new(&mut t.tolerance).range(0..=10).suffix(" y"));
        });
        ui.horizontal_wrapped(|ui| {
            ui.label("Sets");
            ui.add(egui::DragValue::new(&mut t.n_sets).range(1..=5000));
            ui.label("seed");
            let mut seed = t.seed as i64;
            if ui.add(egui::DragValue::new(&mut seed).range(0..=i64::MAX)).changed() {
                t.seed = seed.max(0) as u64;
            }
            ui.add_enabled(
                t.source == Source::Project,
                egui::Checkbox::new(&mut t.weighted, "Weight by strata"),
            )
            .on_hover_text("Each point counts for its stratum's area over the points drawn in it (calibration, not an area estimate)");
        });
        ui.label("Parameters to vary:");
        egui::Grid::new("tune params").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            for r in t.rows.iter_mut() {
                let log = r.spec.scale == tune::Scale::Log && r.spec.choices.is_none();
                ui.checkbox(&mut r.on, &r.label).on_hover_text(format!(
                    "{}{}",
                    r.id,
                    if log { " (searched on a log scale)" } else { "" }
                ));
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(r.on, |ui| {
                        if r.spec.choices.is_some() {
                            for (name, on) in r.choices.iter_mut() {
                                ui.checkbox(on, name.as_str());
                            }
                        } else {
                            let lo = if log { 1e-9 } else { f64::MIN };
                            let speed = ((r.spec.max.unwrap_or(1.0) - r.spec.min.unwrap_or(0.0)) / 200.0).max(1e-6);
                            // a whole number's range in whole numbers
                            let (speed, decimals) = if r.spec.int { (speed.max(0.05), 0) } else { (speed, 4) };
                            ui.add(
                                egui::DragValue::new(&mut r.min)
                                    .speed(speed)
                                    .range(lo..=f64::MAX)
                                    .max_decimals(decimals),
                            );
                            ui.label("to");
                            ui.add(
                                egui::DragValue::new(&mut r.max)
                                    .speed(speed)
                                    .range(lo..=f64::MAX)
                                    .max_decimals(decimals),
                            );
                            r.max = r.max.max(r.min);
                        }
                        if r.edited().is_some()
                            && ui.small_button("Reset").on_hover_text("The range of the file").clicked()
                        {
                            r.reset();
                        }
                    });
                });
                ui.end_row();
            }
        });
    }

    // --- running ---

    fn tune_run_ui(
        &mut self,
        ui: &mut Ui,
        t: &mut TuneUi,
        tool: &'static Tool,
        key: &str,
        problem: Option<&str>,
        ctx: &egui::Context,
    ) {
        section(ui, t, "Run");
        // the points read for this reference: started when none is, or when it changed
        let current = t.prepared.as_ref().is_some_and(|p| p.key == key);
        let failed_now = t.failed.as_ref().is_some_and(|f| f.0 == key);
        if problem.is_none() && !current && !failed_now && t.reading.is_none() && t.running.is_none() {
            self.tune_start_read(t, tool, key.to_string(), ctx);
        }
        if let Some(r) = &t.reading {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if r.key == key { "Reading the points' series at full resolution..." } else { "Reading..." });
            });
        }
        if let Some(e) = problem {
            ui.colored_label(theme::warning(), e);
        }
        if let Some((_, e)) = t.failed.as_ref().filter(|f| f.0 == key) {
            ui.colored_label(theme::error(), e);
        }
        let n_changes;
        let mut estimate = None;
        if let Some(p) = t.prepared.as_ref().filter(|p| p.key == key) {
            n_changes = tune::count_changes(&p.points, t.window);
            let s = &p.stats;
            ui.label(format!(
                "{} points read in {:.1} s ({} with a change in {}-{}, {} without) from {}.",
                p.points.len(),
                p.read_seconds,
                n_changes,
                t.window.0,
                t.window.1,
                p.points.len().saturating_sub(n_changes),
                p.text
            ));
            let mut skipped = Vec::new();
            for (n, what) in [
                (s.left_out, "left out by their label"),
                (s.no_date, "of change without a date"),
                (s.bad_date, "with a date that is not one"),
                (s.outside, "off the series"),
            ] {
                if n > 0 {
                    skipped.push(format!("{n} {what}"));
                }
            }
            if !skipped.is_empty() {
                ui.weak(format!("Left out: {}.", skipped.join(", ")));
            }
            if !s.first_bad.is_empty() {
                ui.colored_label(
                    theme::warning(),
                    format!("The date field has text that is not a date: {}.", s.first_bad),
                );
            }
            if n_changes < tune::MIN_CHANGES {
                ui.colored_label(
                    theme::warning(),
                    format!(
                        "Only {n_changes} points with a change (fewer than {}): the scores and the choice of a set are \
                         unreliable.",
                        tune::MIN_CHANGES
                    ),
                );
            }
            let threads = self.processing_threads().max(1);
            estimate = Some((tune::estimate_seconds(p.sec_per_set, t.n_sets + 2, threads), threads));
        }
        ui.horizontal_wrapped(|ui| {
            let ready = estimate.is_some() && problem.is_none() && t.running.is_none() && t.rows.iter().any(|r| r.on);
            if ui.add_enabled(ready, egui::Button::new("Run")).clicked() {
                self.tune_start_run(t, tool, ctx);
            }
            if let Some((s, threads)) = estimate {
                ui.weak(format!("{} on {threads} threads", format_duration(s)));
            }
        });
        if let Some(r) = &t.running {
            let (progress, message) = {
                let s = r.status.lock().unwrap();
                (s.progress, s.message.clone())
            };
            let bar = egui::ProgressBar::new(progress as f32).text(message);
            let cancel = r.cancel.clone();
            crate::panels::progress_row(ui, bar, |ui| {
                if ui.button("Cancel").clicked() {
                    cancel.store(true, Ordering::Relaxed);
                }
            });
        }
        if !t.error.is_empty() {
            ui.colored_label(theme::error(), &t.error);
        }
        if !t.message.is_empty() {
            ui.weak(&t.message);
        }
    }

    // --- the result ---

    fn tune_results_ui(&mut self, ui: &mut Ui, t: &mut TuneUi, done: &Done) {
        let o = &done.result.opts;
        section(ui, t, "Results");
        ui.label(format!(
            "{} sets on {} points ({} with a change in {}-{}) from {}, in {}; dates within +-{} y.",
            done.result.sets.len(),
            done.points.len(),
            done.result.n_changes,
            o.window.0,
            o.window.1,
            done.reference,
            format_duration(done.seconds),
            o.tolerance
        ));
        if done.result.few_changes {
            ui.colored_label(
                theme::warning(),
                format!(
                    "Only {} points with a change: the scores are unreliable; collect more points before trusting a set.",
                    done.result.n_changes
                ),
            );
        }
        ui.weak("Click a row or a point to try its set: the tool's window, the chart and the pins take it.");
        let mut pick: Option<usize> = None;
        // the table
        let k = crate::fit::scale_of(ui.ctx());
        let row_h = (ui.text_style_height(&egui::TextStyle::Body) + 6.0).max(20.0);
        let mut table = TableBuilder::new(ui)
            .id_salt("tune table")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::auto().at_least(k * 40.0))
            // the table's height does not depend on how far down it is: the room left in the window shrinks to
            // nothing below a long reference section, and the rows would not be drawn
            .min_scrolled_height(((done.ranked.len() as f32 + 1.0) * row_h).min(k * 220.0))
            .max_scroll_height(k * 220.0);
        for _ in &done.result.vary {
            table = table.column(Column::auto().at_least(k * 40.0));
        }
        for _ in 0..6 {
            table = table.column(Column::auto().at_least(k * 40.0));
        }
        table
            .header(row_h, |mut h| {
                for name in ["Rank".to_string()].into_iter().chain(done.result.vary.iter().cloned()) {
                    h.col(|ui| {
                        ui.strong(name);
                    });
                }
                for (name, tip) in [
                    ("Omission", "Reference changes the map misses"),
                    ("Commission", "Map changes the reference does not have"),
                    ("F1", "Change found or not, whatever the date"),
                    ("F1 dated", "As F1, a change found at the wrong date counting as both an omission and a false change: the order of the table"),
                    ("Dates", "Of the changes found, the share whose dates agree within the tolerance"),
                    ("Error", "Mean absolute error of the date, in years"),
                ] {
                    h.col(|ui| {
                        ui.strong(name).on_hover_text(tip);
                    });
                }
            })
            .body(|body| {
                body.rows(row_h, done.ranked.len(), |mut row| {
                    let rank = row.index();
                    let s = done.ranked[rank];
                    let set = &done.result.sets[s];
                    row.set_selected(t.selected == Some(s));
                    row.col(|ui| {
                        let tag = match set.kind {
                            SetKind::Default => " default",
                            SetKind::Current => " current",
                            SetKind::Sampled => "",
                        };
                        ui.label(format!("{}{tag}", rank + 1));
                    });
                    for v in &set.values {
                        row.col(|ui| {
                            ui.label(fmt_value(v));
                        });
                    }
                    let sc = &set.scores;
                    for text in [
                        pct(sc.omission),
                        pct(sc.commission),
                        num3(sc.f1),
                        num3(sc.f1_dated),
                        pct(sc.date_agreement),
                        if sc.mean_abs_error.is_finite() { format!("{:.2}", sc.mean_abs_error) } else { "-".into() },
                    ] {
                        row.col(|ui| {
                            ui.label(text);
                        });
                    }
                    if row.response().clicked() {
                        pick = Some(s);
                    }
                });
            });
        // the scatter
        if let Some(s) = self.tune_scatter(ui, t, done) {
            pick = Some(s);
        }
        if let Some(s) = pick {
            self.tune_select(t, done, s);
        }
        // which parameters matter
        section(ui, t, "Which parameters matter");
        self.tune_prcc_ui(ui, done);
        // the optimism
        section(ui, t, "Optimism");
        match &done.optimism {
            Some(m) => {
                ui.label(format!(
                    "Best set on all the points: F1 {}, F1 dated {}, omission {}, commission {}.",
                    num3(m.in_sample.f1),
                    num3(m.in_sample.f1_dated),
                    pct(m.in_sample.omission),
                    pct(m.in_sample.commission)
                ));
                ui.label(format!(
                    "Held out ({} folds: chosen on the others, scored on the one left): F1 {}, F1 dated {}, omission {}, \
                     commission {}.",
                    m.k,
                    num3(m.held_out.f1),
                    num3(m.held_out.f1_dated),
                    pct(m.held_out.omission),
                    pct(m.held_out.commission)
                ));
                let gap = m.f1_gap();
                if gap.is_finite() {
                    let text = format!("The choice flatters the F1 by {gap:+.3}.");
                    if gap > 0.1 {
                        ui.colored_label(theme::warning(), text);
                    } else {
                        ui.label(text);
                    }
                }
            }
            None => {
                ui.weak("No set ran.");
            }
        }
        if done.result.few_changes {
            ui.colored_label(theme::warning(), "With so few changes the held-out score is itself very uncertain.");
        }
        // keep
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(t.selected.is_some(), egui::Button::new("Use these parameters"))
                .on_hover_text("Keep the set tried in the tool's window; the runs that use it record the calibration")
                .clicked()
            {
                self.tune_keep(t, done);
            }
            if ui
                .add_enabled(t.before.is_some(), egui::Button::new("Back to the current"))
                .on_hover_text("The tool's parameters as they were before a set was tried")
                .clicked()
            {
                self.tune_back(t);
            }
            // the script takes a moment to write (every set's calls): only on the click
            let blocked = self.tune_python_blocker(done);
            let copy = ui
                .add_enabled(blocked.is_none(), egui::Button::new("Copy Python"))
                .on_hover_text(
                    "Python with zeit-cdts that redoes this search: the points' series read as a pin's, the sets \
                     written out, the scores and the table. Needs zeit-cdts 3.2 or newer.",
                )
                .on_disabled_hover_text(blocked.clone().unwrap_or_default());
            if blocked.is_none() && copy.clicked() {
                match self.tune_python(done) {
                    Ok(code) => {
                        ui.ctx().copy_text(code);
                        t.message = "The Python of the search is on the clipboard.".into();
                    }
                    Err(why) => t.error = why,
                }
            }
        });
    }

    /// The omission x commission scatter; the set a click picks.
    fn tune_scatter(&mut self, ui: &mut Ui, t: &TuneUi, done: &Done) -> Option<usize> {
        let sets = &done.result.sets;
        let at = |k: usize| [sets[k].scores.omission, sets[k].scores.commission];
        let finite = |k: usize| sets[k].error.is_none() && at(k)[0].is_finite() && at(k)[1].is_finite();
        let accent = theme::on_plot(theme::accent());
        let grey = theme::on_plot(Color32::from_gray(150));
        let height = (ui.text_style_height(&egui::TextStyle::Body) * 14.0).max(crate::fit::scaled(ui, 200.0));
        let r = Plot::new("tune scatter")
            .height(height)
            .x_axis_label("omission")
            .y_axis_label("commission")
            .include_x(0.0)
            .include_y(0.0)
            // fractions in a short plot: steps of 0.2 (base 10 put the labels 0.1 apart, too close to be drawn)
            .y_grid_spacer(egui_plot::uniform_grid_spacer(|_| [0.05, 0.2, 1.0]))
            .allow_scroll(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .show(ui, |plot| {
                let all: Vec<[f64; 2]> =
                    (0..sets.len()).filter(|&k| finite(k) && sets[k].kind == SetKind::Sampled).map(at).collect();
                plot.points(Points::new("sets", PlotPoints::from(all)).color(grey).radius(3.0));
                let front: Vec<[f64; 2]> = done.pareto.iter().map(|&k| at(k)).collect();
                plot.line(Line::new("Pareto front", PlotPoints::from(front.clone())).color(accent).width(1.5));
                plot.points(Points::new("Pareto front", PlotPoints::from(front)).color(accent).radius(4.0));
                for (kind, name, shape, colour) in [
                    (
                        SetKind::Default,
                        "default",
                        MarkerShape::Diamond,
                        theme::on_plot(Color32::from_rgb(236, 130, 40)),
                    ),
                    (
                        SetKind::Current,
                        "current",
                        MarkerShape::Square,
                        theme::on_plot(Color32::from_rgb(120, 190, 255)),
                    ),
                ] {
                    if let Some(k) = (0..sets.len()).find(|&k| sets[k].kind == kind && finite(k)) {
                        plot.points(
                            Points::new(name, PlotPoints::from(vec![at(k)])).shape(shape).color(colour).radius(7.0),
                        );
                    }
                }
                if let Some(k) = t.selected.filter(|&k| finite(k)) {
                    plot.points(
                        Points::new("selected", PlotPoints::from(vec![at(k)]))
                            .shape(MarkerShape::Circle)
                            .filled(false)
                            .color(theme::on_plot(Color32::from_rgb(255, 255, 255)))
                            .radius(10.0),
                    );
                }
            });
        ui.weak("Grey: the sets; blue line: the Pareto front; orange diamond: the defaults; light square: the current values.");
        if !r.response.clicked() {
            return None;
        }
        let at_px = r.response.interact_pointer_pos()?;
        (0..sets.len())
            .filter(|&k| finite(k))
            .map(|k| {
                let p = r.transform.position_from_point(&PlotPoint::new(at(k)[0], at(k)[1]));
                (k, p.distance(at_px))
            })
            .filter(|(_, d)| *d <= 14.0)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(k, _)| k)
    }

    /// The partial rank correlation of each varied parameter with the dated F1, as bars.
    fn tune_prcc_ui(&self, ui: &mut Ui, done: &Done) {
        let mut rows: Vec<(String, f64)> = done.result.vary.iter().cloned().zip(done.prcc.iter().copied()).collect();
        if rows.is_empty() || rows.iter().all(|r| r.1.is_nan()) {
            ui.weak("Not enough sets that vary to tell.");
            return;
        }
        rows.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
        let bar_w = crate::fit::scaled(ui, 160.0);
        let h = ui.text_style_height(&egui::TextStyle::Body);
        egui::Grid::new("tune prcc").num_columns(3).spacing([8.0, 2.0]).show(ui, |ui| {
            for (id, v) in &rows {
                ui.label(id);
                let (rect, resp) = ui.allocate_exact_size(vec2(bar_w, h), egui::Sense::hover());
                let mid = rect.center().x;
                let painter = ui.painter_at(rect);
                painter.line_segment(
                    [egui::pos2(mid, rect.top()), egui::pos2(mid, rect.bottom())],
                    egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
                );
                if v.is_finite() {
                    let w = (v.abs() as f32).min(1.0) * bar_w / 2.0;
                    let (x0, x1) = if *v >= 0.0 { (mid, mid + w) } else { (mid - w, mid) };
                    let colour = if *v >= 0.0 { theme::accent() } else { theme::warning() };
                    painter.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(x0, rect.top() + 2.0), egui::pos2(x1, rect.bottom() - 2.0)),
                        0.0,
                        colour,
                    );
                }
                resp.on_hover_text("To the right: a larger value gives a better dated F1; to the left: a worse one");
                ui.label(if v.is_finite() { format!("{v:+.2}") } else { "-".into() });
                ui.end_row();
            }
        });
        ui.weak("Partial rank correlation with the dated F1 over the sets.");
    }
}

/// A result for the layout audit (`--shots layout`): the window as it looks with a search done, without a series or a
/// layer. 40 synthetic annual series (half with a drop in a known year), LandTrendr, 12 sets; the engine's own run.
pub(crate) fn demo_result(tool: &Tool) -> Result<(Prepared, Done), String> {
    use janus_zeit::numeric::{decimal_year, year_start};
    let days: Vec<i64> = (1990..=2021).map(|y| year_start(y) + 181).collect();
    let ctx =
        PixelInput { years: days.iter().map(|&d| decimal_year(d)).collect(), days: Some(days), ..Default::default() };
    let points: Vec<TunePoint> = (0..40)
        .map(|k| {
            let by = 1998 + (k * 7) % 18;
            let changed = k % 2 == 1;
            let values = ctx
                .years
                .iter()
                .enumerate()
                .map(|(i, &t)| {
                    let noise = 0.03 * (((k * 31 + i * 17) % 13) as f64 / 6.0 - 1.0);
                    0.75 + noise - if changed && t >= f64::from(by as i32) { 0.35 } else { 0.0 }
                })
                .collect();
            TunePoint { values, reference: changed.then_some(f64::from(by as i32)), weight: 1.0, ..Default::default() }
        })
        .collect();
    let mut cfg = TuneConfig::new(&tool.id, (1990, 2021));
    cfg.n_sets = 12;
    cfg.threads = 2;
    let points = Arc::new(points);
    let res = tune::run(&cfg, &ctx, &points, &AtomicBool::new(false), &AtomicUsize::new(0))?;
    let prep = Prepared {
        key: "demo".into(),
        points: points.clone(),
        places: Arc::new(Vec::new()),
        series_id: 0,
        ctx: Arc::new(ctx),
        sec_per_set: 0.002,
        stats: RefStats::default(),
        text: "a demonstration".into(),
        desc: json!({"kind": "demo"}),
        read_seconds: 0.0,
    };
    let done = Done::new(cfg, res, points, &prep, 0.5);
    Ok((prep, done))
}

impl State {
    /// The window with a demonstration result (the layout audit's place): the tool's window opens too.
    pub(crate) fn tune_demo(&mut self, tool: &Tool) {
        self.open_tune(tool);
        if let Ok((prep, done)) = demo_result(tool) {
            self.tune.prepared = Some(prep);
            self.tune.done = Some(done);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use val::labels::Event;
    use val::{Label, Point};

    fn year(t: &str) -> f64 {
        parse_date(t).unwrap().unwrap().floor()
    }

    #[test]
    fn dates_read_as_years_and_empty_is_no_change() {
        assert_eq!(parse_date(""), Ok(None));
        assert_eq!(parse_date("  "), Ok(None));
        assert_eq!(parse_date("0"), Ok(None));
        assert_eq!(year("2018"), 2018.0);
        assert_eq!(year("2018-05-12"), 2018.0);
        assert_eq!(year("2018/05/12"), 2018.0);
        assert_eq!(year("2018.05.12"), 2018.0);
        assert_eq!(year("20180512"), 2018.0);
        assert_eq!(year("2018-05-12T10:30:00"), 2018.0);
        assert_eq!(year("2018-05"), 2018.0);
        // a decimal year keeps its fraction, a date is later in its year the later it is
        assert!((parse_date("2018.5").unwrap().unwrap() - 2018.5).abs() < 1e-12);
        let (a, b) = (parse_date("2018-02-01").unwrap().unwrap(), parse_date("2018-11-01").unwrap().unwrap());
        assert!(2018.0 < a && a < b && b < 2019.0, "{a} {b}");
        for bad in ["soon", "18-05-12", "2018-13-01", "2018-05-40", "1800", "20181", "2018-x"] {
            assert!(parse_date(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_layers_date_field_is_the_reference() {
        let places = [Some((1.0, 2.0)), Some((3.0, 4.0)), None, Some((5.0, 6.0)), Some((7.0, 8.0))];
        let dates: Vec<String> = ["2019-03-01", "", "2020", "later", "2021"].iter().map(|s| s.to_string()).collect();
        let (pts, st) = layer_reference(&places, &dates);
        // the feature without a geometry and the one with a text that is not a date are left out
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[0].reference.map(f64::floor), Some(2019.0));
        assert_eq!(pts[1].reference, None);
        assert_eq!((pts[1].x, pts[1].y), (3.0, 4.0));
        assert_eq!(pts[2].reference.map(f64::floor), Some(2021.0));
        assert_eq!((st.candidates, st.with_change, st.no_change, st.left_out, st.bad_date), (5, 2, 1, 1, 1));
        assert!(st.first_bad.contains("later"), "{}", st.first_bad);
    }

    #[test]
    fn labels_map_to_change_and_no_change() {
        let mapping: BTreeMap<String, String> =
            [("Deforestation", val::CHANGE), ("Stable forest", val::NO_CHANGE), ("Unsure", "")]
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
        assert_eq!(meaning(&mapping, "Deforestation"), Meaning::Change);
        assert_eq!(meaning(&mapping, "Stable forest"), Meaning::NoChange);
        assert_eq!(meaning(&mapping, "Unsure"), Meaning::LeftOut);
        assert_eq!(meaning(&mapping, "Never heard of it"), Meaning::LeftOut);
        // the guess from the names
        let p = Project {
            label_set: vec!["Deforestation".into(), "Stable forest".into(), "Cloud, can't tell".into()],
            ..Default::default()
        };
        let m = default_mapping(&p);
        assert_eq!(meaning(&m, "Deforestation"), Meaning::Change);
        assert_eq!(meaning(&m, "Stable forest"), Meaning::NoChange);
        assert_eq!(meaning(&m, "Cloud, can't tell"), Meaning::LeftOut);
    }

    fn project(n: usize) -> Project {
        let points: Vec<Point> = (0..n)
            .map(|i| Point {
                id: i as i32 + 1,
                stratum: 1 + (i % 2) as i32,
                x: i as f64,
                y: 10.0 * i as f64,
                ..Default::default()
            })
            .collect();
        let mut p = Project {
            points,
            label_set: vec!["Deforestation".into(), "Stable forest".into(), "Unsure".into()],
            ..Default::default()
        };
        p.strata.strata = vec![
            val::Stratum { code: 1, area: 300.0, ..Default::default() },
            val::Stratum { code: 2, area: 100.0, ..Default::default() },
        ];
        p
    }

    fn label(l: &str, date: &str) -> Label {
        Label { label: l.into(), date: date.into(), ..Default::default() }
    }

    #[test]
    fn a_projects_labels_give_the_reference_of_its_calibration_points() {
        let mut p = project(8);
        let labels = vec![
            label("Deforestation", "2018-06-01"),
            label("Stable forest", ""),
            label("Deforestation", ""),
            label("Unsure", ""),
            Label {
                label: "Stable forest".into(),
                events: vec![Event { label: "Deforestation".into(), date: "2020".into() }],
                ..Default::default()
            },
            Label::default(),
            label("Deforestation", "2015"),
            label("Stable forest", ""),
        ];
        p.labels.insert("a".into(), labels);
        let mapping = default_mapping(&p);
        let idx: Vec<usize> = (0..8).collect();
        let (pts, st) = project_reference(&p, &idx, &mapping);
        // 0: change 2018, 1: no change, 2: change without a date, 3: left out, 4: no change label with a change event
        // (2020), 5: not labelled, 6: change 2015, 7: no change
        let years: Vec<Option<f64>> = pts.iter().map(|q| q.reference).collect();
        assert_eq!(years, [Some(2018.0), None, Some(2020.0), Some(2015.0), None]);
        assert_eq!((st.candidates, st.with_change, st.no_change, st.no_date, st.left_out), (8, 3, 2, 1, 2));
        // the place and the stratum of the point
        assert_eq!((pts[0].x, pts[0].y, pts[0].stratum), (0.0, 0.0, Some(0)));
        assert_eq!(pts[3].stratum, Some(0));
        // only the points asked for
        let (some, st) = project_reference(&p, &[0, 6], &mapping);
        assert_eq!((some.len(), st.candidates), (2, 2));
    }

    #[test]
    fn the_accuracy_points_cannot_calibrate() {
        let mut p = project(10);
        // no split: calibration_points' refusal
        assert!(accuracy_refusal(&p).contains("cannot calibrate"));
        assert!(val::calibration_points(&p).unwrap_err().contains("calibration split"));
        // with a split the accuracy points are still refused, with the reason
        p.calibration = Some(CalibrationSplit::draw(&p.points, 0.3, 7));
        let e = accuracy_refusal(&p);
        assert!(e.contains("cannot calibrate") && e.contains("optimistic"), "{e}");
        assert_eq!(val::calibration_points(&p).unwrap().len(), p.calibration.as_ref().unwrap().ids.len());
    }

    #[test]
    fn stratified_weights_make_each_stratum_count_its_area() {
        let mk = |h: Option<usize>| TunePoint { stratum: h, weight: 1.0, ..Default::default() };
        let mut pts = vec![mk(Some(0)), mk(Some(0)), mk(Some(0)), mk(Some(1)), mk(None)];
        set_weights(&mut pts, &[300.0, 100.0]);
        let w: Vec<f64> = pts.iter().map(|p| p.weight).collect();
        assert_eq!(w, [100.0, 100.0, 100.0, 100.0, 1.0]);
    }

    #[test]
    fn edited_ranges_become_the_searchs() {
        let tool = manifest().tool("bfast").unwrap();
        let spec = tune::spec("bfast").unwrap();
        let mut row = VaryRow::new(spec.param("h").unwrap(), tool);
        assert!(row.edited().is_none());
        (row.min, row.max) = (0.15, 0.2);
        let p = row.edited().unwrap();
        assert_eq!((p.min, p.max), (Some(0.15), Some(0.2)));
        row.reset();
        assert!(row.edited().is_none());
        // an enum: the options kept
        let tool = manifest().tool("bfast_monitor").unwrap();
        let spec = tune::spec("bfast_monitor").unwrap();
        let mut row = VaryRow::new(spec.param("alpha").unwrap(), tool);
        row.choices[1].1 = false;
        assert_eq!(row.edited().unwrap().choices.unwrap(), ["0.01", "0.1"]);
        // the values as text
        assert_eq!(fmt_value(&json!(0.5)), "0.5");
        assert_eq!(fmt_value(&json!(3)), "3");
        assert_eq!(fmt_value(&json!(0.12345678)), "0.1235");
        assert_eq!(fmt_value(&json!("auto")), "auto");
    }

    /// A pseudo-random number in [0, 1) (SplitMix64).
    fn bench_unit(state: &mut u64) -> f64 {
        *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = *state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }

    /// `n` synthetic points over `years` (decimal), `per_year` dates a year from `y0`: half stable, half with a
    /// persistent drop of `drops[k]` at a year in `at`; a season of amplitude `season` and noise `noise`.
    fn bench_points(
        years: &[f64],
        n: usize,
        at: (i32, i32),
        drops: &[f64],
        noise: f64,
        season: f64,
        seed: u64,
    ) -> Vec<TunePoint> {
        let mut st = seed;
        (0..n)
            .map(|k| {
                let changed = k % 2 == 1;
                let by = at.0 + (bench_unit(&mut st) * f64::from(at.1 - at.0 + 1)) as i32;
                let drop = drops[(k / 2) % drops.len()];
                let values = years
                    .iter()
                    .map(|&t| {
                        let normal = (0..12).map(|_| bench_unit(&mut st)).sum::<f64>() - 6.0;
                        let mut v = 0.75 + noise * normal + season * (2.0 * std::f64::consts::PI * t).sin();
                        if changed && t >= f64::from(by) {
                            v -= drop;
                        }
                        v
                    })
                    .collect();
                TunePoint { values, reference: changed.then_some(f64::from(by)), weight: 1.0, ..Default::default() }
            })
            .collect()
    }

    /// The benchmark against the same loop in Python (tools/bench_tune/README.md), not a test: LandTrendr and BFAST on
    /// `JANUS_BENCH_POINTS` (300) synthetic points and `JANUS_BENCH_SETS` (200) sets, on `JANUS_BENCH_THREADS` (4)
    /// threads. It runs here, in the app's own binary, so that the allocator is the app's (mimalloc, as in zeit-cdts'
    /// extension): the pixel kernels allocate a lot, and the system's heap under four threads is several times
    /// slower. Writes what the Python reads (`<tool>_input.json`: the points' series and reference, every set's values
    /// and Zeit calls), the functions the Python scores with (`<tool>_functions.py`, those of the generated script)
    /// and the engine's scores and time (`<tool>_janus.json`) into `JANUS_BENCH_DIR`.
    #[test]
    #[ignore = "benchmark: set JANUS_BENCH_DIR, run in release"]
    fn bench_tune_against_python() {
        use janus_zeit::numeric::{decimal_year, year_start};
        use janus_zeit::python::{tool_call, tune_functions};
        let Ok(dir) = std::env::var("JANUS_BENCH_DIR") else {
            println!("skipped: JANUS_BENCH_DIR names no folder");
            return;
        };
        let get = |k: &str, d: usize| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
        let (p, n, threads) =
            (get("JANUS_BENCH_POINTS", 300), get("JANUS_BENCH_SETS", 200), get("JANUS_BENCH_THREADS", 4));
        // annual dates (1 July) of 1990-2021, monthly (the 15th) of 2005-2016
        let annual: Vec<i64> = (1990..=2021).map(|y| year_start(y) + 181).collect();
        let mut monthly = vec![];
        for y in 2005..=2016 {
            let (s, next) = (year_start(y), year_start(y + 1));
            monthly.extend((0..12).map(|m| s + (f64::from(m) * (next - s) as f64 / 12.0) as i64 + 14));
        }
        let cases = [
            ("landtrendr", annual, (1998, 2014), vec![0.12, 0.2, 0.4], 0.04, 0.0, 5),
            ("bfast", monthly, (2007, 2008), vec![0.3], 0.05, 0.1, 6),
        ];
        for (tool, days, at, drops, noise, season, seed) in cases {
            let ctx = PixelInput {
                years: days.iter().map(|&d| decimal_year(d)).collect(),
                days: Some(days.clone()),
                ..Default::default()
            };
            let pts = bench_points(&ctx.years, p, at, &drops, noise, season, seed);
            let mut cfg = TuneConfig::new(tool, (1990, 2030));
            (cfg.n_sets, cfg.threads) = (n, threads);
            let sets = tune::build_sets(&cfg).unwrap();
            let calls = |values: &[Value]| {
                let mut given = cfg.base.clone();
                for (id, v) in cfg.vary.iter().zip(values) {
                    given.insert(id.clone(), v.clone());
                }
                let params = janus_zeit::Params::with_defaults(tool, &given).unwrap();
                tool_call(tool, &params, &ctx).unwrap().data()
            };
            let set_calls: Vec<_> = sets.iter().map(|s| calls(&s.values)).collect();
            let input = json!({
                "tool": tool,
                "days": days,
                "points": pts.iter().map(|q| json!({ "values": q.values, "reference": q.reference })).collect::<Vec<_>>(),
                "vary": cfg.vary,
                "window": [cfg.opts.window.0, cfg.opts.window.1],
                "tolerance": cfg.opts.tolerance,
                "sets": sets.iter().zip(&set_calls).map(|(s, c)| json!({
                    "values": s.values,
                    "calls": c.iter().map(|c| json!({
                        "func": c.func,
                        "pos": c.pos,
                        "kw": c.kw.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            });
            let write = |name: &str, text: String| std::fs::write(format!("{dir}/{tool}_{name}"), text).unwrap();
            write("input.json", serde_json::to_string(&input).unwrap());
            write("functions.py", tune_functions(tool, &set_calls[0]).unwrap());
            let t = Instant::now();
            let r = tune::run(&cfg, &ctx, &pts, &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap();
            let seconds = t.elapsed().as_secs_f64();
            let scores: Vec<_> = r
                .sets
                .iter()
                .map(|s| {
                    let c = &s.scores;
                    json!({ "omission": c.omission, "commission": c.commission, "f1": c.f1, "f1_dated": c.f1_dated,
                            "agreement": c.date_agreement, "error": c.mean_abs_error })
                })
                .collect();
            let all =
                json!({ "points": p, "sets": r.sets.len(), "threads": threads, "seconds": seconds, "scores": scores });
            write("janus.json", serde_json::to_string(&all).unwrap());
            println!("{tool}: {} sets on {p} points, {threads} threads: {seconds:.2} s", r.sets.len());
        }
    }
}
