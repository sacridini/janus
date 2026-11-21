//! The Noise and change guide's screenshots (`--shots envi LANDSAT OUT`; site/envi.html), on real data. LANDSAT is a
//! folder of six Landsat 8/9 Collection 2 Level-2 dates of central Rondonia, one GeoTIFF a date (blue to SWIR2 and
//! QA_PIXEL, 1690 x 1675 px of 30 m): the July of 2020 to 2025 (tools/make_envi_data.py cuts them from the CCDC
//! guide's folder). The radar part reads a Sentinel-1 RTC series of 8.8 km of the south of Rondonia where forest was cleared in 2022 to 2024, about 10.7 S and 62.8 W (one GeoTIFF
//! a date, VV and VH in linear power, 2022 to 2024) from the folder `JANUS_ENVI_S1` (tools/make_envi_data.py makes it). The tour
//! runs the MNF, the change vector analysis, the iMAD and the radiometric normalization on the Landsat series, the
//! CVA again on the original and on the normalized dates, then the omnibus test on the radar, takes the pictures the
//! page names and prints the numbers it quotes. The full-window pictures are written as PNG (the guide keeps them as
//! JPEG), the windows and panels as cropped PNG, and the maps that go side by side as parts (`parts/`) that
//! tools/make_envi_figures.py puts together. A pixel for the radar's chart is picked from the maps (one change at the
//! commonest date, in the commonest direction), or given with `JANUS_ENVI_PIN=x,y` and `JANUS_ENVI_STABLE=x,y`.

use std::path::{Path, PathBuf};

use egui::Context;
use janus_cube::BandSelection;
use janus_zeit::manifest;
use serde_json::{Value, json};

use crate::app::{State, Tab};
use crate::session::Opening;
use crate::shots::{Shots, Target, tool_tab};
use crate::zeit::JobState;

const TOOLS: [&str; 5] = ["mnf", "cva", "imad", "radcal", "omnibus"];

/// The blocks' first stages; a block ends by jumping to the next one's.
const MNF: usize = 10;
const CVA: usize = 30;
const IMAD: usize = 50;
const RADCAL: usize = 70;
const CHAIN: usize = 90;
const RADAR: usize = 110;
const END: usize = 200;

/// The pixels of the radiometric normalization's chart: a field and a forest that its own iMAD calls invariant (a
/// probability of no change of 0.95 or more between the first and the last date, with the other standardization), in
/// uniform 5 x 5 neighbourhoods, the field's NDVI between 0.3 and 0.7 on average (the one nearest the middle of the
/// image of those the normalization improves).
const FIELD: (usize, usize) = (828, 880);
const FOREST: (usize, usize) = (897, 833);
/// The date of the Landsat series the haze of the 2024 fires is in (index).
const HAZY: usize = 4;
/// The clearest date (index): the reference of the normalization.
const CLEAR: usize = 3;
/// Where the MNF's comparison looks: a forest, as the pixel x and y of its middle and the screen points a pixel takes.
const ZOOM: (f64, f64, f64) = (848.0, 820.0, 6.0);

/// What the tour keeps between its stages.
#[derive(Default)]
pub struct EnviShots {
    /// the Landsat series' layer, the normalized series', the layers of the MNF's comparison
    landsat: usize,
    normalized: usize,
    den: usize,
    orig: usize,
    /// the layers before a run that opens a series
    layers: usize,
    /// the runs' folders by name
    runs: Vec<(String, String)>,
    /// the radar's pins: one change, none
    pin: (usize, usize),
    stable: (usize, usize),
    /// the range of values the MNF's comparison draws both maps in
    range: (f32, f32),
}

fn go(sh: &mut Shots, done: &str, to: usize) {
    sh.next(done);
    sh.stage = to;
}

fn tabs(sh: &mut Shots, under: f32) {
    sh.under = under;
    sh.layout = Some((TOOLS.iter().map(|t| tool_tab(t)).collect(), vec![Tab::Tasks]));
}

fn cat(run: &str, file: &str) -> String {
    std::fs::read_to_string(Path::new(run).join(file)).unwrap_or_default()
}

impl State {
    fn envi_run(&self, sh: &Shots, key: &str) -> String {
        sh.envi.runs.iter().find(|r| r.0 == key).map(|r| r.1.clone()).unwrap_or_default()
    }

    /// The map of a run whose file ends with `file`.
    fn envi_result(&self, run: &str, file: &str) -> Option<&crate::zeit::ResultLayer> {
        self.zeit.results.iter().find(|r| r.run == run && r.path.replace('\\', "/").ends_with(file))
    }

    /// A tool's window: opened with `params` (frame 1), brought to the front (3), its picture once drawn.
    fn envi_window(&mut self, sh: &mut Shots, id: &str, params: Value, file: &str) -> bool {
        if sh.frames == 1 {
            self.shots_open_tool(id, params);
            self.zeit.show_tasks = false;
            return false;
        }
        if sh.frames == 3 {
            sh.focus.push(tool_tab(id));
        }
        if !self.shots_quiet() || sh.time() < 6.0 {
            return false;
        }
        sh.shoot("-", &[(file, Shots::fit(tool_tab(id)))]);
        true
    }

