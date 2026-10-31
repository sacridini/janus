//! The samples' quality as they are gathered (Tools > Classification > Samples): whenever the samples change (a
//! pin dropped or given a class, a polygon labelled), after a short pause and on a thread of its own, every place's
//! series read (each pixel once: a cache keeps them), and a quick Random Forest cross-validated by place (each
//! sample classified by forests that never saw its place: GroupKFold). From it the estimated accuracy and each
//! class's, the classes taken for one another, the places that look like another class (mislabelled, or mixed);
//! with the classes' balance, the samples left out (too few dates with a value), the places sharing a pixel and
//! each class's typical series (its mean and p10-p90 by date). Shown in the window as a summary, a table of the
//! classes, warnings to act on and a table of the samples (bounded, its rows drawn only when seen).

use std::collections::{BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, ComboBox, RichText, Ui, vec2};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints, Polygon};
use janus_cube::CubeInfo;
use janus_ml::forest::{ForestHyper, cross_predict};
use janus_ml::{Calendar, InputBand};

use crate::app::State;
use crate::ml::{Site, class_color};

/// A place is suspect when the forests that never saw it give its own class less than this, and another at least
/// LIKELY.
const SUSPECT: f32 = 0.4;
const LIKELY: f32 = 0.5;
/// Samples a class should have at least, and its places.
const FEW: usize = 10;
const FEW_PLACES: usize = 5;
/// The samples the check classifies at most (a random share of them beyond)
const MAX_CHECKED: usize = 6000;

/// A place's numbers.
#[derive(Clone, Debug)]
pub struct PlaceStats {
    pub label: String,
    pub pin: Option<u32>,
    pub pixel: (usize, usize),
    pub class: usize,
    /// the samples it gives, those kept (values on enough dates), the share of its dates with a value
    pub samples: usize,
    pub kept: usize,
    pub valid: f32,
    /// its own class's mean probability by forests that never saw it (NaN: not checked), the most probable class
    pub own: f32,
    pub likely: usize,
    pub likely_p: f32,
}

impl PlaceStats {
    pub fn suspect(&self) -> bool {
        self.own < SUSPECT && self.likely != self.class && self.likely_p >= LIKELY
    }
}

/// A class's numbers.
#[derive(Clone, Debug, Default)]
pub struct ClassStats {
    pub name: String,
    pub samples: usize,
    pub places: usize,
    pub left_out: usize,
    /// the share of its samples given it (producer's accuracy), of those given it that are it (user's)
    pub producers: f64,
    pub users: f64,
    /// the class its samples are most often taken for, and the share
    pub confused: Option<(usize, f64)>,
    /// no place of it could be checked (all of its samples from one place)
    pub unchecked: bool,
}

/// The check's result.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub key: u64,
    pub classes: Vec<ClassStats>,
    pub places: Vec<PlaceStats>,
    pub samples: usize,
    pub checked: usize,
    pub accuracy: f64,
    pub kappa: f64,
    pub folds: usize,
    /// [class][date * values + value]: p10, mean, p90 of the samples' values (not scaled)
    pub signatures: Vec<Vec<[f32; 3]>>,
    pub inputs: Vec<String>,
    pub years: Vec<f64>,
    pub importance_by_date: Vec<f64>,
    /// pairs of places on a pixel (indices into `places`), and whether their classes differ
    pub shared: Vec<(usize, usize, bool)>,
    pub seconds: f64,
}

impl Report {
    pub fn suspects(&self) -> usize {
        self.places.iter().filter(|p| p.suspect()).count()
    }

    pub fn suspect_pins(&self) -> BTreeSet<u32> {
        self.places.iter().filter(|p| p.suspect()).filter_map(|p| p.pin).collect()
    }

    fn place_of(&self, label: &str, pixel: (usize, usize)) -> Option<&PlaceStats> {
        self.places.iter().find(|p| p.label == label && p.pixel == pixel)
    }
}

/// Which samples the table shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Filter {
    #[default]
    All,
    Suspect,
    Shared,
    NoClass,
    Class(String),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Sort {
    #[default]
    Order,
    LeastFitting,
    Class,
}

/// Each pixel's raw series read, for the series and values they were read of.
#[derive(Default)]
struct RawCache {
    key: String,
    px: HashMap<(usize, usize), Vec<f32>>,
}

type Pending = Arc<Mutex<Option<Result<Report, String>>>>;

#[derive(Default)]
pub struct SampleCheck {
    pub report: Option<Arc<Report>>,
    pub error: String,
    /// the samples' key wanted, since when (the check waits for a pause)
    want: u64,
    since: Option<Instant>,
    job: Option<(u64, Pending)>,
    cache: Arc<Mutex<RawCache>>,
    /// the estimated accuracy as the samples grew: (samples, accuracy)
    pub history: Vec<(usize, f64)>,
    pub filter: Filter,
    pub sort: Sort,
    pub sig_value: usize,
    /// the last pin dropped (its verdict shown)
    pub last_pin: Option<u32>,
    /// no samples to check
    empty: bool,
}

impl SampleCheck {
    pub fn checking(&self) -> bool {
        self.job.is_some() || self.report.as_ref().is_none_or(|r| r.key != self.want)
    }

    /// Nothing being checked: the samples' report made, or none to make.
    pub fn settled(&self) -> bool {
        self.job.is_none() && (self.empty || self.current().is_some() || !self.error.is_empty())
    }

