//! `--selftest-ui` stages 300-309: the Time series analysis window (tsa.rs), on the series with dates and a quality
//! band (C: ten yearly dates, the Fmask band, irregular in days). 300 opens the window; 301-303 draw its three tabs
//! (the Steps tab's "To look at" against `advice()`, the polar phenology refused on the irregular dates and allowed
//! once Interpolate is on); 304 hovers a pixel and waits for the pixel chart (the analysis series on Interpolate's
//! grid); 305-306 run the default chain (Interpolate, Temporal metrics, Fold) over the whole image as one task and
//! check the root folder (provenance.json, rerun.py), a folder per step with its files, the maps (result layers
//! named by their step) and the series (opened as layers); 307 Copy Python's script; 308 Use in Classification;
//! 309 everything closed and restored.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use janus_zeit::manifest::manifest;
use janus_zeit::pixel::Params;

use super::UiTest;
use crate::app::State;
use crate::provenance::{Provenance, SCRIPT};
use crate::tsa::{Advice, STEPS, TsaUi, advice, problem};
use crate::zeit::JobState;

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct TsaTest {
    /// the results folder of the run (and the one the app had before)
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    /// the layers, the active one and the Classification's tab before the test, whether it opened C itself
    layers: usize,
    active: usize,
    /// the layer the chain runs on
    target: usize,
    ml: (bool, usize),
    tasks: bool,
    opened: bool,
    /// the analysis series' length at the pixel (what Interpolate's grid gives), the chain's folder
    len: usize,
    root: PathBuf,
    /// the heights of the window on its three tabs
    heights: [f32; 3],
    /// stage 302's phase
    phase: u8,
}

/// The window's height now (None: not drawn).
fn window_height(ctx: &egui::Context) -> Option<f32> {
    ctx.memory(|m| m.area_rect(egui::Id::new("tsa window"))).map(|r| r.height())
}

impl State {
    /// A loaded series with dates and a quality band: what the chain's clear-sky steps read.
    fn tsa_test_layer(&self) -> Option<usize> {
        (0..self.layers.len()).find(|&k| {
            crate::ui_test::loaded(self.layer_session(k))
                && self.layer_session(k).is_some_and(|s| {
                    let i = &s.info;
                    i.time_is_date && i.t() >= 4 && !i.spectral && i.sel.qa_band > 0
                })
        })
    }

