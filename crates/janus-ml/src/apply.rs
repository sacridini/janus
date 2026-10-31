//! A model applied to a window of a series, as a Zeit run: bands of rows read from the cube (the next one while the
//! model classifies this one), each pixel's gaps filled and its values scaled as for the training, the pixels
//! classified in batches on the model's device. The outputs: the class map (Byte, 0 without a class, the classes'
//! colours and names in the file), the probability of the class given, and the uncertainty (the entropy of the
//! probabilities over that of K equal ones: 0 sure, 1 every class alike). The class map can then be post-processed
//! (post.rs: sits' smoothing of the probabilities, a minimum mapping unit); the uncertainty stays the model's. With
//! its samples kept (aoa.rs), the model's area of applicability: each pixel's dissimilarity index to them and whether
//! it is within the threshold (1), or not (0).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::raster::{Buffer, ColorEntry, ColorTable, RasterCreationOptions};
use gdal::{Dataset, DriverManager};
use janus_cube::CubeInfo;
use serde_json::{Value, json};

use crate::data::{InputBand, Series, fill_gaps, read_window};
use crate::model::{Arch, Model};
use crate::post::{self, PostProcess};

/// A run's window (x0, y0, x1, y1 in the series' pixels) and where its maps go.
#[derive(Clone, Debug, Default)]
pub struct ApplySpec {
    pub window: [usize; 4],
    pub output_dir: String,
    /// the files' stem (e.g. "tempcnn")
    pub stem: String,
    /// pixels a batch (0: by the model's size)
    pub batch: usize,
    /// the class map's post-processing
    pub post: PostProcess,
    /// also the area of applicability (the dissimilarity index and the AOA), if the model kept its samples
    pub aoa: bool,
}

/// The maps written (their descriptions as a Zeit run's outputs) and the window.
pub struct Applied {
    pub outputs: Vec<Value>,
    pub window: [usize; 4],
    pub classified: usize,
}

/// The AOA map's nodata (0 is outside).
const AOA_NODATA: u8 = 255;

/// Pixels in a batch: about 256 MB of the network's activations.
fn batch_of(model: &Model) -> usize {
    if model.forest().is_some() {
        // no activations: the trees on the threads, a band of rows a time
        return 65536;
    }
    let t = model.header.calendar.len().max(1);
    let per_pixel = 4 * t * 512; // the widest layers' activations, float32
    ((256usize << 20) / per_pixel).clamp(256, 65536)
}

