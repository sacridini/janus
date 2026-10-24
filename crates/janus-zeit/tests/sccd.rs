//! S-CCD (a tool after the bridge): a raster run gives zeit.sccd's maps (tests/data/sccd.json, written by
//! tools/sccd_fixtures.py), and a pixel run its rows and lines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::pixel::Shown;
use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::Value;

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
    serde_json::from_str(&std::fs::read_to_string(data().join("sccd.json")).unwrap()).unwrap()
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

#[test]
fn raster_run_is_zeits_sccd() {
    let case = data().join("raster/ccdc-0");
    for (k, c) in fixtures().cases.iter().enumerate() {
        let spec: Value = serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
        let mut spec: JobSpec = serde_json::from_value(spec).unwrap();
        spec.tool = "sccd".into();
        spec.params = c.params.clone();
        let abs = |p: &str| case.join(p).to_string_lossy().to_string();
        spec.input = abs(&spec.input);
        spec.qa_input = spec.qa_input.as_deref().map(abs);
        spec.bands = spec.bands.iter().map(|(r, p)| (r.clone(), abs(p))).collect();
        let out = std::env::temp_dir().join(format!("janus-zeit-sccd-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        for (id, want) in &c.maps {
            let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap_or_else(|| panic!("no output {id}"));
            let got = read(Path::new(o["path"].as_str().unwrap())).remove(0);
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                match w {
                    // zeit's magnitudes are float32 before their norm
                    Some(w) if id == "magnitude" => assert!((g - w).abs() <= 1e-5 * w.abs(), "case {k} {id} [{i}]"),
                    Some(w) => assert_eq!(*g as f32, *w as f32, "case {k} {id} pixel {i}"),
                    None => assert!(g.is_nan(), "case {k} {id} pixel {i}: {g}"),
                }
            }
        }
    }
}

/// The series of one pixel of the CCDC case: its bands, Fmask codes and NDVI.
fn pixel_input(px: usize) -> PixelInput {
    let case = data().join("raster/ccdc-0");
    let spec: Value = serde_json::from_str(&std::fs::read_to_string(case.join("spec.json")).unwrap()).unwrap();
    let days: Vec<i64> = serde_json::from_value(spec["days"].clone()).unwrap();
    let years: Vec<f64> = serde_json::from_value(spec["years"].clone()).unwrap();
    let mut bands = BTreeMap::new();
    for (role, file) in spec["bands"].as_object().unwrap() {
        let v: Vec<f64> = read(&case.join(file.as_str().unwrap())).iter().map(|b| b[px]).collect();
        bands.insert(role.clone(), v);
    }
    let qa: Vec<f64> = read(&case.join("qa.tif")).iter().map(|b| b[px]).collect();
    let values = bands["nir"].iter().zip(&bands["red"]).map(|(n, r)| (n - r) / (n + r)).collect();
    PixelInput {
        years,
        days: Some(days),
        values,
        bands,
        qa: Some(qa),
        qa_rule: Some("fmask".into()),
        shown: Some(Shown { band: None, nd: Some(vec!["nir".into(), "red".into()]) }),
        map_year: None,
    }
}

#[test]
fn pixel_run_rows_and_lines() {
    let maps = &fixtures().cases[0].maps;
    // a pixel with a break, as the raster run found it
    let px = maps["first_break"].iter().position(Option::is_some).expect("a pixel with a break");
    let input = pixel_input(px);
    let p = Params::with_defaults("sccd", &BTreeMap::new()).unwrap();
    let r = run_pixel("sccd", &p, &input).unwrap();
    let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
    assert_eq!(rows["Units"], "Reflectance x 10000", "{rows:?}");
    assert!(rows.contains_key("Segment 1") && rows.contains_key("Monitoring"), "{rows:?}");
    let breaks = r.overlays.iter().find(|o| o.label == "S-CCD breaks").expect("the breaks drawn");
    assert_eq!(breaks.x[0] as f32, maps["first_break"][px].unwrap() as f32);
    assert_eq!(breaks.x.len() as f64, maps["n_breaks"][px].unwrap());
    let segment = r.overlays.iter().find(|o| o.label == "S-CCD segment 1").expect("the first model drawn");
    // the model in the shown series' units (NDVI)
    assert!(segment.y.iter().flatten().all(|v| (-1.0..=1.0).contains(v)), "{:?}", segment.y);
    assert!(segment.y.iter().flatten().count() > 12);

    let (code, inputs) = calls("sccd", &p, &input).unwrap();
    assert!(code.starts_with("result1 = zeit.sccd(bands_cube.sel("), "{code}");
    assert!(code.contains("conse=6, prob_threshold=0.99, outlier_prob=0.99999, max_angle=30"), "{code}");
    assert_eq!(inputs, vec!["bands_cube", "fmask"]);

    // the bands not mapped: said
    let bare = PixelInput { bands: BTreeMap::new(), ..input };
    assert!(run_pixel("sccd", &p, &bare).unwrap().rows[0][1].starts_with("map the bands first"));
}
