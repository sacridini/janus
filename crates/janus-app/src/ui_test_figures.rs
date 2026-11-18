//! `--selftest-ui` stages 335-344: figures (figure.rs, figure_ui.rs, graticule.rs). 335 opens the Save figure dialog from
//! the series chart and writes a PNG (the size mm x dpi, ink on it) and an SVG (the size in mm, text as text, paths); 336
//! the same from an Analysis tab; 337 the map PNG with the coordinate grid, the north arrow and a title (it differs
//! from the plain one where the overlay is); 338 the transect's figure (its image read back from the GPU). Then 355
//! (Tune on samples), and 299, the end.

use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::exports::ExportState;
use crate::figure::{FigureOptions, Format};
use crate::figure_ui::Source;

/// Whether the test put a cursor on the chart (the series gone by now), to take it away.
static CURSOR_PUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

static READY: std::sync::Mutex<Option<Instant>> = std::sync::Mutex::new(None);

/// The layers and the active one before a stage opened its data.
static LAYERS_BEFORE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static ACTIVE_BEFORE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The area of the figure dialog, drawn this frame or not.
fn dialog_drawn(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.area_rect(egui::Id::new("figure dialog"))).is_some()
}

impl State {
    /// Writes `source` as a figure to `path` and waits for the file: Err(why) when it fails.
    fn figures_write(
        &mut self,
        source: Source,
        o: &FigureOptions,
        path: &std::path::Path,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let before = self.exports.len();
        self.save_figure(source, o.clone(), path.to_string_lossy().into_owned(), ctx);
        let Some(job) = self.exports.get(before).cloned() else { return Err("the figure did not start".into()) };
        job.wait();
        if job.state() != ExportState::Done {
            return Err(format!("the figure should be written: {}", job.error()));
        }
        if !path.is_file() {
            return Err(format!("{} should exist", path.display()));
        }
        Ok(())
    }

    /// A PNG and an SVG of `source`: sizes and content. Returns a note.
    fn figures_check(
        &mut self,
        source: Source,
        dir: &std::path::Path,
        name: &str,
        ctx: &egui::Context,
    ) -> Result<String, String> {
        let o = FigureOptions { width_mm: 85.0, height_mm: 55.0, dpi: 150, ..Default::default() };
        let png = dir.join(format!("{name}.png"));
        self.figures_write(source, &o, &png, ctx)?;
        let img = image::open(&png).map_err(|e| format!("the PNG should be readable: {e}"))?.to_rgba8();
        let [w, h] = o.pixels();
        if (img.width(), img.height()) != (w, h) {
            return Err(format!(
                "the PNG should be {w} x {h} px (85 x 55 mm at 150 dpi), it is {} x {}",
                img.width(),
                img.height()
            ));
        }
        // a light figure: a white background with ink on it
        let ink = img.pixels().filter(|p| p.0[3] > 0 && p.0[0] < 200).count();
        let white = img.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count();
        if ink < 200 || white < img.pixels().len() / 4 {
            return Err(format!("the PNG should be a white figure with ink on it ({ink} dark pixels, {white} white)"));
        }
        let svg_path = dir.join(format!("{name}.svg"));
        let s = FigureOptions { format: Format::Svg, ..o.clone() };
        self.figures_write(source, &s, &svg_path, ctx)?;
        let svg = std::fs::read_to_string(&svg_path).map_err(|e| format!("the SVG should be readable: {e}"))?;
        let texts = svg.matches("<text ").count();
        let paths = svg.matches("<path ").count() + svg.matches("<rect ").count();
        if !svg.starts_with("<?xml")
            || !svg.contains("width=\"85mm\"")
            || !svg.contains("height=\"55mm\"")
            || !svg.trim_end().ends_with("</svg>")
        {
            return Err("the SVG should be a document 85 x 55 mm".into());
        }
        if texts < 2 || paths < 2 {
            return Err(format!(
                "the SVG should have its texts as text and its shapes as paths ({texts} texts, {paths} shapes)"
            ));
        }
        Ok(format!("PNG {w} x {h} px ({ink} dark pixels), SVG {texts} texts and {paths} shapes"))
    }

