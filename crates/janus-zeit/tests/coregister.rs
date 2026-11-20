//! Co-registration of the dates (Tools > Data preparation > Co-register dates): synthetic dates, a continuous scene
//! (value noise, so a shift of a fraction of a pixel is exact) moved by known shifts, through `run_job`; the shifts
//! must be zeit-core's, called directly on the same arrays (bit for bit), and near the truth; the series that is
//! written must be the reference again (a whole-pixel shift copies the cells), and finding the shift again on it
//! gives none.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager, Metadata};
use janus_zeit::job::{JobResult, JobSpec, OtherSeries, estimate, run_job};
use serde_json::{Value, json};
use zeit_core::coreg::{self, Image};
use zeit_core::coreg_local::{self, Geo, LocalParams};

const W: usize = 384;
const H: usize = 384;
const N: usize = W * H;
const T: usize = 4;
/// the amount each date must be moved to fit the reference (x right, y down)
const TRUTH: [(f64, f64); T] = [(0.0, 0.0), (3.0, -2.0), (0.4, 1.7), (-2.25, 1.1)];
const GT: [f64; 6] = [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
/// the blob of cloud over the middle of the window of the second date: x, y, side
const BLOB: (usize, usize, usize) = (142, 142, 100);

/// A textured scene: value noise of 5 octaves (lattices of 2 to 32 pixels), continuous, so a scene moved by a fraction
/// of a pixel is exact. `at(x, y)` is in pixel coordinates.
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

    /// The scene at the pixel centres, its content moved by (dx, dy) pixels (right and down for positive).
    fn image(&self, dx: f64, dy: f64) -> Vec<f32> {
        (0..N).map(|i| self.at((i % W) as f64 + 0.5 - dx, (i / W) as f64 + 0.5 - dy)).collect()
    }
}

fn write_tif(path: &Path, bands: &[Vec<f32>]) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(path, W, H, bands.len()).unwrap();
    ds.set_geo_transform(&GT).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
    for (k, v) in bands.iter().enumerate() {
        let mut band = ds.rasterband(k + 1).unwrap();
        band.set_no_data_value(Some(-9999.0)).unwrap();
        band.write((0, 0), (W, H), &mut Buffer::new((W, H), v.clone())).unwrap();
    }
}

fn in_blob(i: usize) -> bool {
    let (x, y) = (i % W, i / W);
    (BLOB.0..BLOB.0 + BLOB.2).contains(&x) && (BLOB.1..BLOB.1 + BLOB.2).contains(&y)
}

struct Fixture {
    dir: PathBuf,
    /// [date][pixel] of the matching band and the second band
    nir: Vec<Vec<f32>>,
    #[allow(dead_code)]
    red: Vec<Vec<f32>>,
    /// the quality band's codes of each date
    codes: Vec<Vec<f32>>,
    spec: JobSpec,
}

/// Four dates of two bands (`nir`, `red`: two scenes, moved together) with a quality band (Fmask codes): the second
/// date has a cloud over the middle, where it shows the reference's own content (the scene as it is not moved), so a
/// match that does not leave it out is pulled to no shift.
fn fixture(name: &str, quality: bool) -> Fixture {
    fixture_with(name, quality, &TRUTH, None)
}

