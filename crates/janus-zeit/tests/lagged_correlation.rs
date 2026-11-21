//! Lagged correlation (a tool after the bridge): a synthetic series of 96 monthly dates on 40 x 30 pixels in UTM whose
//! left half is a driver's series two months later (plus noise) and whose right half is noise; the driver on a coarser
//! grid in longitudes and latitudes (so the run puts it on the series' grid by a warp), the mean of its window, or a CSV
//! file. The best lag is 2 on the left half, what is significant by the false discovery rate is that half, the bands of
//! the lags are the lags, and a pixel's maps are zeit-core's own computation.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::{Dataset, DriverManager, Metadata};
use janus_zeit::job::{JobResult, JobSpec, OtherSeries, estimate, run_job};
use serde_json::{Value, json};
use zeit_core::anomaly::{AnomalySettings, Seasonal, anomalies_cube, days_from_civil};
use zeit_core::correlation::{LagSettings, YRef, lagged};

const T: usize = 96;
const W: usize = 40;
const H: usize = 30;

struct Lcg(u64);

impl Lcg {
    /// A value in [-1, 1).
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn days() -> Vec<i64> {
    (0..T as i64).map(|m| days_from_civil(2016 + (m / 12) as i32, (m % 12) as u32 + 1, 15)).collect()
}

fn years(days: &[i64]) -> Vec<f64> {
    days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect()
}

/// The driver: a seasonal cycle and an AR(1) series with the autocorrelation of a climate index.
fn driver() -> Vec<f64> {
    let mut rng = Lcg(11);
    let mut a = 0.0;
    (0..T)
        .map(|t| {
            a = 0.6 * a + 1.5 * rng.next();
            a + 0.5 * (2.0 * std::f64::consts::PI * (t % 12) as f64 / 12.0).cos()
        })
        .collect()
}

/// The series: [T][H][W]; the left half 0.8 x the driver two dates before + noise, the right half noise, both on a
/// seasonal cycle of their own.
fn series(d: &[f64]) -> Vec<f32> {
    let mut rng = Lcg(5);
    let mut out = Vec::with_capacity(T * W * H);
    for t in 0..T {
        let season = 0.7 * (2.0 * std::f64::consts::PI * (t % 12) as f64 / 12.0).sin();
        for _y in 0..H {
            for x in 0..W {
                let signal = if x < W / 2 && t >= 2 { 0.8 * d[t - 2] } else { 0.0 };
                out.push((5.0 + season + signal + 0.7 * rng.next()) as f32);
            }
        }
    }
    out
}

fn write_tif(path: &Path, values: &[f32], w: usize, h: usize, gt: [f64; 6], epsg: u32) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let bands = values.len() / (w * h);
    let mut ds = driver.create_with_band_type::<f32, _>(path, w, h, bands).unwrap();
    ds.set_geo_transform(&gt).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(epsg).unwrap().to_wkt().unwrap()).unwrap();
    for b in 0..bands {
        let mut band = ds.rasterband(b + 1).unwrap();
        band.set_no_data_value(Some(-9999.0)).unwrap();
        band.write((0, 0), (w, h), &mut Buffer::new((w, h), values[b * w * h..(b + 1) * w * h].to_vec())).unwrap();
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("janus-zeit-lagged-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const GT: [f64; 6] = [600000.0, 100.0, 0.0, 7500000.0, 0.0, -100.0];

/// The series on the UTM grid and the driver on a grid of longitudes and latitudes of 8 x 6 cells covering it (each
/// cell the driver and a little noise); the other series' description.
fn files(dir: &Path, d: &[f64]) -> (String, OtherSeries) {
    let x = dir.join("x.tif");
    write_tif(&x, &series(d), W, H, GT, 32723);
    // the extent of the series in longitudes and latitudes
    let mut utm = SpatialRef::from_epsg(32723).unwrap();
    let mut geo = SpatialRef::from_epsg(4326).unwrap();
    utm.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    geo.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let tr = CoordTransform::new(&utm, &geo).unwrap();
    let mut xs = [GT[0], GT[0] + GT[1] * W as f64, GT[0], GT[0] + GT[1] * W as f64];
    let mut ys = [GT[3], GT[3], GT[3] + GT[5] * H as f64, GT[3] + GT[5] * H as f64];
    tr.transform_coords(&mut xs, &mut ys, &mut [0.0; 4]).unwrap();
    let (lon0, lon1) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
    let (lat0, lat1) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
    let (cw, ch) = (8usize, 6usize);
    let (px, py) = ((lon1 - lon0) * 1.1 / cw as f64, (lat1 - lat0) * 1.1 / ch as f64);
    let gt = [lon0 - 0.05 * (lon1 - lon0), px, 0.0, lat1 + 0.05 * (lat1 - lat0), 0.0, -py];
    let mut rng = Lcg(3);
    let mut values = Vec::with_capacity(T * cw * ch);
    for v in d.iter().take(T) {
        for _ in 0..cw * ch {
            values.push((*v + 0.05 * rng.next()) as f32);
        }
    }
    let y = dir.join("y.tif");
    write_tif(&y, &values, cw, ch, gt, 4326);
    let dd = days();
    let other = OtherSeries {
        name: "driver".into(),
        inputs: vec![y.to_string_lossy().to_string()],
        years: years(&dd),
        days: Some(dd),
        ..Default::default()
    };
    (x.to_string_lossy().to_string(), other)
}

fn spec(dir: &Path, input: &str, other: Option<OtherSeries>, params: Value) -> JobSpec {
    let d = days();
    JobSpec {
        tool: "lagged_correlation".into(),
        params: params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        input: input.into(),
        nodata: Some(-9999.0),
        years: years(&d),
        days: Some(d),
        other,
        output_dir: dir.join("out").to_string_lossy().to_string(),
        threads: Some(4),
        ..Default::default()
    }
}

fn run(spec: &JobSpec) -> JobResult {
    run_job(spec, &|_, _| {}, &AtomicBool::new(false)).unwrap()
}

fn output<'a>(r: &'a JobResult, id: &str) -> &'a Value {
    r.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"))
}

