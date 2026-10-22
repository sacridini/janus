//! SNIC superpixels (zeit.snic; Achanta & Süsstrunk 2017) of a window of the series, on zeit-core's
//! `snic::segment`: every value of a pixel off its position (each date of the shown band or index, or each band of
//! each date) is a feature, so two pixels are alike when their whole trajectories are (sits_segment with the R
//! `snic` package). The seeds are on R snic's grids (zeit.segmentation.snic_grid).
//!
//! Segments never cross the tiles, so the window is segmented one row of tiles at a time (the tiles of a row in
//! parallel): the memory is a row of tiles', and the labels are those of zeit.snic on the whole window with the
//! same tiles. Outputs: the labels (the seed's index, -1 without a segment), each segment's mean of the shown
//! series (a series, opened as a layer) and the segments' polygons in a GeoPackage (zeit.snic_to_polygons:
//! `supercells`, `x`, `y`, `n_pixels` and each feature's mean).

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Dataset;
use serde_json::json;
use zeit_core::snic::{Image, segment};

use crate::job::{Chunk, ChunkOut, Estimate, Inputs, JobResult, JobSpec, Outputs, active_outputs, rows_per_chunk};
use crate::manifest::Tool;
use crate::pixel::Params;
use crate::rng::Pcg64;
use crate::som::fill_gaps;

/// R snic's grids.
pub const GRIDS: [&str; 4] = ["rectangular", "diamond", "hexagonal", "random"];

/// A row of tiles larger than this (its features as float32) is refused: smaller tiles are asked for.
const MAX_STRIP_BYTES: f64 = 4.0 * (1u64 << 30) as f64;

/// The GeoPackage gets each feature's mean as a column up to this many features (SQLite allows 2000 columns).
const MAX_MEAN_COLUMNS: usize = 1000;

