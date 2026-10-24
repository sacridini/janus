//! Emerging hot spots (a tool after the bridge): a raster run gives Zeit's emerging_hotspots exactly (the patterns,
//! the dates of hot and cold spots) and its Gi* and trend to float32 (tests/data/hotspots/hotspots.json, written by
//! tools/hotspots_fixtures.py), the patterns as the tool's classes.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::manifest;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    pattern: Vec<Option<f64>>,
    gi: Vec<Option<f64>>,
    trend_z: Vec<Option<f64>>,
    n_hot: Vec<Option<f64>>,
    n_cold: Vec<Option<f64>>,
}

#[derive(Deserialize)]
struct Fixtures {
    input: String,
    nodata: f64,
    years: Vec<f64>,
    days: Vec<i64>,
    cases: Vec<Case>,
}

/// Every band of a raster, one after the other, NoData as NaN.
fn read_all(path: &str) -> Vec<f64> {
    let ds = Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let mut out = vec![];
    for b in 1..=ds.raster_count() {
        let band = ds.rasterband(b).unwrap();
        let nd = band.no_data_value();
        let data = band.read_as::<f64>((0, 0), (w, h), (w, h), None).unwrap();
        out.extend(data.data().iter().map(|&v| if nd.is_some_and(|d| d == v) || v.is_nan() { f64::NAN } else { v }));
    }
    out
}

fn close(got: &[f64], want: &[Option<f64>], tol: f64, what: &str) {
    assert_eq!(got.len(), want.len(), "{what}");
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        match w {
            Some(w) => assert!((g - w).abs() <= tol * w.abs().max(1.0), "{what} value {i}: {g} != {w}"),
            None => assert!(g.is_nan(), "{what} value {i}: {g}"),
        }
    }
}

#[test]
fn raster_run_is_zeits_emerging_hotspots() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let fx: Fixtures =
        serde_json::from_str(&std::fs::read_to_string(data.join("hotspots/hotspots.json")).unwrap()).unwrap();
    for (k, c) in fx.cases.iter().enumerate() {
        let out = std::env::temp_dir().join(format!("janus-zeit-hotspots-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        let spec = JobSpec {
            tool: "hotspots".into(),
            params: c.params.clone(),
            input: data.join(&fx.input).to_string_lossy().to_string(),
            nodata: Some(fx.nodata),
            years: fx.years.clone(),
            days: Some(fx.days.clone()),
            output_dir: out.to_string_lossy().to_string(),
            ..Default::default()
        };
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let path = |id: &str| {
            result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"))["path"]
                .as_str()
                .unwrap()
                .to_string()
        };
        close(&read_all(&path("pattern")), &c.pattern, 0.0, &format!("case {k} pattern"));
        close(&read_all(&path("n_hot")), &c.n_hot, 0.0, &format!("case {k} n_hot"));
        close(&read_all(&path("n_cold")), &c.n_cold, 0.0, &format!("case {k} n_cold"));
        close(&read_all(&path("gi")), &c.gi, 1e-6, &format!("case {k} gi"));
        close(&read_all(&path("trend_z")), &c.trend_z, 1e-6, &format!("case {k} trend_z"));
        assert_eq!(Dataset::open(path("gi")).unwrap().raster_count(), fx.years.len());
        assert!(c.pattern.iter().flatten().any(|&v| v > 9.0) && c.pattern.iter().flatten().any(|&v| v == 3.0));
    }
}

#[test]
fn patterns_are_named_in_order() {
    let tool = manifest::manifest().tool("hotspots").unwrap();
    let pattern = tool.outputs.iter().find(|o| o.id == "pattern").unwrap();
    let names: Vec<String> = serde_json::from_value(pattern.classes.clone().unwrap()).unwrap();
    assert_eq!(names.len(), 17);
    assert_eq!(
        (names[0].as_str(), names[1].as_str(), names[8].as_str()),
        ("No pattern", "New hot spot", "Historical hot spot")
    );
    assert_eq!((names[9].as_str(), names[16].as_str()), ("New cold spot", "Historical cold spot"));
    for (code, value) in [(0i8, 1.0), (1, 2.0), (8, 9.0), (-1, 10.0), (-8, 17.0)] {
        assert_eq!(janus_zeit::hotspots::class_of(code), value);
    }
    assert!(janus_zeit::hotspots::class_of(i8::MIN).is_nan());
}
