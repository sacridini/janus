//! `--selftest-ui` stages 375-379: the co-registration of dates (Tools > Data preparation > Co-register dates) on series
//! the test writes itself: a scene of value noise (continuous, so a shift of a fraction of a pixel is exact) moved by
//! known shifts, 192 x 192 pixels of 30 m, a date a file (NIR, red and a quality band).
//! 375 writes and opens the first series (4 dates, the reference and three moved by (3, -2), (-2.5, 1) and (1, 2) pixels,
//! the fourth with a cloud marked in its quality band); 376 draws the tool's window, runs it (global, the first date as
//! the reference) and waits for the series to open as a layer and the arrows as a vector layer; 377 checks the shifts
//! (whole pixels found to 0.05 pixel; a half pixel, read from the correlation peak's side maxima, to a third), the
//! series written (a date moved by whole pixels is the reference where both have data), the table of the window and,
//! with JANUS_PYTHON, that the run's Python through Zeit finds the same shifts and writes the same series; 378 opens a
//! second series (the reference and two dates moved by a field of shifts), runs the local mode and checks the tie
//! points and the warped dates; 379 the "?" of the window (its page on the site) and puts everything back. Then 385
//! (ui_test_calc_detach.rs).

use std::path::{Path, PathBuf};
use std::time::Instant;

use janus_cube::{BandSelection, QaRule};
use janus_zeit::manifest::manifest;
use serde_json::{Value, json};

use super::UiTest;
use crate::app::State;
use crate::zeit::JobState;

const N: usize = 192;
const DATES: [&str; 4] = ["2022-03-01", "2022-03-17", "2022-04-02", "2022-04-18"];
const LOCAL_DATES: [&str; 3] = ["2022-05-01", "2022-05-17", "2022-06-02"];
/// the amount each date of the first series must be moved to fit the first (x right, y down)
const TRUTH: [(f64, f64); 4] = [(0.0, 0.0), (3.0, -2.0), (-2.5, 1.0), (1.0, 2.0)];

/// The test's state across its stages.
#[derive(Default)]
pub(crate) struct CoregTest {
    dir: PathBuf,
    results_dir: Option<PathBuf>,
    layers: usize,
    vectors: usize,
    active: usize,
    phase: u8,
    run: String,
    /// the layers there were when a series was opened
    open_base: usize,
}

/// Value noise of 5 octaves (lattices of 2 to 32 pixels): continuous.
struct Scene(u64);

