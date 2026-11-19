//! The Tune on samples guide's screenshots (`--shots tune DATA OUT`; site/tune.html): LandTrendr calibrated on the
//! calibration points of an interpreted validation project. DATA is the folder of the synthetic forest frontier
//! (`ndvi_annual/`, `landtrendr_two_interpreters.gpkg` with its strata raster); the project is copied into a folder
//! of the temporary files first (the calibration split is saved into it), the originals are never written. The
//! tour makes the split, runs the 200 sets, tries the default and the best set on the chart, keeps the best, runs
//! LandTrendr on the whole image with the defaults and with it, and assesses both maps on the accuracy points. What
//! it measured is printed, and what the guide quotes of the run is written next to the project.

use std::path::PathBuf;

use egui::Context;
use janus_cube::BandSelection;
use janus_validation as val;
use janus_zeit::manifest;
use janus_zeit::tune::SetKind;
use serde_json::json;

use crate::app::{State, Tab};
use crate::shots::{Shots, Target, tool_tab};

/// The project's file, and the raster of its strata beside it.
const PROJECT: &str = "landtrendr_two_interpreters.gpkg";
const STRATA: &str = "landtrendr_two_interpreters_strata.tif";

/// What the tour keeps between its stages.
#[derive(Default)]
pub struct TuneShots {
    /// the folder the project was copied to
    work: PathBuf,
    project: String,
    /// the years of detection of the two runs
    yod_default: String,
    yod_tuned: String,
    /// the sets tried on the chart
    set_default: usize,
    set_best: usize,
    /// the pixels (column, row) of the pins
    pins: Vec<(usize, usize)>,
}

/// The year-of-detection result of the last run, from the result maps before it (`first`).
fn yod_since(s: &State, first: usize) -> Option<String> {
    s.zeit.results.iter().skip(first).find(|r| r.name.ends_with("Year of detection")).map(|r| r.path.clone())
}

impl State {
    /// The Tune window kept at the window's right edge, whatever its width (it grows with the table).
    fn shots_place_tune(&mut self, ctx: &Context) {
        if let Some(r) = ctx.memory(|m| m.area_rect(egui::Id::new("tune window"))) {
            self.tune.pos = Some(egui::pos2(ctx.content_rect().width() - r.width() - 6.0, 34.0));
        }
    }

    /// Only the result map at `path` drawn.
    fn shots_only_result(&mut self, path: &str) -> bool {
        let mut found = false;
        for r in &mut self.zeit.results {
            r.visible = r.path == path;
            found |= r.visible;
        }
        self.map.dirty = true;
        found
    }

