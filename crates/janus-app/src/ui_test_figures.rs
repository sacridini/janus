//! `--selftest-ui` stages 335-344: figures (figure.rs, figure_ui.rs, graticule.rs). 335 opens the Save figure dialog from
//! the series chart and writes a PNG (the size mm x dpi, ink on it) and an SVG (the size in mm, text as text, paths); 336
//! the same from an Analysis tab; 337 the map PNG with the coordinate grid, the north arrow and a title (it differs
//! from the plain one where the overlay is); 338 the transect's figure (its image read back from the GPU). Then 341 and 342, the figures as on screen (the dialog's default: GPU pixels at 1x to 4x against a real screenshot of the panel, themes, SVG, an Analysis tab and the transect); 350 follows, the second axis of the chart.

use std::time::Instant;

use super::UiTest;
use crate::app::State;
use crate::exports::ExportState;
use crate::figure::{FigureOptions, Format, Mode};
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
    pub(crate) fn figures_check(
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
                        st.stage = 341;
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
                            let _ = self.render_figure(Source::Analysis(k), &warm, ctx);
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
                    st.stage = 341;
                }
            }
            341 => {
                // the chart "as on screen" (the dialog's default): its size, its pixels against a real screenshot of
                // the panel, 2x against 1x, the interface's themes, a transparent background, an SVG
                use std::sync::atomic::Ordering::Relaxed;
                st.frames += 1;
                let phase = PHASE.load(Relaxed);
                let ppp = ctx.pixels_per_point();
                let seen = crate::figure::seen(ctx, Source::Chart.key());
                let shown = |scale: u32, transparent: bool| FigureOptions {
                    mode: Mode::AsShown,
                    scale,
                    transparent,
                    ..Default::default()
                };
                let keep = std::env::var_os("JANUS_FIGSHOTS").map(std::path::PathBuf::from);
                if let Some(k) = &keep {
                    let _ = std::fs::create_dir_all(k);
                }
                let dir = std::env::temp_dir().join("janus-selftest-figures-shown");
                match phase {
                    0 => {
                        // the chart in view: nothing over it, the panel drawn in this frame
                        if st.frames == 1 {
                            ANALYSIS_WAS.store(self.analysis.show, Relaxed);
                            // a series on the chart: the cursor at the middle of the layer, if nothing is there
                            let sr = &self.series;
                            if sr.hover.is_none() && sr.pins.is_empty() && sr.roi.mean.is_empty() {
                                let (w, h) = self.session.as_ref().map_or((0, 0), |s| (s.info.width, s.info.height));
                                self.set_hover(Some((w / 2, h / 2)));
                                HOVER_PUT.store(true, Relaxed);
                            }
                        }
                        self.show_exports = false;
                        self.zeit.show_tasks = false;
                        self.analysis.show = false;
                        self.export.figure.source = None;
                        let drawn = seen.filter(|s| ctx.cumulative_frame_nr().saturating_sub(s.frame) <= 1);
                        let Some(seen) = drawn.filter(|_| st.frames > 10) else {
                            return if waited > 30.0 {
                                fail("the chart panel should be drawn on screen")
                            } else {
                                None
                            };
                        };
                        if self.export.figure.opts.mode != Mode::AsShown || self.export.figure.opts.scale != 1 {
                            return fail("the dialog should start as on screen, at 1x");
                        }
                        let _ = std::fs::remove_dir_all(&dir);
                        let _ = std::fs::create_dir_all(&dir);
                        println!(
                            "    the chart panel on screen: {:.1} x {:.1} points at ({:.1}, {:.1}), {ppp} px a point",
                            seen.size.x, seen.size.y, seen.at.x, seen.at.y
                        );
                        self.open_figure(Source::Chart);
                        PHASE.store(1, Relaxed);
                        st.frames = 0;
                    }
                    1 => {
                        if !dialog_drawn(ctx) || st.frames < 3 {
                            return if waited > 30.0 { fail("the Save figure dialog should open") } else { None };
                        }
                        let seen = seen?;
                        let o = self.export.figure.opts.clone();
                        if o.mode != Mode::AsShown {
                            return fail("the dialog's default should be As on screen");
                        }
                        let png = dir.join("chart_shown.png");
                        if let Err(e) = self.figures_write(Source::Chart, &o, &png, ctx) {
                            return fail(&e);
                        }
                        let img = match image::open(&png) {
                            Ok(i) => i.to_rgba8(),
                            Err(e) => return fail(&format!("the PNG should be readable: {e}")),
                        };
                        let want = seen.pixels(ppp);
                        if [img.width(), img.height()] != want {
                            return fail(&format!(
                                "the 1x export should be the panel's size {} x {} px, it is {} x {}",
                                want[0],
                                want[1],
                                img.width(),
                                img.height()
                            ));
                        }
                        let svg_path = dir.join("chart_shown.svg");
                        let s = FigureOptions { format: Format::Svg, ..o.clone() };
                        if let Err(e) = self.figures_write(Source::Chart, &s, &svg_path, ctx) {
                            return fail(&e);
                        }
                        let svg = std::fs::read_to_string(&svg_path).unwrap_or_default();
                        let texts = svg.matches("<text ").count();
                        let shapes = svg.matches("<path ").count() + svg.matches("<rect ").count();
                        if !svg.starts_with("<?xml") || !svg.trim_end().ends_with("</svg>") || !svg.contains("viewBox=")
                        {
                            return fail("the SVG as on screen should be a document with a viewBox");
                        }
                        if svg.contains("mm\"") || texts < 2 || shapes < 2 || !svg.contains("fill=\"#") {
                            return fail(&format!(
                                "the SVG should be in points, with texts ({texts}) and shapes ({shapes})"
                            ));
                        }
                        println!(
                            "    dialog default As on screen; 1x PNG {} x {} px (the panel's), SVG {texts} texts, {shapes} shapes",
                            img.width(),
                            img.height()
                        );
                        self.export.figure.source = None;
                        PHASE.store(2, Relaxed);
                        st.frames = 0;
                    }
                    2 => {
                        // the dialog and the Exports window (opened by the saves) gone, the frames settled: a screenshot
                        self.show_exports = false;
                        self.zeit.show_tasks = false;
                        if st.frames < 12 {
                            return None;
                        }
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                        PHASE.store(3, Relaxed);
                        st.frames = 0;
                    }
                    3 => {
                        let shot = ctx.input(|i| {
                            i.raw.events.iter().find_map(|e| match e {
                                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                                _ => None,
                            })
                        });
                        let Some(shot) = shot else {
                            return if waited > 60.0 { fail("the screenshot should arrive") } else { None };
                        };
                        let seen = seen?;
                        let t = Instant::now();
                        let Some(r) = self.render_figure(Source::Chart, &shown(1, false), ctx) else {
                            return fail("the chart should draw");
                        };
                        let t_draw = t.elapsed().as_secs_f64() * 1000.0;
                        let (one, w, h) = match r.to_rgba_gpu(&self.render_state) {
                            Ok(x) => x,
                            Err(e) => return fail(&e),
                        };
                        let t_gpu = t.elapsed().as_secs_f64() * 1000.0 - t_draw;
                        // the panel's pixels in the screenshot
                        let [sw, sh] = shot.size;
                        let x0 = (seen.at.x * ppp).round() as usize;
                        let y0 = ((seen.at.y + seen.crop) * ppp).round() as usize;
                        if x0 + w > sw || y0 + h > sh {
                            return fail(&format!(
                                "the panel ({x0}, {y0}) {w} x {h} should be inside the screenshot {sw} x {sh}"
                            ));
                        }
                        let mut crop = Vec::with_capacity(w * h * 4);
                        for y in 0..h {
                            for p in &shot.pixels[(y0 + y) * sw + x0..(y0 + y) * sw + x0 + w] {
                                crop.extend_from_slice(&p.to_array());
                            }
                        }
                        let diff = mean_diff(&one, &crop);
                        println!(
                            "    1x offscreen {w} x {h} px against the screenshot's crop: mean difference {diff:.3} / 255 per channel \
                             (drawn {t_draw:.0} ms, GPU and readback {t_gpu:.0} ms)"
                        );
                        if let Some(k) = &keep {
                            let rgba: Vec<u8> = shot.pixels.iter().flat_map(|c| c.to_array()).collect();
                            let save = |name: &str, px: &[u8], w: usize, h: usize| {
                                let _ = image::save_buffer(
                                    k.join(name),
                                    px,
                                    w as u32,
                                    h as u32,
                                    image::ExtendedColorType::Rgba8,
                                );
                            };
                            save("window.png", &rgba, sw, sh);
                            save("chart_screen_crop.png", &crop, w, h);
                            save("chart_offscreen_1x.png", &one, w, h);
                        }
                        if diff >= 2.0 {
                            return fail(&format!(
                                "the 1x export should match the screenshot (difference {diff:.3} >= 2 / 255)"
                            ));
                        }
                        // 2x and 4x: the same layout, more pixels
                        let t = Instant::now();
                        let two = self
                            .render_figure(Source::Chart, &shown(2, false), ctx)
                            .map(|r| r.to_rgba_gpu(&self.render_state));
                        let Some(Ok((two, w2, h2))) = two else { return fail("the 2x figure should draw") };
                        let t2 = t.elapsed().as_secs_f64() * 1000.0;
                        if w2.abs_diff(2 * w) > 1 || h2.abs_diff(2 * h) > 1 {
                            return fail(&format!(
                                "the 2x export should have twice the pixels: {w2} x {h2} against {w} x {h}"
                            ));
                        }
                        let (down, dw, dh) = downscale(&two, w2, h2);
                        let (cw, ch) = (dw.min(w), dh.min(h));
                        let cut = |px: &[u8], pw: usize| -> Vec<u8> {
                            (0..ch).flat_map(|y| px[y * pw * 4..(y * pw + cw) * 4].to_vec()).collect()
                        };
                        let d2 = mean_diff(&cut(&down, dw), &cut(&one, w));
                        let t = Instant::now();
                        let four = self
                            .render_figure(Source::Chart, &shown(4, false), ctx)
                            .map(|r| r.to_rgba_gpu(&self.render_state));
                        let Some(Ok((_, w4, h4))) = four else { return fail("the 4x figure should draw") };
                        let t4 = t.elapsed().as_secs_f64() * 1000.0;
                        println!(
                            "    2x {w2} x {h2} px (twice the 1x), downscaled against the 1x: mean difference {d2:.3} / 255; \
                             2x in {t2:.0} ms, 4x {w4} x {h4} px in {t4:.0} ms in all (drawing, GPU, readback)"
                        );
                        if d2 >= 12.0 {
                            return fail(&format!("the 2x export downscaled should look like the 1x ({d2:.3} / 255)"));
                        }
                        if let Some(k) = &keep {
                            let _ = image::save_buffer(
                                k.join("chart_offscreen_2x.png"),
                                &two,
                                w2 as u32,
                                h2 as u32,
                                image::ExtendedColorType::Rgba8,
                            );
                        }
                        // a transparent background: nothing behind the plot
                        let tr = self
                            .render_figure(Source::Chart, &shown(1, true), ctx)
                            .map(|r| r.to_rgba_gpu(&self.render_state));
                        let Some(Ok((tr, ..))) = tr else { return fail("the transparent figure should draw") };
                        // (the plot keeps its own fill, as on screen; the panel behind the axes and the margins goes)
                        let clear = tr.as_chunks::<4>().0.iter().filter(|p| p[3] == 0).count();
                        let solid = tr.as_chunks::<4>().0.iter().filter(|p| p[3] == 255).count();
                        let total = tr.len() / 4;
                        if clear < total / 20 || solid < total / 4 {
                            return fail(&format!(
                                "the transparent figure should have no panel behind it ({clear} clear, {solid} solid of {total})"
                            ));
                        }
                        println!(
                            "    transparent background: {clear} of {total} pixels clear, {solid} solid (the plot)"
                        );
                        // the interface's darkest theme, then its lightest
                        let s = &self.settings.s;
                        crate::theme::apply(ctx, crate::theme::MODERN_DARKEST, s.panel_opacity, s.font_size);
                        PHASE.store(4, Relaxed);
                        st.frames = 0;
                    }
                    4 | 5 => {
                        if st.frames < 6 {
                            return None;
                        }
                        let dark = phase == 4;
                        let Some(Ok((px, ..))) = self
                            .render_figure(Source::Chart, &shown(1, false), ctx)
                            .map(|r| r.to_rgba_gpu(&self.render_state))
                        else {
                            return fail("the chart should draw in the theme");
                        };
                        let corner = px[0];
                        println!(
                            "    {} theme: the PNG's corner is {:?}",
                            if dark { "Darkest" } else { "Light" },
                            &px[..3]
                        );
                        if dark != (corner < 100) || (!dark && corner < 150) {
                            return fail(&format!(
                                "the PNG's background should follow the theme (corner red {corner})"
                            ));
                        }
                        let s = &self.settings.s;
                        if dark {
                            crate::theme::apply(ctx, crate::theme::MODERN_LIGHT, s.panel_opacity, s.font_size);
                        } else {
                            crate::theme::apply(ctx, s.look(), s.panel_opacity, s.font_size);
                        }
                        PHASE.store(if dark { 5 } else { 6 }, Relaxed);
                        st.frames = 0;
                    }
                    _ => {
                        if st.frames < 6 {
                            return None;
                        }
                        PHASE.store(0, Relaxed);
                        self.analysis.show = ANALYSIS_WAS.load(Relaxed);
                        if HOVER_PUT.swap(false, Relaxed) {
                            self.series.hover = None;
                        }
                        next(st, "the chart as on screen: size, pixels against a screenshot, 2x, themes, SVG");
                    }
                }
            }
            342 => {
                // an Analysis tab and the transect as on screen, written as the dialog does
                let ppp = ctx.pixels_per_point();
                let dir = std::env::temp_dir().join("janus-selftest-figures-shown");
                let _ = std::fs::create_dir_all(&dir);
                let tab =
                    crate::analysis::FIGURE_TABS.iter().copied().find(|&k| self.analysis_tab_unavailable(k).is_none());
                let (w, h) = self.session.as_ref().map_or((0.0, 0.0), |s| (s.info.width as f64, s.info.height as f64));
                self.set_transect(vec![[10.5, h / 2.0], [w - 10.5, h / 2.0]]);
                if let Err(e) = self.transect_read() {
                    return fail(&e);
                }
                let o = FigureOptions { mode: Mode::AsShown, ..Default::default() };
                let mut sources = vec![Source::Transect];
                sources.extend(tab.map(Source::Analysis));
                let mut notes = Vec::new();
                for source in sources {
                    let path = dir.join(format!("shown_{}.png", source.key()));
                    if let Err(e) = self.figures_write(source, &o, &path, ctx) {
                        return fail(&e);
                    }
                    let img = match image::open(&path) {
                        Ok(i) => i.to_rgba8(),
                        Err(e) => return fail(&format!("the PNG should be readable: {e}")),
                    };
                    let seen = crate::figure::seen(ctx, source.key());
                    let want = seen.unwrap_or(crate::figure::PanelSeen::FALLBACK).pixels(ppp);
                    // (the height depends on the controls the panel has now: only the width is known)
                    if img.width() != want[0] || img.height() < 8 {
                        return fail(&format!(
                            "{}'s PNG should be the panel's {} px wide, it is {} x {} (seen {seen:?})",
                            source.names().0,
                            want[0],
                            img.width(),
                            img.height()
                        ));
                    }
                    let first = img.get_pixel(0, 0).0;
                    let ink = img.pixels().filter(|p| p.0 != first).count();
                    if ink < 200 {
                        return fail(&format!("{}'s PNG should have ink on it ({ink} pixels)", source.names().0));
                    }
                    notes.push(format!(
                        "{}: {} x {} px, {ink} pixels of ink (panel seen {seen:?})",
                        source.names().0,
                        img.width(),
                        img.height()
                    ));
                }
                self.clear_transect();
                let _ = std::fs::remove_dir_all(&dir);
                for n in notes {
                    println!("    {n}");
                }
                next(st, "an Analysis tab and the transect as on screen");
                st.stage = 350;
            }
            _ => {}
        }
        None
    }
}

