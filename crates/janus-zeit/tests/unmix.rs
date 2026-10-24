//! Spectral unmixing with endmembers of the image (tools after the bridge): the fractions and RMSE of zeit.unmix
//! called as Copy Python calls it, bit for bit (tests/data/unmix, written by tools/unmix_fixtures.py), the
//! outputs one per endmember, and a pixel run's rows and lines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{Chunk, JobCtx, JobSpec, run_chunk, run_job};
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    names: Vec<String>,
    spectra: Vec<Vec<f64>>,
    constraint: String,
    fraction: Vec<Vec<Option<f64>>>,
    rmse: Vec<Option<f64>>,
}

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/unmix")
}

fn cases() -> BTreeMap<String, Case> {
    serde_json::from_str(&std::fs::read_to_string(dir().join("expected.json")).unwrap()).unwrap()
}

/// A raster's bands as [band][pixels], and its width and height.
fn read(name: &str) -> (Vec<f64>, usize, usize, usize) {
    let ds = Dataset::open(dir().join(name)).unwrap();
    let (w, h) = ds.raster_size();
    let n = ds.raster_count();
    let mut out = Vec::with_capacity(n * w * h);
    for b in 1..=n {
        out.extend(ds.rasterband(b).unwrap().read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap().data());
    }
    (out, n, w, h)
}

fn params(tool: &str, c: &Case) -> Params {
    let ems: Vec<Value> =
        c.names.iter().zip(&c.spectra).map(|(n, s)| json!({"name": n, "from": "test", "values": s})).collect();
    let given =
        BTreeMap::from([("endmembers".to_string(), json!(ems)), ("constraint".to_string(), json!(c.constraint))]);
    Params::with_defaults(tool, &given).unwrap()
}

/// zeit's float32 against a value of the chunk.
fn same(got: f64, want: &Option<f64>) -> bool {
    match want {
        Some(w) => got as f32 == *w as f32,
        None => got.is_nan(),
    }
}

fn series_chunk() -> Chunk {
    let (stack, t, w, h) = read("series_shown.tif");
    let bands = (1..=6).map(|b| (format!("b{b:03}"), read(&format!("series_b{b:03}.tif")).0)).collect();
    Chunk { t, h, w, stack, bands, qa: None }
}

#[test]
fn series_fractions_are_zeits() {
    let chunk = series_chunk();
    for (name, c) in cases().iter().filter(|(k, _)| k.starts_with("series")) {
        let res = run_chunk("unmix", &params("unmix", c), &chunk, &JobCtx::default()).unwrap();
        for (j, want) in c.fraction.iter().enumerate() {
            let got = &res[&format!("fraction_{}", j + 1)];
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                assert!(same(*g, w), "{name} fraction {j} [{i}]: {g} vs {w:?}");
            }
        }
        // the mean RMSE over the dates of each pixel
        let n = chunk.pixels();
        for (px, got) in res["rmse"].iter().enumerate() {
            let v: Vec<f64> = (0..chunk.t).filter_map(|i| c.rmse[i * n + px]).collect();
            let want = v.iter().sum::<f64>() / v.len() as f64;
            assert!((got - want).abs() < 1e-6, "{name} rmse {px}");
        }
    }
}

#[test]
fn spectrum_fractions_are_zeits() {
    let (stack, t, w, h) = read("spectrum.tif");
    let chunk = Chunk { t, h, w, stack, ..Default::default() };
    for (name, c) in cases().iter().filter(|(k, _)| k.starts_with("spectrum")) {
        let res = run_chunk("unmix_spectrum", &params("unmix_spectrum", c), &chunk, &JobCtx::default()).unwrap();
        for (j, want) in c.fraction.iter().enumerate() {
            for (i, (g, w)) in res[&format!("fraction_{}", j + 1)].iter().zip(want).enumerate() {
                assert!(same(*g, w), "{name} fraction {j} [{i}]: {g} vs {w:?}");
            }
        }
        for (i, (g, w)) in res["rmse"].iter().zip(&c.rmse).enumerate() {
            assert!(same(*g, w), "{name} rmse [{i}]");
        }
    }
}

#[test]
fn raster_run_writes_one_output_per_endmember() {
    let all = cases();
    let c = &all["series_fcls"];
    let d = dir();
    let spec = JobSpec {
        tool: "unmix".into(),
        params: params("unmix", c).0,
        input: d.join("series_shown.tif").to_string_lossy().into(),
        bands: (1..=6)
            .map(|b| (format!("b{b:03}"), d.join(format!("series_b{b:03}.tif")).to_string_lossy().into()))
            .collect(),
        years: (0..5).map(|i| 2020.0 + f64::from(i)).collect(),
        output_dir: std::env::temp_dir().join("janus-zeit-unmix-test").to_string_lossy().into(),
        ..Default::default()
    };
    let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let names: Vec<&str> = result.outputs.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "vegetation fraction",
            "soil fraction",
            "water fraction",
            "Mean vegetation fraction",
            "Mean soil fraction",
            "Mean water fraction",
            "Mean RMSE",
            "Observations unmixed"
        ]
    );
    let series = Dataset::open(result.outputs[1]["path"].as_str().unwrap()).unwrap();
    assert_eq!(series.raster_count(), 5);
}

