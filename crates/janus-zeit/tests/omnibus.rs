//! The omnibus change test of Sentinel-1 (a tool after the bridge): a synthetic series of VV and VH backscatter (Erlang
//! speckle of a known number of looks, a block whose backscatter rose at a known date, a pixel with one valid date), as
//! linear power and as dB, run through `run_job`, and zeit-core's `omnibus_batch` called directly on the same values:
//! the maps must be what the core gives, bit for bit.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager};
use janus_zeit::job::{JobResult, JobSpec, estimate, run_job};
use janus_zeit::{Params, PixelInput, run_pixel};
use serde_json::{Value, json};
use zeit_core::omnibus::{Approx, OmnibusOptions, omnibus_batch};

const W: usize = 24;
const H: usize = 16;
const T: usize = 16;
/// The date (index) the block's backscatter rose at, and the block.
const STEP: usize = 9;
const BLOCK: (std::ops::Range<usize>, std::ops::Range<usize>) = (8..16, 4..12);
const ENL: f64 = 4.0;

struct Lcg(u64);

impl Lcg {
    /// Uniform in (0, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (((self.0 >> 11) as f64 + 0.5) / (1u64 << 53) as f64).clamp(1e-12, 1.0)
    }

    /// Speckle of mean `mean` and `ENL` looks: a gamma of shape ENL, as the sum of ENL exponentials.
    fn speckle(&mut self, mean: f64) -> f64 {
        let e: f64 = (0..ENL as usize).map(|_| -self.next().ln()).sum();
        mean * e / ENL
    }
}

/// The backscatter of the series, [date][pixel] for VV and VH, linear power: the block rises by a factor 6 (VV) and 5
/// (VH) from `STEP` on; the pixel (0, 0) has one valid date only.
fn series() -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
    let mut rng = Lcg(2024);
    let (mut vv, mut vh) = (vec![vec![0f32; W * H]; T], vec![vec![0f32; W * H]; T]);
    for (t, (dv, dh)) in vv.iter_mut().zip(vh.iter_mut()).enumerate() {
        for y in 0..H {
            for x in 0..W {
                let up = t >= STEP && BLOCK.0.contains(&x) && BLOCK.1.contains(&y);
                let (m_vv, m_vh) = if up { (0.6, 0.1) } else { (0.1, 0.02) };
                let (a, b) = (rng.speckle(m_vv), rng.speckle(m_vh));
                let lone = x == 0 && y == 0 && t != 5;
                dv[y * W + x] = if lone { f32::NAN } else { a as f32 };
                dh[y * W + x] = if lone { f32::NAN } else { b as f32 };
            }
        }
    }
    (vv, vh)
}

fn db(series: &[Vec<f32>]) -> Vec<Vec<f32>> {
    series.iter().map(|d| d.iter().map(|&v| (10.0 * f64::from(v).log10()) as f32).collect()).collect()
}

fn write_tif(path: &Path, dates: &[Vec<f32>]) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(path, W, H, dates.len()).unwrap();
    ds.set_geo_transform(&[600000.0, 10.0, 0.0, 7500000.0, 0.0, -10.0]).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
    for (k, v) in dates.iter().enumerate() {
        let mut band = ds.rasterband(k + 1).unwrap();
        band.set_no_data_value(Some(-9999.0)).unwrap();
        band.write((0, 0), (W, H), &mut Buffer::new((W, H), v.clone())).unwrap();
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("janus-zeit-omnibus-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fixture {
    dir: PathBuf,
    /// VV and VH, linear, [date][pixel]
    vv: Vec<Vec<f32>>,
    vh: Vec<Vec<f32>>,
    spec: JobSpec,
}

/// The series in `dir`: a file a polarization holding every date, as linear power or as dB, with or without VH.
fn fixture(name: &str, in_db: bool, dual: bool) -> Fixture {
    let dir = folder(name);
    let (vv, vh) = series();
    let (fv, fh) = if in_db { (db(&vv), db(&vh)) } else { (vv.clone(), vh.clone()) };
    let mut spec = JobSpec::default();
    let path = dir.join("vv.tif");
    write_tif(&path, &fv);
    spec.input = path.to_string_lossy().to_string();
    spec.bands.insert("vv".into(), spec.input.clone());
    if dual {
        let path = dir.join("vh.tif");
        write_tif(&path, &fh);
        spec.bands.insert("vh".into(), path.to_string_lossy().to_string());
    }
    let days: Vec<i64> = (0..T as i64).map(|k| 19000 + 12 * k).collect();
    spec.years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
    spec.days = Some(days);
    spec.threads = Some(2);
    spec.output_dir = dir.join("out").to_string_lossy().to_string();
    Fixture { dir, vv, vh, spec }
}

impl Fixture {
    fn run(&self, params: Value) -> JobResult {
        let mut spec = self.spec.clone();
        spec.tool = "omnibus".into();
        spec.params = params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let r = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("omnibus: {e}"));
        assert!(estimate(&spec).is_ok());
        r
    }

    fn out(&self, file: &str) -> PathBuf {
        self.dir.join("out").join(file)
    }

    /// The core's planes (and the direction at each date) on the linear values, `dual` or VV alone.
    fn core(&self, dual: bool, approx: Approx, alpha: f64) -> (Vec<f32>, Vec<f32>) {
        let nb = if dual { 2 } else { 1 };
        let mut x = Vec::with_capacity(T * nb * W * H);
        for (vv, vh) in self.vv.iter().zip(&self.vh) {
            x.extend(vv.iter().map(|&v| f64::from(v)));
            if dual {
                x.extend(vh.iter().map(|&v| f64::from(v)));
            }
        }
        let bands: Vec<usize> = (0..nb).collect();
        let o = OmnibusOptions { enl: ENL, alpha, approx, p: nb };
        let (mut planes, mut per) = (vec![0f32; 8 * W * H], vec![0f32; T * W * H]);
        omnibus_batch(&x, nb, W * H, T, &bands, &o, &[], &mut planes, Some(&mut per), 2);
        (planes, per)
    }

    fn year(&self, i: f32) -> f32 {
        if i.is_finite() { self.spec.years[i as usize] as f32 } else { f32::NAN }
    }
}

