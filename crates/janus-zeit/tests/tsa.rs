//! Interpolate (a tool after the bridge, FORCE's TSA interpolation): a raster run gives zeit.interpolate's series on
//! the new grid (tests/data/tsa, written by tools/tsa_fixtures.py: a small series of irregular dates with gaps),
//! one band, and one date as its description, per date of the grid; a pixel run draws the same line. Temporal
//! metrics, Fold and Standardize (tsa_stats.json, from zeit.temporal_metrics and zeit.standardize on the same
//! series) are tested the same way further down.

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

// ---- temporal metrics, fold and standardize ---------------------------------------------------------------------

#[derive(Deserialize)]
struct MetricsCase {
    params: BTreeMap<String, Value>,
    values: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize)]
struct FoldCase {
    params: BTreeMap<String, Value>,
    periods: Vec<String>,
    /// [metric][period][pixel]
    values: Vec<Vec<Vec<Option<f64>>>>,
}

#[derive(Deserialize)]
struct StdCase {
    params: BTreeMap<String, Value>,
    /// [date][pixel]
    values: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize)]
struct Stats {
    temporal_metrics: Vec<MetricsCase>,
    fold: Vec<FoldCase>,
    standardize: Vec<StdCase>,
}

fn stats() -> Stats {
    serde_json::from_str(&std::fs::read_to_string(data().join("tsa_stats.json")).unwrap()).unwrap()
}

