//! MNF, CVA, iMAD and the radiometric normalization (tools after the bridge): a synthetic series of three dates of five
//! bands (two smooth latent fields mixed into the bands, a gain and an offset a date, noise, a block that changed on the
//! last date, a pixel without value) run through `run_job`, and zeit-core called directly on the same values: the maps,
//! the tables and the files must be what the core gives.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::raster::Buffer;
use gdal::spatial_ref::SpatialRef;
use gdal::{Dataset, DriverManager, Metadata};
use janus_zeit::job::{JobResult, JobSpec, estimate, run_job};
use serde_json::{Value, json};
use zeit_core::cva::{self, CvaOptions, Direction};
use zeit_core::imad::{self, ImadOptions};
use zeit_core::mnf::{self, Noise, Scale, StatsOptions};

const NB: usize = 5;
const T: usize = 3;
const NAMES: [&str; NB] = ["blue", "green", "red", "nir", "swir1"];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

/// The bands of a date, [band][y][x] float32: the latent fields mixed, the date's gain and offset, noise; a block
/// changed on the last date; a pixel of the first date without a value.
fn date_cube(w: usize, h: usize, t: usize, noise: f64) -> Vec<f32> {
    let mut rng = Lcg(11 + t as u64);
    let tau = 2.0 * std::f64::consts::PI;
    let (gain, offset) = [(1.0, 0.0), (1.25, 0.03), (0.9, -0.02)][t];
    let mut out = Vec::with_capacity(NB * w * h);
    for b in 0..NB {
        for y in 0..h {
            for x in 0..w {
                let (xf, yf) = (x as f64, y as f64);
                let l1 = (tau * (xf / 37.0 + yf / 53.0)).sin();
                let l2 = (tau * (xf / 23.0 - yf / 41.0)).cos();
                let base = 0.2 + 0.05 * b as f64 + (0.6 + 0.1 * b as f64) * l1 + (0.3 - 0.05 * b as f64) * l2;
                let mut v = gain * base + offset + noise * rng.next();
                if t == 2 && (10..20).contains(&x) && (8..16).contains(&y) {
                    v += match b {
                        3 => 0.9,
                        2 => -0.5,
                        _ => 0.0,
                    };
                }
                if t == 0 && x == 3 && y == 3 {
                    v = f64::NAN;
                }
                out.push(v as f32);
            }
        }
    }
    out
}

