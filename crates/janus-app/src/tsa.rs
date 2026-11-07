//! Time series analysis (IDEIAS.md, phase 56): Tools > Time series analysis (FORCE). FORCE's TSA is a chain, not a
//! list of tools: the clear-sky observations of the raw series, an interpolation (and a standardization) that gives
//! the analysis series, and from it the temporal metrics, the fold by period, the linear trend, CAT and the polar
//! phenology. This window only orchestrates the eight tools that exist (janus_zeit::job::run_job, with the
//! parameters their own windows edit): no computation of its own. The chain is one task (Tools > Tasks) in a folder
//! of its own with a subfolder per step (its job.json, job.log, provenance.json and rerun.py); the analysis series
//! passes from a step to the next as a file (the step's GeoTIFF, opened as a series and written as VRTs again).

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{ComboBox, RichText, Ui};
use janus_cube::{BandSelection, CubeInfo};
use janus_zeit::job::{JobResult, run_job};
use janus_zeit::manifest::{Tool, manifest};
use serde_json::{Value, json};

use crate::app::State;
use crate::provenance::{OutputRecord, Provenance, SCRIPT, SeriesRecord};
use crate::zeit::{Job, JobLog, JobState, JobStatus, applicability, base_spec, series_inputs};

/// The id of the chain's task in Tools > Tasks (not a tool of the manifest).
pub(crate) const JOB_TOOL: &str = "tsa";

/// The steps in the order of the chain: the tool, the window's name for it and what it does.
pub(crate) const STEPS: [(&str, &str, &str); 8] = [
    (
        "clear_observations",
        "Clear-sky observations",
        "How many valid observations each pixel has in windows of months, and the gaps (on the raw series, by its \
         quality band).",
    ),
    (
        "interpolate",
        "Interpolate",
        "Puts every pixel's series on a regular grid of dates: the analysis series of the steps below.",
    ),
    ("standardize", "Standardize", "The analysis series' mean removed along time (or a z-score)."),
    ("temporal_metrics", "Temporal metrics", "Statistics of each pixel's analysis series over all of it: maps."),
    ("fold", "Fold by period", "The same statistics by year, quarter or month: a series."),
    (
        "linear_trend",
        "Linear trend of a fold",
        "The trend of a statistic taken by period (it folds the series itself): maps.",
    ),
    (
        "cat",
        "Change, aftereffect, trend",
        "FORCE's CAT, on a statistic taken by period (it folds the series itself): maps.",
    ),
    (
        "polar_metrics",
        "Polar phenology",
        "Land surface phenology by year on the polar plane (needs a regular series): a series.",
    ),
];
/// The steps that read the raw series (the quality band is there); the others read the analysis series.
const RAW_STEPS: [&str; 2] = ["clear_observations", "interpolate"];
/// The steps that write the analysis series for the next ones.
const ANALYSIS_STEPS: [&str; 2] = ["interpolate", "standardize"];
const SCOPES: [&str; 3] = ["Whole image", "Visible area", "ROI"];

/// The window's state: which steps are on, the tab, the scope of the run.
pub struct TsaUi {
    pub open: bool,
    pub tab: usize,
    /// each step of STEPS on or off
    pub on: [bool; 8],
    /// 0 whole image, 1 visible area, 2 ROI
    pub scope: usize,
}

impl Default for TsaUi {
    fn default() -> TsaUi {
        // Interpolate, Temporal metrics and Fold
        TsaUi { open: false, tab: 0, on: [false, true, false, true, true, false, false, false], scope: 0 }
    }
}

/// A step of the chain: its tool and the parameters it runs with.
pub(crate) struct ChainStep {
    pub tool: &'static Tool,
    pub params: BTreeMap<String, Value>,
}

/// What the chain starts from: the series, its band roles and the window it runs on.
pub(crate) struct ChainInput {
    pub info: Arc<CubeInfo>,
    /// the layer's name and what it opened (for the provenance)
    pub name: String,
    pub inputs: Vec<String>,
    pub roles: BTreeMap<String, i32>,
    pub window: [usize; 4],
    pub scope: String,
    pub scope_index: usize,
    /// the date a map tool takes its year from
    pub map_t: usize,
    pub threads: usize,
}

/// What a chain did: the outputs of every step that ran and the steps left out, with why.
pub(crate) struct Chain {
    pub result: JobResult,
    pub skipped: Vec<(String, String)>,
}

/// Why the dates are not a regular series (a constant step of whole days, what the polar phenology reads), if so.
fn irregular(info: &CubeInfo) -> Option<String> {
    if !info.time_is_date {
        return Some("needs dates (none were found in the file names or band descriptions)".into());
    }
    let n = info.t();
    if n < 3 {
        return None;
    }
    let gap = info.unix_day(1) - info.unix_day(0);
    if gap <= 0 || (2..n).any(|t| info.unix_day(t) - info.unix_day(t - 1) != gap) {
        return Some(
            "the dates are not evenly spaced: turn Interpolate on, which puts the series on a regular grid".into(),
        );
    }
    None
}

/// Why a step cannot run on a series, if it cannot: the tool's own rule, and for the polar phenology a regular
/// series, unless `regular` (an Interpolate comes before it).
pub(crate) fn problem(tool: &Tool, info: &CubeInfo, regular: bool) -> Option<String> {
    if let Some(why) = applicability(tool, info) {
        return Some(why);
    }
    if tool.id == "polar_metrics" && !regular {
        return irregular(info);
    }
    None
}

/// One thing to look at before running the chain: `level` 0 is information, 1 something to look at, 2 something that
/// stops a step (or the chain).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Advice {
    pub level: u8,
    pub text: String,
}

/// A calendar year with fewer dates than this, inside the series, reads little (or fewer than half the median per
/// year, whichever is more).
const FEW_DATES_PER_YEAR: usize = 6;
/// The first or last year of the series covered by fewer days than this is a partial year.
const FULL_YEAR_DAYS: i64 = 300;
/// An Interpolate grid with a step over this many times the median gap between the dates smooths the data away.
const COARSE_GRID: f64 = 4.0;
/// A linear trend or CAT over fewer periods than this is not worth much.
const FEW_PERIODS: usize = 5;
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn param_f64(p: &BTreeMap<String, Value>, id: &str) -> Option<f64> {
    p.get(id).and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
}

