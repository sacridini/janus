//! An expression over the open layers (Tools > Raster calculator): its names resolved against the layers, a plan
//! of what is read from which layer at which date, and the VRTs that compute it.
//!
//! A layer is named by its name in quotes or its short name (`img`, `s2_l2a`); bare band names are the primary
//! layer's. What follows a layer picks a band of each date (`"S2".N`, `s2.B04`, `s2[4]`); of a stack (one file
//! of several bands, one per layer) a band of the file as one image (`img.band1`). A layer alone is what it shows
//! (its band, index or expression, masked by its quality band).
//!
//! The result follows the dates of the series it uses (or those chosen); a layer of one image is the same at
//! every date, another series is matched to them (the same day, the nearest within some days, the same year or
//! month), absent where none matches. Layers on another grid are resampled onto the result's (the same CRS) or
//! warped (another CRS). A reduction over time (`max(series)`) is a VRT of its own, read as one image.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::canonical::canonical_mapped;
use super::compile::{Body, ClimBy, Member, Over, Period, RedFn, Scope, Slice, Value, compile, is_periods};
use super::names::{BandNames, Resolved};
use super::parse::{CalcError, parse};
use super::program::{BinFn, Builder, DateVars, Program, TernFn};
use super::vrt::{self, Band, Grid, Source};
use crate::{BandMeta, CubeInfo, QaRule};

/// An open layer as the calculator sees it.
#[derive(Clone)]
pub struct CalcLayer {
    pub name: String,
    /// its short name (an identifier)
    pub alias: String,
    pub info: Arc<CubeInfo>,
}

/// A short name for a layer: lowercase letters, digits and _, not one of `taken`.
pub fn alias_of(name: &str, taken: &[String]) -> String {
    let mut s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("_");
    if s.is_empty() || s.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        s = format!("layer_{s}");
    }
    if s.len() > 24 {
        s.truncate(s.char_indices().nth(24).map_or(s.len(), |c| c.0));
        s = s.trim_end_matches('_').to_string();
    }
    if super::funcs::reserved(&s) || super::program::DateVar::of(&s).is_some() || super::funcs::constant(&s).is_some() {
        s = format!("{s}_layer");
    }
    let mut out = s.clone();
    let mut k = 2;
    while taken.contains(&out) {
        out = format!("{s}_{k}");
        k += 1;
    }
    out
}

/// What a layer's members are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerKind {
    /// several bands at each date (a Landsat or STAC series; one date of them)
    Bands,
    /// one file of several bands, a layer each (a stack, a spectrum, a series in one file): a band is one image
    Stack,
    /// one band at each date of files of their own (or one image)
    Plain,
}

pub fn kind(info: &CubeInfo) -> LayerKind {
    if info.bands_per_date > 1 {
        LayerKind::Bands
    } else if info.t() > 1 && info.layers.iter().all(|l| l.path == info.layers[0].path) {
        LayerKind::Stack
    } else {
        LayerKind::Plain
    }
}

/// A layer's members by name: the bands of a date, or the stack's bands (the layer holding each, by file band).
pub fn member_names(info: &CubeInfo) -> BandNames {
    match kind(info) {
        LayerKind::Bands => BandNames::of(info),
        LayerKind::Stack => {
            let n = info.layers.iter().map(|l| l.band).max().unwrap_or(0).max(0) as usize;
            let mut names = vec![String::new(); n];
            let mut nm = vec![0.0; n];
            for l in &info.layers {
                if l.band >= 1 {
                    names[l.band as usize - 1] = l.label.clone();
                    if info.spectral {
                        nm[l.band as usize - 1] = l.time;
                    }
                }
            }
            for (i, s) in names.iter_mut().enumerate() {
                if s.is_empty() {
                    *s = format!("Band {}", i + 1);
                }
            }
            BandNames::new(names, &nm)
        }
        LayerKind::Plain => BandNames::new(vec!["Band 1".into()], &[]),
    }
}

/// What an input of the plan reads.
#[derive(Clone, Debug, PartialEq)]
pub enum Key {
    /// band `band` of layer `layer`'s date (0: the band it shows), or of its layer `fixed` (one image)
    Band { layer: usize, band: i32, raw: bool, fixed: Option<usize> },
    /// the image of a reduction over time
    Stage(usize),
}

/// A reduction over time: its body at each date of `series` (of its slice), reduced by `f` over the dates `over`
/// says: one image, or one per period or per date (a VRT each, a slot).
pub struct Stage {
    pub f: RedFn,
    pub body: Program,
    pub inputs: Vec<Key>,
    pub series: usize,
    /// at(): the one date of the series read (the others are not)
    pub only: Option<usize>,
    pub over: Over,
    pub slice: Option<Slice>,
}

/// Which of a stage's slots a date reads.
#[derive(Clone, Copy, Debug, PartialEq)]
enum SlotOf {
    /// its one image
    Fixed,
    /// its period's (by year, by month)
    Period(Period),
    /// its month of the year's (a climatology by month)
    MonthOfYear,
    /// the slot of its date in the stage's series (windows, lags, persist, a climatology by days of the year)
    Date,
}

/// A stage written: a file per slot (None: no date to reduce there), the slots' keys.
struct StageOut {
    of: SlotOf,
    keys: Vec<i64>,
    files: Vec<Option<String>>,
    series: usize,
}

/// The day of the year of Unix seconds.
fn doy(time: f64) -> i64 {
    let days = (time / 86400.0).floor() as i64;
    let (y, _, _) = crate::dates::civil_from_days(days);
    days - crate::dates::days_from_civil(y, 1, 1) + 1
}

/// How a date finds its slot, and each slot's key and the dates it reduces.
type Slots = (SlotOf, Vec<(i64, Vec<usize>)>);