/// The seeds of R snic's `snic_grid` (zeit.segmentation.snic_grid): 0-based (row, col) of an `h` x `w` image,
/// every `spacing` pixels (row, col), a margin `padding` free of them (None: half the spacing). "rectangular";
/// "diamond", a second rectangular grid shifted by half a step, the step scaled by sqrt(2); "hexagonal", the column
/// step scaled by sqrt(3); "random", as many seeds as the rectangular grid drawn without replacement
/// (`default_rng(seed).choice`). Errors are zeit's ValueErrors.
pub fn grid(
    h: usize,
    w: usize,
    spacing: [f64; 2],
    padding: Option<[f64; 2]>,
    kind: &str,
    seed: u64,
) -> Result<Vec<(i64, i64)>, String> {
    if !GRIDS.contains(&kind) {
        return Err(format!("ValueError: Unknown grid type '{kind}'; choose from {GRIDS:?}"));
    }
    let padding = padding.unwrap_or([spacing[0] / 2.0, spacing[1] / 2.0]);
    if h < 1 || w < 1 {
        return Err("ValueError: shape must be at least 1 x 1".into());
    }
    if spacing.iter().any(|s| !s.is_finite() || *s <= 1.0) {
        return Err("ValueError: spacing must be finite and > 1".into());
    }
    if padding.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return Err("ValueError: padding must be finite and >= 0".into());
    }
    let (hf, wf) = (h as f64, w as f64);
    if padding[0] >= hf / 2.0 || padding[1] >= wf / 2.0 {
        return Err("ValueError: padding leaves no room for seeds".into());
    }
    let count = |sp: [f64; 2]| {
        (
            ((hf - 2.0 * padding[0] - 1.0) / sp[0]).floor() as i64 + 1,
            ((wf - 2.0 * padding[1] - 1.0) / sp[1]).floor() as i64 + 1,
        )
    };
    let no_room = || "ValueError: no valid seed positions for this spacing/padding".to_string();
    // R: seq(padding + 1, size - padding, length.out = n), 1-based; expand.grid: the rows vary fastest
    let rect = |sp: [f64; 2]| -> Result<Vec<[f64; 2]>, String> {
        let (n_r, n_c) = count(sp);
        if n_r < 1 || n_c < 1 {
            return Err(no_room());
        }
        let axis = |a: f64, b: f64, n: i64| {
            if n == 1 { vec![(a + b) / 2.0] } else { crate::numeric::linspace(a, b, n as usize) }
        };
        let rows = axis(padding[0] + 1.0, hf - padding[0], n_r);
        let cols = axis(padding[1] + 1.0, wf - padding[1], n_c);
        Ok(cols.iter().flat_map(|&c| rows.iter().map(move |&r| [r, c])).collect())
    };
    let staggered = |sp: [f64; 2]| -> Result<Vec<[f64; 2]>, String> {
        let mut base = rect(sp)?;
        let shifted: Vec<[f64; 2]> = base
            .iter()
            .map(|&[r, c]| [r + sp[0] / 2.0, c + sp[1] / 2.0])
            .filter(|&[r, c]| r <= hf - padding[0] && c <= wf - padding[1])
            .collect();
        base.extend(shifted);
        Ok(base)
    };
    let rc = match kind {
        "rectangular" => rect(spacing)?,
        "diamond" => staggered([spacing[0] * 2f64.sqrt(), spacing[1] * 2f64.sqrt()])?,
        "hexagonal" => staggered([spacing[0], spacing[1] * 3f64.sqrt()])?,
        _ => {
            let (n_r, n_c) = count(spacing);
            if n_r < 1 || n_c < 1 {
                return Err(no_room());
            }
            let inner = (
                (hf - 2.0 * padding[0]).round_ties_even() as usize,
                (wf - 2.0 * padding[1]).round_ties_even() as usize,
            );
            let pop = inner.0 * inner.1;
            let idx = Pcg64::new(seed).choice(pop, (n_r * n_c) as usize).into_iter().take(pop);
            // unravel_index(order="F"): the rows vary fastest
            idx.map(|i| {
                let i = i as usize;
                [(i % inner.0) as f64 + 1.0 + padding[0], (i / inner.0) as f64 + 1.0 + padding[1]]
            })
            .collect()
        }
    };
    // R snic rounds (half to even, as numpy) and takes 1-based indices
    let clip = |v: f64, n: usize| (v.round_ties_even() as i64 - 1).clamp(0, n as i64 - 1);
    Ok(rc.into_iter().map(|[r, c]| (clip(r, h), clip(c, w))).collect())
}

/// What a run segments: the shown series, or every band of every date (features date by date, the bands of a date
/// together, as zeit flattens a (time, band, y, x) cube).
struct Features {
    t: usize,
    /// the bands' roles in the spec (every band), none for the shown series
    roles: Vec<String>,
    /// the dates (decimal years) and the parameters, for filling the gaps
    years: Vec<f64>,
    p: Params,
}

impl Features {
    fn new(spec: &JobSpec, p: &Params) -> Result<Features, String> {
        let roles: Vec<String> =
            if p.str("features") == "bands" { spec.bands.keys().cloned().collect() } else { vec![] };
        if p.str("features") == "bands" && roles.is_empty() {
            return Err("ValueError: every band was asked for, but no band was given".into());
        }
        Ok(Features { t: spec.years.len(), roles, years: spec.years.clone(), p: p.clone() })
    }

    fn count(&self) -> usize {
        self.t * self.roles.len().max(1)
    }

    /// The chunk's features (gaps filled as the parameters say; NaN for a pixel left out) into the planar image
    /// `data` ([feature][n]) from pixel `off`, and, with every band, the shown series filled the same way into
    /// `shown` ([T][n]).
    fn put(&self, chunk: &Chunk, data: &mut [f32], shown: &mut [f32], n: usize, off: usize) {
        let (t, np) = (self.t, chunk.pixels());
        let spread = |series: &[f64], stride: usize, first: usize, out: &mut [f32]| {
            let filled = fill_gaps(&chunk.by_pixel(series), t, &self.years, &self.p);
            for px in 0..np {
                for i in 0..t {
                    out[(i * stride + first) * n + off + px] = filled[px * t + i] as f32;
                }
            }
        };
        if self.roles.is_empty() {
            spread(&chunk.stack, 1, 0, data);
            return;
        }
        for (b, role) in self.roles.iter().enumerate() {
            spread(&chunk.bands[role], self.roles.len(), b, data);
        }
        if !shown.is_empty() {
            spread(&chunk.stack, 1, 0, shown);
        }
    }