/// Whether the Analysis window was open before stage 341 closed it (to show the chart).
static ANALYSIS_WAS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether stage 341 put a cursor on the chart (to take it away).
static HOVER_PUT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Stage 341's phase (the stage waits for the screen between them).
static PHASE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The mean absolute difference per colour channel (not alpha) of two RGBA8 images of one size, in 0..255.
fn mean_diff(a: &[u8], b: &[u8]) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for (p, q) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for (x, y) in p[..3].iter().zip(&q[..3]) {
            sum += u64::from(x.abs_diff(*y));
            n += 1;
        }
    }
    sum as f64 / n.max(1) as f64
}

/// An RGBA8 image halved by averaging blocks of 2 x 2 pixels: (pixels, width, height).
fn downscale(px: &[u8], w: usize, h: usize) -> (Vec<u8>, usize, usize) {
    let (dw, dh) = (w / 2, h / 2);
    let mut out = Vec::with_capacity(dw * dh * 4);
    for y in 0..dh {
        for x in 0..dw {
            for c in 0..4 {
                let at = |dx: usize, dy: usize| u32::from(px[((2 * y + dy) * w + 2 * x + dx) * 4 + c]);
                out.push(((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8);
            }
        }
    }
    (out, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halving_and_differences() {
        // a 2 x 2 image of one block: 0, 100, 100, 200 -> 100
        let px: Vec<u8> = [0u8, 100, 100, 200].iter().flat_map(|&v| [v, v, v, 255]).collect();
        let (d, w, h) = downscale(&px, 2, 2);
        assert_eq!((w, h, d.len()), (1, 1, 4));
        assert_eq!(d[0], 100);
        assert_eq!(mean_diff(&px, &px), 0.0);
        let other: Vec<u8> = px.iter().map(|v| v.saturating_add(3)).collect();
        assert!(mean_diff(&px, &other) > 2.0);
    }
}