/// A GeoTIFF of the window: Byte (its nodata `byte`) or Float32 (NaN).
fn create(
    path: &Path,
    w: usize,
    h: usize,
    byte: Option<u8>,
    info: &CubeInfo,
    win: [usize; 4],
) -> Result<Dataset, String> {
    let driver = DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let opts = RasterCreationOptions::from_iter([
        "COMPRESS=DEFLATE",
        "TILED=YES",
        "BLOCKXSIZE=256",
        "BLOCKYSIZE=256",
        "BIGTIFF=IF_SAFER",
        "NUM_THREADS=ALL_CPUS",
    ]);
    let mut ds = if byte.is_some() {
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
        .set_no_data_value(Some(byte.map_or(f64::NAN, f64::from)))
        .map_err(|e| e.to_string())?;
    Ok(ds)
}

/// The class map's colours and names: value k is class k (1-based).
fn describe_classes(ds: &mut Dataset, model: &Model) -> Result<(), String> {
    let classes: Vec<(&str, [u8; 3])> = model.header.classes.iter().map(|c| (c.name.as_str(), c.color)).collect();
    describe(ds, &[("", [0, 0, 0])].into_iter().chain(classes).collect::<Vec<_>>(), 0)
}

/// A byte map's colours and names, value by value (the nodata transparent).
fn describe(ds: &mut Dataset, values: &[(&str, [u8; 3])], nodata: u16) -> Result<(), String> {
    let mut ct = ColorTable::default();
    for (k, (_, c)) in values.iter().enumerate() {
        let a = if k as u16 == nodata { 0 } else { 255 };
        ct.set_color_entry(k as u16, &ColorEntry::rgba(c[0] as i16, c[1] as i16, c[2] as i16, a));
    }
    if nodata as usize >= values.len() {
        ct.set_color_entry(nodata, &ColorEntry::rgba(0, 0, 0, 0));
    }
    let mut band = ds.rasterband(1).map_err(|e| e.to_string())?;
    band.set_color_table(&ct);
    let names: Vec<std::ffi::CString> =
        values.iter().map(|(n, _)| std::ffi::CString::new(*n).unwrap_or_default()).collect();
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
    let mut class_ds = create(&path("class"), w, h, Some(0), info, spec.window)?;
    describe_classes(&mut class_ds, model)?;
    let mut prob_ds = create(&path("probability"), w, h, None, info, spec.window)?;
    let mut entropy_ds = create(&path("entropy"), w, h, None, info, spec.window)?;
    // the area of applicability: the samples the model kept, as the network read them
    let applicability = match (&model.header.applicability, &model.header.arch) {
        (Some(a), Arch::TempCnn(_) | Arch::LTae(_) | Arch::RandomForest(_)) if spec.aoa => {
            Some((a.train_di()?, a.threshold))
        }
        _ if spec.aoa => return Err("the model kept no samples for its area of applicability".into()),
        _ => None,
    };
    let mut aoa_ds = match &applicability {
        Some(_) => {
            let di = create(&path("di"), w, h, None, info, spec.window)?;
            let mut a = create(&path("aoa"), w, h, Some(AOA_NODATA), info, spec.window)?;
            describe(&mut a, &[("outside the AOA", [215, 48, 39]), ("inside the AOA", [69, 117, 180])], 255)?;
            Some((di, a))
        }
        None => None,
    };
    let k = model.header.classes.len();
    let batch = if spec.batch > 0 { spec.batch } else { batch_of(model) };
    let rows = (batch / w.max(1)).clamp(1, 256).min(h.max(1));
    let bands: Vec<(usize, usize)> = (0..h).step_by(rows).map(|r| (r, rows.min(h - r))).collect();
    let ln_k = (k as f64).ln();
    let t0 = std::time::Instant::now();
    let mut classified = 0;
    // post-processing: the whole map's classes, and its probabilities to smooth
    let mut all_cls = if spec.post.on() { vec![0u8; w * h] } else { vec![] };
    let mut all_probs = if spec.post.smooth { vec![f32::NAN; w * h * k] } else { vec![] };
    let mut kept = |r: usize, i: usize, c: u8, p: &[f32]| {
        if !all_cls.is_empty() {
            all_cls[r * w + i] = c;
        }
        if !all_probs.is_empty() {
            all_probs[(r * w + i) * k..(r * w + i + 1) * k].copy_from_slice(p);
        }
    };
    let mut write_rows = |r: usize, hh: usize, cls: Vec<u8>, prob: Vec<f32>, ent: Vec<f32>| -> Result<(), String> {
        class_ds
            .rasterband(1)
            .map_err(|e| e.to_string())?
            .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), cls))
            .map_err(|e| e.to_string())?;
        for (ds, data) in [(&mut prob_ds, prob), (&mut entropy_ds, ent)] {
            ds.rasterband(1)
                .map_err(|e| e.to_string())?
                .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), data))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let summarize = |p: &[f32]| -> (u8, f32, f32) {
        let (best, &pmax) = p.iter().enumerate().fold((0, &f32::MIN), |a, b| if b.1 > a.1 { b } else { a });
        let hsum: f64 = p.iter().filter(|&&q| q > 0.0).map(|&q| -(q as f64) * (q as f64).ln()).sum();
        (best as u8 + 1, pmax, if ln_k > 0.0 { (hsum / ln_k) as f32 } else { 0.0 })
    };
    if let Arch::Utae(_) = &model.header.arch {
        let standard = model.header.standard.clone().unwrap_or_default();
        crate::dense::classify_windows(
            model,
            info,
            &inputs,
            spec.window,
            &standard,
            progress,
            cancel,
            &mut |r, hh, probs| {
                let n = w * hh;
                let (mut cls, mut prob, mut ent) = (vec![0u8; n], vec![f32::NAN; n], vec![f32::NAN; n]);
                for i in 0..n {
                    let p = &probs[i * k..(i + 1) * k];
                    if p[0].is_finite() {
                        (cls[i], prob[i], ent[i]) = summarize(p);
                        kept(r, i, cls[i], p);
                        classified += 1;
                    }
                }
                write_rows(r, hh, cls, prob, ent)
            },
        )?;
    } else {
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
                if let (Some((td, threshold)), Some((di_ds, aoa_ds))) = (&applicability, &mut aoa_ds) {
                    let d = crate::aoa::di(&sel.values, sel.n, td, -1);
                    let (mut di, mut inside) = (vec![f32::NAN; n], vec![AOA_NODATA; n]);
                    for (j, &i) in idx.iter().enumerate() {
                        if d[j].is_finite() {
                            di[i] = d[j] as f32;
                            inside[i] = (d[j] <= *threshold) as u8;
                        }
                    }
                    di_ds
                        .rasterband(1)
                        .map_err(|e| e.to_string())?
                        .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), di))
                        .map_err(|e| e.to_string())?;
                    aoa_ds
                        .rasterband(1)
                        .map_err(|e| e.to_string())?
                        .write((0, r as isize), (w, hh), &mut Buffer::new((w, hh), inside))
                        .map_err(|e| e.to_string())?;
                }
                let (mut cls, mut prob, mut ent) = (vec![0u8; n], vec![f32::NAN; n], vec![f32::NAN; n]);
                for (j, &i) in idx.iter().enumerate() {
                    (cls[i], prob[i], ent[i]) = summarize(&p[j * k..(j + 1) * k]);
                    kept(r, i, cls[i], &p[j * k..(j + 1) * k]);
                }
                classified += idx.len();
                write_rows(r, hh, cls, prob, ent)?;
                done += hh;
                let el = t0.elapsed().as_secs_f64();
                progress(
                    done as f64 / h as f64,
                    &format!("rows {done}/{h}, {el:.0} s elapsed, ~{:.0} s left", el / done as f64 * (h - done) as f64),
                );
            }
            Ok(())
        })?;
    }
    drop(write_rows);
    drop(kept);
    if spec.post.on() {
        progress(1.0, &format!("post-processing: {}", spec.post.describe()));
        let mut prob = None;
        if spec.post.smooth {
            let (c, p) = post::smooth(&all_probs, w, h, k, &spec.post);
            drop(std::mem::take(&mut all_probs));
            all_cls = c;
            prob = Some(p);
        }
        if spec.post.min_size >= 2 {
            post::sieve(&mut all_cls, w, h, spec.post.min_size);
        }
        class_ds
            .rasterband(1)
            .map_err(|e| e.to_string())?
            .write((0, 0), (w, h), &mut Buffer::new((w, h), all_cls))
            .map_err(|e| e.to_string())?;
        if let Some(p) = prob {
            prob_ds
                .rasterband(1)
                .map_err(|e| e.to_string())?
                .write((0, 0), (w, h), &mut Buffer::new((w, h), p))
                .map_err(|e| e.to_string())?;
        }
    }
    drop((class_ds, prob_ds, entropy_ds, aoa_ds));
    let names: Vec<&str> = model.header.classes.iter().map(|c| c.name.as_str()).collect();
    let arch = model.header.arch.name();
    let post_name = if spec.post.on() { format!(" ({})", spec.post.describe()) } else { String::new() };
    let prob_name = if spec.post.smooth { "Probability of the class (smoothed)" } else { "Probability of the class" };
    let mut outputs = vec![
        json!({"id": "class", "name": format!("Class{post_name}"), "unit": "class", "colormap": "Paired",
               "classes": names, "class_colors": model.header.classes.iter().map(|c| c.color).collect::<Vec<_>>(),
               "path": path("class").to_string_lossy(), "model": arch}),
        json!({"id": "probability", "name": prob_name, "unit": "probability", "colormap": "Viridis",
               "path": path("probability").to_string_lossy()}),
        json!({"id": "entropy", "name": "Uncertainty (entropy)", "unit": "entropy", "colormap": "Plasma",
               "path": path("entropy").to_string_lossy()}),
    ];
    if let Some((_, threshold)) = applicability {
        outputs.push(json!({"id": "di", "name": format!("Dissimilarity index (AOA threshold {threshold:.3})"),
                            "unit": "DI", "colormap": "Hot", "path": path("di").to_string_lossy()}));
        outputs.push(json!({"id": "aoa", "name": "Area of applicability", "unit": "class",
                            "classes": ["outside the AOA", "inside the AOA"],
                            "class_colors": [[215, 48, 39], [69, 117, 180]], "path": path("aoa").to_string_lossy()}));
    }
    Ok(Applied { outputs, window: spec.window, classified })
}