    /// The run `key` of tool `id` done: its folder noted and printed with its maps, the window closed, no map shown.
    fn envi_finish(&mut self, sh: &mut Shots, key: &str, id: &str) -> Option<String> {
        let job = sh.job.and_then(|j| self.zeit.jobs.get(j))?;
        let run = job.output_dir.clone();
        println!("    {key}: {:.1} s, folder {run}", job.status.lock().unwrap().seconds);
        for r in self.zeit.results.iter().filter(|r| r.run == run) {
            println!("      map {:?} ({} x {}, {:?})", r.name, r.w, r.h, r.classes);
        }
        sh.envi.runs.push((key.to_string(), run.clone()));
        // the small tables the figures and the text use, next to the pictures
        let data = sh.out.join("data");
        let _ = std::fs::create_dir_all(&data);
        for f in ["mnf_eigenvalues.csv", "cva_sectors.csv", "cva.json", "imad_rho.csv", "radcal_fits.csv"] {
            let _ = std::fs::copy(Path::new(&run).join(f), data.join(format!("{key}-{f}")));
        }
        self.zeit.show_tasks = false;
        if let Some(t) = manifest().tool(id) {
            self.set_tool_open(t, false);
        }
        self.shots_hide_results();
        Some(run)
    }

    /// Shows the run's first map `pred` accepts alone (frame 1) and takes its picture once drawn (Ok(true)); Err when
    /// the run has no such map.
    #[allow(clippy::too_many_arguments)]
    fn envi_picture(
        &mut self,
        sh: &mut Shots,
        run: &str,
        pred: impl Fn(&str) -> bool,
        name: &str,
        crops: &[(&str, Target)],
        settle: f64,
        room: bool,
    ) -> Result<bool, String> {
        if sh.frames == 1 {
            let mut found = false;
            for r in &mut self.zeit.results {
                let hit = !found && r.run == run && pred(&r.name);
                r.visible = hit;
                found |= hit;
            }
            self.map.dirty = true;
            self.map.fit();
            return if found { Ok(false) } else { Err("the run has no such map".into()) };
        }
        if sh.frames == 12 && room {
            // a class map's legend lies over the foot of the map: the image smaller, moved up
            let (c, s) = (self.map.center, self.map.scale);
            self.map.set_view([c[0], c[1] + 58.0 / (s * 0.9)], s * 0.9);
        }
        if sh.frames < 24 || !self.shots_quiet() || sh.time() < settle {
            return Ok(false);
        }
        sh.shoot(name, crops);
        Ok(true)
    }

    /// Whether the layers have settled after a run opened a series: nothing opening, the last layer loaded.
    fn envi_series_open(&self, sh: &Shots, extra: usize) -> bool {
        self.zeit.series_to_open.is_empty()
            && self.opening.is_none()
            && self.layer_jobs.queue.is_empty()
            && self.layers.len() == sh.envi.layers + extra
            && crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
    }

    /// The map's valid values, as a vector.
    fn envi_values(&self, run: &str, file: &str) -> Vec<f32> {
        self.envi_result(run, file).map(|m| m.data.clone()).unwrap_or_default()
    }

