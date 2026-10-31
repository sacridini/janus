//! The Classification window's methods (Tools > Classification), on the same samples: besides the forest and the
//! networks (ml.rs), TWDTW (Zeit's tool: each class's pattern the mean series of its samples, every pixel the class
//! of the pattern most like its series), the classifier of a layer of embeddings (the Embeddings panel's logistic
//! regression or nearest centroid, fitted on the samples' vectors) and SOM (Zeit's tool: clusters, no samples).
//! The supervised ones estimated on the samples as the forest is, cross-validated by place (each sample classified
//! by a model that never saw its place), on a thread when the samples or the settings change.

use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{Color32, ComboBox, RichText, Ui};
use janus_cube::CubeInfo;
use janus_embed::classify::{Method, fit, layer_norm};
use janus_ml::forest::place_folds;
use janus_ml::{Calendar, InputBand};
use janus_zeit::pixel::Params;
use janus_zeit::twdtw::{Pattern, distances_many};
use serde_json::{Value, json};

use crate::app::State;
use crate::ml::{FOREST, Site, assemble, class_color};
use crate::sample_check::{RawCache, key_of, read_cached};

/// What a method is: learned from the samples (the forest, the networks: ml.rs), matched to each class's series
/// (TWDTW), fitted on a layer of embeddings, or clusters found without samples (SOM).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum Kind {
    #[default]
    Learned,
    Twdtw,
    Embeddings,
    Som,
}

impl Kind {
    /// Its Zeit tool, run on the raster from the window.
    pub fn tool(self) -> Option<&'static str> {
        match self {
            Kind::Twdtw => Some("twdtw"),
            Kind::Som => Some("som"),
            _ => None,
        }
    }

    pub fn of_tool(id: &str) -> Option<Kind> {
        match id {
            "twdtw" => Some(Kind::Twdtw),
            "som" => Some(Kind::Som),
            _ => None,
        }
    }
}

/// Samples a method's estimate classifies at most (every n-th beyond; the patterns and models from all).
const MAX_CHECKED: usize = 3000;
/// The folds by place.
const FOLDS: usize = 5;

/// A method's estimate on the samples, cross-validated by place.
pub struct Estimate {
    pub key: u64,
    pub method: String,
    pub classes: Vec<String>,
    /// [reference][model], the last column the samples left unclassified
    pub confusion: Vec<Vec<usize>>,
    pub accuracy: f64,
    pub kappa: f64,
    pub checked: usize,
    pub folds: usize,
    /// TWDTW: each class's pattern from all its samples, as the tool's "patterns" parameter
    pub patterns: Value,
    pub seconds: f64,
}

impl Estimate {
    /// Each class's accuracy (its samples given their class), NaN without samples.
    pub fn class_accuracy(&self, k: usize) -> f64 {
        let row = &self.confusion[k];
        let n: usize = row.iter().sum();
        if n == 0 { f64::NAN } else { row[k] as f64 / n as f64 }
    }
}

type Pending = Arc<Mutex<Option<Result<Estimate, String>>>>;

/// The estimate of the method chosen, made again when its samples or settings change (after a pause).
#[derive(Default)]
pub struct MethodCheck {
    pub est: Option<Arc<Estimate>>,
    pub error: String,
    want: u64,
    since: Option<Instant>,
    job: Option<Pending>,
    cache: Arc<Mutex<RawCache>>,
}

impl MethodCheck {
    pub fn checking(&self) -> bool {
        self.job.is_some() || self.est.as_ref().is_none_or(|e| e.key != self.want)
    }

    /// No estimate under way or wanted (an error counts as done).
    pub fn settled(&self) -> bool {
        self.job.is_none() && (self.want == 0 || self.current().is_some() || !self.error.is_empty())
    }

    /// The estimate of the samples and settings as they are now.
    pub fn current(&self) -> Option<&Estimate> {
        self.est.as_deref().filter(|e| e.key == self.want)
    }

