//! A model applied to a window of a series, as a Zeit run: bands of rows read from the cube (the next one while the
//! model classifies this one), each pixel's gaps filled and its values scaled as for the training, the pixels
//! classified in batches on the model's device. The outputs: the class map (Byte, 0 without a class, the classes'
//! colours and names in the file), the probability of the class given, and the uncertainty (the entropy of the
//! probabilities over that of K equal ones: 0 sure, 1 every class alike).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::raster::{Buffer, ColorEntry, ColorTable, RasterCreationOptions};
use gdal::{Dataset, DriverManager};
use janus_cube::CubeInfo;
use serde_json::{Value, json};

use crate::data::{InputBand, Series, fill_gaps, read_window};
use crate::model::{Arch, Model};

/// A run's window (x0, y0, x1, y1 in the series' pixels) and where its maps go.
#[derive(Clone, Debug, Default)]
pub struct ApplySpec {
    pub window: [usize; 4],
    pub output_dir: String,
    /// the files' stem (e.g. "tempcnn")
    pub stem: String,
    /// pixels a batch (0: by the model's size)
    pub batch: usize,
}

/// The maps written (their descriptions as a Zeit run's outputs) and the window.
pub struct Applied {
    pub outputs: Vec<Value>,
    pub window: [usize; 4],
    pub classified: usize,
}

/// Pixels in a batch: about 256 MB of the network's activations.
fn batch_of(model: &Model) -> usize {
    let t = model.header.calendar.len().max(1);
    let per_pixel = 4 * t * 512; // the widest layers' activations, float32
    ((256usize << 20) / per_pixel).clamp(256, 65536)
}

