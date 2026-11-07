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
use janus_cube::{BandSelection, CubeInfo, CubeReader, QaRule};
use janus_zeit::job::{JobResult, run_job};
use janus_zeit::manifest::{Tool, manifest};
use janus_zeit::pixel::{Params, PixelInput, PixelResult, run_pixel};
use serde_json::{Value, json};

use crate::app::State;
use crate::provenance::{OutputRecord, Provenance, SCRIPT, SeriesRecord};
use crate::zeit::{Job, JobLog, JobState, JobStatus, applicability, base_spec, read_pixel_input, series_inputs};

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
    /// the chart of the pixel under the cursor (Steps)
    pub pixel: PixelChart,
    /// the series the last chain wrote (name, file), filled when it ends; the one to use in the Classification
    pub series_out: Arc<Mutex<Vec<(String, String)>>>,
    pub use_series: usize,
    /// a series chosen for the Classification that is still opening (its file): made active when it arrives
    pub pending_use: Option<String>,
    /// when Copy Python last copied (for its "Copied")
    pub copied: Option<Instant>,
}

impl Default for TsaUi {
    fn default() -> TsaUi {
        // Interpolate, Temporal metrics and Fold
        TsaUi {
            open: false,
            tab: 0,
            on: [false, true, false, true, true, false, false, false],
            scope: 0,
            pixel: PixelChart::default(),
            series_out: Arc::default(),
            use_series: 0,
            pending_use: None,
            copied: None,
        }
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
    // the chain's Python, as the steps that ran made it
    if !skipped.is_empty()
        && let Ok(code) = chain_code(steps, &skipped, input, &root.join("python").to_string_lossy())
    {
        let _ = std::fs::write(root.join(SCRIPT), code);
    }
    Ok(Chain { result: JobResult { outputs, window: input.window }, skipped })
}

/// The series a chain wrote (its name and file): the analysis series and the series products, what the
/// Classification can read the dates of.
pub(crate) fn series_outputs(result: &JobResult) -> Vec<(String, String)> {
    result
        .outputs
        .iter()
        .filter(|o| o["series"].as_bool() == Some(true))
        .filter_map(|o| {
            let name = o["name"].as_str().or(o["id"].as_str())?.to_string();
            Some((name, o["path"].as_str()?.to_string()))
        })
        .collect()
}

/// The steps the series cannot take (`problem`), with why: what `run_chain` leaves out, judged before it runs (the
/// steps on the analysis series by the raw series' dates, regular once an Interpolate that can run comes first).
pub(crate) fn left_out(steps: &[ChainStep], info: &CubeInfo) -> Vec<(String, String)> {
    let regular = steps.iter().any(|s| s.tool.id == "interpolate" && problem(s.tool, info, false).is_none());
    steps.iter().filter_map(|s| problem(s.tool, info, regular).map(|why| (s.tool.id.clone(), why))).collect()
}

/// The chain as one Python script, what `run_chain` does in Zeit: the series read once from its files over the
/// window, the steps' calls (`janus_zeit::python::calls`, those of each tool's own rerun.py) with their results
/// numbered along (`result1`, `result2`...), the steps on the analysis series reading the variable `analysis` that
/// Interpolate and Standardize leave, and every result written as a GeoTIFF in `out_dir`. `skipped` are the steps
/// the series cannot take (`left_out`, or what `run_chain` reported).
pub(crate) fn chain_code(
    steps: &[ChainStep],
    skipped: &[(String, String)],
    input: &ChainInput,
    out_dir: &str,
) -> Result<String, String> {
    use std::fmt::Write as _;
    let info = &input.info;
    let mut o = crate::python::script_head(info, "the Time series analysis chain", false, input.window);
    o += &crate::python::series_line(info);
    let (mut results, mut analysis, mut names) = (0usize, false, vec![]);
    for (k, step) in steps.iter().enumerate() {
        let tool = step.tool;
        if let Some((_, why)) = skipped.iter().find(|(t, _)| *t == tool.id) {
            let _ = writeln!(o, "\n# {}: left out ({why})", tool.name);
            continue;
        }
        let (code, inputs) =
            crate::python::tool_calls(info, tool, &step.params, &input.roles, Some(input.threads as i64))?;
        // the steps' calls all read the series (a DataArray); the ones on the analysis series read `analysis`
        let from = if RAW_STEPS.contains(&tool.id.as_str()) || !analysis { "series" } else { "analysis" };
        let mut code = code.replace("(series,", &format!("({from},"));
        if inputs != ["series"] || !code.contains(&format!("({from},")) {
            return Err(format!("{}: its Python does not read the series alone", tool.name));
        }
        // its results after those before it (highest first: result2 before result1)
        let n = (1..=9).rev().find(|m| code.contains(&format!("result{m}"))).unwrap_or(0);
        // through a placeholder first: the new names may have two digits, and "result1" is a prefix of "result12"
        for m in (1..=n).rev() {
            code = code.replace(&format!("result{m}"), &format!("\u{1}{m}\u{1}"));
        }
        for m in (1..=n).rev() {
            code = code.replace(&format!("\u{1}{m}\u{1}"), &format!("result{}", results + m));
            let step_name = format!("{:02}_{}", k + 1, tool.id);
            names.push((format!("result{}", results + m), if n > 1 { format!("{step_name}_r{m}") } else { step_name }));
        }
        let k0 = names.len() - n;
        names[k0..].reverse();
        let reads = if from == "series" { "the series" } else { "the analysis series" };
        let _ = writeln!(o, "\n# {}. {}, on {reads}\n{code}", k + 1, tool.name);
        if ANALYSIS_STEPS.contains(&tool.id.as_str()) {
            let _ = writeln!(o, "analysis = result{}  # the analysis series: the steps below read it", results + 1);
            analysis = true;
        }
        results += n;
    }
    if results == 0 {
        return Err("no step can run".into());
    }
    o += &crate::python::save_code(out_dir, &names);
    Ok(o)
}

// ---------------------------------------------------------------------------------------------------------------
// The chain on one pixel
// ---------------------------------------------------------------------------------------------------------------

/// The analysis series of one pixel, as the raster run writes it for the steps after Interpolate or Standardize: the
/// dates of its grid, and its values as float32 (what the GeoTIFF holds, which the next step reads back).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PixelAnalysis {
    /// days since 1970-01-01 of each date (None: the series has no dates)
    pub days: Option<Vec<i64>>,
    /// decimal year of each date
    pub years: Vec<f64>,
    /// NaN = no value
    pub values: Vec<f64>,
    /// Standardize is among the steps that made it: another scale than the observations'
    pub standardized: bool,
    /// what made it, for the legend
    pub label: String,
}