    /// What the window's "To look at" shows for the steps that are on: `advice()` with the parameters the steps hold.
    fn tsa_test_advice(&mut self, info: &janus_cube::CubeInfo) -> Vec<Advice> {
        let mut held: BTreeMap<String, BTreeMap<String, serde_json::Value>> = BTreeMap::new();
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            if let Some(tool) = manifest().tool(id).filter(|_| self.tsa.on[i]) {
                held.insert(id.to_string(), self.zeit.params(tool).clone());
            }
        }
        advice(info, &self.tsa.on, &|id| held.get(id).cloned().unwrap_or_default())
    }

    pub(super) fn test_tsa(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: Instant,
        ctx: &egui::Context,
    ) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(Instant::now());
            st.frames = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let on_default = TsaUi::default().on;
        let step_ids = ["interpolate", "temporal_metrics", "fold"];
        match stage {
            300 => {
                if st.tsa.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.tsa = TsaTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-tsa-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        ml: (self.ml.open, self.ml.tab),
                        tasks: self.zeit.show_tasks,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.tsa.dir);
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                let Some(k) = self.tsa_test_layer() else {
                    // C, opened as a layer of its own (once)
                    if st.tsa.opened {
                        return None;
                    }
                    let Some(c) = input(1) else {
                        println!("    no series C: the Time series analysis stages are left out");
                        let _ = std::fs::remove_dir_all(&st.tsa.dir);
                        st.tsa = Default::default();
                        st.stage = 299;
                        return None;
                    };
                    st.tsa.opened = true;
                    self.open_inputs(vec![c], true, ctx);
                    return None;
                };
                if k != self.active || self.session.is_none() {
                    self.set_active(k, ctx);
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                st.tsa.target = k;
                self.tsa = Default::default();
                self.tsa.open = true;
                self.zeit.results_dir = Some(st.tsa.dir.clone());
                println!(
                    "    series {}: {} dates ({} to {}), quality band {}",
                    self.layers[k].name,
                    info.t(),
                    info.layers[0].label,
                    info.layers[info.t() - 1].label,
                    info.sel.qa_band
                );
                next(st, "Tools > Time series analysis: the window opened on the Input tab");
                None
            }
            301 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let Some(h) = window_height(ctx).filter(|&h| h > 60.0) else {
                    return fail("the Time series analysis window should be drawn (the Input tab)");
                };
                st.tsa.heights[0] = h;
                // the Input tab reads the dates of the series it runs on
                if !info.time_is_date || info.layers.iter().any(|l| l.label.is_empty()) {
                    return fail("the series' dates should have labels for the Input tab");
                }
                self.tsa.tab = 1;
                next(
                    st,
                    &format!(
                        "Input tab drawn ({h:.0} pt): {} dates, {} to {}",
                        info.t(),
                        info.layers[0].label,
                        info.layers[info.t() - 1].label
                    ),
                );
                None
            }
            302 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                match st.tsa.phase {
                    0 => {
                        let Some(h) = window_height(ctx).filter(|&h| h > 60.0) else {
                            return fail("the Time series analysis window should be drawn (the Steps tab)");
                        };
                        st.tsa.heights[1] = h;
                        if STEPS.len() != 8 || STEPS.iter().any(|s| manifest().tool(s.0).is_none()) {
                            return fail("the window should list the eight steps, each a tool of the manifest");
                        }
                        // "To look at" for the default steps: yearly dates, so a year with one date, and a
                        // quality band, so nothing about its lack
                        let look = self.tsa_test_advice(&info);
                        for a in &look {
                            println!("    to look at (level {}): {}", a.level, a.text);
                        }
                        let few = look.iter().any(|a| a.level == 1 && a.text.contains("has 1 date"));
                        let no_qa = look.iter().any(|a| a.text.contains("no quality band"));
                        let stops = look.iter().any(|a| a.level == 2);
                        if !few || no_qa || stops || look.iter().filter(|a| a.level > 0).count() == 0 {
                            return fail(
                                "to look at: the yearly series should warn of the years with one date, and neither \
                                 stop a step nor miss a quality band",
                            );
                        }
                        // the polar phenology on the irregular dates: refused, with why
                        let polar = manifest().tool("polar_metrics")?;
                        let dates_regular = problem(polar, &info, false).is_none();
                        self.tsa.on = [false; 8];
                        self.tsa.on[7] = true;
                        let look = self.tsa_test_advice(&info);
                        let blocked = self.tsa_blocked(&info);
                        if dates_regular {
                            println!("    the series' dates are regular: the polar phenology's refusal is not checked");
                        } else {
                            let why = problem(polar, &info, false).unwrap_or_default();
                            println!("    Polar phenology without Interpolate: {why}");
                            if !look.iter().any(|a| a.level == 2 && a.text.starts_with("Polar phenology: "))
                                || blocked.is_none()
                                || !why.contains("evenly spaced")
                            {
                                return fail(
                                    "the polar phenology on irregular dates should be refused with its reason",
                                );
                            }
                        }
                        st.tsa.phase = 1;
                        st.frames = 0;
                    }
                    _ => {
                        // drawn with the polar phenology on (its reason in the window), then Interpolate on
                        if window_height(ctx).is_none() {
                            return fail("the window should stay drawn with the polar phenology on");
                        }
                        self.tsa.on[1] = true;
                        let look = self.tsa_test_advice(&info);
                        let blocked = self.tsa_blocked(&info);
                        let polar = manifest().tool("polar_metrics")?;
                        if look.iter().any(|a| a.level == 2)
                            || blocked.is_some()
                            || problem(polar, &info, true).is_some()
                        {
                            return fail("with Interpolate on, the polar phenology should be allowed");
                        }
                        self.tsa.on = on_default;
                        self.tsa.tab = 2;
                        next(
                            st,
                            "Steps tab drawn: the eight steps, To look at as advice() says; the polar phenology \
                             refused on the irregular dates, allowed with Interpolate on",
                        );
                    }
                }
                None
            }
            303 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let Some(h) = window_height(ctx).filter(|&h| h > 60.0) else {
                    return fail("the Time series analysis window should be drawn (the Output tab)");
                };
                st.tsa.heights[2] = h;
                if let Some(why) = self.tsa_blocked(&info) {
                    return fail(&format!("the default chain should be able to run: {why}"));
                }
                // the Steps tab again, with a pixel: the chart's place
                self.tsa.tab = 1;
                let (x, y) = (info.width * 3 / 4, info.height / 2);
                self.set_hover(Some((x, y)));
                self.rest_hover();
                let [a, b, c] = st.tsa.heights;
                next(
                    st,
                    &format!(
                        "Output tab drawn; the window's heights on its tabs {a:.0}, {b:.0}, {c:.0} pt, none growing"
                    ),
                );
                None
            }
            304 => {
                let info = self.session.as_ref()?.info.clone();
                let (x, y) = (info.width * 3 / 4, info.height / 2);
                if self.tsa.pixel.at != Some((info.id, x, y)) {
                    return if waited > 10.0 { fail("the Steps tab should take the hovered pixel") } else { None };
                }
                let Some((_, shown)) = &self.tsa.pixel.shown else {
                    return if waited > 60.0 { fail("the pixel chart should be computed") } else { None };
                };
                let v = match shown {
                    Ok(v) => v,
                    Err(e) => return fail(&format!("the pixel chart: {e}")),
                };
                let ids: Vec<&str> = v.chain.steps.iter().map(|(id, _)| id.as_str()).collect();
                if ids != step_ids {
                    return fail(&format!("the pixel chart's steps should be the chain's three: {ids:?}"));
                }
                if let Some((id, Err(e))) = v.chain.steps.iter().find(|(_, r)| r.is_err()) {
                    return fail(&format!("the pixel chart's {id} step failed: {e}"));
                }
                let Some(analysis) = &v.chain.analysis else {
                    return fail("the pixel chart should have the analysis series");
                };
                // its length is Interpolate's grid, as the raster run makes it
                let tool = manifest().tool("interpolate")?;
                let params = Params::with_defaults("interpolate", self.zeit.params(tool))?;
                let grid = match janus_zeit::interpolate_days(&params, &v.input) {
                    Ok(g) => g,
                    Err(e) => return fail(&format!("Interpolate's grid: {e}")),
                };
                let clear = v.clear.iter().filter(|&&c| c).count();
                println!(
                    "    pixel ({x}, {y}): {} of {} dates clear, analysis series of {} values on a grid of {} dates \
                     ({} finite)",
                    clear,
                    v.clear.len(),
                    analysis.values.len(),
                    grid.len(),
                    analysis.values.iter().filter(|q| q.is_finite()).count()
                );
                if analysis.values.len() != grid.len() || analysis.years.len() != grid.len() || grid.len() <= info.t() {
                    return fail(
                        "the analysis series should have Interpolate's grid's length (more dates than the series)",
                    );
                }
                if analysis.days.as_deref() != Some(&grid[..]) {
                    return fail("the analysis series should be on Interpolate's dates");
                }
                if v.clear.len() != info.t()
                    || clear == 0
                    || (info.sel.qa_rule != janus_cube::QaRule::None && clear == v.clear.len())
                {
                    return fail(
                        "the pixel in the cloudy half should have some of its dates masked by the quality band",
                    );
                }
                if !analysis.values.iter().any(|q| q.is_finite()) {
                    return fail("the analysis series should have values");
                }
                st.tsa.len = grid.len();
                next(st, "pixel chart: the analysis series on Interpolate's grid, the three steps on the pixel");
                None
            }
            305 => {
                self.tsa.on = on_default;
                self.tsa.scope = 0;
                self.tsa.tab = 2;
                let before = self.zeit.jobs.len();
                self.tsa_run(ctx);
                if self.zeit.jobs.len() != before + 1 || self.zeit.jobs.last().is_none_or(|j| j.tool != "tsa") {
                    return fail(&format!("the Run should start the chain as a task: {:?}", self.error_popup));
                }
                next(st, "Run: the default chain (Interpolate, Temporal metrics, Fold) as one task (Tools > Tasks)");
                None
            }
            306 => {
                let Some(job) = self.zeit.jobs.iter().rev().find(|j| j.tool == "tsa") else {
                    return fail("the chain's task should be in Tools > Tasks");
                };
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                match state {
                    JobState::Failed | JobState::Cancelled => {
                        return fail(&format!("the chain should finish: {error}"));
                    }
                    JobState::Running => return None,
                    JobState::Done => {}
                }
                let series = self.tsa.series_out.lock().ok()?.clone();
                let key = |p: &str| crate::provenance::absolute(p);
                let layer_of = |s: &State, p: &str| {
                    s.layers.iter().position(|l| l.inputs.len() == 1 && key(&l.inputs[0]) == key(p))
                };
                if !job.loaded
                    || self.opening.is_some()
                    || !self.layer_jobs.queue.is_empty()
                    || self.zeit.loads_pending()
                    || series.len() != 2
                    || series.iter().any(|(_, p)| layer_of(self, p).is_none())
                {
                    return if waited > 90.0 { fail("the chain's maps and series should arrive") } else { None };
                }
                let root = PathBuf::from(&job.output_dir);
                let p = match Provenance::read(&root) {
                    Ok(p) => p,
                    Err(e) => return fail(&e),
                };
                if p.state != "done" {
                    return if waited > 30.0 {
                        fail(&format!("the root provenance should be done, it is {:?}", p.state))
                    } else {
                        None
                    };
                }
                let names: Vec<String> =
                    step_ids.iter().enumerate().map(|(k, id)| format!("{:02}_{id}", k + 1)).collect();
                let listed: Vec<&str> = p
                    .extra
                    .get("steps")
                    .and_then(|v| v.as_array())
                    .map_or(vec![], |a| a.iter().filter_map(|s| s.as_str()).collect());
                if p.kind != "tsa" || p.python != SCRIPT || listed != names || p.outputs.is_empty() {
                    return fail(&format!(
                        "the root provenance should be the chain's with its steps and outputs: {p:?}"
                    ));
                }
                if !root.join(SCRIPT).is_file() || !root.join("job.log").is_file() {
                    return fail("the chain's folder should have rerun.py and job.log");
                }
                // an output is named by its step's tool (its name in the manifest)
                let tool_name = |id: &str| manifest().tool(id).map_or(String::new(), |t| t.name.clone());
                let labels: Vec<String> = step_ids.iter().map(|id| tool_name(id)).collect();
                if let Some(o) =
                    p.outputs.iter().find(|o| !labels.iter().any(|l| o.name.starts_with(&format!("{l}: "))))
                {
                    return fail(&format!("an output should be named by its step: {}", o.name));
                }
                // a folder a step: its files, its provenance done with outputs that exist
                let mut files = 0;
                for name in &names {
                    let dir = root.join(name);
                    for f in ["job.json", "job.log", crate::provenance::FILE, SCRIPT] {
                        if !dir.join(f).is_file() {
                            return fail(&format!("the step folder {name} should have {f}"));
                        }
                        files += 1;
                    }
                    let sp = match Provenance::read(&dir) {
                        Ok(p) => p,
                        Err(e) => return fail(&e),
                    };
                    if sp.state != "done"
                        || sp.outputs.is_empty()
                        || sp.outputs.iter().any(|o| !std::path::Path::new(&o.path).exists())
                    {
                        return fail(&format!("the step {name}: its provenance should be done, its outputs on disk"));
                    }
                }
                // the maps (Temporal metrics) as result layers named by their step; the series as layers
                let tag = job.output_dir.clone();
                let maps: Vec<&str> =
                    self.zeit.results.iter().filter(|r| r.run == tag).map(|r| r.name.as_str()).collect();
                if maps.is_empty()
                    || maps.iter().any(|n| !n.starts_with(&format!("{}: ", tool_name("temporal_metrics"))))
                {
                    return fail(&format!("the maps should be result layers named by their step: {maps:?}"));
                }
                if self.zeit.results.iter().any(|r| r.run == tag && r.data.is_empty()) {
                    return fail("the maps should be loaded");
                }
                let (interp, fold) = (&series[0], &series[1]);
                if !interp.0.starts_with(&tool_name("interpolate")) || !fold.0.starts_with(&tool_name("fold")) {
                    return fail(&format!("the series written should be Interpolate's and Fold's: {series:?}"));
                }
                let ik = layer_of(self, &interp.1)?;
                let it = self.layer_session(ik)?.info.t();
                if it != st.tsa.len {
                    return fail(&format!(
                        "the interpolated series has {it} dates, the pixel chart's analysis series {}",
                        st.tsa.len
                    ));
                }
                if self.active != st.tsa.target {
                    return fail("the series opened by the chain should not take the active layer");
                }
                st.tsa.root = root;
                println!(
                    "    {} folders, {files} files; {} maps ({}), series {} ({} dates) and {}",
                    names.len(),
                    maps.len(),
                    maps.join(", "),
                    interp.0,
                    it,
                    fold.0
                );
                next(
                    st,
                    "the chain's folder: provenance.json (done), rerun.py, a folder a step, maps named by their step, series opened",
                );
                None
            }
            307 => {
                let info = self.session.as_ref()?.info.clone();
                if self.active != st.tsa.target {
                    return fail("the series the chain ran on should still be active");
                }
                let code = match self.tsa_python(&info) {
                    Ok(c) => c,
                    Err(e) => return fail(&format!("Copy Python: {e}")),
                };
                // long calls are broken over lines: read them as one
                let flat = code.replace("(\n    ", "(");
                let count = |s: &str| flat.matches(s).count();
                if count("interpolate_result = zeit.interpolate(series") != 1
                    || count("zeit.temporal_metrics(analysis") != 2
                    || count("analysis = interpolate_result") != 1
                    || count("by='year'") != 1
                {
                    return fail(&format!("Copy Python should have each step's call, on the right series:\n{code}"));
                }
                // the folder's own script has the same calls
                let root_script = std::fs::read_to_string(st.tsa.root.join(SCRIPT)).unwrap_or_default();
                let calls: Vec<&str> =
                    code.lines().filter(|l| l.contains(" = zeit.") && !l.contains("load_raster")).collect();
                if calls.len() != 3 || calls.iter().any(|l| !root_script.contains(l)) {
                    return fail("the chain's rerun.py should hold the calls Copy Python gives");
                }
                println!(
                    "    Copy Python: {} lines, {} calls, the same as the folder's rerun.py",
                    code.lines().count(),
                    calls.len()
                );
                next(st, "Copy Python: the script of the steps on, each step's call");
                None
            }
            308 => {
                if st.frames == 0 {
                    let series = self.tsa.series_out.lock().ok()?.clone();
                    let path = series.first()?.1.clone();
                    self.ml.open = false;
                    self.tsa_use_series(&path, ctx);
                    st.frames = 1;
                    return None;
                }
                let series = self.tsa.series_out.lock().ok()?.clone();
                let path = crate::provenance::absolute(&series.first()?.1);
                let active = self.layers.get(self.active)?;
                let is_it = active.inputs.len() == 1 && crate::provenance::absolute(&active.inputs[0]) == path;
                if !is_it || !self.ml.open || self.tsa.pending_use.is_some() {
                    return if waited > 30.0 {
                        fail("Use in Classification should make the series active and open the Classification")
                    } else {
                        None
                    };
                }
                println!("    active layer {}, the Classification open", active.name);
                next(st, "Use in Classification: the interpolated series active, the Classification open");
                None
            }
            309 => {
                // everything closed, restored as the stages after expect it
                self.tsa = Default::default();
                self.ml.open = st.tsa.ml.0;
                self.ml.tab = st.tsa.ml.1;
                self.zeit.show_tasks = st.tsa.tasks;
                let tag = st.tsa.root.to_string_lossy().into_owned();
                for k in (st.tsa.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                self.zeit.results.retain(|r| r.run != tag);
                self.zeit.jobs.retain(|j| j.output_dir != tag);
                self.zeit.results_dir = st.tsa.results_dir.take();
                if st.tsa.active < self.layers.len() && st.tsa.active != self.active {
                    self.set_active(st.tsa.active, ctx);
                }
                self.series.hover = None;
                let _ = std::fs::remove_dir_all(&st.tsa.dir);
                st.tsa = Default::default();
                next(st, "Time series analysis: the window closed, the layers and results removed");
                st.stage = 299;
                None
            }
            _ => None,
        }
    }
}