    /// Wants the estimate of `key`: takes a finished one, starts `job` after a pause when none is fresh.
    fn drive(
        &mut self,
        key: u64,
        ctx: &egui::Context,
        job: impl FnOnce() -> Box<dyn FnOnce() -> Result<Estimate, String> + Send>,
    ) {
        if key != self.want {
            self.want = key;
            self.since = Some(Instant::now());
        }
        let done = self.job.as_ref().and_then(|p| p.lock().unwrap().take());
        if let Some(r) = done {
            self.job = None;
            match r {
                Ok(e) => {
                    self.error.clear();
                    self.est = Some(Arc::new(e));
                }
                Err(e) => {
                    self.error = e;
                    self.est = None;
                }
            }
        }
        let fresh = self.current().is_some() || !self.error.is_empty() && self.since.is_none();
        if self.job.is_none() && !fresh && self.since.is_none_or(|t| t.elapsed().as_millis() >= 350) {
            let run = job();
            let pending: Pending = Arc::new(Mutex::new(None));
            let (out, ctx2) = (pending.clone(), ctx.clone());
            let spawned = std::thread::Builder::new().name("method-check".into()).spawn(move || {
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run))
                    .unwrap_or_else(|_| Err("the estimate failed".into()));
                *out.lock().unwrap() = Some(r);
                ctx2.request_repaint();
            });
            if spawned.is_ok() {
                self.job = Some(pending);
                // asked once for these samples (an error is not asked again until they change)
                self.since = None;
            }
        }
        if self.job.is_some() || self.current().is_none() && self.error.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(150));
        }
    }
}

/// The confusion matrix of the samples tested (`pred`: None not tested, Some(None) unclassified), the overall
/// accuracy, kappa and the samples tested.
fn score(y: &[u32], pred: &[Option<Option<u32>>], k: usize) -> (Vec<Vec<usize>>, f64, f64, usize) {
    let mut m = vec![vec![0usize; k + 1]; k];
    for (i, p) in pred.iter().enumerate() {
        if let Some(p) = p {
            m[y[i] as usize][p.map_or(k, |c| c as usize)] += 1;
        }
    }
    let n: usize = m.iter().flatten().sum();
    if n == 0 {
        return (m, f64::NAN, f64::NAN, 0);
    }
    let right: usize = (0..k).map(|c| m[c][c]).sum();
    let po = right as f64 / n as f64;
    let pe: f64 = (0..k)
        .map(|c| {
            let row: usize = m[c].iter().sum();
            let col: usize = (0..k).map(|r| m[r][c]).sum();
            row as f64 * col as f64
        })
        .sum::<f64>()
        / (n as f64 * n as f64);
    let kappa = if pe < 1.0 { (po - pe) / (1.0 - pe) } else { f64::NAN };
    (m, po, kappa, n)
}

/// The samples tested by the estimate: all, or every n-th beyond MAX_CHECKED.
fn tested(n: usize, i: usize) -> bool {
    i.is_multiple_of(n.div_ceil(MAX_CHECKED).max(1))
}

/// Each class's mean series of the samples `idx` (None: no sample).
fn class_means(x: &janus_ml::Series, y: &[u32], k: usize, idx: impl Iterator<Item = usize>) -> Vec<Option<Vec<f64>>> {
    let t = x.t * x.b;
    let (mut sum, mut cnt) = (vec![vec![0f64; t]; k], vec![vec![0usize; t]; k]);
    for i in idx {
        let c = y[i] as usize;
        for (j, &v) in x.pixel(i).iter().enumerate() {
            if v.is_finite() {
                sum[c][j] += f64::from(v);
                cnt[c][j] += 1;
            }
        }
    }
    (0..k)
        .map(|c| {
            (cnt[c].iter().any(|&n| n > 0))
                .then(|| (0..t).map(|j| if cnt[c][j] > 0 { sum[c][j] / cnt[c][j] as f64 } else { f64::NAN }).collect())
        })
        .collect()
}

