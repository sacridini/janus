//! ImproPhe, a tool after the bridge: a raster run gives zeit.improphe's bands (tests/data/improphe, written by
//! tools/improphe_fixtures.py: a fine series of 60 x 60 pixels of 10 m, 20 dates in three bands with clouds, and two
//! coarse series of 30 m on other grids: a field with a layer a year in two fields (one NoData value, one NaN) and
//! four scenes of a sensor in two bands), on the whole window and on windows (the fine series cut to the window, as
//! Zeit is given it); the errors a run refuses with.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::{Dataset, DriverManager, Metadata};
use janus_zeit::job::{JobResult, JobSpec, OtherSeries, estimate, run_job};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    name: String,
    coarse: String,
    params: serde_json::Map<String, Value>,
    window: [usize; 4],
    expected: String,
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/improphe")
}

fn abs(name: &str) -> String {
    data().join(name).to_string_lossy().to_string()
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("improphe.json")).unwrap()).unwrap()
}

/// The coarse series of the fixtures ("cf" or "l2") with its files' paths made absolute.
fn other(which: &str) -> OtherSeries {
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(data().join("spec.json")).unwrap()).unwrap();
    let mut o: OtherSeries = serde_json::from_value(raw[format!("other_{which}")].clone()).unwrap();
    o.inputs = o.inputs.iter().map(|p| abs(p)).collect();
    o
}

/// The fine series' spec: every band of each date mapped as b001..b003.
fn spec(coarse: Option<&str>, params: &serde_json::Map<String, Value>, window: [usize; 4]) -> JobSpec {
    let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(data().join("spec.json")).unwrap()).unwrap();
    spec.tool = "improphe".into();
    spec.input = abs("input.tif");
    for b in 1..=3 {
        spec.bands.insert(format!("b{b:03}"), abs(&format!("fine_b{b}.tif")));
    }
    spec.params = params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    spec.window = Some(window);
    spec.other = coarse.map(other);
    spec
}

/// A raster's bands [band][pixels], their descriptions, its geotransform and size.
type Raster = (Vec<Vec<f64>>, Vec<String>, [f64; 6], (usize, usize));

/// A raster's bands as [band][pixels] (NoData as NaN), their descriptions, its geotransform and size.
fn read(path: &str) -> Raster {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let mut descriptions = vec![];
    let bands = (1..=ds.raster_count())
        .map(|b| {
            let band = ds.rasterband(b).unwrap();
            descriptions.push(band.description().unwrap());
            let nd = band.no_data_value();
            let v = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
            v.data()
                .iter()
                .map(|&x| if nd.is_some_and(|d| d == x || d.is_nan() && x.is_nan()) { f64::NAN } else { x })
                .collect()
        })
        .collect();
    (bands, descriptions, ds.geo_transform().unwrap(), (w, h))
}

fn run(spec: &mut JobSpec, name: &str) -> Result<JobResult, String> {
    let out = std::env::temp_dir().join(format!("janus-zeit-improphe-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(spec, &|_, _| {}, &AtomicBool::new(false))
}

fn fails(spec: &mut JobSpec, name: &str) -> String {
    match run(spec, name) {
        Ok(_) => panic!("the run of {name} was accepted"),
        Err(e) => e,
    }
}

#[test]
fn raster_runs_are_zeits() {
    let f = fixtures();
    assert!(f.cases.len() >= 8);
    let (mut values, mut worst) = (0, 0.0f64);
    for c in &f.cases {
        let mut s = spec(Some(&c.coarse), &c.params, c.window);
        let result = run(&mut s, &c.name).unwrap();
        assert_eq!(result.window, c.window, "{}", c.name);
        assert_eq!(result.outputs.len(), 1);
        let o = &result.outputs[0];
        assert_eq!((o["id"].as_str(), o["series"].as_bool()), (Some("improphe"), Some(true)), "{}", c.name);
        let (got, labels, gt, size) = read(o["path"].as_str().unwrap());
        let (want, _, want_gt, want_size) = read(&abs(&c.expected));
        // on the fine grid, from the window's corner
        assert_eq!(size, want_size, "{}", c.name);
        for (a, b) in gt.iter().zip(&want_gt) {
            assert!((a - b).abs() < 1e-9, "{}: {gt:?} vs {want_gt:?}", c.name);
        }
        assert_eq!(got.len(), want.len(), "{}", c.name);
        // the coarse series' dates and, with two fields, its bands' names
        let coarse = other(&c.coarse);
        assert_eq!(labels.len(), coarse.years.len() * coarse.inputs.len());
        assert!(labels[0].starts_with(&coarse.labels.as_ref().unwrap()[0]), "{labels:?}");
        assert!(labels[1].ends_with(&coarse.band_names[1]), "{labels:?}");
        let (mut present, mut cw) = (0, 0.0f64);
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g.len(), w.len());
            for (i, (&g, &w)) in g.iter().zip(w).enumerate() {
                if w.is_nan() {
                    assert!(g.is_nan(), "{} band {k} pixel {i}: {g} vs missing", c.name);
                } else {
                    let rel = (g - w).abs() / w.abs().max(1e-12);
                    assert!(
                        g.is_finite() && (g - w).abs() <= 1e-5 * w.abs() + 1e-12,
                        "{} band {k} pixel {i}: {g} vs {w}",
                        c.name
                    );
                    present += 1;
                    cw = cw.max(rel);
                }
            }
        }
        assert!(present > 1000, "{}: {present} values", c.name);
        values += present;
        worst = worst.max(cw);
    }
    println!("{values} values compared with zeit.improphe, the largest relative difference {worst:.2e}");
}