fn create(path: &Path, w: usize, h: usize, byte: bool, info: &CubeInfo, win: [usize; 4]) -> Result<Dataset, String> {
    let driver = DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let opts = RasterCreationOptions::from_iter([
        "COMPRESS=DEFLATE",
        "TILED=YES",
        "BLOCKXSIZE=256",
        "BLOCKYSIZE=256",
        "BIGTIFF=IF_SAFER",
        "NUM_THREADS=ALL_CPUS",
    ]);
    let mut ds = if byte {
        driver.create_with_band_type_with_options::<u8, _>(path, w, h, 1, &opts)
    } else {
        driver.create_with_band_type_with_options::<f32, _>(path, w, h, 1, &opts)
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(g) = info.geo_transform {
        let x = g[0] + win[0] as f64 * g[1] + win[1] as f64 * g[2];
        let y = g[3] + win[0] as f64 * g[4] + win[1] as f64 * g[5];
        ds.set_geo_transform(&[x, g[1], g[2], y, g[4], g[5]]).map_err(|e| e.to_string())?;
    }
    if !info.crs_wkt.is_empty() {
        ds.set_projection(&info.crs_wkt).map_err(|e| e.to_string())?;
    }
    ds.rasterband(1)
        .map_err(|e| e.to_string())?
        .set_no_data_value(Some(if byte { 0.0 } else { f64::NAN }))
        .map_err(|e| e.to_string())?;
    Ok(ds)
}

/// The class map's colours and names: value k is class k (1-based).
fn describe_classes(ds: &mut Dataset, model: &Model) -> Result<(), String> {
    let mut ct = ColorTable::default();
    ct.set_color_entry(0, &ColorEntry::rgba(0, 0, 0, 0));
    for (k, c) in model.header.classes.iter().enumerate() {
        ct.set_color_entry(
            k as u16 + 1,
            &ColorEntry::rgba(c.color[0] as i16, c.color[1] as i16, c.color[2] as i16, 255),
        );
    }
    let mut band = ds.rasterband(1).map_err(|e| e.to_string())?;
    band.set_color_table(&ct);
    let names: Vec<std::ffi::CString> = std::iter::once(String::new())
        .chain(model.header.classes.iter().map(|c| c.name.clone()))
        .map(|s| std::ffi::CString::new(s).unwrap_or_default())
        .collect();
    let mut ptrs: Vec<*mut std::ffi::c_char> = names.iter().map(|s| s.as_ptr() as *mut _).collect();
    ptrs.push(std::ptr::null_mut());
    unsafe { gdal_sys::GDALSetRasterCategoryNames(band.c_rasterband(), ptrs.as_mut_ptr()) };
    Ok(())
}

/// What the model reads of a window of the series (x, y, w, h): its inputs, and the pixels it classifies.
pub fn input_of(
    model: &Model,
    info: &Arc<CubeInfo>,
    read: &[InputBand],
    win: (usize, usize, usize, usize),
) -> Result<(Series, Vec<bool>), String> {
    let mut sr = read_window(info, read, win);
    if let Arch::Presto(h) = &model.header.arch {
        let bands =
            h.inputs.bands.iter().map(|b| b.as_ref().map(|b| b.find_in(info)).transpose()).collect::<Result<_, _>>()?;
        let inputs = crate::presto::PrestoInputs { bands, ..h.inputs.clone() };
        let pixels: Vec<(f64, f64)> =
            (0..win.2 * win.3).map(|i| ((win.0 + i % win.2) as f64, (win.1 + i / win.2) as f64)).collect();
        return crate::presto::classifier_input(info, &inputs, &sr, &pixels);
    }
    let keep = fill_gaps(&mut sr, &model.header.calendar.years, model.header.min_valid);
    model.header.norm.apply(&mut sr);
    Ok((sr, keep))
}

/// Classifies the window; `progress(fraction, message)`, `cancel` stops it between bands of rows ("cancelled").
pub fn apply(
    spec: &ApplySpec,
    info: &Arc<CubeInfo>,
    model: &Model,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Applied, String> {
    let inputs = model.inputs_in(info)?;
    let [x0, y0, x1, y1] = spec.window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = |id: &str| dir.join(format!("{}_{id}.tif", spec.stem));
    let mut class_ds = create(&path("class"), w, h, true, info, spec.window)?;
    describe_classes(&mut class_ds, model)?;
    let mut prob_ds = create(&path("probability"), w, h, false, info, spec.window)?;
    let mut entropy_ds = create(&path("entropy"), w, h, false, info, spec.window)?;
    let k = model.header.classes.len();
    let batch = if spec.batch > 0 { spec.batch } else { batch_of(model) };
    let rows = (batch / w.max(1)).clamp(1, 256).min(h.max(1));
    let bands: Vec<(usize, usize)> = (0..h).step_by(rows).map(|r| (r, rows.min(h - r))).collect();
    let ln_k = (k as f64).ln();
    let t0 = std::time::Instant::now();
    let mut classified = 0;
    std::thread::scope(|s| -> Result<(), String> {
        // the next band of rows read (and its gaps filled, its values scaled) while this one is classified
        let (tx, rx) = std::sync::mpsc::sync_channel::<Result<(Series, Vec<bool>), String>>(1);
        let (bands_r, inputs_r) = (&bands, &inputs);
        s.spawn(move || {
            for &(r, hh) in bands_r {
                if tx.send(input_of(model, info, inputs_r, (x0, y0 + r, w, hh))).is_err() {
                    break;
                }
            }
        });
        let mut done = 0;
        for &(r, hh) in &bands {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let (sr, keep) = rx.recv().map_err(|e| e.to_string())??;
            let idx: Vec<usize> = (0..sr.n).filter(|&i| keep[i]).collect();
            let sel = sr.select(&idx);
            let p = model.predict(&sel.values, sel.n, batch);
            let n = w * hh;
            let (mut cls, mut prob, mut ent) = (vec![0u8; n], vec![f32::NAN; n], vec![f32::NAN; n]);
            for (j, &i) in idx.iter().enumerate() {
                let pj = &p[j * k..(j + 1) * k];
                let (best, &pmax) = pj.iter().enumerate().fold((0, &f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
                cls[i] = best as u8 + 1;
                prob[i] = pmax;
                let hsum: f64 = pj.iter().filter(|&&q| q > 0.0).map(|&q| -(q as f64) * (q as f64).ln()).sum();
                ent[i] = if ln_k > 0.0 { (hsum / ln_k) as f32 } else { 0.0 };
            }
            classified += idx.len();
            let write = |ds: &mut Dataset, data: Vec<f32>| -> Result<(), String> {
                let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
                b.write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), data)).map_err(|e| e.to_string())
            };
            class_ds
                .rasterband(1)
                .map_err(|e| e.to_string())?
                .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), cls))
                .map_err(|e| e.to_string())?;
            write(&mut prob_ds, prob)?;
            write(&mut entropy_ds, ent)?;
            done += hh;
            let el = t0.elapsed().as_secs_f64();
            progress(
                done as f64 / h as f64,
                &format!("rows {done}/{h}, {el:.0} s elapsed, ~{:.0} s left", el / done as f64 * (h - done) as f64),
            );
        }
        Ok(())
    })?;
    drop((class_ds, prob_ds, entropy_ds));
    let names: Vec<&str> = model.header.classes.iter().map(|c| c.name.as_str()).collect();
    let arch = model.header.arch.name();
    let outputs = vec![
        json!({"id": "class", "name": "Class", "unit": "class", "colormap": "Paired", "classes": names,
               "class_colors": model.header.classes.iter().map(|c| c.color).collect::<Vec<_>>(),
               "path": path("class").to_string_lossy(), "model": arch}),
        json!({"id": "probability", "name": "Probability of the class", "unit": "probability", "colormap": "Viridis",
               "path": path("probability").to_string_lossy()}),
        json!({"id": "entropy", "name": "Uncertainty (entropy)", "unit": "entropy", "colormap": "Plasma",
               "path": path("entropy").to_string_lossy()}),
    ];
    Ok(Applied { outputs, window: spec.window, classified })
}
