//! `--selftest-ui` stages 355-364: Tune on samples (tune_ui.rs), offline. 355 writes a layer of 64 points over the open
//! series, half of them with a date of change in a field (the other half empty = no change), and opens a tool that
//! gives change dates (LandTrendr, else BFAST) and its layer; 356 opens the window on that layer and field with 10
//! sets; 357 waits for the points' series to be read and the one set timed, and starts the search; 358 waits for the
//! task and checks the table (filled, sorted by the dated F1), the Pareto front, the sensitivities, the k-fold
//! check; 359 selects a set, whose parameters go into the tool's window (the live fit refits), and "Back to the
//! current" returns them; 360 selects and keeps one ("Use these parameters": the provenance's calibration entry names
//! it, and goes when the parameters are edited); 361 closes the window and puts everything back; 362 the refusal of
//! the accuracy points (a project open: its message; else the window's problem without one). Then 365 (Climate data, ui_test_climate.rs).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};
use gdal::vector::{Geometry, LayerAccess, LayerOptions, OGRFieldType};
use janus_zeit::tune;
use serde_json::Value;

use super::UiTest;
use crate::app::State;
use crate::tune_ui::Source;
use crate::zeit::applicability;

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct TuneTest {
    dir: PathBuf,
    tool: String,
    /// the layers and the active one before, the tool's parameters before
    layers: usize,
    vectors: usize,
    params: BTreeMap<String, Value>,
    /// the layer's id
    layer: u32,
    /// the set tried
    set: usize,
}

const FILE: &str = "refs.gpkg";
const FIELD: &str = "change_date";
const POINTS: usize = 64;

/// The tools the test tunes, in the order it prefers them.
const TOOLS: [&str; 3] = ["landtrendr", "bfast_lite", "bfast"];

