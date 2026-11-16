//! Temporal EOF (a tool after the bridge): a synthetic series of 48 monthly dates on 40 x 30 pixels (in UTM and in
//! longitudes and latitudes) made of a seasonal cycle, two known patterns times two known components and a little
//! noise. The maps are the patterns, the run is zeit-core's own computation (anomalies, weights, decomposition, signs,
//! maps) on the same values, the cache can go to a file, and the describe has what the app shows.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager};
use janus_zeit::eof::run_with_limit;
use janus_zeit::job::{JobResult, JobSpec, estimate, run_job};
use janus_zeit::{Params, manifest};
use serde_json::{Value, json};
use zeit_core::anomaly::{AnomalySettings, Seasonal, anomalies_cube, days_from_civil};
use zeit_core::eof::{self, Blocks, Dense, Settings as CoreSettings, Solver};

const T: usize = 48;
const W: usize = 40;
const H: usize = 30;

/// A small deterministic generator (a linear congruential one) of values in [-1, 1).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn days() -> Vec<i64> {
    (0..T as i64).map(|m| days_from_civil(2018 + (m / 12) as i32, (m % 12) as u32 + 1, 15)).collect()
}

fn years(days: &[i64]) -> Vec<f64> {
    days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect()
}

/// The two spatial patterns (left blob, right blob), by pixel.
fn pattern(k: usize, x: usize, y: usize) -> f64 {
    let (cx, cy, s) = if k == 0 { (10.0, 15.0, 6.0) } else { (30.0, 8.0, 5.0) };
    let (dx, dy) = (x as f64 - cx, y as f64 - cy);
    (-(dx * dx + dy * dy) / (2.0 * s * s)).exp()
}

/// The two components in time (periods of 29 and 47 months, not the year's), as the anomalies leave them: the monthly
/// climatology taken out of each, the second made orthogonal to the first (so the modes of the data are the patterns).
fn components() -> [Vec<f64>; 2] {
    let tau = 2.0 * std::f64::consts::PI;
    let mut z: Vec<Vec<f64>> = vec![
        (0..T).map(|t| 3.0 * (tau * t as f64 / 29.0).sin()).collect(),
        (0..T).map(|t| 1.5 * (tau * t as f64 / 47.0 + 0.5).cos()).collect(),
    ];
    for v in z.iter_mut() {
        let c: Vec<f64> = (0..12).map(|m| (0..T / 12).map(|k| v[m + 12 * k]).sum::<f64>() / (T / 12) as f64).collect();
        (0..T).for_each(|t| v[t] -= c[t % 12]);
    }
    let dot = |a: &[f64], b: &[f64]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let d = dot(&z[1], &z[0]) / dot(&z[0], &z[0]);
    let first = z[0].clone();
    z[1].iter_mut().zip(&first).for_each(|(a, b)| *a -= d * b);
    let [a, b]: [Vec<f64>; 2] = z.try_into().unwrap();
    [a, b]
}

/// The values of every date and pixel: [T][H][W].
fn cube() -> Vec<f32> {
    cube_with(0.6)
}

/// The same with a seasonal cycle of this amplitude.
fn cube_with(amplitude: f64) -> Vec<f32> {
    let mut rng = Lcg(7);
    let z = components();
    let mut out = Vec::with_capacity(T * W * H);
    for (t, (z0, z1)) in z[0].iter().zip(&z[1]).enumerate() {
        let season = amplitude * (2.0 * std::f64::consts::PI * (t % 12) as f64 / 12.0).sin();
        for y in 0..H {
            for x in 0..W {
                let base = 2.0 + 0.01 * x as f64;
                let v = base
                    + season * (1.0 + 0.02 * y as f64)
                    + pattern(0, x, y) * z0
                    + pattern(1, x, y) * z1
                    + 0.03 * rng.next();
                out.push(v as f32);
            }
        }
    }
    out
}

/// A multiband GeoTIFF of `values` [band][pixels] on a grid.
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

/// The folder of a test, empty.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("janus-zeit-eof-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The series' file: UTM zone 23 south (100 m pixels), or longitudes and latitudes (0.001 degrees).
fn input(dir: &Path, epsg: u32, values: &[f32]) -> String {
    let gt = if epsg == 4326 {
        [-44.0, 0.001, 0.0, -22.6, 0.0, -0.001]
    } else {
        [600000.0, 100.0, 0.0, 7500000.0, 0.0, -100.0]
    };
    let path = dir.join("input.tif");
    write_tif(&path, values, W, H, gt, epsg);
    path.to_string_lossy().to_string()
}

