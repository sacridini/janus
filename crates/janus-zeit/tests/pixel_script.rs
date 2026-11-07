//! "Copy Python" of a pin and of an ROI (`janus_zeit::python::pixel_script`): the script reads the series with Zeit's
//! `load_raster` and makes the tools' calls. The text is checked here; with `JANUS_PYTHON` naming a Python that has
//! zeit-cdts the scripts are run too, and the series they read is compared with what Janus shows.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gdal::raster::Buffer;
use gdal::{DriverManager, Metadata};
use janus_cube::{BandSelection, CubeInfo};
use janus_zeit::Params;
use janus_zeit::python::{Roi, Spec, Target, pixel_script};

const W: usize = 6;
const H: usize = 5;
const NAMES: [&str; 7] = ["Blue", "Green", "Red", "NIR", "SWIR1", "SWIR2", "QA_PIXEL"];

fn dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("janus-zeit-pixel-script-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A GeoTIFF of `bands` (each W x H), their descriptions, scale and offset, 0 as no data.
fn write(path: &Path, bands: &[Vec<u16>], names: &[String], scale: f64, offset: f64) {
    let driver = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = driver.create_with_band_type::<u16, _>(path, W, H, bands.len()).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 9000000.0, 0.0, -30.0]).unwrap();
    // Zeit's reader cuts a window by the raster's coordinates, which it needs a CRS for
    ds.set_projection(&gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap().to_wkt().unwrap()).unwrap();
    for (k, v) in bands.iter().enumerate() {
        let mut b = ds.rasterband(k + 1).unwrap();
        b.set_no_data_value(Some(0.0)).unwrap();
        b.set_description(&names[k]).unwrap();
        if names[k] != "QA_PIXEL" {
            b.set_scale(scale).unwrap();
            b.set_offset(offset).unwrap();
        }
        b.write((0, 0), (W, H), &mut Buffer::new((W, H), v.clone())).unwrap();
    }
}

/// The digital number of band `b` (0 Blue..5 SWIR2) of date `t` at a pixel.
fn dn(b: usize, t: usize, x: usize, y: usize) -> u16 {
    (9000 + 1500 * b + 400 * t + 37 * x + 11 * y) as u16
}

/// QA_PIXEL: clear (21824), or a cloud (bit 3) on the second date and a fill (bit 0) on the third one's first pixel.
fn qa(t: usize, x: usize, y: usize) -> u16 {
    match (t, x, y) {
        (1, _, _) if x.is_multiple_of(2) => 21824 | 8,
        (2, 0, 0) => 21824 | 1,
        _ => 21824,
    }
}

const DATES: [&str; 3] = ["2020-01-05", "2020-02-06", "2020-03-09"];

/// A folder of Landsat-like files, one a date: Blue..SWIR2 in reflectance scale (2.75e-5, -0.2) and QA_PIXEL.
fn landsat_folder(name: &str) -> PathBuf {
    let d = dir(name);
    for (t, date) in DATES.iter().enumerate() {
        let bands: Vec<Vec<u16>> = (0..7)
            .map(|b| (0..W * H).map(|i| if b < 6 { dn(b, t, i % W, i / W) } else { qa(t, i % W, i / W) }).collect())
            .collect();
        let names: Vec<String> = NAMES.iter().map(|s| s.to_string()).collect();
        write(&d.join(format!("ls_{date}.tif")), &bands, &names, 2.75e-5, -0.2);
    }
    d
}

fn open(paths: &[PathBuf], sel: &BandSelection) -> CubeInfo {
    let p: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    janus_cube::open_cube(&p, sel).unwrap()
}

fn roles(info: &CubeInfo) -> BTreeMap<String, i32> {
    janus_cube::roles::guess_band_roles(info).into_iter().map(|(r, b)| (r.to_string(), b)).collect()
}

fn tool(id: &str) -> (String, Params) {
    (id.to_string(), Params::with_defaults(id, &BTreeMap::new()).unwrap())
}

fn script(info: &CubeInfo, target: Target, tools: &[(String, Params)]) -> String {
    let roles = roles(info);
    let path = |p: &str| p.replace('\\', "/");
    let spec = Spec {
        info,
        version: "test",
        name: "series",
        target,
        roles: &roles,
        tools,
        path: &path,
        other: None,
        chain: &[],
    };
    pixel_script(&spec).unwrap()
}

fn pin(col: i64, row: i64) -> Target {
    Target::Pin { id: 1, col, row }
}