impl Scene {
    fn node(&self, octave: u64, ix: i64, iy: i64) -> f64 {
        let mut h = self.0 ^ octave.wrapping_mul(0x9E3779B97F4A7C15);
        h ^= (ix as u64).wrapping_mul(0xBF58476D1CE4E5B9);
        h = h.rotate_left(27).wrapping_mul(0x94D049BB133111EB);
        h ^= (iy as u64).wrapping_mul(0xD6E8FEB86659FD93);
        h ^= h >> 31;
        h = h.wrapping_mul(0x9E3779B97F4A7C15);
        h ^= h >> 29;
        (h >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }

    fn at(&self, x: f64, y: f64) -> f32 {
        let mut v = 0.0;
        for o in 0..5u64 {
            let period = (2u64 << o) as f64;
            let (u, w) = (x / period, y / period);
            let (iu, iw) = (u.floor(), w.floor());
            let (fu, fw) = (u - iu, w - iw);
            let (su, sw) = (fu * fu * (3.0 - 2.0 * fu), fw * fw * (3.0 - 2.0 * fw));
            let n = |dx: i64, dy: i64| self.node(o, iu as i64 + dx, iw as i64 + dy);
            let top = n(0, 0) * (1.0 - su) + n(1, 0) * su;
            let bot = n(0, 1) * (1.0 - su) + n(1, 1) * su;
            v += (top * (1.0 - sw) + bot * sw) * period.sqrt();
        }
        (0.3 + 0.04 * v) as f32
    }

    /// The scene at the pixel centres where the content of the reference is at `(x + dx(x, y), y + dy(x, y))`: to
    /// be moved by (dx, dy) to fit the reference.
    fn image(&self, shift: &dyn Fn(f64, f64) -> (f64, f64)) -> Vec<f32> {
        (0..N * N)
            .map(|i| {
                let (x, y) = ((i % N) as f64 + 0.5, (i / N) as f64 + 0.5);
                let (dx, dy) = shift(x, y);
                self.at(x + dx, y + dy)
            })
            .collect()
    }
}

/// The cloud of the fourth date of the first series.
fn in_cloud(i: usize) -> bool {
    (30..70).contains(&(i % N)) && (30..70).contains(&(i / N))
}

/// The field of shifts of the second series: an affine part and a sinusoid, under a pixel and a half.
fn field(x: f64, y: f64) -> (f64, f64) {
    (0.8 + 0.8 * x / N as f64, -0.5 + 0.3 * (y / 150.0).sin() + 0.4 * y / N as f64)
}

fn write_file(path: &Path, bands: &[(&str, Vec<f32>)]) -> Result<(), String> {
    let e = |e: gdal::errors::GdalError| format!("{}: {e}", path.display());
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(e)?;
    let mut ds = drv.create_with_band_type::<f32, _>(path, N, N, bands.len()).map_err(e)?;
    ds.set_geo_transform(&[600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).map_err(e)?;
    let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32723).and_then(|s| s.to_wkt()).map_err(e)?;
    ds.set_projection(&wkt).map_err(e)?;
    for (k, (name, v)) in bands.iter().enumerate() {
        use gdal::Metadata as _;
        let mut b = ds.rasterband(k + 1).map_err(e)?;
        b.set_description(name).map_err(e)?;
        b.set_no_data_value(Some(-9999.0)).map_err(e)?;
        b.write((0, 0), (N, N), &mut gdal::raster::Buffer::new((N, N), v.clone())).map_err(e)?;
    }
    Ok(())
}

/// The two series' files in `dir`: (the first series', the second's).
fn write_scenes(dir: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    let (nir, red) = (Scene(1), Scene(2));
    let mut first = vec![];
    for (t, date) in DATES.iter().enumerate() {
        let (dx, dy) = TRUTH[t];
        let (mut a, mut b) = (nir.image(&|_, _| (dx, dy)), red.image(&|_, _| (dx, dy)));
        let mut mask = vec![1.0f32; N * N];
        if t == 3 {
            for i in (0..N * N).filter(|&i| in_cloud(i)) {
                (a[i], b[i], mask[i]) = (0.9, 0.9, 0.0);
            }
        }
        let path = dir.join(format!("coreg_{date}.tif"));
        write_file(&path, &[("nir", a), ("red", b), ("mask", mask)])?;
        first.push(path);
    }
    let mut second = vec![];
    for (t, date) in LOCAL_DATES.iter().enumerate() {
        let (a, b) = if t == 0 {
            (nir.image(&|_, _| (0.0, 0.0)), red.image(&|_, _| (0.0, 0.0)))
        } else {
            (nir.image(&field), red.image(&field))
        };
        let path = dir.join(format!("local_{date}.tif"));
        write_file(&path, &[("nir", a), ("red", b), ("mask", vec![1.0; N * N])])?;
        second.push(path);
    }
    Ok((first, second))
}

fn read_band(path: &Path, band: usize) -> Result<Vec<f32>, String> {
    let ds = gdal::Dataset::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let b = ds.rasterband(band).map_err(|e| e.to_string())?;
    Ok(b.read_as::<f32>((0, 0), (N, N), (N, N), None).map_err(|e| e.to_string())?.data().to_vec())
}

/// A run's series files, in date order.
fn series_files(run: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(run.join("series"))
        .map(|d| {
            d.filter_map(|f| f.ok().map(|f| f.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "vrt" || e == "tif"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

/// A CSV's rows, with the quoted fields read.
fn csv_rows(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| {
            let (mut out, mut cur, mut quoted) = (vec![], String::new(), false);
            for c in l.chars() {
                match c {
                    '"' => quoted = !quoted,
                    ',' if !quoted => out.push(std::mem::take(&mut cur)),
                    c => cur.push(c),
                }
            }
            out.push(cur);
            out
        })
        .collect()
}

fn column(rows: &[Vec<String>], name: &str) -> usize {
    rows.first().and_then(|h| h.iter().position(|c| c == name)).unwrap_or(usize::MAX)
}

fn number(row: &[String], at: usize) -> f64 {
    row.get(at).and_then(|v| v.parse::<f64>().ok()).unwrap_or(f64::NAN)
}

impl State {
    /// Opens `files` as a layer with the quality band (the third band, code 0 refused), over two stages' frames.
    fn coreg_open(&mut self, st: &mut UiTest, files: &[PathBuf], ctx: &egui::Context) -> Result<bool, String> {
        if self.opening.is_some() || !self.layer_jobs.queue.is_empty() {
            return Ok(false);
        }
        if st.coreg.phase == 0 {
            st.coreg.open_base = self.layers.len();
            self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, ctx);
            st.coreg.phase = 1;
            return Ok(false);
        }
        if self.layers.len() <= st.coreg.open_base || !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1))
        {
            return Ok(false);
        }
        let k = self.layers.len() - 1;
        if st.coreg.phase == 1 {
            let sel =
                BandSelection { band: 1, qa_band: 3, qa_rule: QaRule::NonZero, chosen: true, ..Default::default() };
            self.reopen_layer(k, sel, ctx);
            st.coreg.phase = 2;
            return Ok(false);
        }
        self.set_active(k, ctx);
        let info = self.layer_session(k).ok_or("the layer is not there")?.info.clone();
        if info.bands_per_date != 3
            || info.sel.qa_band != 3
            || info.width != N
            || info.height != N
            || !info.time_is_date
        {
            return Err(format!(
                "the series should be {} dates of 3 bands (the third the quality band) on {N} x {N}: {} bands, quality band {}",
                files.len(),
                info.bands_per_date,
                info.sel.qa_band
            ));
        }
        if info.t() != files.len() {
            return Err(format!("the series should have {} dates: {}", files.len(), info.t()));
        }
        let roles = crate::zeit::guess_roles(&info);
        if ["red", "nir"].iter().any(|r| !roles.contains_key(*r)) {
            return Err(format!("red and nir should be found by the bands' names: {roles:?}"));
        }
        Ok(true)
    }

    /// The tool's parameters set to the defaults and `extra`, the window open.
    fn coreg_window(&mut self, extra: &[(&str, Value)]) {
        let tool = manifest().tool("coregister").expect("the co-registration is a tool");
        let mut params: std::collections::BTreeMap<String, Value> =
            tool.params.iter().map(|q| (q.id.clone(), q.default.clone())).collect();
        for (k, v) in extra {
            params.insert((*k).to_string(), v.clone());
        }
        *self.zeit.params(tool) = params;
        self.zeit.tools.entry(tool.id.clone()).or_default().scope = 0;
        self.set_tool_open(tool, true);
    }

    /// The run's progress: Ok(Some(dir)) when it is done and its series and arrows are open, Ok(None) while it waits.
    fn coreg_wait(&mut self, st: &UiTest, waited: f64) -> Result<Option<String>, String> {
        let Some(job) = self.zeit.jobs.iter().find(|j| j.output_dir == st.coreg.run) else { return Ok(None) };
        let (state, error) = {
            let Ok(s) = job.status.lock() else { return Ok(None) };
            (s.state, s.error.clone())
        };
        match state {
            JobState::Failed | JobState::Cancelled => return Err(format!("the run should finish: {error}")),
            JobState::Running => return Ok(None),
            JobState::Done => {}
        }
        if !job.loaded
            || !self.zeit.series_to_open.is_empty()
            || self.opening.is_some()
            || !self.layer_jobs.queue.is_empty()
            || !self.vector_loads.is_empty()
        {
            return if waited > 180.0 { Err("the series and the arrows should open".into()) } else { Ok(None) };
        }
        if !crate::ui_test::loaded(self.layer_session(self.layers.len() - 1)) {
            return Ok(None);
        }
        Ok(Some(st.coreg.run.clone()))
    }

    pub(super) fn test_coregister(&mut self, st: &mut UiTest, t0: Instant, ctx: &egui::Context) -> Option<i32> {
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
            st.coreg.phase = 0;
        };
        let waited = st.since.map_or(0.0, |s| s.elapsed().as_secs_f64());
        let tool = manifest().tool("coregister")?;
        match stage {
            375 => {
                if st.coreg.dir.as_os_str().is_empty() {
                    let stamp =
                        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
                    st.coreg = CoregTest {
                        dir: std::env::temp_dir().join(format!("janus-selftest-coregister-{stamp}")),
                        results_dir: self.zeit.results_dir.clone(),
                        layers: self.layers.len(),
                        vectors: self.vectors.len(),
                        active: self.active,
                        ..Default::default()
                    };
                    let _ = std::fs::create_dir_all(&st.coreg.dir);
                    self.zeit.results_dir = Some(st.coreg.dir.join("results"));
                    if let Err(e) = write_scenes(&st.coreg.dir) {
                        return fail(&format!("the test's scenes could not be written: {e}"));
                    }
                }
                let files: Vec<PathBuf> = DATES.iter().map(|d| st.coreg.dir.join(format!("coreg_{d}.tif"))).collect();
                match self.coreg_open(st, &files, ctx) {
                    Err(e) => fail(&e),
                    Ok(false) => None,
                    Ok(true) => {
                        next(
                            st,
                            "4 dates of a scene of value noise (NIR, red and a quality band), the fourth with a cloud, written and opened",
                        );
                        None
                    }
                }
            }
            376 => {
                if st.coreg.phase == 0 {
                    self.coreg_window(&[
                        ("mode", json!("global")),
                        ("reference", json!("date")),
                        ("ref_date", json!(1)),
                        ("window_size", json!(96)),
                        ("mask_buffer", json!(2)),
                    ]);
                    st.coreg.phase = 1;
                    st.frames = 0;
                    return None;
                }
                if st.coreg.phase == 1 {
                    st.frames += 1;
                    if st.frames < 4 {
                        return None;
                    }
                    let drawn = ctx.memory(|m| m.area_rect(egui::Id::new(("zeit tool", &tool.id)))).map(|r| r.height());
                    if drawn.is_none_or(|h| h < 60.0) {
                        return fail("the co-registration's window should be drawn");
                    }
                    match self.zeit.tools.get(&tool.id).and_then(|u| u.estimate()) {
                        Some(Ok(e)) => println!("    estimate: {e:?}"),
                        Some(Err(e)) => return fail(&format!("the estimate: {e}")),
                        None => return if waited > 60.0 { fail("the estimate should arrive") } else { None },
                    }
                    let before = self.zeit.jobs.len();
                    self.run_tool(tool, ctx);
                    if self.zeit.jobs.len() != before + 1
                        || self.zeit.jobs.last().is_none_or(|j| j.tool != "coregister")
                    {
                        return fail(&format!("Run should start the co-registration: {:?}", self.error_popup));
                    }
                    st.coreg.run = self.zeit.jobs.last().map(|j| j.output_dir.clone()).unwrap_or_default();
                    st.coreg.phase = 2;
                    return None;
                }
                match self.coreg_wait(st, waited) {
                    Err(e) => fail(&e),
                    Ok(None) => None,
                    Ok(Some(_)) => {
                        if self.layers.len() != st.coreg.layers + 2 {
                            return fail(&format!(
                                "the co-registered series should open as a layer: {} layers",
                                self.layers.len()
                            ));
                        }
                        self.set_tool_open(tool, false);
                        next(
                            st,
                            "global run (first date as the reference): the series opened as a layer, the arrows as a vector layer",
                        );
                        None
                    }
                }
            }
            377 => {
                let run = PathBuf::from(&st.coreg.run);
                let rows = csv_rows(&run.join("shifts.csv"));
                if rows.len() != 5 {
                    return fail(&format!("shifts.csv should have a row for each of the 4 dates: {}", rows.len()));
                }
                let (dx, dy, applied, rel) = (
                    column(&rows, "x_shift_px"),
                    column(&rows, "y_shift_px"),
                    column(&rows, "applied"),
                    column(&rows, "reliability"),
                );
                let mut worst_whole = 0.0f64;
                let mut worst_half = 0.0f64;
                for (t, (tx, ty)) in TRUTH.iter().enumerate().skip(1) {
                    let row = &rows[1 + t];
                    if row[applied] != "true" {
                        return fail(&format!("date {} should have been moved: {row:?}", t + 1));
                    }
                    let (ex, ey) = ((number(row, dx) - tx).abs(), (number(row, dy) - ty).abs());
                    // whole pixels exactly; the half pixel is read from the peak's side maxima
                    for (e, truth) in [(ex, *tx), (ey, *ty)] {
                        if truth.fract() == 0.0 {
                            worst_whole = worst_whole.max(e);
                        } else {
                            worst_half = worst_half.max(e);
                        }
                    }
                    if number(row, rel) < 40.0 {
                        return fail(&format!("the reliability of date {} is low: {row:?}", t + 1));
                    }
                }
                println!("    shifts: whole pixels off by {worst_whole:.4}, the half pixel by {worst_half:.3}");
                if worst_whole > 0.05 || worst_half > 0.35 {
                    return fail(&format!("the shifts found are off the truth: {rows:?}"));
                }
                // the series written: a date moved by whole pixels is the reference where both have data
                let files = series_files(&run);
                if files.len() != 4 || files[0].extension().is_none_or(|e| e != "vrt") {
                    return fail(&format!("the series should be 4 warped VRTs: {files:?}"));
                }
                let reference = Scene(1).image(&|_, _| (0.0, 0.0));
                for t in [1usize, 3] {
                    let aligned = read_band(&files[t], 1).ok()?;
                    let refused = read_band(&files[t], 3).ok()?;
                    let mut compared = 0;
                    for i in (0..N * N).filter(|&i| (12..N - 12).contains(&(i % N)) && (12..N - 12).contains(&(i / N)))
                    {
                        // (the cloud's own pixels, and the ring where a shift that is not quite whole decides between two)
                        let cloud = t == 3 && (26..76).contains(&(i % N)) && (26..76).contains(&(i / N));
                        if aligned[i].is_finite() && refused[i] != 0.0 && !cloud {
                            compared += 1;
                            if (aligned[i] - reference[i]).abs() > 0.01 * reference[i].abs() {
                                return fail(&format!(
                                    "date {} moved should be the reference at pixel {i}: {} against {}",
                                    t + 1,
                                    aligned[i],
                                    reference[i]
                                ));
                            }
                        }
                    }
                    if compared < 20000 {
                        return fail(&format!("date {} moved has too few pixels: {compared}", t + 1));
                    }
                }
                // the quality band kept as the last band, the cloud still marked
                let qa = read_band(&files[3], 3).ok()?;
                if qa.iter().filter(|&&c| c == 0.0).count() < 1400 {
                    return fail("the quality band of date 4 should still mark its cloud");
                }
                // the arrows: a line for each date moved
                let Some(v) = self.vectors.iter().find(|v| v.data.path.ends_with("shifts.gpkg")) else {
                    return fail("the arrows should be open as a vector layer");
                };
                if v.data.lines != 3 {
                    return fail(&format!("the arrows should be 3 lines, one a date moved: {}", v.data.kind_text()));
                }
                // the window's table of the run
                let table = crate::coregister_ui::rows_of(&st.coreg.run);
                if table.len() != 4 || table.iter().filter(|r| r.applied).count() != 3 || !table[0].reference {
                    return fail(&format!("the window's table should have 4 dates, 3 moved: {table:?}"));
                }
                for f in ["report.txt", "coregister.json"] {
                    if !run.join(f).is_file() {
                        return fail(&format!("the run's folder should have {f}"));
                    }
                }
                // Copy Python: the script calls zeit.coregister (and, with Zeit, finds the shifts and writes the series)
                let code = std::fs::read_to_string(run.join("rerun.py")).unwrap_or_default();
                if !code.contains("zeit.coregister(")
                    || !code.contains("window_size=96")
                    || !code.contains("zeit-cdts>=3.2")
                {
                    return fail(&format!("rerun.py should call zeit.coregister:\n{code}"));
                }
                let mut with_zeit = String::from(" (no JANUS_PYTHON: not run)");
                if let Ok(python) = std::env::var("JANUS_PYTHON") {
                    let cwd = run.join("python");
                    let _ = std::fs::create_dir_all(&cwd);
                    let out = std::process::Command::new(python)
                        .arg("-I")
                        .arg(run.join("rerun.py"))
                        .current_dir(&cwd)
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => return fail(&format!("rerun.py failed: {}", String::from_utf8_lossy(&o.stderr))),
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    let theirs = csv_rows(&cwd.join("shifts.csv"));
                    if theirs.len() != 5 {
                        return fail(&format!("Zeit's table should have 4 dates: {theirs:?}"));
                    }
                    let (tx, ty) = (column(&theirs, "x_shift_px"), column(&theirs, "y_shift_px"));
                    let mut shifts = 0.0f64;
                    // (the dates without the cloud: Janus moves the window of a date with a cloud near the middle off it)
                    for t in 1..3 {
                        shifts = shifts
                            .max((number(&theirs[1 + t], tx) - number(&rows[1 + t], dx)).abs())
                            .max((number(&theirs[1 + t], ty) - number(&rows[1 + t], dy)).abs());
                    }
                    // Zeit's cube is (time, band, y, x) of the two bands: the file's bands run date by date, 2 of each
                    let result = cwd.join("result.tif");
                    let mut worst = 0.0f64;
                    // (the cloud of date 4 is NaN in Zeit's cube, with a margin; GDAL's cubic reads it near its edge)
                    for (t, file) in files.iter().enumerate().filter(|(t, _)| *t != 3) {
                        for b in 0..2 {
                            let (z, o) = (read_band(&result, t * 2 + b + 1).ok()?, read_band(file, b + 1).ok()?);
                            for (x, y) in z.iter().zip(&o).filter(|(x, y)| x.is_finite() && y.is_finite()) {
                                worst = worst.max(f64::from((x - y).abs()));
                            }
                        }
                    }
                    println!(
                        "    Zeit's zeit.coregister against the run: shifts differ by {shifts:.2e}, series by {worst:.2e}"
                    );
                    if shifts > 1e-9 || worst > 2e-3 {
                        return fail("Zeit's own co-registration should find the run's shifts and write its series");
                    }
                    with_zeit = format!(
                        ": Zeit's own call finds the same shifts (off by {shifts:.1e}) and series (off by {worst:.1e})"
                    );
                }
                next(
                    st,
                    &format!(
                        "global co-registration: whole pixels found to {worst_whole:.3}, the half pixel to {worst_half:.2}; the series moved, the cloud kept, the arrows, the window's table, rerun.py{with_zeit}"
                    ),
                );
                None
            }
            378 => {
                if st.coreg.phase <= 2 {
                    // the second series
                    let files: Vec<PathBuf> =
                        LOCAL_DATES.iter().map(|d| st.coreg.dir.join(format!("local_{d}.tif"))).collect();
                    match self.coreg_open(st, &files, ctx) {
                        Err(e) => return fail(&e),
                        Ok(false) => return None,
                        Ok(true) => {}
                    }
                    self.coreg_window(&[
                        ("mode", json!("local")),
                        ("reference", json!("date")),
                        ("ref_date", json!(1)),
                        ("window_size", json!(96)),
                        ("grid_res", json!(48)),
                        ("mask_buffer", json!(2)),
                    ]);
                    st.coreg.phase = 3;
                    st.frames = 0;
                    return None;
                }
                if st.coreg.phase == 3 {
                    st.frames += 1;
                    if st.frames < 4 {
                        return None;
                    }
                    let before = self.zeit.jobs.len();
                    self.run_tool(tool, ctx);
                    if self.zeit.jobs.len() != before + 1
                        || self.zeit.jobs.last().is_none_or(|j| j.tool != "coregister")
                    {
                        return fail(&format!("Run should start the local co-registration: {:?}", self.error_popup));
                    }
                    st.coreg.run = self.zeit.jobs.last().map(|j| j.output_dir.clone()).unwrap_or_default();
                    st.coreg.phase = 4;
                    return None;
                }
                match self.coreg_wait(st, waited) {
                    Err(e) => return fail(&e),
                    Ok(None) => return None,
                    Ok(Some(_)) => {}
                }
                let run = PathBuf::from(&st.coreg.run);
                let rows = csv_rows(&run.join("shifts.csv"));
                let (applied, used) = (column(&rows, "applied"), column(&rows, "used"));
                if rows.len() != 4 || rows[1][applied] != "false" || rows[2..].iter().any(|r| r[applied] != "true") {
                    return fail(&format!("the two moved dates should be applied, the reference not: {rows:?}"));
                }
                let used_n: Vec<usize> = rows[2..].iter().map(|r| r[used].parse().unwrap_or(0)).collect();
                if used_n.iter().any(|&u| u < 12) {
                    return fail(&format!("each date should have at least 12 tie points used: {used_n:?}"));
                }
                // the tie points follow the field where their window is whole
                let points = csv_rows(&run.join("tie_points.csv"));
                let (px, py, sx, sy, is_used, win) = (
                    column(&points, "col"),
                    column(&points, "row"),
                    column(&points, "x_shift_px"),
                    column(&points, "y_shift_px"),
                    column(&points, "used"),
                    column(&points, "win_size_x"),
                );
                // (the part of a pixel is read from a correlation peak of 64 pixels over a field that shears the window: a
                // few tenths of a pixel off at most points, up to a pixel at a few)
                let mut errors: Vec<f64> = vec![];
                for r in points.iter().skip(1).filter(|r| r[is_used] == "true" && number(r, win) >= 90.0) {
                    let (fx, fy) = field(number(r, px), number(r, py));
                    errors.push((number(r, sx) - fx).abs().max((number(r, sy) - fy).abs()));
                }
                errors.sort_by(f64::total_cmp);
                let checked = errors.len();
                let worst = errors.last().copied().unwrap_or(f64::NAN);
                let median = errors.get(checked / 2).copied().unwrap_or(f64::NAN);
                println!(
                    "    tie points: {used_n:?} used, {checked} with a whole window follow the field to {median:.3} (median) and {worst:.3} (worst)"
                );
                if checked < 8 || median > 0.4 || worst > 1.0 {
                    return fail(&format!(
                        "the tie points should follow the field: {checked} checked, off by {median} (median), {worst}"
                    ));
                }
                // the warped dates are nearer the reference than the dates were
                let files = series_files(&run);
                if files.len() != 3 {
                    return fail(&format!("the series should have 3 dates: {files:?}"));
                }
                let reference = read_band(&st.coreg.dir.join(format!("local_{}.tif", LOCAL_DATES[0])), 1).ok()?;
                for t in 1..3 {
                    let before = read_band(&st.coreg.dir.join(format!("local_{}.tif", LOCAL_DATES[t])), 1).ok()?;
                    let after = read_band(&files[t], 1).ok()?;
                    let rms = |a: &[f32]| {
                        let v: Vec<f64> = (0..N * N)
                            .filter(|&i| {
                                (24..N - 24).contains(&(i % N)) && (24..N - 24).contains(&(i / N)) && a[i].is_finite()
                            })
                            .map(|i| f64::from(a[i] - reference[i]).powi(2))
                            .collect();
                        (v.iter().sum::<f64>() / v.len().max(1) as f64).sqrt()
                    };
                    println!(
                        "    date {} warped: rms to the reference {:.4} -> {:.4}",
                        t + 1,
                        rms(&before),
                        rms(&after)
                    );
                    if rms(&after) > 0.8 * rms(&before) {
                        return fail(&format!("date {} warped should be nearer the reference", t + 1));
                    }
                }
                let Some(v) = self.vectors.iter().rev().find(|v| v.data.path.ends_with("shifts.gpkg")) else {
                    return fail("the tie points should be open as a vector layer");
                };
                if v.data.lines < 2 * 12 {
                    return fail(&format!("the tie points' arrows: {}", v.data.kind_text()));
                }
                let code = std::fs::read_to_string(run.join("rerun.py")).unwrap_or_default();
                if !code.contains("zeit.coregister_local(") {
                    return fail(&format!("rerun.py should call zeit.coregister_local:\n{code}"));
                }
                let mut with_zeit = String::from(" (no JANUS_PYTHON: not run)");
                if let Ok(python) = std::env::var("JANUS_PYTHON") {
                    let cwd = run.join("python");
                    let _ = std::fs::create_dir_all(&cwd);
                    match std::process::Command::new(python)
                        .arg("-I")
                        .arg(run.join("rerun.py"))
                        .current_dir(&cwd)
                        .output()
                    {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => return fail(&format!("rerun.py failed: {}", String::from_utf8_lossy(&o.stderr))),
                        Err(e) => return fail(&format!("python: {e}")),
                    }
                    let theirs = csv_rows(&cwd.join("tie_points.csv"));
                    let (tused, tdate) = (column(&theirs, "used"), column(&theirs, "time"));
                    // (Zeit also matches the reference to itself)
                    let count = |rows: &[Vec<String>], used: usize, date: usize| {
                        rows.iter()
                            .skip(1)
                            .filter(|r| r[used].eq_ignore_ascii_case("true") && !r[date].starts_with(LOCAL_DATES[0]))
                            .count()
                    };
                    let (mine, zeit) = (count(&points, is_used, 0), count(&theirs, tused, tdate));
                    println!("    Zeit's coregister_local: {zeit} tie points used, the run {mine}");
                    if mine != zeit {
                        return fail(&format!(
                            "Zeit's own local co-registration should use the run's {mine} tie points: {zeit}"
                        ));
                    }
                    with_zeit = format!(": Zeit's own call uses the same {zeit} tie points");
                }
                next(
                    st,
                    &format!(
                        "local co-registration: {used_n:?} tie points used, they follow the field to {median:.2} (median), the dates warped nearer the reference, the arrows, rerun.py{with_zeit}"
                    ),
                );
                None
            }
            379 => {
                // the "?" of the window opens its section of the guide (no browser: the address is kept)
                if st.coreg.phase == 0 {
                    crate::welcome::test_hooks::start();
                    crate::welcome::test_hooks::press("guide:coregister");
                    self.set_tool_open(tool, true);
                    st.coreg.phase = 1;
                    st.frames = 0;
                    return None;
                }
                st.frames += 1;
                if st.frames < 5 {
                    return None;
                }
                let urls = crate::welcome::test_hooks::urls();
                crate::welcome::test_hooks::stop();
                if !urls.iter().any(|u| u.ends_with("data-preparation.html#co-registration")) {
                    return fail(&format!(
                        "the ? of the window should open data-preparation.html#co-registration: {urls:?}"
                    ));
                }
                self.set_tool_open(tool, false);
                for k in (st.coreg.vectors..self.vectors.len()).rev() {
                    self.remove_vector(k);
                }
                for k in (st.coreg.layers.min(self.layers.len())..self.layers.len()).rev() {
                    self.remove_layer(k, ctx);
                }
                let dir = st.coreg.dir.to_string_lossy().to_string();
                self.zeit.results.retain(|r| !r.run.starts_with(&dir));
                self.zeit.jobs.retain(|j| !j.output_dir.starts_with(&dir));
                self.zeit.results_dir = st.coreg.results_dir.take();
                if st.coreg.active < self.layers.len() && st.coreg.active != self.active {
                    self.set_active(st.coreg.active, ctx);
                }
                let _ = std::fs::remove_dir_all(&st.coreg.dir);
                st.coreg = Default::default();
                next(
                    st,
                    "co-registration: the ? opens data-preparation.html#co-registration; the window closed, the layers, runs and files removed",
                );
                st.stage = 385; // the detached calculator (385-388, ui_test_calc_detach.rs), then 299
                None
            }
            _ => None,
        }
    }
}