/// The same with the dates moved by `truth`, or by the field `warp` (a function of the pixel's column and row giving
/// the shift there).
fn fixture_with(
    name: &str,
    quality: bool,
    truth: &[(f64, f64)],
    field: Option<&dyn Fn(f64, f64) -> (f64, f64)>,
) -> Fixture {
    let dir = std::env::temp_dir().join(format!("janus-zeit-coregister-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (s_nir, s_red) = (Scene(1), Scene(2));
    let moved = |s: &Scene, k: usize| -> Vec<f32> {
        match field {
            Some(f) if k > 0 => (0..N)
                .map(|i| {
                    let (x, y) = ((i % W) as f64 + 0.5, (i / W) as f64 + 0.5);
                    let (dx, dy) = f(x, y);
                    s.at(x + dx, y + dy)
                })
                .collect(),
            _ => s.image(-truth[k].0, -truth[k].1),
        }
    };
    let mut nir: Vec<Vec<f32>> = (0..T).map(|k| moved(&s_nir, k)).collect();
    let red: Vec<Vec<f32>> = (0..T).map(|k| moved(&s_red, k)).collect();
    let mut codes = vec![vec![0f32; N]; T];
    if quality {
        let reference = s_nir.image(0.0, 0.0);
        for i in (0..N).filter(|&i| in_blob(i)) {
            codes[1][i] = 4.0;
            // the reference's own content, with more contrast than the rest of the scene, to dominate the spectrum
            nir[1][i] = 0.3 + 4.0 * (reference[i] - 0.3);
        }
    }
    let mut spec = JobSpec::default();
    let files = [("nir", &nir), ("red", &red)];
    for (b, (role, dates)) in files.iter().enumerate() {
        let path = dir.join(format!("{role}.tif"));
        write_tif(&path, dates);
        let path = path.to_string_lossy().to_string();
        let id = format!("b{:03}", b + 1);
        spec.bands.insert(id.clone(), path.clone());
        spec.bands.insert(role.to_string(), path.clone());
        spec.band_names.insert(id, role.to_string());
        if *role == "nir" {
            spec.input = path;
        }
    }
    if quality {
        let path = dir.join("qa.tif");
        write_tif(&path, &codes);
        spec.qa_input = Some(path.to_string_lossy().to_string());
        spec.qa_rule = Some("fmask".into());
        spec.band_names.insert("qa".into(), "Fmask".into());
    }
    let days: Vec<i64> = vec![19000, 19016, 19032, 19048];
    spec.years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
    spec.days = Some(days);
    spec.threads = Some(2);
    spec.output_dir = dir.join("out").to_string_lossy().to_string();
    Fixture { dir, nir, red, codes, spec }
}

fn params(extra: &[(&str, Value)]) -> BTreeMap<String, Value> {
    let mut p: BTreeMap<String, Value> = [
        ("mode".to_string(), json!("global")),
        ("reference".to_string(), json!("date")),
        ("ref_date".to_string(), json!(1)),
        ("mask_buffer".to_string(), json!(0)),
        ("format".to_string(), json!("vrt")),
    ]
    .into();
    for (k, v) in extra {
        p.insert(k.to_string(), v.clone());
    }
    p
}

impl Fixture {
    fn run(&self, extra: &[(&str, Value)]) -> Result<JobResult, String> {
        let mut spec = self.spec.clone();
        spec.tool = "coregister".into();
        spec.params = params(extra);
        assert!(estimate(&spec).is_ok());
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false))
    }

    fn out(&self, file: &str) -> PathBuf {
        self.dir.join("out").join(file)
    }

    /// The series' file of date `t`.
    fn date_file(&self, t: usize) -> PathBuf {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.out("series"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "vrt" || e == "tif"))
            .collect();
        files.sort();
        files[t].clone()
    }
}

fn read(path: &Path, band: usize) -> Vec<f32> {
    let ds = Dataset::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h) = ds.raster_size();
    ds.rasterband(band).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
}

/// A CSV's rows, with the quoted fields read.
fn csv(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap()
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
    rows[0].iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {:?}", rows[0]))
}

fn number(row: &[String], at: usize) -> f64 {
    row[at].parse::<f64>().unwrap_or(f64::NAN)
}

fn same_bits(a: f64, b: f64, what: &str) {
    assert!(a.to_bits() == b.to_bits() || (a.is_nan() && b.is_nan()), "{what}: {a} against {b}");
}