/// The same run with the cases' parameters but a quality band: the date it refuses is a date NaN in the fine series.
#[test]
fn the_quality_band_leaves_a_date_out() {
    let f = fixtures();
    let c = f.cases.iter().find(|c| c.name == "l2_default").unwrap();
    let dir = std::env::temp_dir().join("janus-zeit-improphe-test-qa");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let t = 20;
    // a copy of the fine bands with date 7 NaN, and a quality band that refuses date 7 (the rule "nonzero")
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let copy = |src: &str, dst: &Path, bad: Option<f32>| {
        let s = Dataset::open(src).unwrap();
        let (w, h) = s.raster_size();
        let mut d = drv.create_with_band_type::<f32, _>(dst, w, h, t).unwrap();
        d.set_geo_transform(&s.geo_transform().unwrap()).unwrap();
        d.set_projection(&s.projection()).unwrap();
        for b in 1..=t {
            let mut v = s.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
            if let Some(bad) = bad
                && b == 8
            {
                v.data_mut().fill(bad);
            }
            let mut band = d.rasterband(b).unwrap();
            band.set_no_data_value(Some(f64::NAN)).unwrap();
            band.write((0, 0), (w, h), &mut v).unwrap();
        }
    };
    let mut masked = spec(Some("l2"), &c.params, c.window);
    for b in 1..=3 {
        let to = dir.join(format!("fine_b{b}.tif"));
        copy(&abs(&format!("fine_b{b}.tif")), &to, Some(f32::NAN));
        masked.bands.insert(format!("b{b:03}"), to.to_string_lossy().into_owned());
    }
    let with_qa = {
        let mut s = spec(Some("l2"), &c.params, c.window);
        let qa = dir.join("qa.tif");
        let src = Dataset::open(abs("input.tif")).unwrap();
        let (w, h) = src.raster_size();
        let d = drv.create_with_band_type::<f32, _>(&qa, w, h, t).unwrap();
        for b in 1..=t {
            let v: Vec<f32> = vec![if b == 8 { 0.0 } else { 1.0 }; w * h];
            d.rasterband(b).unwrap().write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), v)).unwrap();
        }
        s.qa_input = Some(qa.to_string_lossy().into_owned());
        s.qa_rule = Some("nonzero".into());
        s
    };
    let a = run(&mut masked, "qa-nan").unwrap();
    let mut with_qa = with_qa;
    let b = run(&mut with_qa, "qa-band").unwrap();
    let (a, b) = (read(a.outputs[0]["path"].as_str().unwrap()).0, read(b.outputs[0]["path"].as_str().unwrap()).0);
    for (x, y) in a.iter().flatten().zip(b.iter().flatten()) {
        assert!(x.to_bits() == y.to_bits() || (x.is_nan() && y.is_nan()), "{x} vs {y}");
    }
    // and the date does matter
    let mut plain = spec(Some("l2"), &c.params, c.window);
    let p = read(run(&mut plain, "qa-none").unwrap().outputs[0]["path"].as_str().unwrap()).0;
    assert!(p.iter().flatten().zip(a.iter().flatten()).any(|(x, y)| x.is_finite() && x != y));
}

