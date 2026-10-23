//! The screenshots of the guides of segmentation and deep learning (site/snic.html, site/deep-learning.html,
//! site/presto.html): `--shots snic|ml|presto LANDSAT OUT`, LANDSAT the Landsat folder of the CCDC guide
//! (tools/make_guide_data.py landsat) with the labels next to it, in LANDSAT/../labels (tools/make_guide_data.py
//! labels: MapBiomas 2022's samples.gpkg and areas.gpkg).

use std::path::{Path, PathBuf};

use egui::Context;
use serde_json::json;

use crate::app::{State, Tab};
use crate::shots::{Shots, tool_tab};

/// The labels' folder of a tour: next to its Landsat folder.
fn labels_dir(input: &str) -> PathBuf {
    Path::new(input).parent().unwrap_or(Path::new(".")).join("labels")
}

impl State {
    /// A vector file of the labels opened as a vector layer: once placed, its features coloured by their class
    /// (as the class maps colour them: in the order the classes come), its id.
    fn shots_labels_layer(&mut self, file: &str) -> Option<u32> {
        let v = self.vectors.iter_mut().find(|v| v.data.path.ends_with(file) && v.placed_for != 0)?;
        let field = v.data.fields.iter().position(|f| f == "class")?;
        let mut order: Vec<String> = vec![];
        v.colors = v
            .data
            .features
            .iter()
            .map(|f| {
                let c = f.values[field].trim().to_string();
                let k = order.iter().position(|o| *o == c).unwrap_or_else(|| {
                    order.push(c);
                    order.len() - 1
                });
                let [r, g, b] = crate::ml::class_color(k);
                egui::Color32::from_rgb(r, g, b)
            })
            .collect();
        self.map.dirty = true;
        Some(v.id)
    }

    /// The Deep learning window on a layer of labels (its class column), for `arch`.
    fn shots_ml_setup(&mut self, vector: u32, arch: usize) {
        let field = self
            .vectors
            .iter()
            .find(|v| v.id == vector)
            .and_then(|v| v.data.fields.iter().position(|f| f == "class"))
            .unwrap_or(0);
        self.ml.open = true;
        self.ml.source = 2;
        self.ml.vector = vector;
        self.ml.field = field;
        self.ml.arch = arch;
        // NDVI and the six reflectances on each date
        self.ml.shown = true;
        self.ml.bands = (1..=6).collect();
        self.ml.gpu = true;
        self.ml.scope = 0;
        // Landsat in the rainy season: most dates are clouds
        self.ml.min_valid = 10.0;
    }

    /// The training: -1 under way, 0 done (the model kept), 1 failed; its summary printed once.
    fn shots_trained(&mut self, sh: &mut Shots) -> i32 {
        let Some(run) = self.ml.run.clone() else { return 1 };
        let r = run.lock().unwrap();
        if !r.done {
            return -1;
        }
        if !r.error.is_empty() {
            println!("    training failed: {}", r.error);
            return 1;
        }
        if sh.frames > 1 && sh.printed == sh.stage {
            return 0;
        }
        sh.printed = sh.stage;
        if let Some(h) = &r.header {
            let v = &h.training["validation"];
            println!(
                "    {}: {} epochs (best {}), {} training and {} validation samples, overall accuracy {:.3}; \
                 producer's {}; user's {}",
                h.arch.name(),
                h.training["history"].as_array().map_or(0, Vec::len),
                h.training["best_epoch"],
                h.training["samples"]["train"],
                h.training["samples"]["validation"],
                v["overall_accuracy"].as_f64().unwrap_or(f64::NAN),
                v["producers_accuracy"],
                v["users_accuracy"],
            );
        }
        drop(r);
        self.pump_ml();
        0
    }

    /// The model applied to the whole image, followed as a tour's run.
    fn shots_ml_apply(&mut self, sh: &mut Shots, ctx: &Context) {
        self.ml.scope = 0;
        sh.results = self.zeit.results.len();
        self.ml_apply_now(ctx);
        sh.job = Some(self.zeit.jobs.len() - 1);
        // a run: the stages that wait for it may take long
        sh.job_stage = Some(sh.stage);
        self.zeit.show_tasks = true;
    }