/// The median of days (the mean of the two middle ones for an even count).
fn median(v: &mut [i64]) -> f64 {
    v.sort_unstable();
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2] as f64,
        n => (v[n / 2 - 1] + v[n / 2]) as f64 / 2.0,
    }
}

fn month_range(from: u32, to: u32) -> String {
    let (a, b) = (MONTHS[from as usize - 1], MONTHS[to as usize - 1]);
    if from == to { a.to_string() } else { format!("{a} to {b}") }
}

/// What to look at in the series and the steps that are on, from the dates alone (no pixel is read): years with few
/// dates, a first or last year only partly covered, an Interpolate grid that does not fit the dates, a missing
/// quality band, a trend over few years, the steps the series cannot take and a chain with nothing on. `params`
/// gives the parameters of a step by its tool's id (`step` of Interpolate, `by` of the folded steps). A partial year
/// is looked for when a step reads years (Fold, Linear trend or CAT by year, Polar phenology); a fold by quarter,
/// month, week or day of the year is not cut by it.
pub(crate) fn advice(info: &CubeInfo, on: &[bool; 8], params: &dyn Fn(&str) -> BTreeMap<String, Value>) -> Vec<Advice> {
    let mut out = vec![];
    if !on.iter().any(|&o| o) {
        out.push(Advice { level: 2, text: "No step is on.".into() });
        return out;
    }
    let mut add = |level: u8, text: String| out.push(Advice { level, text });
    let index = |id: &str| STEPS.iter().position(|s| s.0 == id).unwrap_or(0);
    let is_on = |id: &str| on[index(id)];
    // a step the series cannot take; the polar phenology goes with the regular grid Interpolate gives it
    let regular =
        is_on("interpolate") && manifest().tool("interpolate").is_some_and(|t| problem(t, info, false).is_none());
    for (i, (id, label, _)) in STEPS.into_iter().enumerate() {
        if let Some(why) = manifest().tool(id).filter(|_| on[i]).and_then(|t| problem(t, info, regular)) {
            add(2, format!("{label}: {why}."));
        }
    }
    if !info.time_is_date || info.t() == 0 {
        // the rest reads the dates
        if is_on("clear_observations") && info.sel.qa_band == 0 {
            add(1, "Clear-sky observations counts every date as clear: the series has no quality band.".into());
        }
        return out;
    }
    let days: Vec<i64> = (0..info.t()).map(|t| info.unix_day(t)).collect();
    let (first, last) = (days[0], days[days.len() - 1]);
    let year_of = |d: i64| janus_zeit::numeric::civil(d).0;
    let (y0, y1) = (year_of(first), year_of(last));
    let reads_statistics =
        ["temporal_metrics", "fold", "linear_trend", "cat", "polar_metrics"].iter().any(|s| is_on(s));
    // 1: years inside the series with few dates
    if reads_statistics && y1 - y0 >= 2 {
        let count = |y: i64| days.iter().filter(|&&d| year_of(d) == y).count();
        let mut per: Vec<i64> = ((y0 + 1)..y1).map(|y| count(y) as i64).collect();
        let med = median(&mut per);
        let limit = (FEW_DATES_PER_YEAR as f64).max(med / 2.0);
        let few: Vec<(i64, usize)> =
            ((y0 + 1)..y1).map(|y| (y, count(y))).filter(|&(_, n)| (n as f64) < limit).collect();
        let others: Vec<usize> = ((y0 + 1)..y1).map(count).filter(|&n| n as f64 >= limit).collect();
        for (y, n) in few {
            let rest = match (others.iter().min(), others.iter().max()) {
                (Some(a), Some(b)) if a == b => format!(" (the others {a})"),
                (Some(a), Some(b)) => format!(" (the others {a} to {b})"),
                _ => String::new(),
            };
            let s = if n == 1 { "" } else { "s" };
            add(1, format!("{y} has {n} date{s}{rest}: its fold and statistics read little."));
        }
    }
    // 2: a first or last year only partly covered, for the steps that read years
    let by_year = |id: &str| is_on(id) && params(id).get("by").and_then(|v| v.as_str()).unwrap_or("year") == "year";
    if by_year("fold") || by_year("linear_trend") || by_year("cat") || is_on("polar_metrics") {
        let civil = |d: i64| janus_zeit::numeric::civil(d);
        let jan1 = |y: i64| janus_cube::dates::days_from_civil(y as i32, 1, 1);
        let mut partial = |y: i64, from: i64, to: i64, covered: i64| {
            if covered >= FULL_YEAR_DAYS {
                return;
            }
            let (m_from, m_to) = (civil(from).1, civil(to).1);
            let months = (covered as f64 / 30.44).round() as i64;
            add(
                1,
                format!(
                    "{y} covers only {}: its yearly values read {months} month{} of it.",
                    month_range(m_from, m_to),
                    if months == 1 { "" } else { "s" }
                ),
            );
        };
        if y0 == y1 {
            partial(y0, first, last, last - first + 1);
        } else {
            partial(y0, first, jan1(y0 + 1) - 1, jan1(y0 + 1) - first);
            partial(y1, jan1(y1), last, last - jan1(y1) + 1);
        }
    }
    // 4: the Interpolate grid against the dates
    let mut gaps: Vec<i64> = days.windows(2).map(|w| w[1] - w[0]).filter(|&g| g > 0).collect();
    let gap = median(&mut gaps);
    if is_on("interpolate")
        && gap > 0.0
        && let Some(step) = param_f64(&params("interpolate"), "step").filter(|&s| s >= 1.0)
    {
        if step < gap / 2.0 {
            add(
                1,
                format!(
                    "Interpolate's grid (every {step:.0} days) is denser than the dates (a median gap of {gap:.0} \
                     days): most of its values are interpolated."
                ),
            );
        } else if step > COARSE_GRID * gap {
            add(
                1,
                format!(
                    "Interpolate's grid (every {step:.0} days) is much coarser than the dates (a median gap of \
                     {gap:.0} days): it smooths the data away."
                ),
            );
        }
    }
    // 5: the quality band
    if info.sel.qa_band == 0 {
        if is_on("clear_observations") {
            add(1, "Clear-sky observations counts every date as clear: the series has no quality band.".into());
        }
        if is_on("interpolate") {
            add(0, "Interpolate without a quality band: clouds stay in the series.".into());
        }
    }
    // 6: a trend over few years
    if is_on("linear_trend") || is_on("cat") {
        let mut years: Vec<i64> = days.iter().map(|&d| year_of(d)).collect();
        years.dedup();
        if years.len() < FEW_PERIODS {
            let which = match (is_on("linear_trend"), is_on("cat")) {
                (true, true) => "Linear trend and CAT fit",
                (true, false) => "Linear trend fits",
                _ => "CAT fits",
            };
            add(
                1,
                format!(
                    "{which} {} yearly values ({y0} to {y1}): fewer than {FEW_PERIODS} periods, a trend of that \
                     reads little.",
                    years.len()
                ),
            );
        }
    }
    out
}