fn spec(dir: &Path, input: &str, params: Value) -> JobSpec {
    let d = days();
    JobSpec {
        tool: "eof".into(),
        params: params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        input: input.into(),
        nodata: Some(-9999.0),
        years: years(&d),
        days: Some(d),
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

/// A map's pixels (NaN where it has no value).
fn read_map(path: &str) -> Vec<f64> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let band = ds.rasterband(1).unwrap();
    let nd = band.no_data_value();
    let v = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
    v.data().iter().map(|&x| if nd.is_some_and(|d| d == x) || x.is_nan() { f64::NAN } else { x }).collect()
}

fn corr(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len() as f64;
    let (ma, mb) = (a.iter().sum::<f64>() / n, b.iter().sum::<f64>() / n);
    let (mut sab, mut saa, mut sbb) = (0.0, 0.0, 0.0);
    for (x, y) in a.iter().zip(b) {
        sab += (x - ma) * (y - mb);
        saa += (x - ma) * (x - ma);
        sbb += (y - mb) * (y - mb);
    }
    sab / (saa * sbb).sqrt()
}

fn truth(k: usize) -> Vec<f64> {
    (0..H).flat_map(|y| (0..W).map(move |x| pattern(k, x, y))).collect()
}

fn amplitude(o: &Value) -> Vec<f64> {
    o["amplitude"]["y"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect()
}

fn defaults(extra: Value) -> Value {
    let mut p = json!({ "n_modes": 6, "maps": 3, "weights": "none" });
    for (k, v) in extra.as_object().unwrap() {
        p[k] = v.clone();
    }
    p
}

#[test]
fn the_maps_are_the_patterns_in_utm_and_in_longitudes_and_latitudes() {
    for epsg in [32723u32, 4326] {
        let dir = folder(&format!("patterns-{epsg}"));
        let tif = input(&dir, epsg, &cube());
        for weights in ["none", "area"] {
            let r = run(&spec(&dir, &tif, defaults(json!({ "weights": weights }))));
            assert_eq!(r.outputs.len(), 3, "{epsg} {weights}");
            let (e1, e2) = (
                read_map(output(&r, "eof_1")["path"].as_str().unwrap()),
                read_map(output(&r, "eof_2")["path"].as_str().unwrap()),
            );
            assert!(e1.iter().all(|v| v.is_finite()));
            // up to sign: the sign rule makes the PC correlate with the mean anomaly, the pattern's sign follows it
            assert!(corr(&e1, &truth(0)).abs() > 0.99, "{epsg} {weights}: EOF 1 {}", corr(&e1, &truth(0)));
            assert!(corr(&e2, &truth(1)).abs() > 0.99, "{epsg} {weights}: EOF 2 {}", corr(&e2, &truth(1)));
            // the georeferencing is the series'
            let (a, b) =
                (Dataset::open(output(&r, "eof_1")["path"].as_str().unwrap()).unwrap(), Dataset::open(&tif).unwrap());
            assert_eq!(a.geo_transform().unwrap(), b.geo_transform().unwrap());
            assert_eq!(a.raster_size(), (W, H));
            assert!(a.projection().contains("23") || epsg == 4326);
        }
    }
}

#[test]
fn describe_has_the_components_and_the_variance_in_the_names() {
    let dir = folder("describe");
    let tif = input(&dir, 32723, &cube());
    let r = run(&spec(&dir, &tif, defaults(json!({ "north_n": "dates" }))));
    let o1 = output(&r, "eof_1");
    let name = o1["name"].as_str().unwrap();
    assert!(name.starts_with("EOF 1: ") && name.ends_with("% of the variance"), "{name}");
    assert_eq!(o1["mode"], 1);
    let variance = o1["variance"].as_f64().unwrap();
    assert!(variance > 0.5 && variance < 1.0, "{variance}");
    assert!(name.contains(&format!("{:.1} %", 100.0 * variance)), "{name}");
    // the amplitude: the decimal year of each date and its component, which follows the true one
    let x: Vec<f64> = o1["amplitude"]["x"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert_eq!(x, years(&days()));
    let pc = amplitude(o1);
    assert_eq!(pc.len(), T);
    let z = components()[0].clone();
    assert!(corr(&pc, &z).abs() > 0.99, "PC 1 {}", corr(&pc, &z));
    // unit variance (scaling "units"): its standard deviation over the dates is 1
    let m = pc.iter().sum::<f64>() / T as f64;
    let sd = (pc.iter().map(|v| (v - m) * (v - m)).sum::<f64>() / (T - 1) as f64).sqrt();
    assert!((sd - 1.0).abs() < 1e-6, "{sd}");
    assert!(o1["north_error"].as_f64().unwrap() > 0.0);
    assert!(o1["separable"].as_bool().unwrap());
    // the first two modes are the two patterns: they hold nearly all the variance
    let v2 = output(&r, "eof_2")["variance"].as_f64().unwrap();
    assert!(variance + v2 > 0.97, "{variance} + {v2}");
    // the files of the run
    let out = dir.join("out");
    let csv = std::fs::read_to_string(out.join("pcs.csv")).unwrap();
    assert_eq!(csv.lines().count(), T + 1);
    assert!(csv.starts_with("date,year,filled,pc1,pc2"));
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(out.join("eof.json")).unwrap()).unwrap();
    assert_eq!(
        (doc["modes"].as_u64(), doc["n_pixels"].as_u64(), doc["cache"].as_str()),
        (Some(6), Some((W * H) as u64), Some("memory"))
    );
    assert_eq!(doc["solver"]["name"], "exact");
}

/// The same computation made directly by zeit-core on the values the file holds.
fn direct(values: &[f32], weights: Option<&[f64]>, units: bool, solver: Solver) -> (Vec<Vec<f64>>, Vec<f64>) {
    let mut x: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
    let a = AnomalySettings {
        seasonal: Seasonal::Monthly { min_per_month: 2 },
        base: None,
        detrend: false,
        standardize: false,
    };
    anomalies_cube(&mut x, T, W * H, &days(), &a, 1);
    let dense = Dense::new(&x, T, W * H, weights, 0.8, 0.5).unwrap();
    let mut d = eof::decompose(&dense, &CoreSettings { modes: 6, solver, seed: 42 }, 1, &|_| {}).unwrap();
    eof::fix_signs(&mut d);
    let f = ((dense.dates() - 1) as f64).sqrt();
    let mut buf = vec![];
    let range = dense.block(0, &mut buf);
    let mut v = vec![0.0; range.len() * 6];
    eof::loadings(&d, &buf, range.len(), &mut v);
    let maps = (0..6)
        .map(|k| {
            (0..range.len())
                .map(|p| {
                    let sw = dense.sqrt_weights_all()[p];
                    if units { v[k * range.len() + p] * d.singular[k] / f / sw } else { v[k * range.len() + p] }
                })
                .collect()
        })
        .collect();
    (maps, d.explained())
}

#[test]
fn a_run_is_a_direct_zeit_core_computation() {
    let dir = folder("direct");
    let values = cube();
    let tif = input(&dir, 32723, &values);
    for (units, scaling) in [(true, "units"), (false, "none")] {
        let r = run(&spec(&dir, &tif, defaults(json!({ "scaling": scaling }))));
        let (maps, explained) = direct(&values, None, units, Solver::Auto);
        for k in 0..3 {
            let got = read_map(output(&r, &format!("eof_{}", k + 1))["path"].as_str().unwrap());
            let scale = maps[k].iter().fold(0.0f64, |m, v| m.max(v.abs()));
            let worst = got.iter().zip(&maps[k]).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
            // the anomalies are kept as float32 and the map is written as float32
            assert!(worst <= 1e-5 * scale, "{scaling} mode {}: {worst} against a scale of {scale}", k + 1);
            let var = output(&r, &format!("eof_{}", k + 1))["variance"].as_f64().unwrap();
            assert!((var - explained[k]).abs() < 1e-6, "variance of mode {}: {var} vs {}", k + 1, explained[k]);
        }
    }
}

#[test]
fn the_area_weights_are_the_pixel_areas_normalized() {
    let dir = folder("area");
    let values = cube();
    let tif = input(&dir, 4326, &values);
    let r = run(&spec(&dir, &tif, defaults(json!({ "weights": "area" }))));
    // the areas of the rows of the series' grid, mean 1
    let ds = Dataset::open(&tif).unwrap();
    let areas = janus_zeit::areas::PixelAreas::new(&ds.geo_transform().unwrap(), &ds.projection(), 0, 0, W, H);
    let mut w = vec![0.0; W * H];
    for y in 0..H {
        areas.row(y, &mut w[y * W..(y + 1) * W]);
    }
    let mean = w.iter().sum::<f64>() / w.len() as f64;
    w.iter_mut().for_each(|v| *v /= mean);
    let (maps, explained) = direct(&values, Some(&w), true, Solver::Auto);
    let got = read_map(output(&r, "eof_1")["path"].as_str().unwrap());
    let scale = maps[0].iter().fold(0.0f64, |m, v| m.max(v.abs()));
    let worst = got.iter().zip(&maps[0]).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
    assert!(worst <= 1e-5 * scale, "{worst} against {scale}");
    assert!((output(&r, "eof_1")["variance"].as_f64().unwrap() - explained[0]).abs() < 1e-6);
    // cosine of the latitude is for the same grid, and refused for a projected one
    let c = run(&spec(&dir, &tif, defaults(json!({ "weights": "coslat" }))));
    let c1 = read_map(output(&c, "eof_1")["path"].as_str().unwrap());
    assert!(corr(&c1, &got) > 0.9999);
    let dir2 = folder("coslat-utm");
    let utm = input(&dir2, 32723, &values);
    let err =
        run_job(&spec(&dir2, &utm, defaults(json!({ "weights": "coslat" }))), &|_, _| {}, &AtomicBool::new(false))
            .err()
            .expect("refused");
    assert!(err.contains("geographic"), "{err}");
}

#[test]
fn the_cache_in_a_file_gives_the_same_maps() {
    let dir = folder("cache");
    let tif = input(&dir, 32723, &cube());
    let mut s = spec(&dir, &tif, defaults(json!({})));
    let tool = manifest::manifest().tool("eof").unwrap();
    let p = Params::with_defaults("eof", &s.params).unwrap();
    let memory = run_with_limit(&s, tool, &p, &|_, _| {}, &AtomicBool::new(false), u64::MAX).unwrap();
    s.output_dir = dir.join("out_file").to_string_lossy().to_string();
    // a limit of a few bytes: the kept pixels' anomalies go to a scratch file in the run's folder
    let file = run_with_limit(&s, tool, &p, &|_, _| {}, &AtomicBool::new(false), 1000).unwrap();
    for k in 1..=3 {
        let (a, b) = (
            read_map(output(&memory, &format!("eof_{k}"))["path"].as_str().unwrap()),
            read_map(output(&file, &format!("eof_{k}"))["path"].as_str().unwrap()),
        );
        assert_eq!(a, b, "mode {k}");
    }
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out_file/eof.json")).unwrap()).unwrap();
    assert_eq!(doc["cache"], "file");
    // the scratch file is deleted at the end
    assert!(!dir.join("out_file/eof_cache.bin").exists());
}

#[test]
fn the_randomized_solver_finds_the_same_modes_and_the_threads_change_nothing() {
    let dir = folder("randomized");
    let tif = input(&dir, 32723, &cube());
    let exact = run(&spec(&dir, &tif, defaults(json!({ "solver": "exact" }))));
    let rand =
        run(&spec(&dir, &tif, defaults(json!({ "solver": "randomized", "oversampling": 10, "power_iterations": 3 }))));
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out/eof.json")).unwrap()).unwrap();
    assert_eq!(doc["solver"]["name"], "randomized");
    for k in 1..=2 {
        let (a, b) = (output(&exact, &format!("eof_{k}")), output(&rand, &format!("eof_{k}")));
        let (va, vb) = (a["variance"].as_f64().unwrap(), b["variance"].as_f64().unwrap());
        assert!((va - vb).abs() < 1e-6 * va, "mode {k}: {va} vs {vb}");
        let (ma, mb) = (read_map(a["path"].as_str().unwrap()), read_map(b["path"].as_str().unwrap()));
        assert!(corr(&ma, &mb) > 0.999999, "mode {k}");
    }
    // one thread or four: the same bits
    let mut one = spec(&dir, &tif, defaults(json!({ "solver": "randomized" })));
    one.threads = Some(1);
    one.output_dir = dir.join("out_one").to_string_lossy().to_string();
    let a = run(&one);
    let mut four = one.clone();
    four.threads = Some(4);
    four.output_dir = dir.join("out_four").to_string_lossy().to_string();
    let b = run(&four);
    assert_eq!(
        read_map(output(&a, "eof_1")["path"].as_str().unwrap()),
        read_map(output(&b, "eof_1")["path"].as_str().unwrap())
    );
}

#[test]
fn the_rotation_and_the_correlation_maps_are_outputs_of_their_own() {
    let dir = folder("rotated");
    let tif = input(&dir, 32723, &cube());
    let r = run(&spec(&dir, &tif, defaults(json!({ "rotate": 3, "correlation_maps": true, "maps": 2 }))));
    let ids: Vec<&str> = r.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["eof_1", "eof_2", "corrmap_1", "corrmap_2", "eof_rotated_1", "eof_rotated_2", "eof_rotated_3"]);
    let name = output(&r, "eof_rotated_1")["name"].as_str().unwrap();
    assert!(name.starts_with("Rotated EOF 1: "), "{name}");
    // a rotation keeps the variance of the modes it rotates
    let rv: f64 = (1..=3).map(|k| output(&r, &format!("eof_rotated_{k}"))["variance"].as_f64().unwrap()).sum();
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("out/eof.json")).unwrap()).unwrap();
    let ev: f64 = doc["variance"].as_array().unwrap()[..3].iter().map(|v| v.as_f64().unwrap()).sum();
    assert!((rv - ev).abs() < 1e-6, "{rv} against {ev}");
    assert_eq!(amplitude(output(&r, "eof_rotated_1")).len(), T);
    // the correlation of a pixel with a mode's PC is a correlation
    for k in 1..=2 {
        let c = read_map(output(&r, &format!("corrmap_{k}"))["path"].as_str().unwrap());
        assert!(c.iter().all(|v| v.is_finite() && v.abs() <= 1.0 + 1e-6), "mode {k}");
    }
    // the rotated patterns are the two blobs, each in its own mode
    let rot: Vec<Vec<f64>> =
        (1..=2).map(|k| read_map(output(&r, &format!("eof_rotated_{k}"))["path"].as_str().unwrap())).collect();
    let best = |t: usize| rot.iter().map(|m| corr(m, &truth(t)).abs()).fold(0.0, f64::max);
    assert!(best(0) > 0.99 && best(1) > 0.99, "{} {}", best(0), best(1));
    // a rotation of one mode, or of more than the modes computed, is refused
    for bad in [1, 9] {
        let err = run_job(&spec(&dir, &tif, defaults(json!({ "rotate": bad }))), &|_, _| {}, &AtomicBool::new(false))
            .err()
            .expect("refused");
        assert!(err.contains("rotate"), "{err}");
    }
}

