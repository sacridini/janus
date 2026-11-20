//! `--selftest-ui` stages 320-327: the temporal EOF and the lagged correlation (Tools > Temporal dynamics, and Analysis >
//! Modes), on series the test writes itself. "Modes": 60 monthly dates on 48 x 32 pixels, a seasonal cycle, two known
//! patterns (disjoint blobs) times two known components (orthogonal, with the monthly cycle taken out) and noise.
//! "Rain": 12 x 8 pixels in longitudes and latitudes (another CRS than the others: the driver is warped onto the run's
//! grid), one AR(1) series; "RainFine": the same on Modes' grid. "Response": the left half is 0.05 x the rain two
//! months before plus noise, the right half noise. 320 writes and opens them; 321 opens the EOF window and runs 3 modes
//! on the whole image; 322 checks the maps, the components, the variance, North's test, Analysis > Modes and its Show
//! map; 323 the chart's overlay of the mode at a pixel; 324 the lagged correlation of Response with Rain at lags 0 to 4
//! months; 325 its best lag, its false discovery rate, q and the lags' series; 326 the raster calculator's corr()
//! against the tool's r at lag 2; 327 everything closed and restored. With JANUS_EOF_SHOTS=<folder> a screenshot of the
//! Modes tab is written there at two font sizes (for looking at it).

use std::path::{Path, PathBuf};
use std::time::Instant;

use gdal::Metadata as _;
use janus_zeit::manifest::manifest;
use serde_json::{Value, json};

use super::UiTest;
use crate::app::State;
use crate::zeit::JobState;

const T: usize = 60;
const W: usize = 48;
const H: usize = 32;
/// The files (one multiband GeoTIFF each, the dates in the band descriptions).
const FILES: [&str; 4] = ["eofmodes.tif", "eofrain.tif", "eofresponse.tif", "eofrainfine.tif"];
/// The amplitudes of the two patterns.
const AMPLITUDE: [f64; 2] = [2.0, 1.0];

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct EofTest {
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    /// the layers, the active one, Analysis' state, the font size and the Tasks window before the test
    layers: usize,
    active: usize,
    analysis: (bool, usize),
    font: i32,
    tasks: bool,
    /// the series opened so far (of FILES)
    opened: usize,
    phase: u8,
    /// the EOF run's folder and the lagged correlation's
    eof_run: String,
    corr_run: String,
    /// a screenshot being taken: its name, the frames waited, whether it was asked for
    shot: Option<(String, usize, bool)>,
}

/// A small deterministic generator (a linear congruential one) of values in [-1, 1).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// The date of step `t`: the 15th of the month, from 2018-01.
fn label(t: usize) -> String {
    format!("{}-{:02}-15", 2018 + t / 12, t % 12 + 1)
}

/// The spatial pattern `k` at a pixel: a Gaussian blob of a compact support, the two disjoint.
fn pattern(k: usize, x: usize, y: usize) -> f64 {
    let cx = if k == 0 { 12.0 } else { 36.0 };
    let (dx, dy) = (x as f64 - cx, y as f64 - 16.0);
    let d2 = dx * dx + dy * dy;
    if d2 < 100.0 { (-d2 / 32.0).exp() } else { 0.0 }
}

/// The two components in time: their monthly means taken out, the second orthogonal to the first, unit variance
/// (n - 1), so the anomalies of the series are the patterns times these and the EOFs are the patterns.
fn components() -> [Vec<f64>; 2] {
    let tau = 2.0 * std::f64::consts::PI;
    let mut z: Vec<Vec<f64>> = vec![
        (0..T).map(|t| (tau * t as f64 / 7.3).sin()).collect(),
        (0..T).map(|t| (tau * t as f64 / 5.1 + 0.7).cos()).collect(),
    ];
    for v in z.iter_mut() {
        let c: Vec<f64> = (0..12).map(|m| (0..T / 12).map(|k| v[m + 12 * k]).sum::<f64>() / (T / 12) as f64).collect();
        (0..T).for_each(|t| v[t] -= c[t % 12]);
    }
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let d = dot(&z[1], &z[0]) / dot(&z[0], &z[0]);
    let first = z[0].clone();
    z[1].iter_mut().zip(&first).for_each(|(a, b)| *a -= d * b);
    for v in z.iter_mut() {
        let sd = (dot(v, v) / (T - 1) as f64).sqrt();
        v.iter_mut().for_each(|x| *x /= sd);
    }
    let [a, b]: [Vec<f64>; 2] = z.try_into().unwrap();
    [a, b]
}