/// The advice as the Classification window shows its warnings: a coloured dot and the text.
fn advice_ui(ui: &mut Ui, items: &[Advice]) {
    if items.is_empty() {
        ui.weak("Nothing to look at.");
    }
    for a in items {
        ui.horizontal(|ui| {
            let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
            ui.painter().circle_filled(dot.center(), 4.0, crate::sample_check::level_color(a.level));
            ui.add(egui::Label::new(&a.text).wrap());
        });
    }
}

/// The chain on the series: each step's tool through `run_job` into a subfolder of `root` (01_interpolate...), the
/// analysis series of Interpolate and Standardize read by the steps after them from their GeoTIFF. A step the
/// series cannot take (`problem`) is left out and reported. Outputs on the window of the input, all of them: the
/// steps on the analysis series run on its whole extent, which is that window. `progress` is of the whole chain.
pub(crate) fn run_chain(
    steps: &[ChainStep],
    input: &ChainInput,
    root: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Chain, String> {
    if steps.is_empty() {
        return Err("no step is on".into());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let n = steps.len();
    // the analysis series once Interpolate or Standardize wrote it (its file), else the raw one
    let mut analysis: Option<(Arc<CubeInfo>, String)> = None;
    let mut outputs: Vec<Value> = vec![];
    let mut skipped: Vec<(String, String)> = vec![];
    for (k, step) in steps.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let tool = step.tool;
        let from_raw = RAW_STEPS.contains(&tool.id.as_str()) || analysis.is_none();
        let (info, roles, window, source) = match (&analysis, from_raw) {
            (Some((a, file)), false) => (a.clone(), BTreeMap::new(), None, Some(file.clone())),
            _ => (input.info.clone(), input.roles.clone(), Some(input.window), None),
        };
        if let Some(why) = problem(tool, &info, false) {
            skipped.push((tool.id.clone(), why));
            continue;
        }
        let dir = root.join(format!("{:02}_{}", k + 1, tool.id));
        let work = dir.join("work");
        std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
        let map_t = if source.is_none() { input.map_t } else { info.t().saturating_sub(1) };
        let mut spec = base_spec(&info, tool, &step.params, map_t, input.threads);
        series_inputs(&info, tool, &roles, &work.join("series"), &mut spec)?;
        spec.window = window;
        spec.output_dir = dir.to_string_lossy().into_owned();
        let win = window.unwrap_or([0, 0, info.width, info.height]);
        let spec_path = dir.join("job.json");
        let _ = std::fs::write(&spec_path, serde_json::to_string_pretty(&spec).unwrap_or_default());
        // its provenance as it starts: the series the step read (for one on the analysis series, the file of the
        // step that wrote it), its Python
        let mut prov = Provenance::start("zeit", &tool.id, &tool.name);
        prov.set_params(&step.params);
        prov.scope = match &source {
            Some(_) => format!("the analysis series ({} x {} px)", info.width, info.height),
            None => input.scope.clone(),
        };
        prov.scope_index = input.scope_index;
        prov.window = Some(win);
        prov.threads = spec.threads;
        prov.series = Some(match &source {
            Some(file) => SeriesRecord::of(&info, "analysis series", std::slice::from_ref(file)),
            None => SeriesRecord::of(&info, &input.name, &input.inputs),
        });
        prov.extra.insert(
            "chain".into(),
            json!({"task": "Time series analysis", "step": k + 1, "of": n, "folder": root.to_string_lossy(),
                   "reads": source.as_deref().unwrap_or("the series")}),
        );
        let to = dir.join("python").to_string_lossy().into_owned();
        match crate::python::run_code(&info, tool, &step.params, &roles, win, spec.threads, &to) {
            Ok(code) if std::fs::write(dir.join(SCRIPT), &code).is_ok() => prov.python = SCRIPT.into(),
            Ok(_) => prov.python_note = format!("{SCRIPT} could not be written"),
            Err(e) => prov.python_note = e,
        }
        prov.write(&dir);
        let log = JobLog { path: dir.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} step {} of {n}: {}\ntool: {}\nreads: {}\nwindow: {win:?}\nspec: {}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            k + 1,
            tool.name,
            tool.id,
            source.as_deref().unwrap_or("the series"),
            spec_path.display(),
            dir.display()
        ));
        let logged = Mutex::new((-1.0, -1.0));
        let step_progress = |f: f64, m: &str| {
            progress((k as f64 + f) / n as f64, &format!("step {}/{n}: {}: {m}", k + 1, tool.name));
            let now = log.t0.elapsed().as_secs_f64();
            let mut l = logged.lock().unwrap();
            if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                log.line(&format!("{:3.0}%  {m}", f * 100.0));
                *l = (f, now);
            }
        };
        let t0 = Instant::now();
        let r = run_job(&spec, &step_progress, cancel);
        prov.seconds = t0.elapsed().as_secs_f64();
        let res = match r {
            Ok(res) => res,
            Err(e) => {
                let cancelled = e == "cancelled";
                log.line(if cancelled { "cancelled" } else { "failed" });
                prov.state = if cancelled { "cancelled" } else { "failed" }.into();
                prov.error = if cancelled { String::new() } else { e.clone() };
                prov.write(&dir);
                return Err(if cancelled { e } else { format!("{}: {e}", tool.name) });
            }
        };
        prov.outputs = res
            .outputs
            .iter()
            .map(|o| OutputRecord {
                name: o["name"].as_str().or(o["id"].as_str()).unwrap_or("").to_string(),
                path: o["path"].as_str().unwrap_or("").to_string(),
            })
            .collect();
        for o in &prov.outputs {
            log.line(&format!("output: {} -> {}", o.name, o.path));
        }
        log.line("done");
        prov.state = "done".into();
        prov.write(&dir);
        if ANALYSIS_STEPS.contains(&tool.id.as_str()) {
            let file = res
                .outputs
                .iter()
                .find(|o| o["id"] == "series")
                .and_then(|o| o["path"].as_str())
                .ok_or_else(|| format!("{}: no series was written", tool.name))?
                .to_string();
            let next = janus_cube::open_cube(std::slice::from_ref(&file), &BandSelection::default())
                .map_err(|e| format!("{}: its series could not be opened: {e}", tool.name))?;
            analysis = Some((Arc::new(next), file));
        }
        // each output named by its step, the layers it becomes being of different tools
        for mut o in res.outputs {
            let name = o["name"].as_str().or(o["id"].as_str()).unwrap_or("").to_string();
            o["name"] = json!(format!("{}: {name}", tool.name));
            outputs.push(o);
        }
    }
    if outputs.is_empty() {
        let why: Vec<String> = skipped.iter().map(|(t, w)| format!("{t}: {w}")).collect();
        return Err(format!("no step could run: {}", why.join("; ")));
    }
    Ok(Chain { result: JobResult { outputs, window: input.window }, skipped })
}