/// Runs a script with the Python of JANUS_PYTHON and returns what it prints after `MARK`; None if there is none.
fn run(code: &str, name: &str) -> Option<Vec<f64>> {
    let python = std::env::var("JANUS_PYTHON").ok()?;
    let d = dir(&format!("run-{name}"));
    let file = d.join("snippet.py");
    let text = format!(
        "{code}\nimport json\nprint(\"MARK \" + json.dumps([None if v != v else float(v) for v in series.values.ravel()]))\n"
    );
    std::fs::write(&file, text).unwrap();
    let out = std::process::Command::new(python).arg("-I").arg(&file).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find_map(|l| l.strip_prefix("MARK "))
        .unwrap_or_else(|| panic!("the script should run:\n{stdout}\n{}", String::from_utf8_lossy(&out.stderr)));
    let v: Vec<Option<f64>> = serde_json::from_str(line).unwrap();
    Some(v.into_iter().map(|x| x.unwrap_or(f64::NAN)).collect())
}

fn same(got: &[f64], want: &[f64]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert!((g.is_nan() && w.is_nan()) || (g - w).abs() < 1e-6, "{got:?} vs {want:?}");
    }
}

#[test]
fn an_annual_stack_is_read_by_zeit_and_the_tool_is_called() {
    let d = dir("annual");
    let bands: Vec<Vec<u16>> = (0..10)
        .map(|t| (0..W * H).map(|i| (5000 + 300 * t + 7 * i) as u16 + if t > 5 { 2000 } else { 0 }).collect())
        .collect();
    let names: Vec<String> = (0..10).map(|t| format!("yr{}", 1990 + t)).collect();
    let path = d.join("annual.tif");
    write(&path, &bands, &names, 1.0, 0.0);
    let info = open(&[path], &BandSelection::default());
    let code = script(&info, pin(2, 3), &[tool("landtrendr")]);
    println!("{code}");
    assert!(code.contains("series = zeit.load_raster("), "{code}");
    assert!(code.contains("masked=True") && !code.contains("clip="), "a pin needs no window:\n{code}");
    assert!(code.contains("pixel=(2, 3)") && !code.contains(".sel(") && !code.contains("chunks="), "{code}");
    assert!(!code.contains("scale=True"), "a file without scale needs none:\n{code}");
    assert!(code.contains("fit = zeit.landtrendr("), "{code}");
    // Zeit's defaults are left out
    assert!(!code.contains("max_segments=6") && !code.contains("pval_threshold"), "{code}");
    assert!(code.contains("events = zeit.extract_events("), "{code}");
    // Zeit's defaults need no help: no rasterio, no numpy, none of the plumbing
    for gone in ["rasterio", "import numpy", "nodata=None", "n_jobs=1", "Window("] {
        assert!(!code.contains(gone), "{gone} in\n{code}");
    }
    assert!(code.lines().count() < 25, "{code}");
    // the pixel: column 2, row 3
    if let Some(v) = run(&code, "annual") {
        let want: Vec<f64> = (0..10)
            .map(|t| f64::from((5000 + 300 * t + 7 * (3 * W + 2)) as u16 + if t > 5 { 2000 } else { 0 }))
            .collect();
        same(&v, &want);
    }
}

