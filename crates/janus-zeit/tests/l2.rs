//! Tasseled Cap and the spectral adjustment (tools after the bridge, FORCE's TC-* and spectral adjustment): a raster
//! run gives zeit.tasseled_cap's and zeit.spectral_adjust's series (tests/data/l2, written by tools/l2_fixtures.py: a
//! small multiband series of 24 dates with an Fmask band, whose dates are of Landsat 5, 7, 8 and 9, MODIS and one of
//! Sentinel-2), one series per component or band, a band per date; a pixel run draws the same lines.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::{Dataset, Metadata};
use janus_zeit::job::{JobResult, JobSpec, run_job};
use janus_zeit::python::calls;
use janus_zeit::{Params, PixelInput, run_pixel};
use serde::Deserialize;
use serde_json::{Value, json};

const W: usize = 12;

#[derive(Deserialize)]
struct TasseledCase {
    params: BTreeMap<String, Value>,
    /// [component][date][pixel]
    values: Vec<Vec<Vec<Option<f64>>>>,
}

#[derive(Deserialize)]
struct AdjustCase {
    params: BTreeMap<String, Value>,
    bands: Vec<String>,
    /// [band][date][pixel]
    values: Vec<Vec<Vec<Option<f64>>>>,
}

#[derive(Deserialize)]
struct Fixtures {
    sensors: Vec<String>,
    landsat_sensors: Vec<String>,
    tasseled_cap: Vec<TasseledCase>,
    spectral_adjust: Vec<AdjustCase>,
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/l2")
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("l2.json")).unwrap()).unwrap()
}

/// The series' spec, its files made absolute.
fn spec() -> JobSpec {
    let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(data().join("spec.json")).unwrap()).unwrap();
    let abs = |p: &str| data().join(p).to_string_lossy().to_string();
    spec.input = abs(&spec.input);
    spec.qa_input = spec.qa_input.as_deref().map(abs);
    for b in spec.bands.values_mut() {
        *b = abs(b);
    }
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

/// A run of `tool` with `params` on the fixtures' series whose dates have the sensors `sensors`.
fn run(tool: &str, params: &BTreeMap<String, Value>, sensors: &[String], name: &str) -> Result<JobResult, String> {
    let mut spec = spec();
    spec.tool = tool.into();
    spec.params = params.clone();
    spec.sensors = sensors.to_vec();
    let out = std::env::temp_dir().join(format!("janus-zeit-l2-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(&spec, &|_, _| {}, &AtomicBool::new(false))
}

fn output_path(result: &JobResult, id: &str) -> String {
    let o = result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"));
    assert_eq!(o["series"], json!(true), "{id}");
    o["path"].as_str().unwrap().to_string()
}

/// A series output is zeit's (a band per date, the pixels in reading order), written as float32.
fn assert_series(path: &str, want: &[Vec<Option<f64>>], what: &str) {
    let (bands, _) = read(Path::new(path));
    assert_eq!(bands.len(), want.len(), "{what}");
    let (mut present, mut worst) = (0, 0.0f64);
    for (k, (got, want)) in bands.iter().zip(want).enumerate() {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            // written as float32
            let w = w.map(|w| f64::from(w as f32));
            assert!(close(*g, w, 1e-5), "{what} date {k} pixel {i}: {g} vs {w:?}");
            if let Some(w) = w {
                present += 1;
                worst = worst.max((g - w).abs() / w.abs().max(1.0));
            }
        }
    }
    assert!(present > 0, "{what}: no value to compare");
    println!("{what}: {present} values, the largest relative difference {worst:.2e}");
}

/// The series' sensors, as strings.
fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|x| x.as_str().unwrap().to_string()).collect()
}

/// One pixel's series of the case's input (the pixels in reading order), with the sensors given.
fn pixel_input(p: usize, sensors: &[String]) -> PixelInput {
    let s = spec();
    let (shown, _) = read(Path::new(&s.input));
    let bands: BTreeMap<String, Vec<f64>> = s
        .bands
        .iter()
        .map(|(role, path)| (role.clone(), read(Path::new(path)).0.iter().map(|b| b[p]).collect()))
        .collect();
    let (qa, _) = read(Path::new(s.qa_input.as_deref().unwrap()));
    PixelInput {
        years: s.years,
        days: s.days,
        values: shown.iter().map(|b| b[p]).collect(),
        bands,
        qa: Some(qa.iter().map(|b| b[p]).collect()),
        qa_rule: s.qa_rule,
        sensors: sensors.to_vec(),
        ..Default::default()
    }
}