#[test]
fn the_runs_refusals() {
    let f = fixtures();
    let case = |name: &str| f.cases.iter().find(|c| c.name == name).unwrap();
    let cf = case("cf_default");
    let l2 = case("l2_default");

    // no coarse series
    let mut s = spec(None, &cf.params, cf.window);
    let e = fails(&mut s, "no-coarse");
    assert!(e.contains("choose the coarse series"), "{e}");

    // another CRS (the coarse series' file again, in UTM 23S)
    let dir = std::env::temp_dir().join("janus-zeit-improphe-test-other-crs");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut s = spec(Some("cf"), &cf.params, cf.window);
    let o = s.other.as_mut().unwrap();
    for (k, p) in o.inputs.iter_mut().enumerate() {
        let src = Dataset::open(&*p).unwrap();
        let (w, h) = src.raster_size();
        let to = dir.join(format!("f{k}.tif"));
        let mut d = DriverManager::get_driver_by_name("GTiff")
            .unwrap()
            .create_with_band_type::<f32, _>(&to, w, h, src.raster_count())
            .unwrap();
        d.set_geo_transform(&src.geo_transform().unwrap()).unwrap();
        d.set_spatial_ref(&gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap()).unwrap();
        for b in 1..=src.raster_count() {
            d.rasterband(b).unwrap().fill(0.5, None).unwrap();
        }
        *p = to.to_string_lossy().into_owned();
    }
    let e = fails(&mut s, "crs");
    assert!(e.contains("different CRS") && e.contains("reproject"), "{e}");

    // a window over the memory cap (no value of it is read)
    let mut s = spec(Some("l2"), &l2.params, [0, 0, 60_000, 60_000]);
    let e = fails(&mut s, "cap");
    assert!(e.contains("GB of memory") && e.contains("choose the ROI"), "{e}");

    // the layout and the parameters
    let mut p = l2.params.clone();
    p.insert("by".into(), json!("year"));
    let mut s = spec(Some("l2"), &p, l2.window);
    // the scenes of 2020 are two: not a layer a year
    let e = fails(&mut s, "by-year");
    assert!(e.contains("different years"), "{e}");
    let mut p = cf.params.clone();
    p.insert("by".into(), json!("auto"));
    let mut s = spec(Some("cf"), &p, cf.window);
    let e = fails(&mut s, "by-auto");
    assert!(e.contains("one date a year"), "{e}");
    let mut p = cf.params.clone();
    p.insert("kernel_size".into(), json!(5.0));
    let e = fails(&mut spec(Some("cf"), &p, cf.window), "kernel");
    assert!(e.contains("kernel_size") && e.contains("at least 1 pixel"), "{e}");
    let mut p = cf.params.clone();
    p.insert("seasons".into(), json!("1,500"));
    let e = fails(&mut spec(Some("cf"), &p, cf.window), "seasons");
    assert!(e.contains("seasons"), "{e}");
    let mut p = cf.params.clone();
    p.insert("pc_variance".into(), json!(0.0));
    let e = fails(&mut spec(Some("cf"), &p, cf.window), "variance");
    assert!(e.contains("pc_variance"), "{e}");

    // a series without dates
    let mut s = spec(Some("cf"), &cf.params, cf.window);
    s.days = None;
    let e = fails(&mut s, "no-dates");
    assert!(e.contains("real dates"), "{e}");
}

/// Copy Python's call: the run's parameters, the cubes the script makes.
#[test]
fn the_python_call_has_the_parameters() {
    let f = fixtures();
    let c = f.cases.iter().find(|c| c.name == "cf_crossing").unwrap();
    let given = c.params.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    let p = janus_zeit::Params::with_defaults("improphe", &given).unwrap();
    let (code, inputs) = janus_zeit::python::calls("improphe", &p, &Default::default()).unwrap();
    assert_eq!(inputs, vec!["improphe_cubes"]);
    assert_eq!(
        code,
        "result1 = zeit.improphe(coarse_cube, fine_cube, by='year', seasons=(330, 60, 330), kernel_size=60.0, \
         texture_size=20.0, pc_variance=0.9, force=False, pca=None, nodata=None, n_jobs=1)"
    );
    let mut given: std::collections::BTreeMap<String, Value> = std::collections::BTreeMap::new();
    given.insert("by".into(), json!("scenes"));
    given.insert("pca".into(), json!("force"));
    let p = janus_zeit::Params::with_defaults("improphe", &given).unwrap();
    let code = janus_zeit::python::calls("improphe", &p, &Default::default()).unwrap().0;
    assert!(code.contains("by=None, seasons=(1, 91, 181, 271, 365), kernel_size=2500.0, texture_size=330.0"), "{code}");
    assert!(code.contains("pc_variance=0.975, force=False, pca='force'"), "{code}");
}

#[test]
fn a_cancelled_run_stops() {
    let f = fixtures();
    let c = &f.cases[0];
    let mut s = spec(Some(&c.coarse), &c.params, c.window);
    s.output_dir = std::env::temp_dir().join("janus-zeit-improphe-test-cancel").to_string_lossy().into_owned();
    let e = run_job(&s, &|_, _| {}, &AtomicBool::new(true)).err().unwrap();
    assert_eq!(e, "cancelled");
}