/// A raster's bands, [band][pixels], NoData as NaN.
fn read(path: &str) -> Vec<Vec<f64>> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    (1..=ds.raster_count())
        .map(|b| {
            let band = ds.rasterband(b).unwrap();
            let nd = band.no_data_value();
            let v = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
            v.data().iter().map(|&x| if nd.is_some_and(|d| d == x) || x.is_nan() { f64::NAN } else { x }).collect()
        })
        .collect()
}

fn map(r: &JobResult, id: &str) -> Vec<f64> {
    read(output(r, id)["path"].as_str().unwrap()).remove(0)
}

fn halves() -> (Vec<usize>, Vec<usize>) {
    let all = |left: bool| (0..W * H).filter(|i| ((i % W) < W / 2) == left).collect::<Vec<_>>();
    (all(true), all(false))
}

/// The checks every way of giving the driver passes: the best lag is 2 on the left half, what is significant is
/// that half, q is there.
fn check_halves(r: &JobResult, what: &str) {
    let (left, right) = halves();
    let (lag, sig, q, best_r) = (map(r, "best_lag"), map(r, "significant"), map(r, "q"), map(r, "best_r"));
    let at2 = left.iter().filter(|&&i| lag[i] == 2.0).count();
    assert!(at2 as f64 >= 0.95 * left.len() as f64, "{what}: best lag 2 on {at2} of {} pixels", left.len());
    let found = left.iter().filter(|&&i| sig[i] == 2.0).count();
    assert!(found as f64 >= 0.95 * left.len() as f64, "{what}: {found} of {} significant", left.len());
    let false_found = right.iter().filter(|&&i| sig[i] == 2.0).count();
    // the false discovery rate: at most alpha (0.10) of what is found, with room for the chance of one run
    assert!(false_found as f64 <= 0.15 * right.len() as f64, "{what}: {false_found} of {} on noise", right.len());
    assert!(q.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), "{what}: q");
    // the correlation at lag 2 on the left: 0.8 signal against noise of 0.7 and the driver's own variance
    let mean = left.iter().map(|&i| best_r[i]).sum::<f64>() / left.len() as f64;
    assert!(mean > 0.5, "{what}: mean r {mean}");
    // significant is the classes 1 and 2 (NaN where there is no value): 2 where q <= alpha
    for i in 0..W * H {
        assert_eq!(sig[i] == 2.0, q[i] <= 0.10, "{what}: pixel {i}: q {} significant {}", q[i], sig[i]);
    }
}