/// The pixels the pixel runs are tried on: with gaps, one under water, the single observation, none at all.
const PIXELS: [(usize, usize); 6] = [(1, 1), (3, 2), (5, 5), (11, 7), (0, 0), (8, 4)];

/// The overlay of a series: the values of its valid dates are zeit's.
fn assert_line(overlay: &janus_zeit::Overlay, want: &[Vec<Option<f64>>], p: usize, what: &str) {
    assert_eq!(overlay.kind, "line");
    let valid: Vec<f64> = want.iter().filter_map(|d| d[p]).collect();
    assert_eq!(overlay.y.len(), valid.len(), "{what}");
    for (g, w) in overlay.y.iter().zip(&valid) {
        // the maps are float32, the chart's values are not rounded to it
        assert!(close(g.unwrap(), Some(f64::from(*w as f32)), 1e-5), "{what}: {g:?} vs {w}");
    }
}

fn component_label(id: &str) -> &'static str {
    match id {
        "brightness" => "Brightness",
        "greenness" => "Greenness",
        "wetness" => "Wetness",
        _ => "Disturbance (B - G - W)",
    }
}

#[test]
fn tasseled_cap_raster_run_is_zeits() {
    let f = fixtures();
    for (k, c) in f.tasseled_cap.iter().enumerate() {
        let result = run("tasseled_cap", &c.params, &f.sensors, &format!("tc-{k}")).unwrap();
        let components = strings(&c.params["components"]);
        // one series per component, in the order asked
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, components.iter().map(|n| format!("tc_{n}")).collect::<Vec<_>>());
        for (m, name) in components.iter().enumerate() {
            let path = output_path(&result, &format!("tc_{name}"));
            assert_series(&path, &c.values[m], &format!("tasseled_cap case {k} {name}"));
        }
    }
    // the outputs' names are the components'
    let result = run("tasseled_cap", &f.tasseled_cap[0].params, &f.sensors, "tc-names").unwrap();
    let names: Vec<&str> = result.outputs.iter().map(|o| o["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Brightness", "Greenness", "Wetness"]);
}

#[test]
fn tasseled_cap_pixel_run_is_zeits() {
    let f = fixtures();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = pixel_input(p, &f.sensors);
        for (k, c) in f.tasseled_cap.iter().enumerate() {
            let params = Params::with_defaults("tasseled_cap", &c.params).unwrap();
            let r = run_pixel("tasseled_cap", &params, &input).unwrap();
            let components = strings(&c.params["components"]);
            let with_values: Vec<usize> =
                (0..components.len()).filter(|&m| c.values[m].iter().any(|d| d[p].is_some())).collect();
            assert_eq!(r.overlays.len(), with_values.len(), "case {k} pixel ({x}, {y})");
            for (o, &m) in r.overlays.iter().zip(&with_values) {
                assert_eq!(o.label, component_label(&components[m]));
                assert_line(o, &c.values[m], p, &format!("case {k} {} pixel ({x}, {y})", components[m]));
            }
            let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
            let valid = c.values[0].iter().filter(|d| d[p].is_some()).count();
            assert_eq!(rows["Valid observations"], format!("{valid} of {}", input.values.len()));
            if c.params["coefficients"] == "sensor" {
                // the matrix of each date's sensor: TM (and MSI) Crist, ETM+ Huang, OLI Baig
                assert!(rows["Matrix"].contains("Huang") && rows["Matrix"].contains("Baig"), "{rows:?}");
            }
        }
    }
}

#[test]
fn spectral_adjust_raster_run_is_zeits() {
    let f = fixtures();
    for (k, c) in f.spectral_adjust.iter().enumerate() {
        // a series of one sensor named, or each date's own (MODIS among them)
        let result = run("spectral_adjust", &c.params, &f.landsat_sensors, &format!("sa-{k}")).unwrap();
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, c.bands.iter().map(String::as_str).collect::<Vec<_>>(), "case {k}");
        for (m, band) in c.bands.iter().enumerate() {
            let path = output_path(&result, band);
            assert_series(&path, &c.values[m], &format!("spectral_adjust case {k} {:?} {band}", c.params));
        }
    }
}