fn read(path: &Path, band: usize) -> Vec<f32> {
    let ds = Dataset::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h) = ds.raster_size();
    ds.rasterband(band).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
}

fn same(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        assert!((x.is_nan() && y.is_nan()) || x == y, "{what}: pixel {i}: {x} against {y}");
    }
}

/// Every output of a run against the core's planes: the dates as the series' decimal years, the directions as classes
/// (the code plus one).
fn matches_core(fx: &Fixture, planes: &[f32], per: &[f32], what: &str) {
    let n = W * H;
    let plane = |k: usize| planes[k * n..(k + 1) * n].to_vec();
    let dates = |k: usize| plane(k).iter().map(|&i| fx.year(i)).collect::<Vec<f32>>();
    let class = |k: usize| plane(k).iter().map(|&c| c + 1.0).collect::<Vec<f32>>();
    for (file, expected) in [
        ("first_change", dates(0)),
        ("last_change", dates(1)),
        ("n_changes", plane(2)),
        ("first_direction", class(3)),
        ("last_direction", class(4)),
        ("greatest_change", dates(5)),
        ("p_change", plane(6)),
        ("n_dates", plane(7)),
    ] {
        same(&read(&fx.out(&format!("omnibus_{file}.tif")), 1), &expected, &format!("{what}: {file}"));
    }
    for t in 0..T {
        same(&read(&fx.out("omnibus_per_date.tif"), t + 1), &per[t * n..(t + 1) * n], &format!("{what}: date {t}"));
    }
}

fn output<'a>(r: &'a JobResult, id: &str) -> &'a Value {
    r.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"))
}

#[test]
fn the_maps_are_the_cores_and_the_block_changes_at_its_date() {
    let fx = fixture("linear", false, true);
    let r = fx.run(json!({ "enl": ENL, "alpha": 0.01, "scale": "linear", "per_date": true }));
    let (planes, per) = fx.core(true, Approx::Improved, 0.01);
    matches_core(&fx, &planes, &per, "dual linear");
    // the outputs the manifest promises, the series hidden
    for id in [
        "first_change",
        "last_change",
        "greatest_change",
        "n_changes",
        "first_direction",
        "last_direction",
        "p_change",
        "n_dates",
        "per_date",
    ] {
        output(&r, id);
    }
    assert_eq!(output(&r, "first_change")["unit"], "year");
    assert_eq!(output(&r, "first_direction")["classes"], json!(["None", "Brightening", "Darkening", "Indefinite"]));
    assert_eq!(output(&r, "per_date")["series"], true);
    // the block: a change at the step, brightening; outside, few changes
    let first = read(&fx.out("omnibus_first_change.tif"), 1);
    let dir = read(&fx.out("omnibus_first_direction.tif"), 1);
    let mut hist = vec![0usize; T + 1];
    for y in BLOCK.1.clone() {
        for x in BLOCK.0.clone() {
            let v = first[y * W + x];
            hist[fx.spec.years.iter().position(|&yr| yr as f32 == v).unwrap_or(T)] += 1;
        }
    }
    println!("first change of the block's pixels by date index (the last: none): {hist:?}");
    // a change is found at its own date in part of the pixels and a date or two later in the rest
    let (step_year, soon) = (fx.spec.years[STEP] as f32, fx.spec.years[STEP + 2] as f32);
    let (mut inside, mut at_step, mut by_then, mut bright, mut outside, mut false_alarms) = (0, 0, 0, 0, 0, 0);
    for y in 0..H {
        for x in 0..W {
            let (v, d) = (first[y * W + x], dir[y * W + x]);
            if x == 0 && y == 0 {
                assert!(v.is_nan() && d.is_nan(), "the pixel with one valid date");
            } else if BLOCK.0.contains(&x) && BLOCK.1.contains(&y) {
                inside += 1;
                at_step += usize::from(v == step_year);
                by_then += usize::from(v >= step_year && v <= soon);
                bright += usize::from(d == 2.0);
            } else {
                outside += 1;
                false_alarms += usize::from(v.is_finite());
            }
        }
    }
    assert!(at_step * 10 >= inside * 6, "{at_step} of {inside} pixels of the block changed at the step");
    assert!(
        by_then * 10 >= inside * 8,
        "{by_then} of {inside} pixels of the block changed at the step or two dates later"
    );
    assert!(bright * 10 >= inside * 8, "{bright} of {inside} brightening");
    assert!(false_alarms * 10 <= outside, "{false_alarms} of {outside} pixels outside changed");
    // the record of the run
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("omnibus.json")).unwrap()).unwrap();
    assert_eq!((rec["enl"].as_f64(), rec["alpha"].as_f64()), (Some(ENL), Some(0.01)));
    assert_eq!((rec["approximation"].as_str(), rec["scale"].as_str()), (Some("improved"), Some("linear")));
    assert_eq!(rec["bands"], json!(["vv", "vh"]));
    assert_eq!(rec["dates"].as_array().unwrap().len(), T);
}