/// What the chain does to one pixel: each step's pixel run (a step the series cannot take, or that failed, with why)
/// and the analysis series the steps after Interpolate and Standardize read.
pub(crate) struct PixelChain {
    pub steps: Vec<(String, Result<PixelResult, String>)>,
    pub analysis: Option<PixelAnalysis>,
}

/// The values a raster step writes, as the step after it reads them: float32, no value as NaN.
fn as_written(y: &[Option<f64>]) -> Vec<f64> {
    y.iter().map(|v| v.map_or(f64::NAN, |v| f64::from(v as f32))).collect()
}

/// The analysis series after an Interpolate or Standardize step `id` that ran on `cur` and gave `r`: Interpolate's
/// values on its grid (the dates `janus_zeit::interpolate_days` gives, those the raster run labels its bands with),
/// Standardize's on the dates it read. `before` is what the series was (the step reads the analysis series).
fn analysis_after(
    id: &str,
    params: &Params,
    cur: &PixelInput,
    r: &PixelResult,
    before: Option<&PixelAnalysis>,
) -> Result<PixelAnalysis, String> {
    let overlay = r.overlays.first().ok_or("no series was made")?;
    let values = as_written(&overlay.y);
    let (days, years) = if id == "interpolate" {
        let days = janus_zeit::interpolate_days(params, cur)?;
        let years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
        (Some(days), years)
    } else {
        (cur.days.clone(), cur.years.clone())
    };
    if values.len() != years.len() {
        return Err(format!("{} values on {} dates", values.len(), years.len()));
    }
    Ok(PixelAnalysis {
        days,
        years,
        values,
        standardized: id == "standardize" || before.is_some_and(|b| b.standardized),
        label: match before {
            Some(b) => format!("{}, {}", b.label, overlay.label),
            None => overlay.label.clone(),
        },
    })
}

/// The chain on one pixel: each step's pixel run (`janus_zeit::run_pixel`, with the parameters the step holds over the
/// tool's defaults), the steps after Interpolate and Standardize reading the analysis series they leave (the raster
/// run's: its dates and its float32 values) and the others the raw series, as `run_chain` does. `skip` says why a
/// step cannot run (`problem`; its second argument is whether the step reads the analysis series); such a step is
/// left out as in the chain. A failed Interpolate or Standardize leaves the steps that read its series with that error.
pub(crate) fn pixel_chain(
    steps: &[ChainStep],
    input: &PixelInput,
    skip: &dyn Fn(&Tool, bool) -> Option<String>,
) -> PixelChain {
    let mut analysis: Option<PixelAnalysis> = None;
    let mut failed: Option<String> = None;
    let mut out = vec![];
    for step in steps {
        let tool = step.tool;
        let id = tool.id.as_str();
        let from_raw = RAW_STEPS.contains(&id) || (analysis.is_none() && failed.is_none());
        let result = (|| {
            if let Some(why) = skip(tool, !from_raw) {
                return Err(why);
            }
            if !from_raw && analysis.is_none() {
                return Err(format!("needs the analysis series: {}", failed.clone().unwrap_or_default()));
            }
            let params = Params::with_defaults(id, &step.params).ok_or_else(|| format!("{id}: unknown tool"))?;
            let cur = match (&analysis, from_raw) {
                (Some(a), false) => PixelInput {
                    years: a.years.clone(),
                    days: a.days.clone(),
                    values: a.values.clone(),
                    map_year: a.years.last().copied(),
                    ..Default::default()
                },
                _ => input.clone(),
            };
            let r = run_pixel(id, &params, &cur)?;
            let next = if ANALYSIS_STEPS.contains(&id) {
                let before = analysis.as_ref().filter(|_| !from_raw);
                Some(analysis_after(id, &params, &cur, &r, before)?)
            } else {
                None
            };
            Ok((r, next))
        })();
        match result {
            Ok((r, next)) => {
                if next.is_some() {
                    analysis = next;
                    failed = None;
                }
                out.push((tool.id.clone(), Ok(r)));
            }
            Err(e) => {
                // a step that cannot run is left out; one that failed takes the series with it
                if ANALYSIS_STEPS.contains(&id) && skip(tool, !from_raw).is_none() {
                    analysis = None;
                    failed = Some(format!("{} failed", tool.name));
                }
                out.push((tool.id.clone(), Err(e)));
            }
        }
    }
    PixelChain { steps: out, analysis }
}

/// One pixel of the series and what the chain does to it.
pub(crate) struct PixelView {
    pub info: Arc<CubeInfo>,
    /// the series' values at every date, quality band or not (NaN: no value)
    pub raw: Vec<f64>,
    /// whether each date has a value the quality band keeps
    pub clear: Vec<bool>,
    /// what the steps on the raw series read: the clear observations (the tests compare against it)
    #[cfg_attr(not(test), allow(dead_code))]
    pub input: PixelInput,
    pub chain: PixelChain,
}