fn write_tif(path: &Path, bands: &[Vec<f32>], w: usize, h: usize) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<f32, _>(path, w, h, bands.len()).unwrap();
    ds.set_geo_transform(&[600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
    ds.set_projection(&SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
    for (k, v) in bands.iter().enumerate() {
        let mut band = ds.rasterband(k + 1).unwrap();
        band.set_no_data_value(Some(-9999.0)).unwrap();
        band.write((0, 0), (w, h), &mut Buffer::new((w, h), v.clone())).unwrap();
    }
}

fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("janus-zeit-change-test-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

struct Fixture {
    dir: PathBuf,
    /// each date's bands [band][pixels]
    dates: Vec<Vec<f32>>,
    spec: JobSpec,
}

/// A series of `dates` dates in `dir`: a file a band holding every date, and the shown band.
fn fixture(name: &str, w: usize, h: usize, dates: usize, noise: f64) -> Fixture {
    let dir = folder(name);
    let cubes: Vec<Vec<f32>> = (0..dates).map(|t| date_cube(w, h, t, noise)).collect();
    let mut spec = JobSpec::default();
    for b in 0..NB {
        let bands: Vec<Vec<f32>> = cubes.iter().map(|c| c[b * w * h..(b + 1) * w * h].to_vec()).collect();
        let path = dir.join(format!("band{}.tif", b + 1)).to_string_lossy().to_string();
        write_tif(Path::new(&path), &bands, w, h);
        spec.bands.insert(format!("b{:03}", b + 1), path.clone());
        spec.band_names.insert(format!("b{:03}", b + 1), NAMES[b].to_string());
        if matches!(NAMES[b], "red" | "nir" | "swir1") {
            spec.bands.insert(NAMES[b].to_string(), path.clone());
            spec.band_names.insert(NAMES[b].to_string(), NAMES[b].to_string());
        }
        if NAMES[b] == "nir" {
            spec.input = path;
        }
    }
    let days: Vec<i64> = (0..dates as i64).map(|k| 18000 + 400 * k).collect();
    spec.years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
    spec.days = Some(days);
    spec.threads = Some(2);
    spec.output_dir = dir.join("out").to_string_lossy().to_string();
    Fixture { dir, dates: cubes, spec }
}

impl Fixture {
    fn run(&self, tool: &str, params: Value) -> JobResult {
        let mut spec = self.spec.clone();
        spec.tool = tool.into();
        spec.params = params.as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        let r = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| panic!("{tool}: {e}"));
        assert!(estimate(&spec).is_ok());
        r
    }

    fn out(&self, file: &str) -> PathBuf {
        self.dir.join("out").join(file)
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

fn output<'a>(r: &'a JobResult, id: &str) -> &'a Value {
    r.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"))
}

/// The pixels of the lattice sample (every `s`-th row and column) of a [band][y][x] cube.
fn lattice(cube: &[f32], w: usize, h: usize, s: usize) -> (Vec<Vec<f64>>, usize, usize) {
    let (lw, lh) = (w.div_ceil(s), h.div_ceil(s));
    let mut out = vec![vec![]; NB];
    for (b, o) in out.iter_mut().enumerate() {
        for y in (0..h).step_by(s) {
            for x in (0..w).step_by(s) {
                o.push(f64::from(cube[(b * h + y) * w + x]));
            }
        }
    }
    (out, lw, lh)
}

fn stride(w: usize, h: usize, sample: usize) -> usize {
    (((w * h) as f64 / sample as f64).sqrt().ceil() as usize).max(1)
}

#[test]
fn mnf_of_a_date_is_the_cores_transform_in_blocks() {
    // 1400 columns: the core's block of rows holds 187 of them, so the 300 rows are two blocks with a halo between
    let (w, h) = (1400, 300);
    let fx = fixture("mnf-blocks", w, h, 1, 0.01);
    let r = fx.run("mnf", json!({ "date": 0.0, "n_components": 3, "output": "both", "noise": "diff_hv" }));
    let opts = StatsOptions::new(Noise::DiffHv);
    let stats = mnf::mnf_stats_batch(&fx.dates[0], 1, NB, h, w, &opts, 2).unwrap();
    let tr = mnf::mnf_fit(&stats).unwrap();
    let mut y = vec![0f32; 3 * w * h];
    mnf::mnf_forward_batch(&fx.dates[0], NB, w * h, &tr, 3, Scale::Noise, &[], None, &mut y, 2);
    for k in 0..3 {
        let path = output(&r, &format!("mnf_{}", k + 1))["path"].as_str().unwrap().to_string();
        same(&read(Path::new(&path), 1), &y[k * w * h..(k + 1) * w * h], &format!("component {}", k + 1));
        // the multiband file holds them all
        same(&read(&fx.out("mnf_components.tif"), k + 1), &y[k * w * h..(k + 1) * w * h], "multiband");
    }
    // the table and the transform
    let csv = std::fs::read_to_string(fx.out("mnf_eigenvalues.csv")).unwrap();
    let rows: Vec<&str> = csv.lines().collect();
    assert_eq!(rows.len(), tr.m + 1);
    for (i, row) in rows[1..].iter().enumerate() {
        let lam: f64 = row.split(',').nth(1).unwrap().parse().unwrap();
        assert!((lam - tr.eigenvalues[i]).abs() <= 1e-12 * lam.abs(), "{row}");
    }
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("mnf_transform.json")).unwrap()).unwrap();
    assert_eq!(doc["format"], "zeit-mnf");
    assert_eq!(doc["eigenvalues"], json!(tr.eigenvalues));
    assert_eq!(doc["n_pixels"], json!(tr.n_pixels));
    assert_eq!(doc["n_pairs"]["h"], json!(tr.n_pairs[0]));
    assert!(tr.eigenvalues[0] > 3.0 && tr.eigenvalues[NB - 1] < 3.0, "{:?}", tr.eigenvalues);
    // the denoised bands, as a series of a file with the bands
    let den = output(&r, "denoised");
    assert_eq!(den["series"], true);
    let k = tr.n_with_snr(1.0);
    let mut d = vec![0f32; NB * w * h];
    mnf::mnf_denoise_batch(&fx.dates[0], NB, w * h, &tr, k, &[], None, &mut d, 2);
    let files: Vec<_> = std::fs::read_dir(den["path"].as_str().unwrap()).unwrap().collect();
    assert_eq!(files.len(), 1);
    let file = files[0].as_ref().unwrap().path();
    for b in 0..NB {
        same(&read(&file, b + 1), &d[b * w * h..(b + 1) * w * h], &format!("denoised band {b}"));
    }
    assert_eq!(Dataset::open(&file).unwrap().rasterband(4).unwrap().description().unwrap(), "nir");
}

