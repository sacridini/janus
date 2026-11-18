//! `--selftest-ui` stages 310-319: the Analysis-ready data window (ard.rs), on Zeit's Landsat series (the `landsat`
//! folder beside A: monthly Blue..SWIR2 and Fmask, no angle bands; the stages are left out without it). 310 opens the
//! window; 311-313 draw its three tabs (Input: the dates' sensors and the bands by role; Steps: the four steps and "To
//! look at" against `advice()`, NBAR left out for want of angle bands; Output) and start the chain with the
//! adjustment and Tasseled Cap on a small ROI; 314 checks its folder (provenance.json done, rerun.py, a folder a step,
//! the harmonized series opened as one layer and the series of each band not) and runs it again from its provenance;
//! 315 Copy Python's script; 316 the preview on the ROI with the swipe on; 317 the preview off (its layers and folder
//! gone); 318 a preview turned off while its harmonized series is being opened (no layer is left); 319 Use in Time
//! series analysis, then everything closed and restored.

use std::path::{Path, PathBuf};
use std::time::Instant;

use janus_zeit::manifest::manifest;

use super::UiTest;
use crate::app::State;
use crate::ard::{ArdUi, HARMONIZED_NAME, JOB_TOOL, Look, STEPS, advice, date_facts, left_out};
use crate::provenance::{Provenance, SCRIPT};
use crate::zeit::JobState;

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct ArdTest {
    /// the results folder of the run (and the one the app had before)
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    /// the layers, the active one, the ROI, the swipe, the Time series analysis window and the tasks before the test
    layers: usize,
    active: usize,
    roi: Option<[usize; 4]>,
    swipe: bool,
    tsa: (bool, usize),
    tasks: bool,
    /// the series opened by the test itself
    opened: bool,
    /// the layer the chain runs on, and the layers there were once it was started
    target: usize,
    before: usize,
    /// the ROI the stages draw (the series' own layers opening clear it: the ROI belongs to the grid of the active
    /// layer, which the chain's series take for a moment), the chain's folder and the preview's
    rect: [i64; 4],
    root: PathBuf,
    preview: String,
    /// a stage's phase
    phase: u8,
}

/// The window's height now (None: not drawn).
fn window_height(ctx: &egui::Context) -> Option<f32> {
    ctx.memory(|m| m.area_rect(egui::Id::new("ard window"))).map(|r| r.height())
}

/// The path as the layers' inputs compare it.
fn key(p: &str) -> String {
    crate::provenance::absolute(p)
}

impl State {
    /// A loaded series with dates, the six reflective bands and a quality band: what the chain reads.
    fn ard_test_layer(&self) -> Option<usize> {
        (0..self.layers.len()).find(|&k| {
            crate::ui_test::loaded(self.layer_session(k))
                && self.layer_session(k).is_some_and(|s| {
                    let i = &s.info;
                    i.time_is_date && i.t() >= 12 && !i.spectral && i.bands_per_date >= 7 && i.sel.qa_band > 0
                })
        })
    }

    /// The layers that opened a file under `dir`.
    fn ard_test_layers_under(&self, dir: &str) -> usize {
        let dir = PathBuf::from(dir);
        self.layers.iter().filter(|l| l.inputs.len() == 1 && Path::new(&l.inputs[0]).starts_with(&dir)).count()
    }