fn options(params: &BTreeMap<String, Value>) -> Vec<String> {
    params["metrics"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect()
}

/// A run of `tool` with `params` on the fixtures' series, into a folder of its own.
fn run(tool: &str, params: &BTreeMap<String, Value>, name: &str) -> janus_zeit::job::JobResult {
    let mut spec = spec();
    spec.tool = tool.into();
    spec.params = params.clone();
    let out = std::env::temp_dir().join(format!("janus-zeit-tsa-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap()
}

fn output_path(result: &janus_zeit::job::JobResult, id: &str) -> String {
    let o = result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"));
    o["path"].as_str().unwrap().to_string()
}

/// A band of an output is zeit's, written as float32.
fn assert_band(got: &[f64], want: &[Option<f64>], what: &str) {
    assert_eq!(got.len(), want.len());
    for (i, (g, w)) in got.iter().zip(want).enumerate() {
        assert!(close(*g, w.map(|w| f64::from(w as f32)), 1e-5), "{what} pixel {i}: {g} vs {w:?}");
    }
}

/// The pixels the pixel runs are tried on: with gaps, the long gap, the single observation, none at all.
const PIXELS: [(usize, usize); 6] = [(1, 1), (3, 2), (5, 5), (11, 7), (0, 0), (8, 4)];

#[test]
fn temporal_metrics_raster_run_is_zeits() {
    for (k, c) in stats().temporal_metrics.iter().enumerate() {
        let result = run("temporal_metrics", &c.params, &format!("stm-{k}"));
        let opts = options(&c.params);
        // one map per chosen metric, named after it
        assert_eq!(result.outputs.len(), opts.len());
        for (m, o) in opts.iter().enumerate() {
            let (bands, _) = read(Path::new(&output_path(&result, &format!("m_{o}"))));
            assert_eq!(bands.len(), 1);
            assert_band(&bands[0], &c.values[m], &format!("case {k} metric {o}"));
        }
    }
    // the outputs' names are the metrics' labels
    let c = &stats().temporal_metrics[2];
    let result = run("temporal_metrics", &c.params, "stm-names");
    let names: Vec<&str> = result.outputs.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Standard deviation", "5th percentile", "Number of observations"]);
}

#[test]
fn temporal_metrics_pixel_run_is_zeits() {
    let s = stats();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = pixel_input(p);
        for (k, c) in s.temporal_metrics.iter().enumerate() {
            let params = Params::with_defaults("temporal_metrics", &c.params).unwrap();
            let r = run_pixel("temporal_metrics", &params, &input).unwrap();
            let opts = options(&c.params);
            // "Valid observations", then a row per metric
            assert_eq!(r.rows.len(), opts.len() + 1);
            for (m, o) in opts.iter().enumerate() {
                let got: f64 = r.rows[m + 1][1].parse().unwrap();
                assert!(
                    close(got, c.values[m][p], 1e-5),
                    "case {k} {o} pixel ({x}, {y}): {got} vs {:?}",
                    c.values[m][p]
                );
            }
        }
    }
}

/// zeit's period coordinate as the tool's band label.
fn label(by: &str, period: &str) -> String {
    let k: usize = period.parse().unwrap();
    match by {
        "year" => period.to_string(),
        "quarter" => format!("Q{k}"),
        "month" => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][k - 1].into(),
        "week" => format!("week {k:02}"),
        _ => format!("doy {k:03}"),
    }
}

#[test]
fn fold_raster_run_is_zeits() {
    for (k, c) in stats().fold.iter().enumerate() {
        let by = c.params["by"].as_str().unwrap();
        let result = run("fold", &c.params, &format!("fold-{k}"));
        let opts = options(&c.params);
        assert_eq!(result.outputs.len(), opts.len());
        let labels: Vec<String> = c.periods.iter().map(|p| label(by, p)).collect();
        for (m, o) in opts.iter().enumerate() {
            let out = result.outputs.iter().find(|x| x["id"] == format!("f_{o}").as_str()).unwrap();
            assert_eq!(out["series"], json!(true));
            assert_eq!(out["bands"], json!(true));
            let (bands, descriptions) = read(Path::new(out["path"].as_str().unwrap()));
            // one band per period, named after it (all of them, the empty ones too)
            assert_eq!(descriptions, labels, "case {k} {by}");
            assert_eq!(bands.len(), c.values[m].len());
            for (b, (got, want)) in bands.iter().zip(&c.values[m]).enumerate() {
                assert_band(got, want, &format!("case {k} {by} metric {o} period {}", labels[b]));
            }
        }
    }
    // the series covers at least three calendar years
    let year = stats().fold.into_iter().find(|c| c.params["by"] == "year").unwrap();
    assert!(year.periods.len() >= 3, "{:?}", year.periods);
}

#[test]
fn fold_pixel_run_is_zeits() {
    let s = stats();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = pixel_input(p);
        for (k, c) in s.fold.iter().enumerate() {
            let by = c.params["by"].as_str().unwrap();
            let params = Params::with_defaults("fold", &c.params).unwrap();
            let r = run_pixel("fold", &params, &input).unwrap();
            let opts = options(&c.params);
            if by == "year" {
                // a line per metric at the middle of each year
                assert_eq!(r.overlays.len(), opts.len());
                for (m, line) in r.overlays.iter().enumerate() {
                    assert_eq!(line.kind, "line");
                    assert_eq!(line.y.len(), c.periods.len());
                    for (i, v) in line.y.iter().enumerate() {
                        let year: f64 = c.periods[i].parse().unwrap();
                        assert!((line.x[i] - (year + 0.5)).abs() < 1e-9);
                        assert!(
                            close(v.unwrap_or(f64::NAN), c.values[m][i][p], 1e-5),
                            "case {k} metric {} year {year} pixel ({x}, {y}): {v:?} vs {:?}",
                            opts[m],
                            c.values[m][i][p]
                        );
                    }
                }
            } else {
                assert!(r.overlays.is_empty());
            }
            // the table lists the periods with a value (twelve at most per metric), in order
            if by == "month" || by == "quarter" {
                let wanted: Vec<f64> = c.values.iter().flat_map(|m| m.iter().filter_map(|per| per[p])).collect();
                let got: Vec<f64> = r.rows[1..]
                    .iter()
                    .filter(|[_, v]| !v.contains("no value"))
                    .map(|[_, v]| v.parse().unwrap())
                    .collect();
                assert_eq!(got.len(), wanted.len(), "case {k} {by} pixel ({x}, {y}): {:?}", r.rows);
                for (g, w) in got.iter().zip(&wanted) {
                    assert!(close(*g, Some(*w), 1e-5), "case {k} {by} pixel ({x}, {y}): {g} vs {w}");
                }
            }
        }
    }
}