/// The pixel (x, y) of the series, read as the chain reads it, and the chain on it. `map_t` is the date the map tools
/// take their year from.
pub(crate) fn pixel_view(
    info: &Arc<CubeInfo>,
    steps: &[ChainStep],
    x: usize,
    y: usize,
    map_t: usize,
) -> Result<PixelView, String> {
    let first = steps.first().ok_or("no step is on")?;
    if x >= info.width || y >= info.height {
        return Err(format!("the pixel ({x}, {y}) is outside the series"));
    }
    // the values as the map shows them but for the quality mask, which is applied below from the band's codes
    let mut unmasked = (**info).clone();
    unmasked.sel.qa_band = 0;
    let mut reader = CubeReader::new(Arc::new(unmasked));
    let n = info.t();
    let raw: Vec<f32> = (0..n).map(|t| reader.read_pixel(t, x, y).unwrap_or(f32::NAN)).collect();
    let rule = info.sel.qa_rule;
    let masked = info.sel.qa_band > 0 && rule != QaRule::None;
    let clear: Vec<bool> = (0..n)
        .map(|t| {
            raw[t].is_finite()
                && (!masked
                    || reader
                        .read_band_pixel(t, info.sel.qa_band, x, y, true)
                        .is_some_and(|q| !q.is_nan() && q >= 0.0 && rule.usable(q as u32)))
        })
        .collect();
    let kept: Vec<f32> = raw.iter().zip(&clear).map(|(&v, &c)| if c { v } else { f32::NAN }).collect();
    let input = read_pixel_input(info, first.tool, &BTreeMap::new(), &kept, x, y, map_t);
    let chain = pixel_chain(steps, &input, &|tool, analysis| problem(tool, info, analysis));
    Ok(PixelView { info: info.clone(), raw: raw.iter().map(|&v| f64::from(v)).collect(), clear, input, chain })
}

/// What a pixel chart is computed for: the pixel, the series' bands and quality rule, and the steps with their
/// parameters (and the version of the bands, which changes the fits).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PixelKey {
    cube: u64,
    x: usize,
    y: usize,
    series: String,
    steps: Vec<(String, String)>,
    version: u64,
}

type PixelPending = Arc<Mutex<Option<Result<PixelView, String>>>>;

/// The pixel chart's state: the last pixel the cursor rested on, what was computed (for the key it was asked for)
/// and the computation under way, off the UI thread.
#[derive(Default)]
pub struct PixelChart {
    /// the last pixel hovered on the map: (cube, x, y); it stays when the cursor goes to the window
    at: Option<(u64, usize, usize)>,
    shown: Option<(PixelKey, Result<PixelView, String>)>,
    job: Option<(PixelKey, PixelPending)>,
}

impl PixelChart {
    /// Takes in a finished computation and, if what is shown is not for `key` and none is under way, starts `work`.
    fn update(
        &mut self,
        key: &PixelKey,
        ctx: &egui::Context,
        work: impl FnOnce() -> Result<PixelView, String> + Send + 'static,
    ) {
        let done = self.job.as_ref().and_then(|(k, pending)| {
            let r = pending.lock().unwrap().take();
            r.map(|r| (k.clone(), r))
        });
        if done.is_some() {
            self.shown = done;
            self.job = None;
        }
        if self.job.is_some() || self.shown.as_ref().is_some_and(|(k, _)| k == key) {
            return;
        }
        let pending: PixelPending = Arc::new(Mutex::new(None));
        let (out, ctx2) = (pending.clone(), ctx.clone());
        let spawned = std::thread::Builder::new().name("tsa-pixel".into()).spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(work))
                .unwrap_or_else(|_| Err("the pixel could not be computed".into()));
            *out.lock().unwrap() = Some(r);
            ctx2.request_repaint();
        });
        match spawned {
            Ok(_) => self.job = Some((key.clone(), pending)),
            Err(e) => self.shown = Some((key.clone(), Err(format!("could not start the computation: {e}")))),
        }
    }
}