#[test]
fn spectral_adjust_pixel_run_is_zeits() {
    let f = fixtures();
    for (x, y) in PIXELS {
        let p = y * W + x;
        let input = pixel_input(p, &f.landsat_sensors);
        for (k, c) in f.spectral_adjust.iter().enumerate() {
            let params = Params::with_defaults("spectral_adjust", &c.params).unwrap();
            let r = run_pixel("spectral_adjust", &params, &input).unwrap();
            assert_eq!(r.overlays.len(), c.bands.len());
            for (o, (m, band)) in r.overlays.iter().zip(c.bands.iter().enumerate()) {
                assert!(o.label.starts_with("Sentinel-2A: "), "{}", o.label);
                assert_line(o, &c.values[m], p, &format!("case {k} {band} pixel ({x}, {y})"));
            }
        }
    }
}

/// The date the series' `k`th band is of, as the tools' messages write it.
fn iso(k: usize) -> String {
    let (y, m, d) = janus_zeit::numeric::civil(spec().days.unwrap()[k]);
    format!("{y:04}-{m:02}-{d:02}")
}

#[test]
fn a_date_without_a_sensor_is_an_error_naming_it() {
    let f = fixtures();
    let mut unknown = f.landsat_sensors.clone();
    unknown[3] = String::new();
    unknown[9] = String::new();
    // Tasseled Cap has no matrix for MODIS, and says which date
    let by_sensor = BTreeMap::from([("coefficients".to_string(), json!("sensor"))]);
    let params = Params::with_defaults("tasseled_cap", &by_sensor).unwrap();
    let e = run_pixel("tasseled_cap", &params, &pixel_input(1, &f.landsat_sensors)).unwrap_err();
    assert!(e.contains("MOD01") && e.contains(&iso(2)), "{e}");
    // a Sentinel-2 date has one
    run_pixel("tasseled_cap", &params, &pixel_input(1, &f.sensors)).unwrap();
    unknown[2] = "LND05".into();
    unknown[20] = "LND09".into();
    // the per-sensor matrices
    let e = run_pixel("tasseled_cap", &params, &pixel_input(1, &unknown)).unwrap_err();
    assert!(e.contains(&iso(3)) && e.contains("choose a matrix for every date"), "{e}");
    let Err(e) = run("tasseled_cap", &by_sensor, &unknown, "tc-unknown") else {
        panic!("an unknown sensor was accepted")
    };
    assert!(e.contains(&iso(3)) && e.contains("choose a matrix"), "{e}");
    // no sensors at all (the caller did not read them)
    let e = run_pixel("tasseled_cap", &params, &pixel_input(1, &[])).unwrap_err();
    assert!(e.contains("needs each date's sensor"), "{e}");
    // a fixed matrix needs none
    let fixed = Params::with_defaults("tasseled_cap", &BTreeMap::new()).unwrap();
    run_pixel("tasseled_cap", &fixed, &pixel_input(1, &unknown)).unwrap();
    run("tasseled_cap", &BTreeMap::new(), &[], "tc-no-sensors").unwrap();

    // the spectral adjustment of each date's own sensor
    let auto = Params::with_defaults("spectral_adjust", &BTreeMap::new()).unwrap();
    let e = run_pixel("spectral_adjust", &auto, &pixel_input(1, &unknown)).unwrap_err();
    assert!(e.contains(&iso(3)) && e.contains("say which one the series is of"), "{e}");
    let Err(e) = run("spectral_adjust", &BTreeMap::new(), &unknown, "sa-unknown") else {
        panic!("an unknown sensor was accepted")
    };
    assert!(e.contains(&iso(3)), "{e}");
    let e = run_pixel("spectral_adjust", &auto, &pixel_input(1, &[])).unwrap_err();
    assert!(e.contains("needs each date's sensor"), "{e}");
    // a Sentinel-2 date is not adjusted (Zeit passes it through with bands Janus does not map)
    let e = run_pixel("spectral_adjust", &auto, &pixel_input(1, &f.sensors)).unwrap_err();
    assert!(e.contains(&iso(11)) && e.contains("Sentinel-2"), "{e}");
    // naming the sensor says it for every date
    let named = BTreeMap::from([("sensor".to_string(), json!("LND08"))]);
    let named = Params::with_defaults("spectral_adjust", &named).unwrap();
    run_pixel("spectral_adjust", &named, &pixel_input(1, &unknown)).unwrap();
    run("spectral_adjust", &BTreeMap::from([("sensor".to_string(), json!("LND08"))]), &[], "sa-named").unwrap();
}