/// A fold by year opens as a dated yearly series; by month, as a stack of 12 named bands, not dates.
#[test]
fn folds_open_as_a_yearly_series_and_as_named_bands() {
    let s = stats();
    let year = s.fold.iter().find(|c| c.params["by"] == "year" && options(&c.params).len() == 1).unwrap();
    let result = run("fold", &year.params, "fold-open-year");
    let path = output_path(&result, "f_mean");
    let info = janus_cube::open_cube(&[path], &janus_cube::BandSelection::default()).unwrap();
    assert!(info.time_is_date);
    assert_eq!(info.t(), year.periods.len());
    let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
    assert_eq!(labels, year.periods.iter().map(String::as_str).collect::<Vec<_>>());

    for (by, n) in [("month", 12), ("quarter", 4), ("week", 52), ("doy", 365)] {
        let c = s.fold.iter().find(|c| c.params["by"] == by).unwrap();
        let result = run("fold", &c.params, &format!("fold-open-{by}"));
        let path = output_path(&result, &format!("f_{}", options(&c.params)[0]));
        let info = janus_cube::open_cube(&[path], &janus_cube::BandSelection::default()).unwrap();
        assert!(!info.time_is_date, "{by} opened as dates");
        assert_eq!(info.t(), n, "{by}");
        let labels: Vec<&str> = info.layers.iter().map(|l| l.label.as_str()).collect();
        assert_eq!(labels[0], label(by, "1"), "{by}");
        assert_eq!(labels[n - 1], label(by, &n.to_string()), "{by}");
    }
}

#[test]
fn fold_needs_dates_and_valid_parameters() {
    let mut input = pixel_input(0);
    input.days = None;
    let params = Params::with_defaults("fold", &BTreeMap::new()).unwrap();
    let e = run_pixel("fold", &params, &input).unwrap_err();
    assert!(e.contains("dated series"), "{e}");
    let bad = Params::with_defaults("fold", &BTreeMap::from([("by".to_string(), json!("decade"))])).unwrap();
    let e = run_pixel("fold", &bad, &pixel_input(0)).unwrap_err();
    assert!(e.starts_with("ValueError: by must be"), "{e}");
    // no metric chosen
    let none = Params::with_defaults("fold", &BTreeMap::from([("metrics".to_string(), json!([]))])).unwrap();
    let e = run_pixel("fold", &none, &pixel_input(0)).unwrap_err();
    assert!(e.contains("at least one metric"), "{e}");
    // temporal metrics need no dates
    let params = Params::with_defaults("temporal_metrics", &BTreeMap::new()).unwrap();
    run_pixel("temporal_metrics", &params, &input).unwrap();
}

#[test]
fn standardize_raster_run_is_zeits() {
    for (k, c) in stats().standardize.iter().enumerate() {
        let result = run("standardize", &c.params, &format!("std-{k}"));
        let o = result.outputs.iter().find(|o| o["id"] == "series").unwrap();
        assert_eq!(o["series"], json!(true));
        assert!(o.get("bands").is_none_or(|b| b != &json!(true)));
        let (bands, descriptions) = read(Path::new(o["path"].as_str().unwrap()));
        // on the run's dates
        assert_eq!(bands.len(), c.values.len());
        assert_eq!(descriptions.len(), spec().years.len());
        for (b, (got, want)) in bands.iter().zip(&c.values).enumerate() {
            assert_band(got, want, &format!("case {k} {:?} date {b}", c.params));
        }
    }
}

#[test]
fn standardize_pixel_run_is_zeits() {
    let s = stats();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = pixel_input(p);
        for (k, c) in s.standardize.iter().enumerate() {
            let params = Params::with_defaults("standardize", &c.params).unwrap();
            let r = run_pixel("standardize", &params, &input).unwrap();
            let line = &r.overlays[0];
            assert_eq!(line.kind, "line");
            assert_eq!(line.x, input.years);
            assert_eq!(line.y.len(), c.values.len());
            for (b, v) in line.y.iter().enumerate() {
                assert!(
                    close(v.unwrap_or(f64::NAN), c.values[b][p], 1e-5),
                    "case {k} pixel ({x}, {y}) date {b}: {v:?} vs {:?}",
                    c.values[b][p]
                );
            }
        }
    }
    let bad = Params::with_defaults("standardize", &BTreeMap::from([("method".to_string(), json!("rank"))])).unwrap();
    let e = run_pixel("standardize", &bad, &pixel_input(0)).unwrap_err();
    assert!(e.starts_with("ValueError: method must be"), "{e}");
}