#[test]
fn the_chi_square_approximation_is_the_cores_and_the_series_is_written_only_on_request() {
    let fx = fixture("chi2", false, true);
    let r = fx.run(json!({ "enl": ENL, "alpha": 0.05, "approximation": "chi2", "scale": "linear" }));
    assert!(r.outputs.iter().all(|o| o["id"] != "per_date"));
    assert!(!fx.out("omnibus_per_date.tif").exists());
    let (planes, per) = fx.core(true, Approx::Chi2, 0.05);
    let n = W * H;
    let first: Vec<f32> = planes[..n].iter().map(|&i| fx.year(i)).collect();
    same(&read(&fx.out("omnibus_first_change.tif"), 1), &first, "chi2 first change");
    same(&read(&fx.out("omnibus_n_changes.tif"), 1), &planes[2 * n..3 * n], "chi2 number of changes");
    // the other approximation gives other p-values
    let (improved, _) = fx.core(true, Approx::Improved, 0.05);
    assert_ne!(&planes[6 * n..7 * n], &improved[6 * n..7 * n]);
    assert_eq!(per.len(), T * n);
}

#[test]
fn a_series_in_db_is_converted_and_the_automatic_scale_finds_it() {
    let lin = fixture("auto-linear", false, true);
    let db_fx = fixture("auto-db", true, true);
    let (planes, per) = lin.core(true, Approx::Improved, 0.01);
    // the dB values are those of the linear series in float32 dB: the core on 10^(dB/10) of them
    let n = W * H;
    let mut x = Vec::with_capacity(T * 2 * n);
    for (vv, vh) in db(&lin.vv).iter().zip(&db(&lin.vh)) {
        x.extend(vv.iter().map(|&v| 10f64.powf(f64::from(v) / 10.0)));
        x.extend(vh.iter().map(|&v| 10f64.powf(f64::from(v) / 10.0)));
    }
    let o = OmnibusOptions { enl: ENL, alpha: 0.01, approx: Approx::Improved, p: 2 };
    let (mut db_planes, mut db_per) = (vec![0f32; 8 * n], vec![0f32; T * n]);
    omnibus_batch(&x, 2, n, T, &[0, 1], &o, &[], &mut db_planes, Some(&mut db_per), 2);
    // explicitly dB
    db_fx.run(json!({ "enl": ENL, "scale": "db", "per_date": true }));
    matches_core(&db_fx, &db_planes, &db_per, "dB");
    let explicit = read(&db_fx.out("omnibus_first_change.tif"), 1);
    // automatic: the same result, and the record says what it found
    let auto_db = fixture("auto-db2", true, true);
    auto_db.run(json!({ "enl": ENL, "scale": "auto", "per_date": true }));
    same(&read(&auto_db.out("omnibus_first_change.tif"), 1), &explicit, "automatic on dB");
    matches_core(&auto_db, &db_planes, &db_per, "automatic on dB");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(auto_db.out("omnibus.json")).unwrap()).unwrap();
    assert_eq!(rec["scale"], "db");
    // automatic on the linear series: as linear, the core's planes
    let auto_lin = fixture("auto-lin2", false, true);
    auto_lin.run(json!({ "enl": ENL, "scale": "auto", "per_date": true }));
    matches_core(&auto_lin, &planes, &per, "automatic on linear");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(auto_lin.out("omnibus.json")).unwrap()).unwrap();
    assert_eq!(rec["scale"], "linear");
    // the dB series found the same block as the linear one (float32 dB changes the values a little)
    let lin_first = read(&auto_lin.out("omnibus_first_change.tif"), 1);
    let agree = lin_first.iter().zip(&explicit).filter(|(a, b)| (a.is_nan() && b.is_nan()) || a == b).count();
    assert!(agree * 100 >= n * 97, "{agree} of {n} pixels agree between dB and linear");
}