/// TWDTW's estimate: in each fold, each class's mean series of the other places' samples its pattern, the samples
/// of the fold given the class of the nearest (unclassified above the maximum distance); the patterns of all the
/// samples for the run.
#[allow(clippy::too_many_arguments)]
fn twdtw_estimate(
    info: &Arc<CubeInfo>,
    sites: &[Site],
    classes: &[String],
    min_valid: f64,
    params: &Params,
    cache: &Mutex<RawCache>,
    key: u64,
) -> Result<Estimate, String> {
    let t0 = Instant::now();
    if !info.time_is_date {
        return Err("TWDTW needs dates (none were found in the file names or band descriptions)".into());
    }
    let shown = [InputBand::shown(info)];
    let raw = read_cached(info, &shown, sites, cache);
    let calendar = Calendar::of(info);
    let (x, y, place) = assemble(&raw, sites, classes, &calendar.years, min_valid);
    let (n, t, k) = (x.n, x.t, classes.len());
    if n == 0 {
        return Err("no sample has enough dates with a value".into());
    }
    let days: Vec<i64> = (0..t).map(|i| info.unix_day(i)).collect();
    let years: Vec<f64> = (0..t).map(|i| info.decimal_year(i)).collect();
    let pattern = |c: usize, values: &[f64]| {
        let ok: Vec<usize> = (0..t).filter(|&i| values[i].is_finite()).collect();
        Pattern {
            name: classes[c].clone(),
            days: ok.iter().map(|&i| days[i]).collect(),
            values: ok.iter().map(|&i| values[i]).collect(),
            years: ok.iter().map(|&i| years[i]).collect(),
        }
    };
    let counts: Vec<usize> = (0..k).map(|c| y.iter().filter(|&&v| v as usize == c).count()).collect();
    let patterns: Vec<Value> = class_means(&x, &y, k, 0..n)
        .iter()
        .enumerate()
        .filter_map(|(c, m)| {
            let m = m.as_ref()?;
            Some(json!({
                "name": classes[c],
                "from": format!("mean of {} samples", counts[c]),
                "years": years,
                "days": days,
                "values": m.iter().map(|&v| if v.is_finite() { json!(v) } else { Value::Null }).collect::<Vec<_>>(),
            }))
        })
        .collect();
    let max_distance = params.f64("max_distance");
    let (fold, folds) = place_folds(&y, k, &place, FOLDS, 7);
    let mut pred: Vec<Option<Option<u32>>> = vec![None; n];
    for f in 0..folds {
        let means = class_means(&x, &y, k, (0..n).filter(|&i| fold[i] != f));
        let present: Vec<usize> = (0..k)
            .filter(|&c| means[c].as_ref().is_some_and(|m| m.iter().filter(|v| v.is_finite()).count() >= 2))
            .collect();
        if present.len() < 2 {
            continue;
        }
        let pats: Vec<Pattern> = present.iter().map(|&c| pattern(c, means[c].as_ref().unwrap())).collect();
        // a class unseen in the fold's training is not asked of its samples
        let test: Vec<usize> =
            (0..n).filter(|&i| fold[i] == f && tested(n, i) && present.contains(&(y[i] as usize))).collect();
        if test.is_empty() {
            continue;
        }
        let values: Vec<f64> = test.iter().flat_map(|&i| x.pixel(i).iter().map(|&v| f64::from(v))).collect();
        let d = distances_many(params, &values, test.len(), &days, &pats, 2);
        for (j, &i) in test.iter().enumerate() {
            let best = (0..pats.len()).filter(|&p| d[p][j].is_finite()).min_by(|&a, &b| d[a][j].total_cmp(&d[b][j]));
            pred[i] =
                Some(best.filter(|&p| !(max_distance > 0.0 && d[p][j] > max_distance)).map(|p| present[p] as u32));
        }
    }
    let (confusion, accuracy, kappa, checked) = score(&y, &pred, k);
    Ok(Estimate {
        key,
        method: "TWDTW".into(),
        classes: classes.to_vec(),
        confusion,
        accuracy,
        kappa,
        checked,
        folds,
        patterns: Value::Array(patterns),
        seconds: t0.elapsed().as_secs_f64(),
    })
}

