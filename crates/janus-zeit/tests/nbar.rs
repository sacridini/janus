//! NBAR (c-factor), a tool after the bridge: a raster run gives zeit.nbar's series (tests/data/nbar, written by
//! tools/nbar_fixtures.py: 12 x 16 pixels, 8 dates, the eleven bands with a BRDF role and the four angle bands, in
//! degrees and in hundredths of a degree, with pixels outside the model's domain, angles and reflectance without a
//! value and a date without any valid angle), one series per band (and per c-factor), a band per date; a pixel run
//! draws the same lines, with the pixel's own solar zenith as the scene's.

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
/// The roles with BRDF parameters, in the core's order (the order of the outputs).
const ROLES: [&str; 11] =
    ["coastal", "blue", "green", "red", "rededge1", "rededge2", "rededge3", "nir", "nir08", "swir1", "swir2"];
const ANGLES: [&str; 4] = ["sza", "saa", "vza", "vaa"];

/// [band][date][pixel], missing as null.
type Values = Vec<Vec<Vec<Option<f64>>>>;

#[derive(Deserialize)]
struct Outcome {
    values: Values,
    cfactor: Option<Values>,
}

#[derive(Deserialize)]
struct Case {
    params: BTreeMap<String, Value>,
    /// "deg" or "cd" (the angle files' unit)
    unit: String,
    roles: Vec<String>,
    #[serde(flatten)]
    whole: Outcome,
    /// by "x,y"
    pixels: BTreeMap<String, Outcome>,
}

#[derive(Deserialize)]
struct Fixtures {
    pixels: Vec<(usize, usize)>,
    cases: Vec<Case>,
}

fn data() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/nbar")
}

fn fixtures() -> Fixtures {
    serde_json::from_str(&std::fs::read_to_string(data().join("nbar.json")).unwrap()).unwrap()
}

fn abs(name: &str) -> String {
    data().join(name).to_string_lossy().to_string()
}

/// The file of a role's band (the angle bands in the case's unit).
fn file(role: &str, unit: &str) -> String {
    if ANGLES.contains(&role) { abs(&format!("{role}_{unit}.tif")) } else { abs(&format!("{role}.tif")) }
}

/// The series' spec with the bands of `roles` and the angle bands of `unit` mapped.
fn spec(roles: &[String], unit: &str, params: &BTreeMap<String, Value>) -> JobSpec {
    let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(data().join("spec.json")).unwrap()).unwrap();
    spec.tool = "nbar".into();
    spec.input = abs("input.tif");
    spec.params = params.clone();
    for r in roles.iter().map(String::as_str).chain(ANGLES) {
        spec.bands.insert(r.to_string(), file(r, unit));
    }
    spec
}

/// A raster's bands as [band][pixels] (NoData as NaN) and their descriptions.
fn read(path: &str) -> (Vec<Vec<f64>>, Vec<String>) {
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

/// `got` is `want` to 1e-6 of its size, or both are missing.
fn close(got: f64, want: Option<f64>) -> bool {
    match want {
        Some(w) => got.is_finite() && (got - w).abs() <= 1e-6 * w.abs() + 1e-12,
        None => !got.is_finite(),
    }
}

fn run(spec: &mut JobSpec, name: &str) -> Result<JobResult, String> {
    let out = std::env::temp_dir().join(format!("janus-zeit-nbar-test-{name}"));
    let _ = std::fs::remove_dir_all(&out);
    spec.output_dir = out.to_string_lossy().to_string();
    run_job(spec, &|_, _| {}, &AtomicBool::new(false))
}

/// The message a run stops with.
fn fails(spec: &mut JobSpec, name: &str) -> String {
    match run(spec, name) {
        Ok(_) => panic!("the run of {name} was accepted"),
        Err(e) => e,
    }
}

/// A series output is zeit's (a band per date, the pixels in reading order), written as float32; its largest
/// relative difference.
fn assert_series(path: &str, want: &[Vec<Option<f64>>], what: &str) -> (usize, f64) {
    let (bands, _) = read(path);
    assert_eq!(bands.len(), want.len(), "{what}");
    let (mut present, mut worst) = (0, 0.0f64);
    for (k, (got, want)) in bands.iter().zip(want).enumerate() {
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            // written as float32
            let w = w.map(|w| f64::from(w as f32));
            assert!(close(*g, w), "{what} date {k} pixel {i}: {g} vs {w:?}");
            if let Some(w) = w {
                present += 1;
                worst = worst.max((g - w).abs() / w.abs().max(1e-12));
            }
        }
    }
    assert!(present > 0, "{what}: no value to compare");
    (present, worst)
}