#[test]
fn mnf_of_all_dates_pools_the_statistics_and_each_option_is_the_cores() {
    let (w, h) = (48, 40);
    let fx = fixture("mnf-all", w, h, T, 0.01);
    let r = fx.run(
        "mnf",
        json!({ "scope": "all_dates", "n_components": 2, "snr_min": 2.0, "output": "both",
                                  "noise": "diff_h", "scale": "unit", "sample_fraction": 0.5, "seed": 3 }),
    );
    let cube: Vec<f32> = fx.dates.concat();
    let mut opts = StatsOptions::new(Noise::DiffH);
    opts.sample_fraction = 0.5;
    opts.seed = 3;
    let stats = mnf::mnf_stats_batch(&cube, T, NB, h, w, &opts, 2).unwrap();
    let tr = mnf::mnf_fit(&stats).unwrap();
    let mut y = vec![0f32; T * 2 * w * h];
    mnf::mnf_forward_batch(&cube, NB, w * h, &tr, 2, Scale::Unit, &[], None, &mut y, 2);
    let o = output(&r, "mnf_2");
    assert_eq!(o["series"], true);
    for t in 0..T {
        let got = read(Path::new(o["path"].as_str().unwrap()), t + 1);
        let want: Vec<f32> = y[(t * 2 + 1) * w * h..(t * 2 + 2) * w * h].to_vec();
        same(&got, &want, &format!("component 2 of date {t}"));
    }
    // the denoised series has a file a date, the components with SNR above 2 kept
    let den = output(&r, "denoised");
    let files: Vec<_> = std::fs::read_dir(den["path"].as_str().unwrap()).unwrap().collect();
    assert_eq!(files.len(), T);
    let k = tr.n_with_snr(2.0);
    let mut d = vec![0f32; T * NB * w * h];
    mnf::mnf_denoise_batch(&cube, NB, w * h, &tr, k, &[], None, &mut d, 2);
    let mut names: Vec<PathBuf> = files.into_iter().map(|f| f.unwrap().path()).collect();
    names.sort();
    assert!(names[0].to_string_lossy().ends_with("2019-04-14.tif") || names[0].extension().unwrap() == "tif");
    for (t, f) in names.iter().enumerate() {
        same(&read(f, 2), &d[(t * NB + 1) * w * h..(t * NB + 2) * w * h], &format!("denoised date {t}"));
    }
    // a noise region and a subset of bands: the band is named, the region is a mask raster on the grid
    let mask: Vec<f32> = (0..w * h).map(|i| f32::from(i % w < 20 && i / w < 20)).collect();
    write_tif(&fx.dir.join("region.tif"), std::slice::from_ref(&mask), w, h);
    let r = fx.run(
        "mnf",
        json!({ "bands": "blue, 3, nir", "noise_region": fx.dir.join("region.tif").to_string_lossy(),
                                  "date": 0.0, "n_components": 0 }),
    );
    let used = [0usize, 2, 3];
    let sub: Vec<f32> = used.iter().flat_map(|&b| fx.dates[2][b * w * h..(b + 1) * w * h].to_vec()).collect();
    let region: Vec<u8> = mask.iter().map(|&v| v as u8).collect();
    let mut opts = StatsOptions::new(Noise::DiffHv);
    opts.noise_region = Some(&region);
    let stats = mnf::mnf_stats_batch(&sub, 1, 3, h, w, &opts, 2).unwrap();
    let tr = mnf::mnf_fit(&stats).unwrap();
    assert_eq!(tr.m, 3);
    let mut y = vec![0f32; 3 * w * h];
    mnf::mnf_forward_batch(&sub, 3, w * h, &tr, 3, Scale::Noise, &[], None, &mut y, 2);
    same(
        &read(Path::new(output(&r, "mnf_3")["path"].as_str().unwrap()), 1),
        &y[2 * w * h..],
        "component 3 in a region",
    );
}