#[test]
fn the_best_lag_is_two_where_the_driver_leads_and_the_driver_is_warped_onto_the_grid() {
    let dir = folder("pixel");
    let (x, other) = files(&dir, &driver());
    let r = run(&spec(&dir, &x, Some(other), json!({ "lags": 4 })));
    let ids: Vec<&str> = r.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["best_r", "best_lag", "best_p", "q", "significant", "n_eff", "r_lags"]);
    check_halves(&r, "the driver on another grid");
    // the bands of r_lags are the lags, 0 to 4
    let lags = read(output(&r, "r_lags")["path"].as_str().unwrap());
    assert_eq!(lags.len(), 5);
    let ds = Dataset::open(output(&r, "r_lags")["path"].as_str().unwrap()).unwrap();
    let names: Vec<String> = (1..=5).map(|b| ds.rasterband(b).unwrap().description().unwrap()).collect();
    assert_eq!(names, ["lag 0 months", "lag 1 month", "lag 2 months", "lag 3 months", "lag 4 months"]);
    let (left, _) = halves();
    let mean = |b: usize| left.iter().map(|&i| lags[b][i]).sum::<f64>() / left.len() as f64;
    assert!(mean(2) > mean(1) + 0.2 && mean(2) > mean(3) + 0.2, "{} {} {}", mean(1), mean(2), mean(3));
    // the driver's grid is not the run's: its warped VRT is gone at the end
    assert!(!dir.join("out/lagged_correlation_driver.vrt").exists());
    assert_eq!(output(&r, "best_lag")["unit"], "months");
    let p_star = output(&r, "significant")["p_threshold"].as_f64().unwrap();
    assert!(p_star > 0.0 && p_star < 1.0, "{p_star}");
    // the best lag is stored in whole numbers, the others in float32
    assert_eq!(output(&r, "best_lag")["dtype"], "int16");
    // bilinear resampling gives the same lags (the driver is the same in every cell)
    let b = run(&spec(
        &folder("pixel-bilinear"),
        &x,
        Some(driver_other(&dir)),
        json!({ "lags": 4, "resampling": "bilinear" }),
    ));
    check_halves(&b, "bilinear");
}

fn driver_other(dir: &Path) -> OtherSeries {
    let dd = days();
    OtherSeries {
        name: "driver".into(),
        inputs: vec![dir.join("y.tif").to_string_lossy().to_string()],
        years: years(&dd),
        days: Some(dd),
        ..Default::default()
    }
}

#[test]
fn the_driver_can_be_the_mean_of_its_window_or_a_csv_file() {
    let dir = folder("modes");
    let d = driver();
    let (x, other) = files(&dir, &d);
    let r = run(&spec(&dir, &x, Some(other), json!({ "lags": 4, "y_mode": "window_mean" })));
    check_halves(&r, "the window's mean");
    // a CSV of months (the first of each), with a header
    let mut csv = String::from("date,index\n");
    for (t, v) in d.iter().enumerate() {
        csv += &format!("{}-{:02},{v}\n", 2016 + t / 12, t % 12 + 1);
    }
    let path = dir.join("driver.csv");
    std::fs::write(&path, csv).unwrap();
    let r = run(&spec(&dir, &x, None, json!({ "lags": 4, "y_mode": "file", "y_file": path.to_string_lossy() })));
    check_halves(&r, "the CSV");
    // the lags in dates: a monthly driver matched to the series' dates within the tolerance, the lag in observations
    let r = run(&spec(
        &dir,
        &x,
        None,
        json!({ "lags": 4, "y_mode": "file", "y_file": path.to_string_lossy(), "step": "dates", "tolerance": 16.0 }),
    ));
    check_halves(&r, "dates");
    assert_eq!(output(&r, "best_lag")["unit"], "dates");
    let names: Vec<String> = {
        let ds = Dataset::open(output(&r, "r_lags")["path"].as_str().unwrap()).unwrap();
        (1..=5).map(|b| ds.rasterband(b).unwrap().description().unwrap()).collect()
    };
    assert_eq!(names[1], "lag 1 date");
}