/// Zeit-core's answer on one date: the arrays as the run builds them (finite and kept by the quality band).
fn core_global(f: &Fixture, date: usize) -> coreg::Outcome {
    let valid = |d: usize| -> Vec<bool> { (0..N).map(|i| matches!(f.codes[d][i] as i64, 0 | 1)).collect() };
    let (rv, tv) = (valid(0), valid(date));
    let rd: Vec<f32> = f.nir[0].iter().zip(&rv).map(|(&v, &ok)| if ok { v } else { 0.0 }).collect();
    let td: Vec<f32> = f.nir[date].iter().zip(&tv).map(|(&v, &ok)| if ok { v } else { 0.0 }).collect();
    let p = coreg::Params { cell: [30.0, 30.0], ..Default::default() };
    coreg::coregister(
        &Image { data: &rd, valid: &rv, ny: H, nx: W },
        &Image { data: &td, valid: &tv, ny: H, nx: W },
        &p,
    )
}

/// The global shifts are zeit-core's, bit for bit, and the scene's known ones within a tenth of a pixel; the
/// reference date is left as it is.
#[test]
fn the_shifts_are_the_cores_and_the_truth() {
    let f = fixture("global", true);
    let r = f.run(&[]).unwrap();
    let rows = csv(&f.out("shifts.csv"));
    assert_eq!(rows.len(), 1 + T);
    let (dx, dy, rel, applied, why, win) = (
        column(&rows, "x_shift_px"),
        column(&rows, "y_shift_px"),
        column(&rows, "reliability"),
        column(&rows, "applied"),
        column(&rows, "reason"),
        column(&rows, "window_x0"),
    );
    assert_eq!(rows[1][applied], "false");
    assert_eq!(rows[1][why], "the reference");
    for d in 1..T {
        let core = core_global(&f, d);
        let row = &rows[1 + d];
        if d == 1 {
            // the cloud is at the middle of what the dates share: the core cannot place its window there, and the
            // run puts it on the largest clear square
            assert_eq!(core.failure, Some(coreg::Failure::WindowTooSmall));
            assert_eq!(row[applied], "true", "{row:?}");
        } else {
            assert!(core.success, "{:?}", core.failure);
            same_bits(number(row, dx), core.x_shift, &format!("dx of date {d}"));
            same_bits(number(row, dy), core.y_shift, &format!("dy of date {d}"));
            same_bits(number(row, rel), core.reliability, &format!("reliability of date {d}"));
            assert_eq!(row[win].parse::<usize>().unwrap(), core.window[0]);
            assert_eq!(row[applied], "true");
        }
        let (tx, ty) = TRUTH[d];
        // the phase correlation's part of a pixel is good to about a tenth
        assert!((number(row, dx) - tx).abs() < 0.2 && (number(row, dy) - ty).abs() < 0.2, "date {d}: {row:?}");
    }
    // the series, the arrows and the report
    let series = r.outputs.iter().find(|o| o["id"] == "aligned").unwrap();
    assert!(series["series"] == json!(true) && Path::new(series["path"].as_str().unwrap()).ends_with("series"));
    assert_eq!(series["qa_band"], json!(3));
    assert_eq!(series["qa_rule"], json!("fmask"));
    let arrows = r.outputs.iter().find(|o| o["id"] == "shifts").unwrap();
    assert_eq!(arrows["vector"], json!(true));
    let ds = Dataset::open(arrows["path"].as_str().unwrap()).unwrap();
    let layer = ds.layer(0).unwrap();
    use gdal::vector::LayerAccess;
    assert_eq!(layer.feature_count(), 3);
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("3 of 4 dates moved") && report.contains("AROSICS"), "{report}");
    assert!(report.contains("2022-01-24: a cloud is near the middle"), "{report}");
}