    /// The features' names: the dates' labels, joined with the bands' names (zeit's `{date}_{band}`).
    fn names(&self, spec: &JobSpec) -> Vec<String> {
        let dates = crate::job::date_labels(spec);
        if self.roles.is_empty() {
            return dates;
        }
        let band = |r: &String| spec.band_names.get(r).cloned().unwrap_or_else(|| r.clone());
        dates.iter().flat_map(|d| self.roles.iter().map(move |r| format!("{d}_{}", band(r)))).collect()
    }
}

/// The seeds of a run on a `w` x `h` window, from its parameters.
fn seeds_of(p: &Params, w: usize, h: usize) -> Result<Vec<(i64, i64)>, String> {
    let s = p.f64("spacing");
    let pad = p.f64("padding") * s;
    grid(h, w, [s, s], Some([pad, pad]), p.str("grid"), p.int("seed").max(0) as u64)
}

/// The tiles' height and width of a run on a `w` x `h` window (0: the whole window).
fn tiles_of(p: &Params, w: usize, h: usize) -> (usize, usize) {
    match p.int("tile").max(0) as usize {
        0 => (h, w),
        t => (t.min(h), t.min(w)),
    }
}

/// The segments of the window: their size, centroid (row, col in the window) and, for the polygons, their mean of
/// every feature.
struct Summary {
    sizes: Vec<i64>,
    centroids: Vec<f64>,
    means: Vec<f64>,
    features: usize,
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let feats = Features::new(spec, p)?;
    let (t, f_n) = (feats.t, feats.count());
    let compactness = p.f64("compactness");
    let seeds = seeds_of(p, w, h)?;
    let (th, tw) = tiles_of(p, w, h);
    let strip_bytes = ((f_n + if feats.roles.is_empty() { 0 } else { t }) * th * w) as f64 * 4.0;
    if strip_bytes > MAX_STRIP_BYTES {
        return Err(format!(
            "ValueError: a row of tiles ({th} x {w} pixels, {f_n} values each) takes {:.1} GB; choose smaller tiles",
            strip_bytes / (1u64 << 30) as f64
        ));
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let wanted = active_outputs(tool, p);
    let rasters: Vec<_> = wanted.iter().copied().filter(|o| o.vector != Some(true)).collect();
    let polygons = wanted.iter().copied().find(|o| o.vector == Some(true));
    let mut outs = Outputs::new(tool, &rasters, &dir, inputs.input.ds(), window, spec)?;
    let mean_out = rasters.iter().any(|o| o.id == "mean");
    let keep_means = polygons.is_some() && f_n <= MAX_MEAN_COLUMNS;

    let k_n = seeds.len();
    let mut sum = Summary {
        sizes: vec![0; k_n],
        centroids: vec![f64::NAN; 2 * k_n],
        means: if keep_means { vec![f64::NAN; k_n * f_n] } else { vec![] },
        features: if keep_means { f_n } else { 0 },
    };
    let strips: Vec<(usize, usize)> = (0..h).step_by(th).map(|r| (r, th.min(h - r))).collect();
    let rows = rows_per_chunk(tool, w);
    let t0 = std::time::Instant::now();
    // the run's share: reading and segmenting the rows of tiles, then the polygons
    let span = if polygons.is_some() { 0.85 } else { 1.0 };
    for (si, &(r0, sh)) in strips.iter().enumerate() {
        let n = sh * w;
        let at = |f: f64| span * (si as f64 + f) / strips.len() as f64;
        let mut data = vec![0f32; f_n * n];
        let mut shown = vec![0f32; if feats.roles.is_empty() || !mean_out { 0 } else { t * n }];
        let mut rr = 0;
        while rr < sh {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let hh = rows.min(sh - rr);
            let chunk = inputs.read(x0, y0 + r0 + rr, w, hh)?;
            feats.put(&chunk, &mut data, &mut shown, n, rr * w);
            rr += hh;
            progress(at(0.5 * rr as f64 / sh as f64), &format!("reading rows {}/{h}", r0 + rr));
        }
        // the seeds of this row of tiles
        let (local, ids): (Vec<(i64, i64)>, Vec<usize>) = seeds
            .iter()
            .enumerate()
            .filter(|(_, (r, _))| (*r as usize) >= r0 && (*r as usize) < r0 + sh)
            .map(|(g, &(r, c))| ((r - r0 as i64, c), g))
            .unzip();
        progress(at(0.5), &format!("segmenting rows {}-{} ({} seeds)", r0 + 1, r0 + sh, local.len()));
        let mut labels = vec![-1i32; n];
        // each segment's mean of the shown series, by its index in this row of tiles
        let mut series = vec![];
        if !local.is_empty() {
            let img = Image { data: &data, features: f_n, rows: sh, cols: w };
            let seg = segment(img, &local, compactness, sh as i64, tw as i64, spec.n_jobs())?;
            for (l, &g) in ids.iter().enumerate() {
                sum.sizes[g] = seg.sizes[l];
                sum.centroids[2 * g] = seg.centroids[2 * l] + r0 as f64;
                sum.centroids[2 * g + 1] = seg.centroids[2 * l + 1];
                if keep_means {
                    sum.means[g * f_n..(g + 1) * f_n].copy_from_slice(&seg.means[l * f_n..(l + 1) * f_n]);
                }
            }
            labels = seg.labels;
            if mean_out && feats.roles.is_empty() {
                series = seg.means;
            } else if mean_out {
                series = shown_means(&labels, &shown, local.len(), t, n);
            }
        }
        drop(data);
        drop(shown);
        // written a band of rows at a time (a series' values as float64 are 8 bytes a value and date)
        let mut rr = 0;
        while rr < sh {
            let hh = rows.min(sh - rr);
            let part = &labels[rr * w..(rr + hh) * w];
            let mut res = ChunkOut::new();
            res.insert(
                "labels".into(),
                part.iter().map(|&l| if l >= 0 { ids[l as usize] as f64 } else { -1.0 }).collect(),
            );
            if mean_out {
                let mut m = vec![f64::NAN; t * hh * w];
                let stride = if feats.roles.is_empty() { f_n } else { t };
                for (px, &l) in part.iter().enumerate() {
                    if l >= 0 {
                        for i in 0..t {
                            m[i * hh * w + px] = series[l as usize * stride + i];
                        }
                    }
                }
                res.insert("mean".into(), m);
            }
            outs.write(&res, r0 + rr, hh)?;
            rr += hh;
        }
        let el = t0.elapsed().as_secs_f64();
        let done = r0 + sh;
        progress(
            at(1.0),
            &format!("rows {done}/{h}, {el:.0} s elapsed, ~{:.0} s left", el / done as f64 * (h - done) as f64),
        );
    }
    let n_segments = sum.sizes.iter().filter(|&&s| s > 0).count();
    let model = json!({ "outputs": { "labels": { "segments": n_segments } } });
    let mut outputs = outs.describe(p, Some(&model));
    let labels_path = dir.join(format!("{}_labels.tif", tool.id));
    drop(outs);
    if let Some(o) = polygons {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(span, &format!("polygons of {n_segments} segments"));
        let path = dir.join(format!("{}_{}.gpkg", tool.id, o.id));
        let names = if keep_means { feats.names(spec) } else { vec![] };
        let written = write_polygons(&labels_path, &path, &sum, &names)?;
        let mut d = serde_json::to_value(o).unwrap_or_default();
        d["path"] = json!(path.to_string_lossy());
        d["features"] = json!(written);
        outputs.push(d);
    }
    progress(1.0, &format!("{n_segments} segments"));
    Ok(JobResult { outputs, window })
}

/// Each segment's mean of the shown series ([segment][T]), over the pixels with a value on that date.
fn shown_means(labels: &[i32], shown: &[f32], k: usize, t: usize, n: usize) -> Vec<f64> {
    let (mut s, mut c) = (vec![0.0; k * t], vec![0u32; k * t]);
    for (px, &l) in labels.iter().enumerate() {
        if l < 0 {
            continue;
        }
        for i in 0..t {
            let v = shown[i * n + px];
            if v.is_finite() {
                s[l as usize * t + i] += v as f64;
                c[l as usize * t + i] += 1;
            }
        }
    }
    s.iter().zip(&c).map(|(&s, &c)| if c > 0 { s / c as f64 } else { f64::NAN }).collect()
}

fn last_error() -> String {
    unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()).to_string_lossy().into_owned() }
}

