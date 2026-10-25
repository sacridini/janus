//! BEAST (a tool after the bridge): a raster run gives zeit.beast's maps, bit for bit (tests/data/beast.json, written
//! by tools/beast_fixtures.py), and a pixel run its rows and lines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    maps: BTreeMap<String, Vec<Option<f64>>>,
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data")
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("beast.json")).unwrap()).unwrap()
}

/// A raster's bands as [band][pixels], NoData as NaN.
fn read(path: &Path) -> Vec<Vec<f64>> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    (1..=ds.raster_count())
        .map(|b| {
            let band = ds.rasterband(b).unwrap();
            let nd = band.no_data_value();
            let v = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
            v.data().iter().map(|&x| if nd.is_some_and(|d| d == x) { f64::NAN } else { x }).collect()
        })
        .collect()
}

fn spec() -> Value {
    serde_json::from_str(&std::fs::read_to_string(data().join("raster/ccdc-0/spec.json")).unwrap()).unwrap()
}

#[test]
fn raster_run_is_zeits_beast() {
    let case = data().join("raster/ccdc-0");
    for (k, c) in fixtures().cases.iter().enumerate() {
        // the CCDC case's series (its NDVI), without its bands
        let mut spec = spec();
        for key in ["bands", "qa_input", "qa_rule", "shown"] {
            spec.as_object_mut().unwrap().remove(key);
        }
        let mut spec: JobSpec = serde_json::from_value(spec).unwrap();
        spec.tool = "beast".into();
        spec.params = c.params.clone();
        spec.input = case.join(&spec.input).to_string_lossy().to_string();
        let out = std::env::temp_dir().join(format!("janus-zeit-beast-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        for (id, want) in &c.maps {
            let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap_or_else(|| panic!("no output {id}"));
            let got = read(Path::new(o["path"].as_str().unwrap())).remove(0);
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                match w {
                    Some(w) => assert_eq!(*g as f32, *w as f32, "case {k} {id} pixel {i}"),
                    // NaN, or an infinite R² (a series without variance on the grid): none in JSON
                    None => assert!(!g.is_finite(), "case {k} {id} pixel {i}: {g}"),
                }
            }
        }
    }
}

/// One pixel's NDVI series of the CCDC case.
fn pixel_input(px: usize) -> PixelInput {
    let s = spec();
    let values: Vec<f64> = read(&data().join("raster/ccdc-0/input.tif")).iter().map(|b| b[px]).collect();
    PixelInput {
        years: serde_json::from_value(s["years"].clone()).unwrap(),
        days: Some(serde_json::from_value(s["days"].clone()).unwrap()),
        values,
        ..Default::default()
    }
}

#[test]
fn pixel_run_rows_and_lines() {
    let f = fixtures();
    let c = &f.cases[0];
    let px = (0..c.maps["cp_prob"].len())
        .max_by(|&a, &b| c.maps["cp_prob"][a].unwrap_or(0.0).total_cmp(&c.maps["cp_prob"][b].unwrap_or(0.0)));
    let px = px.unwrap();
    let input = pixel_input(px);
    let p = Params::with_defaults("beast", &c.params).unwrap();
    let r = run_pixel("beast", &p, &input).unwrap();
    let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
    // the raster's numbers, the same pixel
    let near = |text: &str, want: f64, digits: i32| {
        let got: f64 = text.split_whitespace().next().unwrap().parse().unwrap();
        (got - want).abs() <= want.abs() * 10f64.powi(1 - digits)
    };
    assert!(near(rows["R²"], c.maps["r2"][px].unwrap(), 4), "{rows:?}");
    assert!(near(rows["Trend changepoints"], c.maps["trend_ncp"][px].unwrap(), 3), "{rows:?}");
    assert!(rows.contains_key("Season changepoints") && rows.contains_key("Slope at the end"), "{rows:?}");
    let labels: Vec<&str> = r.overlays.iter().map(|o| o.label.as_str()).collect();
    for l in ["BEAST fit (trend + season)", "BEAST trend", "Trend 95% interval (lower)", "Trend 95% interval (upper)"] {
        assert!(labels.contains(&l), "{l}: {labels:?}");
    }
    // the most probable changepoint is drawn when more likely than not
    if c.maps["cp_prob"][px].unwrap() >= 0.5 {
        let v = r.overlays.iter().find(|o| o.label.starts_with("BEAST trend changepoints")).unwrap();
        assert!(v.x.iter().any(|&x| x as f32 == c.maps["cp_date"][px].unwrap() as f32), "{:?}", v.x);
    }

    let (code, inputs) = calls("beast", &p, &input).unwrap();
    assert!(
        code.starts_with("result1 = zeit.beast(series, season='harmonic', deltat=None, tcp_minmax=(0, 10)"),
        "{code}"
    );
    assert!(code.contains("samples=300, chains=1, burnin=200, thin=5, seed=42"), "{code}");
    assert_eq!(inputs, vec!["series"]);

    // annual values without dates: no season by default, and a harmonic one refused
    let annual = PixelInput {
        years: (0..30).map(|i| 1990.0 + f64::from(i)).collect(),
        values: (0..30).map(|i| if i < 15 { 0.8 } else { 0.4 } + 0.01 * f64::from(i % 3)).collect(),
        ..Default::default()
    };
    let r = run_pixel("beast", &p, &annual).unwrap();
    let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
    assert!(!rows.contains_key("Season changepoints"), "{rows:?}");
    let v = r.overlays.iter().find(|o| o.label.starts_with("BEAST trend changepoints")).expect("the drop found");
    assert!(v.x.iter().any(|&x| (2003.0..=2006.0).contains(&x)), "{:?}", v.x);
    let harmonic =
        Params::with_defaults("beast", &BTreeMap::from([("season".to_string(), json!("harmonic"))])).unwrap();
    assert!(run_pixel("beast", &harmonic, &annual).unwrap_err().contains("needs the dates"));
}