/// A shift of whole pixels copies the cells: the date moved is the reference where both have data. The others are the
/// reference within what a cubic convolution of this rough a scene gives. The quality band moves by nearest neighbour.
#[test]
fn the_series_written_is_the_reference_again() {
    let f = fixture("applied", true);
    f.run(&[]).unwrap();
    let reference = &f.nir[0];
    let inside = |i: usize, margin: usize| {
        let (x, y) = (i % W, i / W);
        (margin..W - margin).contains(&x) && (margin..H - margin).contains(&y)
    };
    // the cloud of that date and its moved copy
    let near_blob = |i: usize| {
        (BLOB.0 - 4..BLOB.0 + BLOB.2 + 8).contains(&(i % W)) && (BLOB.1 - 6..BLOB.1 + BLOB.2 + 4).contains(&(i / W))
    };
    // the date of the whole pixels: equal to the reference up to float rounding
    let file = f.date_file(1);
    let ds = Dataset::open(&file).unwrap();
    assert_eq!(ds.raster_count(), 3, "the bands and the quality band");
    assert_eq!(ds.rasterband(3).unwrap().description().unwrap(), "Fmask");
    let nir = read(&file, 1);
    let worst =
        (0..N).filter(|&i| inside(i, 8) && !near_blob(i)).map(|i| (nir[i] - reference[i]).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "worst difference {worst}");
    // the second band is moved together with the first
    let red = read(&file, 2);
    let want = Scene(2).image(0.0, 0.0);
    let worst = (0..N).filter(|&i| inside(i, 8)).map(|i| (red[i] - want[i]).abs()).fold(0.0f32, f32::max);
    assert!(worst < 1e-4, "red: {worst}");
    // the quality band: the cloud moved by (3, -2) with its codes whole, the pixels brought in from outside refused (255)
    let qa = read(&file, 3);
    assert!(qa.iter().all(|c| [0.0, 4.0, 255.0].contains(c)));
    assert_eq!(qa[(BLOB.1 - 2 + 5) * W + BLOB.0 + 3 + 5], 4.0);
    assert_eq!(qa[(BLOB.1 + 30) * W + BLOB.0 + 1], 0.0, "the old place of the cloud is clear now");
    assert_eq!(qa[(H - 1) * W + 20], 255.0, "the rows that came in from below");
    assert!(nir[(H - 1) * W + 20].is_nan());
    // the fractional dates: the rms of the difference to the reference, against the one of the date unmoved
    for d in [2, 3] {
        let nir = read(&f.date_file(d), 1);
        let rms = |a: &[f32], b: &[f32]| {
            let v: Vec<f64> = (0..N).filter(|&i| inside(i, 12)).map(|i| f64::from(a[i] - b[i]).powi(2)).collect();
            (v.iter().sum::<f64>() / v.len() as f64).sqrt()
        };
        let (after, before) = (rms(&nir, reference), rms(&f.nir[d], reference));
        assert!(after < 0.2 * before, "date {d}: {after} against {before}");
    }
    // the shift found again on the series written: none
    for (d, &(tx, ty)) in TRUTH.iter().enumerate().skip(1) {
        let nir = read(&f.date_file(d), 1);
        // (the cloud of the second date is left out, as the run leaves it)
        let valid: Vec<bool> =
            nir.iter().enumerate().map(|(i, v)| v.is_finite() && !(d == 1 && near_blob(i))).collect();
        let data: Vec<f32> = nir.iter().map(|&v| if v.is_finite() { v } else { 0.0 }).collect();
        let rv: Vec<bool> = (0..N).map(|i| !(d == 1 && near_blob(i))).collect();
        // the window on the left of the cloud for the second date
        let p = coreg::Params {
            window_pos: (d == 1).then_some((70.0, 192.0)),
            window_size: if d == 1 { 128 } else { 256 },
            ..Default::default()
        };
        let o = coreg::coregister(
            &Image { data: reference, valid: &rv, ny: H, nx: W },
            &Image { data: &data, valid: &valid, ny: H, nx: W },
            &p,
        );
        assert!(o.success, "date {d}: {:?}", o.failure);
        // a shift of whole pixels is found again exactly; the part of a pixel is read from the peak's side maxima, good to
        // about a tenth, so what was left of its error is found
        let tol = if tx.fract() == 0.0 && ty.fract() == 0.0 { 0.05 } else { 0.25 };
        assert!(o.x_shift.abs() < tol && o.y_shift.abs() < tol, "date {d}: ({}, {})", o.x_shift, o.y_shift);
    }
}