/// The slots of a stage over `info`'s dates: the dates each one reduces, its key, and how a date finds its slot.
fn slots(st: &Stage, info: &CubeInfo) -> Slots {
    let n = info.t();
    let time = |t: usize| info.layers[t].time;
    let idx: Vec<usize> =
        (0..n).filter(|&t| st.slice.is_none_or(|s| !info.time_is_date || s.contains(time(t)))).collect();
    let ymd = |t: usize| crate::dates::civil_from_days((time(t) / 86400.0).floor() as i64);
    // the last dates of at most this many (lags and persist look back this far)
    let recent = |v: Vec<usize>, k: usize| {
        let keep = 4 * k + 8;
        v[v.len().saturating_sub(keep)..].to_vec()
    };
    if let Some(t) = st.only {
        return (SlotOf::Fixed, vec![(0, vec![t])]);
    }
    match st.over {
        Over::All => (SlotOf::Fixed, vec![(0, idx)]),
        Over::By(p) => {
            let mut out: Vec<(i64, Vec<usize>)> = Vec::new();
            for &t in &idx {
                let k = p.key(time(t));
                match out.iter_mut().find(|s| s.0 == k) {
                    Some(s) => s.1.push(t),
                    None => out.push((k, vec![t])),
                }
            }
            out.sort_by_key(|s| s.0);
            (SlotOf::Period(p), out)
        }
        Over::Window(days) => (
            SlotOf::Date,
            (0..n)
                .map(|t| {
                    let w: Vec<usize> = idx
                        .iter()
                        .copied()
                        .filter(|&d| time(d) <= time(t) && time(d) > time(t) - days * 86400.0)
                        .collect();
                    (t as i64, w)
                })
                .collect(),
        ),
        Over::LagTime { days, years } => {
            // within half the series' usual step, at least 8 days
            let mut steps: Vec<f64> = (1..n).map(|t| time(t) - time(t - 1)).collect();
            steps.sort_by(f64::total_cmp);
            let tol = steps.get(steps.len() / 2).map_or(8.0 * 86400.0, |s| (s / 2.0).max(8.0 * 86400.0));
            (
                SlotOf::Date,
                (0..n)
                    .map(|t| {
                        let target = if years > 0 {
                            let (y, m, d) = ymd(t);
                            crate::dates::days_from_civil(y - years, m, d) as f64 * 86400.0
                        } else {
                            time(t) - days * 86400.0
                        };
                        let near = idx
                            .iter()
                            .copied()
                            .filter(|&d| d != t && (time(d) - target).abs() <= tol)
                            .min_by(|&a, &b| (time(a) - target).abs().total_cmp(&(time(b) - target).abs()));
                        (t as i64, near.into_iter().collect())
                    })
                    .collect(),
            )
        }
        Over::LagObs(k) => (
            SlotOf::Date,
            (0..n).map(|t| (t as i64, recent(idx.iter().copied().filter(|&d| d < t).collect(), k))).collect(),
        ),
        Over::Upto(k) => (
            SlotOf::Date,
            (0..n).map(|t| (t as i64, recent(idx.iter().copied().filter(|&d| d <= t).collect(), k))).collect(),
        ),
        Over::Clim { from, to, by } => {
            let base: Vec<usize> = idx.iter().copied().filter(|&t| (from..=to).contains(&ymd(t).0)).collect();
            match by {
                ClimBy::Month => (
                    SlotOf::MonthOfYear,
                    (1..=12)
                        .map(|m| (m, base.iter().copied().filter(|&t| i64::from(ymd(t).1) == m).collect()))
                        .collect(),
                ),
                ClimBy::Doy(w) => (
                    SlotOf::Date,
                    (0..n)
                        .map(|t| {
                            let near = |d: usize| {
                                let a = (doy(time(d)) - doy(time(t))).abs() as f64;
                                a.min(365.0 - a) <= w
                            };
                            (t as i64, base.iter().copied().filter(|&d| near(d)).collect())
                        })
                        .collect(),
                ),
            }
        }
    }
}

struct CalcScope<'a> {
    layers: &'a [CalcLayer],
    names: Vec<BandNames>,
    primary: Option<usize>,
    apply_qa: bool,
    stages: Vec<Stage>,
}

fn qa_code(r: QaRule) -> f32 {
    r.code() as f32
}