/// The embeddings classifier's estimate: in each fold, fitted on the other places' vectors.
#[allow(clippy::too_many_arguments)]
fn embeddings_estimate(
    store: &janus_embed::EmbeddingStore,
    vectors: &[Vec<f32>],
    y: &[u32],
    place: &[usize],
    classes: &[String],
    method: Method,
    lambda: f64,
    min_score: f32,
    key: u64,
) -> Result<Estimate, String> {
    let t0 = Instant::now();
    let (n, k) = (vectors.len(), classes.len());
    if n == 0 {
        return Err("no sample has a vector in the year shown".into());
    }
    let norm = layer_norm(store);
    let norm = norm.as_ref().map(|(m, s)| (m.as_slice(), s.as_slice()));
    let (fold, folds) = place_folds(y, k, place, FOLDS, 7);
    let mut pred: Vec<Option<Option<u32>>> = vec![None; n];
    for f in 0..folds {
        let mut train: Vec<Vec<Vec<f32>>> = vec![vec![]; k];
        for i in (0..n).filter(|&i| fold[i] != f) {
            train[y[i] as usize].push(vectors[i].clone());
        }
        let present: Vec<usize> = (0..k).filter(|&c| !train[c].is_empty()).collect();
        if present.len() < 2 {
            continue;
        }
        let train: Vec<Vec<Vec<f32>>> = present.iter().map(|&c| std::mem::take(&mut train[c])).collect();
        let c = fit(&train, method, lambda, norm)?;
        for i in (0..n).filter(|&i| fold[i] == f && tested(n, i) && present.contains(&(y[i] as usize))) {
            let (p, s) = c.predict(&vectors[i]);
            pred[i] = Some((s >= min_score || !s.is_finite()).then_some(present[p] as u32));
        }
    }
    let (confusion, accuracy, kappa, checked) = score(y, &pred, k);
    Ok(Estimate {
        key,
        method: if method == Method::Centroid { "nearest centroid" } else { "logistic regression" }.into(),
        classes: classes.to_vec(),
        confusion,
        accuracy,
        kappa,
        checked,
        folds,
        patterns: Value::Null,
        seconds: t0.elapsed().as_secs_f64(),
    })
}

/// A method in the window's list: its kind, the network's or forest's index (Learned), its name.
type MethodEntry = (Kind, usize, &'static str);

/// The methods in the window's list, by group.
pub(crate) fn methods() -> Vec<(&'static str, Vec<MethodEntry>)> {
    vec![
        (
            "Learned from the samples",
            crate::ml::ARCH_ORDER.iter().map(|&i| (Kind::Learned, i, crate::ml::ARCHS[i])).collect(),
        ),
        ("Matched to each class's series", vec![(Kind::Twdtw, 0, "TWDTW")]),
        ("On a layer of embeddings", vec![(Kind::Embeddings, 0, "Logistic regression or nearest centroid")]),
        ("Without samples (clusters)", vec![(Kind::Som, 0, "Self-Organizing Map (SOM)")]),
    ]
}

/// What the method does, in a sentence or two.
pub(crate) fn describe(kind: Kind, arch: usize) -> &'static str {
    match (kind, arch) {
        (Kind::Learned, FOREST) => {
            "Many decision trees, each grown on a bootstrap sample of the samples, vote on each date's values: \
             quick on the CPU, good with few samples, and it tells which dates matter."
        }
        (Kind::Learned, 0) => "Convolutions over the series read as a sequence of fixed length, as sits builds it.",
        (Kind::Learned, 1) => "The dates weighed by attention, as sits builds it.",
        (Kind::Learned, 2) => {
            "Pretrained on Sentinel-1 and -2 series worldwide, it encodes a year of a pixel; a classifier learns \
             from its encodings."
        }
        (Kind::Learned, _) => {
            "A U-Net on each date and attention over the dates: patches segmented, a pixel's class from its \
             neighbourhood too."
        }
        (Kind::Twdtw, _) => {
            "Time-Weighted Dynamic Time Warping (Maus et al. 2016): each class's mean series of its samples is its \
             pattern; every pixel gets the class of the pattern most like its series, dates far apart in time \
             matched at a cost. No training: a few samples per class suffice, the series shown is compared."
        }
        (Kind::Embeddings, _) => {
            "A layer of embeddings (TESSERA, AlphaEarth, Presto) holds a vector per pixel and year: the samples' \
             vectors in the year shown train a logistic regression or give each class its centroid. The class map \
             is the layer's Classes view."
        }
        (Kind::Som, _) => {
            "Kohonen's Self-Organizing Map: the pixels grouped by their whole series without samples, \
             neighbouring clusters alike. A first look at the kinds of trajectories an area has."
        }
    }
}

impl State {
    /// The method chosen: a list grouped by kind, and what it does.
    pub(crate) fn method_choice_ui(&mut self, ui: &mut Ui) {
        crate::panels::section(ui, "Method");
        let (kind, arch) = (self.ml.kind, self.ml.arch);
        let name = methods()
            .into_iter()
            .flat_map(|g| g.1)
            .find(|m| m.0 == kind && (kind != Kind::Learned || m.1 == arch))
            .map_or("", |m| m.2);
        let has_emb = self.embedding_layer().is_some();
        ComboBox::from_id_salt("ml method").width(ui.available_width()).selected_text(name).show_ui(ui, |ui| {
            for (group, list) in methods() {
                ui.weak(group);
                for (k, a, label) in list {
                    let on = k == kind && (k != Kind::Learned || a == arch);
                    let enabled = k != Kind::Embeddings || has_emb;
                    let b = ui.add_enabled(enabled, egui::Button::selectable(on, format!("    {label}")));
                    let b = if enabled { b } else { b.on_disabled_hover_text("Open a layer of embeddings first") };
                    if b.clicked() {
                        self.ml.kind = k;
                        if k == Kind::Learned {
                            self.ml.arch = a;
                        }
                    }
                }
            }
        });
        ui.weak(describe(self.ml.kind, self.ml.arch));
    }

