//! Interpolate (a tool after the bridge, FORCE's TSA interpolation): a raster run gives zeit.interpolate's series on
//! the new grid (tests/data/tsa, written by tools/tsa_fixtures.py: a small series of irregular dates with gaps),
//! one band, and one date as its description, per date of the grid; a pixel run draws the same line.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::{Dataset, Metadata};
use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

const W: usize = 12;

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    dates: Vec<String>,
    values: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize)]
struct Fixtures {
    cases: Vec<Case>,
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/tsa")
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("tsa.json")).unwrap()).unwrap()
}

fn spec() -> JobSpec {
    let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(data().join("spec.json")).unwrap()).unwrap();
    spec.input = data().join(&spec.input).to_string_lossy().to_string();
    spec
}

/// A raster's bands as [band][pixels] (NoData as NaN) and their descriptions.
fn read(path: &Path) -> (Vec<Vec<f64>>, Vec<String>) {
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
    (bands, descriptions)
}

/// `got` is `want` to `rel` (relative to its size, with a little absolute slack), or both are missing.
fn close(got: f64, want: Option<f64>, rel: f64) -> bool {
    match want {
        Some(w) => got.is_finite() && (got - w).abs() <= rel * w.abs() + 1e-6,
        None => !got.is_finite(),
    }
}

#[test]
fn raster_run_is_zeits_interpolate() {
    for (k, c) in fixtures().cases.iter().enumerate() {
        let mut spec = spec();
        spec.params = c.params.clone();
        let out = std::env::temp_dir().join(format!("janus-zeit-tsa-test-{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().to_string();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let o = result.outputs.iter().find(|o| o["id"] == "series").expect("the series output");
        assert_eq!(o["series"], json!(true));
        assert_eq!(o["bands"], json!(true));
        let (bands, descriptions) = read(Path::new(o["path"].as_str().unwrap()));
        // the bands are the new grid's dates, labelled by them
        assert_eq!(descriptions, c.dates, "case {k} {:?}", c.params);
        assert_eq!(bands.len(), c.values.len(), "case {k}");
        for (b, (got, want)) in bands.iter().zip(&c.values).enumerate() {
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(want).enumerate() {
                // written as float32
                assert!(
                    close(*g, w.map(|w| f64::from(w as f32)), 1e-5),
                    "case {k} {:?} band {b} pixel {i}: {g} vs {w:?}",
                    c.params
                );
            }
        }
    }
}

/// A run's file opens as a series with the grid's dates (what the app does with a series output).
#[test]
fn the_output_opens_as_a_dated_series() {
    let c = &fixtures().cases[0];
    let mut spec = spec();
    let out = std::env::temp_dir().join("janus-zeit-tsa-test-cube");
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
    let path = result.outputs[0]["path"].as_str().unwrap().to_string();
    let info = janus_cube::open_cube(&[path], &janus_cube::BandSelection::default()).unwrap();
    assert!(info.time_is_date);
    assert_eq!(info.t(), c.dates.len());
    let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, c.dates.iter().map(String::as_str).collect::<Vec<_>>());
    // 16 days apart: the grid is not the run's dates (120, irregular)
    assert!((1..info.t()).all(|t| info.unix_day(t) - info.unix_day(t - 1) == 16), "{labels:?}");
}

/// One pixel's series of the case's input.
fn pixel_input(p: usize) -> PixelInput {
    let s = spec();
    let (bands, _) = read(Path::new(&s.input));
    PixelInput { years: s.years, days: s.days, values: bands.iter().map(|b| b[p]).collect(), ..Default::default() }
}

#[test]
fn pixel_run_draws_the_same_line() {
    let f = fixtures();
    // a pixel with gaps, the long gap, the one with a single observation, the one without
    for (x, y) in [(1, 1), (3, 2), (5, 5), (11, 7), (0, 0), (8, 4)] {
        let p = y * W + x;
        let input = pixel_input(p);
        for (k, c) in f.cases.iter().enumerate() {
            let params = Params::with_defaults("interpolate", &c.params).unwrap();
            let r = run_pixel("interpolate", &params, &input).unwrap();
            let line = &r.overlays[0];
            assert_eq!(line.kind, "line");
            assert_eq!(line.x.len(), c.dates.len());
            assert_eq!(line.y.len(), c.dates.len());
            for (b, y) in line.y.iter().enumerate() {
                assert!(
                    close(y.unwrap_or(f64::NAN), c.values[b][p], 1e-5),
                    "case {k} pixel ({x}, {y:?}) date {}: {y:?} vs {:?}",
                    c.dates[b],
                    c.values[b][p]
                );
            }
            // x in decimal years, increasing, from the first date of the series
            assert!(line.x.windows(2).all(|w| w[1] > w[0]));
            assert!((line.x[0] - input.years[0]).abs() < 1e-9, "{} vs {}", line.x[0], input.years[0]);
            let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
            assert!(rows["Grid dates"].starts_with(&c.dates.len().to_string()), "{rows:?}");
        }
    }
}

#[test]
fn a_series_without_dates_is_refused() {
    let mut input = pixel_input(0);
    input.days = None;
    let params = Params::with_defaults("interpolate", &BTreeMap::new()).unwrap();
    let e = run_pixel("interpolate", &params, &input).unwrap_err();
    assert!(e.contains("dated series"), "{e}");
    // a series shorter than the step: no date in the grid
    let short = PixelInput {
        years: vec![2020.0; 3],
        days: Some(vec![18300, 18301, 18302]),
        values: vec![1.0; 3],
        ..Default::default()
    };
    let e = run_pixel("interpolate", &params, &short).unwrap_err();
    assert!(e.contains("no date in the grid"), "{e}");
}