#[test]
fn tool_parameters_are_checked() {
    let input = pixel_input(1, &fixtures().sensors);
    let run = |given: &[(&str, Value)]| {
        let given: BTreeMap<String, Value> = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let params = Params::with_defaults("tasseled_cap", &given).unwrap();
        run_pixel("tasseled_cap", &params, &input).unwrap_err()
    };
    assert!(run(&[("components", json!([]))]).contains("components is empty"));
    assert!(run(&[("components", json!(["brightness", "brightness"]))]).contains("twice"));
    assert!(run(&[("components", json!(["tc4"]))]).contains("unknown component"));
    assert!(run(&[("coefficients", json!("tm"))]).contains("coefficients"));
    // the bands unmapped, and no dates
    let mut bare = input.clone();
    bare.bands.remove("swir2");
    let params = Params::with_defaults("tasseled_cap", &BTreeMap::new()).unwrap();
    let r = run_pixel("tasseled_cap", &params, &bare).unwrap();
    assert_eq!(r.rows[0][1], "map the bands first: swir2");
    let mut undated = input.clone();
    undated.days = None;
    let r = run_pixel("tasseled_cap", &params, &undated).unwrap();
    assert_eq!(r.rows[0][1], "needs real dates");
}

#[test]
fn copy_python_calls_zeit() {
    let f = fixtures();
    let input = pixel_input(1, &f.sensors);
    let code = |tool: &str, given: &[(&str, Value)]| {
        let given: BTreeMap<String, Value> = given.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        let p = Params::with_defaults(tool, &given).unwrap();
        let (code, inputs) = calls(tool, &p, &input).unwrap_or_else(|| panic!("{tool}: no Python calls"));
        assert_eq!(inputs, vec!["bands_cube", "fmask"], "{tool}");
        code
    };
    let c =
        code("tasseled_cap", &[("components", json!(["wetness", "disturbance"])), ("coefficients", json!("sensor"))]);
    assert!(
        c.contains("zeit.tasseled_cap(clear_cube, ['wetness', 'disturbance'], coefficients='sensor', sensor=['LND07'"),
        "{c}"
    );
    assert!(c.contains("'sentinel-2'") && c.contains("bands={'blue': 'blue'") && c.contains("nodata=None"), "{c}");
    let c = code("tasseled_cap", &[]);
    assert!(c.contains("['brightness', 'greenness', 'wetness'], coefficients='crist1985', bands="), "{c}");
    assert!(!c.contains("sensor="), "{c}");
    let c = code("spectral_adjust", &[("sensor", json!("LND08")), ("force_search", json!(true))]);
    assert!(c.contains("zeit.spectral_adjust(clear_cube, sensor='LND08', bands={"), "{c}");
    assert!(c.contains("scale=10000.0, nodata=None, n_jobs=1, force_search=True)"), "{c}");
    let c = code("spectral_adjust", &[]);
    assert!(c.contains("sensor=['LND07'") && c.contains("force_search=False"), "{c}");
}