// ---- linear trend and CAT of a fold ------------------------------------------------------------------------------
// tsa_trend.json: zeit.linear_trend and zeit.cat of zeit.temporal_metrics by period, on trend_input.tif (about nine
// years, a clear drop at (3, 2), a dip that recovers at (1, 6), a flat pixel at (5, 5), a year without data at
// (8, 4), a pixel without a valid observation at (0, 0) and one with a single one at (11, 7)).

#[derive(Deserialize)]
struct TrendCase {
    params: BTreeMap<String, Value>,
    /// [statistic][pixel], in the order of zeit's TREND_NAMES or CAT_NAMES
    values: Vec<Vec<Option<f64>>>,
}

#[derive(Deserialize)]
struct Trends {
    linear_trend: Vec<TrendCase>,
    cat: Vec<TrendCase>,
}

const TREND_NAMES: [&str; 12] =
    ["mean", "offset", "slope", "pct_gain", "abs_gain", "rsq", "sig", "rmse", "mae", "maxe", "num", "len"];

fn trend_names() -> Vec<String> {
    TREND_NAMES.iter().map(|s| s.to_string()).collect()
}

fn cat_names() -> Vec<String> {
    let mut v: Vec<String> = ["change", "loss", "change_time"].iter().map(|s| s.to_string()).collect();
    for part in ["total", "before", "after"] {
        v.extend(TREND_NAMES.iter().map(|n| format!("{part}_{n}")));
    }
    v
}

fn trends() -> Trends {
    serde_json::from_str(&std::fs::read_to_string(data().join("tsa_trend.json")).unwrap()).unwrap()
}

fn trend_spec() -> JobSpec {
    let mut spec: JobSpec =
        serde_json::from_str(&std::fs::read_to_string(data().join("trend_spec.json")).unwrap()).unwrap();
    spec.input = data().join(&spec.input).to_string_lossy().to_string();
    spec
}

fn trend_pixel_input(p: usize) -> PixelInput {
    let s = trend_spec();
    let (bands, _) = read(Path::new(&s.input));
    PixelInput { years: s.years, days: s.days, values: bands.iter().map(|b| b[p]).collect(), ..Default::default() }
}

/// The case's parameters with every statistic chosen.
fn with_all(tool: &str, c: &TrendCase) -> BTreeMap<String, Value> {
    let names = if tool == "cat" { cat_names() } else { trend_names() };
    let mut p = c.params.clone();
    p.insert("statistics".into(), json!(names));
    p
}

/// A run of `tool` on the trend series with `params`, into a folder of its own.
fn run_trend(tool: &str, params: BTreeMap<String, Value>, name: &str) -> janus_zeit::job::JobResult {
    let mut spec = trend_spec();
    spec.tool = tool.into();
    spec.params = params;
    let out = std::env::temp_dir().join(format!("janus-zeit-tsa-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap()
}

#[test]
fn linear_trend_and_cat_raster_runs_are_zeits() {
    let t = trends();
    for (tool, cases, names, prefix) in
        [("linear_trend", &t.linear_trend, trend_names(), "t"), ("cat", &t.cat, cat_names(), "c")]
    {
        for (k, c) in cases.iter().enumerate() {
            let result = run_trend(tool, with_all(tool, c), &format!("{tool}-{k}"));
            assert_eq!(result.outputs.len(), names.len());
            for (m, name) in names.iter().enumerate() {
                let (bands, _) = read(Path::new(&output_path(&result, &format!("{prefix}_{name}"))));
                assert_eq!(bands.len(), 1);
                assert_band(&bands[0], &c.values[m], &format!("{tool} case {k} {:?} {name}", c.params));
            }
        }
    }
    // the pixels with something to find: the drop's change, nothing without observations
    let drop = 2 * W + 3;
    let cat = &t.cat[0];
    assert!(cat.values[0][drop].unwrap() > 0.15, "the drop's change: {:?}", cat.values[0][drop]);
    assert!(cat.values[1][0].is_none() && cat.values[1][11 + 7 * W].is_none());
    // the penalty makes a difference somewhere (the dip that recovers)
    assert_ne!(t.cat[0].values[2], t.cat[1].values[2]);
}

#[test]
fn linear_trend_and_cat_defaults_and_names() {
    let result = run_trend("cat", BTreeMap::new(), "cat-defaults");
    let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["c_change", "c_loss", "c_change_time", "c_before_slope", "c_after_slope"]);
    let names: Vec<&str> = result.outputs.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names[0], "Change (the greatest loss)");
    assert_eq!(names[3], "Slope (per period), before the change");
    let result = run_trend("linear_trend", BTreeMap::new(), "trend-defaults");
    let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["t_slope", "t_offset", "t_rsq", "t_sig"]);
    assert_eq!(result.outputs[0]["name"], "Slope (per period)");
}