    /// The report, if it is of the samples as they are now.
    pub fn current(&self) -> Option<&Report> {
        self.report.as_deref().filter(|r| r.key == self.want)
    }
}

/// The samples' key: their places, classes and pixels, the values read, the dates needed and the series.
pub fn key_of(sites: &[Site], classes: &[String], inputs: &[InputBand], min_valid: f64, cube: u64) -> u64 {
    let mut h = std::hash::DefaultHasher::new();
    for s in sites {
        (&s.class, &s.label, &s.pixels, s.mean).hash(&mut h);
    }
    classes.hash(&mut h);
    for b in inputs {
        b.name.hash(&mut h);
    }
    (min_valid.to_bits(), cube).hash(&mut h);
    h.finish()
}

/// The check of the samples (on a thread: reads the pixels not read yet).
fn analyse(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    sites: &[Site],
    classes: &[String],
    min_valid: f64,
    cache: &Mutex<RawCache>,
    key: u64,
) -> Result<Report, String> {
    let t0 = Instant::now();
    let (t, b) = (info.t(), inputs.len());
    if b == 0 {
        return Err("no value is read on the dates".into());
    }
    let pixels: Vec<(usize, usize)> = sites.iter().flat_map(|s| s.pixels.iter().copied()).collect();
    // the pixels not read yet
    let ckey = format!("{}:{}", info.id, inputs.iter().map(|b| b.name.clone()).collect::<Vec<_>>().join(","));
    let missing: Vec<(usize, usize)> = {
        let mut c = cache.lock().unwrap();
        if c.key != ckey {
            *c = RawCache { key: ckey.clone(), px: HashMap::new() };
        }
        let mut seen = BTreeSet::new();
        pixels.iter().copied().filter(|p| !c.px.contains_key(p) && seen.insert(*p)).collect()
    };
    if !missing.is_empty() {
        let raw = janus_ml::data::read_points(info, inputs, &missing);
        let mut c = cache.lock().unwrap();
        if c.key == ckey {
            for (i, p) in missing.iter().enumerate() {
                c.px.insert(*p, raw.pixel(i).to_vec());
            }
        }
    }
    let raw = {
        let c = cache.lock().unwrap();
        let mut values = Vec::with_capacity(pixels.len() * t * b);
        for p in &pixels {
            match c.px.get(p) {
                Some(v) => values.extend_from_slice(v),
                None => values.extend(std::iter::repeat_n(f32::NAN, t * b)),
            }
        }
        janus_ml::Series { n: pixels.len(), t, b, values }
    };
    let calendar = Calendar::of(info);
    let (x, y, place) = crate::ml::assemble(&raw, sites, classes, &calendar.years, min_valid);
    let k = classes.len();

    // each place: its samples, kept, valid share
    let mut places: Vec<PlaceStats> = sites
        .iter()
        .map(|s| PlaceStats {
            label: s.label.clone(),
            pin: s.pin,
            pixel: s.pixels.first().copied().unwrap_or_default(),
            class: classes.iter().position(|c| c == &s.class).unwrap_or(0),
            samples: if s.mean { 1 } else { s.pixels.len() },
            kept: 0,
            valid: 0.0,
            own: f32::NAN,
            likely: 0,
            likely_p: f32::NAN,
        })
        .collect();
    let mut at = 0;
    for (si, s) in sites.iter().enumerate() {
        let n = s.pixels.len();
        let vals = &raw.values[at * t * b..(at + n) * t * b];
        let finite = vals.iter().filter(|v| v.is_finite()).count();
        places[si].valid = if vals.is_empty() { 0.0 } else { finite as f32 / vals.len() as f32 };
        at += n;
    }
    for &pl in &place {
        places[pl].kept += 1;
    }

    // each class's numbers
    let mut stats: Vec<ClassStats> =
        classes.iter().map(|c| ClassStats { name: c.clone(), ..Default::default() }).collect();
    for p in &places {
        let c = &mut stats[p.class];
        c.samples += p.kept;
        c.left_out += p.samples - p.kept;
        if p.kept > 0 {
            c.places += 1;
        }
    }

    // the check: forests that never saw the place
    let mut report = Report { key, inputs: inputs.iter().map(|b| b.name.clone()).collect(), ..Default::default() };
    let present = y.iter().collect::<BTreeSet<_>>().len();
    if present >= 2 && x.n >= 6 {
        let pick: Vec<usize> = if x.n > MAX_CHECKED {
            let mut idx: Vec<usize> = (0..x.n).collect();
            janus_ml::train::Rng::new(11).shuffle(&mut idx);
            idx.truncate(MAX_CHECKED);
            idx.sort_unstable();
            idx
        } else {
            (0..x.n).collect()
        };
        let xs = x.select(&pick);
        let ys: Vec<u32> = pick.iter().map(|&i| y[i]).collect();
        let ps: Vec<usize> = pick.iter().map(|&i| place[i]).collect();
        let h = ForestHyper { trees: 60, ..Default::default() };
        let cp = cross_predict(&xs, &ys, k, &ps, 5, &h, 7, &AtomicBool::new(false))?;
        let mut confusion = vec![vec![0u32; k]; k];
        let mut sum: Vec<Vec<f64>> = vec![vec![0.0; k]; places.len()];
        let mut cnt = vec![0usize; places.len()];
        for (j, (&c, &pl)) in ys.iter().zip(&ps).enumerate() {
            let pr = &cp.probs[j * k..(j + 1) * k];
            if !pr[0].is_finite() {
                continue;
            }
            let best = (0..k).fold(0, |b, q| if pr[q] > pr[b] { q } else { b });
            confusion[c as usize][best] += 1;
            for (a, &v) in sum[pl].iter_mut().zip(pr) {
                *a += f64::from(v);
            }
            cnt[pl] += 1;
        }
        for (pi, p) in places.iter_mut().enumerate() {
            if cnt[pi] > 0 {
                let m: Vec<f64> = sum[pi].iter().map(|v| v / cnt[pi] as f64).collect();
                p.own = m[p.class] as f32;
                p.likely = (0..k).fold(0, |b, q| if m[q] > m[b] { q } else { b });
                p.likely_p = m[p.likely] as f32;
            }
        }
        let total: u32 = confusion.iter().flatten().sum();
        let diag: u32 = (0..k).map(|i| confusion[i][i]).sum();
        report.checked = total as usize;
        if total > 0 {
            let n = f64::from(total);
            report.accuracy = f64::from(diag) / n;
            let pe: f64 = (0..k)
                .map(|i| {
                    let row: u32 = confusion[i].iter().sum();
                    let col: u32 = (0..k).map(|r| confusion[r][i]).sum();
                    f64::from(row) * f64::from(col) / (n * n)
                })
                .sum();
            report.kappa = if pe < 1.0 { (report.accuracy - pe) / (1.0 - pe) } else { f64::NAN };
        } else {
            report.accuracy = f64::NAN;
            report.kappa = f64::NAN;
        }
        for (i, c) in stats.iter_mut().enumerate() {
            let row: u32 = confusion[i].iter().sum();
            let col: u32 = (0..k).map(|r| confusion[r][i]).sum();
            c.producers = if row > 0 { f64::from(confusion[i][i]) / f64::from(row) } else { f64::NAN };
            c.users = if col > 0 { f64::from(confusion[i][i]) / f64::from(col) } else { f64::NAN };
            c.confused = (0..k)
                .filter(|&j| j != i && confusion[i][j] > 0)
                .max_by_key(|&j| confusion[i][j])
                .map(|j| (j, f64::from(confusion[i][j]) / f64::from(row)));
            c.unchecked = c.samples > 0 && row == 0;
        }
        report.folds = cp.folds;
        report.importance_by_date = (0..t).map(|d| cp.importance[d * b..(d + 1) * b].iter().sum()).collect();
    } else {
        report.accuracy = f64::NAN;
        report.kappa = f64::NAN;
    }

    // each class's typical series
    let p = t * b;
    report.signatures = (0..k as u32)
        .map(|c| {
            let idx: Vec<usize> = (0..x.n).filter(|&i| y[i] == c).collect();
            (0..p)
                .map(|j| {
                    let mut v: Vec<f32> = idx.iter().map(|&i| x.values[i * p + j]).filter(|v| v.is_finite()).collect();
                    if v.is_empty() {
                        return [f32::NAN; 3];
                    }
                    v.sort_unstable_by(f32::total_cmp);
                    let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
                    let mean = v.iter().map(|&a| f64::from(a)).sum::<f64>() / v.len() as f64;
                    [q(0.1), mean as f32, q(0.9)]
                })
                .collect()
        })
        .collect();

    // places on a pixel of another
    let mut on: HashMap<(usize, usize), Vec<usize>> = HashMap::new();
    for (si, s) in sites.iter().enumerate() {
        for &px in &s.pixels {
            let v = on.entry(px).or_default();
            if v.last() != Some(&si) {
                v.push(si);
            }
        }
    }
    let mut pairs = BTreeSet::new();
    for v in on.values().filter(|v| v.len() > 1) {
        for a in 0..v.len() {
            for c in a + 1..v.len() {
                pairs.insert((v[a], v[c]));
            }
        }
    }
    report.shared = pairs.into_iter().take(500).map(|(a, c)| (a, c, places[a].class != places[c].class)).collect();

    report.samples = x.n;
    report.classes = stats;
    report.places = places;
    report.years = calendar.years;
    report.seconds = t0.elapsed().as_secs_f64();
    Ok(report)
}