/// The pixel's series and what the chain does to it, as a chart: the observations (grey, and in the series' colour
/// where the quality band keeps them), the analysis series as a line and the overlays of each product step under its
/// name. Under it, the steps that gave no series or failed.
fn pixel_plot(ui: &mut Ui, v: &PixelView, x: usize, y: usize) {
    let info = &v.info;
    let dated = info.time_is_date;
    let at = |t: usize| if dated { info.layers[t].time } else { t as f64 };
    let weak = ui.visuals().weak_text_color();
    let series = crate::theme::on_plot(crate::theme::cursor_series());
    let grey = crate::theme::on_plot(egui::Color32::from_gray(140));
    let analysis = v.chain.analysis.as_ref();
    let standardized = analysis.is_some_and(|a| a.standardized);
    let has_qa = info.sel.qa_band > 0 && info.sel.qa_rule != QaRule::None;
    let products: Vec<(usize, &PixelResult)> = v
        .chain
        .steps
        .iter()
        .filter(|(id, _)| !ANALYSIS_STEPS.contains(&id.as_str()))
        .filter_map(|(id, r)| Some((STEPS.iter().position(|s| s.0 == id)?, r.as_ref().ok()?)))
        .collect();
    let entries = 2 + usize::from(analysis.is_some()) + products.len();
    let mut plot = egui_plot::Plot::new(("tsa pixel", info.id))
        .height(180.0)
        .allow_drag(false)
        .allow_scroll(false)
        .allow_zoom(false)
        .allow_boxed_zoom(false)
        .show_background(false)
        .show_x(false)
        .show_y(false)
        .grid_color(weak.gamma_multiply(0.55))
        .grid_fade(0.9)
        .custom_y_axes(vec![egui_plot::AxisHints::new_y().tick_label_color(weak)]);
    if crate::fit::legend_fits(ui, 180.0, entries) {
        plot = plot.legend(
            egui_plot::Legend::default()
                .position(egui_plot::Corner::LeftTop)
                .background_alpha(0.75)
                .color_conflict_handling(egui_plot::ColorConflictHandling::PickFirst),
        );
    }
    plot = if dated { crate::chart::time_axis(plot, Some(weak)) } else { plot };
    plot.show(ui, |p| {
        // the observations are on the scale the series came in: not drawn against a standardized series
        if !standardized {
            let points = |keep: &dyn Fn(usize) -> bool| -> Vec<[f64; 2]> {
                (0..v.raw.len()).filter(|&t| v.raw[t].is_finite() && keep(t)).map(|t| [at(t), v.raw[t]]).collect()
            };
            if has_qa {
                let all = egui_plot::PlotPoints::from(points(&|_| true));
                p.points(egui_plot::Points::new("Raw observations", all).color(grey).radius(2.0));
            }
            let name = if has_qa { "Clear observations" } else { "Observations" };
            let clear = egui_plot::PlotPoints::from(points(&|t| v.clear[t]));
            p.points(egui_plot::Points::new(name, clear).color(series).radius(2.5));
        }
        if let Some(a) = analysis {
            let name = if standardized {
                format!("Analysis series: {}", a.label)
            } else {
                format!("Analysis series: {}", a.label.to_lowercase())
            };
            let xs = |i: usize| match &a.days {
                Some(d) => d[i] as f64 * 86400.0,
                None => a.years[i],
            };
            let pts: Vec<[f64; 2]> = (0..a.values.len()).map(|i| [xs(i), a.values[i]]).collect();
            for piece in pts.split(|q| !q[1].is_finite()).filter(|q| !q.is_empty()) {
                p.line(
                    egui_plot::Line::new(name.as_str(), egui_plot::PlotPoints::from(piece.to_vec()))
                        .color(series)
                        .width(1.5),
                );
            }
        }
        for &(slot, r) in &products {
            let style = crate::zeit::FitStyle::of(slot);
            let name = STEPS[slot].1;
            for o in &r.overlays {
                if o.kind == "vlines" {
                    for &xv in &o.x {
                        p.vline(
                            egui_plot::VLine::new(name, info.x_from_decimal_year(xv)).color(style.color).width(1.5),
                        );
                    }
                    continue;
                }
                let pts: Vec<[f64; 2]> =
                    o.x.iter()
                        .zip(&o.y)
                        .map(|(&xv, yv)| [info.x_from_decimal_year(xv), yv.unwrap_or(f64::NAN)])
                        .collect();
                if o.kind == "markers" {
                    let valid: Vec<[f64; 2]> = pts.into_iter().filter(|q| q[1].is_finite()).collect();
                    p.points(
                        egui_plot::Points::new(name, egui_plot::PlotPoints::from(valid))
                            .color(style.color)
                            .radius(3.0)
                            .shape(egui_plot::MarkerShape::Square),
                    );
                } else {
                    for piece in pts.split(|q| !q[1].is_finite()).filter(|q| !q.is_empty()) {
                        p.line(
                            egui_plot::Line::new(name, egui_plot::PlotPoints::from(piece.to_vec()))
                                .color(style.color)
                                .width(o.weight.unwrap_or(2.0) as f32)
                                .style(style.line),
                        );
                    }
                }
            }
        }
    });
    let (n_clear, n) = (v.clear.iter().filter(|&&c| c).count(), v.clear.len());
    ui.weak(if has_qa {
        format!("Pixel ({x}, {y}): {n_clear} of {n} dates are clear (a value the quality band keeps).")
    } else {
        format!("Pixel ({x}, {y}): {n_clear} of {n} dates have a value.")
    });
    if standardized {
        ui.weak("Standardize is on: the series is drawn standardized, the observations (another scale) are not.");
    }
    for (id, r) in &v.chain.steps {
        if let Err(e) = r {
            let label = STEPS.iter().find(|s| s.0 == id).map_or(id.as_str(), |s| s.1);
            ui.weak(format!("{label}: {e}"));
        }
    }
}