#[test]
fn a_date_with_few_valid_pixels_gets_its_component_from_the_gappy_projection() {
    let dir = folder("gappy");
    // no seasonal cycle and the mean as the anomaly: a date's missing pixels change the monthly climatology of the others
    let complete = cube_with(0.0);
    let mut values = complete.clone();
    // the date 20: only 30 % of the pixels valid, so it is left out of the fit
    let t = 20;
    for (i, v) in values[t * W * H..(t + 1) * W * H].iter_mut().enumerate() {
        if i % 10 >= 3 {
            *v = -9999.0;
        }
    }
    let tif = input(&dir, 32723, &values);
    let params = defaults(json!({ "min_valid": 0.9, "anomaly": "none" }));
    let r = run(&spec(&dir, &tif, params.clone()));
    let pc = amplitude(output(&r, "eof_1"));
    assert!(pc[t].is_finite(), "{pc:?}");
    let csv = std::fs::read_to_string(dir.join("out/pcs.csv")).unwrap();
    let line: Vec<&str> = csv.lines().nth(t + 1).unwrap().split(',').collect();
    assert_eq!(line[2], "1", "the date is flagged as filled");
    assert_eq!(csv.lines().nth(t).unwrap().split(',').nth(2), Some("0"));
    // zeit-core's own gappy projection on the same values: Janus's is the same component
    let mut x: Vec<f64> = values.iter().map(|&v| if v == -9999.0 { f64::NAN } else { f64::from(v) }).collect();
    let a = AnomalySettings { seasonal: Seasonal::Mean, base: None, detrend: false, standardize: false };
    anomalies_cube(&mut x, T, W * H, &days(), &a, 1);
    let dense = Dense::new(&x, T, W * H, None, 0.9, 0.5).unwrap();
    assert!(!dense.kept_dates().contains(&t) && dense.kept_dates().len() == T - 1);
    let mut d = eof::decompose(&dense, &CoreSettings { modes: 6, solver: Solver::Auto, seed: 42 }, 1, &|_| {}).unwrap();
    eof::fix_signs(&mut d);
    let (mut buf, mut v) = (vec![], vec![]);
    let range = dense.block(0, &mut buf);
    v.resize(range.len() * 6, 0.0);
    eof::loadings(&d, &buf, range.len(), &mut v);
    let mut row = vec![];
    dense.weighted_row(t, &mut row);
    let coef = eof::project_gappy(&v, 6, &row);
    let want = coef[0] / d.singular[0] * ((dense.dates() - 1) as f64).sqrt();
    assert!((pc[t] - want).abs() < 1e-3 * want.abs().max(1.0), "{} against zeit-core's {want}", pc[t]);
    // and the component the complete series has at that date, up to the pixels that are missing
    let full_dir = folder("gappy-full");
    let full = run(&spec(&full_dir, &input(&full_dir, 32723, &complete), params));
    let all = amplitude(output(&full, "eof_1"));
    assert!((pc[t] - all[t]).abs() < 0.35, "{} against {}", pc[t], all[t]);
}

