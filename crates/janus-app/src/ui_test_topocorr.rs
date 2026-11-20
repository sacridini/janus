//! `--selftest-ui` stages 370-374: the topographic correction (Tools > Data preparation > Topographic correction) on a
//! series the test writes itself: a DEM of two hills (a file, so that nothing is downloaded) on 96 x 80 pixels of 30 m, and
//! three dates (a file each: blue, green, red, NIR and a quality band) of a surface that follows the illumination of each
//! date's sun, which the dates' files say as STAC_SUN_ZENITH and STAC_SUN_AZIMUTH, with a block of date 2 marked cloud.
//! 370 writes and opens it with its quality band; 371 opens the tool's window (drawn, its estimate, the sun of each
//! date read from the files: 3 dates from the STAC item); 372 runs SCS+C and waits for the corrected series to open as
//! a layer; 373 checks it (a layer of three dates, four bands and the quality band, the cloud still there as a code, c
//! of the model, the correlation with the illumination before and after, the check's table) and, with JANUS_PYTHON, that
//! the run's Python through Zeit gives the same values; 374 the "?" of the window (its page on the site) and puts
//! everything back.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gdal::Metadata as _;
use janus_cube::{BandSelection, QaRule};
use janus_zeit::manifest::manifest;
use serde_json::{Value, json};

use super::UiTest;
use crate::app::State;
use crate::zeit::JobState;

const W: usize = 96;
const H: usize = 80;
const N: usize = W * H;
const NB: usize = 4;
const NAMES: [&str; NB] = ["blue", "green", "red", "nir"];
const DATES: [&str; 3] = ["2022-03-01", "2022-03-17", "2022-04-02"];
/// each date's sun: zenith and azimuth
const SUNS: [[f64; 2]; 3] = [[38.0, 118.0], [42.0, 135.0], [47.0, 152.0]];

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct TopoTest {
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    layers: usize,
    active: usize,
    phase: u8,
    run: String,
}

fn dem() -> Vec<f32> {
    (0..N)
        .map(|i| {
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let hill =
                |cx: f64, cy: f64, a: f64, s: f64| a * (-((x - cx).powi(2) + (y - cy).powi(2)) / (2.0 * s * s)).exp();
            (300.0 + hill(30.0, 36.0, 420.0, 11.0) + hill(66.0, 48.0, 360.0, 10.0)) as f32
        })
        .collect()
}

/// The cloud of date 2.
fn in_cloud(i: usize) -> bool {
    (40..56).contains(&(i % W)) && (10..26).contains(&(i / W))
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn open_for_write(path: &Path, bands: usize) -> Result<gdal::Dataset, String> {
    let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(e)?;
    let mut ds = drv.create_with_band_type::<f32, _>(path, W, H, bands).map_err(e)?;
    ds.set_geo_transform(&[600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).map_err(e)?;
    let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32723).and_then(|s| s.to_wkt()).map_err(e)?;
    ds.set_projection(&wkt).map_err(e)?;
    Ok(ds)
}

fn write_band(ds: &mut gdal::Dataset, band: usize, name: &str, v: Vec<f32>) -> Result<(), String> {
    let e = |e: gdal::errors::GdalError| e.to_string();
    let mut b = ds.rasterband(band).map_err(e)?;
    b.set_description(name).map_err(e)?;
    b.set_no_data_value(Some(-9999.0)).map_err(e)?;
    b.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), v)).map_err(e)
}

/// The base and gain of the model of a band: `rho = b0 + k cos i / cos sz`.
fn model(b: usize) -> (f64, f64) {
    (0.04 + 0.03 * b as f64, 0.15 + 0.05 * b as f64)
}