impl CalcScope<'_> {
    /// at()'s date of a series: the one in that year, month or day; none or several are an error that says which.
    fn date_at(
        &self,
        layer: usize,
        (year, month, day): (u16, u8, u8),
        span: &Range<usize>,
    ) -> Result<usize, CalcError> {
        let l = &self.layers[layer];
        let info = &l.info;
        if !info.time_is_date {
            return Err(CalcError::new(span.clone(), format!("the dates of \"{}\" are not known", l.name)));
        }
        let ymd = |t: usize| {
            let (y, m, d) = crate::dates::civil_from_days((info.layers[t].time / 86400.0).floor() as i64);
            (y, m, d)
        };
        let hits: Vec<usize> = (0..info.t())
            .filter(|&t| {
                let (y, m, d) = ymd(t);
                y == i32::from(year) && (month == 0 || m == u32::from(month)) && (day == 0 || d == u32::from(day))
            })
            .collect();
        let when = match (month, day) {
            (0, _) => year.to_string(),
            (m, 0) => format!("{year}-{m:02}"),
            (m, d) => format!("{year}-{m:02}-{d:02}"),
        };
        match hits.as_slice() {
            [t] => Ok(*t),
            [] => Err(CalcError::new(
                span.clone(),
                format!(
                    "\"{}\" has no date in {when} (its dates: {} to {})",
                    l.name,
                    info.layers[0].label,
                    info.layers[info.t() - 1].label
                ),
            )),
            many => Err(CalcError::new(
                span.clone(),
                format!(
                    "\"{}\" has {} dates in {when} ({} to {}): give its month or day",
                    l.name,
                    many.len(),
                    info.layers[many[0]].label,
                    info.layers[many[many.len() - 1]].label
                ),
            )),
        }
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.layers
            .iter()
            .position(|l| l.name == name)
            .or_else(|| self.layers.iter().position(|l| l.alias == name))
            .or_else(|| self.layers.iter().position(|l| l.name.eq_ignore_ascii_case(name)))
            .or_else(|| self.layers.iter().position(|l| l.alias.eq_ignore_ascii_case(name)))
    }

    fn series_of(&self, layer: usize) -> Option<usize> {
        (self.layers[layer].info.t() > 1).then_some(layer)
    }

    /// Band `band` of a layer's dates, masked by its quality band (if asked to and it has one).
    fn band(&self, b: &mut Builder<Key>, layer: usize, band: i32, masked: bool) -> Value {
        let info = &self.layers[layer].info;
        let v = b.input(Key::Band { layer, band, raw: false, fixed: None });
        let sel = &info.sel;
        let v = if masked && sel.qa_band > 0 && sel.qa_rule != QaRule::None && band != sel.qa_band {
            let q = b.input(Key::Band { layer, band: sel.qa_band, raw: true, fixed: None });
            let r = b.konst(qa_code(sel.qa_rule));
            b.tern(TernFn::Qa, v, q, r)
        } else {
            v
        };
        Value { node: v, series: self.series_of(layer) }
    }

    /// What a layer shows at each date: its band, normalized difference or expression, masked.
    fn shown(&mut self, b: &mut Builder<Key>, layer: usize, span: &Range<usize>) -> Result<Value, CalcError> {
        let info = self.layers[layer].info.clone();
        if kind(&info) != LayerKind::Bands {
            let v = b.input(Key::Band { layer, band: 0, raw: false, fixed: None });
            return Ok(Value { node: v, series: self.series_of(layer) });
        }
        let sel = info.sel;
        let v = if let Some(e) = &info.expr {
            // the layer's own expression, over its bands here
            let ast = parse(&e.text).map_err(|x| CalcError::new(span.clone(), format!("its expression: {x}")))?;
            let mut inner = Inner { names: &self.names[layer], layer };
            let taken = std::mem::take(b);
            let c = super::compile::compile_with(&ast, &mut inner, taken)
                .map_err(|x| CalcError::new(span.clone(), format!("its expression: {x}")))?;
            *b = c.builder;
            c.value.node
        } else if sel.nd_band > 0 {
            let x = b.input(Key::Band { layer, band: sel.band, raw: false, fixed: None });
            let y = b.input(Key::Band { layer, band: sel.nd_band, raw: false, fixed: None });
            b.bin(BinFn::Nd, x, y)
        } else {
            b.input(Key::Band { layer, band: sel.band, raw: false, fixed: None })
        };
        let v = if sel.qa_band > 0 && sel.qa_rule != QaRule::None {
            let q = b.input(Key::Band { layer, band: sel.qa_band, raw: true, fixed: None });
            let r = b.konst(qa_code(sel.qa_rule));
            b.tern(TernFn::Qa, v, q, r)
        } else {
            v
        };
        Ok(Value { node: v, series: self.series_of(layer) })
    }

    /// A member of a layer resolved: a band of its dates, or a stack's band as one image.
    fn member(
        &mut self,
        b: &mut Builder<Key>,
        layer: usize,
        r: Resolved,
        id: &str,
        span: &Range<usize>,
    ) -> Result<Value, CalcError> {
        let names = &self.names[layer];
        let lname = &self.layers[layer].name;
        let band = match r {
            Resolved::Band(k) => k,
            Resolved::MissingRole(code) => {
                let what = super::names::role(code).map_or(code, |r| r.name);
                return Err(CalcError::new(span.clone(), format!("\"{lname}\" has no band that is {what} ({code})")));
            }
            Resolved::Ambiguous(v) => {
                return Err(CalcError::new(
                    span.clone(),
                    format!("'{id}' could be {} of \"{lname}\": write one of them", names.candidates(&v)),
                ));
            }
        };
        let info = self.layers[layer].info.clone();
        match kind(&info) {
            LayerKind::Bands => Ok(self.band(b, layer, band, self.apply_qa)),
            LayerKind::Stack => {
                let t = info
                    .layers
                    .iter()
                    .position(|l| l.band == band)
                    .ok_or_else(|| CalcError::new(span.clone(), format!("\"{lname}\" has no band {band}")))?;
                let v = b.input(Key::Band { layer, band: 0, raw: false, fixed: Some(t) });
                Ok(Value { node: v, series: None })
            }
            LayerKind::Plain => {
                if band != 1 {
                    return Err(CalcError::new(span.clone(), format!("\"{lname}\" has one band")));
                }
                let _ = names;
                self.shown(b, layer, span)
            }
        }
    }
}

/// A layer's own expression compiled over its bands (as the plan's inputs).
struct Inner<'a> {
    names: &'a BandNames,
    layer: usize,
}

impl Scope for Inner<'_> {
    type K = Key;

    fn name(&mut self, b: &mut Builder<Key>, name: &str, span: &Range<usize>) -> Option<Result<Value, CalcError>> {
        let r = self.names.resolve(name)?;
        Some(match r {
            Resolved::Band(band) => Ok(Value {
                node: b.input(Key::Band { layer: self.layer, band, raw: false, fixed: None }),
                series: Some(self.layer),
            }),
            _ => Err(CalcError::new(span.clone(), format!("'{name}' is not a band of the layer"))),
        })
    }
}

impl Scope for CalcScope<'_> {
    type K = Key;

    fn name(&mut self, b: &mut Builder<Key>, name: &str, span: &Range<usize>) -> Option<Result<Value, CalcError>> {
        if let Some(l) = self.layers.iter().position(|l| l.alias == name) {
            return Some(self.shown(b, l, span));
        }
        let p = self.primary?;
        let r = self.names[p].resolve(name)?;
        Some(self.member(b, p, r, name, span))
    }

    fn is_layer(&self, name: &str) -> bool {
        self.layers.iter().any(|l| l.alias == name)
    }

    fn layer(
        &mut self,
        b: &mut Builder<Key>,
        layer: &str,
        span: &Range<usize>,
        member: Option<Member>,
    ) -> Result<Value, CalcError> {
        let Some(l) = self.find(layer) else {
            let names: Vec<String> = self.layers.iter().map(|l| format!("\"{}\"", l.name)).collect();
            return Err(CalcError::new(
                span.clone(),
                if names.is_empty() {
                    format!("no layer \"{layer}\": open one first")
                } else {
                    format!("no layer \"{layer}\" (open: {})", names.join(", "))
                },
            ));
        };
        match member {
            None => self.shown(b, l, span),
            Some(Member::Name(m, mspan)) => {
                let r = self.names[l].resolve(m).ok_or_else(|| {
                    let mut s = self.names[l].suggestions();
                    s.truncate(12);
                    CalcError::new(
                        mspan.clone(),
                        format!("\"{}\" has no band '{m}' (its bands: {})", self.layers[l].name, s.join(", ")),
                    )
                })?;
                self.member(b, l, r, m, &mspan)
            }
            Some(Member::Index(k, ispan)) => {
                let n = self.names[l].len() as i64;
                if k < 1 || k > n {
                    return Err(CalcError::new(ispan, format!("\"{}\" has bands 1 to {n}", self.layers[l].name)));
                }
                self.member(b, l, Resolved::Band(k as i32), &k.to_string(), &ispan)
            }
        }
    }

    fn reduce(
        &mut self,
        b: &mut Builder<Key>,
        f: RedFn,
        body: Body<Key>,
        span: &Range<usize>,
    ) -> Result<Value, CalcError> {
        if !is_periods(body.series) {
            let l = &self.layers[body.series];
            let by_date = body.slice.is_some()
                || matches!(body.over, Over::By(_) | Over::Window(_) | Over::LagTime { .. } | Over::Clim { .. });
            if by_date && !l.info.time_is_date {
                return Err(CalcError::new(span.clone(), format!("the dates of \"{}\" are not known", l.name)));
            }
        } else if matches!(f, RedFn::At { .. }) {
            return Err(CalcError::new(span.clone(), "at() takes a date of a layer's series"));
        }
        let only = match f {
            RedFn::At { year, month, day } => Some(self.date_at(body.series, (year, month, day), span)?),
            _ => None,
        };
        self.stages.push(Stage {
            f,
            body: body.program,
            inputs: body.inputs,
            series: body.series,
            only,
            over: body.over,
            slice: body.slice,
        });
        Ok(Value { node: b.input(Key::Stage(self.stages.len() - 1)), series: None })
    }

    fn suggestions(&self) -> Vec<String> {
        let mut v: Vec<String> = self.layers.iter().map(|l| l.alias.clone()).collect();
        if let Some(p) = self.primary {
            v.extend(self.names[p].suggestions());
        }
        v
    }
}

