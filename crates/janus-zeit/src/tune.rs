//! Tuning a change tool's parameters on reference points (Janus's "Tune on samples"): which settings of
//! LandTrendr, CCDC, BFAST, ... give the change dates that the interpreted points show. What is tunable, with its
//! range and scale, is in `tune.json` (per tool: the output that is the date the map shows, how a missing change
//! reads in it, and the parameters, 3 or 4 of them on by default); the search is a Latin hypercube over those
//! ranges plus the tool's defaults and the caller's current values.
//!
//! Every set runs the tool's raster kernel (`job::run_chunk`) once on all the points, laid out as one row of
//! pixels, and reads the date output of each pixel: what the map would say at each point, not an approximation
//! of it (the pixel run is the same kernel on a 1 x 1 chunk). The score of a set counts points (calibration, not
//! an area estimate); see `Scores` for the exact definitions.
//!
//! Everything here is written from the method's descriptions: the Latin hypercube (McKay et al. 1979), the
//! partial rank correlation coefficient (Marino et al. 2008) and the k-fold check (Hastie et al. 2009, 7.10).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::sync::atomic::{self, AtomicBool, AtomicUsize};
use std::time::Instant;

use rayon::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::job::{Chunk, ChunkOut, JobCtx, run_chunk};
use crate::manifest::manifest;
use crate::numeric::lstsq;
use crate::pixel::{Params, PixelInput};

/// Fewer reference changes than this make the scores (and the choice of a set) unreliable.
pub const MIN_CHANGES: usize = 30;

// ---------------------------------------------------------------------------------------------------------------
// What is tunable (tune.json)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    #[default]
    Linear,
    Log,
}

/// What the date output holds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DateKind {
    /// a calendar year (LandTrendr's year of detection)
    Year,
    /// a decimal year (2018.43): the calendar year is its integer part
    DecimalYear,
}

/// A second output that must reach a minimum for the date to count as a change (BEAST's probability).
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct Gate {
    pub output: String,
    pub min: f64,
}

/// One tunable parameter: a numeric range (linear or log, rounded when `int`) or a list of the options of an enum.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct TuneParam {
    pub param: String,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub scale: Scale,
    #[serde(default)]
    pub int: bool,
    /// the options of an enum parameter (the search picks one of them)
    #[serde(default)]
    pub choices: Option<Vec<String>>,
    /// searched by default
    pub default_on: bool,
}

impl TuneParam {
    /// The value at `u` in [0, 1): on the range (log scale: equal steps in the logarithm; int: rounded) or the
    /// option it falls in.
    pub fn value(&self, u: f64) -> Value {
        let u = u.clamp(0.0, 1.0);
        if let Some(c) = &self.choices {
            let k = ((u * c.len() as f64).floor() as usize).min(c.len().saturating_sub(1));
            return Value::String(c.get(k).cloned().unwrap_or_default());
        }
        let (lo, hi) = (self.min.unwrap_or(0.0), self.max.unwrap_or(1.0));
        let v = match self.scale {
            Scale::Linear => lo + u * (hi - lo),
            Scale::Log => (lo.ln() + u * (hi.ln() - lo.ln())).exp(),
        };
        if self.int { Value::from(v.round().clamp(lo, hi) as i64) } else { Value::from(v.clamp(lo, hi)) }
    }

    /// A value as a number for the correlations: itself, or the position of an option.
    pub fn numeric(&self, v: &Value) -> f64 {
        if let Some(c) = &self.choices {
            return v.as_str().and_then(|s| c.iter().position(|x| x == s)).map_or(f64::NAN, |k| k as f64);
        }
        v.as_f64().or_else(|| v.as_bool().map(f64::from)).unwrap_or(f64::NAN)
    }
}

/// What tuning a tool takes.
#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ToolTune {
    /// the output that is the change date the map shows
    pub date_output: String,
    pub date: DateKind,
    /// how a pixel without a change reads in the date output: "nan" (NaN, as every tool's map)
    pub none: String,
    #[serde(default)]
    pub gate: Option<Gate>,
    /// why these parameters and ranges
    pub why: String,
    pub params: Vec<TuneParam>,
}

impl ToolTune {
    pub fn param(&self, id: &str) -> Option<&TuneParam> {
        self.params.iter().find(|p| p.param == id)
    }

    /// The parameters searched by default.
    pub fn default_params(&self) -> Vec<&TuneParam> {
        self.params.iter().filter(|p| p.default_on).collect()
    }
}

#[derive(Deserialize)]
struct TuneFile {
    tools: BTreeMap<String, ToolTune>,
}

fn file() -> &'static TuneFile {
    static T: OnceLock<TuneFile> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(include_str!("tune.json")).expect("tune.json"))
}

/// The tools that can be tuned.
pub fn tunable_tools() -> Vec<&'static str> {
    file().tools.keys().map(String::as_str).collect()
}

/// What tuning `tool` takes, None for a tool with no date to tune.
pub fn spec(tool: &str) -> Option<&'static ToolTune> {
    file().tools.get(tool)
}

// ---------------------------------------------------------------------------------------------------------------
// The generator (SplitMix64: Steele, Lea and Flood 2014) and the Latin hypercube
// ---------------------------------------------------------------------------------------------------------------

struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.unit() * (i + 1) as f64) as usize;
            v.swap(i, j.min(i));
        }
    }
}