/// The quality band keeps the cloud out of the window: with it the shift is right, without it the cloud's own
/// content (the reference, not moved) pulls the match towards none.
#[test]
fn a_cloud_is_kept_out_by_the_quality_band() {
    let with = fixture("cloud", true);
    with.run(&[]).unwrap();
    let rows = csv(&with.out("shifts.csv"));
    let (dx, dy) = (column(&rows, "x_shift_px"), column(&rows, "y_shift_px"));
    let err = |rows: &[Vec<String>]| (number(&rows[2], dx) - 3.0).abs().max((number(&rows[2], dy) + 2.0).abs());
    let good = err(&rows);
    assert!(good < 0.2, "with the quality band: {good}");
    // the same dates without the quality band
    let mut without = fixture("cloud_no_qa", true);
    without.spec.qa_input = None;
    without.spec.qa_rule = None;
    without.run(&[]).unwrap();
    let bad_rows = csv(&without.out("shifts.csv"));
    let bad = err(&bad_rows);
    let failed = bad_rows[2][column(&bad_rows, "success")] == "false";
    assert!(failed || bad > good + 0.15, "without it: {bad} against {good} ({})", bad_rows[2].join(","));
}

/// A date with almost no clear pixel is not matched, and says why; a margin around the cloud grows the refusals.
#[test]
fn a_date_that_is_nearly_all_cloud_is_left_as_it_is() {
    let mut f = fixture("cloudy", true);
    // 97% of date 3 is cloud
    let codes: Vec<f32> = (0..N).map(|i| if i % 33 == 0 { 0.0 } else { 4.0 }).collect();
    f.codes[2] = codes;
    let qa: Vec<Vec<f32>> = f.codes.clone();
    write_tif(&f.dir.join("qa.tif"), &qa);
    f.run(&[]).unwrap();
    let rows = csv(&f.out("shifts.csv"));
    let (applied, why) = (column(&rows, "applied"), column(&rows, "reason"));
    assert_eq!(rows[3][applied], "false");
    assert!(rows[3][why].starts_with("too cloudy"), "{:?}", rows[3]);
    // the series still has the date, as it was
    let kept = read(&f.date_file(2), 1);
    assert_eq!(kept, f.nir[2]);
    // the margin: the shift of date 2 with and without it agree (the cloud is far from the window's middle) and the
    // cloud grown to its margin is what the zeit-core sees
    let g = fixture("margin", true);
    g.run(&[("mask_buffer", json!(2))]).unwrap();
    let rows = csv(&g.out("shifts.csv"));
    assert!((number(&rows[2], column(&rows, "x_shift_px")) - 3.0).abs() < 0.2);
}

/// The median of the dates as the reference: the dates are matched to it, and a pixel the quality band refuses on a
/// date is left out of the median.
#[test]
fn a_composite_reference_leaves_out_the_refused_pixels() {
    // four dates, none moved, the cloud of the second date holds a constant that would pull the median
    let f = fixture_with("composite", true, &[(0.0, 0.0); T], None);
    // the cloud's content: a high constant (it would show in the median of 3 dates if it were kept)
    let mut nir = f.nir.clone();
    for i in (0..N).filter(|&i| in_blob(i)) {
        nir[1][i] = 9.0;
    }
    write_tif(&f.dir.join("nir.tif"), &nir);
    let r = f.run(&[("reference", json!("composite")), ("ref_dates", json!("all"))]).unwrap();
    assert!(r.outputs.iter().any(|o| o["id"] == "aligned"));
    let rows = csv(&f.out("shifts.csv"));
    let (dx, dy, applied) = (column(&rows, "x_shift_px"), column(&rows, "y_shift_px"), column(&rows, "applied"));
    for row in &rows[1..] {
        assert_eq!(row[applied], "true", "{row:?}");
        assert!(number(row, dx).abs() < 0.05 && number(row, dy).abs() < 0.05, "{row:?}");
    }
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("the median of 4 dates"), "{report}");
}