    /// The tours of segmentation and deep learning.
    pub(crate) fn shots_ml(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let tour = sh.tour.clone();
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL ({tour}, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        if t > 3600.0 {
            return fail(sh, "timeout");
        }
        let ready = |s: &State| s.opening.is_none() && s.session.is_some();
        let labels = labels_dir(&sh.input);
        let dl = Tab::DeepLearning;
        // a clearing of 2022 among pasture and forest (the CCDC guide's)
        let clearing = [-63.119001, -10.130167];

        if sh.tour == "snic" {
            let snic = tool_tab("snic");
            match sh.stage {
                0 => {
                    self.shots_open_landsat(sh, ctx);
                    sh.next("the Landsat series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![snic], vec![Tab::Tasks]));
                    self.shots_landsat_view();
                    self.shots_pin_at(clearing[0], clearing[1]);
                    sh.next("the series in SWIR1 / NIR / red; a pin on the clearing");
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    if sh.frames < 40 || !self.shots_quiet() {
                        return None;
                    }
                    if !self.shown(snic) && !self.shots_open_tool("snic", json!({"spacing": 12, "min_valid": 10.0})) {
                        return fail(sh, "no SNIC tool");
                    }
                    sh.focus.push(snic);
                    if !sh.docked.contains(&snic) || t < 6.0 {
                        return None;
                    }
                    sh.shoot("01-tool", &[("tool", Shots::fit(snic)), ("chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("SNIC's window");
                }
                3 => match self.shots_run_stage(sh, "snic", 0, Some("02-task"), ctx) {
                    1 => return fail(sh, "the run did not start"),
                    0 => sh.next("SNIC over the image"),
                    _ => {}
                },
                4 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                    }
                }
                5 => {
                    // the segments, the polygons and the mean series loaded
                    if sh.frames == 1 && !self.shots_show_result(sh, "Segment") {
                        return fail(sh, "no segments");
                    }
                    // the outlines would hide the segments, the whole image in view
                    for v in &mut self.vectors {
                        v.visible = false;
                    }
                    if self.vectors.is_empty() || !self.shots_quiet() || t < 4.0 {
                        return None;
                    }
                    // the mean series' layer (on top) hidden until its stage
                    if let Some(l) = self.layers.last_mut()
                        && l.visible
                    {
                        l.visible = false;
                        self.map.dirty = true;
                        return None;
                    }
                    sh.shoot("03-segments", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the segments");
                }
                6 => {
                    // closer, the image under the segments' outlines
                    if sh.frames == 1 {
                        self.shots_hide_results();
                        for v in &mut self.vectors {
                            v.visible = true;
                        }
                        self.shots_zoom_to_geo(clearing[0], clearing[1], 3.0);
                    }
                    if !self.shots_quiet() || t < 4.0 {
                        return None;
                    }
                    sh.shoot("04-polygons", &[]);
                    sh.next("the polygons over the image");
                }
                7 => {
                    // the segments' mean series: a layer of its own, the chart on it
                    if sh.frames == 1 {
                        let last = self.layers.len() - 1;
                        self.layers[last].visible = true;
                        self.set_active(last, ctx);
                    }
                    if sh.frames == 10 {
                        // one value a date (the segments' mean NDVI)
                        self.composite = None;
                        self.shots_colormap("NDVI");
                    }
                    if sh.frames < 20 || (!self.shots_quiet() && t < 60.0) || t < 6.0 {
                        return None;
                    }
                    sh.shoot("05-mean", &[("chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("the segments' mean series");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        if sh.tour == "ml" {
            match sh.stage {
                0 => {
                    self.shots_open_landsat(sh, ctx);
                    sh.next("the Landsat series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![dl], vec![Tab::Tasks]));
                    self.shots_landsat_view();
                    let f = labels.join("samples.gpkg").to_string_lossy().into_owned();
                    self.add_vector_files(vec![f], ctx);
                    sh.next("the series; MapBiomas' samples opening");
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    if sh.frames < 40 || self.shots_labels_layer("samples.gpkg").is_none() || !self.shots_quiet() {
                        return None;
                    }
                    if t < 5.0 {
                        return None;
                    }
                    sh.shoot("01-samples", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the samples over the series");
                }
                3 => {
                    if sh.frames == 1 {
                        let Some(id) = self.shots_labels_layer("samples.gpkg") else { return fail(sh, "no samples") };
                        self.shots_ml_setup(id, 0);
                    }
                    sh.focus.push(dl);
                    if !sh.docked.contains(&dl) || t < 4.0 {
                        return None;
                    }
                    sh.shoot("-", &[("02-window", Shots::fit(dl))]);
                    sh.next("the Deep learning window: TempCNN on the samples");
                }
                4 => {
                    self.ml_train_now(ctx);
                    sh.next("TempCNN's training started");
                }
                5 => {
                    // its curves, half way
                    let epochs = self.ml.run.as_ref().map_or(0, |r| r.lock().unwrap().epochs.len());
                    let done = self.ml.run.as_ref().is_some_and(|r| r.lock().unwrap().done);
                    if epochs < 25 && !done {
                        return None;
                    }
                    sh.scroll = Some((dl, f32::MAX));
                    if sh.frames < 5 {
                        return None;
                    }
                    sh.shoot("-", &[("03-training", Shots::tab(dl))]);
                    sh.next("TempCNN training");
                }
                6 | 13 | 18 => {
                    // trained: its validation
                    match self.shots_trained(sh) {
                        r if r < 0 => return None,
                        r if r > 0 => return fail(sh, "the training failed"),
                        _ => {}
                    }
                    sh.scroll = Some((dl, f32::MAX));
                    if t < 3.0 || sh.frames < 10 {
                        return None;
                    }
                    let file = match sh.stage {
                        6 => "04-validation",
                        13 => "09-ltae",
                        _ => "10-utae",
                    };
                    sh.shoot("-", &[(file, Shots::tab(dl))]);
                    sh.next("the validation");
                }
                7 | 19 => {
                    // applied to the whole image
                    if sh.frames == 1 {
                        self.shots_ml_apply(sh, ctx);
                        return None;
                    }
                    sh.focus.push(Tab::Tasks);
                    let progress =
                        sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(0.0, |j| j.status.lock().unwrap().progress);
                    if progress < 0.2 && self.shots_ran(sh) < 0 {
                        return None;
                    }
                    if sh.stage == 7 {
                        sh.shoot("-", &[("05-task", Shots::fit(Tab::Tasks))]);
                    }
                    sh.next("the classification under way");
                }
                8 | 20 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the classification failed: {}", self.shots_job_error(sh)));
                    }
                }
                9..=11 => {
                    let (name, file) = match sh.stage {
                        9 => ("Class", "06-class"),
                        10 => ("Probability of the class", "07-probability"),
                        _ => ("Uncertainty (entropy)", "08-uncertainty"),
                    };
                    if sh.frames == 1 {
                        if !self.shots_show_result(sh, name) {
                            return fail(sh, &format!("no {name}"));
                        }
                        for v in &mut self.vectors {
                            v.visible = false;
                        }
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    if sh.stage == 9 {
                        sh.shoot(file, &[("layers", Shots::tab(Tab::Layers))]);
                    } else {
                        sh.shoot(file, &[]);
                    }
                    sh.next(name);
                }
                12 => {
                    // L-TAE on the same samples
                    self.ml.arch = 1;
                    if sh.frames < 5 {
                        return None; // the window takes L-TAE's defaults
                    }
                    self.ml_train_now(ctx);
                    sh.next("L-TAE's training started");
                }
                14 => {
                    // U-TAE: MapBiomas' patches in the western half
                    if sh.frames == 1 {
                        let f = labels.join("areas.gpkg").to_string_lossy().into_owned();
                        self.add_vector_files(vec![f], ctx);
                    }
                    let id = self.shots_labels_layer("areas.gpkg")?;
                    self.shots_hide_results();
                    for v in &mut self.vectors {
                        v.visible = v.id == id;
                    }
                    self.shots_ml_setup(id, 3);
                    // 125 dates: patches of 32 px (64 px take most of a 12 GB GPU), the labels of a part of the
                    // western half
                    self.ml.patch = 32;
                    self.set_roi_rect([60, 300, 460, 700]);
                    self.ml.in_roi = true;
                    sh.next("MapBiomas' areas in the western half, an ROI");
                }
                15 => {
                    if sh.frames < 10 || !self.shots_quiet() || t < 4.0 {
                        return None;
                    }
                    sh.scroll = Some((dl, 0.0));
                    sh.shoot("11-areas", &[("window", Shots::fit(dl))]);
                    sh.next("the areas and U-TAE in the window");
                }
                16 => {
                    // fewer epochs than U-TAE's defaults: the guide's run
                    self.ml.cfg.epochs = 20;
                    self.ml.cfg.patience = 5;
                    if sh.frames < 3 {
                        return None;
                    }
                    self.ml_train_now(ctx);
                    sh.next("U-TAE's training started");
                }
                17 => {
                    sh.next("U-TAE training");
                }
                21 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Class") {
                        return fail(sh, "no U-TAE class map");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("12-utae-class", &[]);
                    sh.next("U-TAE's class map");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        if sh.tour == "presto" {
            match sh.stage {
                0 => {
                    self.shots_open_landsat(sh, ctx);
                    sh.next("the Landsat series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![dl], vec![Tab::Tasks]));
                    self.shots_landsat_view();
                    let f = labels.join("samples.gpkg").to_string_lossy().into_owned();
                    self.add_vector_files(vec![f], ctx);
                    sh.next("the series; MapBiomas' samples opening");
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    let id = self.shots_labels_layer("samples.gpkg")?;
                    if sh.frames < 40 || !self.shots_quiet() {
                        return None;
                    }
                    if self.ml.arch != 2 {
                        self.shots_ml_setup(id, 2);
                        self.ml.presto_year = 2025;
                        self.ml.presto_dates = false;
                    }
                    sh.focus.push(dl);
                    if !sh.docked.contains(&dl) || t < 5.0 {
                        return None;
                    }
                    sh.shoot("01-window", &[("window", Shots::fit(dl))]);
                    sh.next("Presto in the window: the monthly composites of 2025");
                }
                3 => {
                    self.ml_train_now(ctx);
                    sh.next("Presto's classifier training (the encoder's weights downloaded once)");
                }
                4 => {
                    match self.shots_trained(sh) {
                        r if r < 0 => return None,
                        r if r > 0 => return fail(sh, "the training failed"),
                        _ => {}
                    }
                    sh.scroll = Some((dl, f32::MAX));
                    if t < 3.0 || sh.frames < 10 {
                        return None;
                    }
                    sh.shoot("-", &[("02-validation", Shots::tab(dl))]);
                    sh.next("the validation");
                }
                5 => {
                    // the embeddings of the whole image
                    if sh.frames == 1 {
                        self.ml.scope = 0;
                        sh.results = self.zeit.results.len();
                        self.ml_embed_now(ctx);
                        sh.job = Some(self.zeit.jobs.len() - 1);
                        sh.job_stage = Some(sh.stage);
                        return None;
                    }
                    sh.focus.push(Tab::Tasks);
                    let progress =
                        sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(0.0, |j| j.status.lock().unwrap().progress);
                    if progress < 0.2 {
                        let state = sh.job.and_then(|j| self.zeit.jobs.get(j)).map(|j| j.status.lock().unwrap().state);
                        if state == Some(crate::zeit::JobState::Running) {
                            return None;
                        }
                    }
                    sh.shoot("-", &[("03-task", Shots::fit(Tab::Tasks))]);
                    sh.next("the embeddings under way");
                }
                6 => {
                    // done: the embedding layer
                    let state = sh.job.and_then(|j| self.zeit.jobs.get(j)).map(|j| j.status.lock().unwrap().state);
                    match state {
                        Some(crate::zeit::JobState::Running) => return None,
                        Some(crate::zeit::JobState::Done) => {}
                        _ => return fail(sh, &format!("the embeddings failed: {}", self.shots_job_error(sh))),
                    }
                    // read, and its principal components made
                    let read = self.layers.iter().any(|l| {
                        l.emb
                            .as_ref()
                            .is_some_and(|e| e.drawn() && !e.busy() && e.store.as_ref().is_some_and(|s| s.complete()))
                    });
                    if !read {
                        return None;
                    }
                    for v in &mut self.vectors {
                        v.visible = false;
                    }
                    if sh.frames < 60 || !self.shots_quiet() || t < 6.0 {
                        return None;
                    }
                    sh.shoot("04-embeddings", &[("embeddings", Shots::tab(Tab::Embeddings))]);
                    sh.next("Presto's embeddings");
                }
                7 => {
                    if sh.frames == 1 {
                        // back to the series, then the classifier over it
                        if let Some(i) = self.layers.iter().position(|l| l.emb.is_none() && !l.canvas) {
                            self.set_active(i, ctx);
                        }
                        return None;
                    }
                    if sh.frames < 20 || !ready(self) {
                        return None;
                    }
                    self.shots_ml_apply(sh, ctx);
                    sh.next("the classifier applied");
                }
                8 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the classification failed: {}", self.shots_job_error(sh)));
                    }
                }
                9 => {
                    if sh.frames == 1 {
                        if !self.shots_show_result(sh, "Class") {
                            return fail(sh, "no class map");
                        }
                        // the embeddings (on top) hidden
                        for l in &mut self.layers {
                            if l.emb.is_some() {
                                l.visible = false;
                            }
                        }
                        self.map.dirty = true;
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("05-class", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("Presto's class map");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }
        fail(sh, "unknown tour")
    }
}