    /// Prints the shares of the pixels with a change (the radar's maps) and picks the pins of its chart.
    fn envi_radar_pins(&self, run: &str) -> Option<((usize, usize), (usize, usize))> {
        let first = self.envi_result(run, "omnibus_first_change.tif")?;
        let (w, h) = (first.w, first.h);
        let n = self.envi_values(run, "omnibus_n_changes.tif");
        let dir = self.envi_values(run, "omnibus_first_direction.tif");
        let when = self.envi_values(run, "omnibus_first_change.tif");
        let p = self.envi_values(run, "omnibus_p_change.tif");
        let valid: Vec<usize> = (0..n.len()).filter(|&i| n[i].is_finite()).collect();
        let hist: Vec<usize> = (0..6).map(|k| valid.iter().filter(|&&i| n[i] as usize == k).count()).collect();
        let changed = valid.iter().filter(|&&i| n[i] >= 1.0).count();
        println!(
            "    omnibus: {} valid pixels of {}; with a change {} ({:.1} %); n_changes 0..5: {hist:?}, more: {}",
            valid.len(),
            n.len(),
            changed,
            100.0 * changed as f64 / valid.len().max(1) as f64,
            valid.iter().filter(|&&i| n[i] >= 6.0).count()
        );
        let mut by_dir = [0usize; 4];
        for &i in &valid {
            if when[i].is_finite() {
                by_dir[(dir[i] as usize).saturating_sub(1).min(3)] += 1;
            }
        }
        println!("    first-change directions (none, brightening, darkening, indefinite): {by_dir:?}");
        // the commonest date and direction of the pixels with one change
        let one: Vec<usize> = valid.iter().copied().filter(|&i| n[i] == 1.0).collect();
        let mut dates: Vec<f32> = one.iter().map(|&i| when[i]).collect();
        dates.sort_by(f32::total_cmp);
        dates.dedup();
        let count = |d: f32| one.iter().filter(|&&i| when[i] == d).count();
        let top = dates.iter().copied().max_by_key(|&d| count(d)).unwrap_or(f32::NAN);
        let info = self.session.as_ref()?.info.clone();
        let label = |y: f32| {
            (0..info.t())
                .min_by(|&a, &b| {
                    (info.decimal_year(a) - f64::from(y)).abs().total_cmp(&(info.decimal_year(b) - f64::from(y)).abs())
                })
                .map(|i| info.layers[i].label.clone())
                .unwrap_or_default()
        };
        println!(
            "    pixels with one change: {}; commonest date {} = {top} ({} of them)",
            one.len(),
            label(top),
            count(top)
        );
        let mode_dir = (1..=4)
            .max_by_key(|&d| one.iter().filter(|&&i| when[i] == top && dir[i] as usize == d).count())
            .unwrap_or(1);
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let dist = |i: usize| ((i % w) as f64 - cx).powi(2) + ((i / w) as f64 - cy).powi(2);
        let mut c: Vec<usize> =
            one.iter().copied().filter(|&i| when[i] == top && dir[i] as usize == mode_dir && p[i] < 1e-4).collect();
        c.sort_by(|&a, &b| dist(a).total_cmp(&dist(b)));
        // a pixel whose neighbours changed too, so that it is in a patch and not a speck
        let patch = |i: usize| {
            let (x, y) = (i % w, i / w);
            (x >= 2 && y >= 2 && x + 2 < w && y + 2 < h)
                && (-2i32..=2).all(|dy| {
                    (-2i32..=2).all(|dx| {
                        let j = ((y as i32 + dy) as usize) * w + (x as i32 + dx) as usize;
                        n[j] >= 1.0
                    })
                })
        };
        let pin = c.iter().copied().find(|&i| patch(i)).or(c.first().copied())?;
        // the stable pixel: no change itself, in the 61 x 61 block with the fewest pixels that changed (the one nearest the
        // middle among equals)
        let changed = |j: usize| usize::from(n[j] >= 1.0);
        let mut sum = vec![0usize; (w + 1) * (h + 1)];
        for y in 0..h {
            for x in 0..w {
                sum[(y + 1) * (w + 1) + x + 1] =
                    changed(y * w + x) + sum[y * (w + 1) + x + 1] + sum[(y + 1) * (w + 1) + x] - sum[y * (w + 1) + x];
            }
        }
        let r = 30usize;
        let block = |i: usize| {
            let (x, y) = (i % w, i / w);
            let (x0, y0, x1, y1) = (x.saturating_sub(r), y.saturating_sub(r), (x + r + 1).min(w), (y + r + 1).min(h));
            sum[y1 * (w + 1) + x1] + sum[y0 * (w + 1) + x0] - sum[y0 * (w + 1) + x1] - sum[y1 * (w + 1) + x0]
        };
        let mut s: Vec<usize> = valid.iter().copied().filter(|&i| n[i] == 0.0).collect();
        s.sort_by(|&a, &b| block(a).cmp(&block(b)).then(dist(a).total_cmp(&dist(b))));
        let stable = s.first().copied()?;
        println!(
            "    pin ({}, {}): first change {}, direction {mode_dir}, p {:.2e}; stable pixel ({}, {})",
            pin % w,
            pin / w,
            when[pin],
            p[pin],
            stable % w,
            stable / w
        );
        Some(((pin % w, pin / w), (stable % w, stable / w)))
    }