impl State {
    /// The 64 points of the reference layer written in the series' CRS: a grid, every other point with a date.
    fn tune_write_reference(&self, path: &std::path::Path) -> Result<(), String> {
        let info = &self.session.as_ref().ok_or("no series")?.info;
        if info.crs_wkt.is_empty() || info.geo_transform.is_none() {
            return Err("the series has no georeferencing".into());
        }
        let mut years: Vec<i32> = (0..info.t()).map(|t| info.decimal_year(t).floor() as i32).collect();
        years.dedup();
        if years.len() < 8 {
            return Err("the series has too few years".into());
        }
        let mut srs = SpatialRef::from_wkt(&info.crs_wkt).map_err(|e| e.to_string())?;
        srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let drv = gdal::DriverManager::get_driver_by_name("GPKG").map_err(|e| e.to_string())?;
        let mut ds = drv.create_vector_only(path).map_err(|e| e.to_string())?;
        let l = ds
            .create_layer(LayerOptions {
                name: "refs",
                srs: Some(&srs),
                ty: gdal::vector::OGRwkbGeometryType::wkbPoint,
                options: None,
            })
            .map_err(|e| e.to_string())?;
        l.create_defn_fields(&[(FIELD, OGRFieldType::OFTString)]).map_err(|e| e.to_string())?;
        for i in 0..POINTS {
            let (c, r) = (i % 8, i / 8);
            let (px, py) = (info.width * (c + 1) / 9, info.height * (r + 1) / 9);
            let (x, y) = info.pixel_to_geo(px as f64 + 0.5, py as f64 + 0.5).ok_or("no georeferencing")?;
            let g = Geometry::from_wkt(&format!("POINT ({x:.6} {y:.6})")).map_err(|e| e.to_string())?;
            let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
            f.set_geometry(g).map_err(|e| e.to_string())?;
            // half of them changed, in a year inside the series (not its first or last two)
            let date = if (c + r) % 2 == 1 {
                format!("{}-06-15", years[2 + (i * 7) % (years.len() - 4)])
            } else {
                String::new()
            };
            f.set_field_string(0, &date).map_err(|e| e.to_string())?;
            f.create(&l).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    pub(super) fn test_tune(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
        let tool = manifest_tool(&st.tune.tool);
        match stage {
            355 => {
                let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
                    println!("    no series is open: Tune on samples is left out");
                    st.stage = 365; // Climate data (365-369, ui_test_climate.rs), then 299
                    return None;
                };
                let Some(tool) = TOOLS
                    .iter()
                    .filter_map(|id| janus_zeit::manifest().tool(id))
                    .find(|t| applicability(t, &info).is_none())
                else {
                    println!(
                        "    no tool that gives change dates applies to the series open: Tune on samples is left out"
                    );
                    st.stage = 365; // Climate data (365-369, ui_test_climate.rs), then 299
                    return None;
                };
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-tune-{stamp}"));
                let _ = std::fs::remove_dir_all(&dir);
                if let Err(e) = std::fs::create_dir_all(&dir) {
                    return fail(&format!("{}: {e}", dir.display()));
                }
                if let Err(e) = self.tune_write_reference(&dir.join(FILE)) {
                    println!("    the reference cannot be written ({e}): Tune on samples is left out");
                    let _ = std::fs::remove_dir_all(&dir);
                    st.stage = 365; // Climate data (365-369, ui_test_climate.rs), then 299
                    return None;
                }
                self.set_tool_open(tool, true);
                let params = self.zeit.params(tool).clone();
                st.tune = TuneTest {
                    dir: dir.clone(),
                    tool: tool.id.clone(),
                    layers: self.layers.len(),
                    vectors: self.vectors.len(),
                    params,
                    ..Default::default()
                };
                self.open_inputs(vec![dir.join(FILE).to_string_lossy().into_owned()], true, ctx);
                next(st, &format!("{POINTS} reference points with a date field written and added; {} open", tool.name));
                None
            }
            356 => {
                let tool = tool?;
                let Some(v) = self
                    .vectors
                    .iter()
                    .find(|v| std::path::Path::new(&v.data.path).file_name().is_some_and(|f| f == FILE))
                else {
                    return if waited > 30.0 { fail("the reference layer should open") } else { None };
                };
                if !self.vector_loads.is_empty() || v.placed_for == 0 {
                    return None;
                }
                if v.data.points != POINTS || !v.data.fields.contains(&FIELD.to_string()) {
                    return fail("the reference layer should have 64 points and its date field");
                }
                let id = v.id;
                st.tune.layer = id;
                self.open_tune(tool);
                let t = &mut self.tune;
                if !t.open || t.tool != tool.id {
                    return fail("the tune window should open for the tool");
                }
                (t.source, t.field, t.n_sets, t.tolerance) = (Source::Layer(id), FIELD.to_string(), 10, 1);
                next(st, "Tune on samples opened on the layer and its date field, 10 sets");
                None
            }
            357 => {
                let tool = tool?;
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                if ctx.memory(|m| m.area_rect(egui::Id::new("tune window"))).is_none() {
                    return fail("the tune window should be drawn");
                }
                if let Some((_, e)) = &self.tune.failed {
                    return fail(&format!("the points should be read: {e}"));
                }
                let Some(p) = self.tune.prepared.as_ref() else {
                    return if waited > 60.0 { fail("the points should be read") } else { None };
                };
                let n_changes = tune::count_changes(&p.points, self.tune.window);
                println!(
                    "    {} points read in {:.2} s ({n_changes} with a change in {}-{}), one set {:.4} s",
                    p.points.len(),
                    p.read_seconds,
                    self.tune.window.0,
                    self.tune.window.1,
                    p.sec_per_set
                );
                if p.points.len() != POINTS || n_changes != POINTS / 2 || p.sec_per_set <= 0.0 {
                    return fail("all 64 points should be read, half with a change, and one set timed");
                }
                if p.points.iter().any(|q| q.values.is_empty() || q.values.iter().all(|v| v.is_nan())) {
                    return fail("every point should have a series");
                }
                self.tune_run_now(tool, ctx);
                if !self.tune.running() {
                    return fail(&format!("the search should start: {}", self.tune.error));
                }
                if !self.zeit.jobs.iter().any(|j| j.tool == crate::tune_ui::JOB_TOOL) {
                    return fail("the search should be a task in Tools > Tasks");
                }
                next(st, "the points' series read, one set timed, the search started as a task");
                None
            }
            358 => {
                let tool = tool?;
                if self.tune.running() {
                    return if waited > 180.0 { fail("the search should end") } else { None };
                }
                if !self.tune.error.is_empty() {
                    return fail(&format!("the search failed: {}", self.tune.error));
                }
                let Some(d) = self.tune.done.as_ref() else { return fail("the search should leave a result") };
                let r = &d.result;
                if st.frames == 0 {
                    println!(
                        "    {} sets, {} on the Pareto front, best {:?}, F1 dated {:.3}; PRCC {:?}",
                        r.sets.len(),
                        d.pareto.len(),
                        r.best().map(|b| &r.sets[b].values),
                        r.best().map_or(f64::NAN, |b| r.sets[b].scores.f1_dated),
                        d.prcc.iter().map(|v| (v * 100.0).round() / 100.0).collect::<Vec<_>>()
                    );
                }
                if r.sets.len() < 10 || r.sets.len() > 12 || r.cancelled || d.ranked.len() != r.sets.len() {
                    return fail("the table should hold the defaults, the current values and about 10 sets");
                }
                let sorted = d.ranked.windows(2).all(|w| {
                    tune::cmp_scores(&r.sets[w[0]].scores, &r.sets[w[1]].scores) != std::cmp::Ordering::Greater
                });
                if !sorted || r.sets.iter().any(|s| s.error.is_some()) {
                    return fail("the sets should all run and be sorted by the dated F1");
                }
                if d.pareto.is_empty() {
                    return fail("the Pareto front should not be empty");
                }
                if d.prcc.len() != r.vary.len() || d.optimism.is_none() || r.few_changes {
                    return fail(
                        "one sensitivity for each parameter that varied, the k-fold check, and enough changes",
                    );
                }
                if !self.zeit.jobs.iter().any(|j| {
                    j.tool == crate::tune_ui::JOB_TOOL && j.status.lock().unwrap().state == crate::zeit::JobState::Done
                }) {
                    return fail("the task should be done in Tools > Tasks");
                }
                // the table and the scatter are drawn with it (a few frames; the growing-window check watches them)
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                // a set that differs from the current values, the best of them
                let current = self.zeit.params(tool).clone();
                let pick = d.ranked.iter().copied().find(|&k| {
                    let vals = &r.sets[k].values;
                    r.vary.iter().zip(vals).any(|(id, v)| current.get(id) != Some(v))
                });
                let Some(k) = pick else { return fail("some set should differ from the current values") };
                st.tune.set = k;
                next(
                    st,
                    "the table is filled and sorted, the Pareto front, the sensitivities and the k-fold check are there",
                );
                None
            }
            359 => {
                let tool = tool?;
                let k = st.tune.set;
                let v0 = self.zeit.tools.get(&tool.id).map_or(0, |u| u.version);
                self.tune_select_now(k);
                let Some(d) = self.tune.done.as_ref() else { return fail("the result should stay") };
                let set = &d.result.sets[k];
                let params = self.zeit.params(tool).clone();
                for (id, v) in d.result.vary.iter().zip(&set.values) {
                    if params.get(id) != Some(v) {
                        return fail(&format!("selecting a set should put {id} = {v} in the tool's window"));
                    }
                }
                if self.zeit.tools.get(&tool.id).map_or(0, |u| u.version) <= v0 || self.tune.selected != Some(k) {
                    return fail("selecting a set should make the chart's fit again with it");
                }
                self.tune_back_now();
                if self.zeit.params(tool).clone() != st.tune.params || self.tune.selected.is_some() {
                    return fail("Back to the current should return the tool's parameters");
                }
                next(st, "selecting a set puts its parameters in the tool's window; Back to the current returns them");
                None
            }
            360 => {
                let tool = tool?;
                let k = st.tune.set;
                self.tune_select_now(k);
                self.tune_keep_now();
                let params = self.zeit.params(tool).clone();
                let Some(e) = self.tune.calibration_entry(&tool.id, &params) else {
                    return fail("the parameters kept should come with a calibration entry for the provenance");
                };
                let layer = e["reference"]["layer"].as_str().unwrap_or("");
                if e["reference"]["kind"] != "layer"
                    || !layer.contains("refs")
                    || e["reference"]["field"] != FIELD
                    || e["points"] != POINTS
                    || e["year_window"][0].as_i64().is_none()
                    || e["tolerance_years"] != 1
                    || e["sets"] != 10
                    || e["chosen"]["params"].as_object().is_none_or(|o| o.is_empty())
                {
                    return fail(&format!("the calibration entry is wrong: {e}"));
                }
                println!("    calibration entry: {}", e.to_string().chars().take(220).collect::<String>());
                // edited since: no longer from the tuning
                let mut edited = params.clone();
                if let Some(id) = self.tune.done.as_ref().and_then(|d| d.result.vary.first().cloned()) {
                    edited.insert(id, Value::from(12345));
                }
                if self.tune.calibration_entry(&tool.id, &edited).is_some()
                    || self.tune.calibration_entry("another tool", &params).is_some()
                {
                    return fail("parameters edited after the tuning should not say they came from it");
                }
                next(st, "Use these parameters keeps the set, with a calibration entry that goes when they are edited");
                None
            }
            361 => {
                let tool = tool?;
                self.tune_close_now();
                if self.tune.open {
                    return fail("the tune window should close");
                }
                // everything back: the tool's parameters, the tool's window, the layer, the task, the files
                *self.zeit.params(tool) = st.tune.params.clone();
                self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
                self.set_tool_open(tool, false);
                if let Some(i) = self.vector_index(st.tune.layer) {
                    self.remove_vector(i);
                }
                for j in self.zeit.jobs.iter().filter(|j| j.tool == crate::tune_ui::JOB_TOOL) {
                    let _ = std::fs::remove_dir_all(&j.output_dir);
                }
                self.zeit.jobs.retain(|j| j.tool != crate::tune_ui::JOB_TOOL);
                self.tune = Default::default();
                let _ = std::fs::remove_dir_all(&st.tune.dir);
                if self.vectors.len() != st.tune.vectors || self.layers.len() != st.tune.layers {
                    return fail("the layers should be as they were");
                }
                next(st, "window closed, the tool's parameters, its window, the layer and the task put back");
                None
            }
            362 => {
                // the accuracy points are refused, with the reason
                let t = crate::tune_ui::TuneUi::asking(Source::Project, true);
                let msg = self.tune_problem(&t).unwrap_or_default();
                if self.val.project.is_some() {
                    if !msg.contains("cannot calibrate") {
                        return fail(&format!("the accuracy points should be refused: {msg}"));
                    }
                    println!("    a project is open: {msg}");
                } else if !msg.contains("project") {
                    return fail(&format!("without a project the window should say so: {msg}"));
                }
                println!("    refusal: {msg}");
                next(st, "the accuracy points are refused with their reason");
                st.stage = 365; // Climate data (365-369, ui_test_climate.rs), then 299
                None
            }
            _ => None,
        }
    }
}

fn manifest_tool(id: &str) -> Option<&'static janus_zeit::Tool> {
    janus_zeit::manifest().tool(id)
}