/// Another layer as the reference: put on the series' grid by the run, whatever its own; the shifts are found to it.
#[test]
fn another_layer_is_the_reference_on_its_own_grid() {
    let f = fixture("layer", false);
    // the reference layer: the scene unmoved, on a grid 60 pixels wider on every side (the same cells)
    let (lw, lh) = (W + 120, H + 120);
    let layer = Scene(1);
    let values: Vec<f32> =
        (0..lw * lh).map(|i| layer.at((i % lw) as f64 + 0.5 - 60.0, (i / lw) as f64 + 0.5 - 60.0)).collect();
    let path = f.dir.join("layer.tif");
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(&path, lw, lh, 1).unwrap();
    ds.set_geo_transform(&[GT[0] - 60.0 * 30.0, 30.0, 0.0, GT[3] + 60.0 * 30.0, 0.0, -30.0]).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
    ds.rasterband(1).unwrap().write((0, 0), (lw, lh), &mut Buffer::new((lw, lh), values)).unwrap();
    drop(ds);
    let mut spec = f.spec.clone();
    spec.tool = "coregister".into();
    spec.other = Some(OtherSeries {
        name: "reference".into(),
        inputs: vec![path.to_string_lossy().to_string()],
        band_names: vec!["nir".into()],
        years: vec![2022.0],
        days: None,
        labels: None,
    });
    spec.params = params(&[("reference", json!("layer")), ("ref_layer", json!({"name": "reference"}))]);
    run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let rows = csv(&f.out("shifts.csv"));
    let (dx, dy) = (column(&rows, "x_shift_px"), column(&rows, "y_shift_px"));
    for d in 0..T {
        let (tx, ty) = TRUTH[d];
        assert!(
            (number(&rows[1 + d], dx) - tx).abs() < 0.2 && (number(&rows[1 + d], dy) - ty).abs() < 0.2,
            "date {d}: {:?}",
            rows[1 + d]
        );
    }
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("resampled to the series' grid"), "{report}");
}

/// The same series as COGs: the same cells as the virtual files, and the band names kept.
#[test]
fn cogs_hold_what_the_virtual_series_computes() {
    let v = fixture("format_vrt", true);
    v.run(&[]).unwrap();
    let c = fixture("format_cog", true);
    c.run(&[("format", json!("cog"))]).unwrap();
    assert!(c.date_file(0).extension().is_some_and(|e| e == "tif"));
    for d in 0..T {
        for b in 1..=3 {
            let (a, b2) = (read(&v.date_file(d), b), read(&c.date_file(d), b));
            assert!(
                a.iter().zip(&b2).all(|(x, y)| x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan())),
                "date {d} band {b}"
            );
        }
    }
    let ds = Dataset::open(c.date_file(1)).unwrap();
    assert_eq!(ds.rasterband(1).unwrap().description().unwrap(), "nir");
    // 'only the shifts': no series
    let n = fixture("format_none", true);
    let r = n.run(&[("format", json!("none"))]).unwrap();
    assert!(r.outputs.iter().all(|o| o["id"] != "aligned"));
    assert!(!n.out("series").exists() && n.out("shifts.csv").exists());
}