/// The segments' polygons (GDALPolygonize of the labels, 4-connected as SNIC grows them: one polygon a segment)
/// as a GeoPackage layer "segments": `supercells` (the label), `x`, `y` (the centroid's pixel centre in the map's
/// coordinates), `n_pixels` and each feature's mean (named in `names`; none without names). Returns the polygons
/// written.
fn write_polygons(labels: &Path, path: &Path, sum: &Summary, names: &[String]) -> Result<usize, String> {
    let src = Dataset::open(labels).map_err(|e| format!("{}: {e}", labels.display()))?;
    let band = src.rasterband(1).map_err(|e| e.to_string())?;
    let gt = src.geo_transform().unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let srs = gdal::spatial_ref::SpatialRef::from_wkt(&src.projection()).ok();
    for side in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{side}", path.display()));
    }
    // column names: unique, ignoring case (SQLite's)
    let mut seen = std::collections::HashSet::new();
    let columns: Vec<String> = names
        .iter()
        .map(|n| {
            let mut c = n.clone();
            let mut k = 2;
            while !seen.insert(c.to_lowercase())
                || ["fid", "geom", "supercells", "x", "y", "n_pixels"].contains(&c.as_str())
            {
                c = format!("{n}_{k}");
                k += 1;
            }
            c
        })
        .collect();
    let c = |s: &str| CString::new(s).unwrap_or_default();
    let fail = |what: &str| {
        let m = last_error();
        format!("{}: {what}{}", path.display(), if m.is_empty() { String::new() } else { format!(": {m}") })
    };
    unsafe {
        gdal_sys::CPLErrorReset();
        let drv = gdal_sys::GDALGetDriverByName(c("GPKG").as_ptr());
        if drv.is_null() {
            return Err("no GPKG driver".into());
        }
        let dst = gdal_sys::GDALCreate(
            drv,
            c(&path.to_string_lossy()).as_ptr(),
            0,
            0,
            0,
            gdal_sys::GDALDataType::GDT_Unknown,
            null_mut(),
        );
        if dst.is_null() {
            return Err(fail("cannot create"));
        }
        let close = |ok: Result<usize, String>| {
            gdal_sys::GDALClose(dst);
            if ok.is_err() {
                let _ = std::fs::remove_file(path);
            }
            ok
        };
        let layer = gdal_sys::GDALDatasetCreateLayer(
            dst,
            c("segments").as_ptr(),
            srs.as_ref().map_or(null_mut(), |s| s.to_c_hsrs()),
            gdal_sys::OGRwkbGeometryType::wkbPolygon,
            null_mut(),
        );
        if layer.is_null() {
            return close(Err(fail("cannot create the layer")));
        }
        let mut fields = vec![
            ("supercells".to_string(), gdal_sys::OGRFieldType::OFTInteger),
            ("x".into(), gdal_sys::OGRFieldType::OFTReal),
            ("y".into(), gdal_sys::OGRFieldType::OFTReal),
            ("n_pixels".into(), gdal_sys::OGRFieldType::OFTInteger64),
        ];
        fields.extend(columns.iter().map(|n| (n.clone(), gdal_sys::OGRFieldType::OFTReal)));
        for (name, kind) in &fields {
            let fd = gdal_sys::OGR_Fld_Create(c(name).as_ptr(), *kind);
            let r = gdal_sys::OGR_L_CreateField(layer, fd, 1);
            gdal_sys::OGR_Fld_Destroy(fd);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                return close(Err(fail(&format!("cannot create the field {name}"))));
            }
        }
        gdal_sys::GDALDatasetStartTransaction(dst, 0);
        let mask = gdal_sys::GDALGetMaskBand(band.c_rasterband());
        let r = gdal_sys::GDALPolygonize(band.c_rasterband(), mask, layer, 0, null_mut(), None, null_mut());
        if r != gdal_sys::CPLErr::CE_None {
            gdal_sys::GDALDatasetRollbackTransaction(dst);
            return close(Err(fail("cannot polygonize the labels")));
        }
        // the segments' numbers on their polygons
        let mut written = 0;
        gdal_sys::OGR_L_ResetReading(layer);
        loop {
            let f = gdal_sys::OGR_L_GetNextFeature(layer);
            if f.is_null() {
                break;
            }
            let k = gdal_sys::OGR_F_GetFieldAsInteger(f, 0);
            if k >= 0 && (k as usize) < sum.sizes.len() {
                let k = k as usize;
                let (row, col) = (sum.centroids[2 * k], sum.centroids[2 * k + 1]);
                let (cx, cy) = (col + 0.5, row + 0.5);
                gdal_sys::OGR_F_SetFieldDouble(f, 1, gt[0] + cx * gt[1] + cy * gt[2]);
                gdal_sys::OGR_F_SetFieldDouble(f, 2, gt[3] + cx * gt[4] + cy * gt[5]);
                gdal_sys::OGR_F_SetFieldInteger64(f, 3, sum.sizes[k]);
                for j in 0..columns.len() {
                    let v = sum.means[k * sum.features + j];
                    if v.is_finite() {
                        gdal_sys::OGR_F_SetFieldDouble(f, 4 + j as i32, v);
                    } else {
                        gdal_sys::OGR_F_SetFieldNull(f, 4 + j as i32);
                    }
                }
            }
            let r = gdal_sys::OGR_L_SetFeature(layer, f);
            gdal_sys::OGR_F_Destroy(f);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                gdal_sys::GDALDatasetRollbackTransaction(dst);
                return close(Err(fail("cannot write a polygon")));
            }
            written += 1;
        }
        if gdal_sys::GDALDatasetCommitTransaction(dst) != gdal_sys::OGRErr::OGRERR_NONE {
            return close(Err(fail("cannot write the polygons")));
        }
        close(Ok(written))
    }
}

