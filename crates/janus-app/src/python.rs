//! "Copy Python" (src/app_python.cpp): code that reproduces outside Janus the series on the chart (a pin's or the
//! ROI's) as Janus reads it and the Zeit tools fitted on it, with the same parameters: `State::python_snippet`
//! asks `janus_zeit::python::pixel_script` (Zeit's own `load_raster` reads the series; the website's guides call
//! the same function). A raster run's rerun.py (`run_code`, and the chain's in tsa.rs) is the same function with
//! the run's window as the target: Zeit reads the window, makes the call and `save_raster` writes the maps.

use crate::app::State;

/// A Python string literal (paths with backslashes, names with quotes).
pub(crate) fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '\\' || c == '"' {
            out.push('\\');
        }
        out.push(c);
    }
    out + "\""
}

/// Which series: a pin's, the ROI's.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Pin(u32),
    Roi,
}

impl State {
    /// The code for a series of the chart (None if it is not there): Zeit reads the series as Janus shows it and
    /// makes the tools' calls (`janus_zeit::python::pixel_script`, which the website's guides use too).
    pub(crate) fn python_snippet(&mut self, which: Which) -> Option<String> {
        let info = self.session.as_ref()?.info.clone();
        let view = match which {
            Which::Pin(id) => self.series.pins.iter().find(|p| p.id == id).cloned(),
            Which::Roi => None,
        };
        let roi = (which == Which::Roi && self.series.roi.complete()).then(|| self.series.roi.data.clone()).flatten();
        if view.is_none() && roi.is_none() {
            return None;
        }
        let name = self.layers.get(self.active).map_or("the series".to_string(), |l| l.name.clone());
        let fit_key = match which {
            Which::Pin(id) => crate::zeit::Which::Pin(id),
            Which::Roi => crate::zeit::Which::Roi,
        };
        // the tools fitted on this series, in the chart's order
        let fitted: Vec<String> = match (&view, which) {
            (_, Which::Roi) => self.roi_fits(),
            (Some(v), _) => self.zeit.fits_of(info.id, fit_key, v.x, v.y),
            _ => vec![],
        }
        .into_iter()
        .filter(|f| f.result.is_some_and(|r| r.is_ok()))
        .map(|f| f.tool.id.clone())
        .collect();
        let tools: Vec<(String, janus_zeit::Params)> = fitted
            .into_iter()
            .filter_map(|id| {
                let tool = janus_zeit::manifest().tool(&id)?;
                let params = janus_zeit::Params::with_defaults(&id, self.zeit.params(tool))?;
                Some((id, params))
            })
            .collect();
        let target = match (&view, &roi) {
            (Some(v), _) => janus_zeit::python::Target::Pin { id: v.id, col: v.x as i64, row: v.y as i64 },
            (None, Some(r)) => janus_zeit::python::Target::Roi(janus_zeit::python::Roi {
                x0: r.x0,
                y0: r.y0,
                x1: r.x1,
                y1: r.y1,
                sampled: r.sampled.then_some((r.bw, r.bh)),
                rings: self.series.roi_poly.clone(),
            }),
            _ => return None,
        };
        let path = |p: &str| crate::provenance::absolute(p);
        let spec = janus_zeit::python::Spec {
            info: &info,
            version: env!("CARGO_PKG_VERSION"),
            name: &name,
            target,
            roles: &self.zeit.roles,
            tools: &tools,
            path: &path,
            other: None,
            chain: &[],
        };
        Some(match janus_zeit::python::pixel_script(&spec) {
            Ok(code) => code,
            Err(e) => format!("# Copy Python cannot write this series: {e}\n"),
        })
    }