/// A warning on the samples: how bad (0 good news, 1 to look at, 2 to fix), what, and the samples it is about.
pub struct Warning {
    pub level: u8,
    pub text: String,
    pub filter: Option<Filter>,
}

/// What to look at in the samples, the worst first.
pub fn warnings(r: &Report) -> Vec<Warning> {
    let mut w = vec![];
    let mut add = |level: u8, text: String, filter: Option<Filter>| w.push(Warning { level, text, filter });
    let with: Vec<&ClassStats> = r.classes.iter().filter(|c| c.samples > 0).collect();
    if with.len() == 1 {
        add(2, format!("Only one class ({}): add samples of another.", with[0].name), None);
    }
    let conflicts = r.shared.iter().filter(|s| s.2).count();
    if conflicts > 0 {
        add(2, format!("{conflicts} pairs of samples on the same pixel with different classes."), Some(Filter::Shared));
    }
    let suspects = r.suspects();
    if suspects > 0 {
        add(
            2,
            format!(
                "{suspects} sample place{} look{} like another class: mislabelled, or a mixed place? Review {}.",
                if suspects == 1 { "" } else { "s" },
                if suspects == 1 { "s" } else { "" },
                if suspects == 1 { "it" } else { "them" }
            ),
            Some(Filter::Suspect),
        );
    }
    for c in &with {
        if c.samples < FEW {
            let level = if c.samples < FEW / 2 { 2 } else { 1 };
            add(
                level,
                format!("{}: {} samples only ({FEW} at least, 30 or more better).", c.name, c.samples),
                Some(Filter::Class(c.name.clone())),
            );
        } else if c.places < FEW_PLACES && c.places < c.samples {
            add(
                1,
                format!(
                    "{}: its {} samples come from {} place{}: a place's pixels are alike, more places teach more.",
                    c.name,
                    c.samples,
                    c.places,
                    if c.places == 1 { "" } else { "s" }
                ),
                Some(Filter::Class(c.name.clone())),
            );
        }
        if c.unchecked {
            add(
                1,
                format!("{}: all from one place, it cannot be checked on a place the model did not see.", c.name),
                None,
            );
        }
    }
    if let (Some(most), Some(least)) = (with.iter().max_by_key(|c| c.samples), with.iter().min_by_key(|c| c.samples))
        && least.samples > 0
        && most.samples >= 4 * least.samples
    {
        add(
            1,
            format!(
                "Unbalanced: {} has {:.0} times the samples of {} (the forest leans to the larger classes).",
                most.name,
                most.samples as f64 / least.samples as f64,
                least.name
            ),
            Some(Filter::Class(least.name.clone())),
        );
    }
    for c in &with {
        if let Some((j, share)) = c.confused
            && share >= 0.2
        {
            add(
                1,
                format!(
                    "{} is taken for {} in {:.0} % of its samples: their series look alike (more samples of both, or \
                     more values on each date).",
                    c.name,
                    r.classes[j].name,
                    share * 100.0
                ),
                Some(Filter::Class(c.name.clone())),
            );
        }
    }
    let left: usize = r.classes.iter().map(|c| c.left_out).sum();
    if left > 0 {
        add(1, format!("{left} samples left out: values on too few dates (clouds, gaps)."), None);
    }
    let twice = r.shared.iter().filter(|s| !s.2).count();
    if twice > 0 {
        add(1, format!("{twice} pairs of samples of a class on the same pixel (counted twice)."), Some(Filter::Shared));
    }
    w.sort_by_key(|x| std::cmp::Reverse(x.level));
    if w.is_empty() && r.checked > 0 {
        w.push(Warning { level: 0, text: "The samples look consistent.".into(), filter: None });
    }
    w
}

