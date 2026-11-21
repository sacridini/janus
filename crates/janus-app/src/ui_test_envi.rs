//! `--selftest-ui` stages 400-409: the MNF, the change vector analysis, the iMAD and the radiometric normalization
//! (Tools > Spectral analysis, Change detection, Data preparation), on a series the test writes itself: three dates (a
//! file each, 2019-03-15, 2020-03-15 and 2021-03-15) of five bands (blue, green, red, nir, swir1) on 48 x 32 pixels,
//! made of two smooth latent fields mixed into the bands, a gain and an offset a date, noise and, on the last date, a
//! block that changed (the NIR up and the red down: vegetation gain). 400 writes and opens it; 401 runs the MNF (the
//! window drawn, three components and the denoised bands, which open as a series); 402 the change vector analysis (its
//! change map, the sectors' classes); 403 the iMAD (its change map and variates); 404 the normalization (the series
//! it writes opens, the gain undoes the date's); 405 the Python of the runs (and, with JANUS_PYTHON, Zeit's own result
//! against Janus's); 406 puts everything back. 407-409 are the omnibus change test of Sentinel-1, on a radar series of
//! its own (14 dates of VV and VH in dB, a file each, with a block whose backscatter rose at one date): 407 writes and
//! opens it and runs the test (the maps, the classes of the directions, the series of directions); 408 puts a pin in the
//! block (its chart marks the change at the date the map gives) and runs the run's Python through Zeit (JANUS_PYTHON);
//! 409 first runs the test on the VV planes alone (a series of one band a date: single polarization), then puts
//! everything back.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gdal::Metadata as _;
use janus_zeit::manifest::manifest;
use serde_json::{Value, json};

use super::UiTest;
use crate::app::State;
use crate::zeit::JobState;

const W: usize = 48;
const H: usize = 32;
const NB: usize = 5;
const NAMES: [&str; NB] = ["blue", "green", "red", "nir", "swir1"];
const DATES: [&str; 3] = ["2019-03-15", "2020-03-15", "2021-03-15"];
/// The block that changed on the last date.
const BLOCK: (std::ops::Range<usize>, std::ops::Range<usize>) = (14..24, 10..20);

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct EnviTest {
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    layers: usize,
    active: usize,
    opened: bool,
    phase: u8,
    /// the runs: (tool, folder)
    runs: Vec<(String, String)>,
    /// the pin of the omnibus stages: its number and place
    pin: Option<(u32, usize, usize)>,
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// A date's bands, [band][pixels]: the latent fields mixed, the date's gain and offset, noise, the changed block.
fn date_bands(t: usize) -> Vec<Vec<f32>> {
    let mut rng = Lcg(5 + t as u64);
    let tau = 2.0 * std::f64::consts::PI;
    let (gain, offset) = [(1.0, 0.0), (1.2, 0.03), (0.9, -0.02)][t];
    (0..NB)
        .map(|b| {
            (0..W * H)
                .map(|i| {
                    let (x, y) = ((i % W) as f64, (i / W) as f64);
                    let l1 = (tau * (x / 17.0 + y / 23.0)).sin();
                    let l2 = (tau * (x / 11.0 - y / 19.0)).cos();
                    let base = 0.3 + 0.05 * b as f64 + (0.5 + 0.1 * b as f64) * l1 + (0.3 - 0.05 * b as f64) * l2;
                    let mut v = gain * base + offset + 0.005 * rng.next();
                    if t == 2 && BLOCK.0.contains(&(i % W)) && BLOCK.1.contains(&(i / W)) {
                        v += match b {
                            3 => 0.9,
                            2 => -0.5,
                            _ => 0.0,
                        };
                    }
                    v as f32
                })
                .collect()
        })
        .collect()
}

/// The files of the dates in `dir`.
fn write_series(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![];
    for (t, date) in DATES.iter().enumerate() {
        let path = dir.join(format!("envi_{date}.tif"));
        let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(e)?;
        let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, NB).map_err(e)?;
        ds.set_geo_transform(&[600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).map_err(e)?;
        let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32723).and_then(|s| s.to_wkt()).map_err(e)?;
        ds.set_projection(&wkt).map_err(e)?;
        for (b, v) in date_bands(t).iter().enumerate() {
            let mut band = ds.rasterband(b + 1).map_err(e)?;
            band.set_description(NAMES[b]).map_err(e)?;
            band.set_no_data_value(Some(-9999.0)).map_err(e)?;
            band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), v.clone())).map_err(e)?;
        }
        paths.push(path);
    }
    Ok(paths)
}

fn read_band(path: &Path, band: usize) -> Result<Vec<f32>, String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = ds.rasterband(band).map_err(|e| e.to_string())?;
    Ok(b.read_as::<f32>((0, 0), (W, H), (W, H), None).map_err(|e| e.to_string())?.data().to_vec())
}

fn in_block(i: usize) -> bool {
    BLOCK.0.contains(&(i % W)) && BLOCK.1.contains(&(i / W))
}

impl State {
    /// The result map of a run by the file's name.
    fn envi_map(&self, run: &str, file: &str) -> Option<&crate::zeit::ResultLayer> {
        self.zeit.results.iter().find(|r| r.run == run && r.path.replace('\\', "/").ends_with(file))
    }

    /// Starts a tool's run on the whole image with `params` over its defaults; the run's folder.
    fn envi_start(&mut self, id: &str, params: Value, ctx: &egui::Context) -> Result<String, String> {
        let tool = manifest().tool(id).ok_or("no such tool")?;
        let mut all: std::collections::BTreeMap<String, Value> =
            tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
        for (k, v) in params.as_object().into_iter().flatten() {
            all.insert(k.clone(), v.clone());
        }
        *self.zeit.params(tool) = all;
        self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
        let before = self.zeit.jobs.len();
        self.run_tool(tool, ctx);
        if self.zeit.jobs.len() != before + 1 || self.zeit.jobs.last().is_none_or(|j| j.tool != id) {
            return Err(format!("Run should start {id} as a task: {:?}", self.error_popup));
        }
        Ok(self.zeit.jobs.last().map(|j| j.output_dir.clone()).unwrap_or_default())
    }