#[test]
fn cva_is_the_cores_vector_threshold_and_sectors() {
    let (w, h) = (48, 40);
    let fx = fixture("cva", w, h, T, 0.01);
    for (threshold, sample, scale) in [("otsu", 400usize, "none"), ("em", 1_000_000, "std")] {
        let r = fx.run("cva", json!({ "threshold": threshold, "sample": sample, "scale": scale, "direction": "auto" }));
        let (x1, x2) = (&fx.dates[0], &fx.dates[2]);
        let s = stride(w, h, sample);
        let (l1, lw, lh) = lattice(x1, w, h, s);
        let (l2, _, _) = lattice(x2, w, h, s);
        // the valid pixels of the sample, as the core's own sample takes them (it takes the whole lattice image)
        let valid: Vec<usize> = (0..lw * lh).filter(|&i| l1.iter().chain(&l2).all(|b| b[i].is_finite())).collect();
        let pick = |l: &[Vec<f64>]| -> Vec<f64> { l.iter().flat_map(|b| valid.iter().map(|&i| b[i])).collect() };
        let use_bands: Vec<usize> = (0..NB).collect();
        let (rho, scales) =
            cva::cva_sample(&pick(&l1), &pick(&l2), NB, valid.len(), 1, &use_bands, scale == "std", 1, &[]);
        let t = if threshold == "em" {
            cva::em_threshold(&rho, 0.2, 500, 1e-9).unwrap().0
        } else {
            let up = cva::percentile(&rho, 0.999);
            cva::otsu(&rho, 1024, up).0
        };
        let direction = Direction::Sign { red: 2, nir: 3, swir1: 4 };
        let o = CvaOptions { bands: &use_bands, scale: &scales, direction, threshold: t, c2va_edges: None };
        let n = w * h;
        let (mut mag, mut dir, mut chg, mut sec) = (vec![0f32; n], vec![0f32; n], vec![0f32; n], vec![0f32; n]);
        cva::cva_batch(x1, x2, NB, n, &o, &[], &mut mag, &mut dir, &mut chg, &mut sec, 2);
        same(&read(Path::new(output(&r, "magnitude")["path"].as_str().unwrap()), 1), &mag, "magnitude");
        let plus1 = |a: &[f32]| -> Vec<f32> { a.iter().map(|v| v + 1.0).collect() };
        same(&read(Path::new(output(&r, "change")["path"].as_str().unwrap()), 1), &plus1(&chg), "change");
        let sector = output(&r, "sector");
        same(&read(Path::new(sector["path"].as_str().unwrap()), 1), &plus1(&sec), "sector");
        assert_eq!(sector["classes"].as_array().unwrap().len(), 9);
        assert_eq!(sector["classes"][0], "No change");
        // the sign direction writes its sectors, not an angle
        assert!(r.outputs.iter().all(|o| o["id"] != "direction"));
        // the changed block is flagged as changed, and in a sector of the NIR going up and red going down
        let changed = read(Path::new(output(&r, "change")["path"].as_str().unwrap()), 1);
        assert!(changed[10 * w + 12] == 2.0, "{threshold}: the block is not flagged");
        assert!(changed[30 * w + 40] == 1.0, "{threshold}: the quiet corner is flagged");
        // the record and the table
        let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("cva.json")).unwrap()).unwrap();
        assert!((rec["threshold"]["threshold"].as_f64().unwrap() - t).abs() <= 1e-12 * t);
        assert_eq!(rec["threshold"]["method"], threshold);
        let table = std::fs::read_to_string(fx.out("cva_sectors.csv")).unwrap();
        assert!(
            table.starts_with(
                "sector,label,pixels,area_ha,mean_magnitude
0,No change,"
            ),
            "{table}"
        );
        let sectors = rec["sectors"].as_array().unwrap();
        assert_eq!(sectors.len(), 9);
        assert_eq!(sectors.iter().map(|s| s["pixels"].as_u64().unwrap()).sum::<u64>() as usize, n - 1);
        // the area in hectares: 30 m pixels
        let first = &sectors[0];
        let area = first["area_ha"].as_f64().unwrap();
        assert!((area - first["pixels"].as_f64().unwrap() * 0.09).abs() < 1e-6, "{first}");
    }
    // the angle to the diagonal, with bands chosen
    let r = fx.run("cva", json!({ "direction": "c2va", "bands": "1,2,4", "threshold": "fixed", "value": 0.5 }));
    assert!(r.outputs.iter().any(|o| o["id"] == "direction") && r.outputs.iter().all(|o| o["id"] != "sector"));
    let mut sub = fx.dates[0][..w * h].to_vec();
    sub.extend_from_slice(&fx.dates[0][w * h..2 * w * h]);
    sub.extend_from_slice(&fx.dates[0][3 * w * h..4 * w * h]);
    let mut sub2 = fx.dates[2][..w * h].to_vec();
    sub2.extend_from_slice(&fx.dates[2][w * h..2 * w * h]);
    sub2.extend_from_slice(&fx.dates[2][3 * w * h..4 * w * h]);
    let o = CvaOptions {
        bands: &[0, 1, 2],
        scale: &[1.0; 3],
        direction: Direction::C2va,
        threshold: 0.5,
        c2va_edges: None,
    };
    let n = w * h;
    let (mut mag, mut dir, mut chg, mut sec) = (vec![0f32; n], vec![0f32; n], vec![0f32; n], vec![0f32; n]);
    cva::cva_batch(&sub, &sub2, 3, n, &o, &[], &mut mag, &mut dir, &mut chg, &mut sec, 2);
    same(&read(Path::new(output(&r, "direction")["path"].as_str().unwrap()), 1), &dir, "direction");
}

