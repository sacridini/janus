//! The topographic correction (Tools > Data preparation > Topographic correction): a synthetic scene of two hills with
//! a surface that follows the illumination (three dates, each with its sun, four bands), run through `run_job`, and
//! zeit-core called directly on the same values (one call over all the dates, the DEM and its slope and aspect as the core
//! gives them): the corrected files must be what the core gives, bit for bit, whatever the way the run reads and writes
//! them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager, Metadata};
use janus_zeit::job::{JobResult, JobSpec, estimate, run_job};
use janus_zeit::sun::SunHint;
use serde_json::{Value, json};
use zeit_core::topocorr::{self, Method};

const W: usize = 64;
const H: usize = 56;
const N: usize = W * H;
const NB: usize = 4;
const T: usize = 3;
const NAMES: [&str; NB] = ["blue", "green", "red", "nir"];
/// each date's sun: zenith and azimuth
const SUNS: [[f64; 2]; T] = [[35.0, 120.0], [40.0, 140.0], [46.0, 165.0]];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn grid() -> (f64, f64) {
    (600000.0, 7500000.0)
}

fn write_tif(path: &Path, bands: &[Vec<f32>], nodata: f64) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(path, W, H, bands.len()).unwrap();
    let (x0, y0) = grid();
    ds.set_geo_transform(&[x0, 30.0, 0.0, y0, 0.0, -30.0]).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
    for (k, v) in bands.iter().enumerate() {
        let mut band = ds.rasterband(k + 1).unwrap();
        band.set_no_data_value(Some(nodata)).unwrap();
        band.write((0, 0), (W, H), &mut Buffer::new((W, H), v.clone())).unwrap();
    }
}

fn dem() -> Vec<f32> {
    (0..N)
        .map(|i| {
            let (x, y) = ((i % W) as f64, (i / W) as f64);
            let hill =
                |cx: f64, cy: f64, a: f64, s: f64| a * (-((x - cx).powi(2) + (y - cy).powi(2)) / (2.0 * s * s)).exp();
            (300.0 + hill(20.0, 24.0, 400.0, 9.0) + hill(45.0, 36.0, 330.0, 8.0)) as f32
        })
        .collect()
}

/// `cos i` of every pixel for a sun (degrees), from the slope and aspect the core gives.
fn cos_i(slope: &[f32], aspect: &[f32], [zenith, azimuth]: [f64; 2]) -> Vec<f64> {
    let (z, saz) = (zenith.to_radians(), azimuth.to_radians());
    slope
        .iter()
        .zip(aspect)
        .map(|(&s, &a)| z.cos() * f64::from(s).cos() + z.sin() * f64::from(s).sin() * (saz - f64::from(a)).cos())
        .collect()
}

struct Fixture {
    dir: PathBuf,
    /// [date][band][pixel]
    cube: Vec<Vec<Vec<f32>>>,
    spec: JobSpec,
}

/// The scene: band b of date t is `base_b + k_b cos i_t / cos sz_t` plus a little noise, except the green band, which
/// does not follow the illumination (flat plus noise). With `qa` a block of date 1 is cloud (code 0 of a mask).
fn fixture(name: &str, qa: bool) -> Fixture {
    fixture_on(name, qa, dem())
}

