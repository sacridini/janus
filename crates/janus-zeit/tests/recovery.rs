//! Recovery (a tool after the bridge): a raster run gives zeit.recovery_metrics' maps on zeit.landtrendr, bit for
//! bit (tests/data/recovery.json, written by tools/recovery_fixtures.py), and a pixel run its rows and lines.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{JobSpec, run_job};
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
fn raster_run_is_zeits_recovery_metrics() {
    let data = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let fixtures: Fixtures =
        serde_json::from_str(&std::fs::read_to_string(data.join("recovery.json")).unwrap()).unwrap();
    let case = data.join("raster/landtrendr-0");
    for (k, c) in fixtures.cases.iter().enumerate() {
        let mut spec: JobSpec =
            serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
        spec.tool = "recovery".into();
        spec.params = c.params.clone();
        spec.input = case.join(&spec.input).to_string_lossy().to_string();
        let out = std::env::temp_dir().join(format!("janus-zeit-recovery-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        for (id, want) in &c.maps {
            let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap_or_else(|| panic!("no output {id}"));
            let got = read_band(o["path"].as_str().unwrap());
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                match w {
                    // the maps are float32, as zeit's
                    Some(w) => assert_eq!(*g as f32, *w as f32, "case {k} {id} pixel {i}"),
                    None => assert!(g.is_nan(), "case {k} {id} pixel {i}: {g}"),
                }
            }
        }
    }
}

/// NBR-like: 0.6, a fire in 2005 down to 0.1, a regrowth of 0.05 a year, a little noise.
fn fire() -> PixelInput {
    let years: Vec<f64> = (1995..=2022).map(|y| f64::from(y) + 0.5).collect();
    let values = years
        .iter()
        .enumerate()
        .map(|(i, &y)| {
            let y = y.floor();
            let v = if y < 2005.0 { 0.6 } else { (0.1 + 0.05 * (y - 2005.0)).min(0.6) };
            v + 0.004 * ((i * 7) % 5) as f64 - 0.008
        })
        .collect();
    // July 1 of each year, in days since 1970
    let days = (1995..=2022).map(|y: i64| (y - 1970) * 365 + (y - 1969) / 4 + 181).collect();
    PixelInput { years, values, days: Some(days), ..Default::default() }
}

fn rows(r: &janus_zeit::PixelResult) -> BTreeMap<String, String> {
    r.rows.iter().map(|[a, b]| (a.clone(), b.clone())).collect()
}

#[test]
fn pixel_run_rows_and_lines() {
    let input = fire();
    let p = Params::with_defaults("recovery", &BTreeMap::new()).unwrap();
    let r = run_pixel("recovery", &p, &input).unwrap();
    let rows = rows(&r);
    assert_eq!(rows["Disturbance (greatest)"], "2004-2005", "{rows:?}");
    // 0.1 + 0.05 k >= 0.8 * 0.6 at k = 8, give or take the noise of the fit
    let y2r: f64 = rows["Years to recovery (Y2R)"].split_whitespace().next().unwrap().parse().unwrap();
    assert!((7.0..=9.0).contains(&y2r), "{rows:?}");
    let rri: f64 = rows["RRI (+5)"].parse().unwrap();
    assert!((rri - 0.5).abs() < 0.1, "{rows:?}");
    let labels: Vec<&str> = r.overlays.iter().map(|o| o.label.as_str()).collect();
    assert!(labels.contains(&"Recovery level (80% of pre-disturbance)"), "{labels:?}");
    assert!(labels.contains(&"Recovered") && labels.contains(&"Recovery window"), "{labels:?}");
    let level = r.overlays.iter().find(|o| o.label.starts_with("Recovery level")).unwrap();
    assert_eq!(level.style.as_deref(), Some("dashed"));
    assert!((level.y[0].unwrap() - 0.48).abs() < 0.02);

    // gains need the magnitude basis
    let gain = Params::with_defaults("recovery", &BTreeMap::from([("event_type".to_string(), json!("gain"))])).unwrap();
    assert!(run_pixel("recovery", &gain, &input).unwrap_err().contains("magnitude"));

    // a series too short for the horizon: the metrics at +k left out, Y2R still measured
    let short = PixelInput {
        years: input.years[..14].to_vec(),
        values: input.values[..14].to_vec(),
        days: input.days.as_ref().map(|d| d[..14].to_vec()),
        ..input
    };
    let rows = self::rows(&run_pixel("recovery", &p, &short).unwrap());
    assert_eq!(rows["RRI (+5)"], "-", "{rows:?}");
}
