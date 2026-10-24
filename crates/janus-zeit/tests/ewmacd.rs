//! EWMACD (a tool after the bridge): a raster run gives zeit.ewmacd's maps, bit for bit (tests/data/ewmacd.json,
//! written by tools/ewmacd_fixtures.py), and a pixel run its rows and lines.

use std::collections::BTreeMap;
use std::path::Path;
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

/// A one-band raster's values, NoData as NaN.
fn read_band(path: &str) -> Vec<f64> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let band = ds.rasterband(1).unwrap();
    let nd = band.no_data_value();
    let data = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
    data.data().iter().map(|&v| if nd.is_some_and(|d| d == v) { f64::NAN } else { v }).collect()
}

#[test]
fn raster_run_is_zeits_ewmacd() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let fixtures: Fixtures = serde_json::from_str(&std::fs::read_to_string(data.join("ewmacd.json")).unwrap()).unwrap();
    let case = data.join("raster/ccdc-0");
    for (k, c) in fixtures.cases.iter().enumerate() {
        // the CCDC case's series (its NDVI), without its bands
        let mut spec: Value = serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
        for key in ["bands", "qa_input", "qa_rule", "shown"] {
            spec.as_object_mut().unwrap().remove(key);
        }
        let mut spec: JobSpec = serde_json::from_value(spec).unwrap();
        spec.tool = "ewmacd".into();
        spec.params = c.params.clone();
        spec.input = case.join(&spec.input).to_string_lossy().to_string();
        let out = std::env::temp_dir().join(format!("janus-zeit-ewmacd-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        for (id, want) in &c.maps {
            let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap_or_else(|| panic!("no output {id}"));
            let got = read_band(o["path"].as_str().unwrap());
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                match w {
                    // the maps are float32 files
                    Some(w) => assert_eq!(*g as f32, *w as f32, "case {k} {id} pixel {i}"),
                    None => assert!(g.is_nan(), "case {k} {id} pixel {i}: {g}"),
                }
            }
        }
    }
}

/// An NDVI-like series of 14 irregular dates a year, 2014-2021, a thinning of 0.12 from mid 2018 to 2020, a cloud in
/// 2015.
fn thinning() -> PixelInput {
    let (mut years, mut days, mut values) = (vec![], vec![], vec![]);
    for y in 2014..2022i64 {
        for k in 0..14i64 {
            let doy = 3 + k * 26 + (k * 7 + y) % 9;
            let d = (y - 1970) * 365 + (y - 1969) / 4 + doy - 1;
            let leap = y % 4 == 0;
            let t = y as f64 + (doy - 1) as f64 / if leap { 366.0 } else { 365.0 };
            let th = 2.0 * std::f64::consts::PI * doy as f64 / 365.0;
            let noise = 0.012 * (((y * 31 + k * 17) % 11) as f64 / 5.0 - 1.0);
            let mut v = 0.6 + 0.15 * th.sin() - 0.05 * th.cos() + noise;
            if (2018.5..2020.0).contains(&t) {
                v -= 0.12;
            }
            if y == 2015 && k == 4 {
                v -= 0.4;
            }
            years.push(t);
            days.push(d);
            values.push(v);
        }
    }
    PixelInput { years, values, days: Some(days), ..Default::default() }
}

fn rows(r: &janus_zeit::PixelResult) -> BTreeMap<String, String> {
    r.rows.iter().map(|[a, b]| (a.clone(), b.clone())).collect()
}

#[test]
fn pixel_run_rows_and_lines() {
    let input = thinning();
    let p = Params::with_defaults("ewmacd", &BTreeMap::from([("train_end".to_string(), json!(2018))])).unwrap();
    let r = run_pixel("ewmacd", &p, &input).unwrap();
    let rows = rows(&r);
    assert_eq!(rows["Training"], "2014-2017", "{rows:?}");
    assert!(rows["Disturbance"].starts_with("2018-0") || rows["Disturbance"].starts_with("2018-1"), "{rows:?}");
    assert!(rows["Recovered"].starts_with("2020"), "{rows:?}");
    assert_eq!(rows["State (last date)"], "recovered (disturbed, then back)", "{rows:?}");
    assert!(rows["Charted dates"].contains("(1 screened)"), "{rows:?}");
    let labels: Vec<&str> = r.overlays.iter().map(|o| o.label.as_str()).collect();
    for l in [
        "Harmonic model (training years)",
        "EWMA (on the model)",
        "Lower control limit",
        "Upper control limit",
        "Screened (X-bar)",
        "Monitoring start",
        "Disturbance",
        "Recovered",
    ] {
        assert!(labels.contains(&l), "{l}: {labels:?}");
    }
    let lower = r.overlays.iter().find(|o| o.label == "Lower control limit").unwrap();
    assert_eq!(lower.style.as_deref(), Some("dashed"));

    // the training end by default: three years
    let auto = Params::with_defaults("ewmacd", &BTreeMap::new()).unwrap();
    assert_eq!(rows_of(&auto, &input)["Training"], "2014-2016");
    let (code, _) = calls("ewmacd", &auto, &input).unwrap();
    assert!(code.starts_with("result1 = zeit.ewmacd(series, train_end=2017, order=2, lmbda=0.3"), "{code}");

    // too short a training period: said, not drawn
    let early = Params::with_defaults("ewmacd", &BTreeMap::from([("train_end".to_string(), json!(2014))])).unwrap();
    assert!(run_pixel("ewmacd", &early, &input).unwrap_err().contains("not after the first year"));
    let few = PixelInput {
        years: input.years[..20].to_vec(),
        values: input.values[..20].iter().enumerate().map(|(i, &v)| if i % 3 == 0 { v } else { f64::NAN }).collect(),
        days: input.days.as_ref().map(|d| d[..20].to_vec()),
        ..Default::default()
    };
    let p2 = Params::with_defaults("ewmacd", &BTreeMap::from([("train_end".to_string(), json!(2015))])).unwrap();
    let r = run_pixel("ewmacd", &p2, &few).unwrap();
    assert!(self::rows(&r)["EWMACD"].contains("too few"), "{:?}", r.rows);
}

fn rows_of(p: &Params, input: &PixelInput) -> BTreeMap<String, String> {
    rows(&run_pixel("ewmacd", p, input).unwrap())
}