    /// The chart's "Python..." menu: the code of a pin's or the ROI's series in the Python history's editor.
    pub(crate) fn copy_python_ui(&mut self, ui: &mut egui::Ui) {
        let any = !self.series.pins.is_empty() || self.series.roi.complete();
        let mut chosen = None;
        const HELP: &str = "Python that reproduces outside Janus the series on the chart: Zeit's load_raster reads\n\
                            it from the files as Janus does (band or index, quality mask, scale) and, with a Zeit\n\
                            tool on the chart, the script makes the same Zeit calls with the same parameters.\n\
                            Shown in an editor: read it, copy it, save it as a .py or a notebook.\n\
                            Needs zeit-cdts 3.2 or newer (pip install -U zeit-cdts).";
        // the button itself in the toolbar's row (in a child Ui of its own, it could neither move to the next row
        // nor fit the end of this one)
        if !any {
            ui.add_enabled(false, egui::Button::new("Python..."))
                .on_disabled_hover_text(format!("{HELP}\n\nDrop a pin (a click on the map) or draw an ROI first"));
            return;
        }
        ui.menu_button("Python...", |ui| {
            for p in &self.series.pins {
                if ui.button(format!("Pin {} ({}, {})", p.id, p.x, p.y)).clicked() {
                    chosen = Some(Which::Pin(p.id));
                }
            }
            if self.series.roi.complete() && ui.button("ROI (rectangle)").clicked() {
                chosen = Some(Which::Roi);
            }
            let tools: Vec<String> = self.zeit.chart_tools().iter().map(|(t, _)| t.name.clone()).collect();
            if !tools.is_empty() {
                ui.weak(format!("with {}", tools.join(", ")));
            }
        })
        .response
        .on_hover_text(HELP);
        if let Some(w) = chosen
            && let Some(code) = self.python_snippet(w)
        {
            let layer = self.layers.get(self.active).map_or(String::new(), |l| format!(" of {}", l.name));
            let what = match w {
                Which::Pin(id) => format!("Pin {id}'s series"),
                Which::Roi => "The ROI's series".to_string(),
            };
            self.history.show_snippet(&format!("{what}{layer}"), code);
        }
    }
}

/// "Copy Python" of a raster run (its provenance's rerun.py): Zeit reads the series over the run's window
/// (`janus_zeit::python::pixel_script`, as for a pin), the tool's call has the run's parameters, and `save_raster`
/// writes every map it returns under `out_dir`. `other` is the second series a tool reads (ImproPhe's coarse one).
pub(crate) fn run_code(
    info: &janus_cube::CubeInfo,
    tool: &janus_zeit::Tool,
    params: &std::collections::BTreeMap<String, serde_json::Value>,
    roles: &std::collections::BTreeMap<String, i32>,
    other: Option<&janus_cube::CubeInfo>,
    window: [usize; 4],
    out_dir: &str,
) -> Result<String, String> {
    let p = janus_zeit::Params::with_defaults(&tool.id, params).ok_or("its parameters are not the tool's")?;
    let tools = [(tool.id.clone(), p)];
    let path = |p: &str| crate::provenance::absolute(p);
    let spec = janus_zeit::python::Spec {
        info,
        version: env!("CARGO_PKG_VERSION"),
        name: &info.description,
        target: janus_zeit::python::Target::Run {
            window,
            out_dir: out_dir.to_string(),
            what: format!("the run of {}", tool.name),
        },
        roles,
        tools: &tools,
        path: &path,
        other,
        chain: &[],
    };
    janus_zeit::python::pixel_script(&spec)
}