impl State {
    pub(crate) fn tsa_menu(&mut self, ui: &mut Ui) {
        if ui
            .add(egui::Button::selectable(self.tsa.open, "Time series analysis (FORCE)..."))
            .on_hover_text(
                "FORCE's chain on the series: clear-sky observations, interpolation, standardization, then temporal \
                 metrics, fold by period, linear trend, CAT and polar phenology, as one task",
            )
            .clicked()
        {
            self.tsa.open = !self.tsa.open;
            ui.close();
        }
    }

    pub(crate) fn tsa_window(&mut self, ctx: &egui::Context) {
        if !self.tsa.open {
            return;
        }
        let mut open = true;
        let width = (ctx.content_rect().width() * 0.4).clamp(420.0, 700.0);
        crate::panels::fitted_window("Time series analysis", ctx, width)
            .id(egui::Id::new("tsa window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.tsa_ui(ui)));
        self.tsa.open = open;
        if self.tsa_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn tsa_running(&self) -> bool {
        self.zeit.jobs.iter().any(|j| j.tool == JOB_TOOL && j.status.lock().unwrap().state == JobState::Running)
    }

    pub(crate) fn tsa_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.colored_label(crate::theme::error(), "Open a series first.");
            return;
        };
        // what to look at, once a frame: from the dates and the parameters of the steps that are on
        let mut held: BTreeMap<String, BTreeMap<String, Value>> = BTreeMap::new();
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            if let Some(tool) = manifest().tool(id).filter(|_| self.tsa.on[i]) {
                held.insert(id.to_string(), self.zeit.params(tool).clone());
            }
        }
        let to_look = advice(&info, &self.tsa.on, &|id| held.get(id).cloned().unwrap_or_default());
        let n_look = to_look.iter().filter(|a| a.level > 0).count();
        ui.horizontal(|ui| {
            for (i, name) in ["Input", "Steps", "Output"].into_iter().enumerate() {
                let text = if i == 1 && n_look > 0 { format!("{name} ({n_look})") } else { name.to_string() };
                ui.selectable_value(&mut self.tsa.tab, i, RichText::new(text).strong());
            }
        });
        ui.separator();
        match self.tsa.tab {
            0 => self.tsa_input_tab(ui, &info),
            1 => {
                self.tsa_steps_tab(ui, &info);
                crate::panels::section(ui, "To look at");
                advice_ui(ui, &to_look);
            }
            _ => {
                crate::panels::section(ui, "To look at");
                advice_ui(ui, &to_look);
                self.tsa_output_tab(ui, &info);
            }
        }
    }