/// The DEM and the dates' files in `dir`; the DEM's path and the files.
fn write_scene(dir: &Path) -> Result<(PathBuf, Vec<PathBuf>), String> {
    let z = dem();
    let dem_path = dir.join("dem.tif");
    let mut ds = open_for_write(&dem_path, 1)?;
    write_band(&mut ds, 1, "elevation", z.clone())?;
    drop(ds);
    let (slope, aspect) = janus_zeit::topocorr::slope_aspect(&z, H, W, 30.0, -30.0, 1);
    let mut rng = Lcg(3);
    let mut files = vec![];
    for (t, date) in DATES.iter().enumerate() {
        let (zen, azi) = (SUNS[t][0], SUNS[t][1]);
        let ci = janus_zeit::topocorr::cos_incidence(&slope, &aspect, &[zen as f32], &[azi as f32], 1);
        let cz = zen.to_radians().cos();
        let path = dir.join(format!("topo_{date}.tif"));
        let mut ds = open_for_write(&path, NB + 1)?;
        for (b, name) in NAMES.iter().enumerate() {
            let (b0, k) = model(b);
            let v: Vec<f32> = ci.iter().map(|&c| (b0 + k * f64::from(c) / cz + 0.0005 * rng.next()) as f32).collect();
            write_band(&mut ds, b + 1, name, v)?;
        }
        // the quality band: 0 is masked (a block of date 2 is cloud)
        let qa: Vec<f32> = (0..N).map(|i| if t == 1 && in_cloud(i) { 0.0 } else { 1.0 }).collect();
        write_band(&mut ds, NB + 1, "mask", qa)?;
        ds.set_metadata_item("STAC_SUN_ZENITH", &zen.to_string(), "").map_err(|e| e.to_string())?;
        ds.set_metadata_item("STAC_SUN_AZIMUTH", &azi.to_string(), "").map_err(|e| e.to_string())?;
        files.push(path);
    }
    Ok((dem_path, files))
}

fn read_band(path: &Path, band: usize) -> Result<Vec<f32>, String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = ds.rasterband(band).map_err(|e| e.to_string())?;
    Ok(b.read_as::<f32>((0, 0), (W, H), (W, H), None).map_err(|e| e.to_string())?.data().to_vec())
}