/// `n` points of the unit hypercube of `dims` dimensions, one per row: the range of each dimension is cut in `n`
/// equal strata, each stratum holds exactly one point (at a random place in it), and the strata of one dimension
/// are matched to those of another at random. The same seed gives the same points.
pub fn latin_hypercube(n: usize, dims: usize, seed: u64) -> Vec<Vec<f64>> {
    let mut rng = SplitMix64(seed);
    let mut out = vec![vec![0.0; dims]; n];
    for d in 0..dims {
        let mut perm: Vec<usize> = (0..n).collect();
        rng.shuffle(&mut perm);
        for (row, &k) in out.iter_mut().zip(&perm) {
            row[d] = (k as f64 + rng.unit()) / n as f64;
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// The sets
// ---------------------------------------------------------------------------------------------------------------

/// Where a set of parameters comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetKind {
    /// the tool's defaults
    Default,
    /// the values the caller has now (the window's)
    Current,
    /// a point of the Latin hypercube
    Sampled,
}

/// A set of values for the parameters that vary (`TuneConfig::vary`'s order).
#[derive(Clone, Debug, PartialEq)]
pub struct ParamSet {
    pub kind: SetKind,
    pub values: Vec<Value>,
    /// the values as numbers (an enum's option by its position), for the correlations
    pub x: Vec<f64>,
}

/// What a tuning runs.
#[derive(Clone, Debug)]
pub struct TuneConfig {
    pub tool: String,
    /// the caller's current values of the tool's parameters by id (those not given are the defaults): what the
    /// parameters that do not vary are held at
    pub base: BTreeMap<String, Value>,
    /// the parameters that vary (ids of tune.json)
    pub vary: Vec<String>,
    /// ranges (or options) the caller edited, by parameter id: they replace tune.json's for the search
    pub ranges: BTreeMap<String, TuneParam>,
    /// Latin hypercube sets (200 by default), besides the defaults and the current values
    pub n_sets: usize,
    pub seed: u64,
    /// threads of the search, at most (sets run in parallel, each on one thread)
    pub threads: usize,
    pub opts: ScoreOpts,
}

impl TuneConfig {
    pub fn new(tool: &str, window: (i32, i32)) -> TuneConfig {
        let vary = spec(tool).map(|s| s.default_params().iter().map(|p| p.param.clone()).collect()).unwrap_or_default();
        TuneConfig {
            tool: tool.into(),
            base: BTreeMap::new(),
            vary,
            ranges: BTreeMap::new(),
            n_sets: 200,
            seed: 1,
            threads: 1,
            opts: ScoreOpts { window, tolerance: 1, weighted: false },
        }
    }
}

/// The sets to run: the tool's defaults, the current values, then `n_sets` of the Latin hypercube over the ranges
/// of the parameters that vary. A sampled set equal to an earlier one (integers and options repeat) is left out,
/// so there can be fewer than `n_sets` of them.
pub fn build_sets(cfg: &TuneConfig) -> Result<Vec<ParamSet>, String> {
    let tt = spec(&cfg.tool).ok_or_else(|| format!("{}: not a tool with a change date to tune", cfg.tool))?;
    let tool = manifest().tool(&cfg.tool).ok_or_else(|| format!("{}: not a tool", cfg.tool))?;
    let mut tps = Vec::new();
    let mut defaults = Vec::new();
    let mut current = Vec::new();
    for id in &cfg.vary {
        let tp = cfg
            .ranges
            .get(id)
            .or_else(|| tt.param(id))
            .ok_or_else(|| format!("{}: {id} is not a tunable parameter", cfg.tool))?;
        let mp = tool.params.iter().find(|p| &p.id == id).ok_or_else(|| format!("{}: no parameter {id}", cfg.tool))?;
        defaults.push(mp.default.clone());
        current.push(cfg.base.get(id).cloned().unwrap_or_else(|| mp.default.clone()));
        tps.push(tp);
    }
    let make = |kind: SetKind, values: Vec<Value>| {
        let x = tps.iter().zip(&values).map(|(tp, v)| tp.numeric(v)).collect();
        ParamSet { kind, values, x }
    };
    let mut sets = vec![make(SetKind::Default, defaults), make(SetKind::Current, current)];
    if !tps.is_empty() {
        for row in latin_hypercube(cfg.n_sets, tps.len(), cfg.seed) {
            let values: Vec<Value> = tps.iter().zip(&row).map(|(tp, &u)| tp.value(u)).collect();
            if !sets.iter().any(|s| s.values == values) {
                sets.push(make(SetKind::Sampled, values));
            }
        }
    }
    Ok(sets)
}

/// The tool's parameters of a set: the caller's current values with the varied ones replaced.
fn params_of(cfg: &TuneConfig, set: &ParamSet) -> Params {
    let mut given = cfg.base.clone();
    for (id, v) in cfg.vary.iter().zip(&set.values) {
        given.insert(id.clone(), v.clone());
    }
    Params::with_defaults(&cfg.tool, &given).unwrap_or_default()
}

// ---------------------------------------------------------------------------------------------------------------
// The points and the scores
// ---------------------------------------------------------------------------------------------------------------

/// One reference point: its series (the dates are the run's, shared) and what the interpreters found.
#[derive(Clone, Debug, Default)]
pub struct TunePoint {
    /// the shown series, one value per date (NaN = missing)
    pub values: Vec<f64>,
    /// multiband tools: each mapped band role's series
    pub bands: BTreeMap<String, Vec<f64>>,
    pub qa: Option<Vec<f64>>,
    /// the year of the change; None = no change
    pub reference: Option<f64>,
    /// the point's stratum, for the stratified weight
    pub stratum: Option<usize>,
    /// the point's weight when the scores are weighted (its stratum's area share over the points drawn in it)
    pub weight: f64,
}

/// How the sets are scored.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoreOpts {
    /// the years (inclusive) in which a change counts, in the reference and in the map
    pub window: (i32, i32),
    /// a matched change's dates agree when the calendar years differ by at most this
    pub tolerance: i32,
    /// count each point with its weight instead of 1
    pub weighted: bool,
}

/// The scores of a set of parameters on the points, counted by points (weighted by `TunePoint::weight` when asked).
/// A point is a *reference change* when its reference year is in the window, and a *map change* when the map's
/// date (its calendar year: the integer part) is in the window; a reference change outside the window counts as no
/// change, and so does a map date outside it.
///
/// - omission: the reference changes the map has no change at, over all the reference changes (NaN without any);
/// - commission: the map changes at a point with no reference change, over all the map changes (0 without any);
/// - f1: 2 TP / (2 TP + FP + FN), the change being detected or not (TP: both are changes, whatever the dates);
/// - date_agreement: of the TP points, the share whose map and reference years differ by at most the tolerance
///   (NaN without any). A change found with a wrong date is a TP and a disagreement here, not an omission:
///   presence and timing are scored apart, so the set that finds the changes and the set that dates them well can
///   both be seen; the choice of the best set breaks the ties of f1 by this share;
/// - mean_abs_error: the mean of |map year - reference year| over the TP points, in years (NaN without any);
/// - f1_dated: the f1 where a change found with a date beyond the tolerance counts as both an omission and a
///   false change (2 TPd / (2 TPd + FP + FN + 2 M), TPd the matched changes whose dates agree, M the others): the
///   score that asks for the right date too (NaN when there is nothing to count).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scores {
    pub omission: f64,
    pub commission: f64,
    pub f1: f64,
    pub date_agreement: f64,
    pub mean_abs_error: f64,
    pub f1_dated: f64,
    /// (weighted) true positives, omitted changes, false changes
    pub tp: f64,
    pub fn_: f64,
    pub fp: f64,
    /// points: with a reference change, with a map change
    pub n_ref: usize,
    pub n_map: usize,
}