/// Local mode: the tie points are zeit-core's, bit for bit; the used ones follow the field of shifts (an affine part
/// and a sinusoid, up to 2 pixels) and the block planted with another scene is not used.
#[test]
fn the_tie_points_follow_the_field_and_drop_the_planted_block() {
    let field = |x: f64, y: f64| (0.8 + 0.8 * x / W as f64, -0.5 + 0.3 * (y / 150.0).sin() + 0.4 * y / H as f64);
    let mut f = fixture_with("local", false, &TRUTH, Some(&field));
    // a block of date 2 (index 1) replaced by another scene: the window of the tie point at (256, 256) is all inside it
    let other = Scene(9).image(0.0, 0.0);
    for (i, (v, o)) in f.nir[1].iter_mut().zip(&other).enumerate() {
        let (x, y) = (i % W, i / W);
        if (176..336).contains(&x) && (176..336).contains(&y) {
            *v = *o;
        }
    }
    write_tif(&f.dir.join("nir.tif"), &f.nir);
    let res = 64;
    let r = f
        .run(&[
            ("mode", json!("local")),
            ("grid_res", json!(res)),
            ("window_size", json!(128)),
            ("ref_date", json!(1)),
            ("format", json!("vrt")),
        ])
        .unwrap();
    // the core's tie points on the same arrays
    let valid = vec![true; N];
    let image = |d: usize| Image { data: &f.nir[d], valid: &valid, ny: H, nx: W };
    let xs: Vec<f64> = (0..).map(|k| (k * res) as f64).take_while(|&x| x < (W + res) as f64).collect();
    let points: Vec<(f64, f64)> = xs.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y))).collect();
    let lp = LocalParams {
        coreg: coreg::Params { window_size: 128, cell: [30.0, 30.0], ..Default::default() },
        ..Default::default()
    };
    let geo = Geo { x0: GT[0], dx: 30.0, y0: GT[3], dy: -30.0 };
    let rows = csv(&f.out("tie_points.csv"));
    let (date, id, dx, dy, used, rel, outlier) = (
        column(&rows, "date"),
        column(&rows, "point_id"),
        column(&rows, "x_shift_px"),
        column(&rows, "y_shift_px"),
        column(&rows, "used"),
        column(&rows, "reliability"),
        column(&rows, "outlier"),
    );
    let labels: Vec<String> = rows[1..].iter().map(|r| r[date].clone()).collect();
    let mut dates: Vec<String> = labels.clone();
    dates.dedup();
    assert_eq!(dates.len(), T - 1, "the reference has no tie points");
    let mut checked_block = false;
    for (k, label) in dates.iter().enumerate() {
        let d = k + 1;
        let core = coreg_local::tie_points(&image(0), &image(d), &points, (0.0, 0.0), &geo, &lp, 1);
        let mine: Vec<&Vec<String>> = rows[1..].iter().filter(|r| &r[date] == label).collect();
        assert_eq!(mine.len(), core.len(), "date {d}");
        let mut good = 0;
        for (row, t) in mine.iter().zip(&core) {
            assert_eq!(row[id].parse::<usize>().unwrap(), t.index);
            same_bits(number(row, dx), t.x_shift_px, "x shift of a tie point");
            same_bits(number(row, dy), t.y_shift_px, "y shift of a tie point");
            same_bits(number(row, rel), t.outcome.reliability, "reliability");
            assert_eq!(row[outlier] == "true", t.outlier);
            assert_eq!(row[used] == "true", t.outcome.success && !t.outlier);
            if row[used] == "true" {
                let (px, py) = points[t.index];
                let (fx, fy) = field(px, py);
                // the windows cut by the image's edge are small: the field is held to where the window is whole
                let whole = row[column(&rows, "fft_size")].parse::<usize>().unwrap() >= 64;
                if whole {
                    assert!(
                        (number(row, dx) - fx).abs() < 0.4 && (number(row, dy) - fy).abs() < 0.4,
                        "{row:?} against ({fx}, {fy})"
                    );
                }
                good += 1;
            }
            let (px, py) = points[t.index];
            if d == 1 && (px, py) == (256.0, 256.0) {
                checked_block = true;
                assert!(row[used] == "false", "a point of the planted block was used: {row:?}");
            }
        }
        assert!(good >= 20, "date {d}: {good} tie points used");
    }
    assert!(checked_block);
    // every date is applied and the series is written; the shifts' table says how many points
    let shifts = csv(&f.out("shifts.csv"));
    let (applied, used_n) = (column(&shifts, "applied"), column(&shifts, "used"));
    for row in &shifts[2..] {
        assert_eq!(row[applied], "true", "{row:?}");
        assert!(row[used_n].parse::<usize>().unwrap() >= 20);
    }
    let series = r.outputs.iter().find(|o| o["id"] == "aligned").unwrap();
    assert!(series.get("qa_band").is_none());
    // the warped date is nearer the reference than it was
    let aligned = read(&f.date_file(2), 1);
    let inside = |i: usize| (40..W - 40).contains(&(i % W)) && (40..H - 40).contains(&(i / W));
    let rms = |a: &[f32]| {
        let v: Vec<f64> =
            (0..N).filter(|&i| inside(i) && a[i].is_finite()).map(|i| f64::from(a[i] - f.nir[0][i]).powi(2)).collect();
        (v.iter().sum::<f64>() / v.len() as f64).sqrt()
    };
    assert!(rms(&aligned) < 0.5 * rms(&f.nir[2]), "after {} before {}", rms(&aligned), rms(&f.nir[2]));
}