/// How another series' dates are matched to the result's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DateMatch {
    /// the same day
    Exact,
    /// the nearest date within this many days
    Nearest(f64),
    SameYear,
    SameMonth,
}

impl DateMatch {
    pub fn label(self) -> String {
        match self {
            DateMatch::Exact => "the same day".into(),
            DateMatch::Nearest(d) => format!("the nearest date within {d} days"),
            DateMatch::SameYear => "the same year".into(),
            DateMatch::SameMonth => "the same month".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    /// whose bands bare names are
    pub primary: Option<usize>,
    /// the layer whose dates the result follows (None: the first series it uses)
    pub dates_from: Option<usize>,
    /// the layer whose grid the result is on (None: the layer of its dates, else the first it uses)
    pub grid_from: Option<usize>,
    pub matching: DateMatch,
    /// how layers on another grid are resampled: nearest, bilinear, cubic, average, mode
    pub resampling: String,
    /// bands named of a layer with a quality band are masked by it
    pub apply_qa: bool,
    /// the reductions read by several dates (`s - mean(s)`, a climatology) computed once into GeoTIFFs when the
    /// result is written: for a result read whole (written out as GeoTIFFs), not one the map reads a view of
    pub materialize: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            primary: None,
            dates_from: None,
            grid_from: None,
            matching: DateMatch::Nearest(8.0),
            resampling: "nearest".into(),
            apply_qa: true,
            materialize: false,
        }
    }
}

/// An expression planned over the layers.
pub struct Plan {
    pub program: Program,
    pub inputs: Vec<Key>,
    pub stages: Vec<Stage>,
    /// the layer whose dates the result follows (None: one image)
    pub series: Option<usize>,
    pub grid: usize,
    /// the name the text gives its result
    pub name: Option<String>,
    /// the layers it reads, in order
    pub used: Vec<usize>,
    /// the series of periods (`by year`, `by month`): their dates, as a series' (a grid of the layer reduced)
    pub periods: Vec<(usize, Arc<CubeInfo>)>,
}

fn key_layer(k: &Key) -> Option<usize> {
    match k {
        Key::Band { layer, .. } => Some(*layer),
        Key::Stage(_) => None,
    }
}

/// Compiles a text over the layers.
pub fn plan(text: &str, layers: &[CalcLayer], opts: &Options) -> Result<Plan, CalcError> {
    let ast = parse(text)?;
    let mut scope = CalcScope {
        layers,
        names: layers.iter().map(|l| member_names(&l.info)).collect(),
        primary: opts.primary.filter(|&p| p < layers.len()),
        apply_qa: opts.apply_qa,
        stages: Vec::new(),
    };
    let c = compile(&ast, &mut scope)?;
    let program = c.program();
    let inputs = c.builder.inputs.clone();
    let mut used: Vec<usize> = Vec::new();
    for k in inputs.iter().chain(scope.stages.iter().flat_map(|s| s.inputs.iter())) {
        if let Some(l) = key_layer(k)
            && !used.contains(&l)
        {
            used.push(l);
        }
    }
    // the periods' dates: those of the dates reduced by year (month), every stage's together
    let mut periods = Vec::new();
    for p in [Period::Year, Period::Month] {
        let by: Vec<&Stage> = scope.stages.iter().filter(|s| s.over == Over::By(p)).collect();
        let Some(first) = by.first() else { continue };
        let mut keys: Vec<i64> = by
            .iter()
            .flat_map(|s| {
                let info = &layers[s.series].info;
                info.layers.iter().filter(|l| s.slice.is_none_or(|x| x.contains(l.time))).map(|l| p.key(l.time))
            })
            .collect();
        keys.sort();
        keys.dedup();
        if keys.is_empty() {
            return Err(CalcError::new(0..text.len(), "no date in the slice"));
        }
        let mut info = (*layers[first.series].info).clone();
        info.layers = keys
            .iter()
            .map(|&k| {
                let (time, label) = p.start(k);
                crate::Layer { time, label, ..Default::default() }
            })
            .collect();
        info.granularity = if p == Period::Year { crate::TimeGranularity::Year } else { crate::TimeGranularity::Month };
        info.time_is_date = true;
        periods.push((p.series(), Arc::new(info)));
    }
    let series = c.value.series.map(|s| {
        if is_periods(s) {
            s
        } else {
            opts.dates_from.filter(|&d| d < layers.len() && layers[d].info.t() > 1).unwrap_or(s)
        }
    });
    let grid = opts
        .grid_from
        .filter(|&g| g < layers.len())
        .or(series.filter(|&s| !is_periods(s)))
        .or_else(|| used.first().copied())
        .or(scope.primary)
        .ok_or_else(|| CalcError::new(0..text.len(), "the expression reads no layer"))?;
    Ok(Plan { program, inputs, stages: scope.stages, series, grid, name: c.name, used, periods })
}

// ---------------------------------------------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------------------------------------------

/// The layer of `info` matched to a time of the result's series (`index`: its place there).
pub fn match_date(info: &CubeInfo, time: f64, index: usize, dated: bool, m: DateMatch) -> Option<usize> {
    if info.t() == 1 {
        return Some(0);
    }
    if !(dated && info.time_is_date) {
        // series without dates: by place
        return (index < info.t()).then_some(index);
    }
    let day = |t: f64| (t / 86400.0).floor() as i64;
    let (y, mo, _) = crate::dates::civil_from_days(day(time));
    let mut best: Option<(f64, usize)> = None;
    for (i, l) in info.layers.iter().enumerate() {
        let d = (l.time - time).abs();
        let (ly, lm, _) = crate::dates::civil_from_days(day(l.time));
        let ok = match m {
            DateMatch::Exact => day(l.time) == day(time),
            DateMatch::Nearest(days) => d <= days * 86400.0 + 1.0,
            DateMatch::SameYear => ly == y,
            DateMatch::SameMonth => ly == y && lm == mo,
        };
        if ok && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, i));
        }
    }
    best.map(|b| b.1)
}