fn pearson(pairs: impl Iterator<Item = (f64, f64)>) -> f64 {
    let (mut n, mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (x, y) in pairs {
        n += 1.0;
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    (sxy - sx * sy / n) / ((sxx - sx * sx / n) * (syy - sy * sy / n)).sqrt()
}

impl State {
    /// The correlation of a band of a date with its illumination on the steep pixels outside the cloud.
    fn topo_r(values: &[f32], t: usize, slope: &[f32], aspect: &[f32]) -> f64 {
        let ci = janus_zeit::topocorr::cos_incidence(slope, aspect, &[SUNS[t][0] as f32], &[SUNS[t][1] as f32], 1);
        let floor = 5f32.to_radians();
        pearson(
            (0..N)
                .filter(|&i| slope[i] >= floor && values[i].is_finite() && !(t == 1 && in_cloud(i)))
                .map(|i| (f64::from(ci[i]), f64::from(values[i]))),
        )
    }

    pub(super) fn test_topocorr(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
            st.topo.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let tool = manifest().tool("topocorr")?;
        match stage {
            370 => {
                if st.topo.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.topo = TopoTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-topocorr-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.topo.dir);
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                if st.topo.phase == 0 {
                    let files = match write_scene(&st.topo.dir) {
                        Ok((_, f)) => f,
                        Err(e) => return fail(&format!("the test's scene could not be written: {e}")),
                    };
                    self.zeit.results_dir = Some(st.topo.dir.join("results"));
                    self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, ctx);
                    st.topo.phase = 1;
                    return None;
                }
                if self.layers.len() <= st.topo.layers
                    || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                {
                    return None;
                }
                let k = self.layers.len() - 1;
                if st.topo.phase == 1 {
                    // the mask is the quality band: reopened with it, as the Display panel does
                    let sel = BandSelection {
                        band: 3,
                        qa_band: NB as i32 + 1,
                        qa_rule: QaRule::NonZero,
                        chosen: true,
                        ..Default::default()
                    };
                    self.reopen_layer(k, sel, ctx);
                    st.topo.phase = 2;
                    return None;
                }
                self.set_active(k, ctx);
                let info = self.layer_session(k)?.info.clone();
                if info.t() != 3
                    || info.bands_per_date != NB + 1
                    || info.sel.qa_band != NB as i32 + 1
                    || info.width != W
                    || info.height != H
                    || !info.time_is_date
                {
                    return fail(&format!(
                        "the series should be 3 dates of {NB} bands and a quality band on {W} x {H}: {} dates, {} bands, quality band {}",
                        info.t(),
                        info.bands_per_date,
                        info.sel.qa_band
                    ));
                }
                let roles = crate::zeit::guess_roles(&info);
                if ["red", "nir"].iter().any(|r| !roles.contains_key(*r)) {
                    return fail(&format!("red and nir should be found by the bands' names: {roles:?}"));
                }
                next(
                    st,
                    "a scene of two hills (DEM file) and 3 dates of 4 bands and a quality band written and opened",
                );
                None
            }
            371 => {
                // the window: drawn, its estimate, the sun of each date from the files
                if st.topo.phase == 0 {
                    let dem_path = st.topo.dir.join("dem.tif");
                    let mut params: std::collections::BTreeMap<String, Value> =
                        tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                    params.insert("dem_source".into(), json!("file"));
                    params.insert("dem".into(), json!(dem_path.to_string_lossy()));
                    *self.zeit.params(tool) = params;
                    self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                    self.set_tool_open(tool, true);
                    st.topo.phase = 1;
                    st.frames = 0;
                    return None;
                }
                st.frames += 1;
                if st.frames < 4 {
                    return None;
                }
                let drawn = ctx.memory(|m| m.area_rect(egui::Id::new(("zeit tool", &tool.id)))).map(|r| r.height());
                if drawn.is_none_or(|h| h < 60.0) {
                    return fail("the topographic correction's window should be drawn");
                }
                let rows = self.sun_rows(tool);
                if rows.len() != 3 || rows.iter().any(|r| r.missing || r.from != "STAC item") {
                    return fail(&format!(
                        "the sun preview should have 3 dates, each from the STAC item: {:?}",
                        rows.iter().map(|r| (&r.label, &r.zenith, &r.from)).collect::<Vec<_>>()
                    ));
                }
                if rows.iter().zip(SUNS).any(|(r, s)| {
                    r.zenith.parse::<f64>().ok() != Some(s[0]) || r.azimuth.parse::<f64>().ok() != Some(s[1])
                }) {
                    return fail("the sun preview should show the zenith and azimuth of each date's file");
                }
                match self.zeit.tools.get(&tool.id).and_then(|u| u.estimate()) {
                    Some(Ok(e)) => println!("    estimate: {e:?}"),
                    Some(Err(e)) => return fail(&format!("the estimate: {e}")),
                    None => return if waited > 60.0 { fail("the estimate should arrive") } else { None },
                }
                self.set_tool_open(tool, false);
                next(st, "the window drawn: the sun of 3 dates from their STAC item, the estimate");
                None
            }
            372 => {
                if st.topo.phase == 0 {
                    let before = self.zeit.jobs.len();
                    self.run_tool(tool, ctx);
                    if self.zeit.jobs.len() != before + 1 || self.zeit.jobs.last().is_none_or(|j| j.tool != "topocorr")
                    {
                        return fail(&format!("Run should start the topographic correction: {:?}", self.error_popup));
                    }
                    st.topo.run = self.zeit.jobs.last().map(|j| j.output_dir.clone()).unwrap_or_default();
                    st.topo.phase = 1;
                    return None;
                }
                let job = self.zeit.jobs.iter().find(|j| j.output_dir == st.topo.run)?;
                let (state, error) = {
                    let s = job.status.lock().ok()?;
                    (s.state, s.error.clone())
                };
                match state {
                    JobState::Failed | JobState::Cancelled => return fail(&format!("the run should finish: {error}")),
                    JobState::Running => return None,
                    JobState::Done => {}
                }
                if !job.loaded
                    || !self.zeit.series_to_open.is_empty()
                    || self.opening.is_some()
                    || !self.layer_jobs.queue.is_empty()
                {
                    return if waited > 120.0 { fail("the corrected series should open") } else { None };
                }
                if self.layers.len() != st.topo.layers + 2 {
                    return fail(&format!("the corrected series should open as a layer: {} layers", self.layers.len()));
                }
                let k = self.layers.len() - 1;
                if !crate::ui_test::loaded(self.layer_session(k)) {
                    return None;
                }
                next(st, "SCS+C run: the corrected series opened as a new layer");
                None
            }
            373 => {
                let k = self.layers.len() - 1;
                let info = self.layer_session(k)?.info.clone();
                if info.t() != 3
                    || info.bands_per_date != NB + 1
                    || info.sel.qa_band != NB as i32 + 1
                    || !info.time_is_date
                {
                    return fail(&format!(
                        "the corrected layer: 3 dates of {NB} bands and the quality band, got {} of {}, quality band {}",
                        info.t(),
                        info.bands_per_date,
                        info.sel.qa_band
                    ));
                }
                let run = Path::new(&st.topo.run);
                let mut files: Vec<PathBuf> = std::fs::read_dir(run.join("series"))
                    .map(|d| {
                        d.filter_map(|f| f.ok().map(|f| f.path()))
                            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
                            .collect()
                    })
                    .unwrap_or_default();
                files.sort();
                if files.len() != 3 {
                    return fail("the corrected series should be a file a date");
                }
                // the cloud kept as a code, and the correlation with the illumination before and after
                let qa = read_band(&files[1], NB + 1).ok()?;
                if qa.iter().filter(|&&c| c == 0.0).count() != 16 * 16 {
                    return fail("the quality band of date 2 should still mark its cloud");
                }
                let z = dem();
                let (slope, aspect) = janus_zeit::topocorr::slope_aspect(&z, H, W, 30.0, -30.0, 1);
                let source: Vec<PathBuf> = DATES.iter().map(|d| st.topo.dir.join(format!("topo_{d}.tif"))).collect();
                let (mut worst_before, mut worst_after) = (1.0f64, 0.0f64);
                for t in 0..3 {
                    for b in 0..NB {
                        let before = Self::topo_r(&read_band(&source[t], b + 1).ok()?, t, &slope, &aspect);
                        let after = Self::topo_r(&read_band(&files[t], b + 1).ok()?, t, &slope, &aspect);
                        worst_before = worst_before.min(before);
                        worst_after = worst_after.max(after.abs());
                    }
                }
                println!(
                    "    r(reflectance, cos i): at least {worst_before:.3} before, at most {worst_after:.4} after"
                );
                // SCS+C keeps cos s in its numerator, which is correlated with cos i: the correlation falls a lot, not to 0
                if worst_before < 0.9 || worst_after > 0.5 || worst_after > 0.5 * worst_before {
                    return fail("the correlation with the illumination should fall from near 1 to well under 0.5");
                }
                // c of the model (b0 cos sz / k) in topo_c.csv, the table of the check
                let table = std::fs::read_to_string(run.join("topo_c.csv")).unwrap_or_default();
                let rows: Vec<Vec<&str>> = table.lines().skip(1).map(|l| l.split(',').collect()).collect();
                if rows.len() != 3 * NB {
                    return fail(&format!("topo_c.csv should have a row for each date and band: {}", rows.len()));
                }
                for (r, row) in rows.iter().enumerate() {
                    let (b0, k) = model(r % NB);
                    let model_c = b0 * SUNS[r / NB][0].to_radians().cos() / k;
                    let c: f64 = row.get(3).and_then(|v| v.parse().ok()).unwrap_or(f64::NAN);
                    if (c - model_c).abs() > 0.01 || row.get(12) != Some(&"STAC item") {
                        return fail(&format!("c of {} should be {model_c:.4} from the STAC item: {row:?}", row[1]));
                    }
                }
                let check = std::fs::read_to_string(run.join("validation.csv")).unwrap_or_default();
                let after_ok = check.lines().skip(1).all(|l| {
                    let f: Vec<&str> = l.split(',').collect();
                    let r = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN);
                    r(2) > 0.9 && r(3).abs() < 0.5
                });
                if !after_ok {
                    return fail(&format!("validation.csv: |r| after should be under 0.5, before over 0.9:\n{check}"));
                }
                for f in ["report.txt", "topocorr.json"] {
                    if !run.join(f).is_file() {
                        return fail(&format!("the run's folder should have {f}"));
                    }
                }
                // Copy Python: the script calls zeit.topographic_correction (and, with Zeit, gives the run's values)
                let code = std::fs::read_to_string(run.join("rerun.py")).unwrap_or_default();
                if !code.contains("zeit.topographic_correction(")
                    || !code.contains("method='scs+c'")
                    || !code.contains("zeit-cdts>=3.9")
                {
                    return fail(&format!("rerun.py should call zeit.topographic_correction with SCS+C:\n{code}"));
                }
                let mut with_zeit = false;
                if let Ok(python) = std::env::var("JANUS_PYTHON") {
                    let out = std::process::Command::new(python).arg("-I").arg(run.join("rerun.py")).output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => return fail(&format!("rerun.py failed: {}", String::from_utf8_lossy(&o.stderr))),
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    // Zeit's cube is (time, band, y, x) with the quality band, copied: the file's bands run date by date, NB + 1 of each
                    let theirs = run.join("python").join("result.tif");
                    let mut worst = 0.0f64;
                    for (t, file) in files.iter().enumerate() {
                        for b in 0..NB {
                            let (z, o) = (read_band(&theirs, t * (NB + 1) + b + 1).ok()?, read_band(file, b + 1).ok()?);
                            // the pixels the quality band masks are NaN in Zeit's cube
                            for (x, y) in z.iter().zip(&o).filter(|(x, _)| x.is_finite()) {
                                worst = worst.max(f64::from((x - y).abs()));
                            }
                        }
                    }
                    println!("    Zeit's zeit.topographic_correction against the run: largest difference {worst:.2e}");
                    if worst > 1e-5 {
                        return fail("Zeit's own correction should be what the run wrote");
                    }
                    with_zeit = true;
                }
                next(
                    st,
                    &format!(
                        "topographic correction: 3 dates of 4 bands and the quality band, |r| with cos i {worst_before:.2} -> {worst_after:.2}, c of the model within 0.01, the check's table, rerun.py{}",
                        if with_zeit { " equal to Zeit's own" } else { " (no JANUS_PYTHON: not run)" }
                    ),
                );
                None
            }
            374 => {
                // the "?" of the window opens its section of the guide (no browser: the address is kept)
                if st.topo.phase == 0 {
                    crate::welcome::test_hooks::start();
                    crate::welcome::test_hooks::press("guide:topocorr");
                    self.set_tool_open(tool, true);
                    st.topo.phase = 1;
                    st.frames = 0;
                    return None;
                }
                st.frames += 1;
                if st.frames < 5 {
                    return None;
                }
                let urls = crate::welcome::test_hooks::urls();
                crate::welcome::test_hooks::stop();
                if !urls.iter().any(|u| u.ends_with("data-preparation.html#topographic-correction")) {
                    return fail(&format!(
                        "the ? of the window should open data-preparation.html#topographic-correction: {urls:?}"
                    ));
                }
                self.set_tool_open(tool, false);
                for k in (st.topo.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let run = st.topo.run.clone();
                self.zeit.results.retain(|r| r.run != run);
                self.zeit.jobs.retain(|j| j.output_dir != run);
                self.zeit.results_dir = st.topo.results_dir.take();
                if st.topo.active < self.layers.len() && st.topo.active != self.active {
                    self.set_active(st.topo.active, ctx);
                }
                let _ = std::fs::remove_dir_all(&st.topo.dir);
                st.topo = Default::default();
                next(
                    st,
                    "topographic correction: the ? opens data-preparation.html#topographic-correction; the window closed, the layers, runs and files removed",
                );
                st.stage = 385; // the detached calculator (385-388, ui_test_calc_detach.rs), then 299
                None
            }
            _ => None,
        }
    }
}