#[test]
fn empty_bands_are_left_out() {
    // a band without a value in the scene and the endmembers (EnMAP's 1331-1391 nm): the same fractions
    let (stack, t, w, h) = read("spectrum.tif");
    let n = w * h;
    let c = &cases()["spectrum_fcls"];
    let want = run_chunk(
        "unmix_spectrum",
        &params("unmix_spectrum", c),
        &Chunk { t, h, w, stack: stack.clone(), ..Default::default() },
        &JobCtx::default(),
    )
    .unwrap();
    let gap: Vec<f64> =
        stack[..2 * n].iter().copied().chain(vec![f64::NAN; n]).chain(stack[2 * n..].iter().copied()).collect();
    let ems: Vec<Value> = c
        .names
        .iter()
        .zip(&c.spectra)
        .map(|(name, s)| {
            let v: Vec<Value> =
                s[..2].iter().map(|&x| json!(x)).chain([Value::Null]).chain(s[2..].iter().map(|&x| json!(x))).collect();
            json!({"name": name, "values": v})
        })
        .collect();
    let p = Params::with_defaults(
        "unmix_spectrum",
        &BTreeMap::from([("endmembers".to_string(), json!(ems)), ("constraint".to_string(), json!(c.constraint))]),
    )
    .unwrap();
    let got = run_chunk(
        "unmix_spectrum",
        &p,
        &Chunk { t: t + 1, h, w, stack: gap, ..Default::default() },
        &JobCtx::default(),
    )
    .unwrap();
    for (k, v) in &want {
        assert!(got[k].iter().zip(v).all(|(a, b)| a == b || (a.is_nan() && b.is_nan())), "{k}");
    }
}

#[test]
fn pixel_runs() {
    let all = cases();
    // the series: a pixel's bands at every date, the fractions drawn over an index
    let chunk = series_chunk();
    let c = &all["series_fcls"];
    let p = params("unmix", c);
    let mut input = chunk
        .pixel_input(&PixelInput { years: (0..5).map(|i| 2020.5 + f64::from(i)).collect(), ..Default::default() }, 9);
    let r = run_pixel("unmix", &p, &input).unwrap();
    let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
    assert_eq!(rows["Observations unmixed"], "5 of 5", "{rows:?}");
    assert!(rows.contains_key("Mean soil fraction") && rows.contains_key("Sum of the fractions"), "{rows:?}");
    assert!(r.overlays.is_empty(), "the shown series is reflectance x 10000: no fractions on its chart");
    input.values = input.values.iter().map(|v| v / 10000.0).collect();
    assert_eq!(run_pixel("unmix", &p, &input).unwrap().overlays.len(), 3);
    // one endmember: asked for more
    let one = Params::with_defaults(
        "unmix",
        &BTreeMap::from([("endmembers".to_string(), json!([{"name": "a", "values": [1.0, 2.0]}]))]),
    )
    .unwrap();
    assert!(run_pixel("unmix", &one, &input).unwrap().rows[0][1].contains("at least two"));

    // the scene: a pixel's spectrum, rebuilt
    let (stack, t, w, h) = read("spectrum.tif");
    let scene = Chunk { t, h, w, stack, ..Default::default() };
    let c = &all["spectrum_fcls"];
    let nm: Vec<f64> = (0..t).map(|i| 450.0 + 67.0 * i as f64).collect();
    let input = scene.pixel_input(&PixelInput { years: nm, ..Default::default() }, 20);
    let r = run_pixel("unmix_spectrum", &params("unmix_spectrum", c), &input).unwrap();
    assert_eq!(r.overlays.len(), 4);
    assert_eq!(r.overlays[0].label, "Rebuilt spectrum");
    let rebuilt: Vec<f64> = r.overlays[0].y.iter().map(|v| v.unwrap()).collect();
    let err = rebuilt.iter().zip(&input.values).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / t as f64;
    let rmse: f64 = r.rows.iter().find(|r| r[0] == "RMSE").unwrap()[1].parse().unwrap();
    assert!((err.sqrt() - rmse).abs() < 1e-3, "{} vs {rmse}", err.sqrt());
    // endmembers of another layer
    let six = Params::with_defaults(
        "unmix_spectrum",
        &BTreeMap::from([(
            "endmembers".to_string(),
            json!([{"name": "a", "values": vec![1.0; 6]}, {"name": "b", "values": vec![0.5; 6]}]),
        )]),
    )
    .unwrap();
    assert!(run_pixel("unmix_spectrum", &six, &input).unwrap().rows[0][1].contains("take them from pins"));
}