#[test]
fn the_drivers_column_of_a_csv_like_the_eof_tools_is_chosen() {
    let dir = folder("columns");
    let d = driver();
    let (x, _) = files(&dir, &d);
    // date,year,filled,pc1,pc2: pc1 is noise, pc2 the real driver
    let mut rng = Lcg(99);
    let mut csv = String::from(
        "date,year,filled,pc1,pc2
",
    );
    for (t, v) in d.iter().enumerate() {
        csv += &format!(
            "{}-{:02},{:.4},0,{},{v}
",
            2016 + t / 12,
            t % 12 + 1,
            2016.0 + t as f64 / 12.0,
            2.0 * rng.next()
        );
    }
    let path = dir.join("pcs.csv");
    std::fs::write(&path, csv).unwrap();
    let file = path.to_string_lossy().to_string();
    let go = |column: Option<&str>| {
        let mut params = json!({ "lags": 4, "y_mode": "file", "y_file": file });
        if let Some(c) = column {
            params["y_column"] = json!(c);
        }
        run_job(&spec(&dir, &x, None, params), &|_, _| {}, &AtomicBool::new(false))
    };
    // by name (exactly or not), and by number after the date column (year 1, filled 2, pc1 3, pc2 4)
    for column in ["pc2", "PC2", "4"] {
        check_halves(&go(Some(column)).unwrap(), column);
    }
    // empty: the first column that is not the year or the flag, pc1, noise: no best lag of 2 to speak of
    let r = go(None).unwrap();
    let (left, _) = halves();
    let lag = map(&r, "best_lag");
    let at2 = left.iter().filter(|&&i| lag[i] == 2.0).count();
    assert!((at2 as f64) < 0.5 * left.len() as f64, "the default column (pc1) is noise: best lag 2 on {at2} pixels");
    // the decimal year is a column too, when asked for
    assert!(go(Some("year")).is_ok());
    // an unknown name or number is refused with the columns
    let e = go(Some("pc9")).err().expect("refused");
    assert!(e.contains("no column 'pc9'") && e.contains("year, filled, pc1, pc2"), "{e}");
    assert!(go(Some("5")).err().expect("refused").contains("4 column(s)"));
}

#[test]
fn a_pixel_is_zeit_cores_own_computation() {
    let dir = folder("direct");
    let d = driver();
    let (x, _) = files(&dir, &d);
    let path = dir.join("driver.csv");
    let mut csv = String::new();
    for (t, v) in d.iter().enumerate() {
        csv += &format!("{}-{:02}-15,{v}\n", 2016 + t / 12, t % 12 + 1);
    }
    std::fs::write(&path, csv).unwrap();
    let r = run(&spec(&dir, &x, None, json!({ "lags": 3, "y_mode": "file", "y_file": path.to_string_lossy() })));
    // the direct computation on the monthly grid (the dates are one a month): the anomalies, then the lagged correlation
    let values = series(&d);
    let a = AnomalySettings {
        seasonal: Seasonal::Monthly { min_per_month: 2 },
        base: None,
        detrend: false,
        standardize: false,
    };
    let mid: Vec<i64> = days();
    let mut xs: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
    anomalies_cube(&mut xs, T, W * H, &mid, &a, 1);
    let mut ys = d.clone();
    anomalies_cube(&mut ys, T, 1, &mid, &a, 1);
    let index: Vec<Vec<i32>> =
        (0..=3).map(|k| (0..T as i32).map(|i| if i - k >= 0 { i - k } else { -1 }).collect()).collect();
    let want = lagged(&xs, T, W * H, YRef::Series(&ys), &index, &LagSettings::default(), 1);
    let lags = read(output(&r, "r_lags")["path"].as_str().unwrap());
    for (l, band) in lags.iter().enumerate().take(4) {
        let worst = (0..W * H).map(|i| (band[i] - want.r[l * W * H + i]).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-6, "lag {l}: {worst}");
    }
    let (best_lag, best_p, n_eff) = (map(&r, "best_lag"), map(&r, "best_p"), map(&r, "n_eff"));
    for i in 0..W * H {
        assert_eq!(best_lag[i], f64::from(want.best_lag[i]), "pixel {i}");
        assert!((best_p[i] - want.best_p[i]).abs() < 1e-6, "pixel {i}");
        assert!((n_eff[i] - want.n_eff[want.best_lag[i] as usize * W * H + i]).abs() < 1e-4, "pixel {i}");
    }
    // Spearman and the largest positive r are options of the same call
    let s = run(&spec(
        &dir,
        &x,
        None,
        json!({ "lags": 3, "y_mode": "file", "y_file": path.to_string_lossy(), "method": "spearman", "best": "positive" }),
    ));
    let (sr, sl) = (map(&s, "best_r"), map(&s, "best_lag"));
    let (left, _) = halves();
    assert!(left.iter().all(|&i| sl[i] == 2.0) && left.iter().all(|&i| sr[i] > 0.3));
}