    /// Whether a run is done and its maps are loaded: Ok(false) while it runs.
    fn envi_done(&self, run: &str, waited: f64) -> Result<bool, String> {
        let Some(job) = self.zeit.jobs.iter().find(|j| j.output_dir == run) else { return Ok(false) };
        let (state, error) = {
            let s = job.status.lock().map_err(|e| e.to_string())?;
            (s.state, s.error.clone())
        };
        match state {
            JobState::Failed | JobState::Cancelled => return Err(format!("the run should finish: {error}")),
            JobState::Running => return Ok(false),
            JobState::Done => {}
        }
        if !job.loaded || self.zeit.loads_pending() {
            return if waited > 90.0 { Err("the run's maps should arrive".into()) } else { Ok(false) };
        }
        Ok(true)
    }

    pub(super) fn test_envi(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
            st.envi.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match stage {
            400 => {
                if st.envi.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.envi = EnviTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-envi-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.envi.dir);
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                if !st.envi.opened {
                    let files = match write_series(&st.envi.dir) {
                        Ok(f) => f,
                        Err(e) => return fail(&format!("the test's series could not be written: {e}")),
                    };
                    st.envi.opened = true;
                    self.zeit.results_dir = Some(st.envi.dir.join("results"));
                    self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, ctx);
                    return None;
                }
                if self.layers.len() <= st.envi.layers
                    || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                {
                    return None;
                }
                let k = self.layers.len() - 1;
                self.set_active(k, ctx);
                let info = self.layer_session(k)?.info.clone();
                if info.t() != 3
                    || info.bands_per_date != NB
                    || info.width != W
                    || info.height != H
                    || !info.time_is_date
                {
                    return fail(&format!(
                        "the series should be 3 dates of {NB} bands on {W} x {H}: {} dates, {} bands",
                        info.t(),
                        info.bands_per_date
                    ));
                }
                let roles = crate::zeit::guess_roles(&info);
                if ["red", "nir", "swir1"].iter().any(|r| !roles.contains_key(*r)) {
                    return fail(&format!("red, nir and swir1 should be found by the bands' names: {roles:?}"));
                }
                next(st, "a series of 3 dates, 5 bands (blue to swir1) on 48 x 32 written and opened, its roles found");
                None
            }
            401 => {
                // MNF of the last date: the window drawn, three components and the denoised bands
                let tool = manifest().tool("mnf")?;
                if st.envi.phase == 0 {
                    let info = self.session.as_ref()?.info.clone();
                    let last = info.decimal_year(2);
                    let mut params: std::collections::BTreeMap<String, Value> =
                        tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
                    params.insert("date".into(), json!(last));
                    params.insert("n_components".into(), json!(3));
                    params.insert("output".into(), json!("both"));
                    *self.zeit.params(tool) = params;
                    self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
                    self.set_tool_open(tool, true);
                    st.envi.phase = 1;
                    st.frames = 0;
                    return None;
                }
                if st.envi.phase == 1 {
                    st.frames += 1;
                    if st.frames < 4 {
                        return None;
                    }
                    let drawn = ctx.memory(|m| m.area_rect(egui::Id::new(("zeit tool", &tool.id)))).map(|r| r.height());
                    if drawn.is_none_or(|h| h < 60.0) {
                        return fail("the MNF window should be drawn");
                    }
                    match self.zeit.tools.get(&tool.id).and_then(|u| u.estimate()) {
                        Some(Ok(e)) => println!("    estimate: {e:?}"),
                        Some(Err(e)) => return fail(&format!("the estimate: {e}")),
                        None => return if waited > 60.0 { fail("the estimate should arrive") } else { None },
                    }
                    self.set_tool_open(tool, false);
                    match self.envi_start(
                        "mnf",
                        json!({ "date": self.session.as_ref()?.info.decimal_year(2),
                                                          "n_components": 3, "output": "both" }),
                        ctx,
                    ) {
                        Ok(run) => st.envi.runs.push(("mnf".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 2;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "mnf")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                let maps: Vec<&crate::zeit::ResultLayer> = self.zeit.results.iter().filter(|r| r.run == run).collect();
                if maps.len() != 3 || maps.iter().any(|m| m.w != W || m.h != H) {
                    return fail(&format!("the MNF should give 3 maps of 48 x 32, got {}", maps.len()));
                }
                let var = |m: &crate::zeit::ResultLayer| {
                    let v: Vec<f64> = m.data.iter().filter(|x| x.is_finite()).map(|&x| f64::from(x)).collect();
                    let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
                    v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len().max(1) as f64
                };
                let (v1, v3) = (var(self.envi_map(&run, "mnf_mnf_1.tif")?), var(self.envi_map(&run, "mnf_mnf_3.tif")?));
                println!("    variance of component 1: {v1:.4}, of component 3: {v3:.4}");
                if !(v1 > v3 && v1 > 0.0) {
                    return fail("component 1 should have a larger variance than component 3");
                }
                let csv = std::fs::read_to_string(Path::new(&run).join("mnf_eigenvalues.csv")).unwrap_or_default();
                let lams: Vec<f64> = csv.lines().skip(1).filter_map(|l| l.split(',').nth(1)?.parse().ok()).collect();
                if lams.len() != NB || lams[0] < 5.0 || lams[0] < lams[NB - 1] {
                    return fail(&format!("the eigenvalues should be {NB}, the first a strong signal: {lams:?}"));
                }
                for f in ["mnf_transform.json", "mnf_components.tif"] {
                    if !Path::new(&run).join(f).is_file() {
                        return fail(&format!("the run's folder should have {f}"));
                    }
                }
                // the denoised bands of one date open as a layer of their own: one file of five bands, which Janus reads as five
                // dates of one band (a stack of variables), the bands named
                if !self.zeit.series_to_open.is_empty() || self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return if waited > 90.0 { fail("the denoised series should open") } else { None };
                }
                if self.layers.len() != st.envi.layers + 2 {
                    return fail(&format!("the denoised bands should open as a layer: {} layers", self.layers.len()));
                }
                let k = self.layers.len() - 1;
                if !crate::ui_test::loaded(self.layer_session(k)) {
                    return None;
                }
                let info = self.layer_session(k)?.info.clone();
                if info.t() != NB || info.bands_per_date != 1 {
                    return fail(&format!(
                        "the denoised bands: {NB} bands of one date, a stack of {NB} layers of 1 band, got {} of {}",
                        info.t(),
                        info.bands_per_date
                    ));
                }
                self.remove_layer(k, ctx);
                self.set_active(st.envi.layers, ctx);
                next(
                    st,
                    "MNF: the window drawn with its estimate, 3 components in variance order, the eigenvalue table, the denoised bands as a series",
                );
                None
            }
            402 => {
                // CVA: the change map and the sectors
                if st.envi.phase == 0 {
                    match self.envi_start("cva", json!({}), ctx) {
                        Ok(run) => st.envi.runs.push(("cva".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 1;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "cva")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                let change = self.envi_map(&run, "cva_change.tif")?;
                let sector = self.envi_map(&run, "cva_sector.tif")?;
                if self.envi_map(&run, "cva_direction.tif").is_some()
                    || self.envi_map(&run, "cva_magnitude.tif").is_none()
                {
                    return fail("the sign sectors replace the angle: a magnitude, a change and sectors");
                }
                let (mut inside, mut flagged, mut outside, mut wrong) = (0, 0, 0, 0);
                for i in 0..W * H {
                    if in_block(i) {
                        inside += 1;
                        flagged += usize::from(change.data[i] == 2.0);
                        // red down, NIR up: the vegetation gain sector, or (where the SWIR went up with the date's gain) the
                        // sector of the NIR and the SWIR up and the red down
                        wrong += usize::from(sector.classes.get(sector.data[i] as usize - 1).is_none_or(|c| {
                            !c.starts_with("Vegetation gain") && !c.starts_with("NIR and SWIR up, red down")
                        }));
                    } else {
                        outside += usize::from(change.data[i] == 2.0);
                    }
                }
                println!(
                    "    CVA: {flagged} of {inside} block pixels changed, {outside} of {} outside; {wrong} of the block with the red not down and the NIR up",
                    W * H - inside
                );
                if flagged * 10 < inside * 9 || outside * 20 > W * H {
                    return fail("the block should be flagged as changed and little else");
                }
                if sector.classes.len() != 9 || change.classes != ["No change", "Change"] || wrong * 10 > inside {
                    return fail("the sectors should name 9 classes, the block's pixels red down and NIR up");
                }
                next(
                    st,
                    "CVA: Otsu's change map flags the block (and little else), its sectors are those of the NIR up and the red down (vegetation gain)",
                );
                None
            }
            403 => {
                // iMAD
                if st.envi.phase == 0 {
                    match self.envi_start("imad", json!({ "n_mad": 2, "alpha": 0.01 }), ctx) {
                        Ok(run) => st.envi.runs.push(("imad".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 1;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "imad")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                let change = self.envi_map(&run, "imad_change.tif")?;
                let chi2 = self.envi_map(&run, "imad_chi2.tif")?;
                let (mut inside, mut flagged, mut outside) = (0, 0, 0);
                let (mut chi_in, mut chi_out) = (0.0f64, 0.0f64);
                for i in 0..W * H {
                    if in_block(i) {
                        inside += 1;
                        flagged += usize::from(change.data[i] == 2.0);
                        chi_in += f64::from(chi2.data[i]);
                    } else {
                        outside += usize::from(change.data[i] == 2.0);
                        chi_out += f64::from(chi2.data[i]);
                    }
                }
                let (chi_in, chi_out) = (chi_in / inside as f64, chi_out / (W * H - inside) as f64);
                println!(
                    "    iMAD: {flagged} of {inside} block pixels changed, {outside} of {} outside; the mean chi-square {chi_in:.1} in the block, {chi_out:.1} outside",
                    W * H - inside
                );
                // (the noise bands of this small synthetic scene make the false alarms of a chi-square with the
                // weighted variance many: what must hold is that the block is flagged and stands out)
                if flagged * 10 < inside * 9 || chi_in < 5.0 * chi_out {
                    return fail("the iMAD should flag the block as changed, with a chi-square well above the rest");
                }
                let mads = self.zeit.results.iter().filter(|r| r.run == run && r.path.contains("imad_mad_")).count();
                if mads != 2
                    || self.envi_map(&run, "imad_p_nochange.tif").is_none()
                    || self.envi_map(&run, "imad_chi2.tif").is_none()
                {
                    return fail(&format!(
                        "the iMAD should give 2 MAD variates, the chi-square and the probability: {mads}"
                    ));
                }
                let rho = std::fs::read_to_string(Path::new(&run).join("imad_rho.csv")).unwrap_or_default();
                if rho.lines().count() < 3 || !Path::new(&run).join("imad.tif").is_file() {
                    return fail("the iMAD's folder should have its iterations' table and the multiband file");
                }
                next(
                    st,
                    &format!(
                        "iMAD: the block flagged ({flagged} of {inside}), 2 MAD variates as layers, {} iterations in the table",
                        rho.lines().count() - 1
                    ),
                );
                None
            }
            404 => {
                // the normalization
                if st.envi.phase == 0 {
                    match self.envi_start("radcal", json!({ "threshold": 0.5 }), ctx) {
                        Ok(run) => st.envi.runs.push(("radcal".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 1;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "radcal")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                if !self.zeit.series_to_open.is_empty() || self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return if waited > 90.0 { fail("the normalized series should open") } else { None };
                }
                if self.layers.len() != st.envi.layers + 2 {
                    return fail(&format!(
                        "the normalized series should open as a layer: {} layers",
                        self.layers.len()
                    ));
                }
                let k = self.layers.len() - 1;
                if !crate::ui_test::loaded(self.layer_session(k)) {
                    return None;
                }
                let info = self.layer_session(k)?.info.clone();
                if info.t() != 3 || info.bands_per_date != NB || !info.time_is_date {
                    return fail(&format!(
                        "the normalized series: 3 dates of {NB} bands, got {} of {}",
                        info.t(),
                        info.bands_per_date
                    ));
                }
                // the second date's gain (1.2) is undone: outside the block, its NIR is nearer the first date's
                let dir = Path::new(&run).join("normalized");
                let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
                    .map(|d| {
                        d.filter_map(|f| f.ok().map(|f| f.path()))
                            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
                            .collect()
                    })
                    .unwrap_or_default();
                files.sort();
                if files.len() != 3 {
                    return fail("the normalized series should be a file a date");
                }
                let reference = read_band(&st.envi.dir.join(format!("envi_{}.tif", DATES[0])), 4).ok()?;
                let raw = read_band(&st.envi.dir.join(format!("envi_{}.tif", DATES[1])), 4).ok()?;
                let normalized = read_band(&files[1], 4).ok()?;
                let err = |a: &[f32]| -> f64 {
                    (0..W * H)
                        .filter(|&i| !in_block(i) && a[i].is_finite())
                        .map(|i| f64::from((a[i] - reference[i]).abs()))
                        .sum::<f64>()
                        / (W * H) as f64
                };
                let (before, after) = (err(&raw), err(&normalized));
                println!(
                    "    normalization: mean |date 2 - reference| in the NIR {before:.4} before, {after:.4} after"
                );
                if after > before / 5.0 {
                    return fail("the normalization should bring date 2 near the reference");
                }
                if read_band(&files[0], 4).ok()? != reference {
                    return fail("the reference should be copied unchanged");
                }
                let fits = std::fs::read_to_string(Path::new(&run).join("radcal_fits.csv")).unwrap_or_default();
                if fits.lines().count() != 1 + 2 * NB {
                    return fail("the table of fits should have a row for each band of the 2 dates");
                }
                self.remove_layer(k, ctx);
                self.set_active(st.envi.layers, ctx);
                next(
                    st,
                    &format!(
                        "normalization: a series of 3 dates opened, date 2 within {after:.4} of the reference (was {before:.4}), the fits' table"
                    ),
                );
                None
            }
            405 => {
                // the Python of the runs
                let mut ran = 0;
                for (id, call) in
                    [("mnf", "zeit.mnf("), ("cva", "zeit.cva("), ("imad", "zeit.imad("), ("radcal", "zeit.radcal(")]
                {
                    let run = st.envi.runs.iter().find(|r| r.0 == id)?.1.clone();
                    let code = std::fs::read_to_string(Path::new(&run).join("rerun.py")).unwrap_or_default();
                    if !code.contains(call) || !code.contains("bands_all") || !code.contains("zeit-cdts>=3.9") {
                        return fail(&format!("{id}'s rerun.py should call {call} on bands_all:\n{code}"));
                    }
                    let Ok(python) = std::env::var("JANUS_PYTHON") else { continue };
                    if id != "mnf" && id != "cva" {
                        continue;
                    }
                    let out =
                        std::process::Command::new(python).arg("-I").arg(Path::new(&run).join("rerun.py")).output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            return fail(&format!("{id}'s rerun.py failed: {}", String::from_utf8_lossy(&o.stderr)));
                        }
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    ran += 1;
                    let from_zeit = Path::new(&run).join("python").join("result.tif");
                    if id == "mnf" {
                        let theirs = read_band(&from_zeit, 1).ok()?;
                        let ours = &self.envi_map(&run, "mnf_mnf_1.tif")?.data;
                        let worst =
                            theirs.iter().zip(ours.iter()).map(|(a, b)| f64::from((a - b).abs())).fold(0.0, f64::max);
                        let scale = theirs.iter().map(|v| f64::from(v.abs())).fold(0.0, f64::max);
                        println!("    Zeit's zeit.mnf against the run: largest difference {worst:.2e} of {scale:.2}");
                        if worst > 1e-4 * scale {
                            return fail("Zeit's own MNF should be what the run wrote");
                        }
                    } else {
                        let theirs = read_band(&from_zeit, 1).ok()?;
                        let ours = &self.envi_map(&run, "cva_magnitude.tif")?.data;
                        let worst =
                            theirs.iter().zip(ours.iter()).map(|(a, b)| f64::from((a - b).abs())).fold(0.0, f64::max);
                        println!(
                            "    Zeit's zeit.cva against the run: largest difference of the magnitude {worst:.2e}"
                        );
                        if worst > 1e-5 {
                            return fail("Zeit's own CVA should be what the run wrote");
                        }
                    }
                }
                next(
                    st,
                    &format!(
                        "Python: the four runs' rerun.py call zeit.mnf, cva, imad and radcal on every band ({ran} run with Zeit, equal)"
                    ),
                );
                None
            }
            406 => {
                for id in ["mnf", "cva", "imad", "radcal"] {
                    if let Some(t) = manifest().tool(id) {
                        self.set_tool_open(t, false);
                    }
                }
                for k in (st.envi.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let runs: Vec<String> = st.envi.runs.iter().map(|r| r.1.clone()).collect();
                self.zeit.results.retain(|r| !runs.contains(&r.run));
                self.zeit.jobs.retain(|j| !runs.contains(&j.output_dir));
                self.zeit.results_dir = st.envi.results_dir.take();
                if st.envi.active < self.layers.len() && st.envi.active != self.active {
                    self.set_active(st.envi.active, ctx);
                }
                let _ = std::fs::remove_dir_all(&st.envi.dir);
                st.envi = Default::default();
                next(st, "MNF, CVA, iMAD and normalization: the windows closed, the layers, runs and files removed");
                st.stage = 407; // the omnibus test (407-409), then 299
                None
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The omnibus change test of Sentinel-1 (stages 407-409)
// ---------------------------------------------------------------------------------------------------------------

const S1_W: usize = 40;
const S1_H: usize = 28;
const S1_T: usize = 14;
/// The date (index) the block's backscatter rose at, and the block.
const S1_STEP: usize = 8;
const S1_BLOCK: (std::ops::Range<usize>, std::ops::Range<usize>) = (12..24, 8..20);
/// The number of looks of the test's speckle (an Erlang of 4 exponentials).
const S1_ENL: f64 = 4.0;

fn s1_day(t: usize) -> i64 {
    18631 + 12 * t as i64
}

fn s1_in_block(i: usize) -> bool {
    S1_BLOCK.0.contains(&(i % S1_W)) && S1_BLOCK.1.contains(&(i / S1_W))
}

/// The series' files in `dir`, a file a date of two bands (VV and VH) in dB: speckled backscatter of a land of 0.10
/// (VV) and 0.02 (VH) in power, a block of which rose sixfold (VV) and fivefold (VH) from the step on.
fn write_s1(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut rng = Lcg(77);
    let mut uniform = move || ((rng.next() + 1.0) / 2.0).clamp(1e-12, 1.0);
    let mut paths = vec![];
    for t in 0..S1_T {
        let (y, m, d) = janus_zeit::numeric::civil(s1_day(t));
        let path = dir.join(format!("s1_{y:04}-{m:02}-{d:02}.tif"));
        let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(e)?;
        let mut ds = drv.create_with_band_type::<f32, _>(&path, S1_W, S1_H, 2).map_err(e)?;
        ds.set_geo_transform(&[600000.0, 10.0, 0.0, 7500000.0, 0.0, -10.0]).map_err(e)?;
        let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32723).and_then(|s| s.to_wkt()).map_err(e)?;
        ds.set_projection(&wkt).map_err(e)?;
        for (b, (name, mean, up)) in [("VV", 0.10, 6.0), ("VH", 0.02, 5.0)].into_iter().enumerate() {
            let v: Vec<f32> = (0..S1_W * S1_H)
                .map(|i| {
                    let m = if t >= S1_STEP && s1_in_block(i) { mean * up } else { mean };
                    let speckle: f64 = (0..4).map(|_| -uniform().ln()).sum::<f64>() / S1_ENL;
                    (10.0 * (m * speckle).log10()) as f32
                })
                .collect();
            let mut band = ds.rasterband(b + 1).map_err(e)?;
            band.set_description(name).map_err(e)?;
            band.set_no_data_value(Some(-9999.0)).map_err(e)?;
            band.write((0, 0), (S1_W, S1_H), &mut gdal::raster::Buffer::new((S1_W, S1_H), v)).map_err(e)?;
        }
        paths.push(path);
    }
    Ok(paths)
}

impl State {
    pub(super) fn test_omnibus(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
            st.envi.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        match stage {
            407 => {
                if st.envi.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.envi = EnviTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-omnibus-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        active: self.active,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.envi.dir);
                }
                if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return None;
                }
                if !st.envi.opened {
                    let files = match write_s1(&st.envi.dir) {
                        Ok(f) => f,
                        Err(e) => return fail(&format!("the radar series could not be written: {e}")),
                    };
                    st.envi.opened = true;
                    self.zeit.results_dir = Some(st.envi.dir.join("results"));
                    self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, ctx);
                    return None;
                }
                if self.layers.len() <= st.envi.layers
                    || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                {
                    return None;
                }
                let tool = manifest().tool("omnibus")?;
                if st.envi.phase == 0 {
                    let k = self.layers.len() - 1;
                    self.set_active(k, ctx);
                    let info = self.layer_session(k)?.info.clone();
                    if info.t() != S1_T
                        || info.bands_per_date != 2
                        || info.width != S1_W
                        || info.height != S1_H
                        || !info.time_is_date
                    {
                        return fail(&format!(
                            "the series should be {S1_T} dates of 2 bands on {S1_W} x {S1_H}: {} dates, {} bands",
                            info.t(),
                            info.bands_per_date
                        ));
                    }
                    let roles = crate::zeit::guess_roles(&info);
                    if roles.get("vv") != Some(&1) || roles.get("vh") != Some(&2) {
                        return fail(&format!("vv and vh should be found by the bands' names: {roles:?}"));
                    }
                    if let Some(why) = crate::zeit::applicability(tool, &info) {
                        return fail(&format!("the omnibus test should apply to the series: {why}"));
                    }
                    match self.envi_start(
                        "omnibus",
                        json!({ "enl": S1_ENL, "alpha": 0.01, "scale": "auto", "per_date": true }),
                        ctx,
                    ) {
                        Ok(run) => st.envi.runs.push(("omnibus".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 1;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "omnibus")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                // the series of directions opens as a layer, hidden
                if !self.zeit.series_to_open.is_empty() || self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                    return if waited > 120.0 { fail("the series of directions should open") } else { None };
                }
                if self.layers.len() != st.envi.layers + 2 {
                    return fail(&format!(
                        "the direction at each date should open as a layer: {} layers",
                        self.layers.len()
                    ));
                }
                let k = self.layers.len() - 1;
                if !crate::ui_test::loaded(self.layer_session(k)) {
                    return None;
                }
                let info = self.layer_session(k)?.info.clone();
                if info.t() != S1_T || info.bands_per_date != 1 {
                    return fail(&format!(
                        "the directions: {S1_T} dates of one band, got {} of {}",
                        info.t(),
                        info.bands_per_date
                    ));
                }
                self.remove_layer(k, ctx);
                self.set_active(st.envi.layers, ctx);
                // a change is found at its own date in part of the pixels and a date or two later in the rest
                let step_year = self.session.as_ref()?.info.decimal_year(S1_STEP) as f32;
                let soon = self.session.as_ref()?.info.decimal_year(S1_STEP + 2) as f32;
                let first = self.envi_map(&run, "omnibus_first_change.tif")?;
                let dir = self.envi_map(&run, "omnibus_first_direction.tif")?;
                let ndates = self.envi_map(&run, "omnibus_n_dates.tif")?;
                let p = self.envi_map(&run, "omnibus_p_change.tif")?;
                if first.unit != "year" || first.w != S1_W || first.h != S1_H {
                    return fail("the first change should be a map of years of the series' size");
                }
                if dir.classes != ["None", "Brightening", "Darkening", "Indefinite"] {
                    return fail(&format!("the direction's classes: {:?}", dir.classes));
                }
                let (mut inside, mut at_step, mut by_then, mut bright, mut outside, mut false_alarms) =
                    (0, 0, 0, 0, 0, 0);
                let (mut p_in, mut p_out) = (0.0f64, 0.0f64);
                for (i, &f) in first.data.iter().enumerate() {
                    if ndates.data[i] != S1_T as f32 {
                        return fail(&format!("every pixel has {S1_T} valid dates: {} at {i}", ndates.data[i]));
                    }
                    if s1_in_block(i) {
                        inside += 1;
                        at_step += usize::from(f == step_year);
                        by_then += usize::from(f >= step_year && f <= soon);
                        bright += usize::from(dir.data[i] == 2.0);
                        p_in += f64::from(p.data[i]);
                    } else {
                        outside += 1;
                        false_alarms += usize::from(f.is_finite());
                        p_out += f64::from(p.data[i]);
                    }
                }
                println!(
                    "    omnibus: {at_step} of {inside} block pixels changed at the step ({by_then} by two dates later, {bright} brightening), {false_alarms} of {outside} outside; the mean p-value {:.4} in the block, {:.3} outside",
                    p_in / inside as f64,
                    p_out / outside as f64
                );
                if at_step * 10 < inside * 6
                    || by_then * 10 < inside * 8
                    || bright * 10 < inside * 8
                    || false_alarms * 10 > outside
                {
                    return fail("the block should change at the step, brightening, and little else");
                }
                let text = std::fs::read_to_string(Path::new(&run).join("omnibus.json")).unwrap_or_default();
                let rec: Value = serde_json::from_str(&text).unwrap_or_default();
                if rec["scale"] != "db" || rec["enl"].as_f64() != Some(S1_ENL) || rec["bands"] != json!(["vv", "vh"]) {
                    return fail(&format!("omnibus.json should say the bands were read in dB: {rec}"));
                }
                let maps = self.zeit.results.iter().filter(|r| r.run == run).count();
                if maps != 8 {
                    return fail(&format!("the test should give 8 maps (and the series as a layer): {maps}"));
                }
                next(
                    st,
                    &format!(
                        "omnibus: the block changed at the step in {at_step} of {inside} pixels, brightening in {bright}, {false_alarms} of {outside} outside; the dB series found by the automatic scale; 8 maps and the series of directions"
                    ),
                );
                None
            }
            408 => {
                // a pin in the block: its chart marks the change at the date the map gives
                if st.envi.phase == 0 {
                    self.clear_pins();
                    let (x, y) = (18, 14);
                    self.add_pin(x, y);
                    let id = self.series.pins.last()?.id;
                    st.envi.pin = Some((id, x, y));
                    self.zeit.fit_only(Some("omnibus"));
                    st.envi.phase = 1;
                    return None;
                }
                let (id, x, y) = st.envi.pin?;
                let cube = self.session.as_ref()?.info.id;
                let run = st.envi.runs.iter().find(|r| r.0 == "omnibus")?.1.clone();
                let fit = match self.zeit.fit_of("omnibus", cube, crate::zeit::Which::Pin(id), x, y) {
                    Some(Ok(f)) => f.clone(),
                    Some(Err(e)) => return fail(&format!("the pin's fit: {e}")),
                    None => return if waited > 60.0 { fail("the pin's fit should arrive") } else { None },
                };
                let lines: Vec<f64> =
                    fit.overlays.iter().filter(|o| o.kind == "vlines").flat_map(|o| o.x.clone()).collect();
                let map_first = self.envi_map(&run, "omnibus_first_change.tif")?.data[y * S1_W + x];
                let first_line = lines.iter().copied().fold(f64::INFINITY, f64::min);
                println!(
                    "    pin ({x}, {y}): {} line(s), the first at {first_line:.4}; the map's first change {map_first:.4}",
                    lines.len()
                );
                if lines.is_empty() || first_line as f32 != map_first {
                    return fail("the pin's first change line should be the date the map gives");
                }
                if !fit.overlays.iter().any(|o| o.label == "Omnibus change (brightening)") {
                    return fail("the change should be marked as a brightening");
                }
                let row = |name: &str| fit.rows.iter().find(|r| r[0] == name).map(|r| r[1].clone()).unwrap_or_default();
                if !row("Scale").starts_with("dB") || !row("First change").contains("brightening") {
                    return fail(&format!("the pin's table: {:?}", fit.rows));
                }
                self.zeit.fit_only(None);
                self.clear_pins();
                // the run's Python, through Zeit
                let code = std::fs::read_to_string(Path::new(&run).join("rerun.py")).unwrap_or_default();
                if !code.contains("zeit.omnibus(")
                    || !code.contains("bands=['b001', 'b002']")
                    || !code.contains("10.0 ** (bands_all / 10.0)")
                    || !code.contains("zeit-cdts>=3.9")
                {
                    return fail(&format!("omnibus's rerun.py should call zeit.omnibus on bands_all:\n{code}"));
                }
                let mut report = "the Python not run (no JANUS_PYTHON)".to_string();
                if let Ok(python) = std::env::var("JANUS_PYTHON") {
                    match std::process::Command::new(python).arg("-I").arg(Path::new(&run).join("rerun.py")).output() {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            return fail(&format!("omnibus's rerun.py failed: {}", String::from_utf8_lossy(&o.stderr)));
                        }
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    // Zeit's eight planes against the run's maps: the dates, the counts and the p-values; the
                    // directions are Zeit's codes, the run's classes (the code plus one)
                    let from_zeit = Path::new(&run).join("python").join("result.tif");
                    let planes = [
                        ("first_change", false),
                        ("last_change", false),
                        ("n_changes", false),
                        ("first_direction", true),
                        ("last_direction", true),
                        ("greatest_change", false),
                        ("p_change", false),
                        ("n_dates", false),
                    ];
                    let mut worst = 0.0f64;
                    let mut differing = 0usize;
                    for (b, (name, class)) in planes.iter().enumerate() {
                        let theirs = match gdal::Dataset::open(&from_zeit)
                            .and_then(|d| d.rasterband(b + 1)?.read_as::<f32>((0, 0), (S1_W, S1_H), (S1_W, S1_H), None))
                        {
                            Ok(v) => v.data().to_vec(),
                            Err(e) => return fail(&format!("Zeit's result.tif, plane {name}: {e}")),
                        };
                        let ours = &self.envi_map(&run, &format!("omnibus_{name}.tif"))?.data;
                        for (a, o) in theirs.iter().zip(ours.iter()) {
                            let o = if *class { o - 1.0 } else { *o };
                            if a.is_nan() && o.is_nan() {
                                continue;
                            }
                            let d = f64::from((a - o).abs());
                            // a pixel on the edge of the test (its p-value or statistic a rounding from the level) may
                            // find a change a date earlier or later: that cell differs
                            if *name != "p_change" && d > 1e-4 {
                                differing += 1;
                            } else {
                                worst = worst.max(d);
                            }
                        }
                    }
                    println!(
                        "    Zeit's zeit.omnibus against the run: {differing} cells differ (of {}), the largest difference of the equal ones {worst:.2e}",
                        8 * S1_W * S1_H
                    );
                    if differing * 100 > 8 * S1_W * S1_H || worst > 1e-3 {
                        return fail("Zeit's own omnibus test should be what the run wrote");
                    }
                    report = format!(
                        "Zeit's own result: {differing} cells of {} differ, the rest within {worst:.1e}",
                        8 * S1_W * S1_H
                    );
                }
                next(
                    st,
                    &format!(
                        "omnibus: a pin in the block marks the change at the map's date, brightening; rerun.py calls zeit.omnibus on the converted bands ({report})"
                    ),
                );
                None
            }
            409 if st.envi.phase < 3 => {
                // a series of one band a date (the VV planes alone): the band is VV, single polarization
                if st.envi.phase == 0 {
                    if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
                        return None;
                    }
                    let one = st.envi.dir.join("vv_only");
                    let _ = std::fs::create_dir_all(&one);
                    for t in 0..S1_T {
                        let (y, m, d) = janus_zeit::numeric::civil(s1_day(t));
                        let name = format!("s1_{y:04}-{m:02}-{d:02}.tif");
                        let write = || -> Result<(), String> {
                            let src = gdal::Dataset::open(st.envi.dir.join(&name)).map_err(|e| e.to_string())?;
                            let v = src
                                .rasterband(1)
                                .and_then(|b| b.read_as::<f32>((0, 0), (S1_W, S1_H), (S1_W, S1_H), None))
                                .map_err(|e| e.to_string())?;
                            let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
                            let mut ds = drv
                                .create_with_band_type::<f32, _>(one.join(&name), S1_W, S1_H, 1)
                                .map_err(|e| e.to_string())?;
                            ds.set_geo_transform(&[600000.0, 10.0, 0.0, 7500000.0, 0.0, -10.0])
                                .map_err(|e| e.to_string())?;
                            let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32723)
                                .and_then(|s| s.to_wkt())
                                .map_err(|e| e.to_string())?;
                            ds.set_projection(&wkt).map_err(|e| e.to_string())?;
                            let mut band = ds.rasterband(1).map_err(|e| e.to_string())?;
                            band.set_no_data_value(Some(-9999.0)).map_err(|e| e.to_string())?;
                            let mut buf = gdal::raster::Buffer::new((S1_W, S1_H), v.data().to_vec());
                            band.write((0, 0), (S1_W, S1_H), &mut buf).map_err(|e| e.to_string())
                        };
                        if let Err(e) = write() {
                            return fail(&format!("the single-band series could not be written: {e}"));
                        }
                    }
                    let files: Vec<String> = (0..S1_T)
                        .map(|t| {
                            let (y, m, d) = janus_zeit::numeric::civil(s1_day(t));
                            one.join(format!("s1_{y:04}-{m:02}-{d:02}.tif")).to_string_lossy().into_owned()
                        })
                        .collect();
                    self.open_inputs(files, true, ctx);
                    st.envi.phase = 1;
                    return None;
                }
                if st.envi.phase == 1 {
                    if self.opening.is_some()
                        || !self.layer_jobs.queue.is_empty()
                        || self.layers.len() != st.envi.layers + 2
                        || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
                    {
                        return None;
                    }
                    let k = self.layers.len() - 1;
                    self.set_active(k, ctx);
                    let info = self.layer_session(k)?.info.clone();
                    if info.t() != S1_T || info.bands_per_date != 1 {
                        return fail(&format!(
                            "the series should be {S1_T} dates of one band: {} of {}",
                            info.t(),
                            info.bands_per_date
                        ));
                    }
                    let tool = manifest().tool("omnibus")?;
                    if self.tool_active_roles(tool).get("vv") != Some(&1) {
                        return fail("the only band of the series should be VV for the omnibus test");
                    }
                    // and only for a tool that needs VV: the series' own roles stay as guessed
                    if self.active_roles().contains_key("vv") {
                        return fail("a series of one band should not have a VV role for the other tools");
                    }
                    if let Some(why) = crate::zeit::applicability(tool, &info) {
                        return fail(&format!("the omnibus test should apply to a series of one band: {why}"));
                    }
                    match self.envi_start("omnibus", json!({ "enl": S1_ENL, "alpha": 0.01, "scale": "auto" }), ctx) {
                        Ok(run) => st.envi.runs.push(("omnibus-vv".into(), run)),
                        Err(e) => return fail(&e),
                    }
                    st.envi.phase = 2;
                    return None;
                }
                let run = st.envi.runs.iter().find(|r| r.0 == "omnibus-vv")?.1.clone();
                match self.envi_done(&run, waited) {
                    Err(e) => return fail(&e),
                    Ok(false) => return None,
                    Ok(true) => {}
                }
                let step_year = self.session.as_ref()?.info.decimal_year(S1_STEP) as f32;
                let soon = self.session.as_ref()?.info.decimal_year(S1_STEP + 2) as f32;
                let first = self.envi_map(&run, "omnibus_first_change.tif")?;
                let ndates = self.envi_map(&run, "omnibus_n_dates.tif")?;
                let (mut inside, mut by_then, mut outside, mut false_alarms) = (0, 0, 0, 0);
                for (i, &f) in first.data.iter().enumerate() {
                    if ndates.data[i] != S1_T as f32 {
                        return fail(&format!("every pixel has {S1_T} valid dates: {} at {i}", ndates.data[i]));
                    }
                    if s1_in_block(i) {
                        inside += 1;
                        by_then += usize::from(f >= step_year && f <= soon);
                    } else {
                        outside += 1;
                        false_alarms += usize::from(f.is_finite());
                    }
                }
                println!(
                    "    omnibus, VV alone: {by_then} of {inside} block pixels by two dates after the step, {false_alarms} of {outside} outside"
                );
                if by_then * 10 < inside * 7 || false_alarms * 10 > outside {
                    return fail("VV alone should find the block at the step, and little else");
                }
                let text = std::fs::read_to_string(Path::new(&run).join("omnibus.json")).unwrap_or_default();
                let rec: Value = serde_json::from_str(&text).unwrap_or_default();
                let code = std::fs::read_to_string(Path::new(&run).join("rerun.py")).unwrap_or_default();
                if rec["bands"] != json!(["vv"]) || !code.contains("bands=['b001']") {
                    return fail(&format!("single polarization: the record {rec}, the script:\n{code}"));
                }
                let mut report = "the Python not run".to_string();
                if let Ok(python) = std::env::var("JANUS_PYTHON") {
                    match std::process::Command::new(python).arg("-I").arg(Path::new(&run).join("rerun.py")).output() {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            return fail(&format!("the VV script failed: {}", String::from_utf8_lossy(&o.stderr)));
                        }
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    let theirs = gdal::Dataset::open(Path::new(&run).join("python").join("result.tif"))
                        .and_then(|d| d.rasterband(1)?.read_as::<f32>((0, 0), (S1_W, S1_H), (S1_W, S1_H), None))
                        .map(|b| b.data().to_vec());
                    let Ok(theirs) = theirs else { return fail("Zeit's result.tif of the VV run should be readable") };
                    let differing = theirs
                        .iter()
                        .zip(&first.data)
                        .filter(|(a, b)| !(a.is_nan() && b.is_nan()) && (*a - *b).abs() > 1e-4)
                        .count();
                    if differing * 100 > S1_W * S1_H {
                        return fail(&format!("Zeit's first change of VV alone differs in {differing} pixels"));
                    }
                    report = format!("Zeit's first change differs in {differing} pixels");
                }
                println!("    single band: {report}");
                st.envi.phase = 3;
                None
            }
            409 => {
                if let Some(t) = manifest().tool("omnibus") {
                    self.set_tool_open(t, false);
                }
                for k in (st.envi.layers..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let runs: Vec<String> = st.envi.runs.iter().map(|r| r.1.clone()).collect();
                self.zeit.results.retain(|r| !runs.contains(&r.run));
                self.zeit.jobs.retain(|j| !runs.contains(&j.output_dir));
                self.zeit.results_dir = st.envi.results_dir.take();
                if st.envi.active < self.layers.len() && st.envi.active != self.active {
                    self.set_active(st.envi.active, ctx);
                }
                let _ = std::fs::remove_dir_all(&st.envi.dir);
                st.envi = Default::default();
                next(st, "omnibus: the layers, the pin, the run and the files removed");
                st.stage = 410; // the menu bar's hover switching (410-412, ui_test_menus.rs), then 299
                None
            }
            _ => None,
        }
    }
}