/// How a layer is matched to the result's dates: (dates matched, of how many).
pub fn match_stats(plan_series: Option<usize>, layers: &[CalcLayer], layer: usize, m: DateMatch) -> (usize, usize) {
    let Some(r) = plan_series.filter(|&r| !is_periods(r)) else { return (1, 1) };
    let rinfo = &layers[r].info;
    let info = &layers[layer].info;
    let dated = rinfo.time_is_date;
    let n = (0..rinfo.t()).filter(|&t| match_date(info, rinfo.layers[t].time, t, dated, m).is_some()).count();
    (n, rinfo.t())
}

// ---------------------------------------------------------------------------------------------------------------
// Grids
// ---------------------------------------------------------------------------------------------------------------

fn same_crs(a: &CubeInfo, b: &CubeInfo) -> bool {
    if a.crs_wkt.is_empty() || b.crs_wkt.is_empty() {
        return a.crs_wkt.is_empty() && b.crs_wkt.is_empty();
    }
    if !a.crs_authority.is_empty() && a.crs_authority == b.crs_authority {
        return true;
    }
    if a.crs_wkt == b.crs_wkt {
        return true;
    }
    match (gdal::spatial_ref::SpatialRef::from_wkt(&a.crs_wkt), gdal::spatial_ref::SpatialRef::from_wkt(&b.crs_wkt)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// How a layer goes onto the result's grid.
#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    /// the same grid
    Same,
    /// the same CRS, another grid: the source window and where it goes
    Window([f64; 4], [f64; 4]),
    /// another CRS: warped
    Warp,
    /// does not meet the grid
    Outside,
}

/// Where layer `info` goes on the grid of `grid`.
pub fn placement(info: &CubeInfo, grid: &CubeInfo) -> Result<Placement, String> {
    let (Some(g), Some(r)) = (info.geo_transform, grid.geo_transform) else {
        return if info.width == grid.width && info.height == grid.height {
            Ok(Placement::Same)
        } else {
            Err("a layer without coordinates has another size than the result's".into())
        };
    };
    if !same_crs(info, grid) {
        return Ok(Placement::Warp);
    }
    if g == r && info.width == grid.width && info.height == grid.height {
        return Ok(Placement::Same);
    }
    if g[2] != 0.0 || g[4] != 0.0 || r[2] != 0.0 || r[4] != 0.0 {
        return Ok(Placement::Warp);
    }
    // the result's grid in the layer's pixels, clipped to the layer
    let (sx, sy) = ((r[0] - g[0]) / g[1], (r[3] - g[3]) / g[5]);
    let (kx, ky) = (r[1] / g[1], r[5] / g[5]);
    let (sw, sh) = (grid.width as f64 * kx, grid.height as f64 * ky);
    let (cx0, cy0) = (sx.max(0.0), sy.max(0.0));
    let (cx1, cy1) = ((sx + sw).min(info.width as f64), (sy + sh).min(info.height as f64));
    if cx1 <= cx0 || cy1 <= cy0 || kx <= 0.0 || ky <= 0.0 {
        return Ok(Placement::Outside);
    }
    Ok(Placement::Window(
        [cx0, cy0, cx1 - cx0, cy1 - cy0],
        [(cx0 - sx) / kx, (cy0 - sy) / ky, (cx1 - cx0) / kx, (cy1 - cy0) / ky],
    ))
}

// ---------------------------------------------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------------------------------------------

/// What was written.
pub struct Written {
    /// the result's VRTs, a date each, in order
    pub files: Vec<String>,
    pub dir: PathBuf,
    /// the layers warped onto the grid
    pub warped: usize,
}

/// The band metadata (scale, offset, no data) of the bands read, opened once per file.
struct Metas {
    cache: HashMap<(String, i32), BandMeta>,
}

impl Metas {
    fn get(&mut self, path: &str, band: i32) -> BandMeta {
        if let Some(m) = self.cache.get(&(path.to_string(), band)) {
            return *m;
        }
        let m = crate::open_raster(path)
            .ok()
            .and_then(|ds| ds.rasterband(band as usize).ok().map(|b| crate::band_meta(&b)))
            .unwrap_or_default();
        self.cache.insert((path.to_string(), band), m);
        m
    }

    /// Opens the files of the bands that will be asked for, several at a time (each open of a network file is a few
    /// round trips).
    fn prefetch(&mut self, wanted: Vec<(String, i32)>) {
        let todo: Vec<(String, i32)> = wanted.into_iter().filter(|k| !self.cache.contains_key(k)).collect();
        for chunk in todo.chunks(16) {
            let got: Vec<((String, i32), BandMeta)> = std::thread::scope(|s| {
                let hs: Vec<_> = chunk
                    .iter()
                    .map(|(p, b)| {
                        let (p, b) = (p.clone(), *b);
                        s.spawn(move || {
                            let m = crate::open_raster(&p)
                                .ok()
                                .and_then(|ds| ds.rasterband(b as usize).ok().map(|x| crate::band_meta(&x)))
                                .unwrap_or_default();
                            ((p, b), m)
                        })
                    })
                    .collect();
                hs.into_iter().filter_map(|h| h.join().ok()).collect()
            });
            self.cache.extend(got);
        }
    }
}

struct Writer<'a> {
    layers: &'a [CalcLayer],
    grid_info: &'a CubeInfo,
    opts: &'a Options,
    dir: PathBuf,
    places: HashMap<usize, Placement>,
    /// warped VRT templates by (layer, no data, resampling)
    templates: HashMap<String, String>,
    warped: usize,
    metas: Metas,
}