fn output<'a>(result: &'a JobResult, id: &str) -> &'a Value {
    let o = result.outputs.iter().find(|o| o["id"] == id).unwrap_or_else(|| panic!("no output {id}"));
    assert_eq!(o["series"], json!(true), "{id}");
    o
}

/// The roles of `roles` in the core's order, as the outputs follow them.
fn in_core_order(roles: &[String]) -> Vec<&'static str> {
    ROLES.iter().copied().filter(|r| roles.iter().any(|x| x == r)).collect()
}

#[test]
fn raster_run_is_zeits() {
    let f = fixtures();
    let (mut values, mut worst) = (0, 0.0f64);
    for (k, c) in f.cases.iter().enumerate() {
        let mut s = spec(&c.roles, &c.unit, &c.params);
        let result = run(&mut s, &format!("raster-{k}")).unwrap();
        let ordered = in_core_order(&c.roles);
        let cfactor = c.params["cfactor"] == json!(true);
        // one series per band, in the core's order, and the c-factors after them when asked
        let mut want: Vec<String> = ordered.iter().map(|r| format!("nbar_{r}")).collect();
        if cfactor {
            want.extend(ordered.iter().map(|r| format!("cfactor_{r}")));
        }
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, want, "case {k}");
        for role in &ordered {
            let m = c.roles.iter().position(|r| r == role).unwrap();
            let what = format!("nbar case {k} {role}");
            let o = output(&result, &format!("nbar_{role}"));
            assert_eq!(o["name"], json!(format!("NBAR {role}")));
            let (n, w) = assert_series(o["path"].as_str().unwrap(), &c.whole.values[m], &what);
            values += n;
            worst = worst.max(w);
            if cfactor {
                let o = output(&result, &format!("cfactor_{role}"));
                assert_eq!(o["name"], json!(format!("c-factor {role}")));
                let (n, w) = assert_series(o["path"].as_str().unwrap(), &c.whole.cfactor.as_ref().unwrap()[m], &what);
                values += n;
                worst = worst.max(w);
            }
        }
    }
    println!("{values} values compared with zeit.nbar, the largest relative difference {worst:.2e}");
}

/// The pixels with gaps, outside the model's domain, and the date without any angle.
fn pixel_input(p: usize, roles: &[String], unit: &str) -> PixelInput {
    let s = spec(roles, unit, &BTreeMap::new());
    let (shown, _) = read(&s.input);
    let bands: BTreeMap<String, Vec<f64>> =
        s.bands.iter().map(|(role, path)| (role.clone(), read(path).0.iter().map(|b| b[p]).collect())).collect();
    PixelInput {
        years: s.years,
        days: s.days,
        values: shown.iter().map(|b| b[p]).collect(),
        bands,
        ..Default::default()
    }
}