impl Scores {
    /// A set that did not run.
    pub fn failed() -> Scores {
        Scores {
            omission: f64::NAN,
            commission: f64::NAN,
            f1: f64::NAN,
            date_agreement: f64::NAN,
            mean_abs_error: f64::NAN,
            f1_dated: f64::NAN,
            tp: 0.0,
            fn_: 0.0,
            fp: 0.0,
            n_ref: 0,
            n_map: 0,
        }
    }

    pub fn get(&self, kind: ScoreKind) -> f64 {
        match kind {
            ScoreKind::F1 => self.f1,
            ScoreKind::Omission => self.omission,
            ScoreKind::Commission => self.commission,
            ScoreKind::DateAgreement => self.date_agreement,
            ScoreKind::MeanAbsError => self.mean_abs_error,
            ScoreKind::F1Dated => self.f1_dated,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreKind {
    F1,
    Omission,
    Commission,
    DateAgreement,
    MeanAbsError,
    F1Dated,
}

/// The calendar year of a date (None: no date).
fn year_of(d: f64) -> Option<i32> {
    (d.is_finite() && d > 0.0).then(|| d.floor() as i32)
}

/// The scores of the dates `dates` (one per point, NaN = no change) on the points of `subset` (their indices).
pub fn score_subset(dates: &[f64], points: &[TunePoint], subset: &[usize], o: &ScoreOpts) -> Scores {
    let (y0, y1) = o.window;
    let inside = |y: Option<i32>| y.filter(|y| (y0..=y1).contains(y));
    let (mut tp, mut fn_, mut fp, mut agree, mut err) = (0.0, 0.0, 0.0, 0.0, 0.0);
    let (mut n_ref, mut n_map) = (0, 0);
    for &i in subset {
        let w = if o.weighted { points[i].weight } else { 1.0 };
        let r = inside(points[i].reference.and_then(year_of));
        let m = inside(dates.get(i).copied().and_then(year_of));
        n_ref += usize::from(r.is_some());
        n_map += usize::from(m.is_some());
        match (r, m) {
            (Some(r), Some(m)) => {
                tp += w;
                let d = f64::from((r - m).abs());
                err += w * d;
                if (r - m).abs() <= o.tolerance {
                    agree += w;
                }
            }
            (Some(_), None) => fn_ += w,
            (None, Some(_)) => fp += w,
            (None, None) => {}
        }
    }
    let ratio = |a: f64, b: f64, empty: f64| if b > 0.0 { a / b } else { empty };
    Scores {
        omission: ratio(fn_, tp + fn_, f64::NAN),
        commission: ratio(fp, tp + fp, 0.0),
        f1: ratio(2.0 * tp, 2.0 * tp + fp + fn_, f64::NAN),
        date_agreement: ratio(agree, tp, f64::NAN),
        mean_abs_error: ratio(err, tp, f64::NAN),
        f1_dated: ratio(2.0 * agree, 2.0 * agree + fp + fn_ + 2.0 * (tp - agree), f64::NAN),
        tp,
        fn_,
        fp,
        n_ref,
        n_map,
    }
}

/// The scores on all the points.
pub fn score(dates: &[f64], points: &[TunePoint], o: &ScoreOpts) -> Scores {
    let all: Vec<usize> = (0..points.len()).collect();
    score_subset(dates, points, &all, o)
}

/// How many points have a reference change in the window.
pub fn count_changes(points: &[TunePoint], window: (i32, i32)) -> usize {
    points.iter().filter(|p| p.reference.and_then(year_of).is_some_and(|y| (window.0..=window.1).contains(&y))).count()
}

/// The order of the sets: the higher dated f1 first (the score that asks for the right date too: a set that finds
/// every change at a wrong date must not win), then the higher plain f1, then the higher date agreement, then the
/// smaller mean error; NaN last.
pub fn cmp_scores(a: &Scores, b: &Scores) -> Ordering {
    let high = |x: f64, y: f64| match (x.is_nan(), y.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        _ => y.total_cmp(&x),
    };
    high(a.f1_dated, b.f1_dated)
        .then_with(|| high(a.f1, b.f1))
        .then_with(|| high(a.date_agreement, b.date_agreement))
        .then_with(|| high(-a.mean_abs_error, -b.mean_abs_error))
}

/// The sets no other set beats on both omission and commission (smaller is better), by increasing omission. A pair
/// with a NaN is left out. Equal pairs are all kept.
pub fn pareto_front(points: &[(f64, f64)]) -> Vec<usize> {
    let ok = |p: &(f64, f64)| p.0.is_finite() && p.1.is_finite();
    let mut front: Vec<usize> = (0..points.len())
        .filter(|&i| {
            ok(&points[i])
                && !points.iter().enumerate().any(|(j, q)| {
                    j != i
                        && ok(q)
                        && q.0 <= points[i].0
                        && q.1 <= points[i].1
                        && (q.0 < points[i].0 || q.1 < points[i].1)
                })
        })
        .collect();
    front.sort_by(|&a, &b| points[a].0.total_cmp(&points[b].0).then(points[a].1.total_cmp(&points[b].1)));
    front
}

// ---------------------------------------------------------------------------------------------------------------
// Sensitivity: the partial rank correlation coefficient
// ---------------------------------------------------------------------------------------------------------------

/// Ranks 1..n of `v`, the mean rank to ties.
fn ranks(v: &[f64]) -> Vec<f64> {
    let mut order: Vec<usize> = (0..v.len()).collect();
    order.sort_by(|&a, &b| v[a].total_cmp(&v[b]));
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < order.len() {
        let mut j = i;
        while j + 1 < order.len() && v[order[j + 1]] == v[order[i]] {
            j += 1;
        }
        let mean = (i + j) as f64 / 2.0 + 1.0;
        for &k in &order[i..=j] {
            r[k] = mean;
        }
        i = j + 1;
    }
    r
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    if saa <= 1e-12 || sbb <= 1e-12 { f64::NAN } else { sab / (saa * sbb).sqrt() }
}

/// What is left of `t` after the least-squares fit on `cols` (and a constant).
fn residual(cols: &[&Vec<f64>], t: &[f64]) -> Vec<f64> {
    let rows: Vec<Vec<f64>> =
        (0..t.len()).map(|i| std::iter::once(1.0).chain(cols.iter().map(|c| c[i])).collect()).collect();
    let beta = lstsq(&rows, t);
    rows.iter().zip(t).map(|(r, &y)| y - r.iter().zip(&beta).map(|(a, b)| a * b).sum::<f64>()).collect()
}

/// The partial rank correlation of each parameter (a column of `x`, one row per set) with `y` over the sets: both
/// rank-transformed, the other parameters regressed out of each by least squares, the residuals correlated. In
/// [-1, 1]; the sign says which way the score goes as the parameter grows, the size how much the parameter
/// matters whatever the others are. A set with a NaN (in `y` or in `x`) is left out; a parameter that does not
/// change, or a score that does not, gives NaN; so do too few sets.
pub fn prcc(x: &[Vec<f64>], y: &[f64]) -> Vec<f64> {
    let p = x.first().map_or(0, Vec::len);
    let keep: Vec<usize> =
        (0..y.len().min(x.len())).filter(|&i| y[i].is_finite() && x[i].iter().all(|v| v.is_finite())).collect();
    if keep.len() < p + 3 {
        return vec![f64::NAN; p];
    }
    let ry = ranks(&keep.iter().map(|&i| y[i]).collect::<Vec<_>>());
    let rx: Vec<Vec<f64>> = (0..p).map(|j| ranks(&keep.iter().map(|&i| x[i][j]).collect::<Vec<_>>())).collect();
    let varies = |c: &Vec<f64>| c.iter().any(|&v| v != c[0]);
    (0..p)
        .map(|j| {
            if !varies(&rx[j]) {
                return f64::NAN;
            }
            let others: Vec<&Vec<f64>> = (0..p).filter(|&k| k != j && varies(&rx[k])).map(|k| &rx[k]).collect();
            pearson(&residual(&others, &rx[j]), &residual(&others, &ry))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------------------------

/// A set's result.
#[derive(Clone, Debug)]
pub struct SetResult {
    pub kind: SetKind,
    pub values: Vec<Value>,
    pub x: Vec<f64>,
    /// the map's date at each point (NaN = no change)
    pub dates: Vec<f64>,
    pub scores: Scores,
    /// why the set did not run
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TuneResult {
    pub tool: String,
    pub vary: Vec<String>,
    pub opts: ScoreOpts,
    /// in the order they were made: the defaults, the current values, the sampled ones
    pub sets: Vec<SetResult>,
    /// the search was cancelled: `sets` holds those that finished
    pub cancelled: bool,
    /// points with a reference change in the window
    pub n_changes: usize,
    /// fewer than MIN_CHANGES of them: the scores are unreliable
    pub few_changes: bool,
}

impl TuneResult {
    /// The sets that ran, best first (see `cmp_scores`).
    pub fn ranked(&self) -> Vec<usize> {
        let mut v: Vec<usize> = (0..self.sets.len()).filter(|&i| self.sets[i].error.is_none()).collect();
        v.sort_by(|&a, &b| cmp_scores(&self.sets[a].scores, &self.sets[b].scores).then(a.cmp(&b)));
        v
    }

    pub fn best(&self) -> Option<usize> {
        self.ranked().first().copied()
    }

    /// The sets on the (omission, commission) Pareto front.
    pub fn pareto(&self) -> Vec<usize> {
        let pts: Vec<(f64, f64)> = self
            .sets
            .iter()
            .map(|s| if s.error.is_none() { (s.scores.omission, s.scores.commission) } else { (f64::NAN, f64::NAN) })
            .collect();
        pareto_front(&pts)
    }

    /// The partial rank correlation of each varied parameter with a score over the sets that ran.
    pub fn sensitivity(&self, kind: ScoreKind) -> Vec<f64> {
        let ran: Vec<&SetResult> = self.sets.iter().filter(|s| s.error.is_none()).collect();
        let x: Vec<Vec<f64>> = ran.iter().map(|s| s.x.clone()).collect();
        let y: Vec<f64> = ran.iter().map(|s| s.scores.get(kind)).collect();
        prcc(&x, &y)
    }

    /// How much choosing the best set on the points flatters it: k-fold, with the points split by `seed`, the best
    /// set chosen on k-1 folds and scored on the one left out; the held-out predictions of all the folds are then
    /// scored together. None without two points or without a set that ran.
    pub fn optimism(&self, points: &[TunePoint], k: usize, seed: u64) -> Option<Optimism> {
        let n = points.len();
        let k = k.clamp(2, n.max(2));
        let ran: Vec<usize> = (0..self.sets.len()).filter(|&i| self.sets[i].error.is_none()).collect();
        if n < 2 || ran.is_empty() {
            return None;
        }
        let o = &self.opts;
        // each fold gets its share of the points with a change and of those without
        let (y0, y1) = o.window;
        let (mut with, mut without): (Vec<usize>, Vec<usize>) =
            (0..n).partition(|&i| points[i].reference.and_then(year_of).is_some_and(|y| (y0..=y1).contains(&y)));
        let mut rng = SplitMix64(seed);
        rng.shuffle(&mut with);
        rng.shuffle(&mut without);
        let mut fold = vec![0usize; n];
        for (c, &i) in with.iter().chain(&without).enumerate() {
            fold[i] = c % k;
        }
        let best_on = |subset: &[usize]| {
            *ran.iter()
                .min_by(|&&a, &&b| {
                    let (sa, sb) = (
                        score_subset(&self.sets[a].dates, points, subset, o),
                        score_subset(&self.sets[b].dates, points, subset, o),
                    );
                    cmp_scores(&sa, &sb).then(a.cmp(&b))
                })
                .unwrap_or(&ran[0])
        };
        let all: Vec<usize> = (0..n).collect();
        let best = best_on(&all);
        let mut pooled = vec![f64::NAN; n];
        let (mut chosen, mut fold_f1) = (Vec::new(), Vec::new());
        for f in 0..k {
            let train: Vec<usize> = all.iter().copied().filter(|&i| fold[i] != f).collect();
            let test: Vec<usize> = all.iter().copied().filter(|&i| fold[i] == f).collect();
            if test.is_empty() || train.is_empty() {
                continue;
            }
            let c = best_on(&train);
            for &i in &test {
                pooled[i] = self.sets[c].dates[i];
            }
            fold_f1.push(score_subset(&self.sets[c].dates, points, &test, o).f1);
            chosen.push(c);
        }
        Some(Optimism {
            k,
            best,
            in_sample: self.sets[best].scores,
            held_out: score(&pooled, points, o),
            chosen,
            fold_f1,
        })
    }
}

/// The k-fold check of the best set.
#[derive(Clone, Debug)]
pub struct Optimism {
    pub k: usize,
    /// the best set on all the points, and its scores there
    pub best: usize,
    pub in_sample: Scores,
    /// the scores of the folds' held-out predictions, taken together
    pub held_out: Scores,
    /// the set chosen on each fold's training points
    pub chosen: Vec<usize>,
    /// f1 of each fold's chosen set on its held-out points
    pub fold_f1: Vec<f64>,
}

impl Optimism {
    /// How much higher the in-sample f1 is than the held-out one (what the choice has fitted to the points).
    pub fn f1_gap(&self) -> f64 {
        self.in_sample.f1 - self.held_out.f1
    }
}

/// The points as the one-row chunk a raster run would hold: [T][P].
fn chunk_of(ctx: &PixelInput, points: &[TunePoint]) -> Result<Chunk, String> {
    let (t, n) = (ctx.years.len(), points.len());
    if n == 0 {
        return Err("there are no points to tune on".into());
    }
    let layout = |get: &dyn Fn(&TunePoint) -> Option<&Vec<f64>>, what: &str| -> Result<Vec<f64>, String> {
        let mut a = vec![f64::NAN; t * n];
        for (p, pt) in points.iter().enumerate() {
            let s = get(pt).ok_or_else(|| format!("point {} has no {what}", p + 1))?;
            if s.len() != t {
                return Err(format!("point {} has {} values of {t} dates ({what})", p + 1, s.len()));
            }
            for (i, &v) in s.iter().enumerate() {
                a[i * n + p] = v;
            }
        }
        Ok(a)
    };
    let stack = layout(&|p| Some(&p.values), "series")?;
    let mut bands = BTreeMap::new();
    for role in points[0].bands.keys() {
        bands.insert(role.clone(), layout(&|p| p.bands.get(role), &format!("band {role}"))?);
    }
    let qa = if points[0].qa.is_some() { Some(layout(&|p| p.qa.as_ref(), "quality band")?) } else { None };
    Ok(Chunk { t, h: 1, w: n, stack, bands, qa })
}

/// The date at each point from a chunk's outputs: NaN where there is no change.
fn dates_of(out: &ChunkOut, tt: &ToolTune, n: usize) -> Result<Vec<f64>, String> {
    let col = |id: &str| -> Result<&Vec<f64>, String> {
        let v = out.get(id).ok_or_else(|| format!("the tool gave no output '{id}'"))?;
        if v.len() != n {
            return Err(format!("the output '{id}' has {} values for {n} points", v.len()));
        }
        Ok(v)
    };
    let d = col(&tt.date_output)?;
    let gate = tt.gate.as_ref().map(|g| col(&g.output).map(|v| (v, g.min))).transpose()?;
    Ok((0..n)
        .map(|i| {
            let open = gate.as_ref().is_none_or(|(v, min)| v[i] >= *min);
            if open && d[i].is_finite() && d[i] > 0.0 { d[i] } else { f64::NAN }
        })
        .collect())
}

fn run_set(cfg: &TuneConfig, tt: &ToolTune, set: &ParamSet, chunk: &Chunk, jc: &JobCtx) -> Result<Vec<f64>, String> {
    let out = run_chunk(&cfg.tool, &params_of(cfg, set), chunk, jc)?;
    dates_of(&out, tt, chunk.pixels())
}

/// Runs the search: every set on all the points, `cfg.threads` at a time at most (each set on one thread). `ctx`
/// holds the series' dates (years, days, and what the tool needs of them) shared by the points. `cancel` stops it
/// between sets (the result then holds the sets done and says so); `progress` counts the sets done. An error when
/// no set could run (the first set's reason).
pub fn run(
    cfg: &TuneConfig,
    ctx: &PixelInput,
    points: &[TunePoint],
    cancel: &AtomicBool,
    progress: &AtomicUsize,
) -> Result<TuneResult, String> {
    let tt = spec(&cfg.tool).ok_or_else(|| format!("{}: not a tool with a change date to tune", cfg.tool))?;
    let sets = build_sets(cfg)?;
    let chunk = chunk_of(ctx, points)?;
    let jc = JobCtx::for_pixel(ctx);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(cfg.threads.max(1))
        .build()
        .map_err(|e| format!("cannot start the threads: {e}"))?;
    let done: Vec<Option<SetResult>> = pool.install(|| {
        sets.par_iter()
            .map(|s| {
                if cancel.load(atomic::Ordering::Relaxed) {
                    return None;
                }
                let r = match run_set(cfg, tt, s, &chunk, &jc) {
                    Ok(dates) => {
                        let scores = score(&dates, points, &cfg.opts);
                        SetResult { kind: s.kind, values: s.values.clone(), x: s.x.clone(), dates, scores, error: None }
                    }
                    Err(e) => SetResult {
                        kind: s.kind,
                        values: s.values.clone(),
                        x: s.x.clone(),
                        dates: vec![f64::NAN; points.len()],
                        scores: Scores::failed(),
                        error: Some(e),
                    },
                };
                progress.fetch_add(1, atomic::Ordering::Relaxed);
                Some(r)
            })
            .collect()
    });
    let cancelled = done.iter().any(Option::is_none);
    let sets: Vec<SetResult> = done.into_iter().flatten().collect();
    if !cancelled && !sets.is_empty() && sets.iter().all(|s| s.error.is_some()) {
        return Err(sets[0].error.clone().unwrap_or_default());
    }
    let n_changes = count_changes(points, cfg.opts.window);
    Ok(TuneResult {
        tool: cfg.tool.clone(),
        vary: cfg.vary.clone(),
        opts: cfg.opts,
        sets,
        cancelled,
        n_changes,
        few_changes: n_changes < MIN_CHANGES,
    })
}

/// The seconds one set takes on all the points on one thread (the defaults, run once): what the estimate of the
/// whole search is made from.
pub fn time_one_set(cfg: &TuneConfig, ctx: &PixelInput, points: &[TunePoint]) -> Result<f64, String> {
    let tt = spec(&cfg.tool).ok_or_else(|| format!("{}: not a tool with a change date to tune", cfg.tool))?;
    let chunk = chunk_of(ctx, points)?;
    let jc = JobCtx::for_pixel(ctx);
    let set = build_sets(&TuneConfig { n_sets: 0, ..cfg.clone() })?.remove(0);
    let t = Instant::now();
    run_set(cfg, tt, &set, &chunk, &jc)?;
    Ok(t.elapsed().as_secs_f64())
}

/// The seconds of a search of `n_sets` sets from the seconds of one (`time_one_set`), on `threads` threads.
pub fn estimate_seconds(sec_per_set: f64, n_sets: usize, threads: usize) -> f64 {
    sec_per_set * n_sets as f64 / threads.max(1) as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::numeric::{decimal_year, year_start};

    /// A normal draw (the sum of twelve uniforms).
    fn normal(rng: &mut SplitMix64) -> f64 {
        (0..12).map(|_| rng.unit()).sum::<f64>() - 6.0
    }

    /// Annual dates (1 July) of `y0..=y1`.
    fn annual_ctx(y0: i32, y1: i32) -> PixelInput {
        let days: Vec<i64> = (y0..=y1).map(|y| year_start(i64::from(y)) + 181).collect();
        PixelInput { years: days.iter().map(|&d| decimal_year(d)).collect(), days: Some(days), ..Default::default() }
    }

    /// Monthly dates (the 15th) of `y0..=y1`.
    fn monthly_ctx(y0: i32, y1: i32) -> PixelInput {
        let mut days = vec![];
        for y in y0..=y1 {
            let s = year_start(i64::from(y));
            let next = year_start(i64::from(y) + 1);
            for m in 0..12 {
                days.push(s + (f64::from(m) * (next - s) as f64 / 12.0) as i64 + 14);
            }
        }
        PixelInput { years: days.iter().map(|&d| decimal_year(d)).collect(), days: Some(days), ..Default::default() }
    }

    /// Half stable, half with a persistent drop of `drops[k % len]` at a year in `break_years` of `ctx`'s
    /// annual (or monthly, with a season) series; the reference is the year of the drop.
    fn synthetic(
        ctx: &PixelInput,
        n: usize,
        break_years: (i32, i32),
        drops: &[f64],
        noise: f64,
        season: f64,
        seed: u64,
    ) -> Vec<TunePoint> {
        let mut rng = SplitMix64(seed);
        (0..n)
            .map(|k| {
                let changed = k % 2 == 1;
                let by = break_years.0 + (rng.unit() * f64::from(break_years.1 - break_years.0 + 1)) as i32;
                let drop = drops[(k / 2) % drops.len()];
                let values = ctx
                    .years
                    .iter()
                    .map(|&t| {
                        let mut v = 0.75 + noise * normal(&mut rng) + season * (2.0 * std::f64::consts::PI * t).sin();
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

    fn config(tool: &str, vary: &[&str], n: usize) -> TuneConfig {
        let mut cfg = TuneConfig::new(tool, (1990, 2030));
        cfg.vary = vary.iter().map(|s| s.to_string()).collect();
        cfg.n_sets = n;
        cfg.threads = 2;
        cfg
    }

    fn go(cfg: &TuneConfig, ctx: &PixelInput, pts: &[TunePoint]) -> TuneResult {
        run(cfg, ctx, pts, &AtomicBool::new(false), &AtomicUsize::new(0)).unwrap()
    }

    fn num(v: &Value) -> f64 {
        v.as_f64().unwrap()
    }

    #[test]
    fn tune_json_matches_the_manifest() {
        let m = manifest();
        let mut tools = tunable_tools();
        tools.sort_unstable();
        assert_eq!(
            tools,
            ["beast", "bfast", "bfast_lite", "bfast_monitor", "ccdc", "coded", "ewmacd", "landtrendr", "sccd"]
        );
        for (id, tt) in &file().tools {
            let tool = m.tool(id).unwrap_or_else(|| panic!("{id}: not in tools.json"));
            assert!(tool.modes.iter().any(|m| m == "raster"), "{id}: no raster kernel");
            assert!(tool.outputs.iter().any(|o| o.id == tt.date_output), "{id}: no output {}", tt.date_output);
            if let Some(g) = &tt.gate {
                assert!(tool.outputs.iter().any(|o| o.id == g.output), "{id}: no gate output {}", g.output);
            }
            assert!(!tt.why.is_empty());
            let on = tt.params.iter().filter(|p| p.default_on).count();
            assert!((3..=4).contains(&on), "{id}: {on} parameters on by default");
            for tp in &tt.params {
                let mp =
                    tool.params.iter().find(|p| p.id == tp.param).unwrap_or_else(|| panic!("{id}: no {}", tp.param));
                if let Some(c) = &tp.choices {
                    assert_eq!(mp.kind, "enum", "{id}.{}", tp.param);
                    let options = mp.options.as_ref().unwrap();
                    assert!(c.len() >= 2 && c.iter().all(|x| options.contains(x)), "{id}.{}: {c:?}", tp.param);
                    continue;
                }
                assert!(mp.kind == "int" || mp.kind == "float", "{id}.{}: kind {}", tp.param, mp.kind);
                assert_eq!(tp.int, mp.kind == "int", "{id}.{}: int flag", tp.param);
                let (lo, hi) = (tp.min.unwrap(), tp.max.unwrap());
                assert!(lo < hi, "{id}.{}", tp.param);
                assert!(
                    mp.min.is_none_or(|m| lo >= m) && mp.max.is_none_or(|m| hi <= m),
                    "{id}.{}: outside the manifest",
                    tp.param
                );
                if tp.scale == Scale::Log {
                    assert!(lo > 0.0, "{id}.{}: log scale from {lo}", tp.param);
                }
            }
        }
    }

    #[test]
    fn latin_hypercube_hits_each_stratum_once() {
        let (n, d) = (40, 5);
        let a = latin_hypercube(n, d, 3);
        assert_eq!(a, latin_hypercube(n, d, 3));
        assert_ne!(a, latin_hypercube(n, d, 4));
        for j in 0..d {
            let mut hit = vec![0; n];
            for row in &a {
                assert!((0.0..1.0).contains(&row[j]));
                hit[(row[j] * n as f64) as usize] += 1;
            }
            assert!(hit.iter().all(|&h| h == 1), "dim {j}: {hit:?}");
        }
        // the dimensions are not matched in the same order
        assert!((0..n).any(|i| ((a[i][0] * n as f64) as usize) != ((a[i][1] * n as f64) as usize)));
    }

    #[test]
    fn values_by_scale_int_and_choice() {
        let tp = |min, max, scale, int| TuneParam {
            param: "a".into(),
            min: Some(min),
            max: Some(max),
            scale,
            int,
            choices: None,
            default_on: true,
        };
        let lin = tp(2.0, 10.0, Scale::Linear, true);
        assert_eq!(lin.value(0.0), Value::from(2));
        assert_eq!(lin.value(0.5), Value::from(6));
        assert_eq!(lin.value(0.9999), Value::from(10));
        let log = tp(0.01, 1.0, Scale::Log, false);
        // the middle of a log range is the geometric mean
        assert!((num(&log.value(0.5)) - 0.1).abs() < 1e-12);
        assert!((num(&log.value(0.0)) - 0.01).abs() < 1e-15);
        let ch = TuneParam {
            choices: Some(vec!["x".into(), "y".into(), "z".into()]),
            default_on: false,
            ..tp(0.0, 1.0, Scale::Linear, false)
        };
        assert_eq!(ch.value(0.0), Value::from("x"));
        assert_eq!(ch.value(0.5), Value::from("y"));
        assert_eq!(ch.value(0.99), Value::from("z"));
        assert_eq!(ch.numeric(&Value::from("z")), 2.0);
        // the sets: defaults, current, then the sampled ones (no repeated set); integers repeat, so fewer
        let mut cfg = config("bfast", &["max_breaks_trend", "order"], 100);
        cfg.base.insert("order".into(), Value::from(2));
        let sets = build_sets(&cfg).unwrap();
        assert_eq!((sets[0].kind, sets[1].kind), (SetKind::Default, SetKind::Current));
        assert_eq!(sets[0].values, [Value::from(5), Value::from(3)]);
        assert_eq!(sets[1].values, [Value::from(5), Value::from(2)]);
        // 6 x 4 combinations at most, besides the two first
        assert!(sets.len() <= 26 && sets.len() > 10, "{}", sets.len());
        for (i, a) in sets.iter().enumerate().skip(2) {
            assert!(sets[..i].iter().all(|b| b.values != a.values));
        }
    }

    /// Points whose reference is `r` and whose map date is `d` (years), weight `w`.
    fn table(rows: &[(Option<f64>, f64, f64)]) -> (Vec<TunePoint>, Vec<f64>) {
        let pts = rows.iter().map(|&(r, _, w)| TunePoint { reference: r, weight: w, ..Default::default() }).collect();
        (pts, rows.iter().map(|r| r.1).collect())
    }

    #[test]
    fn scores_on_a_table() {
        let nan = f64::NAN;
        let o = ScoreOpts { window: (2000, 2010), tolerance: 1, weighted: false };
        let (pts, dates) = table(&[
            (Some(2003.0), 2003.4, 1.0), // match, same year
            (Some(2004.0), 2005.9, 1.0), // match, 1 year off (within the tolerance)
            (Some(2005.0), 2009.2, 1.0), // match, 4 years off: a date disagreement, not an omission
            (Some(2006.0), nan, 1.0),    // omission
            (Some(2007.0), 2015.5, 1.0), // the map's change is outside the window: omission
            (Some(1995.0), nan, 1.0),    // a reference change outside the window: no change; the map agrees
            (Some(1995.0), 2008.0, 1.0), // ... and the map's change inside the window: commission
            (None, 2002.0, 1.0),         // commission
            (None, nan, 1.0),
            (None, nan, 1.0),
        ]);
        let s = score(&dates, &pts, &o);
        assert_eq!((s.n_ref, s.n_map), (5, 5));
        assert_eq!((s.tp, s.fn_, s.fp), (3.0, 2.0, 2.0));
        assert!((s.omission - 0.4).abs() < 1e-12);
        assert!((s.commission - 0.4).abs() < 1e-12);
        assert!((s.f1 - 6.0 / 10.0).abs() < 1e-12);
        assert!((s.date_agreement - 2.0 / 3.0).abs() < 1e-12);
        assert!((s.mean_abs_error - (0.0 + 1.0 + 4.0) / 3.0).abs() < 1e-12);
        // the dated f1 counts the 4-year miss as an omission and a false change
        assert!((s.f1_dated - 4.0 / (4.0 + 2.0 + 2.0 + 2.0)).abs() < 1e-12);
        // weighted: the points of weight 2 (the first, the omission and a commission)
        let mut w = pts.clone();
        for i in [0, 3, 7] {
            w[i].weight = 2.0;
        }
        let sw = score(&dates, &w, &ScoreOpts { weighted: true, ..o });
        assert_eq!((sw.tp, sw.fn_, sw.fp), (4.0, 3.0, 3.0));
        // a subset, and the empty cases (no reference change; no map change gives commission 0)
        let none = score_subset(&dates, &pts, &[8, 9], &o);
        assert!(none.omission.is_nan() && none.f1.is_nan() && none.commission == 0.0);
        assert_eq!(count_changes(&pts, o.window), 5);
        // NaN scores rank last
        assert_eq!(cmp_scores(&s, &none), Ordering::Less);
    }

    #[test]
    fn pareto_front_of_a_hand_made_set() {
        let pts = [
            (0.1, 0.5), // 0: front
            (0.3, 0.3), // 1: front
            (0.5, 0.1), // 2: front
            (0.4, 0.4), // 3: beaten by 1
            (0.1, 0.6), // 4: beaten by 0
            (f64::NAN, 0.0),
            (0.3, 0.3), // 6: equal to 1: kept
            (0.9, 0.0), // 7: front (no commission)
        ];
        assert_eq!(pareto_front(&pts), vec![0, 1, 6, 2, 7]);
        assert!(pareto_front(&[]).is_empty());
    }

    #[test]
    fn prcc_finds_the_parameter_that_drives_the_score() {
        let mut rng = SplitMix64(9);
        let n = 200;
        // a drives the score (nonlinearly), b is correlated with a but does nothing, c is a dummy
        let x: Vec<Vec<f64>> = (0..n)
            .map(|_| {
                let a = rng.unit();
                vec![a, a + 0.5 * rng.unit(), rng.unit()]
            })
            .collect();
        let y: Vec<f64> = x.iter().map(|r| (3.0 * r[0]).exp() + 0.01 * rng.unit()).collect();
        let r = prcc(&x, &y);
        assert!(r[0] > 0.95, "{r:?}");
        assert!(r[1].abs() < 0.25, "{r:?}");
        assert!(r[2].abs() < 0.25, "{r:?}");
        // a decreasing effect is negative; a NaN set is left out; a parameter that does not vary is NaN
        let mut x2: Vec<Vec<f64>> = x.iter().map(|r| vec![r[0], 1.0]).collect();
        let mut y2: Vec<f64> = y.iter().map(|v| -v).collect();
        x2[0][0] = f64::NAN;
        y2[1] = f64::NAN;
        let r2 = prcc(&x2, &y2);
        assert!(r2[0] < -0.95 && r2[1].is_nan(), "{r2:?}");
        assert!(prcc(&x[..3], &y[..3]).iter().all(|v| v.is_nan()));
        assert_eq!(ranks(&[3.0, 1.0, 3.0, 2.0]), vec![3.5, 1.0, 3.5, 2.0]);
    }

    #[test]
    fn landtrendr_tuning_finds_the_good_region() {
        let ctx = annual_ctx(1990, 2021);
        let pts = synthetic(&ctx, 60, (1998, 2014), &[0.12, 0.2, 0.4], 0.04, 0.0, 5);
        let vary = ["pval_threshold", "max_segments", "spike_threshold", "recovery_threshold"];
        let mut cfg = config("landtrendr", &vary, 40);
        // the user's current values: every fit accepted, whatever its p-value
        cfg.base.insert("pval_threshold".into(), Value::from(0.5));
        let r = go(&cfg, &ctx, &pts);
        assert!(!r.cancelled && r.n_changes == 30 && !r.few_changes);
        let (best, cur) = (&r.sets[r.best().unwrap()], &r.sets[1]);
        assert_eq!(cur.kind, SetKind::Current);
        // the permissive p-value finds changes in the noise of the stable points; a strict one does not
        assert!(cur.scores.commission > 0.3, "{:?}", cur.scores);
        assert!(best.scores.f1_dated > cur.scores.f1_dated + 0.1, "best {:?} vs current {:?}", best.scores, cur.scores);
        assert!(best.scores.commission < 0.2 && best.scores.omission < 0.1, "{:?}", best.scores);
        assert!(num(&best.values[0]) < 0.1, "p-value of the best set: {:?}", best.values);
        // with two segments the drop cannot be isolated: the dates are the series' ends
        assert!(num(&best.values[1]) >= 3.0 && best.scores.date_agreement > 0.5, "{:?}", best.values);
        // the p-value is the parameter that matters for the commission (a higher p-value, more commission)
        let s = r.sensitivity(ScoreKind::Commission);
        assert!(s[0] > 0.5, "{s:?}");
        assert!(r.sensitivity(ScoreKind::F1)[0] < -0.5);
        // the front holds the best set's trade-off, and nothing on it is beaten
        let front = r.pareto();
        assert!(!front.is_empty());
        let pts2: Vec<(f64, f64)> = r.sets.iter().map(|s| (s.scores.omission, s.scores.commission)).collect();
        for &i in &front {
            assert!(!pts2.iter().any(|q| q.0 <= pts2[i].0 && q.1 <= pts2[i].1 && (q.0 < pts2[i].0 || q.1 < pts2[i].1)));
        }
        // the cross-validation: the held-out score is close to what the choice found on the points
        let o = r.optimism(&pts, 5, 1).unwrap();
        assert_eq!((o.k, o.chosen.len(), o.fold_f1.len()), (5, 5, 5));
        assert!(o.held_out.f1_dated > 0.3, "{:?}", o.held_out);
        assert_eq!(o.best, r.best().unwrap());
    }

    #[test]
    fn bfast_tuning_finds_a_window_small_enough_for_an_early_break() {
        // monthly 2005-2016 (144 dates): the break in 2007-2008 is within the first 25% of the series, so a minimum
        // segment of much more than 0.3 of the observations leaves it no room (seen on this data: a window above 0.34
        // finds it late or not at all)
        let ctx = monthly_ctx(2005, 2016);
        let pts = synthetic(&ctx, 30, (2007, 2008), &[0.3], 0.05, 0.1, 6);
        let mut cfg = config("bfast", &["h", "level", "max_breaks_trend", "order"], 24);
        cfg.base.insert("h".into(), Value::from(0.4));
        let r = go(&cfg, &ctx, &pts);
        let (best, cur) = (&r.sets[r.best().unwrap()], &r.sets[1]);
        assert!(best.scores.f1 > 0.9 && best.scores.date_agreement > 0.8, "{:?}", best.scores);
        assert!(num(&best.values[0]) < 0.31 && best.scores.f1_dated == 1.0, "h of the best set: {:?}", best.values);
        assert!(cur.scores.f1_dated + 0.3 < best.scores.f1_dated, "best {:?} vs current {:?}", best.scores, cur.scores);
        // every set with a window above ~0.34 misses the break or dates it wrong
        for s in r.sets.iter().filter(|s| num(&s.values[0]) > 0.34) {
            assert!(s.scores.f1_dated < 0.5, "{:?} {:?}", s.values, s.scores);
        }
    }

    #[test]
    fn cancel_progress_and_cost() {
        let ctx = annual_ctx(1990, 2021);
        let pts = synthetic(&ctx, 10, (1998, 2014), &[0.3], 0.04, 0.0, 2);
        let cfg = config("landtrendr", &["max_segments"], 8);
        let progress = AtomicUsize::new(0);
        let r = run(&cfg, &ctx, &pts, &AtomicBool::new(false), &progress).unwrap();
        assert_eq!(progress.load(atomic::Ordering::Relaxed), r.sets.len());
        assert!(r.few_changes && r.n_changes == 5);
        let stop = AtomicBool::new(true);
        let r = run(&cfg, &ctx, &pts, &stop, &AtomicUsize::new(0)).unwrap();
        assert!(r.cancelled && r.sets.is_empty());
        let t = time_one_set(&cfg, &ctx, &pts).unwrap();
        assert!(t > 0.0);
        assert!((estimate_seconds(2.0, 200, 8) - 50.0).abs() < 1e-12);
        // a tool with nothing to tune, a parameter not tunable, a series of the wrong length
        assert!(run(&config("smooth", &[], 1), &ctx, &pts, &stop, &progress).is_err());
        assert!(build_sets(&config("landtrendr", &["event_type"], 1)).is_err());
        let mut short = pts.clone();
        short[3].values.pop();
        assert!(run(&cfg, &ctx, &short, &stop, &progress).unwrap_err().contains("point 4"));
    }

    #[test]
    fn edited_ranges_replace_the_files() {
        let mut cfg = config("bfast", &["h", "order"], 20);
        let mut h = spec("bfast").unwrap().param("h").unwrap().clone();
        (h.min, h.max) = (Some(0.2), Some(0.25));
        cfg.ranges.insert("h".into(), h);
        let sets = build_sets(&cfg).unwrap();
        for s in sets.iter().filter(|s| s.kind == SetKind::Sampled) {
            assert!((0.2..=0.25).contains(&num(&s.values[0])), "{:?}", s.values);
        }
    }
}