fn level_color(level: u8) -> Color32 {
    match level {
        2 => crate::theme::error(),
        1 => Color32::from_rgb(230, 160, 40),
        _ => Color32::from_rgb(90, 180, 100),
    }
}

/// How well a share fits: green, amber, red.
fn fit_color(p: f32) -> Color32 {
    if p >= 0.7 {
        Color32::from_rgb(90, 180, 100)
    } else if p >= SUSPECT {
        Color32::from_rgb(230, 160, 40)
    } else {
        crate::theme::error()
    }
}

fn swatch(ui: &mut Ui, k: usize) {
    let [r, g, b] = class_color(k);
    let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 2.0, Color32::from_rgb(r, g, b));
}

/// A bar `share` of `width` wide.
fn bar(ui: &mut Ui, share: f32, width: f32, color: Color32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 8.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    let mut r = rect;
    r.set_width(width * share.clamp(0.0, 1.0));
    p.rect_filled(r, 2.0, color);
    resp
}

/// A summary's card: a big value, its caption.
fn card(ui: &mut Ui, value: RichText, caption: &str, hover: &str) {
    egui::Frame::group(ui.style())
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.label(value);
                ui.label(RichText::new(caption).small().weak());
            });
        })
        .response
        .on_hover_text(hover);
}

/// The estimate as the samples grew: a small line.
fn sparkline(ui: &mut Ui, h: &[(usize, f64)]) {
    let (rect, resp) = ui.allocate_exact_size(vec2(90.0, 26.0), egui::Sense::hover());
    let pts: Vec<(usize, f64)> = h.iter().copied().filter(|p| p.1.is_finite()).collect();
    if pts.len() < 2 {
        return;
    }
    let (x0, x1) = (pts[0].0 as f32, pts[pts.len() - 1].0.max(pts[0].0 + 1) as f32);
    let at = |(n, a): (usize, f64)| {
        egui::pos2(
            rect.left() + (n as f32 - x0) / (x1 - x0) * rect.width(),
            rect.bottom() - (a as f32).clamp(0.0, 1.0) * rect.height(),
        )
    };
    let line: Vec<egui::Pos2> = pts.iter().map(|&p| at(p)).collect();
    ui.painter().add(egui::Shape::line(line, egui::Stroke::new(1.5, ui.visuals().selection.bg_fill)));
    resp.on_hover_text(format!(
        "The estimated accuracy as the samples grew: from {:.0} % with {} samples to {:.0} % with {}",
        pts[0].1 * 100.0,
        pts[0].0,
        pts[pts.len() - 1].1 * 100.0,
        pts[pts.len() - 1].0
    ));
}

/// A row of the samples' table.
struct Row {
    label: String,
    pin: Option<u32>,
    class: String,
    pixel: (usize, usize),
}