#[test]
fn a_window_is_a_run_on_its_own_and_the_estimate_is_a_number() {
    let dir = folder("window");
    let tif = input(&dir, 32723, &cube());
    let mut s = spec(&dir, &tif, defaults(json!({ "maps": 1 })));
    s.window = Some([5, 4, 33, 26]);
    let r = run(&s);
    assert_eq!(r.window, [5, 4, 33, 26]);
    let ds = Dataset::open(output(&r, "eof_1")["path"].as_str().unwrap()).unwrap();
    assert_eq!(ds.raster_size(), (28, 22));
    assert_eq!(ds.geo_transform().unwrap()[0], 600000.0 + 5.0 * 100.0);
    let e = estimate(&s).unwrap();
    let secs = e.seconds(28, 22);
    assert!(secs.is_finite() && secs > 0.0, "{secs}");
}

#[test]
fn the_tool_is_refused_without_what_it_needs() {
    let dir = folder("errors");
    let tif = input(&dir, 32723, &cube());
    let mut s = spec(&dir, &tif, defaults(json!({})));
    s.days = None;
    let err = run_job(&s, &|_, _| {}, &AtomicBool::new(false)).err().expect("refused");
    assert!(err.contains("real dates"), "{err}");
    // the mean as the anomaly needs none
    let mut s = spec(&dir, &tif, defaults(json!({ "anomaly": "none" })));
    s.days = None;
    assert!(run_job(&s, &|_, _| {}, &AtomicBool::new(false)).is_ok());
    let tool = manifest::manifest().tool("eof").unwrap();
    assert_eq!((tool.modes.as_slice(), tool.requires.min_dates), (&["raster".to_string()][..], 8));
}