    pub(super) fn test_ard(
        &mut self,
        st: &mut UiTest,
        _input: &dyn Fn(usize) -> Option<String>,
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
            st.ard.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let on_default = ArdUi::default().on;
        let on_test = [true, true, true, false];
        match stage {
            310 => {
                if st.ard.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.ard = ArdTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-ard-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        roi: self.series.roi_rect,
                        swipe: self.swipe.is_some(),
                        tsa: (self.tsa.open, self.tsa.tab),
                        tasks: self.zeit.show_tasks,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.ard.dir);
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                let Some(k) = self.ard_test_layer() else {
                    // Zeit's Landsat series beside A, opened as a layer of its own (once)
                    if st.ard.opened {
                        return None;
                    }
                    let landsat = self
                        .opts
                        .inputs
                        .first()
                        .and_then(|a| Path::new(a).parent().map(|d| d.join("landsat")))
                        .filter(|d| d.is_dir());
                    let Some(landsat) = landsat else {
                        println!("    no landsat folder beside A: the Analysis-ready data stages are left out");
                        let _ = std::fs::remove_dir_all(&st.ard.dir);
                        st.ard = Default::default();
                        st.stage = 330; // the crash notice (330-333, ui_test_crash.rs), then 299
                        return None;
                    };
                    st.ard.opened = true;
                    self.open_inputs(vec![landsat.to_string_lossy().into_owned()], true, ctx);
                    return None;
                };
                if k != self.active || self.session.is_none() {
                    self.set_active(k, ctx);
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                st.ard.target = k;
                self.ard = Default::default();
                self.ard.open = true;
                self.zeit.results_dir = Some(st.ard.dir.clone());
                println!(
                    "    series {}: {} dates ({} to {}), {} bands a date, quality band {}",
                    self.layers[k].name,
                    info.t(),
                    info.layers[0].label,
                    info.layers[info.t() - 1].label,
                    info.bands_per_date,
                    info.sel.qa_band
                );
                next(st, "Tools > Analysis-ready data: the window opened on the Input tab");
                None
            }
            311 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                let Some(h) = window_height(ctx).filter(|&h| h > 60.0) else {
                    return fail("the Analysis-ready data window should be drawn (the Input tab)");
                };
                // the Input tab: the six reflective bands mapped, no angle band, a sensor for every date
                let roles = self.active_roles();
                let six = ["blue", "green", "red", "nir", "swir1", "swir2"];
                if six.iter().any(|r| !roles.contains_key(*r))
                    || ["sza", "saa", "vza", "vaa"].iter().any(|r| roles.contains_key(*r))
                {
                    return fail(&format!(
                        "the series should have the six reflective bands mapped and no angle: {roles:?}"
                    ));
                }
                let sensors = crate::zeit::sensors_for("tasseled_cap", &info);
                if sensors.len() != info.t() || sensors.iter().any(String::is_empty) {
                    return fail("every date of the Landsat series should have a sensor (the file names say it)");
                }
                let mut kinds: Vec<&str> = sensors.iter().map(String::as_str).collect();
                kinds.dedup();
                println!("    Input tab ({h:.0} pt): sensors {kinds:?}, six bands mapped, no angle bands");
                self.ard.tab = 1;
                next(st, "Input tab drawn: the dates' sensors, the bands by role");
                None
            }
            312 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                if window_height(ctx).is_none_or(|h| h < 60.0) {
                    return fail("the Analysis-ready data window should be drawn (the Steps tab)");
                }
                if STEPS.len() != 4 || STEPS.iter().any(|s| manifest().tool(s.0).is_none()) {
                    return fail("the window should list the four steps, each a tool of the manifest");
                }
                if self.ard.on != on_default {
                    return fail("NBAR and the spectral adjustment should be on by default");
                }
                // NBAR needs the angles: no angle band here (nor Sentinel-2 granules), so it is left out and said so
                let steps = self.ard_steps();
                let roles = self.active_roles();
                let out = left_out(&steps, &info, &roles, None);
                let ids: Vec<&str> = out.iter().map(|o| o.0.as_str()).collect();
                if ids != ["nbar"] || !out[0].1.contains("angle") {
                    return fail(&format!("NBAR alone should be left out, for want of the angle bands: {out:?}"));
                }
                if let Some(why) = self.ard_blocked(&info) {
                    return fail(&format!("the chain should still run with the adjustment: {why}"));
                }
                // "To look at" as advice() says it (the window's own call)
                let look = self.ard_advice(&info);
                for a in &look {
                    println!("    to look at (level {}): {}", a.level, a.text);
                }
                let facts = date_facts(&steps, &info);
                let l = Look { info: &info, roles: &roles, coarse: None, facts: &facts, shown_hi: None };
                let want: Vec<(u8, String)> =
                    advice(&steps, &l).into_iter().filter(|a| a.level == 2).map(|a| (a.level, a.text)).collect();
                let got: Vec<(u8, String)> =
                    look.iter().filter(|a| a.level == 2).map(|a| (a.level, a.text.clone())).collect();
                if got != want || got.len() != 1 || got[0].1 != format!("{}: {}.", STEPS[0].1, out[0].1) {
                    return fail(&format!("To look at should be advice()'s NBAR line alone: {got:?} against {want:?}"));
                }
                // Tasseled Cap on too: nothing more to look at (the sensors are known)
                self.ard.on = on_test;
                let look = self.ard_advice(&info);
                if look.iter().filter(|a| a.level == 2).count() != 1 || self.ard_blocked(&info).is_some() {
                    return fail("with Tasseled Cap on, only NBAR should be left out");
                }
                self.ard.tab = 2;
                next(
                    st,
                    "Steps tab drawn: the four steps, To look at as advice() says, NBAR left out, the chain still runs",
                );
                None
            }
            313 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let info = self.session.as_ref()?.info.clone();
                if window_height(ctx).is_none_or(|h| h < 60.0) {
                    return fail("the Analysis-ready data window should be drawn (the Output tab)");
                }
                // a small scope: an ROI of 24 x 18 pixels
                let (w, h) = (info.width as i64, info.height as i64);
                st.ard.rect = [w / 4, h / 4, w / 4 + 24.min(w / 2), h / 4 + 18.min(h / 2)];
                self.set_roi_rect(st.ard.rect);
                self.ard.scope = 2;
                let (window, scope) = self.run_window(2);
                if scope != "ROI" {
                    return fail("the ROI should be the run's scope");
                }
                st.ard.before = self.layers.len();
                let jobs = self.zeit.jobs.len();
                self.ard_run(ctx);
                if self.zeit.jobs.len() != jobs + 1 || self.zeit.jobs.last().is_none_or(|j| j.tool != JOB_TOOL) {
                    return fail(&format!("the Run should start the chain as a task: {:?}", self.error_popup));
                }
                println!("    run on the ROI {window:?} ({} x {} px)", window[2] - window[0], window[3] - window[1]);
                next(st, "Run: the adjustment and Tasseled Cap on the ROI as one task (Tools > Tasks)");
                None
            }
            314 => {
                let Some(job) = self.zeit.jobs.iter().rev().find(|j| j.tool == JOB_TOOL) else {
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
                let series = self.ard.series_out.lock().ok()?.clone();
                let root = PathBuf::from(&job.output_dir);
                let harmonized = root.join("02_spectral_adjust").join("series").to_string_lossy().into_owned();
                let layer_of = |s: &State, p: &str| {
                    s.layers.iter().position(|l| l.inputs.len() == 1 && key(&l.inputs[0]) == key(p))
                };
                if !job.loaded
                    || self.opening.is_some()
                    || !self.layer_jobs.queue.is_empty()
                    || self.zeit.loads_pending()
                    || series.len() != 4
                    || series.iter().any(|(_, p)| layer_of(self, p).is_none())
                {
                    return if waited > 90.0 {
                        fail(&format!("the chain's series should arrive: {series:?}"))
                    } else {
                        None
                    };
                }
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
                let names = ["01_nbar", "02_spectral_adjust", "03_tasseled_cap"];
                let listed: Vec<&str> = p
                    .extra
                    .get("steps")
                    .and_then(|v| v.as_array())
                    .map_or(vec![], |a| a.iter().filter_map(|s| s.as_str()).collect());
                let left = p.extra.get("left_out").and_then(|v| v.as_array()).map_or(0, Vec::len);
                if p.kind != "ard"
                    || p.python != SCRIPT
                    || listed != names
                    || left != 1
                    || p.outputs.is_empty()
                    || p.scope != "ROI"
                    || p.scope_index != 2
                    || p.window.is_none()
                    || p.params.len() != 3
                    || p.series.as_ref().is_none_or(|s| s.dates == 0)
                {
                    return fail(&format!(
                        "the root provenance should be the chain's, with its steps and outputs: {p:?}"
                    ));
                }
                if !root.join(SCRIPT).is_file() || !root.join("job.log").is_file() {
                    return fail("the chain's folder should have rerun.py and job.log");
                }
                // an output is named by its step's tool, but for the harmonized series
                let tool_name = |id: &str| manifest().tool(id).map_or(String::new(), |t| t.name.clone());
                let labels = [tool_name("spectral_adjust"), tool_name("tasseled_cap")];
                if let Some(o) = p.outputs.iter().find(|o| {
                    o.name != HARMONIZED_NAME && !labels.iter().any(|l| o.name.starts_with(&format!("{l}: ")))
                }) {
                    return fail(&format!("an output should be named by its step: {}", o.name));
                }
                // a folder a step that ran (NBAR was left out: none), its provenance done with outputs on disk
                if root.join(names[0]).exists() {
                    return fail("NBAR was left out: its folder should not exist");
                }
                let mut files = 0;
                for name in &names[1..] {
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
                        || sp.outputs.iter().any(|o| !Path::new(&o.path).exists())
                    {
                        return fail(&format!("the step {name}: its provenance should be done, its outputs on disk"));
                    }
                }
                // the harmonized series is one layer, with the dates, ten bands and the quality band; the series of each
                // band are files only, and Tasseled Cap's three are layers
                let Some(hk) = layer_of(self, &harmonized) else {
                    return fail("the harmonized series should be a layer");
                };
                let adjust_dir = root.join("02_spectral_adjust");
                let under = self.ard_test_layers_under(&adjust_dir.to_string_lossy());
                if under != 1 {
                    return fail(&format!(
                        "only the harmonized series of the adjustment's folder should be a layer, {under} are"
                    ));
                }
                if self.layers.len() != st.ard.before + 4 {
                    return fail(&format!(
                        "the chain should add four layers, it added {}",
                        self.layers.len() - st.ard.before
                    ));
                }
                let raw = self.layer_session(st.ard.target)?.info.clone();
                let hi = self.layer_session(hk)?.info.clone();
                if hi.t() != raw.t()
                    || hi.bands_per_date != 11
                    || hi.sel.qa_band != 11
                    || hi.sel.qa_rule != raw.sel.qa_rule
                {
                    return fail(&format!(
                        "the harmonized series should have the series' {} dates, ten bands and its quality band: {} dates, {} bands",
                        raw.t(),
                        hi.t(),
                        hi.bands_per_date
                    ));
                }
                if (hi.width, hi.height) != (24, 18) {
                    return fail(&format!(
                        "the harmonized series should be the ROI's 24 x 18 px, it is {} x {}",
                        hi.width, hi.height
                    ));
                }
                let bands = std::fs::read_dir(&adjust_dir).map_or(0, |d| {
                    d.filter_map(Result::ok).filter(|e| e.path().extension().is_some_and(|x| x == "tif")).count()
                });
                if bands != 10 {
                    return fail(&format!(
                        "the adjustment should leave ten series of bands as files, there are {bands}"
                    ));
                }
                if series[0].0 != HARMONIZED_NAME || self.active != st.ard.target {
                    return fail("the harmonized series should come first and not take the active layer");
                }
                // Run again from the provenance: the same steps on, the same scope, the Output tab
                self.set_roi_rect(st.ard.rect);
                self.ard.on = [false; 4];
                self.ard.scope = 0;
                self.ard.open = false;
                self.run_again(&p);
                if self.ard.on != on_test || self.ard.scope != 2 || !self.ard.open || self.ard.tab != 2 {
                    return fail(&format!(
                        "Run again should turn the chain's steps on: {:?} scope {}",
                        self.ard.on, self.ard.scope
                    ));
                }
                st.ard.root = root;
                println!(
                    "    {files} files in two step folders, {bands} per-band series as files, the harmonized series ({} dates, {} bands) and {} more as layers",
                    hi.t(),
                    hi.bands_per_date,
                    series.len() - 1
                );
                next(
                    st,
                    "the chain's folder: provenance.json (done), rerun.py, a folder a step, the harmonized series one layer, run again",
                );
                None
            }
            315 => {
                let info = self.session.as_ref()?.info.clone();
                if self.active != st.ard.target {
                    return fail("the series the chain ran on should still be active");
                }
                let code = match self.ard_python(&info) {
                    Ok(c) => c,
                    Err(e) => return fail(&format!("Copy Python: {e}")),
                };
                let count = |s: &str| code.matches(s).count();
                if count("spectral_adjust_result = zeit.spectral_adjust(") != 1
                    || count("tasseled_cap_result = zeit.tasseled_cap(") != 1
                    || count("zeit.nbar(") != 0
                    || count("left out (") != 1
                    || count("harmonized = spectral_adjust_result.sel(band=[") != 1
                    || count("bands_cube = harmonized") != 1
                    || count("bands_cube = zeit.load_raster(") != 1
                {
                    return fail(&format!("Copy Python should have each step's call, on the right series:\n{code}"));
                }
                // the folder's own script has the same calls
                let root_script = std::fs::read_to_string(st.ard.root.join(SCRIPT)).unwrap_or_default();
                let calls: Vec<&str> = code.lines().filter(|l| l.contains("_result = zeit.")).collect();
                if calls.len() != 2 || calls.iter().any(|l| !root_script.contains(l)) {
                    return fail("the chain's rerun.py should hold the calls Copy Python gives");
                }
                println!(
                    "    Copy Python: {} lines, {} calls (NBAR a comment), the same as the folder's rerun.py",
                    code.lines().count(),
                    calls.len()
                );
                next(st, "Copy Python: the script of the steps on, each step's call");
                None
            }
            316 => {
                if st.ard.phase == 0 {
                    self.set_roi_rect(st.ard.rect);
                    self.ard_preview(ctx);
                    let Some(p) = self.ard.preview.as_ref() else {
                        return fail(&format!("the preview should start: {:?}", self.error_popup));
                    };
                    st.ard.preview = p.dir.clone();
                    st.ard.phase = 1;
                    st.since = Some(Instant::now());
                    return None;
                }
                let Some(p) = self.ard.preview.as_ref() else {
                    return fail("the preview should stay until turned off");
                };
                if let Some(j) = self.zeit.jobs.iter().find(|j| j.output_dir == p.dir)
                    && j.status.lock().ok()?.state == JobState::Failed
                {
                    return fail(&format!("the preview should finish: {}", j.status.lock().ok()?.error));
                }
                if !p.curtain {
                    return if waited > 90.0 {
                        fail("the swipe should come on the preview's harmonized series")
                    } else {
                        None
                    };
                }
                let dir = st.ard.preview.clone();
                let harmonized =
                    Path::new(&dir).join("02_spectral_adjust").join("series").to_string_lossy().into_owned();
                let Some(hk) = self.series_layer(&harmonized) else {
                    return fail("the preview's harmonized series should be a layer");
                };
                let cube = self.layer_session(hk)?.info.id;
                if self.swipe.as_ref().is_none_or(|s| s.view.cube != cube) {
                    return fail("the swipe should show the preview's harmonized series at its right");
                }
                if self.ard_test_layers_under(&dir) != 4 || self.active != st.ard.target {
                    return fail("the preview should add its four series as layers, without taking the active one");
                }
                // a preview has no provenance of its own (as the tools' previews)
                if Provenance::read(Path::new(&dir)).is_ok() || Path::new(&dir).join(SCRIPT).exists() {
                    return fail("a preview should write no provenance");
                }
                if self.zeit.preview.as_ref().is_none_or(|z| z.dir != dir) {
                    return fail("the preview should be the zeit preview (its dashed outline)");
                }
                println!("    preview on the ROI: swipe on, the harmonized series at its right, 4 series layers");
                next(st, "Preview on the ROI: the chain into a folder of its own, the swipe on the harmonized series");
                None
            }
            317 => {
                let dir = st.ard.preview.clone();
                if st.ard.phase == 0 {
                    self.ard_preview_stop(ctx);
                    st.ard.phase = 1;
                    st.since = Some(Instant::now());
                    return None;
                }
                if self.ard.preview.is_some()
                    || self.zeit.preview.is_some()
                    || self.ard_test_layers_under(&dir) != 0
                    || self.zeit.jobs.iter().any(|j| j.output_dir == dir)
                {
                    return fail("turning the preview off should remove its layers, its task and the preview");
                }
                if self.swipe.is_some() != st.ard.swipe || self.layers.len() != st.ard.before + 4 {
                    return fail("turning the preview off should take the swipe and its layers away, and only them");
                }
                if Path::new(&dir).exists() {
                    return if waited > 20.0 { fail("the preview's folder should be removed") } else { None };
                }
                next(st, "Preview off: its layers, the swipe, its task and its folder gone");
                None
            }
            318 => {
                // a preview turned off while its harmonized series is still being opened
                match st.ard.phase {
                    0 => {
                        self.set_roi_rect(st.ard.rect);
                        self.ard_preview(ctx);
                        let Some(p) = self.ard.preview.as_ref() else {
                            return fail(&format!("the preview should start: {:?}", self.error_popup));
                        };
                        st.ard.preview = p.dir.clone();
                        st.ard.phase = 1;
                        st.since = Some(Instant::now());
                    }
                    1 => {
                        let Some(p) = self.ard.preview.as_ref() else { return fail("the preview should stay") };
                        if !p.curtain {
                            return if waited > 90.0 { fail("the preview should finish") } else { None };
                        }
                        // as if its series had not been opened yet: the layers away, the series opened again, and the
                        // preview turned off before it is there
                        let dir = PathBuf::from(&st.ard.preview);
                        let harmonized = dir.join("02_spectral_adjust").join("series").to_string_lossy().into_owned();
                        self.ard_remove_layers_under(&dir, ctx);
                        if self.opening.is_some() || !self.zeit.series_to_open.is_empty() {
                            return None;
                        }
                        self.open_inputs(vec![harmonized], true, ctx);
                        if self.opening.is_none() {
                            return fail("the harmonized series should be being opened");
                        }
                        self.ard_preview_stop(ctx);
                        if self.ard.orphans.is_empty() {
                            return fail("a preview turned off while its series is opened should remember its folder");
                        }
                        st.ard.phase = 2;
                        st.since = Some(Instant::now());
                    }
                    _ => {
                        let dir = st.ard.preview.clone();
                        if self.opening.is_some() || !self.ard.orphans.is_empty() {
                            return if waited > 30.0 {
                                fail("the series should finish opening and its layer be dropped")
                            } else {
                                None
                            };
                        }
                        if self.ard_test_layers_under(&dir) != 0 || self.layers.len() != st.ard.before + 4 {
                            return fail("no layer of the preview's series should be left once it is turned off");
                        }
                        if self.swipe.is_some() != st.ard.swipe {
                            return fail("the swipe should be off");
                        }
                        if Path::new(&dir).exists() {
                            return if waited > 20.0 { fail("the preview's folder should be removed") } else { None };
                        }
                        next(st, "Preview off while its series was being opened: no layer left, the folder gone");
                    }
                }
                None
            }
            319 => {
                let series = self.ard.series_out.lock().ok()?.clone();
                let path = series.iter().find(|(n, _)| n == HARMONIZED_NAME)?.1.clone();
                if st.ard.phase == 0 {
                    // the Output tab's button: the harmonized series active, the Time series analysis window open
                    self.tsa = Default::default();
                    self.use_series(&path, ctx);
                    (self.tsa.open, self.tsa.tab) = (true, 0);
                    st.ard.phase = 1;
                    st.frames = 0;
                    return None;
                }
                st.frames += 1;
                let active = self.layers.get(self.active)?;
                let is_it = active.inputs.len() == 1 && key(&active.inputs[0]) == key(&path);
                let drawn = ctx.memory(|m| m.area_rect(egui::Id::new("tsa window"))).is_some();
                if !is_it || !self.tsa.open || self.tsa.pending_use.is_some() || !drawn || st.frames < 3 {
                    return if waited > 30.0 {
                        fail(
                            "Use in Time series analysis should make the harmonized series active and open that window",
                        )
                    } else {
                        None
                    };
                }
                println!("    active layer {}, the Time series analysis window open", active.name);
                // everything closed, restored as the stages after expect it
                self.ard = Default::default();
                self.tsa = Default::default();
                (self.tsa.open, self.tsa.tab) = st.ard.tsa;
                self.zeit.show_tasks = st.ard.tasks;
                let tag = st.ard.root.to_string_lossy().into_owned();
                for k in (st.ard.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                self.zeit.results.retain(|r| r.run != tag);
                self.zeit.jobs.retain(|j| j.output_dir != tag);
                self.zeit.results_dir = st.ard.results_dir.take();
                if st.ard.active < self.layers.len() && st.ard.active != self.active {
                    self.set_active(st.ard.active, ctx);
                }
                if self.swipe.is_some() != st.ard.swipe {
                    self.toggle_swipe();
                }
                if st.ard.roi.is_none() {
                    self.clear_roi();
                }
                self.series.hover = None;
                let _ = std::fs::remove_dir_all(&st.ard.dir);
                st.ard = Default::default();
                next(
                    st,
                    "Use in Time series analysis: the harmonized series active, the window open; all closed, layers and results removed",
                );
                st.stage = 330; // the crash notice (330-333, ui_test_crash.rs), then 299
                None
            }
            _ => None,
        }
    }
}