/// Copy Python's code run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio and xarray; the test is left out
/// without it) gives the tools' values: the code of each case on the cube and the Fmask as the snippets define them
/// (a raster run's, whole, and a pixel run's, one pixel).
#[test]
fn copy_python_runs_in_zeit_and_gives_the_tools_values() {
    let Ok(python) = std::env::var("JANUS_PYTHON") else {
        println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio and xarray");
        return;
    };
    let f = fixtures();
    let (px, py) = (3, 2);
    // (the case, the values zeit gave, the code)
    let mut cases: Vec<(String, &Values, String)> = vec![];
    for (k, c) in f.tasseled_cap.iter().enumerate() {
        let input = pixel_input(0, &f.sensors);
        let params = Params::with_defaults("tasseled_cap", &c.params).unwrap();
        cases.push((format!("tasseled_cap {k}"), &c.values, calls("tasseled_cap", &params, &input).unwrap().0));
    }
    for (k, c) in f.spectral_adjust.iter().enumerate() {
        let input = pixel_input(0, &f.landsat_sensors);
        let params = Params::with_defaults("spectral_adjust", &c.params).unwrap();
        cases.push((format!("spectral_adjust {k}"), &c.values, calls("spectral_adjust", &params, &input).unwrap().0));
    }
    let base = std::env::temp_dir().join("janus-zeit-l2-test-python");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let s = spec();
    let snippets = format!(
        "[{}]",
        cases.iter().map(|c| format!("({}, {})", py_str(&c.0), py_str(&c.2))).collect::<Vec<_>>().join(", ")
    );
    let script = format!(
        r#"import json, sys
import numpy as np
import rasterio
import xarray as xr
import zeit

roles = ["blue", "green", "red", "nir", "swir1", "swir2"]
data = {data}
days = {days}
bands = np.stack([rasterio.open(data + "/" + r + ".tif").read().astype(np.float64) for r in roles], axis=1)
qa = rasterio.open(data + "/qa.tif").read()
complete = np.all(np.isfinite(bands), axis=1)
fmask_all = np.where(complete, qa, 255).astype(np.int32)
time = (np.datetime64("1970-01-01") + np.array(days).astype("timedelta64[D]")).astype("datetime64[ns]")
snippets = {snippets}
out = {{}}
for name, code in snippets:
    for where, sel in (("raster", (slice(None), slice(None))), ("pixel", (slice({py}, {py} + 1), slice({px}, {px} + 1)))):
        y, x = sel
        cube = bands[:, :, y, x]
        bands_cube = xr.DataArray(cube, dims=("time", "band", "y", "x"), coords={{"time": time, "band": roles}})
        fmask = fmask_all[:, y, x]
        scope = dict(np=np, xr=xr, zeit=zeit, bands_cube=bands_cube, fmask=fmask)
        exec(code, scope)
        r = scope["result1"]
        out[name + " " + where] = [[[None if not np.isfinite(v) else float(v) for v in r.isel(time=k, band=b).values.ravel()]
                                    for k in range(r.sizes["time"])] for b in range(r.sizes["band"])]
json.dump(out, open(sys.argv[1], "w"))
"#,
        data = py_str(&data().to_string_lossy()),
        days = serde_json::to_string(&s.days).unwrap(),
    );
    let script_path = base.join("copy_python.py");
    std::fs::write(&script_path, script).unwrap();
    let result_path = base.join("result.json");
    // the PATH as an activated conda environment has it and Windows' own: the other DLLs on the test's PATH (the
    // GDAL builds, the libraries Janus is built with) shadow the environment's
    let root = Path::new(&python).parent().unwrap().to_path_buf();
    let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let mut path: Vec<String> = ["", "Library\\mingw-w64\\bin", "Library\\usr\\bin", "Library\\bin", "Scripts"]
        .iter()
        .map(|d| root.join(d).to_string_lossy().into_owned())
        .collect();
    path.extend([format!("{system}\\system32"), system.clone()]);
    let run = std::process::Command::new(python)
        .arg(&script_path)
        .arg(&result_path)
        .env_clear()
        .envs(["SystemRoot", "TEMP", "TMP"].iter().filter_map(|k| Some((*k, std::env::var(k).ok()?))))
        .env("PATH", path.join(";"))
        .env("PYTHONNOUSERSITE", "1")
        .env("OMP_NUM_THREADS", "4")
        .output()
        .unwrap();
    assert!(run.status.success(), "{}\n{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    let got: BTreeMap<String, Vec<Vec<Vec<Option<f64>>>>> =
        serde_json::from_str(&std::fs::read_to_string(&result_path).unwrap()).unwrap();
    let (mut values, mut worst) = (0, 0.0f64);
    for (name, want, _) in &cases {
        for (where_, pixels) in [("raster", None), ("pixel", Some(py * W + px))] {
            let g = &got[&format!("{name} {where_}")];
            assert_eq!(g.len(), want.len(), "{name} {where_}");
            for (gb, wb) in g.iter().zip(want.iter()) {
                for (gd, wd) in gb.iter().zip(wb) {
                    let wd: Vec<Option<f64>> = match pixels {
                        Some(p) => vec![wd[p]],
                        None => wd.clone(),
                    };
                    assert_eq!(gd.len(), wd.len(), "{name} {where_}");
                    for (a, b) in gd.iter().zip(&wd) {
                        assert!(close(a.unwrap_or(f64::NAN), *b, 1e-5), "{name} {where_}: {a:?} vs {b:?}");
                        if let (Some(a), Some(b)) = (a, b) {
                            worst = worst.max((a - b).abs() / b.abs().max(1.0));
                            values += 1;
                        }
                    }
                }
            }
        }
    }
    println!(
        "{} snippets run in Zeit, {values} values compared, the largest relative difference {worst:.2e}",
        cases.len()
    );
    assert!(values > 1000);
}

/// [band or component][date][pixel].
type Values = Vec<Vec<Vec<Option<f64>>>>;

/// A string as a Python literal.
fn py_str(s: &str) -> String {
    format!("{:?}", s)
}