#[test]
fn the_tool_says_what_it_lacks() {
    let dir = folder("errors");
    let d = driver();
    let (x, other) = files(&dir, &d);
    let fail = |s: &JobSpec| run_job(s, &|_, _| {}, &AtomicBool::new(false)).err().expect("refused");
    assert!(fail(&spec(&dir, &x, None, json!({}))).contains("driver"));
    assert!(fail(&spec(&dir, &x, None, json!({ "y_mode": "file" }))).contains("CSV"));
    let mut bad = spec(&dir, &x, Some(other.clone()), json!({}));
    bad.other.as_mut().unwrap().days = None;
    assert!(fail(&bad).contains("real dates"));
    let mut s = spec(&dir, &x, Some(other), json!({ "alpha_fdr": 2.0 }));
    assert!(fail(&s).contains("alpha_fdr"));
    s.params.insert("alpha_fdr".into(), json!(0.1));
    s.days = None;
    assert!(fail(&s).contains("real dates"));
    // the estimate is a number
    let e = estimate(&spec(&dir, &x, None, json!({ "y_mode": "file" }))).unwrap();
    assert!(e.seconds(40, 30).is_finite() && e.seconds(40, 30) > 0.0);
    let tool = janus_zeit::manifest().tool("lagged_correlation").unwrap();
    let y = tool.params.iter().find(|p| p.id == "y").unwrap();
    assert_eq!((y.kind.as_str(), y.any_crs), ("series", Some(true)));
}

/// With LAGGED_COMPARE_DIR set: the series and the driver (a CSV) and Janus's maps are written there for
/// tools/lagged_compare.py's comparison with zeit.lagged_correlation (not part of the usual run).
#[test]
fn write_the_series_for_the_comparison_with_zeit() {
    let Ok(dir) = std::env::var("LAGGED_COMPARE_DIR") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let d = driver();
    let (x, _) = files(&dir, &d);
    let mut csv = String::from("date,value\n");
    for (t, v) in d.iter().enumerate() {
        csv += &format!("{}-{:02}-15,{v}\n", 2016 + t / 12, t % 12 + 1);
    }
    let path = dir.join("driver.csv");
    std::fs::write(&path, csv).unwrap();
    for (tag, extra) in [
        ("month", json!({})),
        ("spearman", json!({ "method": "spearman", "best": "positive" })),
        ("dates", json!({ "step": "dates", "tolerance": 8.0 })),
        ("harmonic", json!({ "anomaly": "harmonic" })),
    ] {
        let mut p = json!({ "lags": 4, "y_mode": "file", "y_file": path.to_string_lossy() });
        for (k, v) in extra.as_object().unwrap() {
            p[k] = v.clone();
        }
        let mut s = spec(&dir, &x, None, p);
        s.output_dir = dir.join(format!("janus_{tag}")).to_string_lossy().to_string();
        run(&s);
    }
}