/// With EOF_COMPARE_DIR set: the synthetic series and Janus's maps are written there for tools/eof_compare.py's
/// comparison with zeit.eof (not part of the usual run).
#[test]
fn write_the_series_for_the_comparison_with_zeit() {
    let Ok(dir) = std::env::var("EOF_COMPARE_DIR") else { return };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let tif = input(&dir, 32723, &cube());
    for (weights, extra) in
        [("none", json!({})), ("area", json!({})), ("none", json!({ "rotate": 3, "correlation_maps": true }))]
    {
        let tag = format!("{weights}{}", if extra.get("rotate").is_some() { "_rot" } else { "" });
        let mut s = spec(&dir, &tif, defaults(json!({ "weights": weights, "maps": 4 })));
        for (k, v) in extra.as_object().unwrap() {
            s.params.insert(k.clone(), v.clone());
        }
        s.output_dir = dir.join(format!("janus_{tag}")).to_string_lossy().to_string();
        run(&s);
    }
    // longitudes and latitudes with pixels of 0.1 degrees (4 x 3 degrees at about 15 degrees south): the weights vary
    let geo = dir.join("input4326.tif");
    write_tif(&geo, &cube(), W, H, [-50.0, 0.1, 0.0, -15.0, 0.0, -0.1], 4326);
    for weights in ["area", "coslat"] {
        let mut s = spec(&dir, &geo.to_string_lossy(), defaults(json!({ "weights": weights, "maps": 4 })));
        s.output_dir = dir.join(format!("janus_geo_{weights}")).to_string_lossy().to_string();
        run(&s);
    }
}