#[test]
fn pixel_run_is_zeits() {
    let f = fixtures();
    for (k, c) in f.cases.iter().enumerate() {
        let params = Params::with_defaults("nbar", &c.params).unwrap();
        let ordered = in_core_order(&c.roles);
        for &(x, y) in &f.pixels {
            let want = &c.pixels[&format!("{x},{y}")];
            let r = run_pixel("nbar", &params, &pixel_input(y * W + x, &c.roles, &c.unit)).unwrap();
            assert_eq!(r.overlays.len(), ordered.len(), "case {k} pixel ({x}, {y})");
            for (o, role) in r.overlays.iter().zip(&ordered) {
                let m = c.roles.iter().position(|r| r == role).unwrap();
                assert_eq!(o.label, format!("NBAR {role}"));
                let valid: Vec<f64> = want.values[m].iter().filter_map(|d| d[0]).collect();
                assert_eq!(o.y.len(), valid.len(), "case {k} {role} pixel ({x}, {y})");
                for (g, w) in o.y.iter().zip(&valid) {
                    // the maps are float32, the chart's values are not rounded to it
                    assert!(close(g.unwrap(), Some(*w)), "case {k} {role} pixel ({x}, {y}): {g:?} vs {w}");
                }
                if let Some(cf) = &want.cfactor {
                    let finite: Vec<f64> = cf[m].iter().filter_map(|d| d[0]).collect();
                    let row = r.rows.iter().find(|row| row[0] == format!("Mean c-factor ({role})")).unwrap();
                    match finite.len() {
                        0 => assert_eq!(row[1], "no valid angles"),
                        n => {
                            let mean = finite.iter().sum::<f64>() / n as f64;
                            let shown: f64 = row[1].parse().unwrap();
                            assert!((shown - mean).abs() <= 1e-5 * mean.abs(), "{} vs {mean}", row[1]);
                        }
                    }
                }
            }
            let rows: BTreeMap<&str, &str> = r.rows.iter().map(|[a, b]| (a.as_str(), b.as_str())).collect();
            let valid = want.values[0].iter().filter(|d| d[0].is_some()).count();
            assert_eq!(rows["Valid observations"], format!("{valid} of 8"));
            if c.params["sza"] == "fixed" {
                assert!(rows["Normalized solar zenith"].ends_with("degrees"), "{rows:?}");
            } else if valid > 0 {
                assert!(rows["Normalized solar zenith"].contains("own"), "{rows:?}");
            } else {
                // a pixel outside the model's domain on every date has no solar zenith to be brought to
                assert_eq!(rows["Normalized solar zenith"], "no date with a valid solar zenith");
            }
        }
    }
}

#[test]
fn a_pixel_run_on_a_scene_is_the_pixels_own_sun() {
    // the date's scene mean needs the window: on the chart each date goes to its own solar zenith, which is
    // what zeit.nbar gives a 1 x 1 cube (the fixtures' pixel references); the scene's values differ
    let f = fixtures();
    let c = &f.cases[0];
    let (x, y) = (3, 2);
    let own = &c.pixels[&format!("{x},{y}")];
    let scene = &c.whole;
    let (a, b) = (own.values[3][0][0].unwrap(), scene.values[3][0][y * W + x].unwrap());
    assert!((a - b).abs() > 1e-4 * b.abs(), "the pixel's own sun and the scene's gave the same value");
}

#[test]
fn the_tool_stops_with_a_message_when_something_is_not_mapped() {
    let f = fixtures();
    let c = &f.cases[0];
    // no band to normalize: the angle bands alone
    let mut s = spec(&[], &c.unit, &c.params);
    let e = fails(&mut s, "no-bands");
    assert!(e.contains("map at least one band to normalize"), "{e}");
    let params = Params::with_defaults("nbar", &c.params).unwrap();
    let mut input = pixel_input(1, &c.roles, &c.unit);
    input.bands.retain(|r, _| ANGLES.contains(&r.as_str()));
    let r = run_pixel("nbar", &params, &input).unwrap();
    assert!(r.overlays.is_empty() && r.rows[0][1].starts_with("map at least one band"), "{:?}", r.rows);
    // an angle band missing
    let mut s = spec(&c.roles, &c.unit, &c.params);
    s.bands.remove("vaa");
    let e = fails(&mut s, "no-vaa");
    assert!(e.contains("map the angle bands first: vaa"), "{e}");
    let mut input = pixel_input(1, &c.roles, &c.unit);
    input.bands.remove("sza");
    let r = run_pixel("nbar", &params, &input).unwrap();
    assert_eq!(r.rows[0][1], "map the angle bands first: sza");
    // a fixed zenith outside 0-90 degrees
    let bad = BTreeMap::from([("sza".to_string(), json!("fixed")), ("sza_fixed".to_string(), json!(95.0))]);
    let mut s = spec(&c.roles, &c.unit, &bad);
    let e = fails(&mut s, "bad-sza");
    assert!(e.contains("must be 0 to 90 degrees"), "{e}");
    // the angles read as degrees when they are hundredths: every pixel is outside the model's domain
    let wrong = BTreeMap::from([("angle_scale".to_string(), json!("degrees"))]);
    let mut s = spec(&["nir".to_string()], "cd", &wrong);
    let result = run(&mut s, "wrong-unit").unwrap();
    let (bands, _) = read(output(&result, "nbar_nir")["path"].as_str().unwrap());
    assert!(bands.iter().all(|b| b.iter().all(|v| v.is_nan())));
}