    pub(crate) fn shots_tune(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (tune, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        if t > if matches!(sh.stage, 4 | 5 | 9 | 10 | 18 | 19) { 1800.0 } else { 180.0 } {
            return fail(sh, "timeout");
        }
        let data = PathBuf::from(&sh.input);
        let lt = tool_tab("landtrendr");
        let Some(tool) = manifest().tool("landtrendr") else { return fail(sh, "no LandTrendr tool") };
        let n_dates = |s: &State| s.session.as_ref().map_or(0, |s| s.info.t());
        let window = || Target::Area("tune window");
        if (6..=17).contains(&sh.stage) && self.tune.open {
            self.shots_place_tune(ctx);
        }
        match sh.stage {
            0 => {
                // the project copied, the annual NDVI series opening
                let work = std::env::temp_dir().join("janus-tune-shots");
                let _ = std::fs::remove_dir_all(&work);
                if let Err(e) = std::fs::create_dir_all(&work) {
                    return fail(sh, &format!("{}: {e}", work.display()));
                }
                for f in [PROJECT, STRATA] {
                    if let Err(e) = std::fs::copy(data.join(f), work.join(f)) {
                        return fail(sh, &format!("copying {f}: {e}"));
                    }
                }
                sh.tune.project = work.join(PROJECT).to_string_lossy().into_owned();
                sh.tune.work = work;
                self.settings.s.interpreter = "Ana".into();
                self.shots_open(&data.join("ndvi_annual").to_string_lossy(), BandSelection::default(), ctx);
                sh.next("the project copied; the annual NDVI series opening");
            }
            1 => {
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                sh.layout = Some((vec![lt, Tab::Validation], vec![Tab::Tasks, Tab::Chips]));
                self.shots_colormap("NDVI");
                self.set_date(n_dates(self) - 1);
                sh.next(&format!("{} dates; the layout asked for", n_dates(self)));
            }
            2 => {
                // the project opened once the layout is made (its panel is docked where the layout puts it)
                if sh.frames == 1 {
                    let path = sh.tune.project.clone();
                    // the tour's own layout stays: the interpretation's arrangement is for a user's first project
                    // (the tour's state is taken out of `self.shots` while its stage runs, which that check misses)
                    self.interp_layout_done = true;
                    self.open_validation_project(&path, ctx);
                    self.val.show_chips = false;
                    self.val.select_tab = Some(1);
                    return None;
                }
                if sh.frames == 3 {
                    self.map.fit();
                    self.clear_pins();
                    sh.focus.push(Tab::Validation);
                }
                self.labels.show = false;
                self.val.show_chips = false;
                if self.val.project.is_none() || !self.shots_val_quiet() || t < 5.0 {
                    return None;
                }
                if self.val.blind {
                    self.set_validation_blind(false);
                    return None;
                }
                let p = self.val.project.as_ref()?;
                let n = p.points.len();
                sh.shoot("01-project", &[]);
                sh.next(&format!("the project: {n} points"));
            }
            3 => {
                // LandTrendr's window, its defaults
                if sh.frames == 1 && !self.shots_open_tool("landtrendr", json!({})) {
                    return fail(sh, "no LandTrendr tool");
                }
                if sh.frames == 3 {
                    sh.focus.push(lt);
                }
                if !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("-", &[("02-tool", Shots::fit(lt))]);
                sh.next("LandTrendr's window with the defaults");
            }
            4 => match self.shots_run_stage(sh, "landtrendr", 0, None, ctx) {
                1 => return fail(sh, "the run with the defaults did not start"),
                0 => sh.next("LandTrendr with the defaults over the image"),
                _ => {}
            },
            5 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                }
            }
            6 => {
                // the window of Tune on samples, before the split
                if sh.frames == 1 {
                    let Some(y) = yod_since(self, sh.results) else { return fail(sh, "no year of detection") };
                    sh.tune.yod_default = y;
                    self.shots_hide_results();
                    self.open_tune(tool);
                    let x = ctx.content_rect().width() - 590.0;
                    let tu = &mut self.tune;
                    (tu.window, tu.n_sets, tu.tolerance) = ((2001, 2020), 200, 1);
                    tu.pos = Some(egui::pos2(x, 34.0));
                    tu.scroll_to = Some("top");
                    return None;
                }
                if sh.frames < 8 || t < 2.0 {
                    return None;
                }
                sh.shoot("-", &[("03-window", window())]);
                sh.next("Tune on samples, before the split");
            }
            7 => {
                // the split made, the calibration points read
                if sh.frames == 1 {
                    self.tune_split_now();
                    return None;
                }
                if let Some((_, e)) = &self.tune.failed {
                    return fail(sh, &format!("the points should be read: {e}"));
                }
                let p = self.tune.prepared.as_ref()?;
                if t < 3.0 {
                    return None;
                }
                println!(
                    "    {} calibration points read in {:.2} s ({} with a change in 2001-2020), one set {:.3} s: {}",
                    p.points.len(),
                    p.read_seconds,
                    janus_zeit::tune::count_changes(&p.points, self.tune.window),
                    p.sec_per_set,
                    p.text
                );
                println!("    reference stats: {:?}", p.stats);
                println!("    labels: {:?}", self.tune.mapping);
                sh.shoot("-", &[("04-reference", window())]);
                sh.next("the split made, the calibration points read");
            }
            8 => {
                // (the window shows the reference, the search and the run at once: the screenshot of 7 holds them)
                sh.next("the search's settings are in the window");
            }
            9 => {
                if sh.frames == 1 {
                    self.tune_run_now(tool, ctx);
                    if !self.tune.running() {
                        return fail(sh, &format!("the search should start: {}", self.tune.error));
                    }
                    self.zeit.show_tasks = false;
                }
                sh.next("the 200 sets running");
            }
            10 => {
                if self.tune.running() {
                    return None;
                }
                if !self.tune.error.is_empty() {
                    return fail(sh, &format!("the search failed: {}", self.tune.error));
                }
                let Some(d) = self.tune.done.as_ref() else { return fail(sh, "the search left no result") };
                let r = &d.result;
                let show = |k: usize| {
                    let s = &r.sets[k];
                    format!(
                        "{:?}: omission {:.3}, commission {:.3}, F1 {:.3}, F1 dated {:.3}, dates {:.3}, error {:.2}",
                        s.values,
                        s.scores.omission,
                        s.scores.commission,
                        s.scores.f1,
                        s.scores.f1_dated,
                        s.scores.date_agreement,
                        s.scores.mean_abs_error
                    )
                };
                println!(
                    "    {} sets in {:.1} s on {} points; varied {:?}",
                    r.sets.len(),
                    d.seconds,
                    d.points.len(),
                    r.vary
                );
                let Some(def) = r.sets.iter().position(|s| s.kind == SetKind::Default) else {
                    return fail(sh, "the table has no default set");
                };
                let Some(best) = r.best() else { return fail(sh, "no best set") };
                println!(
                    "    default (rank {} of {}): {}",
                    d.ranked.iter().position(|&k| k == def).map_or(0, |r| r + 1),
                    r.sets.len(),
                    show(def)
                );
                if let Some(cur) = r.sets.iter().position(|s| s.kind == SetKind::Current) {
                    println!("    current: {}", show(cur));
                }
                println!("    best (rank 1): {}", show(best));
                println!("    {} on the Pareto front; PRCC {:?}", d.pareto.len(), d.prcc);
                if let Some(m) = &d.optimism {
                    println!(
                        "    k-fold ({} folds): in sample F1 {:.3} dated {:.3}; held out F1 {:.3} dated {:.3}; gap {:+.3}",
                        m.k,
                        m.in_sample.f1,
                        m.in_sample.f1_dated,
                        m.held_out.f1,
                        m.held_out.f1_dated,
                        m.f1_gap()
                    );
                }
                sh.tune.set_default = def;
                sh.tune.set_best = best;
                // a change the defaults miss or date wrongly and the best set dates right, and a no-change point the
                // defaults call a change and the best set does not
                let tol = f64::from(r.opts.tolerance);
                let right = |k: usize, i: usize| {
                    let at = r.sets[k].dates[i];
                    match (d.points[i].reference, at.is_finite()) {
                        (Some(y), true) => (at.floor() - y).abs() <= tol,
                        (None, false) => true,
                        _ => false,
                    }
                };
                let inside = |i: usize| {
                    d.points[i]
                        .reference
                        .is_none_or(|y| y >= f64::from(r.opts.window.0) && y <= f64::from(r.opts.window.1))
                };
                let hit = (0..d.points.len())
                    .find(|&i| d.points[i].reference.is_some() && inside(i) && !right(def, i) && right(best, i));
                let quiet =
                    (0..d.points.len()).find(|&i| d.points[i].reference.is_none() && !right(def, i) && right(best, i));
                println!("    pins: a change fixed {hit:?}, a no-change fixed {quiet:?}");
                let pins: Vec<usize> = hit.into_iter().chain(quiet).collect();
                sh.tune.pins = pins.iter().map(|&i| d.places[i]).collect();
                self.tune.scroll_to = Some("Results");
                sh.next("the search done");
            }
            11 => {
                if t < 3.0 {
                    return None;
                }
                sh.shoot("-", &[("06-results", window())]);
                sh.next("the table and the scatter");
            }
            12 => {
                if sh.frames == 1 {
                    self.tune.scroll_to = Some("Which parameters matter");
                }
                if t < 2.0 {
                    return None;
                }
                sh.next("which parameters matter, the optimism");
            }
            13 | 15 => {
                // a set tried: the window over the map and the table's selected row
                let best = sh.stage == 15;
                if sh.frames == 1 {
                    self.tune.open = true;
                    if !best {
                        self.clear_pins();
                        for &(c, r) in &sh.tune.pins.clone() {
                            self.add_pin(c, r);
                        }
                    }
                    self.tune_select_now(if best { sh.tune.set_best } else { sh.tune.set_default });
                    self.tune.scroll_to = Some("Results");
                    return None;
                }
                if !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                if best {
                    sh.shoot("09-live-best", &[]);
                }
                sh.next(if best { "the best set tried" } else { "the defaults tried" });
            }
            14 | 16 => {
                // the same, the window put aside: the chart and the statistics with the set's fit
                let best = sh.stage == 16;
                if sh.frames == 1 {
                    self.tune.open = false;
                    return None;
                }
                if !self.shots_quiet() || t < 2.0 {
                    return None;
                }
                let file = if best { "09-live-best-chart" } else { "08-live-default-chart" };
                sh.shoot("-", &[(file, Target::Union(Tab::TimeSeries, Tab::Statistics))]);
                sh.next(if best { "the best set's fit on the chart" } else { "the defaults' fit on the chart" });
            }
            17 => {
                // kept: the parameters stay in LandTrendr's window; what the guide quotes is written out
                if sh.frames == 1 {
                    self.tune.open = true;
                    self.tune_keep_now();
                    let params = self.zeit.params(tool).clone();
                    let Some(e) = self.tune.calibration_entry(&tool.id, &params) else {
                        return fail(sh, "the parameters kept should have a calibration entry");
                    };
                    let _ = std::fs::write(
                        sh.tune.work.join("calibration.json"),
                        serde_json::to_string_pretty(&e).unwrap_or_default(),
                    );
                    match self.tune.calibration_python(&tool.id, &params) {
                        Some(code) => {
                            let _ = std::fs::write(sh.tune.work.join("tune_search.py"), code);
                        }
                        None => println!("    no Python for the search"),
                    }
                    println!("    kept: {params:?}");
                    self.tune.scroll_to = Some("Which parameters matter");
                    return None;
                }
                if t < 2.0 {
                    return None;
                }
                sh.next("the parameters kept");
            }
            18 => {
                if sh.frames == 1 {
                    self.tune_close_now();
                    sh.focus.push(lt);
                    return None;
                }
                if sh.frames < 4 || !self.shots_quiet() {
                    return None;
                }
                if sh.frames == 4 {
                    sh.shoot("-", &[("11-tool-tuned", Shots::fit(lt))]);
                    return None;
                }
                self.clear_pins();
                match self.shots_run_stage(sh, "landtrendr", 0, None, ctx) {
                    1 => return fail(sh, "the run with the tuned parameters did not start"),
                    0 => sh.next("LandTrendr with the tuned parameters over the image"),
                    _ => {}
                }
            }
            19 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                }
            }
            20 => {
                // both maps assessed on the accuracy points; the three maps the project came with leave
                if sh.frames == 1 {
                    let Some(y) = yod_since(self, sh.results) else { return fail(sh, "no year of detection") };
                    sh.tune.yod_tuned = y;
                    self.shots_hide_results();
                    let sources = self.validation_sources();
                    let mut rules = Vec::new();
                    for (path, name) in
                        [(&sh.tune.yod_default, "LandTrendr, defaults"), (&sh.tune.yod_tuned, "LandTrendr, tuned")]
                    {
                        let Some(s) = sources.iter().find(|s| s.rule.path == *path) else {
                            return fail(sh, &format!("{path} is not offered as a map to assess"));
                        };
                        let mut rule = s.rule.clone();
                        rule.name = name.into();
                        rules.push(rule);
                    }
                    let Some(p) = self.val.project.as_mut() else { return fail(sh, "the project closed") };
                    let first = p.maps.len();
                    for rule in rules {
                        p.maps.push(val::AssessedMap { rule, mapping: Default::default() });
                    }
                    for k in first..first + 2 {
                        self.validation_add_map_data(k, ctx);
                    }
                    let v = &mut self.val;
                    for _ in 0..first {
                        if let Some(p) = v.project.as_mut() {
                            p.maps.remove(0);
                        }
                        v.data.remove(0);
                    }
                    v.selected = 0;
                    v.meta_dirty = true;
                    v.version += 1;
                    v.select_tab = Some(2);
                    return None;
                }
                let v = &self.val;
                let ready = v.data.len() == 2
                    && v.data.iter().all(|d| d.ready())
                    && v.project.as_ref().is_some_and(|p| p.maps.iter().all(|m| !m.mapping.is_empty()));
                if !ready || !self.shots_val_quiet() || t < 4.0 {
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.focus.push(Tab::Validation);
                sh.next("both maps assessed");
            }
            21 | 22 => {
                // each map's accuracy on the accuracy points
                let tuned = sh.stage == 22;
                if sh.frames == 1 {
                    self.val.selected = usize::from(tuned);
                    self.val.select_tab = Some(1);
                    self.val.sections = Some(0);
                    return None;
                }
                if !self.shots_val_quiet() || t < 3.0 {
                    return None;
                }
                let file = if tuned { "13-accuracy-tuned" } else { "12-accuracy-defaults" };
                sh.shoot("-", &[(file, Shots::fit(Tab::Validation))]);
                sh.next(if tuned { "the tuned map's accuracy" } else { "the map with the defaults: its accuracy" });
            }
            23 | 24 => {
                // the two maps over the series
                let tuned = sh.stage == 24;
                if sh.frames == 1 {
                    self.val.selected = usize::from(tuned);
                    let path = if tuned { sh.tune.yod_tuned.clone() } else { sh.tune.yod_default.clone() };
                    if !self.shots_only_result(&path) {
                        return fail(sh, "the result map is gone");
                    }
                    self.val.select_tab = Some(1);
                    for v in &mut self.vectors {
                        v.visible = false; // the sample hides the map
                    }
                    self.map.dirty = true;
                    self.map.fit();
                    return None;
                }
                if !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot(if tuned { "15-map-tuned" } else { "14-map-defaults" }, &[]);
                sh.next(if tuned { "the tuned map" } else { "the map with the defaults" });
            }
            _ => {
                println!("    results: {} | {}", sh.tune.yod_default, sh.tune.yod_tuned);
                println!("    work folder: {}", sh.tune.work.display());
                self.close_validation_project();
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