/// A run's time: SNIC on a square from the middle of the window, one tile on one core, per pixel; the tiles of a
/// row segmented in parallel. The reading and the polygons not included.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let [x0, y0, x1, y1] = spec.window.unwrap_or([0, 0, iw, ih]);
    let (w, h) = (x1 - x0, y1 - y0);
    let feats = Features::new(spec, p)?;
    let side = 128.min(w).min(h).max(1);
    let n = side * side;
    let chunk = inputs.read(x0 + (w - side) / 2, y0 + (h - side) / 2, side, side)?;
    let mut data = vec![0f32; feats.count() * n];
    feats.put(&chunk, &mut data, &mut [], n, 0);
    let seeds = seeds_of(p, side, side).unwrap_or_else(|_| vec![(side as i64 / 2, side as i64 / 2)]);
    let img = Image { data: &data, features: feats.count(), rows: side, cols: side };
    let t = std::time::Instant::now();
    segment(img, &seeds, p.f64("compactness"), 0, 0, 1)?;
    let per_px = t.elapsed().as_secs_f64() / n as f64;
    let (_, tw) = tiles_of(p, w, h);
    let threads = match spec.n_jobs() {
        k if k > 0 => k as usize,
        _ => std::thread::available_parallelism().map_or(4, |k| k.get()),
    };
    let parallel = w.div_ceil(tw).min(threads).max(1) as f64;
    Ok(Estimate { sec_per_px: per_px / parallel, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 })
}