#[test]
fn imad_is_the_cores_iterations_and_its_maps() {
    let (w, h) = (48, 40);
    let fx = fixture("imad", w, h, T, 0.01);
    let r = fx.run("imad", json!({ "sample": 600, "n_mad": 2, "alpha": 0.05 }));
    let s = stride(w, h, 600);
    let (l1, _, _) = lattice(&fx.dates[0], w, h, s);
    let (l2, _, _) = lattice(&fx.dates[2], w, h, s);
    let mut z = vec![];
    for i in 0..l1[0].len() {
        if l1.iter().chain(&l2).all(|b| b[i].is_finite()) {
            z.extend((0..NB).map(|b| l1[b][i]));
            z.extend((0..NB).map(|b| l2[b][i]));
        }
    }
    let opts = ImadOptions { max_iter: 50, tol: 1e-3, lam: 0.0, penalty: imad::Penalty::Ridge, weighted_sd: true };
    let fit = imad::imad_fit(&z, NB, &opts, 2).unwrap();
    let n = w * h;
    let (mut mad, mut chi2, mut pnc) = (vec![0f32; NB * n], vec![0f32; n], vec![0f32; n]);
    imad::imad_apply(&fx.dates[0], &fx.dates[2], NB, n, &fit.transform, &[], &mut mad, &mut chi2, &mut pnc, None, 2);
    for k in 0..2 {
        let o = output(&r, &format!("mad_{}", k + 1));
        same(&read(Path::new(o["path"].as_str().unwrap()), 1), &mad[k * n..(k + 1) * n], "mad");
        // every variate is in the multiband file, MAD 1 first
        same(&read(&fx.out("imad.tif"), k + 1), &mad[k * n..(k + 1) * n], "multiband mad");
    }
    assert!(r.outputs.iter().all(|o| o["id"] != "mad_3"));
    same(&read(Path::new(output(&r, "chi2")["path"].as_str().unwrap()), 1), &chi2, "chi2");
    same(&read(Path::new(output(&r, "p_nochange")["path"].as_str().unwrap()), 1), &pnc, "p_nochange");
    let change: Vec<f32> =
        pnc.iter().map(|&p| if p.is_nan() { f32::NAN } else { f32::from(u8::from(p < 0.05)) + 1.0 }).collect();
    same(&read(Path::new(output(&r, "change")["path"].as_str().unwrap()), 1), &change, "change");
    same(&read(&fx.out("imad.tif"), NB + 3), &change.iter().map(|v| v - 1.0).collect::<Vec<_>>(), "change band");
    assert_eq!(change[10 * w + 12], 2.0, "the changed block");
    // the iterations' table and the record
    let csv = std::fs::read_to_string(fx.out("imad_rho.csv")).unwrap();
    assert_eq!(csv.lines().count(), fit.iterations + 1);
    assert!(csv.lines().next().unwrap().starts_with("iteration,rho_1,rho_2"));
    let last: Vec<f64> = csv.lines().last().unwrap().split(',').skip(1).take(NB).map(|v| v.parse().unwrap()).collect();
    for (a, b) in last.iter().zip(&fit.cca.rho) {
        assert!((a - b).abs() < 1e-12);
    }
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("imad.json")).unwrap()).unwrap();
    assert_eq!(rec["converged"], fit.converged);
    assert_eq!(rec["n_stats_pixels"], json!(fit.n_pixels));
    assert_eq!(rec["stride"], json!(s));
    assert_eq!(rec["a"], json!(fit.transform.a));
    // the same dates swapped: a gain and an offset leave the canonical correlations alone
    let two = fx.run("imad", json!({ "before": 0.0, "after": fx.spec.years[1], "max_iter": 1 }));
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("imad.json")).unwrap()).unwrap();
    assert_eq!(rec["iterations"], 1);
    // two latent fields: two canonical correlations near 1, the others are noise
    let rho: Vec<f64> = rec["rho_final"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect();
    assert!(rho[0] > 0.99 && rho[1] > 0.99 && rho[2] < 0.5, "{rho:?}");
    assert!(two.outputs.len() >= 3);
}