#[test]
fn the_roles_the_run_maps_are_its_outputs() {
    // a subset of the optional bands, mapped in no particular order: the outputs follow the core's
    let mut s = spec(&["swir2".to_string(), "blue".to_string(), "nir08".to_string()], "deg", &BTreeMap::new());
    let result = run(&mut s, "outputs").unwrap();
    let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["nbar_blue", "nbar_nir08", "nbar_swir2"]);
}

#[test]
fn copy_python_calls_zeit() {
    let f = fixtures();
    let code = |c: &Case| {
        let p = Params::with_defaults("nbar", &c.params).unwrap();
        let (code, inputs) = calls("nbar", &p, &pixel_input(1, &c.roles, &c.unit)).unwrap();
        assert_eq!(inputs, vec!["nbar_cube"]);
        code
    };
    let c = code(&f.cases[0]);
    assert!(c.starts_with("angles = xr.Dataset({a.upper(): nbar_angles.sel(band=a, drop=True) for a in nbar_angles.band.values})  # the angle bands, in degrees\nresult1 = zeit.nbar(nbar_cube, angles, sza='scene', bands={'coastal': 'coastal', 'blue': 'blue'"), "{c}");
    assert!(c.ends_with("nodata=None, n_jobs=1, return_cfactor=False)"), "{c}");
    let c = code(&f.cases[1]);
    assert!(c.starts_with("angles = xr.Dataset({a.upper(): nbar_angles.sel(band=a, drop=True) * 0.01 for a in nbar_angles.band.values})  # the angle bands, in degrees\nresult1, result2 = zeit.nbar(nbar_cube, angles, sza=32.5, bands={'blue': 'blue'"), "{c}");
    assert!(c.ends_with("return_cfactor=True)"), "{c}");
    // the bands in the core's order, the angle bands not among them
    let c = code(&f.cases[2]);
    assert!(c.contains("bands={'red': 'red', 'rededge2': 'rededge2', 'nir08': 'nir08'}"), "{c}");
}

/// A string as a Python literal.
fn py_str(s: &str) -> String {
    format!("{:?}", s)
}