#[test]
fn a_folder_of_landsat_dates_gives_the_normalized_difference_under_the_quality_band() {
    let d = landsat_folder("nd");
    let info = open(std::slice::from_ref(&d), &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    assert_eq!(info.sel.qa_band, 7, "the quality band is found by its name");
    let code = script(&info, pin(2, 1), &[tool("mann_kendall")]);
    println!("{code}");
    assert!(!code.contains("ls_2020"), "the folder, not each file:\n{code}");
    // one read: the two bands, scaled and masked by the quality band (band 7), the pixel only
    assert_eq!(code.matches("zeit.load_raster(").count(), 1, "{code}");
    assert!(code.contains("bands={\"nir\": 4, \"red\": 3}") && code.contains("qa=7"), "{code}");
    assert!(code.contains("scale=True") && code.contains("qa_rule=\"landsat_c2\""), "{code}");
    assert!(code.contains("pixel=(2, 1)"), "{code}");
    assert!(code.contains("zeit.normalized_difference(cube, \"nir\", \"red\")"), "{code}");
    assert!(code.contains("\"zeit-cdts>=3.2\""), "{code}");
    assert!(code.lines().count() <= 14, "{code}");
    assert!(code.contains("result = zeit.mann_kendall("), "{code}");
    if let Some(v) = run(&code, "nd") {
        let want: Vec<f64> = (0..3)
            .map(|t| {
                let r = |b: usize| f64::from(dn(b, t, 2, 1)) * 2.75e-5 - 0.2;
                if qa(t, 2, 1) & 0x3B != 0 { f64::NAN } else { (r(3) - r(2)) / (r(3) + r(2)) }
            })
            .collect();
        same(&v, &want);
        assert!(v[1].is_nan(), "the cloud of the second date is masked");
    }
}

#[test]
fn a_band_of_a_date_that_is_not_the_first_is_read_for_every_date_in_a_stack() {
    // 3 dates x 7 bands in one file, the dates in a side-car CSV: date t's band b is band 7 t + b + 1
    let d = dir("stack");
    let mut bands: Vec<Vec<u16>> = vec![];
    let mut names: Vec<String> = vec![];
    for t in 0..3 {
        for (b, name) in NAMES.iter().enumerate() {
            bands.push((0..W * H).map(|i| if b < 6 { dn(b, t, i % W, i / W) } else { qa(t, i % W, i / W) }).collect());
            names.push(name.to_string());
        }
    }
    let path = d.join("stack.tif");
    write(&path, &bands, &names, 2.75e-5, -0.2);
    std::fs::write(d.join("stack_dates.csv"), format!("Date\n{}\n", DATES.join("\n"))).unwrap();
    let info = open(&[path], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    assert_eq!(info.bands_per_date, 7);
    let code = script(&info, pin(1, 1), &[tool("ccdc")]);
    println!("{code}");
    assert!(code.contains("cube = zeit.load_raster("), "{code}");
    assert!(code.contains("cube.isel(band=[0, 1, 2, 3, 4, 5])"), "the bands are those of each date:\n{code}");
    // Zeit's own codes, units and missing bands: no cascade, no median, no fillna
    assert!(code.contains("qa=zeit.qa_to_fmask(qa, \"landsat_c2\"), scale='auto'"), "{code}");
    assert!(!code.contains("fillna") && !code.contains("median") && !code.contains("complete"), "{code}");
    assert!(code.contains("zeit-cdts>=3.2.1"), "{code}");
    run_script(&code, "stack-ccdc"); // three dates: no segment, but the shapes of the pixel's cube are Zeit's
    // the folder of the same dates gives the same bands, read file by file
    let f = landsat_folder("stack-folder");
    let info = open(&[f], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    let code = script(&info, pin(1, 1), &[tool("ccdc")]);
    let roles = "bands={\"blue\": 1, \"green\": 2, \"red\": 3, \"nir\": 4, \"swir1\": 5, \"swir2\": 6}";
    assert!(code.contains(roles), "{code}");
    assert!(code.contains("qa = zeit.load_raster(src, band=7") && code.contains("zeit.qa_to_fmask("), "{code}");
    run_script(&code, "folder-ccdc");
    // digital numbers without a scale in the files may be Landsat's (Zeit's auto does not tell them): the units are
    // tested in the script, as Janus does
    let bare = d.join("bare.tif");
    write(&bare, &bands, &names, 1.0, 0.0);
    std::fs::write(d.join("bare_dates.csv"), format!("Date\n{}\n", DATES.join("\n"))).unwrap();
    let info = open(&[bare], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    let code = script(&info, pin(1, 1), &[tool("ccdc")]);
    assert!(code.contains("median = float(") && code.contains("2.75e-05") && !code.contains("scale='auto'"), "{code}");
    run_script(&code, "bare-ccdc");
}

#[test]
fn an_roi_is_the_mean_of_the_cells_inside() {
    let d = landsat_folder("roi");
    let info = open(&[d], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    let rect = Roi { x0: 1, y0: 1, x1: 5, y1: 4, ..Default::default() };
    let code = script(&info, Target::Roi(rect), &[tool("mann_kendall")]);
    println!("{code}");
    assert!(code.contains(".mean([\"y\", \"x\"])") && !code.contains("compute()"), "{code}");
    assert!(code.contains("window=(1, 1, 5, 4)") && !code.contains("clip="), "{code}");
    if let Some(v) = run(&code, "roi") {
        let want: Vec<f64> = (0..3)
            .map(|t| {
                let mut vals = vec![];
                for y in 1..4 {
                    for x in 1..5 {
                        let r = |b: usize| f64::from(dn(b, t, x, y)) * 2.75e-5 - 0.2;
                        if qa(t, x, y) & 0x3B == 0 {
                            vals.push((r(3) - r(2)) / (r(3) + r(2)));
                        }
                    }
                }
                if vals.is_empty() { f64::NAN } else { vals.iter().sum::<f64>() / vals.len() as f64 }
            })
            .collect();
        same(&v, &want);
    }
    // a polygon: its rings in the files' coordinates, a hole as a ring inside another
    let poly = Roi {
        x0: 0,
        y0: 0,
        x1: 6,
        y1: 5,
        rings: vec![vec![0.0, 0.0, 6.0, 0.0, 6.0, 5.0, 0.0, 5.0], vec![2.0, 1.0, 4.0, 1.0, 4.0, 3.0, 2.0, 3.0]],
        ..Default::default()
    };
    let info = open(&[landsat_folder("roi2")], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    let code = script(&info, Target::Roi(poly), &[]);
    println!("{code}");
    assert!(code.contains("polygon = {\"type\": \"Polygon\"") && code.contains("clip=[polygon]"), "{code}");
    assert!(code.contains("print(series.squeeze().to_series())"), "no tool: the series itself\n{code}");
    if let Some(v) = run(&code, "poly") {
        // the 2 x 2 cells of the hole (columns 2 and 3, rows 1 and 2) are outside
        let want: Vec<f64> = (0..3)
            .map(|t| {
                let mut vals = vec![];
                for y in 0..5 {
                    for x in 0..6 {
                        if (2..4).contains(&x) && (1..3).contains(&y) {
                            continue;
                        }
                        let r = |b: usize| f64::from(dn(b, t, x, y)) * 2.75e-5 - 0.2;
                        if qa(t, x, y) & 0x3B == 0 {
                            vals.push((r(3) - r(2)) / (r(3) + r(2)));
                        }
                    }
                }
                vals.iter().sum::<f64>() / vals.len() as f64
            })
            .collect();
        same(&v, &want);
    }
}

#[test]
fn every_tool_that_had_a_pin_script_still_has_one() {
    let d = dir("tools");
    let bands: Vec<Vec<u16>> = (0..30).map(|t| (0..W * H).map(|i| (5000 + 100 * t + i) as u16).collect()).collect();
    let names: Vec<String> = (0..30).map(|t| format!("2019-{:02}-{:02}", 1 + t / 3, 1 + 10 * (t % 3))).collect();
    let path = d.join("series.tif");
    write(&path, &bands, &names, 1e-4, 0.0);
    let info = open(&[path], &BandSelection::default());
    for id in [
        "landtrendr",
        "recovery",
        "mann_kendall",
        "ar_trend",
        "smooth",
        "bfast",
        "bfast_lite",
        "bfast_monitor",
        "phenology",
        "postproc",
        "ewmacd",
        "beast",
        "interpolate",
        "temporal_metrics",
        "fold",
        "standardize",
        "linear_trend",
        "cat",
        "clear_observations",
        "polar_metrics",
    ] {
        let code = script(&info, pin(1, 1), &[tool(id)]);
        assert!(code.contains("zeit.load_raster(") && code.contains("zeit."), "{id}:\n{code}");
        assert!(!code.contains("nodata=None") && !code.contains("n_jobs=1"), "{id}:\n{code}");
        // the series is the one pixel Zeit reads: every tool takes it, but those that want annual or evenly
        // spaced dates, which this series (three dates a month, every ten days) is not
        if let Ok(python) = std::env::var("JANUS_PYTHON") {
            let file = dir(&format!("script-tool-{id}")).join("rerun.py");
            std::fs::write(&file, &code).unwrap();
            let out = std::process::Command::new(python).arg("-I").arg(&file).output().unwrap();
            let err = String::from_utf8_lossy(&out.stderr);
            let dates = ["landtrendr", "recovery", "polar_metrics"].contains(&id);
            assert!(out.status.success() || dates, "{id}:\n{code}\n{err}");
        }
    }
}

#[test]
fn the_calls_in_full_keep_the_plumbing_a_pin_script_leaves_out() {
    // `calls` is what a raster run's script and the bridge's recordings have; the pin's script shows the same
    // call without `nodata=None, n_jobs=1` (it reads with masked=True and a pixel is one job)
    let (name, p) = tool("landtrendr");
    let input = janus_zeit::PixelInput {
        years: vec![2000.0, 2001.0],
        days: Some(vec![10957, 11323]),
        values: vec![0.1, 0.2],
        ..Default::default()
    };
    let (full, _) = janus_zeit::python::calls(&name, &p, &input).unwrap();
    assert!(full.contains("zeit.landtrendr(series, nodata=None, n_jobs=1, direction="), "{full}");
    let call = janus_zeit::python::tool_call(&name, &p, &input).unwrap();
    assert_eq!(call.full(), full);
    let (compact, shown) = call.compact("");
    assert!(!compact.contains("nodata") && !compact.contains("n_jobs"), "{compact}");
    assert_eq!(shown, vec!["fit", "events"]);
    assert!(compact.contains("events = zeit.extract_events("), "{compact}");
}

#[test]
fn an_roi_has_no_script_for_the_tools_of_one_pixels_bands() {
    let info = open(&[landsat_folder("roi-ccdc")], &BandSelection { band: 4, nd_band: 3, ..Default::default() });
    let roi = Roi { x0: 1, y0: 1, x1: 5, y1: 4, ..Default::default() };
    let code = script(&info, Target::Roi(roi), &[tool("ccdc"), tool("mann_kendall")]);
    assert!(code.contains("# CCDC: its bands are a pin's") && !code.contains("zeit.ccdc("), "{code}");
    assert!(code.contains("zeit.mann_kendall("), "{code}");
}

fn run_target(window: [usize; 4], out: &Path) -> Target {
    Target::Run { window, out_dir: out.to_string_lossy().replace('\\', "/"), what: "the run of a test".into() }
}

/// Runs a script with the Python of JANUS_PYTHON; false if there is none.
fn run_script(code: &str, name: &str) -> bool {
    let Ok(python) = std::env::var("JANUS_PYTHON") else { return false };
    let file = dir(&format!("script-{name}")).join("rerun.py");
    std::fs::write(&file, code).unwrap();
    let out = std::process::Command::new(python).arg("-I").arg(&file).output().unwrap();
    assert!(
        out.status.success(),
        "{code}\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    true
}

fn read_band(path: &Path, band: usize) -> (usize, usize, Vec<f32>) {
    let ds = gdal::Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    let v = ds.rasterband(band).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
    (w, h, v.data().to_vec())
}

#[test]
fn a_run_reads_its_window_with_zeit_and_writes_the_maps_with_zeit() {
    let d = dir("window-annual");
    let bands: Vec<Vec<u16>> = (0..10)
        .map(|t| (0..W * H).map(|i| (5000 + 300 * t + 7 * i) as u16 + if t > 5 { 2000 } else { 0 }).collect())
        .collect();
    let names: Vec<String> = (0..10).map(|t| format!("yr{}", 1990 + t)).collect();
    let path = d.join("annual.tif");
    write(&path, &bands, &names, 1.0, 0.0);
    let info = open(&[path], &BandSelection::default());
    let out = d.join("maps");
    // the whole raster: no window to cut
    let code = script(&info, run_target([0, 0, W, H], &out), &[tool("landtrendr")]);
    println!("{code}");
    assert!(!code.contains("clip="), "{code}");
    // a window: the cells whose centres are in it
    let code = script(&info, run_target([1, 1, 5, 4], &out), &[tool("mann_kendall")]);
    println!("{code}");
    assert!(code.contains("window=(1, 1, 5, 4)") && !code.contains("chunks="), "{code}");
    assert!(
        code.contains("result = zeit.mann_kendall(series)") || code.contains("result = zeit.mann_kendall("),
        "{code}"
    );
    assert!(code.contains("zeit.save_raster(result, f\"{out_dir}/result\")"), "{code}");
    for gone in ["rasterio", "import numpy", "nodata=None", "n_jobs", "Window(", "print("] {
        assert!(!code.contains(gone), "{gone} in\n{code}");
    }
    assert!(code.lines().count() <= 16, "{code}");
    if run_script(&code, "window-annual") {
        // a folder of maps, the window's size
        let mut maps: Vec<PathBuf> = std::fs::read_dir(out.join("result"))
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
            .collect();
        maps.sort();
        assert!(maps.len() >= 3, "{maps:?}");
        for m in &maps {
            let (w, h, v) = read_band(m, 1);
            assert_eq!((w, h), (4, 3), "{m:?}");
            assert!(v.iter().any(|x| x.is_finite()), "{m:?}");
        }
    }
    // a tool with two results writes two
    let code = script(&info, run_target([0, 0, W, H], &out), &[tool("landtrendr")]);
    assert!(code.contains("{out_dir}/fit") && code.contains("{out_dir}/events"), "{code}");
}

#[test]
fn a_chain_of_steps_names_its_variables_and_its_files_by_step_and_chains_the_analysis_series() {
    let d = dir("window-chain");
    let bands: Vec<Vec<u16>> = (0..10).map(|t| (0..W * H).map(|i| (5000 + 300 * t + 7 * i) as u16).collect()).collect();
    let names: Vec<String> = (0..10).map(|t| format!("yr{}", 1990 + t)).collect();
    let path = d.join("annual.tif");
    write(&path, &bands, &names, 1.0, 0.0);
    let info = open(&[path], &BandSelection::default());
    let tools = [tool("smooth"), tool("mann_kendall"), tool("mann_kendall")];
    let step = |name: &str, reads_analysis, leaves_analysis, skipped: Option<&str>| janus_zeit::python::Step {
        name: name.into(),
        reads_analysis,
        leaves_analysis,
        skipped: skipped.map(String::from),
    };
    let chain = [
        step("01_smooth", false, true, None),
        step("02_mann_kendall", true, false, None),
        step("03_mann_kendall", true, false, Some("why")),
    ];
    let roles = roles(&info);
    let path = |p: &str| p.replace('\\', "/");
    let spec = Spec {
        info: &info,
        version: "test",
        name: "series",
        target: run_target([0, 0, W, H], &d.join("maps")),
        roles: &roles,
        tools: &tools,
        path: &path,
        other: None,
        chain: &chain,
    };
    let code = pixel_script(&spec).unwrap();
    println!("{code}");
    assert!(code.contains("smooth_result = zeit.smooth(series"), "{code}");
    assert!(code.contains("analysis = smooth_result"), "{code}");
    assert!(code.contains("mann_kendall_result = zeit.mann_kendall(analysis"), "{code}");
    assert!(code.contains("# 3. Mann-Kendall trend: left out (why)"), "{code}");
    assert!(code.contains("{out_dir}/01_smooth") && code.contains("{out_dir}/02_mann_kendall"), "{code}");
    assert!(!code.contains("03_mann_kendall"), "{code}");
    run_script(&code, "window-chain");
}

#[test]
fn nbar_of_a_pin_reads_the_bands_and_the_angle_bands_and_runs() {
    let d = dir("nbar-pin");
    let names: Vec<String> =
        ["blue", "nir", "swir1", "sza", "saa", "vza", "vaa"].iter().map(|s| s.to_string()).collect();
    for (t, date) in DATES.iter().enumerate() {
        let bands: Vec<Vec<u16>> = (0..7)
            .map(|b| {
                (0..W * H)
                    .map(|i| match b {
                        3 => 3000 + 100 * t as u16 + i as u16,
                        4 => 14000,
                        5 => 200 + i as u16,
                        6 => 28000,
                        _ => 1000 + 500 * b as u16 + 100 * t as u16 + i as u16,
                    })
                    .collect()
            })
            .collect();
        write(&d.join(format!("n_{date}.tif")), &bands, &names, 1.0, 0.0);
    }
    let info = open(&[d], &BandSelection::default());
    let roles: BTreeMap<String, i32> = names.iter().enumerate().map(|(k, r)| (r.clone(), k as i32 + 1)).collect();
    let params: BTreeMap<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({ "angle_scale": "hundredths", "cfactor": true })).unwrap();
    let tools = [("nbar".to_string(), Params::with_defaults("nbar", &params).unwrap())];
    let path = |p: &str| p.replace('\\', "/");
    let spec = Spec {
        info: &info,
        version: "test",
        name: "series",
        target: pin(2, 1),
        roles: &roles,
        tools: &tools,
        path: &path,
        other: None,
        chain: &[],
    };
    let code = pixel_script(&spec).unwrap();
    println!("{code}");
    assert!(code.contains("nbar_angles = ") && code.contains("* 0.01"), "{code}");
    assert!(code.contains("nbar, cfactor = zeit.nbar(") && code.contains("return_cfactor=True"), "{code}");
    for gone in ["rasterio", "nodata=None", "n_jobs"] {
        assert!(!code.contains(gone), "{gone} in\n{code}");
    }
    let check = "\nimport numpy as np\nassert nbar.sizes['band'] == 3 and np.isfinite(nbar.values).all(), nbar\nassert np.isfinite(cfactor.values).all()\n";
    run_script(&format!("{code}{check}"), "nbar-pin");
}