impl State {
    /// Starts the check when the samples have changed and paused, takes its result when done.
    pub(crate) fn drive_sample_check(
        &mut self,
        info: &Arc<CubeInfo>,
        inputs: &[InputBand],
        sites: &[Site],
        classes: &[String],
        ctx: &egui::Context,
    ) {
        let min_valid = self.ml.min_valid;
        let key = key_of(sites, classes, inputs, min_valid, info.id);
        let c = &mut self.ml.check;
        c.empty = sites.is_empty();
        if key != c.want {
            c.want = key;
            c.since = Some(Instant::now());
        }
        let done = c.job.as_ref().and_then(|(_, pending)| pending.lock().unwrap().take());
        if let Some(r) = done {
            c.job = None;
            match r {
                Ok(rep) => {
                    if rep.key == c.want
                        && rep.accuracy.is_finite()
                        && c.history.last().is_none_or(|h| h.0 != rep.samples)
                    {
                        c.history.push((rep.samples, rep.accuracy));
                    }
                    c.error.clear();
                    c.report = Some(Arc::new(rep));
                }
                Err(e) => c.error = e,
            }
        }
        let fresh = c.report.as_ref().is_some_and(|r| r.key == c.want);
        if c.job.is_none() && !fresh && c.since.is_none_or(|t| t.elapsed().as_millis() >= 350) && !sites.is_empty() {
            let pending: Pending = Arc::new(Mutex::new(None));
            let (info, inputs, sites, classes, cache, out) =
                (info.clone(), inputs.to_vec(), sites.to_vec(), classes.to_vec(), c.cache.clone(), pending.clone());
            let ctx2 = ctx.clone();
            let spawned = std::thread::Builder::new().name("sample-check".into()).spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyse(&info, &inputs, &sites, &classes, min_valid, &cache, key)
                }))
                .unwrap_or_else(|_| Err("the check failed".into()));
                *out.lock().unwrap() = Some(r);
                ctx2.request_repaint();
            });
            if spawned.is_ok() {
                c.job = Some((key, pending));
            }
        }
        if c.job.is_some() || !fresh {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }

    /// The samples' summary: their count, classes, estimated accuracy, warnings, and the last pin's verdict.
    pub(crate) fn samples_summary_ui(&mut self, ui: &mut Ui, counts: &[(String, usize)]) {
        let total: usize = counts.iter().map(|c| c.1).sum();
        let c = &self.ml.check;
        let rep = c.report.clone();
        let stale = c.checking();
        let warns = rep.as_deref().map(warnings).unwrap_or_default();
        ui.horizontal_wrapped(|ui| {
            card(ui, RichText::new(total.to_string()).size(18.0).strong(), "samples", "Samples kept for the training");
            card(ui, RichText::new(counts.len().to_string()).size(18.0).strong(), "classes", "Classes with samples");
            let (acc, hover) = match rep.as_deref() {
                Some(r) if r.accuracy.is_finite() => (
                    format!("{:.0} %", r.accuracy * 100.0),
                    format!(
                        "Estimated accuracy: each of {} samples classified by forests that never saw its place \
                         ({} folds by place, 60 trees each), kappa {:.2}. A guide while the samples are gathered; \
                         the training's validation is the measure.",
                        r.checked, r.folds, r.kappa
                    ),
                ),
                _ => ("-".into(), "Two classes and a few samples are needed for an estimate".into()),
            };
            card(ui, RichText::new(acc).size(18.0).strong(), "estimated accuracy", &hover);
            let bad = warns.iter().filter(|w| w.level > 0).count();
            let worst = warns.iter().map(|w| w.level).max().unwrap_or(0);
            card(
                ui,
                RichText::new(bad.to_string()).size(18.0).strong().color(level_color(if bad == 0 { 0 } else { worst })),
                "warnings",
                "What to look at in the samples (below)",
            );
            ui.vertical(|ui| {
                sparkline(ui, &self.ml.check.history);
                if stale {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.small("checking...");
                    });
                } else if let Some(r) = rep.as_deref() {
                    ui.small(format!("checked in {:.1} s", r.seconds)).on_hover_text(
                        "Checked again whenever the samples change (after a short pause), in the background",
                    );
                }
            });
        });
        if !self.ml.check.error.is_empty() {
            ui.colored_label(crate::theme::error(), format!("The check failed: {}", self.ml.check.error));
        }
        // the last pin's verdict
        if self.ml.source == 3
            && let Some(id) = self.ml.check.last_pin
            && let Some(pin) = self.series.pins.iter().find(|p| p.id == id)
        {
            let class = self.ml.pin_classes.get(&id).cloned().unwrap_or_default();
            let st =
                rep.as_deref().and_then(|r| r.place_of(&format!("Pin {id}"), (pin.x, pin.y)).map(|p| (r, p.clone())));
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(pin.color, format!("Pin {id}"));
                match st {
                    _ if class.is_empty() => {
                        ui.weak("has no class: give it one to use it.");
                    }
                    Some((r, p)) if p.own.is_finite() => {
                        let likely = &r.classes[p.likely].name;
                        if p.likely == p.class {
                            ui.colored_label(
                                fit_color(p.own),
                                format!("({class}) looks like {likely} ({:.0} %).", p.own * 100.0),
                            );
                        } else {
                            ui.colored_label(
                                fit_color(p.own),
                                format!(
                                    "({class}) looks more like {likely} ({:.0} %; {class} {:.0} %): check it.",
                                    p.likely_p * 100.0,
                                    p.own * 100.0
                                ),
                            );
                        }
                    }
                    Some(_) if !stale => {
                        ui.weak(format!("({class}): not checked (the first of its class, or too few samples)."));
                    }
                    _ => {
                        ui.weak(format!("({class}): checking..."));
                    }
                }
            });
        }
    }

    /// The classes' table: samples, places, balance, accuracy, confusion.
    pub(crate) fn classes_table_ui(&mut self, ui: &mut Ui, counts: &[(String, usize)]) {
        let rep = self.ml.check.report.clone();
        let most = counts.iter().map(|c| c.1).max().unwrap_or(1).max(1);
        egui::Grid::new("sample classes").striped(true).num_columns(5).show(ui, |ui| {
            ui.weak("class");
            ui.weak("samples");
            ui.weak("balance");
            ui.weak("accuracy").on_hover_text(
                "The share of its samples the forests that never saw their place gave it (producer's accuracy)",
            );
            ui.weak("taken for");
            ui.end_row();
            for (k, (name, n)) in counts.iter().enumerate() {
                let st = rep.as_deref().and_then(|r| r.classes.iter().find(|c| &c.name == name));
                let pick = ui
                    .horizontal(|ui| {
                        swatch(ui, k);
                        ui.add(egui::Label::new(name).sense(egui::Sense::click()))
                    })
                    .inner;
                if pick.on_hover_text("Show its samples below").clicked() {
                    self.ml.check.filter = Filter::Class(name.clone());
                }
                let places = st.map_or(String::new(), |s| format!(" ({} places)", s.places));
                let few = *n < FEW;
                let txt = format!("{n}{places}");
                if few {
                    ui.colored_label(level_color(if *n < FEW / 2 { 2 } else { 1 }), txt)
                        .on_hover_text(format!("{FEW} samples at least, 30 or more better"));
                } else {
                    ui.label(txt);
                }
                let [r, g, b] = class_color(k);
                bar(ui, *n as f32 / most as f32, 70.0, Color32::from_rgb(r, g, b)).on_hover_text(format!(
                    "{:.0} % of the samples",
                    *n as f64 / counts.iter().map(|c| c.1).sum::<usize>().max(1) as f64 * 100.0
                ));
                match st {
                    Some(s) if s.producers.is_finite() => {
                        ui.colored_label(fit_color(s.producers as f32), format!("{:.0} %", s.producers * 100.0))
                            .on_hover_text(format!(
                                "Producer's {:.0} %, user's {:.0} %",
                                s.producers * 100.0,
                                s.users * 100.0
                            ));
                    }
                    Some(s) if s.unchecked => {
                        ui.weak("one place").on_hover_text("All its samples from one place: not checked");
                    }
                    _ => {
                        ui.weak("-");
                    }
                }
                match st.and_then(|s| s.confused).zip(rep.as_deref()) {
                    Some(((j, share), r)) => {
                        let col = if share >= 0.2 { level_color(1) } else { ui.visuals().weak_text_color() };
                        ui.colored_label(col, format!("{} {:.0} %", r.classes[j].name, share * 100.0));
                    }
                    None => {
                        ui.weak("-");
                    }
                }
                ui.end_row();
            }
        });
    }

    /// The warnings, each showing its samples when clicked.
    pub(crate) fn warnings_ui(&mut self, ui: &mut Ui) {
        let Some(rep) = self.ml.check.report.clone() else {
            ui.weak("The samples are checked as they are gathered.");
            return;
        };
        for w in warnings(&rep) {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(10.0, 14.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, level_color(w.level));
                let r = ui.add(egui::Label::new(&w.text).wrap().sense(egui::Sense::click()));
                if let Some(f) = w.filter
                    && r.on_hover_text("Show these samples below").clicked()
                {
                    self.ml.check.filter = f;
                }
            });
        }
    }

    /// Each class's typical series: its mean and the p10-p90 band of a value on each date.
    pub(crate) fn signatures_ui(&mut self, ui: &mut Ui, height: f32) {
        let Some(rep) = self.ml.check.report.clone() else { return };
        let b = rep.inputs.len().max(1);
        if rep.signatures.is_empty() || rep.years.is_empty() {
            return;
        }
        if b > 1 {
            let v = &mut self.ml.check.sig_value;
            *v = (*v).min(b - 1);
            ComboBox::from_id_salt("signature value").selected_text(&rep.inputs[*v]).show_ui(ui, |ui| {
                for (j, n) in rep.inputs.iter().enumerate() {
                    ui.selectable_value(v, j, n);
                }
            });
        }
        let j = self.ml.check.sig_value.min(b - 1);
        Plot::new("sample signatures")
            .height(height)
            .legend(Legend::default())
            .allow_scroll(false)
            .x_axis_formatter(|m, _| format!("{:.1}", m.value))
            .show(ui, |p| {
                for (k, sig) in rep.signatures.iter().enumerate() {
                    let pts: Vec<(f64, [f32; 3])> = rep
                        .years
                        .iter()
                        .enumerate()
                        .filter_map(|(d, &yr)| sig.get(d * b + j).filter(|s| s[1].is_finite()).map(|s| (yr, *s)))
                        .collect();
                    if pts.is_empty() {
                        continue;
                    }
                    let [r, g, bl] = class_color(k);
                    let col = Color32::from_rgb(r, g, bl);
                    let name = &rep.classes[k].name;
                    // the p10-p90 band a quad between each two dates (the plot fills convex shapes only)
                    for w in pts.windows(2) {
                        let ((x0, a), (x1, c)) = (w[0], w[1]);
                        let quad = vec![
                            [x0, f64::from(a[0])],
                            [x1, f64::from(c[0])],
                            [x1, f64::from(c[2])],
                            [x0, f64::from(a[2])],
                        ];
                        p.polygon(
                            Polygon::new(name.as_str(), PlotPoints::from(quad))
                                .fill_color(col.gamma_multiply(0.12))
                                .stroke(egui::Stroke::NONE),
                        );
                    }
                    p.line(
                        Line::new(name.as_str(), PlotPoints::from_iter(pts.iter().map(|(x, s)| [*x, f64::from(s[1])])))
                            .color(col)
                            .width(2.0),
                    );
                }
            });
    }

    /// The samples' table: filtered, sorted, drawn only where seen, `height` tall at most.
    pub(crate) fn samples_table_ui(&mut self, ui: &mut Ui, sites: &[Site], classes: &[String], height: f32) {
        let pins = self.ml.source == 3;
        let rep = self.ml.check.report.clone();
        let mut rows: Vec<Row> = if pins {
            self.series
                .pins
                .iter()
                .map(|p| Row {
                    label: format!("Pin {}", p.id),
                    pin: Some(p.id),
                    class: self.ml.pin_classes.get(&p.id).cloned().unwrap_or_default(),
                    pixel: (p.x, p.y),
                })
                .collect()
        } else {
            sites
                .iter()
                .map(|s| Row {
                    label: s.label.clone(),
                    pin: None,
                    class: s.class.clone(),
                    pixel: s.pixels.first().copied().unwrap_or_default(),
                })
                .collect()
        };
        let stats = |r: &Row| rep.as_deref().and_then(|x| x.place_of(&r.label, r.pixel)).cloned();
        let shared: BTreeSet<String> = rep
            .as_deref()
            .map(|r| r.shared.iter().flat_map(|s| [r.places[s.0].label.clone(), r.places[s.1].label.clone()]).collect())
            .unwrap_or_default();
        let all = rows.len();
        let filter = self.ml.check.filter.clone();
        rows.retain(|r| match &filter {
            Filter::All => true,
            Filter::Suspect => stats(r).is_some_and(|s| s.suspect()),
            Filter::Shared => shared.contains(&r.label),
            Filter::NoClass => r.class.is_empty(),
            Filter::Class(c) => &r.class == c,
        });
        match self.ml.check.sort {
            Sort::Order => {}
            Sort::Class => rows.sort_by(|a, b| a.class.cmp(&b.class)),
            Sort::LeastFitting => rows.sort_by(|a, b| {
                let f =
                    |r: &Row| stats(r).map_or(f32::INFINITY, |s| if s.own.is_finite() { s.own } else { f32::INFINITY });
                f(a).total_cmp(&f(b))
            }),
        }
        // the filter and the order
        ui.horizontal_wrapped(|ui| {
            let name = |f: &Filter| match f {
                Filter::All => "All".to_string(),
                Filter::Suspect => "Look like another class".into(),
                Filter::Shared => "On a shared pixel".into(),
                Filter::NoClass => "No class".into(),
                Filter::Class(c) => c.clone(),
            };
            ComboBox::from_id_salt("sample filter").selected_text(name(&filter)).show_ui(ui, |ui| {
                let mut opts = vec![Filter::All, Filter::Suspect, Filter::Shared];
                if pins {
                    opts.push(Filter::NoClass);
                }
                opts.extend(classes.iter().map(|c| Filter::Class(c.clone())));
                for f in opts {
                    let t = name(&f);
                    ui.selectable_value(&mut self.ml.check.filter, f, t);
                }
            });
            ComboBox::from_id_salt("sample sort")
                .selected_text(match self.ml.check.sort {
                    Sort::Order => "In order",
                    Sort::LeastFitting => "Least fitting first",
                    Sort::Class => "By class",
                })
                .show_ui(ui, |ui| {
                    let s = &mut self.ml.check.sort;
                    ui.selectable_value(s, Sort::Order, "In order");
                    ui.selectable_value(s, Sort::LeastFitting, "Least fitting first");
                    ui.selectable_value(s, Sort::Class, "By class");
                });
            ui.weak(format!("{} of {all}", rows.len()));
        });
        let class_list = if pins { self.ml.pin_class_list.clone() } else { classes.to_vec() };
        let color_of = |c: &str| class_list.iter().position(|k| k == c);
        let mut goto = None;
        let mut moves: Vec<(u32, String)> = vec![];
        let mut remove = None;
        let row_h = 20.0;
        TableBuilder::new(ui)
            .id_salt("samples table")
            .striped(true)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(84.0).at_least(60.0).clip(true))
            .column(Column::initial(110.0).at_least(70.0).clip(true))
            .column(Column::remainder().at_least(110.0).clip(true))
            .column(Column::initial(44.0).at_least(36.0))
            .column(Column::exact(if pins { 52.0 } else { 28.0 }))
            .min_scrolled_height(0.0)
            .max_scroll_height(height)
            .header(row_h, |mut h| {
                h.col(|ui| {
                    ui.strong("sample");
                });
                h.col(|ui| {
                    ui.strong("class");
                });
                h.col(|ui| {
                    ui.strong("fits its class").on_hover_text(
                        "Its class's probability by forests that never saw it; when another class is likelier, \
                         that class",
                    );
                });
                h.col(|ui| {
                    ui.strong("dates").on_hover_text("The share of its dates with a value");
                });
                h.col(|_| {});
            })
            .body(|body| {
                body.rows(row_h, rows.len(), |mut row| {
                    let r = &rows[row.index()];
                    let st = stats(r);
                    row.col(|ui| {
                        if let Some(k) = color_of(&r.class) {
                            swatch(ui, k);
                        }
                        if ui
                            .add(egui::Label::new(&r.label).sense(egui::Sense::click()))
                            .on_hover_text("Go to it")
                            .clicked()
                        {
                            goto = Some((r.pixel, r.label.clone()));
                        }
                    });
                    row.col(|ui| match r.pin {
                        Some(id) => {
                            let cur = if r.class.is_empty() { "no class" } else { r.class.as_str() };
                            ComboBox::from_id_salt(("sample pin class", id)).width(100.0).selected_text(cur).show_ui(
                                ui,
                                |ui| {
                                    if ui.selectable_label(r.class.is_empty(), "no class").clicked() {
                                        moves.push((id, String::new()));
                                    }
                                    for c in &class_list {
                                        if ui.selectable_label(&r.class == c, c).clicked() {
                                            moves.push((id, c.clone()));
                                        }
                                    }
                                },
                            );
                        }
                        None => {
                            ui.label(&r.class);
                        }
                    });
                    row.col(|ui| match (&st, rep.as_deref()) {
                        (Some(s), Some(x)) if s.own.is_finite() => {
                            bar(ui, s.own, 34.0, fit_color(s.own));
                            if s.likely == s.class {
                                ui.label(format!("{:.0} %", s.own * 100.0));
                            } else {
                                ui.colored_label(
                                    fit_color(s.own),
                                    format!("{} {:.0} %", x.classes[s.likely].name, s.likely_p * 100.0),
                                )
                                .on_hover_text(format!(
                                    "Looks like {} ({:.0} %) more than its class {} ({:.0} %)",
                                    x.classes[s.likely].name,
                                    s.likely_p * 100.0,
                                    r.class,
                                    s.own * 100.0
                                ));
                            }
                        }
                        (Some(s), _) if s.kept == 0 => {
                            ui.colored_label(level_color(1), "left out").on_hover_text("Values on too few dates");
                        }
                        _ if r.class.is_empty() => {
                            ui.weak("-");
                        }
                        _ => {
                            ui.weak("not checked")
                                .on_hover_text("Checking, or its class has too few places to check it");
                        }
                    });
                    row.col(|ui| match &st {
                        Some(s) => {
                            let col = if s.valid < 0.5 { level_color(1) } else { ui.visuals().text_color() };
                            ui.colored_label(col, format!("{:.0} %", s.valid * 100.0));
                        }
                        None => {
                            ui.weak("-");
                        }
                    });
                    row.col(|ui| {
                        if ui.small_button(">").on_hover_text("Go to it").clicked() {
                            goto = Some((r.pixel, r.label.clone()));
                        }
                        if let Some(id) = r.pin
                            && ui.small_button("x").on_hover_text("Remove the pin").clicked()
                        {
                            remove = Some(id);
                        }
                    });
                });
            });
        for (id, to) in moves {
            self.set_pin_class(id, &to);
        }
        if let Some(id) = remove {
            self.remove_pin(id);
        }
        if let Some((px, label)) = goto {
            self.go_to_pixel([px.0 as f64 + 0.5, px.1 as f64 + 0.5], &label, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> Report {
        let class = |name: &str, samples: usize, places: usize| ClassStats {
            name: name.into(),
            samples,
            places,
            producers: 0.9,
            users: 0.9,
            ..Default::default()
        };
        let place = |label: &str, class: usize, own: f32, likely: usize| PlaceStats {
            label: label.into(),
            pin: None,
            pixel: (0, 0),
            class,
            samples: 1,
            kept: 1,
            valid: 1.0,
            own,
            likely,
            likely_p: 1.0 - own,
        };
        Report {
            classes: vec![class("Forest", 40, 40), class("Pasture", 6, 6), class("Water", 30, 2)],
            places: vec![place("Pin 1", 0, 0.9, 0), place("Pin 2", 1, 0.2, 0)],
            checked: 76,
            accuracy: 0.9,
            shared: vec![(0, 1, true)],
            ..Default::default()
        }
    }

    #[test]
    fn warns_of_what_to_fix() {
        let r = report();
        assert_eq!(r.suspects(), 1);
        let w = warnings(&r);
        let text: Vec<&str> = w.iter().map(|x| x.text.as_str()).collect();
        // the worst first: the conflicting pixel, the suspect; then the few samples, the few places, the balance
        assert_eq!(w[0].level, 2, "{text:?}");
        assert!(text.iter().any(|t| t.contains("same pixel with different classes")));
        assert!(text.iter().any(|t| t.contains("1 sample place looks like another class")));
        assert!(text.iter().any(|t| t.starts_with("Pasture: 6 samples only")));
        assert!(text.iter().any(|t| t.starts_with("Water: its 30 samples come from 2 places")));
        assert!(text.iter().any(|t| t.starts_with("Unbalanced: Forest has 7 times")));
        assert!(w.windows(2).all(|p| p[0].level >= p[1].level));
        // all well
        let good = Report {
            classes: vec![
                ClassStats { name: "A".into(), samples: 30, places: 30, ..Default::default() },
                ClassStats { name: "B".into(), samples: 40, places: 40, ..Default::default() },
            ],
            checked: 70,
            ..Default::default()
        };
        let w = warnings(&good);
        assert_eq!((w.len(), w[0].level), (1, 0));
    }
}