    /// The series the chain starts from.
    fn tsa_input_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let n = info.t();
        let label = |t: usize| info.layers.get(t).map_or(String::new(), |l| l.label.clone());
        ui.label(RichText::new(crate::zeit::series_name(info, &inputs)).strong());
        ui.label(format!("{n} dates, {} to {}", label(0), label(n.saturating_sub(1))));
        if info.time_is_date && n > 1 {
            match irregular(info) {
                None => ui.label(format!("Regular: a date every {} days.", info.unix_day(1) - info.unix_day(0))),
                Some(_) => ui.label("Irregular dates (Interpolate puts them on a regular grid)."),
            };
        } else if !info.time_is_date {
            ui.colored_label(crate::theme::error(), "No dates were found: the steps need them.");
        }
        ui.label(format!("Band shown: {}", info.selection_text()));
        if info.sel.qa_band > 0 {
            ui.label(format!("Quality band {} (rule: {}).", info.sel.qa_band, info.sel.qa_rule.id()));
        } else {
            ui.weak("No quality band: set one in the series' settings to mask clouds before Interpolate.");
        }
        ui.add_space(4.0);
        ui.weak(
            "The steps run on the active series (the band shown); the analysis series made by Interpolate and \
             Standardize is read by the steps after them.",
        );
    }

    /// The steps, each with its tool's own parameters (the ones the tool's window edits).
    fn tsa_steps_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let regular =
            self.tsa.on[1] && manifest().tool("interpolate").is_some_and(|t| problem(t, info, false).is_none());
        for (i, (id, label, line)) in STEPS.into_iter().enumerate() {
            let Some(tool) = manifest().tool(id) else { continue };
            let why = problem(tool, info, regular);
            ui.add_space(2.0);
            ui.checkbox(&mut self.tsa.on[i], RichText::new(label).strong()).on_hover_text(&tool.description);
            ui.weak(line);
            if let Some(w) = &why {
                ui.colored_label(crate::theme::error(), format!("Not for this series: {w}"));
            }
            if self.tsa.on[i] {
                ui.indent(("tsa step", id), |ui| {
                    if self.tool_params_ui(ui, tool) {
                        self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
                    }
                });
            }
        }
    }

    /// The scope, Run, the progress of the chain under way and where it wrote.
    fn tsa_output_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        if self.tsa.scope == 2 && self.series.roi_rect.is_none() {
            self.tsa.scope = 0;
        }
        let roi = self.series.roi_rect.is_some();
        ComboBox::from_id_salt("tsa scope").width(ui.available_width()).selected_text(SCOPES[self.tsa.scope]).show_ui(
            ui,
            |ui| {
                for (i, s) in SCOPES.iter().enumerate() {
                    if ui.add_enabled(i != 2 || roi, egui::Button::selectable(self.tsa.scope == i, *s)).clicked() {
                        self.tsa.scope = i;
                    }
                }
            },
        );
        let (window, scope_name) = self.run_window(self.tsa.scope);
        ui.weak(format!("{scope_name}: {} x {} px", window[2] - window[0], window[3] - window[1]));
        self.output_folder_ui(ui);
        let on: Vec<&str> = (0..STEPS.len()).filter(|&i| self.tsa.on[i]).map(|i| STEPS[i].1).collect();
        ui.label(if on.is_empty() { "No step is on.".to_string() } else { format!("Runs: {}.", on.join(", ")) });
        let running = self.tsa_running();
        let blocked = if on.is_empty() {
            Some("turn at least one step on (Steps)".to_string())
        } else if running {
            Some("a chain is running".to_string())
        } else {
            self.tsa_blocked(info)
        };
        if ui
            .add_enabled(blocked.is_none(), egui::Button::new("Run").min_size(egui::vec2(ui.available_width(), 0.0)))
            .clicked()
        {
            self.tsa_run(&ui.ctx().clone());
        }
        if let Some(why) = &blocked
            && !running
        {
            ui.colored_label(crate::theme::error(), format!("Not yet: {why}."));
        }
        // the last chain: its progress and Cancel, or how it ended and its folder
        let mut open_folder = None;
        if let Some(j) = self.zeit.jobs.iter().rev().find(|j| j.tool == JOB_TOOL) {
            let s = j.status.lock().unwrap();
            ui.separator();
            ui.label(&j.title);
            match s.state {
                JobState::Running => {
                    ui.add(egui::ProgressBar::new(s.progress as f32).text(format!(
                        "{:.0}%  {:.0} s  {}",
                        s.progress * 100.0,
                        j.started.elapsed().as_secs_f64(),
                        s.message
                    )));
                    if ui.button("Cancel").clicked() {
                        j.cancel.store(true, Ordering::Relaxed);
                    }
                }
                state => {
                    ui.label(match state {
                        JobState::Done => format!("Done in {:.0} s. {}", s.seconds, s.message),
                        JobState::Cancelled => "Cancelled.".to_string(),
                        _ => format!("Failed: {}", s.error),
                    });
                    if ui.button("Folder").on_hover_text(&j.output_dir).clicked() {
                        open_folder = Some(j.output_dir.clone());
                    }
                }
            }
        }
        if let Some(d) = open_folder {
            crate::platform::open_in_explorer(&d);
        }
    }

    /// Why the chain cannot run on the series, if no step of it can.
    fn tsa_blocked(&self, info: &CubeInfo) -> Option<String> {
        let regular = self.tsa.on[1];
        let mut reasons = vec![];
        for (i, (id, label, _)) in STEPS.into_iter().enumerate() {
            let Some(tool) = manifest().tool(id).filter(|_| self.tsa.on[i]) else { continue };
            // a step that can run: the chain can
            let w = problem(tool, info, regular)?;
            reasons.push(format!("{label}: {w}"));
        }
        Some(reasons.join("; "))
    }

    /// The chain's steps that are on, with the parameters their windows hold.
    fn tsa_steps(&mut self) -> Vec<ChainStep> {
        let mut steps = vec![];
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            if let Some(tool) = manifest().tool(id).filter(|_| self.tsa.on[i]) {
                steps.push(ChainStep { tool, params: self.zeit.params(tool).clone() });
            }
        }
        steps
    }

    /// Starts the chain as one task (Tools > Tasks): the steps on, on the scope, into a folder of its own.
    pub(crate) fn tsa_run(&mut self, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            self.error_popup = Some("no series is open".into());
            return;
        };
        let steps = self.tsa_steps();
        if steps.is_empty() {
            self.error_popup = Some("no step of the Time series analysis is on".into());
            return;
        }
        let layer = self.layers.get(self.active).map(|l| (l.name.clone(), l.inputs.clone()));
        let inputs = layer.as_ref().map(|l| l.1.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(&info, &inputs);
        let (window, scope) = self.run_window(self.tsa.scope);
        let roles = self.active_roles();
        let input = ChainInput {
            info: info.clone(),
            name: layer.map_or(name.clone(), |l| l.0),
            inputs,
            roles,
            window,
            scope: scope.to_string(),
            scope_index: self.tsa.scope,
            map_t: self.display.t.min(info.t().saturating_sub(1)),
            threads: self.processing_threads(),
        };
        let root = self.results_dir().join(format!("{name}_tsa_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&root) {
            self.error_popup = Some(format!("could not create {}: {e}", root.display()));
            return;
        }
        let title =
            format!("Time series analysis - {scope} ({} x {} px)", window[2] - window[0], window[3] - window[1]);
        let log = JobLog { path: root.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} job: {title}\nsteps: {}\nwindow: {window:?}\nthreads: {}\noutput: {}\n",
            crate::zeit::timestamp_text(),
            steps.iter().map(|s| s.tool.id.as_str()).collect::<Vec<_>>().join(", "),
            input.threads,
            root.display()
        ));
        let log_path = log.path.to_string_lossy().to_string();
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, dir, ctx) = (status.clone(), cancel.clone(), root.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("zeit-tsa".into())
            .spawn(move || {
                let t0 = Instant::now();
                // progress to the log every 5 s or 10%
                let logged = Mutex::new((-1.0, -1.0));
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
                let r = run_chain(&steps, &input, &dir, &progress, &cn);
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(chain) => {
                        for (tool, why) in &chain.skipped {
                            log.line(&format!("skipped {tool}: {why}"));
                        }
                        for o in &chain.result.outputs {
                            let name = o["name"].as_str().or(o["id"].as_str()).unwrap_or("");
                            log.line(&format!("output: {name} -> {}", o["path"].as_str().unwrap_or("")));
                        }
                        log.line("done");
                        s.message = match chain.skipped.len() {
                            0 => String::new(),
                            _ => format!(
                                "Left out: {}.",
                                chain.skipped.iter().map(|(t, w)| format!("{t} ({w})")).collect::<Vec<_>>().join("; ")
                            ),
                        };
                        s.state = JobState::Done;
                        s.progress = 1.0;
                        s.result = Some(chain.result);
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
            tool: JOB_TOOL.into(),
            cube: info.id,
            output_dir: root.to_string_lossy().to_string(),
            log_path,
            started: Instant::now(),
            status,
            cancel,
            loaded: false,
        });
        self.zeit.show_tasks = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_cube::open_cube;
    use janus_zeit::job::JobSpec;

    const W: usize = 8;
    const H: usize = 6;
    const GT: [f64; 6] = [500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];

    /// 40 dates over three years, a gap of 20 to 40 days between them, one file a date: a seasonal curve that varies
    /// from pixel to pixel, about one value in nine missing, one pixel missing at every date.
    fn series(dir: &Path) -> Arc<CubeInfo> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap();
        let mut rng = 12345u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        let mut day = 17900i64;
        for k in 0..40 {
            day += 20 + (k * 7 % 21);
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let path = dir.join(format!("s_{y:04}-{m:02}-{d:02}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, 1).unwrap();
            ds.set_geo_transform(&GT).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            let data: Vec<f32> = (0..W * H)
                .map(|p| {
                    let season = (day as f32 / 365.25 * std::f32::consts::TAU + p as f32 * 0.1).sin();
                    let v = 0.5 + 0.3 * season + 0.05 * next();
                    if p == 0 || next() < 0.11 { -9999.0 } else { v }
                })
                .collect();
            let mut band = ds.rasterband(1).unwrap();
            band.set_no_data_value(Some(-9999.0)).unwrap();
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
        }
        Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap())
    }

    fn input(info: &Arc<CubeInfo>, window: [usize; 4]) -> ChainInput {
        ChainInput {
            info: info.clone(),
            name: "s".into(),
            inputs: vec![],
            roles: BTreeMap::new(),
            window,
            scope: "test".into(),
            scope_index: 0,
            map_t: info.t() - 1,
            threads: 2,
        }
    }

    fn step(id: &str) -> ChainStep {
        ChainStep { tool: manifest().tool(id).unwrap(), params: BTreeMap::new() }
    }

    /// A tool on its own: its spec built as the chain builds it, over `window` of `info`.
    fn alone(id: &str, info: &CubeInfo, window: Option<[usize; 4]>, out: &Path) -> JobResult {
        let tool = manifest().tool(id).unwrap();
        std::fs::create_dir_all(out).unwrap();
        let mut spec: JobSpec = base_spec(info, tool, &BTreeMap::new(), info.t() - 1, 2);
        series_inputs(info, tool, &BTreeMap::new(), &out.join("in"), &mut spec).unwrap();
        spec.window = window;
        spec.output_dir = out.join("out").to_string_lossy().into_owned();
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap()
    }

    /// A raster's bands (NoData as NaN) and their descriptions.
    fn read(path: &str) -> (Vec<Vec<f32>>, Vec<String>) {
        let ds = gdal::Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        let mut descriptions = vec![];
        let bands = (1..=ds.raster_count())
            .map(|b| {
                use gdal::Metadata;
                let band = ds.rasterband(b).unwrap();
                descriptions.push(band.description().unwrap());
                band.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
            })
            .collect();
        (bands, descriptions)
    }

    fn same(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || x.is_nan() && y.is_nan())
    }

    #[test]
    fn the_chain_is_the_tools_run_one_by_one() {
        let base = std::env::temp_dir().join("janus-tsa-test-chain");
        let info = series(&base.join("series"));
        assert_eq!(info.t(), 40);
        let steps = [step("interpolate"), step("temporal_metrics"), step("fold"), step("polar_metrics")];
        let window = [0, 0, W, H];
        let chain =
            run_chain(&steps, &input(&info, window), &base.join("chain"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert!(chain.skipped.is_empty(), "{:?}", chain.skipped);
        assert_eq!(chain.result.window, window);
        // by hand: Interpolate on the raw series, then each product on the interpolated file
        let by_hand = base.join("by hand");
        let first = alone("interpolate", &info, Some(window), &by_hand.join("interpolate"));
        let file = first.outputs[0]["path"].as_str().unwrap().to_string();
        let analysis = open_cube(&[file], &BandSelection::default()).unwrap();
        let mut want = first.outputs.clone();
        for id in ["temporal_metrics", "fold", "polar_metrics"] {
            want.extend(alone(id, &analysis, None, &by_hand.join(id)).outputs);
        }
        let got = &chain.result.outputs;
        assert_eq!(got.len(), want.len());
        assert!(got.len() > 4, "{} outputs", got.len());
        let mut compared = 0;
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g["id"], w["id"]);
            let (gb, gd) = read(g["path"].as_str().unwrap());
            let (wb, wd) = read(w["path"].as_str().unwrap());
            assert_eq!(gd, wd, "{}: the bands' labels", g["id"]);
            assert_eq!(gb.len(), wb.len(), "{}", g["id"]);
            for (b, (x, y)) in gb.iter().zip(&wb).enumerate() {
                assert!(same(x, y), "{} band {b}", g["id"]);
                compared += x.len();
            }
        }
        assert!(compared > 1000, "{compared} values compared");
        // a step's folder: its spec, log, provenance (the file it read, for a step on the analysis series), Python
        let fold = base.join("chain").join("03_fold");
        for f in ["job.json", "job.log", "provenance.json", "rerun.py"] {
            assert!(fold.join(f).is_file(), "{f}");
        }
        let p = Provenance::read(&fold).unwrap();
        assert_eq!(p.state, "done");
        assert_eq!(p.tool, "fold");
        let read_file = &p.series.unwrap().inputs[0];
        assert!(read_file.replace('\\', "/").ends_with("01_interpolate/interpolate_series.tif"), "{read_file}");
        assert!(Provenance::read(&base.join("chain").join("01_interpolate")).unwrap().series.unwrap().dates == 40);
    }

    #[test]
    fn an_irregular_series_is_refused_by_the_polar_phenology_without_interpolate() {
        let base = std::env::temp_dir().join("janus-tsa-test-polar");
        let info = series(&base.join("series"));
        let steps = [step("temporal_metrics"), step("polar_metrics")];
        let chain =
            run_chain(&steps, &input(&info, [0, 0, W, H]), &base.join("chain"), &|_, _| {}, &AtomicBool::new(false))
                .unwrap();
        assert_eq!(chain.skipped.len(), 1, "{:?}", chain.skipped);
        assert_eq!(chain.skipped[0].0, "polar_metrics");
        assert!(chain.skipped[0].1.contains("not evenly spaced") && chain.skipped[0].1.contains("Interpolate"));
        assert!(chain.result.outputs.iter().all(|o| o["path"].as_str().unwrap().contains("temporal_metrics")));
        assert!(!base.join("chain").join("02_polar_metrics").exists());
        // the window's own check says the same, and lets it through once Interpolate comes first
        let polar = manifest().tool("polar_metrics").unwrap();
        assert!(problem(polar, &info, false).is_some());
        assert!(problem(polar, &info, true).is_none());
        // with Interpolate on, nothing is left out
        let steps = [step("interpolate"), step("polar_metrics")];
        let chain =
            run_chain(&steps, &input(&info, [0, 0, W, H]), &base.join("with"), &|_, _| {}, &AtomicBool::new(false))
                .unwrap();
        assert!(chain.skipped.is_empty(), "{:?}", chain.skipped);
        assert!(base.join("with").join("02_polar_metrics").is_dir());
        // only a step the series cannot take: no chain
        let e = run_chain(
            &[step("polar_metrics")],
            &input(&info, [0, 0, W, H]),
            &base.join("none"),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .err()
        .expect("an error");
        assert!(e.contains("no step could run") && e.contains("polar_metrics"), "{e}");
    }

    #[test]
    fn every_output_of_a_chain_lies_on_its_window() {
        let base = std::env::temp_dir().join("janus-tsa-test-window");
        let info = series(&base.join("series"));
        // a window: the steps on the analysis series run on its whole extent, which is the window
        let window = [2, 1, 7, 5];
        let steps = [step("interpolate"), step("temporal_metrics"), step("fold")];
        let chain =
            run_chain(&steps, &input(&info, window), &base.join("chain"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(chain.result.window, window);
        assert!(chain.result.outputs.len() >= 3);
        for o in &chain.result.outputs {
            let ds = gdal::Dataset::open(o["path"].as_str().unwrap()).unwrap();
            assert_eq!(ds.raster_size(), (5, 4), "{}", o["id"]);
            let gt = ds.geo_transform().unwrap();
            assert_eq!([gt[0], gt[3]], [GT[0] + 2.0 * 30.0, GT[3] - 30.0], "{}", o["id"]);
        }
        // and the values are those of the whole image's at those pixels
        let whole =
            run_chain(&steps, &input(&info, [0, 0, W, H]), &base.join("whole"), &|_, _| {}, &AtomicBool::new(false))
                .unwrap();
        let (a, _) = read(chain.result.outputs[1]["path"].as_str().unwrap());
        let (b, _) = read(whole.result.outputs[1]["path"].as_str().unwrap());
        for (x, y) in a.iter().zip(&b) {
            let cut: Vec<f32> = (1..5).flat_map(|r| (2..7).map(move |c| (r, c))).map(|(r, c)| y[r * W + c]).collect();
            assert!(same(x, &cut));
        }
    }

    /// A series in memory (no file): a date every `step` days from `from` to `to` (both y, m, d).
    fn dates(from: (i32, u32, u32), to: (i32, u32, u32), step: usize) -> CubeInfo {
        use janus_cube::dates::days_from_civil;
        let (a, b) = (days_from_civil(from.0, from.1, from.2), days_from_civil(to.0, to.1, to.2));
        let layers = (a..=b)
            .step_by(step)
            .map(|d| janus_cube::Layer { time: d as f64 * 86400.0, band: 1, ..Default::default() })
            .collect();
        CubeInfo { layers, width: 4, height: 4, time_is_date: true, bands_per_date: 1, ..Default::default() }
    }

    fn only(ids: &[&str]) -> [bool; 8] {
        let mut on = [false; 8];
        for id in ids {
            on[STEPS.iter().position(|s| s.0 == *id).unwrap()] = true;
        }
        on
    }

    /// The advice with the parameters given by (tool, id, value).
    fn look(info: &CubeInfo, on: &[bool; 8], set: &[(&str, &str, Value)]) -> Vec<Advice> {
        advice(info, on, &|tool| set.iter().filter(|s| s.0 == tool).map(|s| (s.1.to_string(), s.2.clone())).collect())
    }

    fn has(a: &[Advice], level: u8, part: &str) -> bool {
        a.iter().any(|x| x.level == level && x.text.contains(part))
    }

    #[test]
    fn a_year_with_few_dates_is_found() {
        // every 16 days from 2018 to 2022, then the dates of 2020 thinned out to three
        let mut info = dates((2018, 1, 1), (2022, 12, 31), 16);
        let year = |l: &janus_cube::Layer| janus_zeit::numeric::civil((l.time / 86400.0) as i64).0;
        let mut seen = 0;
        info.layers.retain(|l| {
            year(l) != 2020 || {
                seen += 1;
                seen % 6 == 0
            }
        });
        let on = only(&["fold"]);
        let a = look(&info, &on, &[]);
        assert!(has(&a, 1, "2020 has 3 dates (the others 23)"), "{a:?}");
        assert_eq!(a.iter().filter(|x| x.text.contains("has")).count(), 1, "{a:?}");
        // the edge years are not judged by their count, and a step that reads no statistic does not care
        assert!(!has(&a, 1, "2018 has") && !has(&a, 1, "2022 has"));
        assert!(!has(&look(&info, &only(&["interpolate"]), &[]), 1, "2020 has"));
    }

    #[test]
    fn a_cut_last_year_is_found_with_fold_by_year_on() {
        // 2019 to June 2024, a date every 16 days
        let info = dates((2019, 1, 1), (2024, 6, 20), 16);
        let a = look(&info, &only(&["fold"]), &[]);
        assert!(has(&a, 1, "2024 covers only January to June: its yearly values read 5 months"), "{a:?}");
        assert!(!has(&a, 1, "2019 covers"), "{a:?}");
        // off, or by quarter (of every year), it is not looked at
        assert!(!has(&look(&info, &only(&["temporal_metrics"]), &[]), 1, "covers only"));
        let q = look(&info, &only(&["fold"]), &[("fold", "by", json!("quarter"))]);
        assert!(!has(&q, 1, "covers only"), "{q:?}");
        // a first year starting in July
        let from = dates((2019, 7, 3), (2023, 12, 31), 16);
        let a = look(&from, &only(&["linear_trend"]), &[]);
        assert!(has(&a, 1, "2019 covers only July to December"), "{a:?}");
    }

    #[test]
    fn the_polar_phenology_on_irregular_dates_blocks_unless_interpolate_comes_first() {
        let mut info = dates((2018, 1, 1), (2022, 12, 31), 16);
        info.layers.remove(5);
        let a = look(&info, &only(&["polar_metrics"]), &[]);
        assert!(has(&a, 2, "Polar phenology: the dates are not evenly spaced"), "{a:?}");
        let a = look(&info, &only(&["interpolate", "polar_metrics"]), &[]);
        assert!(!a.iter().any(|x| x.level == 2), "{a:?}");
    }

    #[test]
    fn an_interpolate_grid_denser_or_coarser_than_the_dates_is_found() {
        let info = dates((2018, 1, 1), (2022, 12, 31), 16);
        let on = only(&["interpolate"]);
        let dense = look(&info, &on, &[("interpolate", "step", json!(5))]);
        assert!(has(&dense, 1, "every 5 days) is denser than the dates (a median gap of 16 days)"), "{dense:?}");
        let coarse = look(&info, &on, &[("interpolate", "step", json!(100))]);
        assert!(has(&coarse, 1, "every 100 days) is much coarser"), "{coarse:?}");
        let fine = look(&info, &on, &[("interpolate", "step", json!(16))]);
        assert!(!has(&fine, 1, "Interpolate's grid"), "{fine:?}");
        // and the default of the tool is 16 days
        assert!(!has(&look(&info, &on, &[]), 1, "Interpolate's grid"));
    }

    #[test]
    fn clear_sky_observations_without_a_quality_band_is_noted() {
        let mut info = dates((2018, 1, 1), (2022, 12, 31), 16);
        let a = look(&info, &only(&["clear_observations", "interpolate"]), &[]);
        assert!(has(&a, 1, "Clear-sky observations counts every date as clear"), "{a:?}");
        assert!(has(&a, 0, "clouds stay in the series"), "{a:?}");
        info.sel.qa_band = 2;
        let a = look(&info, &only(&["clear_observations", "interpolate"]), &[]);
        assert!(!has(&a, 1, "Clear-sky") && !has(&a, 0, "clouds"), "{a:?}");
    }

    #[test]
    fn a_trend_over_too_few_years_is_found() {
        let info = dates((2020, 1, 1), (2022, 12, 31), 16);
        let a = look(&info, &only(&["linear_trend", "cat"]), &[]);
        assert!(has(&a, 1, "Linear trend and CAT fit 3 yearly values (2020 to 2022)"), "{a:?}");
        let long = dates((2015, 1, 1), (2022, 12, 31), 16);
        assert!(!has(&look(&long, &only(&["linear_trend"]), &[]), 1, "yearly values (20"));
    }

    #[test]
    fn nothing_on_blocks_and_a_series_without_dates_stops_the_polar_phenology() {
        let info = dates((2018, 1, 1), (2022, 12, 31), 16);
        let a = look(&info, &[false; 8], &[]);
        assert_eq!(a, vec![Advice { level: 2, text: "No step is on.".into() }]);
        let mut nodates = info.clone();
        nodates.time_is_date = false;
        let a = look(&nodates, &only(&["polar_metrics"]), &[]);
        assert!(has(&a, 2, "needs dates"), "{a:?}");
    }

    #[test]
    fn a_well_covered_series_has_nothing_at_level_one_or_two() {
        let mut info = dates((2018, 1, 1), (2022, 12, 31), 16);
        info.sel.qa_band = 2;
        let a = look(&info, &only(&["interpolate", "temporal_metrics", "fold", "linear_trend"]), &[]);
        assert!(!a.iter().any(|x| x.level > 0), "{a:?}");
    }

    #[test]
    fn a_cancelled_chain_stops() {
        let base = std::env::temp_dir().join("janus-tsa-test-cancel");
        let info = series(&base.join("series"));
        let cancel = AtomicBool::new(true);
        let e =
            run_chain(&[step("interpolate")], &input(&info, [0, 0, W, H]), &base.join("chain"), &|_, _| {}, &cancel)
                .err()
                .expect("an error");
        assert_eq!(e, "cancelled");
    }
}