/// The same scene over the elevations `z`.
fn fixture_on(name: &str, qa: bool, z: Vec<f32>) -> Fixture {
    let dir = std::env::temp_dir().join(format!("janus-zeit-topocorr-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let z64: Vec<f64> = z.iter().map(|&v| f64::from(v)).collect();
    let (slope, aspect) = topocorr::slope_aspect(&z64, H, W, 30.0, -30.0, 1);
    let mut rng = Lcg(5);
    let mut cube: Vec<Vec<Vec<f32>>> = vec![];
    for sun in SUNS {
        let ci = cos_i(&slope, &aspect, sun);
        let cz = sun[0].to_radians().cos();
        let bands: Vec<Vec<f32>> = (0..NB)
            .map(|b| {
                ci.iter()
                    .map(|&c| {
                        let flat = 0.12 + 0.01 * rng.next();
                        let lit = 0.04 + 0.03 * b as f64 + (0.15 + 0.05 * b as f64) * c / cz + 0.002 * rng.next();
                        (if b == 1 { flat } else { lit }) as f32
                    })
                    .collect()
            })
            .collect();
        cube.push(bands);
    }
    let mut spec = JobSpec::default();
    write_tif(&dir.join("dem.tif"), &[z], -9999.0);
    for b in 0..NB {
        let dates: Vec<Vec<f32>> = cube.iter().map(|c| c[b].clone()).collect();
        let path = dir.join(format!("band{}.tif", b + 1)).to_string_lossy().to_string();
        write_tif(Path::new(&path), &dates, -9999.0);
        spec.bands.insert(format!("b{:03}", b + 1), path.clone());
        spec.band_names.insert(format!("b{:03}", b + 1), NAMES[b].to_string());
        if matches!(NAMES[b], "red" | "nir") {
            spec.bands.insert(NAMES[b].to_string(), path.clone());
        }
        if NAMES[b] == "nir" {
            spec.input = path;
        }
    }
    if qa {
        let codes: Vec<Vec<f32>> = (0..T)
            .map(|t| {
                (0..N)
                    .map(|i| {
                        let (x, y) = (i % W, i / W);
                        if t == 1 && (14..30).contains(&x) && (16..32).contains(&y) { 0.0 } else { 1.0 }
                    })
                    .collect()
            })
            .collect();
        let path = dir.join("qa.tif");
        write_tif(&path, &codes, -9999.0);
        spec.qa_input = Some(path.to_string_lossy().to_string());
        spec.qa_rule = Some("nonzero".into());
        spec.band_names.insert("qa".into(), "Mask".into());
    }
    let days: Vec<i64> = vec![19000, 19016, 19032];
    spec.years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
    spec.days = Some(days);
    spec.threads = Some(2);
    spec.suns = SUNS.iter().map(|s| SunHint { stac: Some(*s), ..Default::default() }).collect();
    spec.output_dir = dir.join("out").to_string_lossy().to_string();
    Fixture { dir, cube, spec }
}

fn params(extra: &[(&str, Value)]) -> BTreeMap<String, Value> {
    let mut p: BTreeMap<String, Value> =
        [("dem_source".to_string(), json!("file")), ("method".to_string(), json!("scs+c"))].into();
    for (k, v) in extra {
        p.insert(k.to_string(), v.clone());
    }
    p
}

impl Fixture {
    fn run(&self, extra: &[(&str, Value)]) -> Result<JobResult, String> {
        let mut spec = self.spec.clone();
        spec.tool = "topocorr".into();
        let mut p = params(extra);
        p.entry("dem".into()).or_insert(json!(self.dir.join("dem.tif").to_string_lossy()));
        spec.params = p;
        assert!(estimate(&spec).is_ok());
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false))
    }

    fn out(&self, file: &str) -> PathBuf {
        self.dir.join("out").join(file)
    }

    /// The corrected series' file of date `t`.
    fn date_file(&self, t: usize) -> PathBuf {
        let mut files: Vec<PathBuf> = std::fs::read_dir(self.out("series"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
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

fn same_bits(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len(), "{what}");
    let bad: Vec<usize> = (0..a.len()).filter(|&i| a[i].to_bits() != b[i].to_bits()).collect();
    assert!(
        bad.is_empty(),
        "{what}: {} of {} differ, e.g. {} against {} at {}",
        bad.len(),
        a.len(),
        a[bad[0]],
        b[bad[0]],
        bad[0]
    );
}

fn csv(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path).unwrap().lines().map(|l| l.split(',').map(str::to_string).collect()).collect()
}

fn column(rows: &[Vec<String>], name: &str) -> usize {
    rows[0].iter().position(|c| c == name).unwrap_or_else(|| panic!("no column {name}: {:?}", rows[0]))
}

/// The core's answer on the whole series in one call: `x` is [date][band][pixel].
fn core_all(x: &[Vec<Vec<f32>>], method: Method, sample: Option<(&[u8], usize)>) -> (Vec<f32>, Vec<topocorr::Fit>) {
    let z: Vec<f64> = dem().iter().map(|&v| f64::from(v)).collect();
    let (slope, aspect) = topocorr::slope_aspect(&z, H, W, 30.0, -30.0, 1);
    let flat: Vec<f32> = x.iter().flatten().flatten().copied().collect();
    let zen: Vec<f32> = SUNS.iter().map(|s| s[0] as f32).collect();
    let azi: Vec<f32> = SUNS.iter().map(|s| s[1] as f32).collect();
    let mut out = vec![0f32; flat.len()];
    let fits = topocorr::topo_correct(
        &flat,
        T,
        NB,
        N,
        &slope,
        &aspect,
        &zen,
        &azi,
        1,
        sample,
        method,
        5f32.to_radians(),
        100,
        &mut out,
        1,
    );
    (out, fits)
}

/// No quality band: the files are what one call of the core over all the dates gives, bit for bit, and the fits are its
/// fits.
#[test]
fn the_files_are_what_the_core_gives_for_all_the_dates() {
    let f = fixture("parity", false);
    let r = f.run(&[]).unwrap();
    let series = r.outputs[0]["path"].as_str().unwrap();
    assert!(Path::new(series).ends_with("series") && r.outputs[0]["series"] == json!(true));
    assert!(r.outputs[0].get("qa_band").is_none(), "no quality band, none kept");
    let (expected, fits) = core_all(&f.cube, Method::ScsC, None);
    for t in 0..T {
        let file = f.date_file(t);
        assert_eq!(Dataset::open(&file).unwrap().raster_count(), NB);
        for b in 0..NB {
            let at = (t * NB + b) * N;
            same_bits(&read(&file, b + 1), &expected[at..at + N], &format!("date {t} band {b}"));
        }
    }
    // topo_c.csv: the c of every date and band, the sun and where it came from
    let rows = csv(&f.out("topo_c.csv"));
    assert_eq!(rows.len(), 1 + T * NB);
    let (c, src, zen) = (column(&rows, "c"), column(&rows, "sun_from"), column(&rows, "sun_zenith"));
    for (k, fit) in fits.iter().enumerate() {
        let row = &rows[1 + k];
        if fit.c.is_finite() {
            assert_eq!(row[c].parse::<f64>().unwrap(), fit.c, "c of row {k}");
        } else {
            assert!(row[c].is_empty());
        }
        assert_eq!(row[src], "STAC item");
        assert_eq!(row[zen].parse::<f64>().unwrap(), f64::from(SUNS[k / NB][0] as f32), "the sun as the core got it");
    }
    // the surface follows the illumination, so every band but the flat one is fitted, with the model's own c =
    // b / m = (0.04 + 0.03 b) cos sz / (0.15 + 0.05 b)
    let (fitted, name) = (column(&rows, "fitted"), column(&rows, "band"));
    for (k, row) in rows[1..].iter().enumerate().filter(|(_, r)| r[name] != "green") {
        assert_eq!(row[fitted], "true", "{row:?}");
        let b = (k % NB) as f64;
        let model = (0.04 + 0.03 * b) * SUNS[k / NB][0].to_radians().cos() / (0.15 + 0.05 * b);
        assert!((row[c].parse::<f64>().unwrap() - model).abs() < 0.02, "{row:?} against {model}");
    }
    // the report and the record
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("SCS+C") && report.contains("STAC item"), "{report}");
    assert!(f.out("topocorr.json").exists());
}

/// The fit and the correction flatten the scene: the correlation of the reflectance with cos i is high before and near 0 after.
#[test]
fn the_correlation_with_the_illumination_goes_to_zero() {
    let f = fixture("validation", false);
    f.run(&[("method", json!("c"))]).unwrap();
    let rows = csv(&f.out("validation.csv"));
    assert_eq!(rows.len(), 1 + T * NB);
    let (b, a, name) = (column(&rows, "r_before"), column(&rows, "r_after"), column(&rows, "band"));
    for row in &rows[1..] {
        if row[name] == "green" {
            continue;
        }
        let (before, after) = (row[b].parse::<f64>().unwrap(), row[a].parse::<f64>().unwrap());
        assert!(before > 0.95, "{row:?}");
        assert!(after.abs() < 0.05, "{row:?}");
    }
    // the steep slopes facing the sun are brighter than those facing away before, alike after
    let (sb, hb, sa, ha) = (
        column(&rows, "sunlit_mean_before"),
        column(&rows, "shaded_mean_before"),
        column(&rows, "sunlit_mean_after"),
        column(&rows, "shaded_mean_after"),
    );
    let red = rows.iter().find(|r| r[name] == "red").unwrap();
    let ratio = |s: usize, h: usize| red[s].parse::<f64>().unwrap() / red[h].parse::<f64>().unwrap();
    assert!(ratio(sb, hb) > 1.5, "before: {}", ratio(sb, hb));
    assert!((ratio(sa, ha) - 1.0).abs() < 0.35 * (ratio(sb, hb) - 1.0), "after: {}", ratio(sa, ha));
}

/// A band whose reflectance does not follow the illumination is copied unchanged, with the reason.
#[test]
fn a_band_that_does_not_follow_the_illumination_is_copied_and_listed() {
    let f = fixture("guards", false);
    // 100000 sample pixels are not there: every band is copied, with the number said
    f.run(&[("min_sample", json!(100000))]).unwrap();
    let rows = csv(&f.out("topo_c.csv"));
    let (fitted, why) = (column(&rows, "fitted"), column(&rows, "why_not"));
    assert!(rows[1..].iter().all(|r| r[fitted] == "false" && r[why].starts_with("fewer than 100000")), "{rows:?}");
    for t in 0..T {
        for b in 0..NB {
            same_bits(&read(&f.date_file(t), b + 1), &f.cube[t][b], "copied");
        }
    }
    let report = std::fs::read_to_string(f.out("report.txt")).unwrap();
    assert!(report.contains("copied unchanged"), "{report}");
}

/// With a quality band: its refusals stay out of the sample, every pixel is corrected, clouds included, and the band is the
/// last band of each file.
#[test]
fn the_quality_band_leaves_the_sample_and_is_kept() {
    let f = fixture("qa", true);
    let r = f.run(&[]).unwrap();
    assert_eq!(r.outputs[0]["qa_band"], json!(NB + 1));
    assert_eq!(r.outputs[0]["qa_rule"], json!("nonzero"));
    // the core with the sample made by hand: steep, and clear
    let z: Vec<f64> = dem().iter().map(|&v| f64::from(v)).collect();
    let (slope, _) = topocorr::slope_aspect(&z, H, W, 30.0, -30.0, 1);
    let floor = 5f32.to_radians().sin();
    let mask: Vec<u8> = (0..T * N)
        .map(|k| {
            let (t, i) = (k / N, k % N);
            let cloud = t == 1 && (14..30).contains(&(i % W)) && (16..32).contains(&(i / W));
            u8::from(slope[i].sin() >= floor && !cloud)
        })
        .collect();
    let (expected, _) = core_all(&f.cube, Method::ScsC, Some((&mask, T)));
    for t in 0..T {
        let file = f.date_file(t);
        let ds = Dataset::open(&file).unwrap();
        assert_eq!(ds.raster_count(), NB + 1);
        assert_eq!(ds.rasterband(NB + 1).unwrap().description().unwrap(), "Mask");
        for b in 0..NB {
            let at = (t * NB + b) * N;
            same_bits(&read(&file, b + 1), &expected[at..at + N], &format!("date {t} band {b}"));
        }
        // the codes as they were
        let codes = read(&file, NB + 1);
        let cloud = codes.iter().filter(|&&c| c == 0.0).count();
        assert_eq!(cloud, if t == 1 { 16 * 16 } else { 0 });
    }
    // the cloud was corrected like the rest (its pixels differ from the input where the slope does)
    let changed = (0..N).filter(|&i| (read(&f.date_file(1), 4)[i] - f.cube[1][3][i]).abs() > 1e-6).count();
    assert!(changed > 1000, "{changed}");
}

/// The vegetation sample takes the pixels whose NDVI is above the threshold (and steep, and clear) only.
#[test]
fn the_vegetation_sample_counts_fewer_pixels() {
    let f = fixture("ndvi", false);
    f.run(&[]).unwrap();
    let all = csv(&f.out("topo_c.csv"));
    let n = column(&all, "n");
    // the scene's NDVI falls with the illumination (0.13 in the shade, 0.10 in the sun): a threshold between keeps the shade
    f.run(&[("sample", json!("ndvi")), ("ndvi_min", json!(0.115))]).unwrap();
    let veg = csv(&f.out("topo_c.csv"));
    let sum = |rows: &[Vec<String>]| rows[1..].iter().map(|r| r[n].parse::<usize>().unwrap()).sum::<usize>();
    assert!(sum(&veg) < sum(&all), "{} against {}", sum(&veg), sum(&all));
    // without red and nir the vegetation sample says so
    let mut bare = f.spec.clone();
    bare.tool = "topocorr".into();
    bare.bands.remove("red");
    bare.params = params(&[("sample", json!("ndvi")), ("dem", json!(f.dir.join("dem.tif").to_string_lossy()))]);
    let e = run_job(&bare, &|_, _| {}, &AtomicBool::new(false)).err().unwrap();
    assert!(e.contains("red and nir"), "{e}");
}

/// A date with no sun stops the run and is named; typed angles stand in for it when asked.
#[test]
fn a_date_without_a_sun_is_named_or_given_the_typed_angles() {
    let mut f = fixture("nosun", false);
    f.spec.suns[1] = SunHint::default();
    let e = f.run(&[]).err().unwrap();
    assert!(e.contains("no sun for 2022-01-24"), "{e}");
    f.run(&[("missing_sun", json!("fixed")), ("sun_zenith", json!(41.0)), ("sun_azimuth", json!(150.0))]).unwrap();
    let rows = csv(&f.out("topo_c.csv"));
    let src = column(&rows, "sun_from");
    let kinds: Vec<&str> = rows[1..].iter().map(|r| r[src].as_str()).collect();
    assert_eq!(kinds.iter().filter(|k| **k == "typed angles").count(), NB);
    assert_eq!(kinds.iter().filter(|k| **k == "STAC item").count(), 2 * NB);
}

/// The angle bands (hundredths of a degree) are a sun for every pixel, and the corrected files are what the core gives for it.
#[test]
fn angle_bands_are_the_sun_of_each_pixel() {
    let mut f = fixture("angles", false);
    let mut spec_bands = f.spec.bands.clone();
    for (role, k) in [("sza", 0), ("saa", 1)] {
        // the same sun on every pixel but a little ramp, in hundredths of a degree, for each date
        let dates: Vec<Vec<f32>> =
            SUNS.iter().map(|s| (0..N).map(|i| (100.0 * (s[k] + 0.001 * (i % W) as f64)) as f32).collect()).collect();
        let path = f.dir.join(format!("{role}.tif"));
        write_tif(&path, &dates, -9999.0);
        spec_bands.insert(role.into(), path.to_string_lossy().to_string());
    }
    f.spec.bands = spec_bands;
    f.spec.suns = vec![];
    f.run(&[("angle_scale", json!("hundredths"))]).unwrap();
    let z: Vec<f64> = dem().iter().map(|&v| f64::from(v)).collect();
    let (slope, aspect) = topocorr::slope_aspect(&z, H, W, 30.0, -30.0, 1);
    for (t, sun) in SUNS.iter().enumerate() {
        let zen: Vec<f32> =
            (0..N).map(|i| ((100.0 * (sun[0] + 0.001 * (i % W) as f64)) as f32 as f64 / 100.0) as f32).collect();
        let azi: Vec<f32> =
            (0..N).map(|i| ((100.0 * (sun[1] + 0.001 * (i % W) as f64)) as f32 as f64 / 100.0) as f32).collect();
        let x: Vec<f32> = f.cube[t].iter().flatten().copied().collect();
        let mut out = vec![0f32; x.len()];
        topocorr::topo_correct(
            &x,
            1,
            NB,
            N,
            &slope,
            &aspect,
            &zen,
            &azi,
            N,
            None,
            Method::ScsC,
            5f32.to_radians(),
            100,
            &mut out,
            1,
        );
        for b in 0..NB {
            same_bits(&read(&f.date_file(t), b + 1), &out[b * N..(b + 1) * N], &format!("date {t} band {b}"));
        }
    }
    let rows = csv(&f.out("topo_c.csv"));
    assert!(rows[1..].iter().all(|r| r[column(&rows, "sun_from")] == "angle bands"));
}

/// The bands to correct can be chosen; the others are not in the files.
#[test]
fn only_the_chosen_bands_are_corrected() {
    let f = fixture("bands", false);
    f.run(&[("bands", json!("nir, 3"))]).unwrap();
    let ds = Dataset::open(f.date_file(0)).unwrap();
    assert_eq!(ds.raster_count(), 2);
    let names: Vec<String> = (1..=2).map(|b| ds.rasterband(b).unwrap().description().unwrap()).collect();
    assert_eq!(names, ["nir", "red"]);
    let rows = csv(&f.out("topo_c.csv"));
    assert_eq!(rows.len(), 1 + T * 2);
}

/// The DEM's notice is in the corrected files' metadata only when it is Copernicus's; a missing DEM file is an error.
#[test]
fn a_dem_file_is_needed_and_the_files_carry_the_method() {
    let f = fixture("dem", false);
    let e = f.run(&[("dem", json!(f.dir.join("none.tif").to_string_lossy()))]).err().unwrap();
    assert!(e.contains("does not exist"), "{e}");
    f.run(&[]).unwrap();
    let ds = Dataset::open(f.date_file(0)).unwrap();
    assert_eq!(ds.metadata_item("TOPOCORR_METHOD", "").as_deref(), Some("scs+c"));
    assert_eq!(ds.metadata_item("COPERNICUS_DEM_NOTICE", ""), None);
}

/// The Copernicus DEM from Planetary Computer over the scene's place (UTM 23S, the Serra da Mantiqueira, 64 x 56 pixels of
/// 30 m): the download, the cache, the warp, then the correction on a scene made of the real relief. Run by hand with
/// `cargo test --release -p janus-zeit --test topocorr -- --ignored --nocapture` (the network).
#[test]
#[ignore = "needs the network: Planetary Computer"]
fn the_copernicus_dem_is_read_from_planetary_computer() {
    use janus_zeit::dem;
    let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
    let (x0, y0) = grid();
    let g = janus_zeit::warp::Grid { wkt, gt: [x0, 30.0, 0.0, y0, 0.0, -30.0], w: W, h: H };
    let cache = std::env::temp_dir().join("janus-zeit-topocorr-test-network-cache");
    let _ = std::fs::remove_dir_all(&cache);
    let stop = AtomicBool::new(false);
    let t = std::time::Instant::now();
    let file = dem::copernicus(dem::GLO30, &g, &cache, "bilinear", 2, &|_, m| println!("    {m}"), &stop).unwrap();
    let first = t.elapsed().as_secs_f64();
    let t = std::time::Instant::now();
    assert_eq!(dem::copernicus(dem::GLO30, &g, &cache, "bilinear", 2, &|_, _| {}, &stop).unwrap(), file);
    let second = t.elapsed().as_secs_f64();
    let z = dem::on_grid(&file, &g, "bilinear", 2).unwrap();
    let (lo, hi) = z.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(l, h), &v| (l.min(v), h.max(v)));
    println!(
        "    GLO-30 over {W} x {H} pixels: {lo:.0} to {hi:.0} m; {first:.2} s the first time, {second:.3} s from the cache"
    );
    assert!(z.iter().all(|v| v.is_finite()) && lo > 0.0 && hi < 3000.0 && hi - lo > 10.0, "{lo} {hi}");
    let notice = Dataset::open(&file).unwrap().metadata_item("COPERNICUS_DEM_NOTICE", "").unwrap();
    assert!(notice.contains("WorldDEM-30") && notice.contains("Airbus"), "{notice}");
    // the correction through the job runner with the downloaded model (its cache removed first: the run reads it itself)
    let f = fixture_on("network", false, z);
    let _ = std::fs::remove_dir_all(f.dir.join("dem"));
    let mut spec = f.spec.clone();
    spec.tool = "topocorr".into();
    spec.params = params(&[("dem_source", json!("cop-dem-glo-30"))]);
    spec.output_dir = f.dir.join("run").join("out").to_string_lossy().into_owned();
    let t = std::time::Instant::now();
    run_job(&spec, &|_, _| {}, &stop).unwrap();
    println!("    the run with the download of the DEM: {:.2} s", t.elapsed().as_secs_f64());
    let report = std::fs::read_to_string(f.dir.join("run").join("out").join("report.txt")).unwrap();
    assert!(report.contains("Copernicus WorldDEM-30") && report.contains("do not incur any liability"), "{report}");
    let rows = csv(&f.dir.join("run").join("out").join("validation.csv"));
    let (b, a) = (column(&rows, "r_before"), column(&rows, "r_after"));
    for row in rows[1..].iter().filter(|r| r[column(&rows, "band")] != "green") {
        println!("    {} {}: r {} -> {}", row[0], row[1], row[b], row[a]);
    }
    let _ = std::fs::remove_dir_all(&cache);
}