    /// The estimate of a supervised method: its accuracy by place and each class's, with the forest's beside it.
    fn estimate_ui(&mut self, ui: &mut Ui) {
        crate::panels::section(ui, "Estimated on the samples");
        let forest = self.ml.check.current().map(|r| r.accuracy).filter(|a| a.is_finite());
        let c = &self.ml.method_check;
        if !c.error.is_empty() {
            ui.colored_label(crate::theme::error(), &c.error);
            return;
        }
        let Some(e) = c.est.clone() else {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Classifying the samples...");
            });
            return;
        };
        let stale = c.checking();
        ui.horizontal_wrapped(|ui| {
            let acc = if e.accuracy.is_finite() { format!("{:.0} %", e.accuracy * 100.0) } else { "-".into() };
            ui.label(RichText::new(acc).size(18.0).strong()).on_hover_text(format!(
                "Each of {} samples classified by {} without its place ({} folds by place), kappa {:.2}, in {:.1} s",
                e.checked, e.method, e.folds, e.kappa, e.seconds
            ));
            ui.label(format!("{} by place", e.method));
            if let Some(f) = forest {
                ui.weak(format!("(the forest's check: {:.0} %)", f * 100.0));
            }
            if stale {
                ui.spinner();
            }
        });
        let k = e.classes.len();
        let unclassified: usize = e.confusion.iter().map(|r| r[k]).sum();
        egui::Grid::new("method confusion").striped(true).show(ui, |ui| {
            ui.weak("reference \\ method");
            for (j, c) in e.classes.iter().enumerate() {
                let [r, g, b] = class_color(j);
                ui.label(RichText::new(c).color(Color32::from_rgb(r, g, b)));
            }
            if unclassified > 0 {
                ui.weak("none");
            }
            ui.weak("accuracy");
            ui.end_row();
            for (i, row) in e.confusion.iter().enumerate() {
                ui.label(&e.classes[i]);
                for (j, n) in row.iter().enumerate().take(k + usize::from(unclassified > 0)) {
                    if i == j {
                        ui.strong(n.to_string());
                    } else if *n > 0 {
                        ui.colored_label(crate::theme::warning(), n.to_string());
                    } else {
                        ui.weak("0");
                    }
                }
                let a = e.class_accuracy(i);
                ui.label(if a.is_finite() { format!("{:.0} %", a * 100.0) } else { "-".into() });
                ui.end_row();
            }
        });
    }

    /// The samples and their classes, if any (a link to the Samples tab otherwise).
    fn method_samples(&mut self, ui: &mut Ui) -> Option<(Vec<Site>, Vec<String>)> {
        match self.ml.sites.as_ref().map(|c| c.1.clone()) {
            Some(Ok((sites, classes))) if !sites.is_empty() && classes.len() >= 2 => Some((sites, classes)),
            Some(Err(e)) if !e.is_empty() => {
                ui.colored_label(crate::theme::error(), e);
                None
            }
            _ => {
                if ui.link("Samples of two classes at least are needed: gather them in Samples.").clicked() {
                    self.ml.tab = 0;
                }
                None
            }
        }
    }

    /// TWDTW in the Method tab: the series compared, the classes' patterns, its settings and its estimate.
    pub(crate) fn twdtw_method_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let tool = janus_zeit::manifest::manifest().tool("twdtw").expect("the TWDTW tool");
        crate::panels::section(ui, "Values read on each date");
        ui.label(format!("The series shown ({})", InputBand::shown(info).name));
        if let Some(why) = crate::zeit::applicability(tool, info) {
            ui.colored_label(crate::theme::error(), format!("Not for this series: {why}"));
            return;
        }
        crate::panels::section(ui, "Settings");
        if self.tool_params_ui(ui, tool) {
            self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
        }
        if self.method_samples(ui).is_none() {
            return;
        }
        self.estimate_ui(ui);
        if let Some(list) = self.zeit.params(tool).get("patterns").and_then(Value::as_array).filter(|a| !a.is_empty()) {
            crate::panels::section(ui, "Patterns");
            ui.horizontal_wrapped(|ui| {
                for (k, p) in list.iter().enumerate() {
                    let [r, g, b] = class_color(k);
                    ui.label(RichText::new(p["name"].as_str().unwrap_or("")).color(Color32::from_rgb(r, g, b)))
                        .on_hover_text(p["from"].as_str().unwrap_or(""));
                }
            });
            ui.weak("Each class's mean series of its samples (Samples, Typical series, draws them).");
            let mut on = self.zeit.fitted("twdtw");
            if ui.checkbox(&mut on, "Classify the cursor, pins and ROI on the chart").changed() {
                self.zeit.set_fitted("twdtw", on);
            }
        }
    }

    /// The estimate of the method chosen (TWDTW, the embeddings' classifier) kept up to date, whatever the tab:
    /// TWDTW's patterns are made by it.
    pub(crate) fn drive_method_check(&mut self, info: &Arc<CubeInfo>, ctx: &egui::Context) {
        let Some(Ok((sites, classes))) = self.ml.sites.as_ref().map(|c| c.1.clone()) else { return };
        if sites.is_empty() || classes.len() < 2 {
            return;
        }
        match self.ml.kind {
            Kind::Twdtw => self.drive_twdtw(info, sites, classes, ctx),
            Kind::Embeddings => self.drive_embeddings(sites, classes, ctx),
            _ => {}
        }
    }

    fn drive_twdtw(&mut self, info: &Arc<CubeInfo>, sites: Vec<Site>, classes: Vec<String>, ctx: &egui::Context) {
        let tool = janus_zeit::manifest::manifest().tool("twdtw").expect("the TWDTW tool");
        if crate::zeit::applicability(tool, info).is_some() {
            return;
        }
        let given = self.zeit.params(tool).clone();
        let params = Params::with_defaults("twdtw", &given).expect("TWDTW's parameters");
        let min_valid = self.ml.min_valid;
        let shown = [InputBand::shown(info)];
        let mut h = std::hash::DefaultHasher::new();
        (key_of(&sites, &classes, &shown, min_valid, info.id), "twdtw").hash(&mut h);
        for (k, v) in &params.0 {
            if k != "patterns" {
                (k, v.to_string()).hash(&mut h);
            }
        }
        let key = h.finish();
        let cache = self.ml.method_check.cache.clone();
        let info2 = info.clone();
        self.ml.method_check.drive(key, ctx, move || {
            Box::new(move || twdtw_estimate(&info2, &sites, &classes, min_valid, &params, &cache, key))
        });
        // the patterns the run and the chart use
        if let Some(e) = self.ml.method_check.current()
            && e.method == "TWDTW"
        {
            let patterns = e.patterns.clone();
            let p = self.zeit.params(tool);
            if p.get("patterns") != Some(&patterns) {
                p.insert("patterns".into(), patterns);
                self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
                self.zeit.version += 1;
            }
        }
    }

    fn drive_embeddings(&mut self, sites: Vec<Site>, classes: Vec<String>, ctx: &egui::Context) {
        let Some(i) = self.embedding_layer() else { return };
        let t = self.layer_date(i);
        let Some(e) = self.layers[i].emb.as_ref() else { return };
        let Some(store) = e.store.clone().filter(|s| s.year_ready(t)) else { return };
        let (method, lambda, min_score) =
            (if e.cls_method == 1 { Method::Centroid } else { Method::Logistic }, f64::from(e.cls_lambda), e.cls_min);
        let (vectors, y, place) = self.embedding_samples(i, &sites, &classes);
        let mut h = std::hash::DefaultHasher::new();
        (&vectors.len(), &y, &place, &classes, i, t, method == Method::Centroid, lambda.to_bits(), min_score.to_bits())
            .hash(&mut h);
        for v in vectors.iter().step_by(vectors.len().div_ceil(64).max(1)) {
            v.iter().map(|x| x.to_bits()).collect::<Vec<_>>().hash(&mut h);
        }
        let key = h.finish();
        self.ml.method_check.drive(key, ctx, move || {
            Box::new(move || {
                embeddings_estimate(&store, &vectors, &y, &place, &classes, method, lambda, min_score, key)
            })
        });
    }

    /// TWDTW or SOM in the Apply tab: Zeit's run on the raster (its scope, mask, threads, preview).
    pub(crate) fn zeit_method_apply_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>, id: &str) {
        let tool = janus_zeit::manifest::manifest().tool(id).expect("a Zeit tool");
        let mut why = crate::zeit::applicability(tool, info);
        if id == "twdtw" {
            let have = self.zeit.params(tool).get("patterns").and_then(Value::as_array).is_some_and(|a| a.len() >= 2);
            if self.ml.kind != Kind::Twdtw || self.ml.method_check.current().is_none() || !have {
                why = why.or(Some("the classes' patterns are made in the Method tab, from the samples".into()));
            }
        }
        if let Some(w) = &why {
            ui.colored_label(crate::theme::error(), format!("Not yet: {w}"));
        }
        self.raster_run_ui(ui, tool, why.is_some());
        ui.weak("The maps are layers of their own when the run ends; Tools > Classification > Post-processing smooths a class map.");
    }

    /// SOM in the Method tab: its settings (no samples).
    pub(crate) fn som_method_ui(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let tool = janus_zeit::manifest::manifest().tool("som").expect("the SOM tool");
        crate::panels::section(ui, "Values read on each date");
        ui.label(format!("The series shown ({})", InputBand::shown(info).name));
        if let Some(why) = crate::zeit::applicability(tool, info) {
            ui.colored_label(crate::theme::error(), format!("Not for this series: {why}"));
            return;
        }
        crate::panels::section(ui, "Settings");
        if self.tool_params_ui(ui, tool) {
            self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
        }
        ui.weak("No samples: the clusters are named after the run, by what their series show.");
        if ui.button("Run it (Apply)").clicked() {
            self.ml.tab = 2;
        }
    }

    /// The vectors of the samples' pixels in layer i's year shown (a place's mean for a mean site), their classes
    /// and places; the samples without one left out.
    fn embedding_samples(&self, i: usize, sites: &[Site], classes: &[String]) -> (Vec<Vec<f32>>, Vec<u32>, Vec<usize>) {
        let (mut v, mut y, mut place) = (vec![], vec![], vec![]);
        let Some(s) = self.layers[i].emb.as_ref().and_then(|e| e.store.clone()) else { return (v, y, place) };
        let t = self.layer_date(i);
        if !s.year_ready(t) {
            return (v, y, place);
        }
        for (si, site) in sites.iter().enumerate() {
            let k = classes.iter().position(|c| c == &site.class).unwrap_or(0) as u32;
            let mut got: Vec<Vec<f32>> = vec![];
            for &(x, yy) in &site.pixels {
                if let Some(p) = self.embedding_pixel(i, x as f64 + 0.5, yy as f64 + 0.5)
                    && s.valid(t, p)
                {
                    let mut a = vec![0f32; s.d];
                    s.dequant(t, p, &mut a);
                    got.push(a);
                }
            }
            if got.is_empty() {
                continue;
            }
            if site.mean {
                let mut m = vec![0f32; s.d];
                for g in &got {
                    for (a, b) in m.iter_mut().zip(g) {
                        *a += b / got.len() as f32;
                    }
                }
                got = vec![m];
            }
            for g in got {
                v.push(g);
                y.push(k);
                place.push(si);
            }
        }
        (v, y, place)
    }

    /// The embeddings' classifier in the Method tab: the layer and year, the method, its estimate.
    pub(crate) fn embeddings_method_ui(&mut self, ui: &mut Ui) {
        let Some(i) = self.embedding_layer() else {
            ui.colored_label(crate::theme::error(), "Open a layer of embeddings first (File > Open, or Embeddings).");
            return;
        };
        let t = self.layer_date(i);
        let Some(e) = self.layers[i].emb.as_ref() else { return };
        crate::panels::section(ui, "Values read");
        ui.label(format!(
            "The vectors of {} ({} values) in {}",
            self.layers[i].name,
            e.store.as_ref().map_or(0, |s| s.d),
            e.info.layers[t].label
        ));
        if !e.store.as_ref().is_some_and(|s| s.year_ready(t)) {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Reading the year shown...");
            });
            return;
        }
        crate::panels::section(ui, "Settings");
        let e = self.layers[i].emb.as_mut().unwrap();
        let before = (e.cls_method, e.cls_lambda.to_bits(), e.cls_min.to_bits());
        ui.horizontal(|ui| {
            ComboBox::from_id_salt("ml emb method")
                .selected_text(if e.cls_method == 1 { "Nearest centroid (cosine)" } else { "Logistic regression (L2)" })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut e.cls_method, 0, "Logistic regression (L2)");
                    ui.selectable_value(&mut e.cls_method, 1, "Nearest centroid (cosine)");
                });
            if e.cls_method == 0 {
                ui.add(egui::DragValue::new(&mut e.cls_lambda).range(1e-4..=100.0).speed(0.01).prefix("L2 "));
            }
        });
        let score = if e.cls_method == 1 { "cosine" } else { "probability" };
        ui.add(egui::Slider::new(&mut e.cls_min, 0.0..=1.0).text(format!("minimum {score}")))
            .on_hover_text("Pixels whose class scores less are left unclassified");
        if (e.cls_method, e.cls_lambda.to_bits(), e.cls_min.to_bits()) != before {
            e.cls_gen += 1;
            self.map.dirty = true;
        }
        if self.method_samples(ui).is_none() {
            return;
        }
        self.estimate_ui(ui);
    }

    /// The embeddings' classifier in the Apply tab: the samples made the layer's classes, its Classes view shown.
    pub(crate) fn embeddings_apply_ui(&mut self, ui: &mut Ui) {
        let Some(i) = self.embedding_layer() else {
            ui.colored_label(crate::theme::error(), "Open a layer of embeddings first.");
            return;
        };
        crate::panels::section(ui, "Apply");
        let Some((sites, classes)) = self.method_samples(ui) else { return };
        ui.weak(
            "The samples become the layer's classes (one per class, its samples' vectors), and its Classes view \
             is shown: the map classified as it is drawn, any year; exported from the Embeddings panel.",
        );
        if ui
            .add(egui::Button::new("Classify the layer of embeddings").min_size(egui::vec2(ui.available_width(), 0.0)))
            .clicked()
        {
            self.classify_embeddings(i, &sites, &classes);
        }
    }

    /// The samples made layer i's classes (Embeddings > Classes) and that view shown.
    pub(crate) fn classify_embeddings(&mut self, i: usize, sites: &[Site], classes: &[String]) {
        let (vectors, y, _) = self.embedding_samples(i, sites, classes);
        let t = self.layer_date(i);
        let Some(e) = self.layers[i].emb.as_mut() else { return };
        let when = e.info.layers.get(t).map_or(String::new(), |l| format!(" ({})", l.label));
        e.classes = classes
            .iter()
            .enumerate()
            .map(|(k, name)| {
                let samples: Vec<Vec<f32>> =
                    vectors.iter().zip(&y).filter(|(_, c)| **c as usize == k).map(|(v, _)| v.clone()).collect();
                let [r, g, b] = class_color(k);
                crate::embeddings::EmbClass {
                    name: name.clone(),
                    color: Color32::from_rgb(r, g, b),
                    sources: vec![format!("{} samples of the Classification window{when}", samples.len())],
                    samples,
                }
            })
            .filter(|c| !c.samples.is_empty())
            .collect();
        e.cls_gen += 1;
        e.view = crate::embeddings::View::Classes;
        self.show_embeddings = true;
        self.map.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_with_unclassified() {
        let y = [0, 0, 1, 1, 1];
        let pred = [Some(Some(0)), Some(None), Some(Some(1)), Some(Some(0)), None];
        let (m, acc, _, n) = score(&y, &pred, 2);
        assert_eq!(n, 4);
        assert_eq!(m, vec![vec![1, 0, 1], vec![1, 1, 0]]);
        assert!((acc - 0.5).abs() < 1e-12);
    }

    #[test]
    fn means_by_class() {
        let x = janus_ml::Series { n: 3, t: 2, b: 1, values: vec![1.0, 2.0, 3.0, f32::NAN, 10.0, 10.0] };
        let m = class_means(&x, &[0, 0, 1], 2, 0..3);
        assert_eq!(m[0].as_ref().unwrap(), &vec![2.0, 2.0]);
        assert_eq!(m[1].as_ref().unwrap(), &vec![10.0, 10.0]);
        assert!(class_means(&x, &[0, 0, 1], 3, 0..3)[2].is_none());
    }
}