/// "Copy Python" of a run of a tool on a folder (Level 1C to surface reflectance): Zeit's call on each scene of the input
/// folder and `save` under `out_dir`.
pub(crate) fn folder_code(
    tool: &janus_zeit::Tool,
    params: &std::collections::BTreeMap<String, serde_json::Value>,
    out_dir: &str,
) -> Result<String, String> {
    let p = janus_zeit::Params::with_defaults(&tool.id, params).ok_or("its parameters are not the tool's")?;
    if tool.id != "level2" {
        return Err(format!("{}: no Python of it", tool.name));
    }
    let path = |p: &str| crate::provenance::absolute(p);
    janus_zeit::python::level2_script(env!("CARGO_PKG_VERSION"), &p, &path, out_dir)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::atomic::AtomicBool;

    use gdal::Metadata;
    use janus_cube::{BandSelection, open_cube};
    use janus_zeit::job::run_job;
    use janus_zeit::manifest;
    use serde_json::{Value, json};

    use crate::zeit::{base_spec, series_inputs};

    const W: usize = 10;
    const H: usize = 8;
    const T: usize = 6;
    /// The bands of each date's file: three to normalize and the four angles (hundredths of a degree).
    const BANDS: [&str; 7] = ["blue", "nir", "swir1", "sza", "saa", "vza", "vaa"];

    /// A series of `T` dates, a file each with `BANDS`: reflectance of 0.05-0.5 with gaps, a solar zenith of the date
    /// that varies a little over the scene, a view zenith of 0-4 degrees, a pixel outside the model's domain.
    fn series(dir: &Path) -> janus_cube::CubeInfo {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut rng = 4321u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        for k in 0..T {
            let day = 18300 + 16 * k as i64;
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let path = dir.join(format!("n_{y:04}-{m:02}-{d:02}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, BANDS.len()).unwrap();
            ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0]).unwrap();
            ds.set_spatial_ref(&gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap()).unwrap();
            for (b, name) in BANDS.iter().enumerate() {
                let data: Vec<f32> = (0..W * H)
                    .map(|p| {
                        let (x, row) = ((p % W) as f32, (p / W) as f32);
                        match *name {
                            "sza" if p == 0 => 9500.0,
                            "sza" => 100.0 * (30.0 + 4.0 * k as f32 + 0.1 * row + 0.2 * next()),
                            "saa" => 100.0 * (140.0 + k as f32 + 0.1 * x),
                            "vza" => 100.0 * ((x - 4.5).abs() * 0.8 + 0.1 * next()),
                            "vaa" => 100.0 * if x < 4.5 { 280.0 } else { 100.0 },
                            _ if next() < 0.08 => -9999.0,
                            _ => (0.05 + 0.45 * next()) * (b as f32 + 1.0) / 3.0,
                        }
                    })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_no_data_value(Some(-9999.0)).unwrap();
                band.set_description(name).unwrap();
                band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
            }
        }
        open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap()
    }

    /// A raster's bands as float32 values over the run's window (the file is that window's), NoData as NaN.
    fn read(path: &str) -> Vec<Vec<f32>> {
        let ds = gdal::Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        (1..=ds.raster_count())
            .map(|b| {
                let band = ds.rasterband(b).unwrap();
                let v = band.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
                v.data().to_vec()
            })
            .collect()
    }

    /// The raster run's Copy Python (the rerun script) run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio
    /// and xarray; the test is left out without it) gives the maps `run_job` wrote: the bands read by role, the
    /// angles scaled by the parameter, the scene's solar zenith over the window.
    #[test]
    fn the_nbar_script_runs_in_zeit_and_gives_the_runs_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-nbar-test-script");
        let info = series(&base.join("series"));
        let tool = manifest().tool("nbar").unwrap();
        let roles: BTreeMap<String, i32> =
            BANDS.iter().enumerate().map(|(k, r)| (r.to_string(), k as i32 + 1)).collect();
        let params: BTreeMap<String, Value> =
            serde_json::from_value(json!({ "angle_scale": "hundredths", "cfactor": true })).unwrap();
        let window = [1, 1, W - 1, H - 1];
        let mut spec = base_spec(&info, tool, &params, T - 1, 2);
        series_inputs(&info, tool, &roles, &base.join("in"), &mut spec).unwrap();
        spec.window = Some(window);
        spec.output_dir = base.join("out").to_string_lossy().into_owned();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        // the run's outputs: a series per band and per c-factor
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["nbar_blue", "nbar_nir", "nbar_swir1", "cfactor_blue", "cfactor_nir", "cfactor_swir1"]);

        let out = base.join("python");
        let code = super::run_code(&info, tool, &params, &roles, None, window, &out.to_string_lossy()).unwrap();
        assert!(code.contains("zeit.nbar(") && code.contains("return_cfactor=True"), "{code}");
        assert!(!code.contains("n_jobs") && !code.contains("rasterio"), "{code}");
        let script = base.join("nbar.py");
        std::fs::write(&script, code).unwrap();
        run_python(&python, &script);
        // the script's stacks are (date, band) in the core's order; the run's files a band per date
        let (mut values, mut worst) = (0, 0f32);
        for (r, prefix) in [("nbar", "nbar"), ("cfactor", "cfactor")] {
            let got = read(&out.join(format!("{r}.tif")).to_string_lossy());
            assert_eq!(got.len(), T * 3);
            for (b, role) in ["blue", "nir", "swir1"].iter().enumerate() {
                let id = format!("{prefix}_{role}");
                let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap();
                let want = read(o["path"].as_str().unwrap());
                assert_eq!(want.len(), T);
                for k in 0..T {
                    for (g, w) in got[k * 3 + b].iter().zip(&want[k]) {
                        assert!(
                            (g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-5 * w.abs(),
                            "{prefix} {role} date {k}: {g} vs {w}"
                        );
                        if g.is_finite() {
                            worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                            values += 1;
                        }
                    }
                }
            }
        }
        println!("{values} values of the script compared with the run's, the largest relative difference {worst:e}");
        assert!(values > 500);
    }

    /// The dates of the Sentinel-2 series below.
    const S2_T: usize = 3;

    /// A script run by the Python of JANUS_PYTHON on the PATH as an activated conda environment has it and Windows'
    /// own (the other DLLs shadow the environment's); it must succeed.
    fn run_python(python: &str, script: &Path) {
        let root = Path::new(python).parent().unwrap().to_path_buf();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let mut path: Vec<String> = ["", "Library\\mingw-w64\\bin", "Library\\usr\\bin", "Library\\bin", "Scripts"]
            .iter()
            .map(|d| root.join(d).to_string_lossy().into_owned())
            .collect();
        path.extend([format!("{system}\\system32"), system.clone()]);
        let run = std::process::Command::new(python)
            .arg(script)
            .env_clear()
            .envs(["SystemRoot", "TEMP", "TMP"].iter().filter_map(|k| Some((*k, std::env::var(k).ok()?))))
            .env("PATH", path.join(";"))
            .env("PYTHONNOUSERSITE", "1")
            .env("OMP_NUM_THREADS", "2")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    /// A series of Sentinel-2 dates in tile 23LKC (UTM 23S, pixels of 1.6 km across the east edge of the partial
    /// swath of 2023-07-26), `BANDS`-like files of three bands (no angle bands), each carrying its granule's metadata
    /// as the VRTs of a STAC series do (the fixtures of Zeit's tests).
    fn s2_series(dir: &Path) -> janus_cube::CubeInfo {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../janus-zeit/tests/data/s2angles");
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap();
        let mut rng = 99u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        for (date, xml) in [
            ("2023-07-23", "s2_23LKC_20230723_MTD_TL.xml"),
            ("2023-07-24", "s2_23LKC_20230723_MTD_TL.xml"),
            ("2023-07-26", "s2_23LKC_20230726_partial_MTD_TL.xml"),
        ] {
            let path = dir.join(format!("s2_{date}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, W, H, 3).unwrap();
            ds.set_geo_transform(&[199980.0 + 12500.0, 1600.0, 0.0, 8300020.0 - 3000.0, 0.0, -1600.0]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            ds.set_metadata_item("STAC_GRANULE_METADATA", &fixtures.join(xml).to_string_lossy(), "").unwrap();
            for (b, name) in ["blue", "nir", "swir1"].iter().enumerate() {
                let data: Vec<f32> = (0..W * H)
                    .map(|_| if next() < 0.05 { -9999.0 } else { (0.05 + 0.4 * next()) * (b as f32 + 1.0) / 3.0 })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_no_data_value(Some(-9999.0)).unwrap();
                band.set_description(name).unwrap();
                band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
            }
        }
        open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap()
    }

    /// With the angles from Sentinel-2's metadata the spec carries each date's files (found from the files'
    /// metadata), the run needs no angle bands, and its Copy Python (`zeit.sentinel2_angles` on the cube placed on the
    /// files' grid, then `zeit.nbar`) run in Zeit gives the maps `run_job` wrote.
    #[test]
    fn the_nbar_sentinel2_script_runs_in_zeit_and_gives_the_runs_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio, rioxarray and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-nbar-s2-test-script");
        let info = s2_series(&base.join("series"));
        let tool = manifest().tool("nbar").unwrap();
        let roles: BTreeMap<String, i32> =
            ["blue", "nir", "swir1"].iter().enumerate().map(|(k, r)| (r.to_string(), k as i32 + 1)).collect();
        let params: BTreeMap<String, Value> =
            serde_json::from_value(json!({ "angles": "sentinel2", "view_band": "B06", "cfactor": true })).unwrap();
        let window = [1, 1, W - 1, H - 1];
        let mut spec = base_spec(&info, tool, &params, S2_T - 1, 2);
        series_inputs(&info, tool, &roles, &base.join("in"), &mut spec).unwrap();
        assert_eq!(spec.granules.len(), 3);
        assert!(spec.granules.iter().all(|g| g.len() == 1 && g[0].ends_with("_MTD_TL.xml")), "{:?}", spec.granules);
        spec.window = Some(window);
        spec.output_dir = base.join("out").to_string_lossy().into_owned();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let ids: Vec<&str> = result.outputs.iter().map(|o| o["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["nbar_blue", "nbar_nir", "nbar_swir1", "cfactor_blue", "cfactor_nir", "cfactor_swir1"]);

        let out = base.join("python");
        let code = super::run_code(&info, tool, &params, &roles, None, window, &out.to_string_lossy()).unwrap();
        assert!(code.contains("angles = zeit.sentinel2_angles(sources, like=nbar_cube)"), "{code}");
        assert!(code.contains("MTD_TL.xml") && !code.contains("nbar_angles") && !code.contains("rasterio"), "{code}");
        let script = base.join("nbar.py");
        std::fs::write(&script, code).unwrap();
        run_python(&python, &script);
        let (mut values, mut worst) = (0, 0f32);
        for (r, prefix) in [("nbar", "nbar"), ("cfactor", "cfactor")] {
            let got = read(&out.join(format!("{r}.tif")).to_string_lossy());
            assert_eq!(got.len(), S2_T * 3);
            for (b, role) in ["blue", "nir", "swir1"].iter().enumerate() {
                let id = format!("{prefix}_{role}");
                let o = result.outputs.iter().find(|o| o["id"] == id.as_str()).unwrap();
                let want = read(o["path"].as_str().unwrap());
                assert_eq!(want.len(), S2_T);
                for k in 0..S2_T {
                    for (g, w) in got[k * 3 + b].iter().zip(&want[k]) {
                        assert!(
                            (g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-5 * w.abs(),
                            "{prefix} {role} date {k}: {g} vs {w}"
                        );
                        if g.is_finite() {
                            worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                            values += 1;
                        }
                    }
                }
            }
        }
        println!(
            "{values} values of the Sentinel-2 script compared with the run's, the largest relative difference {worst:e}"
        );
        assert!(values > 300);
    }

    /// ImproPhe's fine and coarse series as two folders: the fine of 24 x 24 pixels of 10 m, eight dates in three
    /// bands and a quality band (the rule "0 is masked") that refuses one date over a block, with gaps; the coarse
    /// of 8 x 8 pixels of 30 m on the same extent, three scenes in two bands (UTM 22S).
    fn improphe_series(dir: &Path) -> (janus_cube::CubeInfo, janus_cube::CubeInfo) {
        let _ = std::fs::remove_dir_all(dir);
        let (fine, coarse) = (dir.join("fine"), dir.join("coarse"));
        std::fs::create_dir_all(&fine).unwrap();
        std::fs::create_dir_all(&coarse).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap();
        let mut rng = 777u64;
        let mut next = move || {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (rng >> 33) as f32 / (1u64 << 31) as f32
        };
        let mut write = |dir: &Path, day: i64, prefix: &str, w: usize, res: f64, bands: usize, qa: bool| {
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let path = dir.join(format!("{prefix}_{y:04}-{m:02}-{d:02}.tif"));
            let mut ds = drv.create_with_band_type::<f32, _>(&path, w, w, bands + usize::from(qa)).unwrap();
            ds.set_geo_transform(&[500000.0, res, 0.0, 8900000.0, 0.0, -res]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            for b in 0..bands + usize::from(qa) {
                let data: Vec<f32> = (0..w * w)
                    .map(|p| {
                        let (x, row) = ((p % w) as f32, (p / w) as f32);
                        if qa && b == bands {
                            // the date of the 5th file is refused over a block
                            return if day == 18300 + 4 * 40 && x < 12.0 && row < 12.0 { 0.0 } else { 1.0 };
                        }
                        if next() < 0.06 {
                            return -9999.0;
                        }
                        let cover = ((x / (w as f32 / 3.0)).floor() + 2.0 * (row / (w as f32 / 2.0)).floor()) % 4.0;
                        let season = 1.0 + 0.3 * ((day as f32 / 58.0) + cover).sin();
                        (0.1 + 0.15 * cover) * season * (b as f32 + 1.0) / 2.0 + 0.01 * next()
                    })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_no_data_value(Some(-9999.0)).unwrap();
                band.write((0, 0), (w, w), &mut gdal::raster::Buffer::new((w, w), data)).unwrap();
            }
        };
        for k in 0..8 {
            write(&fine, 18300 + 40 * k, "fine", 24, 10.0, 3, true);
        }
        for day in [18300 + 30, 18300 + 190, 18300 + 400] {
            write(&coarse, day, "coarse", 8, 30.0, 2, false);
        }
        let sel = BandSelection {
            band: 1,
            qa_band: 4,
            qa_rule: janus_cube::QaRule::NonZero,
            chosen: true,
            ..Default::default()
        };
        (
            open_cube(&[fine.to_string_lossy().into_owned()], &sel).unwrap(),
            open_cube(&[coarse.to_string_lossy().into_owned()], &BandSelection::default()).unwrap(),
        )
    }

    /// ImproPhe's Copy Python (the rerun script) run in Zeit gives the maps `run_job` wrote: the fine series is the
    /// run's (every band but the quality band, the dates it refuses missing), the coarse one the other series.
    #[test]
    fn the_improphe_script_runs_in_zeit_and_gives_the_runs_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio, rioxarray and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-improphe-test-script");
        let (info, other) = improphe_series(&base.join("series"));
        assert_eq!((info.t(), other.t(), info.bands_per_date, other.bands_per_date), (8, 3, 4, 2));
        let tool = manifest().tool("improphe").unwrap();
        let params: BTreeMap<String, Value> = serde_json::from_value(
            json!({ "by": "scenes", "kernel_size": 60.0, "texture_size": 20.0, "force": true, "pca": "principal" }),
        )
        .unwrap();
        let roles = BTreeMap::new();
        let window = [1, 1, 23, 22];
        let mut spec = base_spec(&info, tool, &params, 7, 2);
        series_inputs(&info, tool, &roles, &base.join("in"), &mut spec).unwrap();
        // every band but the quality band
        assert_eq!(spec.bands.keys().map(String::as_str).collect::<Vec<_>>(), ["b001", "b002", "b003"]);
        crate::zeit::other_series(&other, "coarse", &base.join("other"), &mut spec).unwrap();
        assert_eq!(spec.other.as_ref().unwrap().inputs.len(), 2);
        spec.window = Some(window);
        spec.output_dir = base.join("out").to_string_lossy().into_owned();
        let result = run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let want = read(result.outputs[0]["path"].as_str().unwrap());
        assert_eq!(want.len(), 6);

        let out = base.join("python");
        let code = super::run_code(&info, tool, &params, &roles, Some(&other), window, &out.to_string_lossy()).unwrap();
        // long calls are broken over lines
        let flat = code.replace("(\n    ", "(");
        assert!(flat.contains("zeit.improphe(coarse_cube, fine_cube, by=None, kernel_size=60.0"), "{code}");
        assert!(code.contains("bands={\"Band 1\": 1, \"Band 2\": 2, \"Band 3\": 3}"), "{code}");
        assert!(code.contains("fine_cube.where(zeit.qa_mask(qa, \"nonzero\"))"), "{code}");
        let script = base.join("improphe.py");
        std::fs::write(&script, code).unwrap();
        run_python(&python, &script);
        let got = read(&out.join("result.tif").to_string_lossy());
        assert_eq!(got.len(), want.len());
        let (mut values, mut worst) = (0, 0f32);
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            for (g, w) in g.iter().zip(w) {
                assert!((g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-5 * w.abs(), "band {k}: {g} vs {w}");
                if g.is_finite() {
                    worst = worst.max((g - w).abs() / w.abs().max(1e-12));
                    values += 1;
                }
            }
        }
        println!(
            "{values} values of the ImproPhe script compared with the run's, the largest relative difference {worst:e}"
        );
        assert!(values > 1000);
    }

    /// Level 1C to surface reflectance's Copy Python: Zeit's call on each scene of the folder, the options that are not
    /// Zeit's defaults, and `save` under the output folder by product.
    #[test]
    fn the_level2_script_calls_zeit_level2_on_each_scene() {
        let base = std::env::temp_dir().join(format!("janus-level2-script-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let name = "S2B_MSIL1C_20230728T132239_N0509_R038_T23LKC_20230728T150149";
        std::fs::create_dir_all(base.join(format!("{name}.SAFE"))).unwrap();
        let tool = manifest().tool("level2").unwrap();
        let params: BTreeMap<String, Value> = serde_json::from_value(
            json!({ "input": base.to_string_lossy(), "dem": "dem.tif", "merge": "improphe", "adjacency": false }),
        )
        .unwrap();
        let code = super::folder_code(tool, &params, "out").unwrap();
        let flat = code.replace("(\n    ", "(").replace(",\n    ", ", ").replace("\n)", ")");
        assert!(code.contains("import zeit") && code.contains("zeit-cdts>=3.6.0"), "{code}");
        assert!(flat.contains(".SAFE\", dem=\"dem.tif\", merge=\"improphe\", adjacency=False)"), "{code}");
        assert!(code.contains(&format!("l2.zeit.save(\"out/{name}\")")), "{code}");
        assert!(!code.contains("brdf") && !code.contains("topography") && !code.contains("del l2"), "{code}");
        let e = super::folder_code(tool, &BTreeMap::new(), "out").unwrap_err();
        assert!(e.contains("choose the folder"), "{e}");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The Copy Python of a run of Level 1C to surface reflectance on the real scene of tile 23LKC (the JPEG 2000 product,
    /// no merge) run in Zeit (the Python of JANUS_PYTHON, with the Zeit repository in ZEIT_REPO: the 3.6 `zeit.level2`) gives
    /// GeoTIFFs equal in every pixel to those the tool wrote. By hand: the two runs take 18 and 29 GiB, one after the other.
    ///
    /// ```text
    /// JANUS_PYTHON=C:\Users\eduar\miniforge3\envs\zeit\python.exe ZEIT_REPO=C:\Users\eduar\Documents\dev\zeit \
    ///   cargo test -j 8 --release -p janus-app level2_script_real -- --ignored --nocapture --test-threads=1
    /// ```
    #[test]
    #[ignore]
    fn the_level2_script_real_scene_equals_the_tool() {
        let (Ok(python), Ok(zeit)) = (std::env::var("JANUS_PYTHON"), std::env::var("ZEIT_REPO")) else {
            panic!("JANUS_PYTHON (a Python with zeit's dependencies) and ZEIT_REPO (the Zeit repository, 3.6) are needed");
        };
        let data = std::path::PathBuf::from(
            std::env::var("JANUS_L2_DATA")
                .unwrap_or_else(|_| r"C:\Users\eduar\Documents\dev\data_force_l2_real".into()),
        );
        let product = "S2B_MSIL1C_20230728T132239_N0509_R038_T23LKC_20230728T150149";
        let base = data.join("janus_out").join("script_test");
        let _ = std::fs::remove_dir_all(&base);
        let tool = manifest().tool("level2").unwrap();
        let params: BTreeMap<String, Value> = serde_json::from_value(json!({
            "input": data.join(format!("{product}.SAFE")).to_string_lossy(),
            "dem": data.join("cop_glo30_23LKC.tif").to_string_lossy(),
        }))
        .unwrap();
        let spec = janus_zeit::job::JobSpec {
            tool: "level2".into(),
            params: params.clone(),
            threads: Some(8),
            output_dir: base.join("tool").to_string_lossy().into_owned(),
            ..Default::default()
        };
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        let code = super::folder_code(tool, &params, &base.join("python").to_string_lossy()).unwrap();
        let script = base.join("level2.py");
        let path_line = format!("import sys\nsys.path.insert(0, {})\n", super::py_str(&zeit));
        std::fs::write(&script, format!("{path_line}{code}")).unwrap();
        run_python(&python, &script);
        let mut values = 0usize;
        for name in ["boa", "qai", "dst", "vzn", "hot", "aod", "wvp"] {
            let got = read(&base.join("python").join(product).join(format!("{name}.tif")).to_string_lossy());
            let want = read(&base.join("tool").join(product).join(format!("{name}.tif")).to_string_lossy());
            assert_eq!(got.len(), want.len(), "{name}");
            for (g, w) in got.iter().zip(&want) {
                assert_eq!(g.len(), w.len(), "{name}");
                assert!(g.iter().zip(w).all(|(a, b)| a == b), "{name} differs");
                values += g.len();
            }
        }
        println!("{values} values of the Python script equal to the tool's");
    }
}