#[test]
fn the_normalization_is_the_cores_fit_applied_to_every_pixel() {
    let (w, h) = (48, 40);
    let fx = fixture("radcal", w, h, T, 0.005);
    let r = fx.run("radcal", json!({ "reference": 0.0, "threshold": 0.5, "sample": 0 }));
    let o = output(&r, "normalized");
    assert_eq!(o["series"], true);
    let dir = PathBuf::from(o["path"].as_str().unwrap());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "tif"))
        .collect();
    files.sort();
    assert_eq!(files.len(), T);
    // the reference is copied
    for b in 0..NB {
        same(&read(&files[0], b + 1), &fx.dates[0][b * w * h..(b + 1) * w * h], "the reference");
    }
    // the second date: iMAD against the reference on every pixel, the fit, a + b x
    let n = w * h;
    let mut z = vec![];
    for i in 0..n {
        let ok = (0..NB).all(|b| fx.dates[0][b * n + i].is_finite() && fx.dates[1][b * n + i].is_finite());
        if ok {
            z.extend((0..NB).map(|b| f64::from(fx.dates[0][b * n + i])));
            z.extend((0..NB).map(|b| f64::from(fx.dates[1][b * n + i])));
        }
    }
    let opts = ImadOptions { max_iter: 50, tol: 1e-3, lam: 0.0, penalty: imad::Penalty::Ridge, weighted_sd: true };
    let fit = imad::imad_fit(&z, NB, &opts, 2).unwrap();
    let (rows, invariant) = imad::radcal_fit(&z, NB, &fit.transform, 0.5, 3, 2).unwrap();
    assert!(invariant > 30);
    for (b, row) in rows.iter().enumerate() {
        let got = read(&files[1], b + 1);
        let want: Vec<f32> =
            fx.dates[1][b * n..(b + 1) * n].iter().map(|&v| (row.fit.a + row.fit.b * f64::from(v)) as f32).collect();
        same(&got, &want, &format!("band {b} of date 2"));
        // the gain undoes the date's: close to 1 / 1.25
        assert!((row.fit.b - 0.8).abs() < 0.05, "gain {}", row.fit.b);
    }
    // the table: a row a date and band, the gain as the core's
    let csv = std::fs::read_to_string(fx.out("radcal_fits.csv")).unwrap();
    assert_eq!(csv.lines().count(), 1 + (T - 1) * NB);
    let first: Vec<&str> = csv.lines().nth(1).unwrap().split(',').collect();
    assert_eq!(first[1], "blue");
    assert!((first[4].parse::<f64>().unwrap() - rows[0].fit.b).abs() < 1e-12);
    let rec: Value = serde_json::from_str(&std::fs::read_to_string(fx.out("radcal.json")).unwrap()).unwrap();
    assert_eq!(rec["imad"].as_array().unwrap().len(), T - 1);
    // only one date, the others copied
    let r = fx.run("radcal", json!({ "target": fx.spec.years[1], "threshold": 0.5 }));
    let dir = PathBuf::from(output(&r, "normalized")["path"].as_str().unwrap());
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|f| f.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "tif"))
        .collect();
    files.sort();
    same(&read(&files[2], 1), &fx.dates[2][..n], "a date not chosen is copied");
    // a date of the reference's own: refused
    let mut spec = fx.spec.clone();
    spec.tool = "radcal".into();
    spec.params = BTreeMap::from([("target".to_string(), json!(spec.years[0])), ("reference".to_string(), json!(0.0))]);
    assert!(fail(&spec).contains("reference"));
}