    pub(crate) fn shots_envi(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL (envi, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        // the runs (and the radar's download) take as long as they take
        let slow = matches!(sh.stage, 14 | 15 | 34 | 35 | 54 | 55 | 63 | 64 | 74 | 75 | 93 | 94 | 96 | 97 | 114 | 115);
        if t > if slow { 7200.0 } else { 300.0 } {
            return fail(sh, "timeout");
        }
        // (no notice of a task that ended over a window or the chart)
        self.clear_notices();
        let landsat = sh.envi.landsat;
        match sh.stage {
            // ---- the data --------------------------------------------------------------------------------------
            0 => {
                let cache = std::env::var_os("JANUS_SHOTS_CACHE")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| std::env::temp_dir().join("janus-envi-shots"));
                let _ = std::fs::remove_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(cache.join("results"));
                let _ = std::fs::create_dir_all(sh.out.join("parts"));
                self.zeit.results_dir = Some(cache.join("results"));
                self.shots_open_landsat(sh, ctx);
                sh.next("the Landsat series opening");
            }
            1 => {
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                tabs(sh, 0.68);
                self.shots_colormap("NDVI");
                let info = self.session.as_ref()?.info.clone();
                self.set_date(info.t() - 1);
                sh.envi.landsat = self.active;
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    series: {} x {} px, {} dates {labels:?}, {} bands a date {:?}, geotransform {:?}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    info.bands_per_date,
                    info.band_names,
                    info.geo_transform,
                    info.crs_name
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                sh.next("the series open");
            }
            2 => {
                if sh.frames == 1 {
                    // two pins, so that the chart has something to say: a bare surface and a forest
                    self.add_pin(FIELD.0, FIELD.1);
                    self.add_pin(FOREST.0, FOREST.1);
                }
                if sh.frames == 30 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("01-landsat-series", &[]);
                go(sh, "the series at its last date (NDVI)", MNF);
            }
            // ---- the MNF ---------------------------------------------------------------------------------------
            10 => {
                if sh.frames == 1 {
                    self.clear_pins();
                }
                let info = self.session.as_ref()?.info.clone();
                let date = info.decimal_year(info.t() - 1);
                if self.envi_window(
                    sh,
                    "mnf",
                    json!({ "date": date, "output": "both", "snr_min": 3.0 }),
                    "02-mnf-window",
                ) {
                    sh.next("the MNF's window");
                    sh.stage = MNF + 4;
                }
            }
            14 => {
                sh.envi.layers = self.layers.len();
                match self.shots_run_stage(sh, "mnf", 0, None, ctx) {
                    1 => return fail(sh, &format!("the MNF did not start: {:?}", self.error_popup)),
                    0 => sh.next("the MNF running"),
                    _ => {}
                }
            }
            15 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the MNF failed: {}", self.shots_job_error(sh)));
                }
            }
            16 => {
                // the denoised bands open as a series of their own
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "mnf", "mnf")?;
                    println!("    eigenvalues:\n{}", cat(&run, "mnf_eigenvalues.csv"));
                    return None;
                }
                if !self.envi_series_open(sh, 1) {
                    return if t > 600.0 { fail(sh, "the denoised series should open") } else { None };
                }
                sh.envi.den = self.layers.len() - 1;
                println!("    denoised layer {:?}", self.layers[sh.envi.den].name);
                self.layers[sh.envi.den].visible = false;
                self.set_active(landsat, ctx);
                // the blue of the same dates, as it is, for the comparison
                let sel = BandSelection { band: 1, ..Default::default() };
                let input = sh.input.clone();
                self.opening = Some(Opening::start_with(vec![input], true, sel, ctx));
                sh.next("the denoised bands open as a series");
            }
            17 => {
                if !self.envi_series_open(sh, 2) {
                    return if t > 600.0 { fail(sh, "the blue series should open") } else { None };
                }
                sh.envi.orig = self.layers.len() - 1;
                println!("    original blue layer {:?}", self.layers[sh.envi.orig].name);
                sh.next("the blue series open");
            }
            18..=22 => {
                // the components: MNF 1 with the window (the Layers panel names their SNR), then the others, as parts
                let k = sh.stage - 17;
                let run = self.envi_run(sh, "mnf");
                if sh.frames == 1 && k == 1 {
                    for l in &mut self.layers {
                        l.visible = false;
                    }
                    self.set_active(landsat, ctx);
                    tabs(sh, 0.68);
                }
                let (whole, part) = if k == 1 {
                    ("-", "parts/03a-mnf1".to_string())
                } else {
                    ("-", format!("parts/03{}-mnf{k}", (b'a' + k as u8 - 1) as char))
                };
                let want = format!("MNF {k}:");
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.starts_with(&want) || n.contains(&format!(": {want}")),
                    whole,
                    &[(&part, Shots::tab(Tab::Map))],
                    4.0,
                    false,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next(&part),
                    Ok(false) => {}
                }
            }
            23 => {
                let run = self.envi_run(sh, "mnf");
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.contains("MNF 6:"),
                    "-",
                    &[("parts/03f-mnf6", Shots::tab(Tab::Map))],
                    4.0,
                    false,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next("MNF 6"),
                    Ok(false) => {}
                }
            }
            24 => {
                // the blue as it is, zoomed on a forest, then rebuilt from the components kept
                if sh.frames == 1 {
                    self.shots_hide_results();
                    for l in &mut self.layers {
                        l.visible = false;
                    }
                    let k = sh.envi.orig;
                    self.layers[k].visible = true;
                    self.set_active(k, ctx);
                    self.shots_colormap("Greys");
                    self.set_date(5);
                    tabs(sh, 0.68);
                    return None;
                }
                if sh.frames == 20 {
                    self.shots_zoom_to(ZOOM.0, ZOOM.1, ZOOM.2);
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("-", &[("parts/04a-blue-original", Shots::tab(Tab::Map))]);
                sh.next("the blue as it is, zoomed");
            }
            25 => {
                if sh.frames == 1 {
                    let (o, d) = (sh.envi.orig, sh.envi.den);
                    // the same range of values on both
                    let range = self.display.ranges[janus_gpu::Mode::Value as usize];
                    sh.envi.range = (range.lo, range.hi);
                    self.layers[o].visible = false;
                    self.layers[d].visible = true;
                    self.set_active(d, ctx);
                    self.shots_colormap("Greys");
                    self.set_date(0);
                    println!("    blue range {} to {}", range.lo, range.hi);
                    return None;
                }
                if sh.frames == 6 {
                    // (the stack's own stretch of this band is taken when it is first drawn: set after that)
                    let m = janus_gpu::Mode::Value as usize;
                    let (lo, hi) = sh.envi.range;
                    self.display.ranges[m] = crate::display::Range::fixed(lo, hi);
                    if let Some(b) = self.display.band_ranges.get_mut(0) {
                        *b = crate::display::Range::fixed(lo, hi);
                    }
                    self.map.dirty = true;
                }
                if sh.frames < 30 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("-", &[("parts/04b-blue-denoised", Shots::tab(Tab::Map))]);
                sh.next("the blue rebuilt from the components kept");
            }
            26 => {
                // back to the Landsat series alone
                for k in (sh.envi.den.min(sh.envi.orig)..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                self.set_active(landsat, ctx);
                self.layers[landsat].visible = true;
                self.shots_colormap("NDVI");
                let n = self.session.as_ref()?.info.t();
                self.set_date(n - 1);
                self.map.fit();
                go(sh, "the comparison's layers removed", CVA);
            }
            // ---- the change vector analysis ---------------------------------------------------------------------
            30 => {
                if self.envi_window(sh, "cva", json!({}), "04-cva-window") {
                    sh.next("the CVA's window");
                    sh.stage = CVA + 4;
                }
            }
            34 => match self.shots_run_stage(sh, "cva", 0, None, ctx) {
                1 => return fail(sh, &format!("the CVA did not start: {:?}", self.error_popup)),
                0 => sh.next("the CVA running"),
                _ => {}
            },
            35 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the CVA failed: {}", self.shots_job_error(sh)));
                }
            }
            36 => {
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "cva", "cva")?;
                    println!("    cva.json: {}\n    sectors:\n{}", cat(&run, "cva.json"), cat(&run, "cva_sectors.csv"));
                    self.layers[landsat].visible = false;
                    tabs(sh, 0.68);
                    return None;
                }
                sh.next("the CVA's folder read");
            }
            37 => {
                let run = self.envi_run(sh, "cva");
                match self.envi_picture(sh, &run, |n| n.contains("magnitude"), "05-cva-magnitude", &[], 5.0, false) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next("the CVA's magnitude"),
                    Ok(false) => {}
                }
            }
            38 => {
                let run = self.envi_run(sh, "cva");
                match self.envi_picture(sh, &run, |n| n.contains("sectors"), "06-cva-sectors", &[], 5.0, true) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => go(sh, "the CVA's sectors", IMAD),
                    Ok(false) => {}
                }
            }
            // ---- iMAD ------------------------------------------------------------------------------------------
            50 => {
                if sh.frames == 1 {
                    self.set_active(landsat, ctx);
                }
                if self.envi_window(sh, "imad", json!({}), "07-imad-window") {
                    sh.next("the iMAD's window");
                    sh.stage = IMAD + 4;
                }
            }
            54 => match self.shots_run_stage(sh, "imad", 0, None, ctx) {
                1 => return fail(sh, &format!("the iMAD did not start: {:?}", self.error_popup)),
                0 => sh.next("the iMAD running"),
                _ => {}
            },
            55 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the iMAD failed: {}", self.shots_job_error(sh)));
                }
            }
            56 => {
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "imad", "imad")?;
                    println!(
                        "    imad_rho.csv:\n{}\n    imad.json: {}",
                        cat(&run, "imad_rho.csv"),
                        cat(&run, "imad.json")
                    );
                    let change = self.envi_values(&run, "imad_change.tif");
                    let valid = change.iter().filter(|v| v.is_finite()).count();
                    let flagged = change.iter().filter(|&&v| v == 2.0).count();
                    println!(
                        "    iMAD change: {flagged} of {valid} valid pixels ({:.1} %)",
                        100.0 * flagged as f64 / valid.max(1) as f64
                    );
                    self.layers[landsat].visible = false;
                    tabs(sh, 0.68);
                    return None;
                }
                sh.next("the iMAD's folder read");
            }
            57..=59 => {
                // MAD 1 to 3, as parts
                let k = sh.stage - 56;
                let run = self.envi_run(sh, "imad");
                let part = format!("parts/08{}-mad{k}", (b'a' + k as u8 - 1) as char);
                let want = format!("MAD {k}");
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.contains(&want),
                    "-",
                    &[(&part, Shots::tab(Tab::Map))],
                    4.0,
                    false,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next(&part),
                    Ok(false) => {}
                }
            }
            60 | 61 => {
                // the probability of no change, and the change map
                let run = self.envi_run(sh, "imad");
                let (suffix, part) = if sh.stage == 60 {
                    ("Probability of no change", "parts/09a-pnochange")
                } else {
                    ("Change (iMAD)", "parts/09b-change")
                };
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.contains(suffix),
                    "-",
                    &[(part, Shots::tab(Tab::Map))],
                    4.0,
                    false,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) if sh.stage == 60 => sh.next(part),
                    Ok(true) => go(sh, part, IMAD + 12),
                    Ok(false) => {}
                }
            }
            62 => {
                // the same with the other standardization (the text's caveat): its count of changed pixels only
                if sh.frames == 1 {
                    self.set_active(landsat, ctx);
                    self.shots_open_tool("imad", json!({ "standardize": "all" }));
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.next("the iMAD with standardize=all");
            }
            63 => match self.shots_run_stage(sh, "imad", 0, None, ctx) {
                1 => return fail(sh, &format!("the iMAD did not start: {:?}", self.error_popup)),
                0 => sh.next("the iMAD with standardize=all running"),
                _ => {}
            },
            64 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the iMAD failed: {}", self.shots_job_error(sh)));
                }
            }
            65 => {
                let run = self.envi_finish(sh, "imad_all", "imad")?;
                let change = self.envi_values(&run, "imad_change.tif");
                let valid = change.iter().filter(|v| v.is_finite()).count();
                let flagged = change.iter().filter(|&&v| v == 2.0).count();
                println!(
                    "    iMAD standardize=all: change {flagged} of {valid} valid pixels ({:.1} %); rho {}",
                    100.0 * flagged as f64 / valid.max(1) as f64,
                    cat(&run, "imad_rho.csv").lines().last().unwrap_or_default()
                );
                go(sh, "the iMAD with standardize=all done", RADCAL);
            }
            // ---- the radiometric normalization -------------------------------------------------------------------
            70 => {
                if sh.frames == 1 {
                    self.set_active(landsat, ctx);
                }
                // the clearest date as the reference (the first, 2020-07-29, is hazier: its red is 0.075 against 0.053)
                let reference = self.session.as_ref()?.info.decimal_year(CLEAR);
                if self.envi_window(sh, "radcal", json!({ "reference": reference }), "10-radcal-window") {
                    sh.next("the normalization's window");
                    sh.stage = RADCAL + 4;
                }
            }
            74 => {
                sh.envi.layers = self.layers.len();
                match self.shots_run_stage(sh, "radcal", 0, None, ctx) {
                    1 => return fail(sh, &format!("the normalization did not start: {:?}", self.error_popup)),
                    0 => sh.next("the normalization running"),
                    _ => {}
                }
            }
            75 => {
                // (it writes a series, no map: the run is done when its task is)
                let state = sh.job.and_then(|j| self.zeit.jobs.get(j)).map(|j| j.status.lock().unwrap().state);
                match state {
                    Some(JobState::Done) => sh.next("the normalization done"),
                    Some(JobState::Running) => {}
                    _ => return fail(sh, &format!("the normalization failed: {}", self.shots_job_error(sh))),
                }
            }
            76 => {
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "radcal", "radcal")?;
                    println!(
                        "    radcal_fits.csv:\n{}\n    radcal.json: {}",
                        cat(&run, "radcal_fits.csv"),
                        cat(&run, "radcal.json")
                    );
                    return None;
                }
                if !self.envi_series_open(sh, 1) {
                    return if t > 600.0 { fail(sh, "the normalized series should open") } else { None };
                }
                sh.envi.normalized = self.layers.len() - 1;
                println!("    normalized layer {:?}", self.layers[sh.envi.normalized].name);
                // shown as the NDVI, as the series it comes from is
                let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
                self.reopen_layer(sh.envi.normalized, sel, ctx);
                sh.next("the normalized series opens");
            }
            77 => {
                if self.opening.is_some() || !crate::ui_test::loaded(self.layer_session(sh.envi.normalized)) {
                    return None;
                }
                sh.next("the normalized series shown as the NDVI");
            }
            78 => {
                // the chart of two stable pixels: the series as it was and as normalized
                if sh.frames == 1 {
                    let (o, n) = (landsat, sh.envi.normalized);
                    for k in 0..self.layers.len() {
                        self.layers[k].visible = k == o || k == n;
                    }
                    self.set_active(o, ctx);
                    self.shots_hide_results();
                    self.clear_pins();
                    self.add_pin(FIELD.0, FIELD.1);
                    self.add_pin(FOREST.0, FOREST.1);
                    self.set_chart_layers(1);
                    tabs(sh, 0.45);
                    return None;
                }
                if sh.frames == 30 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 10.0 {
                    return None;
                }
                sh.shoot("-", &[("11-radcal-chart", Shots::tab(Tab::TimeSeries))]);
                go(sh, "the chart of two stable pixels, before and after", CHAIN);
            }
            // ---- CVA before and after the normalization -----------------------------------------------------------
            90 => {
                // on the original dates: the first and the hazy one
                if sh.frames == 1 {
                    self.clear_pins();
                    self.set_chart_layers(0);
                    self.set_active(landsat, ctx);
                    let info = self.session.as_ref()?.info.clone();
                    self.shots_open_tool(
                        "cva",
                        json!({ "before": info.decimal_year(CLEAR), "after": info.decimal_year(HAZY) }),
                    );
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.next("the CVA between the clear and the hazy date, on the original");
                sh.stage = CHAIN + 3;
            }
            93 => {
                sh.envi.layers = self.layers.len();
                match self.shots_run_stage(sh, "cva", 0, None, ctx) {
                    1 => return fail(sh, &format!("the CVA did not start: {:?}", self.error_popup)),
                    0 => sh.next("the CVA on the original running"),
                    _ => {}
                }
            }
            94 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the CVA failed: {}", self.shots_job_error(sh)));
                }
            }
            95 => {
                // the same on the normalized series
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "cva_original", "cva")?;
                    println!("    cva.json: {}\n    sectors:\n{}", cat(&run, "cva.json"), cat(&run, "cva_sectors.csv"));
                    let n = sh.envi.normalized;
                    self.set_active(n, ctx);
                    let info = self.session.as_ref()?.info.clone();
                    self.shots_open_tool(
                        "cva",
                        json!({ "before": info.decimal_year(CLEAR), "after": info.decimal_year(HAZY) }),
                    );
                    self.zeit.show_tasks = false;
                    return None;
                }
                if sh.frames < 8 {
                    return None;
                }
                sh.next("the same on the normalized series");
            }
            96 => match self.shots_run_stage(sh, "cva", 0, None, ctx) {
                1 => return fail(sh, &format!("the CVA did not start: {:?}", self.error_popup)),
                0 => sh.next("the CVA on the normalized running"),
                _ => {}
            },
            97 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the CVA failed: {}", self.shots_job_error(sh)));
                }
            }
            98 => {
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "cva_normalized", "cva")?;
                    println!("    cva.json: {}\n    sectors:\n{}", cat(&run, "cva.json"), cat(&run, "cva_sectors.csv"));
                    // the same colours on both magnitudes
                    let hi = {
                        let mut v: Vec<f32> = self
                            .envi_values(&self.envi_run(sh, "cva_original"), "cva_magnitude.tif")
                            .into_iter()
                            .filter(|x| x.is_finite())
                            .collect();
                        v.sort_by(f32::total_cmp);
                        v.get(v.len() * 99 / 100).copied().unwrap_or(1.0)
                    };
                    for r in &mut self.zeit.results {
                        if r.path.replace('\\', "/").ends_with("cva_magnitude.tif") {
                            r.range = Some([0.0, hi]);
                            (r.lo, r.hi) = (0.0, hi);
                        }
                    }
                    println!("    magnitude drawn from 0 to {hi}");
                    for l in &mut self.layers {
                        l.visible = false;
                    }
                    self.set_active(landsat, ctx);
                    tabs(sh, 0.68);
                    return None;
                }
                sh.next("the second CVA's folder read");
            }
            99..=102 => {
                let (key, suffix, part) = match sh.stage {
                    99 => ("cva_original", "magnitude", "parts/12a-magnitude-original"),
                    100 => ("cva_normalized", "magnitude", "parts/12b-magnitude-normalized"),
                    101 => ("cva_original", "sectors", "parts/13a-sectors-original"),
                    _ => ("cva_normalized", "sectors", "parts/13b-sectors-normalized"),
                };
                let run = self.envi_run(sh, key);
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.contains(suffix),
                    "-",
                    &[(part, Shots::tab(Tab::Map))],
                    4.0,
                    sh.stage >= 101,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) if sh.stage == 102 => go(sh, part, RADAR),
                    Ok(true) => sh.next(part),
                    Ok(false) => {}
                }
            }
            // ---- the omnibus test ----------------------------------------------------------------------------------
            110 => {
                // everything of the Landsat part out, the radar series opened as VH in dB
                let Some(s1) = std::env::var_os("JANUS_ENVI_S1").map(|s| s.to_string_lossy().into_owned()) else {
                    return fail(sh, "JANUS_ENVI_S1 should name the Sentinel-1 folder");
                };
                self.zeit.results.clear();
                self.map.dirty = true;
                let mut sel = BandSelection::default();
                let name = janus_cube::calc::indices::find("VHdB").map_or(String::new(), |i| i.name.clone());
                sel.expr = janus_cube::calc::ExprId::intern(&name, "VHdB");
                self.opening = Some(Opening::start_with(vec![s1], false, sel, ctx));
                sh.next("the radar series opening");
            }
            111 => {
                if self.opening.is_some() || self.session.is_none() {
                    return None;
                }
                if self.layers.len() != 1 {
                    return fail(
                        sh,
                        &format!("the radar series should replace the Landsat ones: {} layers", self.layers.len()),
                    );
                }
                tabs(sh, 0.68);
                let info = self.session.as_ref()?.info.clone();
                self.set_date(info.t() / 2);
                let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
                println!(
                    "    radar series: {} x {} px, {} dates {} to {}, {} bands a date {:?}, {}, {}",
                    info.width,
                    info.height,
                    info.t(),
                    labels.first().unwrap_or(&""),
                    labels.last().unwrap_or(&""),
                    info.bands_per_date,
                    info.band_names,
                    info.crs_name,
                    info.description
                );
                println!("    roles: {:?}", crate::zeit::guess_roles(&info));
                sh.next("the radar series open");
            }
            112 => {
                if sh.frames == 30 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 4.0 {
                    return None;
                }
                sh.shoot("14-radar-series", &[]);
                sh.next("the radar series (VH in dB) before the test");
            }
            113 => {
                // the window, then the run on the whole image
                if sh.frames == 1 {
                    self.shots_open_tool("omnibus", json!({ "per_date": true }));
                    self.zeit.show_tasks = false;
                    self.clear_pins();
                    self.zeit.fit_only(None);
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(tool_tab("omnibus"));
                }
                if !self.shots_quiet() || t < 6.0 {
                    return None;
                }
                sh.shoot("-", &[("15-omnibus-window", Shots::fit(tool_tab("omnibus")))]);
                sh.next("the omnibus window");
                sh.stage = RADAR + 4;
            }
            114 => {
                sh.envi.layers = self.layers.len();
                match self.shots_run_stage(sh, "omnibus", 0, None, ctx) {
                    1 => return fail(sh, &format!("the omnibus test did not start: {:?}", self.error_popup)),
                    0 => sh.next("the omnibus test running"),
                    _ => {}
                }
            }
            115 => {
                if self.shots_done_stage(sh) > 0 {
                    return fail(sh, &format!("the omnibus test failed: {}", self.shots_job_error(sh)));
                }
            }
            116 => {
                if sh.frames == 1 {
                    let run = self.envi_finish(sh, "omnibus", "omnibus")?;
                    println!("    omnibus.json: {}", cat(&run, "omnibus.json"));
                    let Some((pin, stable)) = self.envi_radar_pins(&run) else {
                        return fail(sh, "no pixel to pin");
                    };
                    let env = |k: &str| {
                        std::env::var(k).ok().and_then(|v| {
                            let mut it = v.split(',').filter_map(|x| x.trim().parse::<usize>().ok());
                            Some((it.next()?, it.next()?))
                        })
                    };
                    sh.envi.pin = env("JANUS_ENVI_PIN").unwrap_or(pin);
                    sh.envi.stable = env("JANUS_ENVI_STABLE").unwrap_or(stable);
                    return None;
                }
                if !self.envi_series_open(sh, 1) {
                    return if t > 600.0 { fail(sh, "the series of directions should open") } else { None };
                }
                let k = self.layers.len() - 1;
                self.layers[k].visible = false;
                self.set_active(0, ctx);
                sh.next("the series of directions open (hidden)");
            }
            117 => {
                // the maps, over the radar series hidden, with the pins' values under them
                if sh.frames == 1 {
                    self.layers[0].visible = false;
                    self.clear_pins();
                    self.add_pin(sh.envi.pin.0, sh.envi.pin.1);
                    self.add_pin(sh.envi.stable.0, sh.envi.stable.1);
                    tabs(sh, 0.62);
                }
                let run = self.envi_run(sh, "omnibus");
                match self.envi_picture(
                    sh,
                    &run,
                    |n| n.contains("First change"),
                    "16-omnibus-first-change",
                    &[],
                    5.0,
                    false,
                ) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next("the first change"),
                    Ok(false) => {}
                }
            }
            118 | 119 => {
                let run = self.envi_run(sh, "omnibus");
                let (suffix, name) = if sh.stage == 118 {
                    ("Number of changes", "17-omnibus-number")
                } else {
                    ("Direction of the first change", "18-omnibus-direction")
                };
                match self.envi_picture(sh, &run, |n| n.contains(suffix), name, &[], 5.0, sh.stage == 119) {
                    Err(e) => return fail(sh, &e),
                    Ok(true) => sh.next(name),
                    Ok(false) => {}
                }
            }
            120 | 121 => {
                // the pin's chart with the change lines, then the stable pixel's
                if sh.frames == 1 {
                    self.shots_hide_results();
                    self.layers[0].visible = true;
                    self.clear_pins();
                    let at = if sh.stage == 120 { sh.envi.pin } else { sh.envi.stable };
                    self.add_pin(at.0, at.1);
                    self.set_chart_layers(0);
                    self.shots_open_tool("omnibus", json!({}));
                    self.zeit.show_tasks = false;
                    self.zeit.fit_only(Some("omnibus"));
                    tabs(sh, 0.5);
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(tool_tab("omnibus"));
                }
                if sh.frames == 30 {
                    self.map.fit();
                }
                if sh.frames < 40 || !self.shots_quiet() || t < 10.0 {
                    return None;
                }
                if sh.stage == 120 {
                    sh.shoot(
                        "19-omnibus-pin",
                        &[("chart", Shots::tab(Tab::TimeSeries)), ("table", Shots::tab(Tab::Statistics))],
                    );
                    sh.next("the omnibus' chart at a pixel that changed");
                } else {
                    sh.shoot("-", &[("22-omnibus-stable", Shots::tab(Tab::TimeSeries))]);
                    go(sh, "the omnibus' chart at a stable pixel", END);
                }
            }
            _ => {
                println!("    runs: {:?}", sh.envi.runs);
                println!("OK");
                return Some(0);
            }
        }
        None
    }
}