impl Writer<'_> {
    /// The source of band `band` (0: the shown) of layer `layer`'s date `t`.
    fn source(&mut self, layer: usize, band: i32, raw: bool, t: usize) -> Result<Option<Source>, String> {
        let info = self.layers[layer].info.clone();
        let l = &info.layers[t];
        let file_band = if band <= 0 { l.band } else { band + l.band_base };
        let meta = if raw {
            BandMeta::default()
        } else if band <= 0 || file_band == l.band {
            l.meta
        } else if band == info.sel.nd_band {
            l.nd_meta
        } else {
            self.metas.get(&l.path, file_band)
        };
        // categorical layers and quality codes are never interpolated
        let categorical = raw || info.file_categorical;
        let resampling =
            if categorical && self.opts.resampling != "mode" { "nearest" } else { self.opts.resampling.as_str() };
        let place = self.places.get(&layer).cloned().unwrap_or(Placement::Same);
        let path = l.path.clone();
        Ok(match place {
            Placement::Same => Some(Source { path, band: file_band, meta, raw, rects: None, resampling: None }),
            Placement::Outside => None,
            Placement::Window(s, d) => Some(Source {
                path,
                band: file_band,
                meta,
                raw,
                rects: Some((s, d)),
                resampling: Some(resampling.into()),
            }),
            Placement::Warp => {
                let warp =
                    self.warp(&info, layer, &path, file_band, if raw { None } else { meta.no_data }, resampling, t)?;
                // the warped VRT holds the band as band 1, already on the grid; its no data stays the band's
                Some(Source { path: warp, band: 1, meta, raw, rects: None, resampling: None })
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn warp(
        &mut self,
        info: &CubeInfo,
        layer: usize,
        path: &str,
        band: i32,
        nodata: Option<f64>,
        resampling: &str,
        t: usize,
    ) -> Result<String, String> {
        let r = match resampling {
            "nearest" => "near",
            r => r,
        };
        let key = format!("{layer} {nodata:?} {r}");
        if !self.templates.contains_key(&key) {
            let g = info.geo_transform.ok_or("a layer to warp has no coordinates")?;
            let geom = crate::stac_vrt::Geom {
                crs: info.crs_wkt.clone(),
                gt: g,
                w: info.width,
                h: info.height,
                dtype: if info.data_type.is_empty() { "Float32".into() } else { info.data_type.clone() },
                nodata,
            };
            let gi = self.grid_info;
            let grid = crate::stac_vrt::Grid {
                crs: gi.crs_wkt.clone(),
                gt: gi.geo_transform.ok_or("the result's grid has no coordinates")?,
                w: gi.width,
                h: gi.height,
            };
            let tpl = crate::stac_vrt::warp_template(&geom, &grid, r)?;
            self.templates.insert(key.clone(), tpl);
        }
        let dir = self.dir.join("warp");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let f = dir.join(format!("{}_{t:04}_b{band}_{}.vrt", self.layers[layer].alias, self.templates.len()));
        let src = format!("<SourceDataset relativeToVRT=\"0\">{}</SourceDataset>", xml_escape(path));
        let xml = self.templates[&key]
            .replace("{SOURCE}", &src)
            .replace("<BandMapping src=\"1\"", &format!("<BandMapping src=\"{band}\""));
        std::fs::write(&f, xml).map_err(|e| format!("{}: {e}", f.display()))?;
        self.warped += 1;
        Ok(f.to_string_lossy().into_owned())
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The date variables of a layer's date.
pub fn date_vars(info: &CubeInfo, t: usize) -> DateVars {
    DateVars::new(info.time_is_date.then(|| info.layers[t].time), t)
}

/// The text of a date as a file's name, as open_cube reads it back.
fn date_stem(info: &CubeInfo, t: usize) -> String {
    if info.time_is_date {
        let g = info.granularity.max(crate::TimeGranularity::Year);
        crate::dates::format_date(info.layers[t].time, g).replace([' ', ':'], "_")
    } else {
        format!("t{:04}", t + 1)
    }
}

/// Images computed at once by `materialize`, each on one thread (GDAL reads a VRT on its caller's), compressed on
/// two: eight threads at most.
const MATERIALIZE_THREADS: usize = 4;

/// A stage's VRTs computed into tiled GeoTIFFs beside them (`.tif` for `.vrt`), read once each by blocks: what reads
/// them then reads an image, not the reduction again.
fn materialize(files: Vec<Option<String>>, cancel: &AtomicBool) -> Result<Vec<Option<String>>, String> {
    let next = std::sync::atomic::AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let out: Vec<std::sync::Mutex<Option<Result<String, String>>>> =
        files.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|sc| {
        for _ in 0..MATERIALIZE_THREADS.min(files.len()) {
            sc.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() || failed.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let Some(f) = &files[i] else { continue };
                    let tif = Path::new(f).with_extension("tif");
                    let r = vrt::with_block_reads(true, || {
                        let src = gdal::Dataset::open(f).map_err(|e| format!("{f}: {e}"))?;
                        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
                        let co = gdal::raster::RasterCreationOptions::from_iter([
                            "TILED=YES",
                            "COMPRESS=DEFLATE",
                            "ZLEVEL=1",
                            "NUM_THREADS=2",
                            "BIGTIFF=IF_SAFER",
                        ]);
                        src.create_copy(&drv, &tif, &co).map_err(|e| format!("{}: {e}", tif.display()))?;
                        Ok(tif.to_string_lossy().into_owned())
                    });
                    if r.is_err() {
                        failed.store(true, Ordering::Relaxed);
                    }
                    *out[i].lock().unwrap() = Some(r);
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let out: Vec<Option<Result<String, String>>> = out.into_iter().map(|o| o.into_inner().unwrap()).collect();
    // the failure that stopped the others
    if let Some(Some(Err(e))) = out.iter().find(|o| matches!(o, Some(Err(_)))) {
        return Err(e.clone());
    }
    Ok(out.into_iter().map(|o| o.and_then(Result::ok)).collect())
}

impl Plan {
    /// The dates of a series: a layer's, or those of periods (by year).
    pub fn series_info(&self, layers: &[CalcLayer], s: usize) -> Arc<CubeInfo> {
        match self.periods.iter().find(|p| p.0 == s) {
            Some(p) => p.1.clone(),
            None => layers[s].info.clone(),
        }
    }

    /// The result's dates: (time, label) of each, one entry for an image.
    pub fn dates(&self, layers: &[CalcLayer]) -> Vec<(f64, String)> {
        match self.series {
            Some(r) => self.series_info(layers, r).layers.iter().map(|l| (l.time, l.label.clone())).collect(),
            None => vec![(0.0, String::new())],
        }
    }

    /// The source of stage `out` at a date (`time`, its place `t` in its own series): its slot's file.
    fn stage_source(
        &self,
        layers: &[CalcLayer],
        out: &StageOut,
        time: f64,
        t: usize,
        opts: &Options,
    ) -> Option<Source> {
        let slot = match out.of {
            SlotOf::Fixed => Some(0),
            SlotOf::Period(p) => out.keys.iter().position(|&k| k == p.key(time)),
            SlotOf::MonthOfYear => {
                let m = crate::dates::civil_from_days((time / 86400.0).floor() as i64).1;
                out.keys.iter().position(|&k| k == i64::from(m))
            }
            SlotOf::Date => {
                let info = self.series_info(layers, out.series);
                match_date(&info, time, t, info.time_is_date, opts.matching)
            }
        };
        let path = slot.and_then(|s| out.files.get(s).cloned().flatten())?;
        Some(Source { path, band: 1, meta: BandMeta::default(), raw: true, rects: None, resampling: None })
    }

    /// Writes the result's VRTs into `dir` (a date each, `<date>.vrt`; reductions in stages/, warped layers in
    /// warp/), the text and the plan in calc.json.
    pub fn write(
        &self,
        text: &str,
        layers: &[CalcLayer],
        opts: &Options,
        dir: &Path,
        progress: &dyn Fn(f64, &str),
        cancel: &AtomicBool,
    ) -> Result<Written, String> {
        vrt::register();
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let grid_info = layers[self.grid].info.clone();
        let mut places = HashMap::new();
        for &l in &self.used {
            let p = placement(&layers[l].info, &grid_info).map_err(|e| format!("\"{}\": {e}", layers[l].name))?;
            if p == Placement::Outside {
                return Err(format!(
                    "\"{}\" does not overlap the result's grid (\"{}\")",
                    layers[l].name, layers[self.grid].name
                ));
            }
            places.insert(l, p);
        }
        let mut w = Writer {
            layers,
            grid_info: &grid_info,
            opts,
            dir: dir.to_path_buf(),
            places,
            templates: HashMap::new(),
            warped: 0,
            metas: Metas { cache: HashMap::new() },
        };
        // band metadata of other bands than those shown, opened together beforehand
        let mut wanted = Vec::new();
        for k in self.inputs.iter().chain(self.stages.iter().flat_map(|s| s.inputs.iter())) {
            if let Key::Band { layer, band, raw: false, .. } = k {
                let info = &layers[*layer].info;
                for l in &info.layers {
                    let fb = if *band <= 0 { l.band } else { band + l.band_base };
                    if *band > 0 && fb != l.band && *band != info.sel.nd_band {
                        wanted.push((l.path.clone(), fb));
                    }
                }
            }
        }
        wanted.sort();
        wanted.dedup();
        if !wanted.is_empty() {
            progress(0.0, &format!("reading the metadata of {} bands", wanted.len()));
            w.metas.prefetch(wanted);
        }
        let grid = Grid {
            w: grid_info.width,
            h: grid_info.height,
            gt: grid_info.geo_transform,
            srs: crate::vrt::dataset_srs(&grid_info),
        };
        // the reductions, each a VRT of one band over the dates of its series of each slot (one, or one per period or
        // per date)
        let mut outs: Vec<StageOut> = Vec::new();
        let mut stage_block = (usize::MAX, usize::MAX);
        // how often each stage's slots are read: one read more often than it has slots (a mean under every date, a
        // climatology under each date of its month) is computed once, into GeoTIFFs, when materializing
        let all_slots: Vec<Slots> =
            self.stages.iter().map(|st| slots(st, &self.series_info(layers, st.series))).collect();
        let mut reads = vec![0usize; self.stages.len()];
        let n_dates = self.dates(layers).len();
        for k in &self.inputs {
            if let Key::Stage(j) = k {
                reads[*j] += n_dates;
            }
        }
        for (st, (_, sl)) in self.stages.iter().zip(&all_slots) {
            for k in &st.inputs {
                if let Key::Stage(j) = k {
                    reads[*j] += if all_slots[*j].0 == SlotOf::Fixed {
                        sl.iter().filter(|s| !s.1.is_empty()).count()
                    } else {
                        sl.iter().map(|s| s.1.len()).sum()
                    };
                }
            }
        }
        for (i, st) in self.stages.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            progress(
                0.1 * i as f64 / self.stages.len() as f64,
                &format!("reduction {} of {}", i + 1, self.stages.len()),
            );
            let sinfo = self.series_info(layers, st.series);
            let dated = sinfo.time_is_date;
            // inputs read once (images, reductions to one image) first, then those of each date
            let is_capture = |k: &Key| match k {
                Key::Stage(j) => outs[*j].of == SlotOf::Fixed,
                Key::Band { fixed: Some(_), .. } => true,
                Key::Band { layer, .. } => layers[*layer].info.t() == 1,
            };
            let caps: Vec<usize> = (0..st.inputs.len()).filter(|&k| is_capture(&st.inputs[k])).collect();
            let loops: Vec<usize> = (0..st.inputs.len()).filter(|&k| !is_capture(&st.inputs[k])).collect();
            let mut map = vec![0usize; st.inputs.len()];
            for (j, &k) in caps.iter().chain(&loops).enumerate() {
                map[k] = j + 1;
            }
            let expr = canonical_mapped(&st.body, &|k| map[k as usize]);
            let mut captured = Vec::new();
            for &k in &caps {
                captured.push(self.fixed_source(&mut w, &st.inputs[k], &outs)?);
            }
            let (of, slots) = all_slots[i].clone();
            let sdir = dir.join("stages");
            std::fs::create_dir_all(&sdir).map_err(|e| format!("{}: {e}", sdir.display()))?;
            let mut files = Vec::with_capacity(slots.len());
            for (key, ts) in &slots {
                if ts.is_empty() {
                    files.push(None);
                    continue;
                }
                let mut sources = captured.clone();
                let mut vars = Vec::new();
                for &t in ts {
                    let time = sinfo.layers[t].time;
                    for &k in &loops {
                        sources.push(match &st.inputs[k] {
                            Key::Band { layer, band, raw, .. } => {
                                match match_date(&layers[*layer].info, time, t, dated, opts.matching) {
                                    Some(d) => w.source(*layer, *band, *raw, d)?,
                                    None => None,
                                }
                            }
                            Key::Stage(j) => self.stage_source(layers, &outs[*j], time, t, opts),
                        });
                    }
                    vars.push(vrt::vars_text(&date_vars(&sinfo, t)));
                }
                let args = vec![
                    ("expr".to_string(), expr.clone()),
                    // at(): its date alone, as a mean over one date
                    ("reduce".to_string(), if st.only.is_some() { "mean".into() } else { st.f.name() }),
                    ("dates".to_string(), ts.len().to_string()),
                    ("stride".to_string(), loops.len().to_string()),
                    ("captures".to_string(), caps.len().to_string()),
                    ("vars".to_string(), vars.join(";")),
                ];
                let description = match (st.only, of) {
                    (Some(t), _) => format!("at {}", sinfo.layers[t].label),
                    (None, SlotOf::Fixed) => format!("{} over {} dates", st.f.name(), ts.len()),
                    (None, SlotOf::Period(p)) => format!("{} of {}", st.f.name(), p.start(*key).1),
                    (None, SlotOf::MonthOfYear) => format!("{} of month {key}", st.f.name()),
                    (None, SlotOf::Date) => format!("{} at {}", st.f.name(), sinfo.layers[*key as usize].label),
                };
                let band = Band { description, sources, args };
                let name = match of {
                    SlotOf::Fixed => format!("stage{}_{}.vrt", i + 1, st.f.name()),
                    _ => format!("stage{}_{}_{key}.vrt", i + 1, st.f.name()),
                };
                let f = sdir.join(name);
                let shape = vrt::block_shape(&grid, &band.sources);
                stage_block = (stage_block.0.min(shape.0), stage_block.1.min(shape.1));
                std::fs::write(&f, vrt::vrt_xml(&grid, &[band], shape)).map_err(|e| format!("{}: {e}", f.display()))?;
                files.push(Some(f.to_string_lossy().into_owned()));
            }
            let written = files.iter().flatten().count();
            if opts.materialize && reads[i] > written {
                progress(
                    0.1 * i as f64 / self.stages.len() as f64,
                    &format!("computing reduction {} of {} ({written} images)", i + 1, self.stages.len()),
                );
                files = materialize(files, cancel)?;
            }
            outs.push(StageOut { of, keys: slots.iter().map(|s| s.0).collect(), files, series: st.series });
        }
        // the result, a VRT per date
        let expr = canonical_mapped(&self.program, &|k| k as usize + 1);
        let dates = self.dates(layers);
        let rinfo = self.series.map(|r| self.series_info(layers, r));
        let mut files = Vec::new();
        let mut result_shape = None;
        for (t, (time, _)) in dates.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let mut sources = Vec::new();
            for k in &self.inputs {
                sources.push(match k {
                    Key::Band { layer, band, raw, fixed: None } if layers[*layer].info.t() > 1 => {
                        let dated = rinfo.as_ref().is_some_and(|r| r.time_is_date);
                        match rinfo
                            .as_ref()
                            .and_then(|_| match_date(&layers[*layer].info, *time, t, dated, opts.matching))
                        {
                            Some(d) => w.source(*layer, *band, *raw, d)?,
                            // a series in a result of one image: its first date (only reductions see them all)
                            None if rinfo.is_none() => w.source(*layer, *band, *raw, 0)?,
                            None => None,
                        }
                    }
                    Key::Stage(j) if outs[*j].of != SlotOf::Fixed => {
                        self.stage_source(layers, &outs[*j], *time, t, opts)
                    }
                    k => self.fixed_source(&mut w, k, &outs)?,
                });
            }
            let vars = match &rinfo {
                Some(r) => date_vars(r, t),
                None => DateVars::default(),
            };
            let band = Band {
                description: rinfo.as_ref().map_or_else(
                    || self.name.clone().unwrap_or_else(|| "result".into()),
                    |r| r.layers[t].label.clone(),
                ),
                sources,
                args: vrt::date_args(&expr, self.inputs.len(), &vars),
            };
            let stem = rinfo.as_ref().map_or_else(|| "result".to_string(), |r| date_stem(r, t));
            let f = dir.join(format!("{stem}.vrt"));
            // no larger than the stages' blocks: a block of the result then needs one of each stage's
            // the dates' sources alike: their files opened once to see their blocks
            let shape = *result_shape.get_or_insert_with(|| vrt::block_shape(&grid, &band.sources));
            std::fs::write(&f, vrt::vrt_xml(&grid, &[band], (shape.0.min(stage_block.0), shape.1.min(stage_block.1))))
                .map_err(|e| format!("{}: {e}", f.display()))?;
            files.push(f.to_string_lossy().into_owned());
            progress(0.1 + 0.9 * (t + 1) as f64 / dates.len() as f64, &format!("{} of {} dates", t + 1, dates.len()));
        }
        // what was computed, to read again
        let mut j = String::new();
        let _ = write!(
            j,
            "{{\n  \"janus_calc\": 1,\n  \"expression\": {},\n  \"layers\": [{}],\n  \"dates_from\": {},\n  \"grid_from\": {},\n  \
             \"matching\": {},\n  \"resampling\": {},\n  \"quality_masks\": {}\n}}\n",
            serde_json::Value::from(text),
            self.used
                .iter()
                .map(|&l| serde_json::Value::from(layers[l].name.as_str()).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            serde_json::Value::from(self.series.map(|r| match Period::of_series(r) {
                Some(Period::Year) => "by year".to_string(),
                Some(Period::Month) => "by month".to_string(),
                None => layers[r].name.clone(),
            })),
            serde_json::Value::from(layers[self.grid].name.as_str()),
            serde_json::Value::from(opts.matching.label()),
            serde_json::Value::from(opts.resampling.as_str()),
            opts.apply_qa
        );
        let _ = std::fs::write(dir.join("calc.json"), j);
        Ok(Written { files, dir: dir.to_path_buf(), warped: w.warped })
    }

    /// The source of an input that is the same at every date: a stack's band, a layer of one image, a reduction.
    fn fixed_source(&self, w: &mut Writer, k: &Key, stages: &[StageOut]) -> Result<Option<Source>, String> {
        Ok(match k {
            Key::Stage(i) => stages[*i].files.first().cloned().flatten().map(|path| Source {
                path,
                band: 1,
                meta: BandMeta::default(),
                raw: true,
                rects: None,
                resampling: None,
            }),
            Key::Band { layer, band, raw, fixed } => w.source(*layer, *band, *raw, fixed.unwrap_or(0))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_names() {
        assert_eq!(alias_of("S2 L2A (Earth Search)", &[]), "s2_l2a_earth_search");
        assert_eq!(alias_of("2020 mosaic", &[]), "layer_2020_mosaic");
        assert_eq!(alias_of("img", &["img".into()]), "img_2");
        assert_eq!(alias_of("max", &[]), "max_layer");
        assert_eq!(alias_of("year", &[]), "year_layer");
    }
}