#[test]
fn two_dates_and_bands_are_checked() {
    let (w, h) = (48, 40);
    let fx = fixture("errors", w, h, 2, 0.01);
    let mut spec = fx.spec.clone();
    spec.tool = "cva".into();
    spec.params =
        BTreeMap::from([("before".to_string(), json!(spec.years[1])), ("after".to_string(), json!(spec.years[1]))]);
    let e = fail(&spec);
    assert!(e.contains("two dates"), "{e}");
    spec.params = BTreeMap::from([("bands".to_string(), json!("blue, nothing"))]);
    let e = fail(&spec);
    assert!(e.contains("nothing"), "{e}");
    spec.tool = "mnf".into();
    spec.params = BTreeMap::from([("bands".to_string(), json!("blue"))]);
    let e = fail(&spec);
    assert!(e.contains("2 bands"), "{e}");
    // the sign sectors need the roles
    spec.tool = "cva".into();
    spec.params =
        BTreeMap::from([("direction".to_string(), json!("sign")), ("bands".to_string(), json!("blue, green"))]);
    let e = fail(&spec);
    assert!(e.contains("red, NIR and SWIR1"), "{e}");
}

fn fail(spec: &JobSpec) -> String {
    match run_job(spec, &|_, _| {}, &AtomicBool::new(false)) {
        Err(e) => e,
        Ok(_) => panic!("{}: it should have failed", spec.tool),
    }
}