/// Copy Python's code run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio and xarray; the test is left out
/// without it) gives the tool's values: the code of each case on the cube and the angles as the snippets define
/// them (a raster run's, whole, and a pixel run's, one pixel).
#[test]
fn copy_python_runs_in_zeit_and_gives_the_tools_values() {
    let Ok(python) = std::env::var("JANUS_PYTHON") else {
        println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio and xarray");
        return;
    };
    let f = fixtures();
    let (px, py) = (3, 2);
    let s = spec(&[], "deg", &BTreeMap::new());
    let snippets: Vec<String> = f
        .cases
        .iter()
        .map(|c| {
            let p = Params::with_defaults("nbar", &c.params).unwrap();
            let code = calls("nbar", &p, &pixel_input(0, &c.roles, &c.unit)).unwrap().0;
            let cf = if c.params["cfactor"] == json!(true) { "True" } else { "False" };
            format!("({}, {}, {}, {cf})", py_str(&c.unit), serde_json::to_string(&c.roles).unwrap(), py_str(&code))
        })
        .collect();
    let script = format!(
        r#"import json, sys
import numpy as np
import rasterio
import xarray as xr
import zeit

data = {data}
days = {days}
roles_all = {roles}
time = (np.datetime64("1970-01-01") + np.array(days).astype("timedelta64[D]")).astype("datetime64[ns]")


def band(name):
    src = rasterio.open(data + "/" + name + ".tif")
    a = src.read().astype(np.float64)
    if src.nodata is not None and not np.isnan(src.nodata):
        a[a == src.nodata] = np.nan
    return a


raw_all = {{n: band(n) for n in roles_all + [a + u for a in ("sza", "saa", "vza", "vaa") for u in ("_deg", "_cd")]}}
out = {{}}
for case, (unit, roles, code, cfactor) in enumerate([{snippets}]):
    for where, sel in (("raster", (slice(None), slice(None))), ("pixel", (slice({py}, {py} + 1), slice({px}, {px} + 1)))):
        y, x = sel
        bands = [r for r in roles_all if r in roles]
        raw = {{r: raw_all[r][:, y, x] for r in bands}}
        for a in ("sza", "saa", "vza", "vaa"):
            raw[a] = raw_all[a + "_" + unit][:, y, x]
        nbar_cube = xr.DataArray(np.stack([raw[r] for r in bands], axis=1), dims=("time", "band", "y", "x"),
                                 coords={{"time": time, "band": bands}})
        angle_names = ["sza", "saa", "vza", "vaa"]
        nbar_angles = xr.DataArray(np.stack([raw[a] for a in angle_names], axis=1), dims=("time", "band", "y", "x"),
                                   coords={{"time": time, "band": angle_names}})
        scope = dict(np=np, xr=xr, zeit=zeit, nbar_cube=nbar_cube, nbar_angles=nbar_angles)
        exec(code, scope)
        results = [scope["result1"]] + ([scope["result2"]] if cfactor else [])
        out[f"{{case}} {{where}}"] = [[[[None if not np.isfinite(v) else float(v) for v in r.sel(band=b).isel(time=k).values.ravel()]
                                      for k in range(r.sizes["time"])] for b in bands] for r in results]
json.dump(out, open(sys.argv[1], "w"))
"#,
        data = py_str(&data().to_string_lossy()),
        days = serde_json::to_string(&s.days).unwrap(),
        roles = serde_json::to_string(&ROLES).unwrap(),
        snippets = snippets.join(", "),
    );
    let base = std::env::temp_dir().join("janus-zeit-nbar-test-python");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
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
    // [result][band][date][pixel]
    let got: BTreeMap<String, Vec<Values>> =
        serde_json::from_str(&std::fs::read_to_string(&result_path).unwrap()).unwrap();
    let (mut values, mut worst) = (0, 0.0f64);
    for (k, c) in f.cases.iter().enumerate() {
        // the bands of the run, in the core's order, as the snippet's cube has them
        let ordered = in_core_order(&c.roles);
        for (where_, outcome) in [("raster", &c.whole), ("pixel", &c.pixels[&format!("{px},{py}")])] {
            let g = &got[&format!("{k} {where_}")];
            let mut wanted: Vec<&Values> = vec![&outcome.values];
            wanted.extend(outcome.cfactor.as_ref());
            assert_eq!(g.len(), wanted.len(), "case {k} {where_}");
            for (gr, wr) in g.iter().zip(wanted) {
                assert_eq!(gr.len(), ordered.len());
                for (gb, role) in gr.iter().zip(&ordered) {
                    let wb = &wr[c.roles.iter().position(|r| r == role).unwrap()];
                    assert_eq!(gb.len(), wb.len());
                    for (gd, wd) in gb.iter().zip(wb) {
                        assert_eq!(gd.len(), wd.len(), "case {k} {where_} {role}");
                        for (a, b) in gd.iter().zip(wd) {
                            assert!(close(a.unwrap_or(f64::NAN), *b), "case {k} {where_} {role}: {a:?} vs {b:?}");
                            if let (Some(a), Some(b)) = (a, b) {
                                worst = worst.max((a - b).abs() / b.abs().max(1e-12));
                                values += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    println!(
        "{} snippets run in Zeit, {values} values compared, the largest relative difference {worst:.2e}",
        f.cases.len() * 2
    );
    assert!(values > 1000);
}