    pub(super) fn test_figures(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
        let dir = std::env::temp_dir().join("janus-selftest-figures");
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        self.session.as_ref()?;
        match stage {
            335 | 336 => {
                // the dialog opened from the panel's button; its window drawn
                let tab =
                    crate::analysis::FIGURE_TABS.iter().copied().find(|&k| self.analysis_tab_unavailable(k).is_none());
                let source = if stage == 335 { Source::Chart } else { tab.map_or(Source::Chart, Source::Analysis) };
                if stage == 336 && tab.is_none() {
                    println!("    no Analysis tab applies to the data open: the tab's figure is left out");
                    if CURSOR_PUT.swap(false, std::sync::atomic::Ordering::Relaxed) {
                        self.series.hover = None;
                    }
                    next(st, "no Analysis tab to draw");
                    return None;
                }
                if st.frames == 0 {
                    if stage == 335 {
                        let _ = std::fs::remove_dir_all(&dir);
                        let _ = std::fs::create_dir_all(&dir);
                        // a series on the chart: the cursor at the middle of the layer, if nothing is there
                        let sr = &self.series;
                        if sr.hover.is_none() && sr.pins.is_empty() && sr.roi.mean.is_empty() {
                            let (w, h) = self.session.as_ref().map_or((0, 0), |s| (s.info.width, s.info.height));
                            self.set_hover(Some((w / 2, h / 2)));
                            CURSOR_PUT.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    self.open_figure(source);
                    st.frames = 1;
                    return None;
                }
                st.frames += 1;
                if !dialog_drawn(ctx) || st.frames < 3 {
                    return if waited > 30.0 { fail("the Save figure dialog should open") } else { None };
                }
                if self.export.figure.source != Some(source) {
                    return fail("the dialog should be open for the panel that asked");
                }
                let name = if stage == 335 { "chart".to_string() } else { format!("analysis_{}", tab.unwrap_or(0)) };
                let mut note = match self.figures_check(source, &dir, &name, ctx) {
                    Ok(n) => n,
                    Err(e) => return fail(&e),
                };
                // every other tab that applies to the data open
                if stage == 336 {
                    for k in crate::analysis::FIGURE_TABS.iter().copied().filter(|&k| Some(k) != tab) {
                        if self.analysis_tab_unavailable(k).is_some() {
                            continue;
                        }
                        match self.figures_check(Source::Analysis(k), &dir, &format!("analysis_{k}"), ctx) {
                            Ok(n) => {
                                note += &format!(
                                    "
    tab {k}: {n}"
                                )
                            }
                            Err(e) => return fail(&format!("tab {k}: {e}")),
                        }
                    }
                }
                self.export.figure.source = None;
                if stage == 336 && CURSOR_PUT.swap(false, std::sync::atomic::Ordering::Relaxed) {
                    self.series.hover = None;
                }
                println!("    {note}");
                next(
                    st,
                    if stage == 335 {
                        "the chart's figure: PNG and SVG"
                    } else {
                        "an Analysis tab's figure: PNG and SVG"
                    },
                );
            }
            337 => {
                // the map with the grid, the arrow and a title, against the plain one
                let (geo, name) = {
                    let info = &self.session.as_ref()?.info;
                    (
                        info.geo_transform.is_some() && !info.crs_wkt.is_empty(),
                        self.layers.get(self.active).map(|l| l.name.clone()),
                    )
                };
                if st.frames == 0 {
                    let plain = crate::export_map::PngOptions { label: false, legend: false, ..Default::default() };
                    let full = crate::export_map::PngOptions {
                        graticule: 1,
                        north_arrow: true,
                        title: crate::export_map::ShortText::new("Figure test"),
                        ..plain
                    };
                    let (Some(a), Some(b)) = (
                        self.export_png(dir.join("map_plain.png").to_string_lossy().into_owned(), plain, ctx),
                        self.export_png(dir.join("map_grid.png").to_string_lossy().into_owned(), full, ctx),
                    ) else {
                        return fail("the map PNGs did not start");
                    };
                    st.jobs = vec![a, b];
                    st.frames = 1;
                    return None;
                }
                let jobs = st.jobs.clone();
                for j in &jobs {
                    j.wait();
                    if j.state() != ExportState::Done {
                        return fail(&format!("the map PNG should be written: {}", j.error()));
                    }
                }
                st.jobs.clear();
                let (Ok(a), Ok(b)) = (image::open(dir.join("map_plain.png")), image::open(dir.join("map_grid.png")))
                else {
                    return fail("the map PNGs should be readable");
                };
                let (a, b) = (a.to_rgba8(), b.to_rgba8());
                if (a.width(), a.height()) != (b.width(), b.height()) {
                    return fail("the two map PNGs should be of one size");
                }
                let differ = a.pixels().zip(b.pixels()).filter(|(p, q)| p != q).count();
                if geo && differ < 200 {
                    return fail(&format!(
                        "the grid, the arrow and the title should draw over the map ({differ} pixels differ)"
                    ));
                }
                if !geo && differ == 0 {
                    return fail("without a CRS only the title draws, and it should");
                }
                println!(
                    "    {} {}: {} x {} px, {differ} pixels changed by the grid, the north arrow and the title",
                    name.unwrap_or_default(),
                    if geo { "(with a CRS)" } else { "(no CRS)" },
                    a.width(),
                    a.height()
                );
                next(st, "the map PNG with a coordinate grid, a north arrow and a title");
            }
            338 => {
                // the transect's figure (its image read back from the GPU into the figure's own texture)
                let (w, h) = self.session.as_ref().map_or((0.0, 0.0), |s| (s.info.width as f64, s.info.height as f64));
                self.set_transect(vec![[10.5, h / 2.0], [w - 10.5, h / 2.0]]);
                if let Err(e) = self.transect_read() {
                    return fail(&e);
                }
                let note = match self.figures_check(Source::Transect, &dir, "transect", ctx) {
                    Ok(n) => n,
                    Err(e) => return fail(&e),
                };
                self.clear_transect();
                println!("    {note}");
                next(st, "the transect's figure: PNG and SVG");
            }
            339 | 340 => {
                // the Analysis tabs on other data: Zeit's Landsat series (monthly: Seasonal, Cycles, Bands), then the
                // categorical D (Classes); each opened as a layer, its tabs' figures written, the layer closed
                let first =
                    self.opts.inputs.first().and_then(|a| std::path::Path::new(a).parent().map(|d| d.to_path_buf()));
                let path = if stage == 339 {
                    first.map(|d| d.join("landsat")).filter(|d| d.is_dir()).map(|d| d.to_string_lossy().into_owned())
                } else {
                    first.map(|d| d.join("D")).filter(|d| d.is_dir()).map(|d| d.to_string_lossy().into_owned())
                };
                let Some(path) = path else {
                    println!("    no data for the stage: its Analysis figures are left out");
                    next(st, "no other data");
                    if stage == 340 {
                        let _ = std::fs::remove_dir_all(&dir);
                        st.stage = 355; // Tune on samples (355-364, ui_test_tune.rs), then 299
                    }
                    return None;
                };
                use std::sync::atomic::Ordering::Relaxed;
                if st.frames == 0 {
                    LAYERS_BEFORE.store(self.layers.len(), Relaxed);
                    ACTIVE_BEFORE.store(self.active, Relaxed);
                    self.open_inputs(vec![path], true, ctx);
                    st.frames = 1;
                    return None;
                }
                let before = LAYERS_BEFORE.load(Relaxed);
                let ready = self.opening.is_none()
                    && self.layer_jobs.queue.is_empty()
                    && self.layers.len() > before
                    && super::loaded(self.layer_session(self.layers.len() - 1));
                if !ready {
                    return if waited > 90.0 { fail("the data should open as a layer") } else { None };
                }
                st.frames += 1;
                let last = self.layers.len() - 1;
                if st.frames == 2 {
                    self.set_active(last, ctx);
                    let (w, h) = self.session.as_ref().map_or((0, 0), |s| (s.info.width, s.info.height));
                    self.set_hover(Some((w / 2, h / 2)));
                    return None;
                }
                // some seconds for what the tabs read (the overview, the bands' grid)
                // (the tabs ask for what they read when they are drawn: once now, as the panel would, for the seconds)
                if READY.lock().ok()?.is_none() {
                    let warm = FigureOptions { dpi: 72, ..Default::default() };
                    for k in crate::analysis::FIGURE_TABS {
                        if self.analysis_tab_unavailable(k).is_none() {
                            let _ = self.render_figure(Source::Analysis(k), &warm);
                        }
                    }
                }
                let since = *READY.lock().ok()?.get_or_insert_with(Instant::now);
                if st.frames < 6 || since.elapsed().as_secs_f64() < 10.0 {
                    return None;
                }
                *READY.lock().ok()? = None;
                let mut notes = Vec::new();
                for k in crate::analysis::FIGURE_TABS {
                    if self.analysis_tab_unavailable(k).is_some() {
                        continue;
                    }
                    let name = format!("data{}_tab{k}", stage - 338);
                    match self.figures_check(Source::Analysis(k), &dir, &name, ctx) {
                        Ok(n) => notes.push(format!("tab {k}: {n}")),
                        // (a tab with nothing to plot on this data is a message: not a failure)
                        Err(e) => notes.push(format!("tab {k}: {e}")),
                    }
                }
                for n in &notes {
                    println!("    {n}");
                }
                self.series.hover = None;
                for k in (before..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let a = ACTIVE_BEFORE.load(Relaxed);
                if a < self.layers.len() && a != self.active {
                    self.set_active(a, ctx);
                }
                if stage == 340 {
                    // JANUS_FIGURE_KEEP=<folder>: the files kept there (to look at them)
                    if let Ok(keep) = std::env::var("JANUS_FIGURE_KEEP") {
                        let _ = std::fs::create_dir_all(&keep);
                        for f in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                            let _ = std::fs::copy(f.path(), std::path::Path::new(&keep).join(f.file_name()));
                        }
                    }
                    let _ = std::fs::remove_dir_all(&dir);
                }
                next(st, "the Analysis tabs' figures on other data");
                if stage == 340 {
                    st.stage = 355; // Tune on samples (355-364, ui_test_tune.rs), then 299
                }
            }
            _ => {}
        }
        None
    }
}