impl State {
    /// The chart of the pixel last hovered on the map: what the chain does to it, step by step.
    fn tsa_pixel_chart(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        crate::panels::section(ui, "Pixel");
        if let Some(h) = &self.series.hover {
            self.tsa.pixel.at = Some((info.id, h.x, h.y));
        }
        let Some((_, x, y)) = self.tsa.pixel.at.filter(|a| a.0 == info.id) else {
            ui.weak("Hover the map to see a pixel.");
            return;
        };
        let steps = self.tsa_steps();
        if steps.is_empty() {
            ui.weak("Turn a step on to see what it does to the pixel.");
            return;
        }
        let key = PixelKey {
            cube: info.id,
            x,
            y,
            series: format!("{} qa {} {}", info.selection_text(), info.sel.qa_band, info.sel.qa_rule.id()),
            steps: steps
                .iter()
                .map(|s| (s.tool.id.clone(), serde_json::to_string(&s.params).unwrap_or_default()))
                .collect(),
            version: self.zeit.version
                + steps.iter().map(|s| self.zeit.tools.get(&s.tool.id).map_or(0, |t| t.version)).sum::<u64>(),
        };
        let map_t = self.display.t.min(info.t().saturating_sub(1));
        let ctx = ui.ctx().clone();
        let work_info = info.clone();
        self.tsa.pixel.update(&key, &ctx, move || pixel_view(&work_info, &steps, x, y, map_t));
        match &self.tsa.pixel.shown {
            Some((_, Ok(v))) => pixel_plot(ui, v, x, y),
            Some((_, Err(e))) => {
                ui.colored_label(crate::theme::error(), e);
            }
            None => {
                ui.weak("Computing...");
            }
        }
        if self.tsa.pixel.shown.as_ref().is_some_and(|(k, _)| *k != key) {
            ui.weak("Updating...");
        }
    }

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
        // a series chosen for the Classification that was not open: active once it has arrived (after the layer that
        // was active when it opened has been made active again)
        if let Some(p) = self.tsa.pending_use.clone()
            && self.opening.is_none()
            && self.zeit.keep_active.is_none()
            && !self.zeit.series_to_open.iter().any(|(q, _)| *q == p)
        {
            self.tsa.pending_use = None;
            self.tsa_activate(&p, ctx);
        }
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
                self.tsa_pixel_chart(ui, &info);
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
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!on.is_empty(), egui::Button::new("Copy Python"))
                .on_hover_text(
                    "One script that does in Zeit what the chain does on this series and scope: the series read once \
                     from its files, each step's call (the steps after Interpolate and Standardize reading the \
                     analysis series), every result written as a GeoTIFF. Copied to the clipboard.",
                )
                .clicked()
            {
                match self.tsa_python(info) {
                    Ok(code) => {
                        ui.ctx().copy_text(code);
                        self.tsa.copied = Some(Instant::now());
                    }
                    Err(e) => self.error_popup = Some(e),
                }
            }
            if self.tsa.copied.is_some_and(|t| t.elapsed().as_secs_f32() < 3.0) {
                ui.weak("Copied.");
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
        // the last chain: its progress and Cancel, or how it ended and its folder
        let mut open_folder = None;
        let mut finished = false;
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
                    finished = state == JobState::Done;
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
        // a series of the chain to classify
        let series = self.tsa.series_out.lock().unwrap().clone();
        if finished && !series.is_empty() {
            self.tsa.use_series = self.tsa.use_series.min(series.len() - 1);
            ui.add_space(4.0);
            ComboBox::from_id_salt("tsa use series")
                .width(ui.available_width())
                .selected_text(&series[self.tsa.use_series].0)
                .show_ui(ui, |ui| {
                    for (i, (name, path)) in series.iter().enumerate() {
                        ui.selectable_value(&mut self.tsa.use_series, i, name).on_hover_text(path);
                    }
                });
            if ui
                .add(egui::Button::new("Use in Classification").min_size(egui::vec2(ui.available_width(), 0.0)))
                .on_hover_text(
                    "Makes this series the active layer and opens the Classification: it reads the dates of the \
                     active series as its features (the samples' values at each date), so this classifies on the \
                     chain's series (the interpolated, the standardized, the metrics by period, the phenology by \
                     year) instead of the observations.",
                )
                .clicked()
            {
                let path = series[self.tsa.use_series].1.clone();
                self.tsa_use_series(&path, &ui.ctx().clone());
            }
        }
    }

    /// The chain's script for the steps on and the scope now, even before it runs.
    fn tsa_python(&mut self, info: &Arc<CubeInfo>) -> Result<String, String> {
        let steps = self.tsa_steps();
        let (input, name) = self.tsa_chain_input(info);
        let out = self.results_dir().join(format!("{name}_tsa_python"));
        chain_code(&steps, &left_out(&steps, info), &input, &out.to_string_lossy())
    }

    /// Makes the layer that opened `path` the active one, if there is one.
    fn tsa_activate(&mut self, path: &str, ctx: &egui::Context) -> bool {
        let key = crate::provenance::absolute(path);
        let found =
            self.layers.iter().position(|l| l.inputs.len() == 1 && crate::provenance::absolute(&l.inputs[0]) == key);
        if let Some(i) = found
            && i != self.active
        {
            self.set_active(i, ctx);
        }
        found.is_some()
    }

    /// The series `path` the chain wrote, active and the Classification open on its samples' tab: the series' layer
    /// if it is open, else opened as the chain's other series are (it becomes active when it arrives).
    fn tsa_use_series(&mut self, path: &str, ctx: &egui::Context) {
        if !self.tsa_activate(path, ctx) {
            if !self.zeit.series_to_open.iter().any(|(q, _)| q == path) {
                self.zeit.series_to_open.push((path.to_string(), false));
            }
            self.tsa.pending_use = Some(path.to_string());
        }
        (self.ml.open, self.ml.tab) = (true, 0);
    }

    /// Run again from a chain's provenance: its steps on with their parameters and its scope, in the window's
    /// Output tab (on the active series).
    pub(crate) fn tsa_again(&mut self, p: &Provenance) {
        let ids: Vec<&str> = p
            .extra
            .get("steps")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.split_once('_').map_or(s, |x| x.1)).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            self.error_popup = Some("the chain's steps are not in its provenance".into());
            return;
        }
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            self.tsa.on[i] = ids.contains(&id);
        }
        for (id, v) in &p.params {
            if let (Some(tool), Some(m)) = (manifest().tool(id), v.as_object()) {
                *self.zeit.params(tool) = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.zeit.tools.entry(id.clone()).or_default().version += 1;
            }
        }
        self.tsa.scope = if p.scope_index == 2 && self.series.roi_rect.is_none() { 0 } else { p.scope_index.min(2) };
        (self.tsa.open, self.tsa.tab) = (true, 2);
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

    /// What the chain starts from now: the active layer's series, the scope and the roles of its bands. And the
    /// series' name for the folders.
    fn tsa_chain_input(&mut self, info: &Arc<CubeInfo>) -> (ChainInput, String) {
        let layer = self.layers.get(self.active).map(|l| (l.name.clone(), l.inputs.clone()));
        let inputs = layer.as_ref().map(|l| l.1.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(info, &inputs);
        let (window, scope) = self.run_window(self.tsa.scope);
        let input = ChainInput {
            info: info.clone(),
            name: layer.map_or(name.clone(), |l| l.0),
            inputs,
            roles: self.active_roles(),
            window,
            scope: scope.to_string(),
            scope_index: self.tsa.scope,
            map_t: self.display.t.min(info.t().saturating_sub(1)),
            threads: self.processing_threads(),
        };
        (input, name)
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
        let (input, name) = self.tsa_chain_input(&info);
        let (window, scope) = (input.window, input.scope.clone());
        let root = self.results_dir().join(format!("{name}_tsa_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&root) {
            self.error_popup = Some(format!("could not create {}: {e}", root.display()));
            return;
        }
        // the chain's provenance at its folder (where its result layers point): the steps with their parameters and
        // folders, the series, the scope, and its Python; `pump_provenance` completes it from the task's result
        let mut prov = Provenance::start("tsa", JOB_TOOL, "Time series analysis");
        prov.params = steps.iter().map(|s| (s.tool.id.clone(), json!(s.params))).collect();
        prov.scope = scope.clone();
        prov.scope_index = input.scope_index;
        prov.window = Some(window);
        prov.threads = Some(input.threads as i64);
        prov.series = Some(SeriesRecord::of(&info, &input.name, &input.inputs));
        prov.extra.insert(
            "steps".into(),
            json!(steps.iter().enumerate().map(|(k, s)| format!("{:02}_{}", k + 1, s.tool.id)).collect::<Vec<_>>()),
        );
        let python = root.join("python").to_string_lossy().into_owned();
        match chain_code(&steps, &left_out(&steps, &info), &input, &python) {
            Ok(code) if std::fs::write(root.join(SCRIPT), &code).is_ok() => prov.python = SCRIPT.into(),
            Ok(_) => prov.python_note = format!("{SCRIPT} could not be written"),
            Err(e) => prov.python_note = e,
        }
        self.begin_provenance(&root, prov);
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
        // the series it writes, for Use in Classification
        let written = Arc::new(Mutex::new(vec![]));
        self.tsa.series_out = written.clone();
        self.tsa.use_series = 0;
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
                        *written.lock().unwrap() = series_outputs(&chain.result);
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
        // the series it wrote (what the Classification can use): the analysis series and the series products
        let series = series_outputs(&chain.result);
        assert!(series.iter().any(|s| s.0.ends_with(": Interpolated") && s.1.contains("01_interpolate")), "{series:?}");
        assert!(series.iter().any(|s| s.1.contains("04_polar_metrics")), "{series:?}");
        assert!(series.iter().all(|s| Path::new(&s.1).is_file()) && series.len() < chain.result.outputs.len());
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

    const FIVE: [&str; 5] = ["interpolate", "standardize", "temporal_metrics", "fold", "polar_metrics"];

    #[test]
    fn the_chain_script_has_each_steps_call_once_on_the_right_series() {
        let base = std::env::temp_dir().join("janus-tsa-test-script");
        let info = series(&base.join("series"));
        let steps: Vec<ChainStep> = FIVE.iter().map(|id| step(id)).collect();
        let code = chain_code(&steps, &[], &input(&info, [0, 0, W, H]), "OUT").unwrap();
        let count = |s: &str| code.matches(s).count();
        // each call once; Interpolate on the series read, the others on the analysis series it leaves
        assert_eq!(count("zeit.interpolate(series, "), 1, "{code}");
        assert_eq!(count("zeit.standardize(analysis, "), 1, "{code}");
        assert_eq!(count("zeit.temporal_metrics(analysis, "), 2, "{code}");
        assert_eq!(count(", by='year'"), 1, "{code}");
        assert_eq!(count("zeit.polar_metrics(analysis, "), 1, "{code}");
        assert_eq!(count("(series,"), 1, "{code}");
        // the results numbered along, the analysis series chained (Interpolate's, then Standardize's)
        for (k, call) in
            ["interpolate", "standardize", "temporal_metrics", "temporal_metrics", "polar_metrics"].iter().enumerate()
        {
            assert_eq!(count(&format!("result{} = zeit.{call}(", k + 1)), 1, "result{}: {code}", k + 1);
        }
        assert_eq!(count("analysis = result1"), 1, "{code}");
        assert_eq!(count("analysis = result2"), 1, "{code}");
        assert_eq!(count("analysis = result"), 2, "{code}");
        // the series is read once, the files written under the step's name
        assert_eq!(count("layers = ["), 1, "{code}");
        assert!(code.contains("\"result5\": \"05_polar_metrics\""), "{code}");
        assert!(code.contains("names.get(key, key)"), "{code}");
        // a step with two results (the trend folds first), numbered after the ones before it
        let steps: Vec<ChainStep> =
            ["interpolate", "linear_trend", "polar_metrics"].iter().map(|id| step(id)).collect();
        let code = chain_code(&steps, &[], &input(&info, [0, 0, W, H]), "OUT").unwrap();
        assert!(code.contains("result2 = zeit.temporal_metrics(analysis, "), "{code}");
        assert!(code.contains("result3 = zeit.linear_trend(result2, "), "{code}");
        assert!(code.contains("result4 = zeit.polar_metrics(analysis, "), "{code}");
        // a step left out is said so; none left is an error
        let code =
            chain_code(&steps, &[("polar_metrics".into(), "why".into())], &input(&info, [0, 0, W, H]), "OUT").unwrap();
        assert!(code.contains("): left out (why)") && !code.contains("zeit.polar_metrics"), "{code}");
        let all: Vec<(String, String)> = steps.iter().map(|s| (s.tool.id.clone(), String::new())).collect();
        assert!(chain_code(&steps, &all, &input(&info, [0, 0, W, H]), "OUT").is_err());
        // every step on: past ten results, each name assigned once and none mangled (result1 is a prefix of
        // result12)
        let steps: Vec<ChainStep> = STEPS.iter().map(|s| step(s.0)).collect();
        let code = chain_code(&steps, &[], &input(&info, [0, 0, W, H]), "OUT").unwrap();
        let n = (1..100).take_while(|k| code.contains(&format!("result{k} = "))).count();
        assert!(n >= 10, "{n} results: {code}");
        for k in 1..=n {
            assert_eq!(code.matches(&format!("result{k} = ")).count(), 1, "result{k}: {code}");
        }
        assert!(!code.contains(&format!("result{} = ", n + 1)) && !code.contains('\u{1}'), "{code}");
    }

    /// The chain's script run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio and xarray; the test is left
    /// out without it) gives the values `run_chain` writes.
    #[test]
    fn the_chain_script_runs_in_zeit_and_gives_the_chains_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-tsa-test-zeit");
        let info = series(&base.join("series"));
        let steps: Vec<ChainStep> = FIVE.iter().map(|id| step(id)).collect();
        let window = [1, 1, W - 1, H - 1];
        let input = input(&info, window);
        let chain = run_chain(&steps, &input, &base.join("chain"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let out = base.join("python");
        let _ = std::fs::remove_dir_all(&out);
        let code = chain_code(&steps, &[], &input, &out.to_string_lossy()).unwrap();
        let script = base.join("chain.py");
        std::fs::write(&script, code).unwrap();
        // the PATH as an activated conda environment has it, without the GDAL builds that shadow rasterio's
        let root = Path::new(&python).parent().unwrap().to_path_buf();
        let mut path: Vec<String> = ["", "Library\\mingw-w64\\bin", "Library\\usr\\bin", "Library\\bin", "Scripts"]
            .iter()
            .map(|d| root.join(d).to_string_lossy().into_owned())
            .collect();
        path.extend(
            std::env::var("PATH")
                .unwrap_or_default()
                .split(';')
                .filter(|p| {
                    let p = p.to_lowercase();
                    !["program files\\gdal", "strawberry", "git", "\\target\\"].iter().any(|x| p.contains(x))
                })
                .map(str::to_string),
        );
        let run = std::process::Command::new(python)
            .arg(&script)
            .env_clear()
            .envs(["SystemRoot", "TEMP", "TMP"].iter().filter_map(|k| Some((*k, std::env::var(k).ok()?))))
            .env("PATH", path.join(";"))
            .env("PYTHONNOUSERSITE", "1")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        // the script's bands by step (its files start with the step's folder name)
        let files: Vec<(String, Vec<Vec<f32>>)> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), read(&p.to_string_lossy()).0))
            .collect();
        let close = |a: f32, b: f32| (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-4 * b.abs().max(1.0);
        let (mut bands, mut values, mut worst) = (0, 0, 0f32);
        for o in &chain.result.outputs {
            let path = o["path"].as_str().unwrap();
            let step = Path::new(path).parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
            let (want, _) = read(path);
            for w in &want {
                // a band of the script's files of the step with the chain's values
                let hit = files
                    .iter()
                    .filter(|f| f.0.starts_with(&format!("{step}_")))
                    .flat_map(|f| &f.1)
                    .find(|g| g.len() == w.len() && g.iter().zip(w).all(|(&a, &b)| close(a, b)));
                let Some(g) = hit else {
                    panic!("no band of {step} in the script has the values of {} ({path})", o["name"])
                };
                worst = g
                    .iter()
                    .zip(w)
                    .filter(|(a, b)| a.is_finite() && b.is_finite())
                    .fold(worst, |m, (a, b)| m.max((a - b).abs()));
                values += w.len();
                bands += 1;
            }
        }
        println!(
            "{} outputs, {bands} bands, {values} values compared, the largest difference {worst:e}",
            chain.result.outputs.len()
        );
        assert!(bands >= 10 && values > 500);
    }

    /// The outputs of a chain written by the step in `folder` (such as "/01_interpolate/").
    fn outputs_of(chain: &Chain, folder: &str) -> Vec<String> {
        let paths = chain.result.outputs.iter().map(|o| o["path"].as_str().unwrap().replace('\\', "/"));
        paths.filter(|p| p.contains(folder)).collect()
    }

    /// The analysis series of a pixel against the raster series (a GeoTIFF, [dates][pixels]) at that pixel: the dates
    /// by their labels, the values bit for bit (the raster holds float32).
    fn assert_analysis_is_raster(a: &PixelAnalysis, path: &str, at: (usize, usize)) {
        let (bands, labels) = read(path);
        assert_eq!(a.values.len(), bands.len(), "{path}: dates");
        let days = a.days.as_ref().expect("dates");
        assert_eq!(a.years, days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect::<Vec<_>>());
        for (k, band) in bands.iter().enumerate() {
            let (y, m, d) = janus_zeit::numeric::civil(days[k]);
            assert_eq!(labels[k], format!("{y:04}-{m:02}-{d:02}"), "{path}: date {k}");
            let want = band[at.1 * W + at.0];
            let got = a.values[k] as f32;
            assert!(
                got.to_bits() == want.to_bits() || got.is_nan() && want.is_nan(),
                "{path} {at:?} date {k}: {got} {want}"
            );
        }
    }

    /// The rows' value of a metric (a number to 6 significant digits) against the raster's float32 at a pixel.
    fn assert_row_is_raster(row: &[String; 2], path: &str, at: (usize, usize)) {
        let (bands, _) = read(path);
        assert_eq!(bands.len(), 1, "{path}");
        let want = f64::from(bands[0][at.1 * W + at.0]);
        let got: f64 = row[1].parse().unwrap();
        assert!(
            got.is_nan() && want.is_nan() || (got - want).abs() <= 6e-6 * want.abs().max(1e-12),
            "{path} {at:?} {}: {got} {want}",
            row[0]
        );
    }

    #[test]
    fn the_pixel_chain_is_the_raster_chains_at_that_pixel() {
        let base = std::env::temp_dir().join("janus-tsa-test-pixel");
        let info = series(&base.join("series"));
        let steps = [step("interpolate"), step("temporal_metrics"), step("fold")];
        let chain =
            run_chain(&steps, &input(&info, [0, 0, W, H]), &base.join("chain"), &|_, _| {}, &AtomicBool::new(false))
                .unwrap();
        let interpolated = outputs_of(&chain, "/01_interpolate/");
        let metrics = outputs_of(&chain, "/02_temporal_metrics/");
        let folds = outputs_of(&chain, "/03_fold/");
        assert_eq!(interpolated.len(), 1);
        assert!(!metrics.is_empty() && !folds.is_empty());
        let mut compared = 0;
        // the pixel missing at every date, and others of different curves
        for at in [(0, 0), (3, 2), (7, 5), (1, 4), (5, 0)] {
            let v = pixel_view(&info, &steps, at.0, at.1, info.t() - 1).unwrap();
            let ids: Vec<&str> = v.chain.steps.iter().map(|s| s.0.as_str()).collect();
            assert_eq!(ids, ["interpolate", "temporal_metrics", "fold"]);
            // the grid's dates and values: those of the raster's series
            let a = v.chain.analysis.as_ref().expect("an analysis series");
            assert!(!a.standardized);
            assert_analysis_is_raster(a, &interpolated[0], at);
            // the temporal metrics: one row a metric after the count of valid observations
            let tm = v.chain.steps[1].1.as_ref().unwrap();
            assert_eq!(tm.rows.len(), metrics.len() + 1);
            for (row, path) in tm.rows[1..].iter().zip(&metrics) {
                assert_row_is_raster(row, path, at);
                compared += 1;
            }
            // the fold by year: a line a metric, a value a year (the raster's band)
            let fold = v.chain.steps[2].1.as_ref().unwrap();
            assert_eq!(fold.overlays.len(), folds.len());
            for (o, path) in fold.overlays.iter().zip(&folds) {
                let (bands, _) = read(path);
                assert_eq!(o.y.len(), bands.len(), "{path}");
                for (yv, band) in o.y.iter().zip(&bands) {
                    let got = yv.map_or(f32::NAN, |v| v as f32);
                    let want = band[at.1 * W + at.0];
                    assert!(got.to_bits() == want.to_bits() || got.is_nan() && want.is_nan(), "{path} {at:?}");
                    compared += 1;
                }
            }
        }
        assert!(compared > 40, "{compared} values compared");
    }

    #[test]
    fn the_pixel_chain_reads_the_standardized_series_after_standardize() {
        let base = std::env::temp_dir().join("janus-tsa-test-pixel-std");
        let info = series(&base.join("series"));
        let steps = [step("interpolate"), step("standardize"), step("temporal_metrics")];
        let chain =
            run_chain(&steps, &input(&info, [0, 0, W, H]), &base.join("chain"), &|_, _| {}, &AtomicBool::new(false))
                .unwrap();
        let standardized = outputs_of(&chain, "/02_standardize/");
        let metrics = outputs_of(&chain, "/03_temporal_metrics/");
        assert_eq!(standardized.len(), 1);
        for at in [(3, 2), (6, 1), (2, 5)] {
            let v = pixel_view(&info, &steps, at.0, at.1, info.t() - 1).unwrap();
            let a = v.chain.analysis.as_ref().expect("an analysis series");
            assert!(
                a.standardized && a.label.contains("Interpolated") && a.label.contains("Standardized"),
                "{}",
                a.label
            );
            assert_analysis_is_raster(a, &standardized[0], at);
            let tm = v.chain.steps[2].1.as_ref().unwrap();
            for (row, path) in tm.rows[1..].iter().zip(&metrics) {
                assert_row_is_raster(row, path, at);
            }
        }
    }

    #[test]
    fn without_interpolate_the_pixel_chain_reads_the_raw_series() {
        let base = std::env::temp_dir().join("janus-tsa-test-pixel-raw");
        let info = series(&base.join("series"));
        let steps = [step("fold"), step("temporal_metrics")];
        let v = pixel_view(&info, &steps, 3, 2, info.t() - 1).unwrap();
        assert!(v.chain.analysis.is_none());
        // no quality band: every date with a value is clear, and the input is the series itself
        assert_eq!(v.clear, v.raw.iter().map(|x| x.is_finite()).collect::<Vec<_>>());
        assert!(v.clear.iter().any(|&c| !c) && v.clear.iter().any(|&c| c), "some dates are missing");
        let bits = |a: &[f64]| a.iter().map(|x| if x.is_finite() { x.to_bits() } else { 0 }).collect::<Vec<_>>();
        assert_eq!(bits(&v.input.values), bits(&v.raw));
        assert_eq!(v.input.days.as_ref().unwrap().len(), info.t());
        for (id, r) in &v.chain.steps {
            let want = run_pixel(id, &Params::with_defaults(id, &BTreeMap::new()).unwrap(), &v.input).unwrap();
            assert_eq!(r.as_ref().unwrap(), &want, "{id}");
        }
        // a step the series cannot take is left out with why, the others run
        let steps = [step("polar_metrics"), step("temporal_metrics")];
        let v = pixel_view(&info, &steps, 3, 2, info.t() - 1).unwrap();
        assert!(v.chain.steps[0].1.as_ref().err().unwrap().contains("not evenly spaced"));
        assert!(v.chain.steps[1].1.is_ok());
    }

    #[test]
    fn a_pixel_outside_the_series_or_without_steps_is_an_error() {
        let base = std::env::temp_dir().join("janus-tsa-test-pixel-out");
        let info = series(&base.join("series"));
        assert!(pixel_view(&info, &[step("fold")], W, 0, 0).is_err());
        assert!(pixel_view(&info, &[], 0, 0, 0).is_err());
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