/// Too few tie points: the date is left as it is, with the reason.
#[test]
fn a_date_with_too_few_tie_points_says_why() {
    let f = fixture("few_points", false);
    f.run(&[("mode", json!("local")), ("grid_res", json!(400)), ("min_points", json!(20))]).unwrap();
    let rows = csv(&f.out("shifts.csv"));
    let (applied, why) = (column(&rows, "applied"), column(&rows, "reason"));
    for row in &rows[2..] {
        assert_eq!(row[applied], "false");
        assert!(row[why].contains("fewer than min_points=20"), "{row:?}");
    }
    // the files are the dates as they were
    assert_eq!(read(&f.date_file(1), 1), f.nir[1]);
}

/// Mistakes are said before anything is computed.
#[test]
fn mistakes_are_said() {
    let f = fixture("errors", false);
    let fails = |r: Result<JobResult, String>| r.err().expect("it should have failed");
    let e = fails(f.run(&[("ref_date", json!(9))]));
    assert!(e.contains("reference date 9"), "{e}");
    let e = fails(f.run(&[("band", json!("swir1"))]));
    assert!(e.contains("not mapped"), "{e}");
    let e = fails(f.run(&[("reference", json!("layer"))]));
    assert!(e.contains("reference layer"), "{e}");
}

/// A cloud beside the middle of what the dates share leaves AROSICS' window small (it grows from the middle until it
/// meets the cloud): the run puts it at the largest clear square, and the shift is found.
#[test]
fn a_cloud_beside_the_middle_moves_the_window_to_a_clear_square() {
    let mut f = fixture("near_cloud", true);
    // the block 100-170: the middle of the images (192, 192) is clear, 22 pixels from its corner
    for i in 0..N {
        let (x, y) = (i % W, i / W);
        let cloud = (100..170).contains(&x) && (100..170).contains(&y);
        f.codes[1][i] = if cloud { 4.0 } else { 0.0 };
        f.nir[1][i] = if cloud { 0.9 } else { Scene(1).at(x as f64 + 0.5 + 3.0, y as f64 + 0.5 - 2.0) };
    }
    write_tif(&f.dir.join("nir.tif"), &f.nir);
    let codes = f.codes.clone();
    write_tif(&f.dir.join("qa.tif"), &codes);
    // what the core makes of the middle window
    let core = core_global(&f, 1);
    let side = core.window[2].saturating_sub(core.window[0]);
    f.run(&[]).unwrap();
    let rows = csv(&f.out("shifts.csv"));
    let (dx, dy, win0, win2) = (
        column(&rows, "x_shift_px"),
        column(&rows, "y_shift_px"),
        column(&rows, "window_x0"),
        column(&rows, "window_x1"),
    );
    assert!(
        (number(&rows[2], dx) - 3.0).abs() < 0.2 && (number(&rows[2], dy) + 2.0).abs() < 0.2,
        "{:?} (the core's window: {side} pixels)",
        rows[2]
    );
    let moved = rows[2][win2].parse::<usize>().unwrap() - rows[2][win0].parse::<usize>().unwrap();
    assert!(moved > side, "the window should be larger than the core's {side}: {moved}");
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("a cloud is near the middle"), "{report}");
}