#[test]
fn the_estimate_grows_with_the_kernel_and_the_layers() {
    let f = fixtures();
    let c = f.cases.iter().find(|c| c.name == "cf_default").unwrap();
    let small = estimate(&spec(Some("cf"), &c.params, c.window)).unwrap();
    let mut big = c.params.clone();
    big.insert("kernel_size".into(), json!(250.0));
    let big = estimate(&spec(Some("cf"), &big, c.window)).unwrap();
    assert!(small.sec_per_px > 0.0 && big.sec_per_px > 2.0 * small.sec_per_px, "{small:?} {big:?}");
    // without a coarse series picked yet the form still gets a figure
    assert!(estimate(&spec(None, &c.params, c.window)).unwrap().sec_per_px > 0.0);
}

/// A fine series of `n` x `n` pixels (the fixtures' 20 dates, three bands) and a coarse one of `nl` scenes in `nf`
/// bands (30 m), of smooth values with noise and gaps, for the timing.
fn synthetic(dir: &Path, n: usize, nl: usize, nf: usize) -> JobSpec {
    let _ = std::fs::create_dir_all(dir);
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut rng = 12345u64;
    let mut next = move || {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (rng >> 40) as f32 / (1u64 << 24) as f32
    };
    let mut make = |name: &str, w: usize, count: usize, res: f64, gap: f32| -> String {
        let path = dir.join(name);
        let mut d = drv.create_with_band_type::<f32, _>(&path, w, w, count).unwrap();
        d.set_geo_transform(&[500000.0, res, 0.0, 8900000.0, 0.0, -res]).unwrap();
        d.set_spatial_ref(&gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap()).unwrap();
        for b in 1..=count {
            let v: Vec<f32> = (0..w * w)
                .map(|i| {
                    let (x, y) = ((i % w) as f32, (i / w) as f32);
                    if next() < gap {
                        f32::NAN
                    } else {
                        1000.0 + 500.0 * ((x / 17.0).sin() + (y / 23.0 + b as f32).cos()) + 50.0 * next()
                    }
                })
                .collect();
            let mut band = d.rasterband(b).unwrap();
            band.set_no_data_value(Some(f64::NAN)).unwrap();
            band.write((0, 0), (w, w), &mut gdal::raster::Buffer::new((w, w), v)).unwrap();
        }
        path.to_string_lossy().into_owned()
    };
    let mut s = spec(None, &serde_json::Map::new(), [0, 0, n, n]);
    s.input = make("fine_shown.tif", n, 20, 10.0, 0.1);
    for b in 1..=3 {
        s.bands.insert(format!("b{b:03}"), make(&format!("fine_b{b}.tif"), n, 20, 10.0, 0.1));
    }
    let mut o = other("l2");
    o.years.truncate(nl);
    o.days.as_mut().unwrap().truncate(nl);
    o.labels.as_mut().unwrap().truncate(nl);
    o.inputs = (0..nf).map(|f| make(&format!("coarse_{f}.tif"), n / 3, nl, 30.0, 0.02)).collect();
    s.other = Some(o);
    s.threads = Some(4);
    s
}

/// The constants of the estimate (`cargo test --release -p janus-zeit --test improphe measure -- --ignored
/// --nocapture`): the time a pixel takes (four threads) in runs on 300 x 300 pixels of three radii, one and two
/// bands of the coarse series, one and four scenes. Not a check.
#[test]
#[ignore]
fn measure_the_constants() {
    let dir = std::env::temp_dir().join("janus-zeit-improphe-measure");
    let n = 300;
    for (nl, nf) in [(1, 1), (4, 1), (4, 2)] {
        let mut s = synthetic(&dir.join(format!("{nl}_{nf}")), n, nl, nf);
        for kernel in [100.0, 200.0, 400.0] {
            s.params.insert("kernel_size".into(), json!(kernel));
            s.params.insert("by".into(), json!("scenes"));
            s.params.insert("texture_size".into(), json!(30.0));
            let t0 = std::time::Instant::now();
            run(&mut s, "measure").unwrap();
            let sec = t0.elapsed().as_secs_f64();
            let est = estimate(&s).unwrap();
            println!(
                "{nl} scene(s) x {nf} band(s), kernel {kernel}: {sec:.3} s for {} px = {:.3e} s/px; the estimate {:.3e} s/px",
                n * n,
                sec / (n * n) as f64,
                est.sec_per_px
            );
        }
    }
}
