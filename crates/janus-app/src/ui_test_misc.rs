//! `--selftest-ui` stages 70-78 and 81 (src/app_selftest.cpp), with A active and B open: another font size
//! (ProggyForever, the scalable face) and the Files panel revealing the opened file (70-72); the Log window following
//! a file as it grows (73-74); the Analysis panel drawn on every tab (75-76); the layout scrambled and reset (77-78);
//! a finished Zeit run with a series output and a class map with typical series (81); a SNIC run on the series, its
//! segments, their mean series and their polygons (82-83); deep learning: a TempCNN trained on pins given a class,
//! kept in its file and applied, its class map, probability and uncertainty as layers (84-86).

use super::UiTest;
use crate::app::{LayoutCmd, State};

impl State {
    pub(super) fn test_misc(&mut self, st: &mut UiTest, t0: std::time::Instant, ctx: &egui::Context) -> Option<i32> {
        let stage = st.stage;
        let fail = |what: &str| {
            println!("FAIL (stage {stage}): {what}");
            Some(1)
        };
        let next = |st: &mut UiTest, done: &str| {
            println!("[{:5.1} s] {done}", t0.elapsed().as_secs_f64());
            st.stage += 1;
            st.since = Some(std::time::Instant::now());
            st.frames = 0;
        };
        let apply_font = |s: &State, px: i32| {
            let st = &s.settings.s;
            crate::theme::apply(ctx, st.theme, st.panel_opacity, px);
        };
        match st.stage {
            70 => {
                // another font size (applied before the next frame); the Files panel to A's first file
                st.font = self.settings.s.font_size;
                let px = if st.font == 20 { 16 } else { 20 };
                self.settings.s.font_size = px;
                apply_font(self, px);
                let first = self.session.as_ref()?.info.first_path.clone();
                self.files.reveal(&first);
                st.reveal = Some(first.into());
                next(st, "font size changed, Files panel asked to reveal A's first file");
            }
            71 => {
                if st.font > 0 {
                    let px = self.settings.s.font_size;
                    let size = ctx.style_of(ctx.theme()).text_styles[&egui::TextStyle::Body].size;
                    let scalable = crate::theme::scalable_face();
                    println!(
                        "    font size {px}: body {size:.1} pt, {} face",
                        if scalable { "scalable" } else { "pixel" }
                    );
                    if (size - crate::theme::egui_font_size(px)).abs() > 0.01 || !scalable {
                        return fail("the font size should apply at the next frame, with the scalable face");
                    }
                    self.settings.s.font_size = st.font;
                    apply_font(self, st.font);
                    st.font = -1;
                }
                // the panel, drawn here as well (the Files tab may be hidden)
                egui::Window::new("Self-test files").fixed_size([400.0, 600.0]).show(ctx, |ui| {
                    let _ = self.files.ui(ui);
                });
                if self.files.revealing() {
                    return None;
                }
                let want = std::path::absolute(st.reveal.as_ref()?).ok()?.to_string_lossy().to_lowercase();
                println!("    Files panel at {}", self.files.revealed());
                if self.files.revealed() != want {
                    return fail("the Files panel should reveal the opened file");
                }
                next(st, "font size applied and restored; the Files panel went to the opened file");
            }
            72 => {
                let px = self.settings.s.font_size;
                let size = ctx.style_of(ctx.theme()).text_styles[&egui::TextStyle::Body].size;
                if (size - crate::theme::egui_font_size(px)).abs() > 0.01
                    || crate::theme::scalable_face() != (px != crate::theme::DEFAULT_FONT_SIZE)
                {
                    return fail("the font size setting should be restored");
                }
                next(st, "font size restored");
            }
            73 => {
                // the Log window follows a file: its lines, then what it gains (Windows line ends dropped)
                let path = std::env::temp_dir().join("janus-selftest-log.txt");
                if std::fs::write(&path, "=== start\r\nline 2\r\nTraceback (most recent call last):\r\n").is_err() {
                    return fail("the log file cannot be written");
                }
                self.show_log_file(&path.to_string_lossy());
                st.reveal = Some(path);
                next(st, "Log window asked to show a file");
            }
            74 => {
                let path = st.reveal.clone()?;
                let v = &self.zeit.log;
                if v.shown() != path.to_string_lossy() {
                    return None; // drawn at the next frame
                }
                if st.frames == 0 {
                    if v.text() != "=== start\nline 2\nTraceback (most recent call last):\n" {
                        return fail("the Log window should show the file's lines");
                    }
                    use std::io::Write as _;
                    let mut f = std::fs::OpenOptions::new().append(true).open(&path).ok()?;
                    let _ = f.write_all(b"partial");
                    st.frames = 1;
                    return None;
                }
                if !v.text().ends_with("partial") {
                    return None; // looked at 4 times a second
                }
                let lines = v.text().lines().count();
                println!("    Log window: {} lines, then the line added", lines - 1);
                if lines != 4 {
                    return fail("the Log window should add what the file gains");
                }
                let _ = std::fs::remove_file(&path);
                self.zeit.log = Default::default();
                next(st, "Log window follows its file");
            }
            75 => {
                // Analysis: the panel drawn on every tab (its numbers are checked with the transect and the classes)
                self.analysis.show = true;
                self.analysis.tab = 0;
                next(st, "Analysis panel shown");
            }
            76 => {
                // a frame per tab
                st.frames += 1;
                if st.frames < 3 {
                    self.analysis.tab = st.frames;
                    return None;
                }
                if st.frames < 4 {
                    return None;
                }
                self.analysis.show = false;
                next(st, "Analysis panel drawn (seasonal, classes, scatter)");
            }
            77 => {
                // the layout scrambled: a panel in a window of its own
                self.show_exports = true;
                st.frames += 1;
                if st.frames == 2 {
                    self.layout_cmd = Some(LayoutCmd::Scramble);
                    return None;
                }
                if st.frames < 4 {
                    return None;
                }
                let r = &self.layout_report;
                println!(
                    "    scrambled: Display {}, {} window(s) of their own",
                    if r.display.is_some() { "docked" } else { "in a window of its own" },
                    r.detached
                );
                if r.display.is_some() || r.detached == 0 {
                    return fail("Display should be in a window of its own before the reset");
                }
                self.layout_cmd = Some(LayoutCmd::Reset); // what Ctrl+Shift+R does
                next(st, "layout scrambled: Display undocked");
            }
            78 => {
                st.frames += 1;
                if st.frames < 3 {
                    return None;
                }
                let r = self.layout_report.clone();
                println!(
                    "    after the reset: Display in node {:?}, Layers {:?}, Map {:?}, {} window(s) of their own",
                    r.display, r.layers, r.map, r.detached
                );
                let (Some(d), Some(l), Some(m)) = (&r.display, &r.layers, &r.map) else {
                    return fail("the reset should dock Display, Layers and Map in the main window");
                };
                if d == l || d == m || r.detached != 0 {
                    return fail("the reset should dock Display, Layers and Map apart in the main window");
                }
                if r.exports.is_none() {
                    return fail("the Exports window should be drawn again after the reset");
                }
                self.show_exports = false;
                next(st, "layout reset: panels docked as at the first start, floating windows placed again");
                st.stage = 79; // the export shortcuts, then Save all outputs
            }
            81 => {
                // a finished Zeit run with a series output (as NDFI's) and a class map with typical series (SOM's)
                let info = self.session.as_ref()?.info.clone();
                if st.frames == 0 {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    let dir = std::env::temp_dir().join(format!("janus-selftest-series-{stamp}"));
                    let _ = std::fs::create_dir_all(&dir);
                    let drv = gdal::DriverManager::get_driver_by_name("GTiff").ok()?;
                    let (w, h) = (info.width, info.height);
                    let create = |p: &std::path::Path, bands: usize| -> Option<gdal::Dataset> {
                        let mut ds = drv.create_with_band_type::<f32, _>(p, w, h, bands).ok()?;
                        if let Some(g) = info.geo_transform {
                            ds.set_geo_transform(&g).ok()?;
                        }
                        if !info.crs_wkt.is_empty() {
                            ds.set_projection(&info.crs_wkt).ok()?;
                        }
                        Some(ds)
                    };
                    // 3 dates in the band descriptions; the classes: 1 west, 2 east
                    {
                        use gdal::Metadata as _;
                        let (Some(series), Some(cls)) =
                            (create(&dir.join("tool_series.tif"), 3), create(&dir.join("tool_cluster.tif"), 1))
                        else {
                            return fail("could not write the run's files");
                        };
                        for b in 1..=3 {
                            let mut band = series.rasterband(b).ok()?;
                            let mut buf = gdal::raster::Buffer::new((w, h), vec![b as f32; w * h]);
                            band.write((0, 0), (w, h), &mut buf).ok()?;
                            band.set_description(&format!("200{b}-06-15")).ok()?;
                        }
                        let v: Vec<f32> = (0..w * h).map(|i| if i % w < w / 2 { 1.0 } else { 2.0 }).collect();
                        let mut buf = gdal::raster::Buffer::new((w, h), v);
                        cls.rasterband(1).ok()?.write((0, 0), (w, h), &mut buf).ok()?;
                    }
                    let path = |f: &str| dir.join(f).to_string_lossy().into_owned();
                    let outputs = vec![
                        serde_json::json!({"id": "series", "name": "Series", "series": true, "path": path("tool_series.tif")}),
                        serde_json::json!({
                            "id": "cluster", "name": "Cluster", "colormap": "Paired", "unit": "class",
                            "path": path("tool_cluster.tif"), "classes": ["West", "East"],
                            "class_series": [{"x": [2001.0, 2002.0], "y": [0.1, 0.2]}, {"x": [2001.0, 2002.0], "y": [0.8, 0.9]}],
                        }),
                    ];
                    let status = crate::zeit::JobStatus {
                        state: crate::zeit::JobState::Done,
                        progress: 1.0,
                        message: String::new(),
                        error: String::new(),
                        result: Some(janus_zeit::job::JobResult { outputs, window: [0, 0, w, h] }),
                        seconds: 0.0,
                    };
                    self.zeit.jobs.push(crate::zeit::Job {
                        title: "selftest run".into(),
                        tool: "som".into(),
                        cube: info.id,
                        output_dir: dir.to_string_lossy().into_owned(),
                        log_path: String::new(),
                        started: std::time::Instant::now(),
                        status: std::sync::Arc::new(std::sync::Mutex::new(status)),
                        cancel: Default::default(),
                        loaded: false,
                    });
                    st.run = Some((dir, self.layers.len(), self.active));
                    st.frames = 1;
                    return None;
                }
                let (dir, before, active) = st.run.clone()?;
                let tag = dir.file_name()?.to_string_lossy().into_owned();
                let cluster =
                    self.zeit.results.iter().position(|r| r.name.contains("Cluster") && r.path.contains(&tag));
                let ci = cluster.filter(|&k| !self.zeit.results[k].data.is_empty())?;
                if self.opening.is_some() || self.layers.len() == before {
                    return None; // the series output opens as a layer
                }
                let added = self.layers.last()?;
                let ai = added.session.as_ref()?.info.clone();
                println!(
                    "    series output: layer \"{}\", {} dates ({} .. {}), active layer {}",
                    added.name,
                    ai.t(),
                    ai.layers.first()?.label,
                    ai.layers.last()?.label,
                    if self.active == active { "kept" } else { "CHANGED" }
                );
                let c = &self.zeit.results[ci];
                let (west, east) = (c.value_at(0, info.height / 2), c.value_at(info.width - 1, info.height / 2));
                println!(
                    "    class map: {} classes, {} typical series, west {west} east {east}",
                    c.classes.len(),
                    c.class_series.len()
                );
                if self.layers.len() != before + 1
                    || ai.t() != 3
                    || !ai.time_is_date
                    || ai.layers[0].label != "2001-06-15"
                {
                    return fail("the series output should open as a new layer with its 3 dates");
                }
                if self.active != active {
                    return fail("the series output should not take the active layer");
                }
                if c.classes.len() != 2
                    || c.class_series.len() != 2
                    || c.class_series[1].1.len() != 2
                    || west != 1.0
                    || east != 2.0
                {
                    return fail("the class map should load with its classes and their typical series");
                }
                let last = self.layers.len() - 1;
                self.remove_layer(last, ctx);
                self.zeit.results.remove(ci);
                self.zeit.jobs.pop();
                let _ = std::fs::remove_dir_all(&dir);
                next(
                    st,
                    "Zeit run: a series output added as a layer (active one kept), a class map with typical series",
                );
                st.stage = 82;
            }
            82 => {
                // SNIC on the whole series, run as the tool's Run button runs it, into a folder of the test's
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-snic-{stamp}"));
                let tool = janus_zeit::manifest().tool("snic")?;
                let mut params: std::collections::BTreeMap<String, serde_json::Value> =
                    tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                params.insert("spacing".into(), 12.into());
                params.insert("tile".into(), 64.into());
                *self.zeit.params(tool) = params;
                self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                self.zeit.results_dir = Some(dir.clone());
                st.run = Some((dir, self.layers.len(), self.active));
                st.run_vectors = self.vectors.len();
                self.run_tool(tool, ctx);
                next(st, "SNIC run started on the whole series (seeds every 12 px, tiles of 64 px)");
            }
            83 => {
                let (dir, before, active) = st.run.clone()?;
                let job = self.zeit.jobs.last()?;
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                match state {
                    crate::zeit::JobState::Failed | crate::zeit::JobState::Cancelled => {
                        return fail(&format!("the SNIC run should finish: {error}"));
                    }
                    crate::zeit::JobState::Running => return None,
                    crate::zeit::JobState::Done => {}
                }
                let tag = dir.file_name()?.to_string_lossy().into_owned();
                let seg = self.zeit.results.iter().position(|r| r.is_segments() && r.path.contains(&tag));
                let si =
                    seg.filter(|&k| !self.zeit.results[k].data.is_empty() && self.zeit.results[k].tex.is_some())?;
                if self.opening.is_some()
                    || self.layers.len() == before
                    || self.vectors.len() == st.run_vectors
                    || self.zeit.loads_pending()
                {
                    return None; // the mean series opens as a layer, the polygons as a vector layer
                }
                let info = self.layers.get(active)?.session.as_ref().map(|s| s.info.clone()).or_else(|| {
                    (self.active == active).then(|| self.session.as_ref().map(|s| s.info.clone())).flatten()
                })?;
                let r = &self.zeit.results[si];
                let labels: std::collections::BTreeSet<i64> =
                    r.data.iter().filter(|v| v.is_finite()).map(|&v| v as i64).collect();
                let added = self.layers.last()?;
                let ai = added.session.as_ref()?.info.clone();
                let v = self.vectors.last()?;
                println!(
                    "    {} segments on the map ({}, {} x {} px), mean series \"{}\" of {} dates, {} polygons in {}",
                    labels.len(),
                    janus_gpu::colormaps::all()[r.cmap].name,
                    r.w,
                    r.h,
                    added.name,
                    ai.t(),
                    v.data.features.len(),
                    v.name
                );
                if !janus_gpu::colormaps::all()[r.cmap].qualitative || r.lo != 0.5 || labels.len() < 10 {
                    return fail("the segments should show one qualitative colour each");
                }
                if r.w != info.width || r.h != info.height || labels.iter().any(|&l| l < 0) {
                    return fail("the segments should cover the series, their labels the seeds' numbers");
                }
                if self.layers.len() != before + 1 || ai.t() != info.t() || self.active != active {
                    return fail(
                        "the mean series should open as a new layer of the series' dates, the active one kept",
                    );
                }
                if v.data.features.len() != labels.len() || !v.data.fields.iter().any(|f| f == "n_pixels") {
                    return fail("the polygons should open as a vector layer, one polygon a segment with its size");
                }
                let last = self.layers.len() - 1;
                self.remove_layer(last, ctx);
                self.remove_vector(self.vectors.len() - 1);
                self.zeit.results.remove(si);
                self.zeit.results.retain(|r| !r.path.contains(&tag));
                self.zeit.jobs.pop();
                self.zeit.results_dir = None;
                let _ = std::fs::remove_dir_all(&dir);
                next(
                    st,
                    "SNIC: the segments (a colour each), their mean series as a layer, their polygons as a vector layer",
                );
            }
            84 => {
                // pins given a class: the west and the east of the series; a TempCNN of a few epochs on the CPU
                let info = self.session.as_ref()?.info.clone();
                self.clear_pins();
                for k in 0..24 {
                    let east = k % 2 == 1;
                    let x = if east { info.width - 1 - (k * 7) % (info.width / 3) } else { (k * 7) % (info.width / 3) };
                    self.add_pin(x, (k * 13) % info.height);
                }
                let pins: Vec<(u32, usize)> = self.series.pins.iter().map(|p| (p.id, p.x)).collect();
                for (id, x) in pins {
                    let c = if x > info.width / 2 { "East" } else { "West" };
                    self.ml.pin_classes.insert(id, c.into());
                }
                self.ml.source = 3;
                self.ml.arch = 0;
                self.ml.cfg = janus_ml::TrainConfig { epochs: 5, ..janus_ml::TrainConfig::tempcnn() };
                self.ml.gpu = false;
                self.ml.scope = 0;
                let stamp =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                let dir = std::env::temp_dir().join(format!("janus-selftest-ml-{stamp}"));
                self.zeit.results_dir = Some(dir.clone());
                st.run = Some((dir, self.layers.len(), self.active));
                let (sites, classes) = match self.ml_sites() {
                    Ok(s) => s,
                    Err(e) => return fail(&format!("the pins should give samples: {e}")),
                };
                if classes != ["West", "East"] || sites.len() != 24 {
                    return fail(&format!("24 pins in two classes, got {} in {classes:?}", sites.len()));
                }
                self.ml_train_now(ctx);
                next(st, "deep learning: 24 pins given a class (West, East), a TempCNN training started");
            }
            85 => {
                let run = self.ml.run.clone()?;
                let r = run.lock().ok()?;
                if !r.done {
                    return None;
                }
                if !r.error.is_empty() {
                    return fail(&format!("the training should finish: {}", r.error));
                }
                let h = r.header.clone()?;
                let path = r.model_path.clone();
                drop(r);
                self.pump_ml();
                let back = match janus_ml::Model::read_header(&path) {
                    Ok(b) => b,
                    Err(e) => return fail(&format!("the model file should read back: {e}")),
                };
                println!(
                    "    {}: {} epochs, {} classes, validation accuracy {:.2}; {}",
                    h.arch.name(),
                    h.training["history"].as_array().map_or(0, Vec::len),
                    h.classes.len(),
                    h.training["validation"]["overall_accuracy"].as_f64().unwrap_or(f64::NAN),
                    std::path::Path::new(&path).file_name()?.to_string_lossy()
                );
                if back != h || h.classes.len() != 2 || self.ml.model.as_ref().map(|m| &m.0) != Some(&path) {
                    return fail("the trained model should be kept in its file and be the one to apply");
                }
                self.ml_apply_now(ctx);
                next(st, "the model trained and kept in its file; applied to the whole image");
            }
            86 => {
                let (dir, _, _) = st.run.clone()?;
                let job = self.zeit.jobs.last()?;
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                match state {
                    crate::zeit::JobState::Failed | crate::zeit::JobState::Cancelled => {
                        return fail(&format!("the classification should finish: {error}"));
                    }
                    crate::zeit::JobState::Running => return None,
                    crate::zeit::JobState::Done => {}
                }
                let tag = dir.to_string_lossy().into_owned();
                let mine: Vec<usize> =
                    (0..self.zeit.results.len()).filter(|&k| self.zeit.results[k].path.starts_with(&tag)).collect();
                if mine.len() < 3
                    || mine.iter().any(|&k| self.zeit.results[k].data.is_empty())
                    || self.zeit.loads_pending()
                {
                    return None;
                }
                let class = mine.iter().map(|&k| &self.zeit.results[k]).find(|r| !r.classes.is_empty())?;
                let n = class.data.iter().filter(|v| v.is_finite() && **v >= 1.0).count();
                println!(
                    "    class map: {:?}, {n} of {} pixels classified; with its probability and uncertainty",
                    class.classes,
                    class.data.len()
                );
                if class.classes != ["West", "East"] || n == 0 {
                    return fail("the class map should load with the model's classes");
                }
                self.zeit.results.retain(|r| !r.path.starts_with(&tag));
                self.zeit.jobs.retain(|j| !j.output_dir.starts_with(&tag));
                self.zeit.results_dir = None;
                self.ml = Default::default();
                self.clear_pins();
                let _ = std::fs::remove_dir_all(&dir);
                next(st, "deep learning: the class map (its classes), its probability and uncertainty as layers");
                st.stage = 7;
            }
            _ => {}
        }
        None
    }
}