/// The rain: one AR(1) series (the same in every pixel), mean 100.
fn rain() -> Vec<f64> {
    let mut rng = Lcg(11);
    let mut a = 0.0;
    (0..T)
        .map(|_| {
            a = 0.6 * a + 10.0 * rng.next() * 3f64.sqrt();
            100.0 + a
        })
        .collect()
}

/// A multiband GeoTIFF of `values` [band][row-major pixels], the dates in the band descriptions.
fn write_tif(path: &Path, values: &[Vec<f32>], w: usize, h: usize, gt: [f64; 6], epsg: u32) -> Result<(), String> {
    let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(e)?;
    let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, values.len()).map_err(e)?;
    ds.set_geo_transform(&gt).map_err(e)?;
    let wkt = gdal::spatial_ref::SpatialRef::from_epsg(epsg).and_then(|s| s.to_wkt()).map_err(e)?;
    ds.set_projection(&wkt).map_err(e)?;
    for (b, v) in values.iter().enumerate() {
        let mut band = ds.rasterband(b + 1).map_err(e)?;
        band.write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), v.clone())).map_err(e)?;
        band.set_description(&label(b)).map_err(e)?;
    }
    Ok(())
}

/// The four series' files into `dir`.
fn write_series(dir: &Path) -> Result<(), String> {
    let utm = [600000.0, 100.0, 0.0, 7500000.0, 0.0, -100.0];
    let z = components();
    let mut rng = Lcg(7);
    let modes: Vec<Vec<f32>> = (0..T)
        .map(|t| {
            let season = 3.0 * (2.0 * std::f64::consts::PI * (t % 12) as f64 / 12.0).sin();
            (0..W * H)
                .map(|i| {
                    let (x, y) = (i % W, i / W);
                    let signal: f64 = (0..2).map(|k| AMPLITUDE[k] * pattern(k, x, y) * z[k][t]).sum();
                    (10.0 + 0.02 * x as f64 + season * (1.0 + 0.02 * y as f64) + signal + 0.05 * rng.next()) as f32
                })
                .collect()
        })
        .collect();
    write_tif(&dir.join(FILES[0]), &modes, W, H, utm, 32723)?;
    let r = rain();
    // 12 x 8 pixels over longitudes and latitudes that cover Modes' grid (UTM 23S near -44 degrees, -22.6)
    let geographic = [-44.10, 0.2 / 12.0, 0.0, -22.50, 0.0, -0.2 / 8.0];
    let coarse: Vec<Vec<f32>> = r.iter().map(|&v| vec![v as f32; 12 * 8]).collect();
    write_tif(&dir.join(FILES[1]), &coarse, 12, 8, geographic, 4326)?;
    let fine: Vec<Vec<f32>> = r.iter().map(|&v| vec![v as f32; W * H]).collect();
    write_tif(&dir.join(FILES[3]), &fine, W, H, utm, 32723)?;
    let mean = r.iter().sum::<f64>() / T as f64;
    let response: Vec<Vec<f32>> = (0..T)
        .map(|t| {
            (0..W * H)
                .map(|i| {
                    let signal = if i % W < W / 2 && t >= 2 { 0.05 * (r[t - 2] - mean) } else { 0.0 };
                    (5.0 + signal + 0.2 * rng.next()) as f32
                })
                .collect()
        })
        .collect();
    write_tif(&dir.join(FILES[2]), &response, W, H, utm, 32723)
}

fn pearson(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma).powi(2);
        sbb += (y - mb).powi(2);
    }
    sab / (saa * sbb).sqrt()
}

/// The values of `a` minus the mean of its calendar month.
fn monthly_anomaly(a: &[f64]) -> Vec<f64> {
    (0..a.len())
        .map(|t| {
            let same: Vec<f64> = (0..a.len()).filter(|&u| u % 12 == t % 12).map(|u| a[u]).collect();
            a[t] - same.iter().sum::<f64>() / same.len() as f64
        })
        .collect()
}