#[test]
fn a_single_polarization_uses_vv_alone() {
    let fx = fixture("single", false, false);
    fx.run(json!({ "enl": ENL, "scale": "linear", "per_date": true }));
    let (planes, per) = fx.core(false, Approx::Improved, 0.01);
    matches_core(&fx, &planes, &per, "VV alone");
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("omnibus.json")).unwrap()).unwrap();
    assert_eq!(rec["bands"], json!(["vv"]));
    // the dual-pol result is another one
    let (dual, _) = fx.core(true, Approx::Improved, 0.01);
    assert_ne!(&planes[6 * W * H..7 * W * H], &dual[6 * W * H..7 * W * H]);
}

#[test]
fn a_pixel_with_fewer_than_two_valid_dates_is_nan_everywhere() {
    let fx = fixture("lone", false, true);
    fx.run(json!({ "enl": ENL, "scale": "linear", "per_date": true }));
    for id in ["first_change", "last_change", "greatest_change", "n_changes", "first_direction", "last_direction"] {
        assert!(read(&fx.out(&format!("omnibus_{id}.tif")), 1)[0].is_nan(), "{id}");
    }
    for id in ["p_change", "n_dates"] {
        assert!(read(&fx.out(&format!("omnibus_{id}.tif")), 1)[0].is_nan(), "{id}");
    }
    for t in 0..T {
        assert!(read(&fx.out("omnibus_per_date.tif"), t + 1)[0].is_nan(), "date {t}");
    }
    // a pixel next to it is not
    assert!(read(&fx.out("omnibus_n_dates.tif"), 1)[1] == T as f32);
}

#[test]
fn a_pin_marks_the_changes_on_the_chart() {
    let fx = fixture("pin", false, true);
    let (x, y) = (11, 7);
    let at = y * W + x;
    let input = PixelInput {
        years: fx.spec.years.clone(),
        days: fx.spec.days.clone(),
        values: fx.vv.iter().map(|d| f64::from(d[at])).collect(),
        bands: [
            ("vv".to_string(), fx.vv.iter().map(|d| f64::from(d[at])).collect()),
            ("vh".to_string(), fx.vh.iter().map(|d| f64::from(d[at])).collect()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let given = [("enl".to_string(), json!(ENL)), ("scale".to_string(), json!("linear"))].into_iter().collect();
    let params = Params::with_defaults("omnibus", &given).unwrap();
    let res = run_pixel("omnibus", &params, &input).unwrap();
    let (planes, per) = fx.core(true, Approx::Improved, 0.01);
    // every change the core found is a line of its direction, at the series' decimal year
    let mut expected: Vec<(usize, f32)> = (0..T).map(|t| (t, per[t * W * H + at])).filter(|(_, c)| *c >= 1.0).collect();
    assert!(!expected.is_empty(), "the pixel of the block has a change");
    let names = ["", "brightening", "darkening", "indefinite"];
    for overlay in &res.overlays {
        assert_eq!(overlay.kind, "vlines");
        let code = (1..4).find(|&c| overlay.label == format!("Omnibus change ({})", names[c])).expect(&overlay.label);
        for &year in &overlay.x {
            let t = fx.spec.years.iter().position(|&y| f64::from(y as f32) == year).expect("a date of the series");
            let k = expected.iter().position(|e| *e == (t, code as f32)).expect("a change of the core");
            expected.remove(k);
        }
    }
    assert!(expected.is_empty(), "changes not drawn: {expected:?}");
    let row = |name: &str| res.rows.iter().find(|r| r[0] == name).unwrap_or_else(|| panic!("no row {name}"))[1].clone();
    assert_eq!(row("Valid dates"), format!("{T} of {T}"));
    assert_eq!(row("Changes"), format!("{}", planes[2 * W * H + at] as usize));
    assert!(row("First change").contains("brightening"), "{}", row("First change"));
    // without VV the tool says what is missing
    let none = PixelInput { bands: Default::default(), ..input };
    let res = run_pixel("omnibus", &params, &none).unwrap();
    assert!(res.overlays.is_empty() && res.rows[0][1].contains("VV"));
}