#[test]
fn linear_trend_and_cat_pixel_runs_are_zeits() {
    let t = trends();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = trend_pixel_input(p);
        for (tool, cases, names) in [("linear_trend", &t.linear_trend, trend_names()), ("cat", &t.cat, cat_names())] {
            for (k, c) in cases.iter().enumerate() {
                let by = c.params["by"].as_str().unwrap();
                let params = Params::with_defaults(tool, &with_all(tool, c)).unwrap();
                let r = run_pixel(tool, &params, &input).unwrap();
                // "Valid observations", then a row per statistic
                assert_eq!(r.rows.len(), names.len() + 1);
                for (m, name) in names.iter().enumerate() {
                    let got: f64 = r.rows[m + 1][1].parse().unwrap();
                    assert!(
                        close(got, c.values[m][p], 1e-5),
                        "{tool} case {k} {name} pixel ({x}, {y}): {got} vs {:?}",
                        c.values[m][p]
                    );
                }
                if by != "year" {
                    assert!(r.overlays.is_empty());
                    continue;
                }
                // the fold's values as markers at the middle of each year
                let markers = &r.overlays[0];
                assert_eq!(markers.kind, "markers");
                assert!(markers.x.iter().all(|x| (x.fract() - 0.5).abs() < 1e-9), "{:?}", markers.x);
                let at = |name: &str| c.values[names.iter().position(|n| n == name).unwrap()][p];
                if tool == "linear_trend" {
                    // the line is the fit: offset + slope * (period - first)
                    match (at("offset"), at("slope")) {
                        (Some(o), Some(s)) => {
                            let line = &r.overlays[1];
                            assert_eq!(line.kind, "line");
                            for (i, v) in line.y.iter().enumerate() {
                                assert!((v.unwrap() - (o + s * i as f64)).abs() < 1e-6 * (1.0 + o.abs()));
                            }
                        }
                        _ => assert_eq!(r.overlays.len(), 1),
                    }
                } else if let Some(year) = at("change_time") {
                    let v = r.overlays.iter().find(|o| o.kind == "vlines").expect("the change");
                    assert_eq!(v.x, [year + 0.5], "case {k} pixel ({x}, {y})");
                } else {
                    assert_eq!(r.overlays.len(), 1);
                }
            }
        }
    }
}

#[test]
fn the_change_time_is_the_year_or_the_period_number() {
    let t = trends();
    let time = cat_names().iter().position(|n| n == "change_time").unwrap();
    let drop = 2 * W + 3;
    // by year: the calendar year of the drop (the series starts in 2012 and the drop is after its fifth year)
    let year = t.cat[0].values[time][drop].unwrap();
    assert!((2016.0..=2019.0).contains(&year), "{year}");
    // by month: 1-12
    assert!(t.cat[4].values[time].iter().flatten().all(|m| (1.0..=12.0).contains(m)));
}

#[test]
fn linear_trend_and_cat_refuse_bad_parameters() {
    let input = trend_pixel_input(1);
    for tool in ["linear_trend", "cat"] {
        let run = |k: &str, v: Value| {
            let params = Params::with_defaults(tool, &BTreeMap::from([(k.to_string(), v)])).unwrap();
            run_pixel(tool, &params, &input).unwrap_err()
        };
        assert!(run("conf", json!(1.5)).starts_with("ValueError: conf must be between 0 and 1"));
        assert!(run("tail", json!("both")).starts_with("ValueError: tail must be one of"));
        assert!(run("by", json!("decade")).starts_with("ValueError: by must be"));
        assert!(run("statistics", json!([])).contains("at least one statistic"));
        let e = run("metric", json!("mode"));
        assert!(e.starts_with("ValueError"), "{e}");
        let mut undated = input.clone();
        undated.days = None;
        let params = Params::with_defaults(tool, &BTreeMap::new()).unwrap();
        assert!(run_pixel(tool, &params, &undated).unwrap_err().contains("dated series"));
    }
}