impl State {
    /// The layer of one of the test's files.
    fn eof_test_layer(&self, file: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.inputs.len() == 1 && l.inputs[0].ends_with(file))
    }

    /// The result map of a run's output (by its id).
    fn eof_test_map(&self, run: &str, id: &str) -> Option<&crate::zeit::ResultLayer> {
        let m = self.zeit.modes_runs.iter().find(|m| m.run == run)?;
        let path = &m.maps.iter().find(|(i, _)| i == id)?.1;
        self.zeit.results.iter().find(|r| &r.path == path)
    }

    /// The windows of the stages before closed, so that what a screenshot shows is the Modes tab.
    fn eof_test_clear(&mut self) {
        for id in ["eof", "lagged_correlation"] {
            if let Some(t) = manifest().tool(id) {
                self.set_tool_open(t, false);
            }
        }
        self.show_exports = false;
        self.zeit.show_tasks = false;
        self.zeit.log.show = false;
        self.error_popup = None;
    }

    /// A screenshot of the window into JANUS_EOF_SHOTS (when set), once its frames have drawn: true when done.
    fn eof_test_shot(&mut self, st: &mut UiTest, ctx: &egui::Context, name: &str) -> bool {
        let Some(dir) = std::env::var_os("JANUS_EOF_SHOTS").map(PathBuf::from) else { return true };
        let shot = st.eof.shot.get_or_insert((name.to_string(), 0, false));
        shot.1 += 1;
        if shot.1 < 8 {
            return false;
        }
        if !shot.2 {
            shot.2 = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            return false;
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        let Some(img) = image else { return false };
        let _ = std::fs::create_dir_all(&dir);
        let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
        let [w, h] = img.size;
        let path = dir.join(format!("{name}.png"));
        match image::save_buffer(&path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8) {
            Ok(()) => println!("    screenshot {}", path.display()),
            Err(e) => println!("    screenshot {}: {e}", path.display()),
        }
        st.eof.shot = None;
        true
    }

    pub(super) fn test_eof(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
            st.eof.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let z = components();
        match stage {
            320 => {
                if st.eof.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.eof = EofTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-eof-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        analysis: (self.analysis.show, self.analysis.tab),
                        font: self.settings.s.font_size,
                        tasks: self.zeit.show_tasks,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.eof.dir);
                    if let Err(e) = write_series(&st.eof.dir) {
                        return fail(&format!("the test's series could not be written: {e}"));
                    }
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                let have = self.layers.len() - st.eof.layers;
                if have < st.eof.opened
                    || (have > 0 && !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1)))
                {
                    return None;
                }
                if st.eof.opened < FILES.len() {
                    let f = st.eof.dir.join(FILES[st.eof.opened]);
                    st.eof.opened += 1;
                    self.open_inputs(vec![f.to_string_lossy().into_owned()], true, ctx);
                    return None;
                }
                let modes = self.eof_test_layer(FILES[0])?;
                self.zeit.results_dir = Some(st.eof.dir.join("results"));
                self.set_active(modes, ctx);
                let info = self.layer_session(modes)?.info.clone();
                if info.t() != T || info.width != W || info.height != H || !info.time_is_date {
                    return fail(&format!("Modes should be {T} dates on {W} x {H}: {} dates", info.t()));
                }
                next(
                    st,
                    "four series written and opened: Modes (48 x 32, 60 months), Rain (12 x 8, EPSG:4326), Response, RainFine",
                );
                None
            }
            321 => {
                let tool = manifest().tool("eof")?;
                if self.session.as_ref()?.info.t() != T {
                    return None;
                }
                if st.frames == 0 {
                    let mut params: std::collections::BTreeMap<String, Value> =
                        tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                    params.insert("n_modes".into(), json!(3));
                    params.insert("maps".into(), json!(3));
                    *self.zeit.params(tool) = params;
                    self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                    self.set_tool_open(tool, true);
                    st.frames = 1;
                    return None;
                }
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                let drawn = ctx.memory(|m| m.area_rect(egui::Id::new(("zeit tool", &tool.id)))).map(|r| r.height());
                if drawn.is_none_or(|h| h < 60.0) {
                    return fail("the Temporal EOF window should be drawn");
                }
                match self.zeit.tools.get(&tool.id).and_then(|u| u.estimate()) {
                    Some(Ok(e)) => {
                        if st.frames == 4 {
                            println!("    estimate: {e:?}");
                        }
                    }
                    Some(Err(e)) => return fail(&format!("the estimate: {e}")),
                    None => return if waited > 60.0 { fail("the estimate should arrive") } else { None },
                }
                let before = self.zeit.jobs.len();
                self.run_tool(tool, ctx);
                if self.zeit.jobs.len() != before + 1 || self.zeit.jobs.last().is_none_or(|j| j.tool != "eof") {
                    return fail(&format!("Run should start the EOF as a task: {:?}", self.error_popup));
                }
                st.eof.eof_run = self.zeit.jobs.last()?.output_dir.clone();
                next(st, "Temporal EOF: the window drawn with its estimate, Run on the whole image (3 modes)");
                None
            }
            322 => {
                let run = st.eof.eof_run.clone();
                match st.eof.phase {
                    0 => {
                        let job = self.zeit.jobs.iter().find(|j| j.output_dir == run)?;
                        let (state, error) = {
                            let s = job.status.lock().ok()?;
                            (s.state, s.error.clone())
                        };
                        match state {
                            JobState::Failed | JobState::Cancelled => {
                                return fail(&format!("the EOF should finish: {error}"));
                            }
                            JobState::Running => return None,
                            JobState::Done => {}
                        }
                        if !job.loaded || self.zeit.loads_pending() {
                            return if waited > 90.0 { fail("the EOF's maps should arrive") } else { None };
                        }
                        let modes: Vec<&crate::modes::EofMode> =
                            self.zeit.results.iter().filter(|r| r.run == run).filter_map(|r| r.eof.as_ref()).collect();
                        if modes.len() != 3 || self.zeit.results.iter().filter(|r| r.run == run).count() != 3 {
                            return fail(&format!("the EOF should give 3 maps with their modes, got {}", modes.len()));
                        }
                        // the variance: the amplitudes' squared norms of the patterns over their sum
                        let norm: Vec<f64> = (0..2)
                            .map(|k| {
                                AMPLITUDE[k].powi(2) * (0..W * H).map(|i| pattern(k, i % W, i / W).powi(2)).sum::<f64>()
                            })
                            .collect();
                        let total: f64 = norm.iter().sum();
                        for k in 0..2 {
                            let m = self.eof_test_map(&run, &format!("eof_{}", k + 1))?;
                            let (mode, layer) = (m.eof.as_ref()?, m);
                            let want = norm[k] / total;
                            // the map against the pattern, the component against the known one
                            let (mut a, mut b) = (vec![], vec![]);
                            for i in 0..W * H {
                                let v = f64::from(layer.data[i]);
                                if v.is_finite() {
                                    a.push(v);
                                    b.push(pattern(k, i % W, i / W));
                                }
                            }
                            let rm = pearson(&a, &b).abs();
                            let rp = pearson(&mode.pc, &z[k]).abs();
                            println!(
                                "    EOF {}: {:.1} % of the variance (analytic {:.1} %), |corr| of the map with its pattern {rm:.4}, of its PC with its component {rp:.4}, North's error {:.2} %, separable {:?}",
                                k + 1,
                                100.0 * mode.variance,
                                100.0 * want,
                                100.0 * mode.north_error.unwrap_or(f64::NAN),
                                mode.separable
                            );
                            if layer.data.len() != W * H || mode.pc.len() != T {
                                return fail("a mode's map should be 48 x 32 and its component 60 dates");
                            }
                            if rm < 0.99 || rp < 0.99 {
                                return fail(&format!("EOF {} should recover its pattern and its component", k + 1));
                            }
                            if (mode.variance - want).abs() > 0.02 {
                                return fail(&format!("EOF {}'s variance should be within 0.02 of {want:.3}", k + 1));
                            }
                            if mode.separable != Some(true) {
                                return fail(&format!("mode {} should be separable (North et al. 1982)", k + 1));
                            }
                            if !layer.name.contains(&format!("EOF {k1}", k1 = k + 1)) && layer.name.is_empty() {
                                return fail("a map should be named for its mode");
                            }
                        }
                        // the files of the run
                        for f in ["eof.json", "pcs.csv", "job.json"] {
                            if !Path::new(&run).join(f).is_file() {
                                return fail(&format!("the run's folder should have {f}"));
                            }
                        }
                        let Some(csv) = self.modes_pcs_csv(&run) else {
                            return fail("the run should be known to Modes");
                        };
                        if csv.lines().count() != T + 1 || !csv.starts_with("date,year,filled,pc1,pc2,pc3") {
                            return fail("pcs.csv should have a line a date and the three components");
                        }
                        self.eof_test_clear();
                        self.analysis.show = true;
                        self.analysis.focus = true;
                        self.analysis.tab = crate::modes::TAB;
                        st.eof.phase = 1;
                        st.frames = 0;
                        None
                    }
                    1 => {
                        // Analysis > Modes available and drawn
                        st.frames += 1;
                        if self.analysis_tab_unavailable(crate::modes::TAB).is_some() {
                            return fail("Analysis > Modes should be available with an EOF run");
                        }
                        if st.frames < 6 {
                            return None;
                        }
                        if self.shown_analysis_tab() != crate::modes::TAB || self.analysis.modes.drawn == 0 {
                            return if waited > 30.0 {
                                fail("Analysis > Modes should be shown and drawn")
                            } else {
                                None
                            };
                        }
                        println!("    Analysis > Modes drawn ({} frames)", self.analysis.modes.drawn);
                        st.eof.phase = 2;
                        None
                    }
                    2 | 3 => {
                        // the screenshots (when asked for) at two font sizes
                        let px = if st.eof.phase == 2 { 13 } else { 20 };
                        if std::env::var_os("JANUS_EOF_SHOTS").is_some() {
                            if self.settings.s.font_size != px {
                                self.settings.s.font_size = px;
                                let s = &self.settings.s;
                                crate::theme::apply(ctx, s.look(), s.panel_opacity, px);
                            }
                            if self.eof_test_shot(st, ctx, &format!("modes_eof_font{px}")) {
                                st.eof.phase += 1;
                            }
                            return None;
                        }
                        st.eof.phase = 4;
                        None
                    }
                    _ => {
                        self.settings.s.font_size = st.eof.font;
                        let s = &self.settings.s;
                        crate::theme::apply(ctx, s.look(), s.panel_opacity, st.eof.font);
                        // Show map on mode 2: its map visible, the others of the run not
                        if !self.modes_show_map(&run, 2, false) {
                            return fail("Show map should find the map of mode 2");
                        }
                        let visible: Vec<usize> = (1..=3)
                            .filter(|k| self.eof_test_map(&run, &format!("eof_{k}")).is_some_and(|r| r.visible))
                            .collect();
                        if visible != [2] {
                            return fail(&format!("Show map on mode 2 should leave eof_2 alone visible: {visible:?}"));
                        }
                        if self.modes_show_map(&run, 7, false) || self.modes_show_map(&run, 2, true) {
                            return fail("Show map should refuse a mode whose map was not written");
                        }
                        next(
                            st,
                            "EOF: patterns and components recovered, variances within 0.02, modes 1 and 2 separable; Analysis > Modes drawn; Show map on mode 2",
                        );
                        None
                    }
                }
            }
            323 => {
                // the chart's overlay of mode 1 at a pixel of its blob
                let run = st.eof.eof_run.clone();
                if st.frames == 0 {
                    if !self.modes_show_map(&run, 1, false) {
                        return fail("Show map should find the map of mode 1");
                    }
                    let (x, y) = (12, 16);
                    self.set_hover(Some((x, y)));
                    self.rest_hover();
                    st.frames = 1;
                    return None;
                }
                let Some(h) = self.series.hover.clone().filter(|h| h.exact && h.values.len() == T) else {
                    return if waited > 30.0 { fail("the cursor's exact series should be read") } else { None };
                };
                let Some((name, pts)) = self.eof_overlay((h.x, h.y), &h.values) else {
                    return fail("with an EOF map on top the chart should have the mode's overlay at the cursor");
                };
                let ov: Vec<f64> = pts.iter().map(|p| p[1]).collect();
                let own: Vec<f64> = h.values.iter().map(|&v| f64::from(v)).collect();
                // the overlay carries the seasonal cycle too: its anomaly is the mode, the pixel's anomaly the data's
                let r = pearson(&monthly_anomaly(&ov), &monthly_anomaly(&own));
                println!("    {name}: {} points, correlation with the pixel's anomaly {r:.4}", ov.len());
                if ov.len() != T || r < 0.9 || !name.starts_with("EOF 1") {
                    return fail("the overlay should follow the pixel's anomaly (> 0.9)");
                }
                // hidden: no overlay
                for r in self.zeit.results.iter_mut().filter(|r| r.run == run) {
                    r.visible = false;
                }
                if self.eof_overlay((h.x, h.y), &h.values).is_some() {
                    return fail("with no mode's map visible there should be no overlay");
                }
                self.modes_show_map(&run, 1, false);
                next(st, "chart overlay: EOF 1 at the cursor follows the pixel's anomaly");
                None
            }
            324 => {
                let tool = manifest().tool("lagged_correlation")?;
                if st.frames == 0 {
                    let response = self.eof_test_layer(FILES[2])?;
                    let rain = self.eof_test_layer(FILES[1])?;
                    self.set_active(response, ctx);
                    // Rain is in another CRS, and the driver may be: the form lists it, enabled
                    let choices = self.other_series_choices();
                    let rain_choice = choices.iter().find(|c| c.inputs == self.layers[rain].inputs);
                    if rain_choice.is_none_or(|c| c.same_crs) {
                        return fail("Rain should be listed as a series in another CRS");
                    }
                    let mut params: std::collections::BTreeMap<String, Value> =
                        tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                    params.insert(
                        "y".into(),
                        json!({ "name": self.layers[rain].name, "inputs": self.layers[rain].inputs }),
                    );
                    params.insert("lags".into(), json!(4));
                    params.insert("anomaly".into(), json!("none"));
                    *self.zeit.params(tool) = params;
                    self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                    self.set_tool_open(tool, true);
                    st.frames = 1;
                    return None;
                }
                if self.session.as_ref()?.info.id != self.layer_session(self.eof_test_layer(FILES[2])?)?.info.id {
                    return None;
                }
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                let drawn = ctx.memory(|m| m.area_rect(egui::Id::new(("zeit tool", &tool.id)))).map(|r| r.height());
                if drawn.is_none_or(|h| h < 60.0) {
                    return fail("the Lagged correlation window should be drawn");
                }
                match self.zeit.tools.get(&tool.id).and_then(|u| u.estimate()) {
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return fail(&format!("the estimate: {e}")),
                    None => return if waited > 60.0 { fail("the estimate should arrive") } else { None },
                }
                let before = self.zeit.jobs.len();
                self.run_tool(tool, ctx);
                if self.zeit.jobs.len() != before + 1
                    || self.zeit.jobs.last().is_none_or(|j| j.tool != "lagged_correlation")
                {
                    return fail(&format!("Run should start the lagged correlation as a task: {:?}", self.error_popup));
                }
                st.eof.corr_run = self.zeit.jobs.last()?.output_dir.clone();
                next(
                    st,
                    "Lagged correlation: Response against Rain (another CRS) at lags 0 to 4 months, Run on the whole image",
                );
                None
            }
            325 => {
                let run = st.eof.corr_run.clone();
                match st.eof.phase {
                    0 => {
                        let job = self.zeit.jobs.iter().find(|j| j.output_dir == run)?;
                        let (state, error) = {
                            let s = job.status.lock().ok()?;
                            (s.state, s.error.clone())
                        };
                        match state {
                            JobState::Failed | JobState::Cancelled => {
                                return fail(&format!("the lagged correlation should finish: {error}"));
                            }
                            JobState::Running => return None,
                            JobState::Done => {}
                        }
                        let rlags = self
                            .zeit
                            .modes_runs
                            .iter()
                            .find(|m| m.run == run)
                            .and_then(|m| m.corr.as_ref())
                            .and_then(|c| c.r_lags.clone());
                        let series_open = rlags.as_ref().is_some_and(|p| {
                            self.layers.iter().any(|l| {
                                l.inputs.len() == 1
                                    && crate::provenance::absolute(&l.inputs[0]) == crate::provenance::absolute(p)
                            })
                        });
                        if !job.loaded || self.zeit.loads_pending() || self.opening.is_some() || !series_open {
                            return if waited > 90.0 {
                                fail("the maps and the lags' series should arrive")
                            } else {
                                None
                            };
                        }
                        let get = |id: &str| self.eof_test_map(&run, id);
                        let (Some(lag), Some(r), Some(sig), Some(q)) =
                            (get("best_lag"), get("best_r"), get("significant"), get("q"))
                        else {
                            return fail("the run should give best_lag, best_r, significant and q");
                        };
                        let (mut left, mut best2, mut strong, mut right, mut right_sig) = (0, 0, 0, 0, 0);
                        for i in 0..W * H {
                            if i % W < W / 2 {
                                left += 1;
                                if lag.data[i].round() as i32 == 2 {
                                    best2 += 1;
                                }
                                if r.data[i] > 0.7 {
                                    strong += 1;
                                }
                            } else {
                                right += 1;
                                if sig.data[i].round() as i32 == 2 {
                                    right_sig += 1;
                                }
                            }
                        }
                        let (f2, fs, fr) =
                            (best2 as f64 / left as f64, strong as f64 / left as f64, right_sig as f64 / right as f64);
                        println!(
                            "    left half: best lag 2 on {:.1} % of the pixels, r > 0.7 on {:.1} %; right half: {:.1} % significant",
                            100.0 * f2,
                            100.0 * fs,
                            100.0 * fr
                        );
                        if f2 < 0.95 || fs < 0.95 || fr > 0.12 {
                            return fail(
                                "the best lag should be 2 with r > 0.7 on the left half, few significant on the right",
                            );
                        }
                        if q.data.iter().filter(|v| v.is_finite()).count() < W * H / 2 {
                            return fail("q should be written for the pixels");
                        }
                        // the lags' series: 5 bands, hidden
                        let k = self.layers.iter().position(|l| {
                            l.inputs.len() == 1
                                && crate::provenance::absolute(&l.inputs[0])
                                    == crate::provenance::absolute(rlags.as_deref().unwrap_or(""))
                        })?;
                        let bands = self.layer_session(k)?.info.t();
                        if bands != 5 || self.layers[k].visible {
                            return fail(&format!(
                                "r at the lags should be a hidden series of 5 bands ({bands}, visible {})",
                                self.layers[k].visible
                            ));
                        }
                        // r's colours: -1 to 1
                        if (r.lo, r.hi) != (-1.0, 1.0) {
                            return fail(&format!("best_r should have the fixed range -1 to 1: {} to {}", r.lo, r.hi));
                        }
                        // Analysis > Modes for the run: r against the lag at a pixel of the left half
                        self.set_hover(Some((10, 10)));
                        self.rest_hover();
                        self.analysis.show = true;
                        self.analysis.focus = true;
                        self.analysis.tab = crate::modes::TAB;
                        self.analysis.modes.run = run.clone();
                        self.analysis.modes.lags_at = None;
                        st.eof.phase = 1;
                        st.frames = 0;
                        None
                    }
                    1 => {
                        st.frames += 1;
                        if st.frames < 8 {
                            return None;
                        }
                        let Some(l) = self.analysis.modes.lags_at.clone().filter(|l| l.3.len() == 5) else {
                            return if waited > 30.0 {
                                fail("Analysis > Modes should read r at the 5 lags of the cursor")
                            } else {
                                None
                            };
                        };
                        let best = l.3.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).map(|a| a.0);
                        println!("    Modes: r at the lags of the pixel {:?}: {:.3?}", (l.1, l.2), l.3);
                        if best != Some(2) || self.shown_analysis_tab() != crate::modes::TAB {
                            return fail("Analysis > Modes should show r strongest at lag 2");
                        }
                        let share = self.analysis.modes.share_significant();
                        println!("    Modes: significant share {share:?}");
                        if !share.is_some_and(|s| (0.4..0.7).contains(&s)) {
                            return fail("about half the pixels should be significant");
                        }
                        st.eof.phase = 2;
                        None
                    }
                    2 => {
                        self.eof_test_clear();
                        let px = 13;
                        if std::env::var_os("JANUS_EOF_SHOTS").is_some() {
                            if self.eof_test_shot(st, ctx, &format!("modes_correlation_font{px}")) {
                                st.eof.phase = 3;
                            }
                            return None;
                        }
                        st.eof.phase = 3;
                        None
                    }
                    _ => {
                        next(
                            st,
                            "Lagged correlation: best lag 2 on the left half, few significant on the right, q, r at the lags (5 bands, hidden), Analysis > Modes",
                        );
                        None
                    }
                }
            }
            326 => {
                // the raster calculator's corr() against the tool's r at lag 2 (Response against RainFine, on one grid)
                let run = st.eof.corr_run.clone();
                if st.frames == 0 {
                    let response = self.eof_test_layer(FILES[2])?;
                    let fine = self.eof_test_layer(FILES[3])?;
                    self.set_active(response, ctx);
                    let (ls, at) = self.calc_layers();
                    let alias = |k: usize| at.iter().position(|&i| i == k).map(|p| ls[p].alias.clone());
                    let (a, b) = (alias(response)?, alias(fine)?);
                    self.calc.show = true;
                    self.calc.text = format!("corr({a}, {b}, lag=2m)");
                    self.calc.primary = None;
                    self.calc.cog = false;
                    st.eof.opened = self.layers.len();
                    self.calc_run(ctx);
                    st.frames = 1;
                    return None;
                }
                if self.opening.is_some()
                    || self.layers.len() == st.eof.opened
                    || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                {
                    if self.emb_downloads.iter().any(|d| d.status.lock().unwrap().state == JobState::Failed) {
                        return fail("the calculator's run failed (Tools > Tasks > Log)");
                    }
                    return if waited > 90.0 { fail("the calculator's corr() should arrive") } else { None };
                }
                let k = self.layers.len() - 1;
                let info = self.layer_session(k)?.info.clone();
                let tool = self.zeit.modes_runs.iter().find(|m| m.run == run)?.corr.as_ref()?.r_lags.clone()?;
                let mut worst: f64 = 0.0;
                for (x, y) in [(3, 4), (20, 20), (10, 30), (30, 8), (44, 25)] {
                    let Some(v) = janus_cube::with_thread_reader(&info, |r| r.read_pixel(0, x, y)) else {
                        return fail("the calculator's result should be readable");
                    };
                    let lags = crate::modes::read_lags(&tool, x, y).unwrap_or_default();
                    let want = lags.get(2).copied().unwrap_or(f64::NAN);
                    println!("    pixel ({x}, {y}): corr() {v:.5}, the tool's r at lag 2 {want:.5}");
                    worst = worst.max((f64::from(v) - want).abs());
                }
                if info.t() != 1 || worst > 1e-4 || worst.is_nan() {
                    return fail(&format!(
                        "corr(response, rain, lag=2m) should equal the tool's lag-2 r to 1e-4 ({worst:.2e})"
                    ));
                }
                self.calc.show = false;
                self.remove_layer(k, ctx);
                next(
                    st,
                    &format!(
                        "the calculator's corr(response, rain, lag=2m) equals the tool's r at lag 2 (largest difference {worst:.1e})"
                    ),
                );
                None
            }
            327 => {
                // everything closed, restored as the stages after expect it
                for id in ["eof", "lagged_correlation"] {
                    if let Some(t) = manifest().tool(id) {
                        self.set_tool_open(t, false);
                    }
                }
                self.settings.s.font_size = st.eof.font;
                let s = &self.settings.s;
                crate::theme::apply(ctx, s.look(), s.panel_opacity, st.eof.font);
                (self.analysis.show, self.analysis.tab) = st.eof.analysis;
                self.analysis.modes = Default::default();
                self.zeit.show_tasks = st.eof.tasks;
                self.calc.show = false;
                for k in (st.eof.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let runs = [st.eof.eof_run.clone(), st.eof.corr_run.clone()];
                self.zeit.results.retain(|r| !runs.contains(&r.run));
                self.zeit.jobs.retain(|j| !runs.contains(&j.output_dir));
                self.zeit.modes_runs.retain(|m| !runs.contains(&m.run));
                self.zeit.results_dir = st.eof.results_dir.take();
                if st.eof.active < self.layers.len() && st.eof.active != self.active {
                    self.set_active(st.eof.active, ctx);
                }
                self.series.hover = None;
                let _ = std::fs::remove_dir_all(&st.eof.dir);
                st.eof = Default::default();
                next(st, "Temporal EOF and lagged correlation: the windows closed, the layers, runs and files removed");
                st.stage = 330; // the crash notice (330-333), the figures, the chart axis, Tune, Climate (ui_test_crash.rs...), then 299
                None
            }
            _ => None,
        }
    }
}
