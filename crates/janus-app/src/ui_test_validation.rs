//! `--selftest-ui` stages 170-179 (src/app_validation.cpp, selfTestValidationUi). D (classes 3 Forest, 15 Pasture,
//! 24 Urban, 33 Water; 2000-2007): 170 opens it; 171 stratifies its first date (Pasture as change, a buffer of 1
//! px); 172 checks the strata against a pixel-by-pixel count and the strata layer, then draws 60 points into a
//! project; 173 checks the project, its sample layer and the interpretation (blind); 174 labels every point (every
//! 5th wrong) and checks the matrix; 175 reads the GeoPackage back, then merges a second interpreter's copy; 176
//! adjudicates a disagreement and adds a second map (D in 2007); 177 checks its assessment, draws every tab and
//! opens the chips; 178 waits for them and exports the report; 179 checks the report's files and closes the project.

#![allow(clippy::needless_range_loop)] // parallel arrays indexed together, as the C++

use std::collections::BTreeMap;

use gdal::vector::LayerAccess;
use janus_validation as val;

use super::UiTest;
use crate::app::State;
use crate::exports::ExportState;
use crate::validation::fmt_pm;

impl State {
    pub(super) fn test_validation(
        &mut self,
        st: &mut UiTest,
        input: &dyn Fn(usize) -> Option<String>,
        t0: std::time::Instant,
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
            st.since = Some(std::time::Instant::now());
        };
        let strata_layer =
            |s: &State, path: &str| s.layers.iter().position(|l| l.inputs.len() == 1 && l.inputs[0] == path);
        match st.stage {
            170 => {
                if self.opening.is_some() {
                    return None;
                }
                let Some(d) = input(2) else { return fail("D is the fourth input") };
                if self.layers.iter().any(|l| !l.canvas) {
                    self.close_validation_project();
                    self.stop_interpretation();
                    self.close_all(ctx);
                    return None; // a frame without a raster series
                }
                if self.layout_report.series_panels {
                    return fail("the chart and the statistics should go with the last raster series");
                }
                println!("    no raster series open: the chart and the statistics out of the dock");
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                st.dir = std::env::temp_dir().join(format!("janus-selftest-validation-ui-{stamp}"));
                let _ = std::fs::create_dir_all(&st.dir);
                self.open_inputs(vec![d], false, ctx);
                next(st, "D opened for a validation project");
            }
            171 => {
                let ready = self.opening.is_none()
                    && self.session.as_ref().is_some_and(|s| s.s.overview.complete())
                    && !self.layers.is_empty()
                    && self.classes.state != crate::classes::ClassState::Undecided;
                if !ready {
                    return None;
                }
                if self.classes.state != crate::classes::ClassState::On {
                    return fail("D should be categorical");
                }
                self.set_date(0);
                let first = self.session.as_ref()?.info.layers[0].path.clone();
                let src = self.validation_sources();
                let Some(s) = src.iter().find(|s| s.rule.path == first) else {
                    return fail("D's first date is not among the maps to stratify");
                };
                let mut r = s.rule.clone();
                if r.kind != val::MapKind::Classes
                    || r.names.len() < 4
                    || r.names.get(&15).map(String::as_str) != Some("Pasture")
                {
                    return fail("D's first date: classes with their names expected");
                }
                r.change = [15].into();
                let v = &mut self.val;
                v.maps = vec![r];
                v.bins_text = vec![String::new()];
                v.change_text = vec![String::new()];
                v.buffer = 1;
                v.limit = 0;
                let spec = match self.validation_spec() {
                    Ok(s) => s,
                    Err(e) => return fail(&e),
                };
                self.validation_compute_strata(spec, ctx);
                next(st, "strata of D's classes (Pasture as change, a buffer of 1 px) computing");
            }
            172 => {
                if self.val.strata_job.busy() || !self.val.to_open.is_empty() || self.opening.is_some() {
                    return None;
                }
                if !self.val.design_error.is_empty() {
                    return fail(&self.val.design_error.clone());
                }
                // the same strata, pixel by pixel, from the file
                let Ok(ds) = gdal::Dataset::open(&self.val.maps[0].path) else {
                    return fail("D's first date cannot be read");
                };
                let (w, h) = ds.raster_size();
                let a: Vec<i32> = ds
                    .rasterband(1)
                    .and_then(|b| b.read_as::<i32>((0, 0), (w, h), (w, h), None))
                    .map(|b| b.into_shape_and_vec().1)
                    .unwrap_or_default();
                let names: BTreeMap<i32, &str> = [(3, "Forest"), (15, "Pasture"), (24, "Urban"), (33, "Water")].into();
                let mut want: BTreeMap<String, i64> = BTreeMap::new();
                for y in 0..h as i64 {
                    for x in 0..w as i64 {
                        let c = a[y as usize * w + x as usize];
                        let mut near = false;
                        for dy in -1..=1 {
                            for dx in -1..=1 {
                                let (xx, yy) = (x + dx, y + dy);
                                near |= xx >= 0
                                    && yy >= 0
                                    && xx < w as i64
                                    && yy < h as i64
                                    && a[yy as usize * w + xx as usize] == 15;
                            }
                        }
                        let n = if c != 15 && near { "buffer (1 px)".to_string() } else { names[&c].to_string() };
                        *want.entry(n).or_insert(0) += 1;
                    }
                }
                let have: BTreeMap<String, i64> =
                    self.val.strata.strata.iter().map(|s| (s.name.clone(), s.pixels)).collect();
                if have != want {
                    println!("    strata {have:?}\n    counted {want:?}");
                    return fail("the strata differ from the pixel-by-pixel count");
                }
                if strata_layer(self, &self.val.strata_path).is_none() {
                    return fail("the strata should be shown as a layer");
                }
                let v = &mut self.val;
                v.alloc = 0;
                v.min_per = 8;
                let n = val::allocate(&v.strata.strata, 60, val::Alloc::Proportional, 8);
                for (s, n) in v.strata.strata.iter_mut().zip(n) {
                    s.n = n;
                }
                v.seed = 11;
                let spec = match self.validation_spec() {
                    Ok(s) => s,
                    Err(e) => return fail(&e),
                };
                let path = st.dir.join("proj.gpkg").to_string_lossy().into_owned();
                self.validation_draw(&path, spec, ctx);
                next(
                    st,
                    &format!("{} strata as counted pixel by pixel, shown as a layer; drawing 60 points", have.len()),
                );
            }
            173 => {
                if self.val.draw_job.busy() || !self.val.to_open.is_empty() || self.opening.is_some() {
                    return None;
                }
                if !self.val.design_error.is_empty() {
                    return fail(&self.val.design_error.clone());
                }
                let Some(p) = &self.val.project else { return fail("no project after the draw") };
                let n: i32 = p.strata.strata.iter().map(|s| s.n).sum();
                if p.points.len() != n as usize || n < 60 {
                    return fail("the project should have the points allocated");
                }
                if !self.layout_report.series_panels {
                    return fail("the chart and the statistics should come with a raster series");
                }
                let vi = self.vector_index(self.val.vector_id);
                if vi.is_none_or(|i| self.vectors[i].data.features.len() != p.points.len()) {
                    return fail("the sample should be a vector layer");
                }
                if !self.labels.project
                    || self.labels.vector != Some(self.val.vector_id)
                    || self.labels.who != self.interpreter_name()
                    || !self.val.blind
                {
                    return fail("the interpretation of the sample should start, blind");
                }
                next(st, &format!("project with {n} points, interpreted blind"));
            }
            174 => {
                let p = self.val.project.as_ref()?;
                let strata = strata_layer(self, &p.strata_path);
                let hidden = strata.is_some_and(|i| !self.layers[i].visible);
                if !hidden || self.val.data.first().is_none_or(|d| !d.ready()) {
                    return None;
                }
                let r = p.maps[0].rule.clone();
                let values = self.val.data[0].values.clone();
                let mut hits = 0;
                for i in 0..p.points.len() {
                    self.interp_go(i);
                    let name = r.class_name(r.class_of(values[i]));
                    let Some(mut k) = self.labels.classes.iter().position(|c| *c == name) else {
                        return fail(&format!("the label set should hold the map's classes ({name})"));
                    };
                    if i % 5 == 0 {
                        k = (k + 1) % 4;
                        hits += 1;
                    }
                    self.interp_label(Some(k));
                    self.interp_set_confidence(1 + (i % 3) as i32);
                    if i == 2 {
                        self.interp_add_event("Pasture".into());
                    }
                }
                if self.labels.entries[2].events.len() != 1 || self.labels.entries[4].confidence != 2 {
                    return fail("events and confidence");
                }
                self.save_labels();
                let p = self.val.project.as_ref()?;
                let d = &self.val.data[0];
                let pp = val::prepare(p, &p.maps[0], &d.values, &d.mapped, 0.95, &[]);
                let a = val::assess(&pp.input);
                let diag: i32 = (0..a.classes.len()).map(|c| a.counts[c][c]).sum();
                if a.n as usize != p.points.len() || diag != a.n - hits {
                    return fail("the error matrix of the labels given");
                }
                if !(a.overall.est > 0.5 && a.overall.est < 1.0) {
                    return fail("the overall accuracy");
                }
                st.frames = hits as usize;
                next(st, &format!("every point labelled ({hits} wrong): overall {}", fmt_pm(&a.overall)));
            }
            175 => {
                if !self.val.idle() {
                    return None;
                }
                if !self.val.write_error.is_empty() {
                    return fail(&self.val.write_error.clone());
                }
                let p = self.val.project.as_ref()?;
                let back = match val::load_project(&p.path) {
                    Ok(b) => b,
                    Err(e) => return fail(&e),
                };
                let Some(mine) = back.labels.get(&self.interpreter_name()) else { return fail("my labels read back") };
                for i in 0..p.points.len() {
                    let e = &self.labels.entries[i];
                    if mine.get(i).is_none_or(|m| m.label != e.label || m.confidence != e.confidence) {
                        return fail("the labels read back differ");
                    }
                }
                if mine[2].events.len() != 1 {
                    return fail("the event read back");
                }
                {
                    let Ok(ds) = gdal::Dataset::open(&p.path) else { return fail("the GeoPackage cannot be opened") };
                    let q = "SELECT COUNT(*) FROM samples WHERE status = 'labelled'";
                    let n =
                        ds.execute_sql(q, None, gdal::vector::sql::Dialect::DEFAULT).ok().flatten().and_then(
                            |mut rs| rs.features().next().and_then(|f| f.field_as_integer64(0).ok().flatten()),
                        );
                    if n != Some(p.points.len() as i64) {
                        return fail("the samples layer should hold the reference of every point");
                    }
                }
                // a second interpreter on a copy: the first 10 points, 3 of them otherwise
                let copy = st.dir.join("copy.gpkg").to_string_lossy().into_owned();
                if std::fs::copy(&p.path, &copy).is_err() {
                    return fail("the copy");
                }
                let classes = self.labels.classes.clone();
                let rows: Vec<(usize, val::Label)> = (0..10)
                    .map(|i| {
                        let mut l = self.labels.entries[i].clone();
                        if i == 1 || i == 3 || i == 6 {
                            let k = classes.iter().position(|c| *c == l.label).unwrap_or(0);
                            l.label = classes[(k + 1) % 4].clone();
                        }
                        l.updated = val::now_iso();
                        (i, l)
                    })
                    .collect();
                if let Err(e) = val::save_labels(&copy, "second", &rows, &[]) {
                    return fail(&e);
                }
                let (n, msg) = self.validation_merge(&copy);
                if n != 10 {
                    return fail(&format!("10 labels should be merged: {msg}"));
                }
                next(st, "the GeoPackage read back; 10 labels of a second interpreter merged");
            }
            176 => {
                if !self.val.idle() {
                    return None;
                }
                if !self.val.write_error.is_empty() {
                    return fail(&self.val.write_error.clone());
                }
                if self.val.conflicts() != 3 {
                    return fail(&format!("3 disagreements expected, {}", self.val.conflicts()));
                }
                self.start_adjudication();
                if self.labels.who != val::ADJUDICATED || self.labels.filter.len() != 3 {
                    return fail("adjudication over the 3 disagreements");
                }
                self.interp_label(Some(0));
                self.save_labels();
                if self.val.conflicts() != 2 {
                    return fail("an adjudicated point should settle");
                }
                self.labels.who = self.interpreter_name();
                self.validation_entries();
                self.labels.filter.clear();
                // a second map: D in 2007, assessed with the same sample
                let n = self.session.as_ref()?.info.t();
                self.set_date(n - 1);
                let last = self.session.as_ref()?.info.layers[n - 1].path.clone();
                let src = self.validation_sources();
                let Some(s) = src.iter().find(|s| s.rule.path == last) else {
                    return fail("D's last date among the maps");
                };
                let rule = s.rule.clone();
                let p = self.val.project.as_mut()?;
                p.maps.push(val::AssessedMap { rule, mapping: BTreeMap::new() });
                let k = p.maps.len() - 1;
                self.validation_add_map_data(k, ctx);
                self.val.meta_dirty = true;
                self.val.version += 1;
                next(st, "a disagreement adjudicated; D in 2007 added as a second map");
            }
            177 => {
                if self.val.data.get(1).is_none_or(|d| !d.ready()) || !self.val.idle() {
                    return None;
                }
                let p = self.val.project.as_ref()?;
                if p.maps[1].mapping.is_empty() {
                    return None; // its translations, guessed in the pump
                }
                // every tab of the panel drawn once (colours by stratum and right / wrong too), then on
                self.val.show = true;
                if st.frames < 10 {
                    self.val.select_tab = Some(st.frames % 5);
                    self.val.color_by = st.frames % 3;
                    self.val.selected = st.frames % 2;
                    st.frames += 1;
                    return None;
                }
                st.frames = 0;
                let d = &self.val.data[1];
                let pp = val::prepare(p, &p.maps[1], &d.values, &d.mapped, 0.95, &[]);
                let a = val::assess(&pp.input);
                if (a.n as usize) + 3 < p.points.len() {
                    return fail("the second map: every point with a reference");
                }
                let area: f64 = a.area.iter().map(|c| c.mapped).sum();
                if (area - p.strata.total_area()).abs() > 1e-6 * area {
                    return fail("the second map's areas over the strata");
                }
                self.val.show_chips = true;
                self.interp_go(0);
                next(st, &format!("the second map assessed (overall {}); chips", fmt_pm(&a.overall)));
            }
            178 => {
                if !self.val.chips.has_texture() {
                    return None;
                }
                if self.val.chips.dates.len() != self.session.as_ref()?.info.t() {
                    return fail("a chip per date expected");
                }
                let dir = st.dir.join("report");
                let _ = std::fs::create_dir_all(&dir);
                let Some(job) = self.export_validation_report(&dir.to_string_lossy(), ctx) else {
                    return fail("the report did not start");
                };
                st.jobs = vec![job];
                let p = self.val.project.as_ref()?;
                let py = val::python_snippet(p, &p.maps[0], 0.95);
                if !py.contains("zeit.accuracy(") || !py.contains("strata=") {
                    return fail("Copy Python");
                }
                next(st, "chips of every date; the report being written");
            }
            179 => {
                if st.jobs.first().is_none_or(|j| j.state() == ExportState::Running) || !self.val.idle() {
                    return None;
                }
                if st.jobs[0].state() != ExportState::Done {
                    return fail(&format!("the report failed: {}", st.jobs[0].error()));
                }
                for f in [
                    "report.html",
                    "design.csv",
                    "maps.csv",
                    "map1_accuracy.csv",
                    "map1_matrix_points.csv",
                    "map2_matrix_area.csv",
                    "interpreters.csv",
                ] {
                    if std::fs::metadata(st.dir.join("report").join(f)).map_or(0, |m| m.len()) == 0 {
                        return fail(&format!("the report's {f}"));
                    }
                }
                let strata = self.val.project.as_ref()?.strata_path.clone();
                let vid = self.val.vector_id;
                self.close_validation_project();
                if self.labels.project || self.vector_index(vid).is_some() {
                    return fail("closing the project");
                }
                if strata_layer(self, &strata).is_none_or(|i| !self.layers[i].visible) {
                    return fail("the strata layer should be shown again after the blind interpretation");
                }
                st.jobs.clear();
                self.val.show_chips = false;
                next(st, "report written; project closed");
                let _ = std::fs::remove_dir_all(&st.dir);
                println!("OK ({:.1} s)", t0.elapsed().as_secs_f64());
                return Some(0);
            }
            _ => {}
        }
        None
    }
}
