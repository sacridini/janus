//! Topographic correction (Tools > Data preparation > Topographic correction), as Zeit's `topographic_correction`: the
//! slope and aspect of an elevation model (Horn 1981) give each pixel's illumination for the sun of each date, the
//! reflectance of each band of each date is regressed on it, and the C-correction (Teillet, Guindon & Goodenough 1982)
//! or SCS+C (Soenen, Peddle & Coburn 2005) takes the relief's modulation out. zeit-core does the arithmetic
//! (`topocorr::slope_aspect`, `topocorr::topo_correct`); this module reads the dates, finds the DEM and each date's sun,
//! makes the sample the regression runs on, and writes the corrected series (a file a date, the quality band kept as
//! the last band), the fits (`topo_c.csv`), a before and after check (`validation.csv`) and the report.
//!
//! Every (date, band) fit is independent and the illumination is per date, so the correction is called once per date
//! (the result is the one of a single call over all the dates). The window is held in memory: the bands of a date, the
//! corrected bands, the DEM and its slope and aspect.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::json;
use zeit_core::topocorr::{self as core, Fit, Method};

use crate::dem;
use crate::job::{Estimate, Inputs, JobResult, JobSpec, qa_usable};
use crate::manifest::Tool;
use crate::pixel::Params;
use crate::planes::{self, Tifs};
use crate::sun::{self, Known, Source};
use crate::warp::Grid;

/// A run needing more memory than this (bytes) is refused.
const MAX_BYTES: f64 = 6.0 * (1u64 << 30) as f64;

/// The slope (degrees) from which a pixel counts for the sunlit and shaded slopes of the validation.
const STEEP: f64 = 15.0;

/// The roles of the angle bands: not corrected.
pub const ANGLE_ROLES: [&str; 4] = ["sza", "saa", "vza", "vaa"];

pub(crate) struct Settings {
    pub method: Method,
    pub min_slope: f64,
    pub min_sample: usize,
    pub ndvi: bool,
    pub ndvi_min: f64,
}

pub(crate) fn settings(p: &Params) -> Result<Settings, String> {
    let method = match p.str("method") {
        "c" => Method::C,
        "scs+c" => Method::ScsC,
        other => return Err(format!("ValueError: method must be 'c' or 'scs+c', got {other:?}")),
    };
    let min_slope = p.f64("min_slope");
    if !(min_slope.is_finite() && (0.0..90.0).contains(&min_slope)) {
        return Err(format!("ValueError: min_slope must be a slope of 0 to 90 degrees, got {min_slope}"));
    }
    let min_sample = p.int("min_sample");
    if min_sample < 3 {
        return Err(format!("ValueError: min_sample must be an integer of at least 3, got {min_sample}"));
    }
    let ndvi = match p.str("sample") {
        "slope" => false,
        "ndvi" => true,
        other => return Err(format!("ValueError: sample must be 'slope' or 'ndvi', got {other:?}")),
    };
    Ok(Settings { method, min_slope, min_sample: min_sample as usize, ndvi, ndvi_min: p.f64("ndvi_min") })
}

/// The run's time: reading and writing every band of every date, the illumination and the regressions.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let nb =
        planes::band_set(spec, p.str("bands"), 1, "the topographic correction").map_or(4, |b| b.roles.len()) as f64;
    let t = spec.years.len().max(1) as f64;
    // the download of the elevation model, once
    let fixed = if p.str("dem_source") == "file" { 2.0 } else { 20.0 };
    Ok(Estimate {
        sec_per_px: t * (nb * 1.5e-7 + 4e-7) + 4e-7,
        sec_per_chunk: 0.0,
        chunk_cells: cells,
        sec_fixed: fixed,
    })
}

/// The grid of the run's window: the CRS (WKT), the geotransform and the size, with the metres of its linear unit.
pub(crate) fn window_grid(src: &gdal::Dataset, window: [usize; 4]) -> Result<(Grid, f64), String> {
    let g = src.geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    if g[2] != 0.0 || g[4] != 0.0 {
        return Err("ValueError: the series' grid is rotated: the slope needs a north-up grid".into());
    }
    let wkt = src.projection();
    if wkt.is_empty() {
        return Err(
            "ValueError: the series has no CRS: the slope needs the pixel size in metres, so give it a projected CRS"
                .into(),
        );
    }
    let sr = gdal::spatial_ref::SpatialRef::from_wkt(&wkt).map_err(|e| format!("ValueError: the series' CRS: {e}"))?;
    if sr.is_geographic() {
        return Err(
            "ValueError: the series is in a geographic CRS (degrees): the slope needs a projected CRS in metres. \
                    Reproject it to the UTM zone of the scene first (Tools > Reproject)"
                .into(),
        );
    }
    let unit = sr.linear_units();
    let unit = if unit.is_finite() && unit > 0.0 { unit } else { 1.0 };
    let gt = [g[0] + window[0] as f64 * g[1], g[1], 0.0, g[3] + window[1] as f64 * g[5], 0.0, g[5]];
    Ok((Grid { wkt, gt, w: window[2] - window[0], h: window[3] - window[1] }, unit))
}

/// The slope and aspect (radians) of a DEM of `ny` rows of `nx` columns (metres; `dx` the easting step of a column and
/// `dy` the northing step of a row, negative for a north-up grid, in metres) by Horn's kernel: zeit-core's.
pub fn slope_aspect(z: &[f32], ny: usize, nx: usize, dx: f64, dy: f64, n_jobs: i32) -> (Vec<f32>, Vec<f32>) {
    let z: Vec<f64> = z.iter().map(|&v| f64::from(v)).collect();
    core::slope_aspect(&z, ny, nx, dx, dy, n_jobs)
}

/// `cos i` of every pixel for the sun (`ns` 1: one for the scene, else per pixel), f64 inside, f32 out; NaN where the
/// slope or the sun is.
pub fn cos_incidence(slope: &[f32], aspect: &[f32], zen: &[f32], azi: &[f32], ns: usize) -> Vec<f32> {
    slope
        .iter()
        .zip(aspect)
        .enumerate()
        .map(|(p, (&s, &a))| {
            let k = if ns == 1 { 0 } else { p };
            let (z, saz) = (f64::from(zen[k]).to_radians(), f64::from(azi[k]).to_radians());
            let (s, a) = (f64::from(s), f64::from(a));
            (z.cos() * s.cos() + z.sin() * s.sin() * (saz - a).cos()) as f32
        })
        .collect()
}

/// The sample of the regression (`min_slope` as Zeit's default, with the quality band, and the vegetation) as a byte
/// mask; None when Zeit's own default (the slope alone) is the sample.
fn sample_mask(slope: &[f32], min_slope: f32, usable: Option<&[bool]>, ndvi: Option<(&[f64], f64)>) -> Option<Vec<u8>> {
    if usable.is_none() && ndvi.is_none() {
        return None;
    }
    // Zeit's own test: sin(slope) >= sin(min_slope), in f32
    let floor = min_slope.sin();
    Some(
        slope
            .iter()
            .enumerate()
            .map(|(p, &s)| {
                let steep = s.sin() >= floor;
                let clear = usable.is_none_or(|u| u[p]);
                let green = ndvi.is_none_or(|(v, t)| v[p] > t);
                u8::from(steep && clear && green)
            })
            .collect(),
    )
}

/// Pearson's r of the pairs.
fn pearson(pairs: impl Iterator<Item = (f64, f64)>) -> f64 {
    let (mut n, mut sx, mut sy, mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (x, y) in pairs {
        n += 1.0;
        sx += x;
        sy += y;
        sxx += x * x;
        syy += y * y;
        sxy += x * y;
    }
    let (vx, vy) = (sxx - sx * sx / n, syy - sy * sy / n);
    if n < 3.0 || !(vx > 0.0 && vy > 0.0) { f64::NAN } else { (sxy - sx * sy / n) / (vx * vy).sqrt() }
}

/// The smallest angle between two directions in degrees.
fn angle_between(a: f64, b: f64) -> f64 {
    let d = (a - b).rem_euclid(360.0);
    d.min(360.0 - d)
}

/// What the check of a band of a date says: the correlation of the reflectance with cos i on the sample, and the mean
/// reflectance of the steep slopes facing the sun and facing away, before and after.
#[derive(Clone, Debug, Default)]
pub struct Check {
    pub r_before: f64,
    pub r_after: f64,
    pub sunlit_before: f64,
    pub shaded_before: f64,
    pub sunlit_after: f64,
    pub shaded_after: f64,
    pub n_sunlit: usize,
    pub n_shaded: usize,
}

/// The check of one band: `before` and `after` its values, over the pixels `in_sample`, with `cosi`; the steep slopes
/// by their aspect against the sun's azimuth (`azi`, `ns` as everywhere), the pixels `usable`.
#[allow(clippy::too_many_arguments)]
fn check_band(
    before: &[f32],
    after: &[f32],
    cosi: &[f32],
    in_sample: &[bool],
    slope: &[f32],
    aspect: &[f32],
    azi: &[f32],
    ns: usize,
    usable: Option<&[bool]>,
) -> Check {
    let on = |p: usize| in_sample[p] && cosi[p].is_finite() && before[p].is_finite();
    let r_before = pearson((0..before.len()).filter(|&p| on(p)).map(|p| (f64::from(cosi[p]), f64::from(before[p]))));
    let r_after = pearson(
        (0..before.len()).filter(|&p| on(p) && after[p].is_finite()).map(|p| (f64::from(cosi[p]), f64::from(after[p]))),
    );
    let steep = STEEP.to_radians() as f32;
    let (mut sb, mut sa, mut ns_l) = (0.0, 0.0, 0usize);
    let (mut hb, mut ha, mut ns_h) = (0.0, 0.0, 0usize);
    for p in 0..before.len() {
        if !(slope[p] >= steep && before[p].is_finite() && after[p].is_finite()) || usable.is_some_and(|u| !u[p]) {
            continue;
        }
        let saz = f64::from(azi[if ns == 1 { 0 } else { p }]);
        let facing = f64::from(aspect[p]).to_degrees();
        if angle_between(facing, saz) <= 45.0 {
            sb += f64::from(before[p]);
            sa += f64::from(after[p]);
            ns_l += 1;
        } else if angle_between(facing, saz + 180.0) <= 45.0 {
            hb += f64::from(before[p]);
            ha += f64::from(after[p]);
            ns_h += 1;
        }
    }
    let mean = |s: f64, n: usize| if n > 0 { s / n as f64 } else { f64::NAN };
    Check {
        r_before,
        r_after,
        sunlit_before: mean(sb, ns_l),
        sunlit_after: mean(sa, ns_l),
        shaded_before: mean(hb, ns_h),
        shaded_after: mean(ha, ns_h),
        n_sunlit: ns_l,
        n_shaded: ns_h,
    }
}

/// Why a band was copied.
fn why_not(fit: &Fit, min_sample: usize) -> String {
    if fit.c.is_finite() {
        String::new()
    } else if fit.n < min_sample.max(3) {
        format!("fewer than {} sample pixels ({})", min_sample.max(3), fit.n)
    } else if !fit.slope.is_finite() {
        "no spread in cos i".into()
    } else if fit.slope <= 0.0 {
        format!("the reflectance does not rise with the illumination (slope {:.4} <= 0)", fit.slope)
    } else {
        "c is not finite".into()
    }
}

/// The bands of the series' bands the run corrects, without the angle bands and the quality band.
fn corrected_bands(spec: &JobSpec, p: &Params) -> Result<planes::BandSet, String> {
    planes::band_set(spec, p.str("bands"), 1, "the topographic correction")
}

/// The sun of a date, as the fits' table says it: the scene's, or the mean over the pixels that have one.
fn sun_numbers(zen: &[f32], azi: &[f32]) -> (f64, f64) {
    let mean = |v: &[f32]| {
        let ok: Vec<f64> = v.iter().filter(|x| x.is_finite()).map(|&x| f64::from(x)).collect();
        if ok.is_empty() { f64::NAN } else { ok.iter().sum::<f64>() / ok.len() as f64 }
    };
    (
        mean(zen),
        janus_cube::stac::circular_mean_deg(
            &azi.iter().filter(|x| x.is_finite()).map(|&x| f64::from(x)).collect::<Vec<_>>(),
        )
        .unwrap_or(f64::NAN),
    )
}

pub(crate) fn run(
    spec: &JobSpec,
    _tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let set = settings(p)?;
    let bands = corrected_bands(spec, p)?;
    let nb = bands.roles.len();
    let t = spec.years.len();
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = w * h;
    let (grid, unit) = window_grid(inputs.input.ds(), window)?;
    let series_gt = inputs.input.ds().geo_transform().map_err(|e| e.to_string())?;
    let series_wkt = inputs.input.ds().projection();
    let bytes = n as f64 * (8.0 * nb as f64 + 64.0);
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window ({w} x {h} pixels, {nb} bands) needs about {:.1} GB of memory, over the {:.0} GB limit: \
             choose the ROI or the visible area",
            bytes / (1u64 << 30) as f64,
            MAX_BYTES / (1u64 << 30) as f64
        ));
    }
    if set.ndvi && !(spec.bands.contains_key("red") && spec.bands.contains_key("nir")) {
        return Err(
            "ValueError: the vegetation sample needs the bands red and nir mapped (the Bands of the series)".into()
        );
    }
    let dir = PathBuf::from(&spec.output_dir);
    let n_jobs = spec.n_jobs();
    let labels = crate::job::date_labels(spec);

    // ---- the sun of each date, before anything is read
    let have_bands = spec.bands.contains_key("sza") && spec.bands.contains_key("saa");
    let known = Known {
        hints: &spec.suns,
        bands: have_bands,
        granules: &spec.granules,
        centre: sun::centre_lat_lon(&series_wkt, &series_gt, window),
    };
    let plan = sun::plan(p, &known, t);
    if let Some(e) = sun::missing_error(&plan, &labels) {
        return Err(e);
    }
    for s in &plan {
        if let Source::Scene { zenith, azimuth, .. } = s
            && !(zenith.is_finite() && (0.0..90.0).contains(zenith) && azimuth.is_finite())
        {
            return Err(format!(
                "ValueError: the sun zenith must be 0 to 90 degrees, got {zenith} (azimuth {azimuth})"
            ));
        }
    }
    let granule_dates: Vec<usize> = (0..t).filter(|&k| plan[k] == Source::Granules).collect();
    let angles = if granule_dates.is_empty() {
        None
    } else {
        let sub: Vec<Vec<String>> = granule_dates.iter().map(|&k| spec.granules[k].clone()).collect();
        let days: Option<Vec<i64>> = spec.days.as_ref().map(|d| granule_dates.iter().map(|&k| d[k]).collect());
        Some(
            crate::granules::Angles::new(&sub, days.as_deref(), sub.len(), "B06", &series_wkt, series_gt)
                .map_err(|e| format!("ValueError: the sun from Sentinel-2's metadata: {e}"))?,
        )
    };

    // ---- the elevation model on the window's grid, and its slope and aspect
    progress(0.0, "the elevation model");
    let (dem_file, dem_note): (PathBuf, Option<String>) = if p.str("dem_source") == "file" {
        let f = p.str("dem");
        if f.is_empty() || !Path::new(f).exists() {
            return Err(format!("ValueError: the DEM file {f:?} does not exist: choose it in the tool's form"));
        }
        (PathBuf::from(f), None)
    } else {
        let collection = p.str("dem_source");
        let cache = dir.parent().unwrap_or(&dir).join("dem");
        let sub = |f: f64, m: &str| progress(0.05 * f, m);
        let f =
            dem::copernicus(collection, &grid, &cache, p.str("dem_resampling"), n_jobs.max(1) as usize, &sub, cancel)?;
        (f, Some(dem::notice(collection)))
    };
    let elevation = dem::on_grid(&dem_file, &grid, p.str("dem_resampling"), n_jobs.max(1) as usize)?;
    if elevation.iter().all(|v| v.is_nan()) {
        return Err("ValueError: the elevation model has no value over the window".into());
    }
    let z: Vec<f64> = elevation.iter().map(|&v| f64::from(v)).collect();
    drop(elevation);
    let (slope, aspect) = core::slope_aspect(&z, h, w, grid.gt[1] * unit, grid.gt[5] * unit, n_jobs);
    drop(z);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }

    // ---- the files
    let out_dir = dir.join("series");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let file_names = planes::date_file_names(spec);
    let paths: Vec<PathBuf> = (0..t).map(|k| out_dir.join(format!("{}.tif", file_names[k]))).collect();
    let has_qa = inputs.qa_rule().is_some();
    let mut names = bands.names.clone();
    if has_qa {
        names.push(spec.band_names.get("qa").cloned().unwrap_or_else(|| "quality".into()));
    }
    let mut tifs = Tifs::create(paths, &names, inputs.input.ds(), window, spec)?;
    if let Some(note) = &dem_note {
        tifs.set_metadata("COPERNICUS_DEM_NOTICE", note)?;
        tifs.set_metadata("TIFFTAG_COPYRIGHT", note)?;
    }
    tifs.set_metadata("TOPOCORR_METHOD", p.str("method"))?;
    let cosi_dir = dir.join("cosi");
    let mut cosi_tifs = if p.bool("write_cosi") {
        std::fs::create_dir_all(&cosi_dir).map_err(|e| e.to_string())?;
        let cp: Vec<PathBuf> = (0..t).map(|k| cosi_dir.join(format!("{}.tif", file_names[k]))).collect();
        Some(Tifs::create(cp, &["cos i".to_string()], inputs.input.ds(), window, spec)?)
    } else {
        None
    };

    // ---- each date: read, correct, check, write
    let min_slope_rad = set.min_slope.to_radians() as f32;
    let strip = planes::strip_rows(1, nb, w, h);
    let mut fit_rows: Vec<Vec<String>> = vec![];
    let mut check_rows: Vec<Vec<String>> = vec![];
    let mut guards: Vec<String> = vec![];
    let mut sources: Vec<String> = vec![];
    let (mut sum_before, mut sum_after, mut count_r) = (0.0, 0.0, 0usize);
    for k in 0..t {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.05 + 0.9 * k as f64 / t as f64, &format!("{} ({}/{t})", labels[k], k + 1));
        // the sun: one for the scene, or per pixel
        let (zen, azi, ns): (Vec<f32>, Vec<f32>, usize) = match &plan[k] {
            Source::Scene { zenith, azimuth, .. } => (vec![*zenith as f32], vec![*azimuth as f32], 1),
            Source::Bands => {
                let scale = if p.str("angle_scale") == "hundredths" { 100.0 } else { 1.0 };
                let a = inputs.read_dates_with(&["sza".to_string(), "saa".to_string()], &[k], x0, y0, w, h, false)?;
                let a = &a[0];
                (
                    a[..n].iter().map(|&v| (v / scale) as f32).collect(),
                    a[n..].iter().map(|&v| (v / scale) as f32).collect(),
                    n,
                )
            }
            Source::Granules => {
                let at = granule_dates.iter().position(|&d| d == k).unwrap_or(0);
                let [zen, azi, _, _] = angles.as_ref().ok_or("no granule angles")?.block(at, [x0, y0, w, h])?;
                (zen, azi, n)
            }
            Source::Missing(_) => return Err("a date has no sun".into()),
        };
        let (sun_z, sun_a) = sun_numbers(&zen, &azi);
        sources.push(plan[k].name().to_string());
        // the bands of the date, strip by strip into float32
        let mut x = vec![0f32; nb * n];
        let mut r = 0;
        while r < h {
            let hh = strip.min(h - r);
            let np = w * hh;
            let plane = inputs.read_dates_with(&bands.roles, &[k], x0, y0 + r, w, hh, false)?;
            for b in 0..nb {
                for (dst, &v) in x[b * n + r * w..b * n + r * w + np].iter_mut().zip(&plane[0][b * np..(b + 1) * np]) {
                    *dst = v as f32;
                }
            }
            r += hh;
        }
        let codes = inputs.qa_codes(&[k], x0, y0, w, h)?.map(|mut c| c.swap_remove(0));
        let rule = inputs.qa_rule().map(str::to_string);
        let usable: Option<Vec<bool>> =
            codes.as_ref().map(|c| c.iter().map(|&code| qa_usable(rule.as_deref(), code)).collect());
        // the vegetation: NDVI of the date
        let ndvi: Option<Vec<f64>> = if set.ndvi {
            let rn = inputs.read_dates_with(&["red".to_string(), "nir".to_string()], &[k], x0, y0, w, h, false)?;
            let (red, nir) = rn[0].split_at(n);
            Some(red.iter().zip(nir).map(|(&r, &n)| if r + n != 0.0 { (n - r) / (n + r) } else { f64::NAN }).collect())
        } else {
            None
        };
        let mask = sample_mask(&slope, min_slope_rad, usable.as_deref(), ndvi.as_deref().map(|v| (v, set.ndvi_min)));
        let mut out = vec![0f32; nb * n];
        let fits = core::topo_correct(
            &x,
            1,
            nb,
            n,
            &slope,
            &aspect,
            &zen,
            &azi,
            ns,
            mask.as_deref().map(|m| (m, 1)),
            set.method,
            min_slope_rad,
            set.min_sample,
            &mut out,
            n_jobs,
        );
        // the check, on the sample the fit used
        let cosi = cos_incidence(&slope, &aspect, &zen, &azi, ns);
        let sin_floor = min_slope_rad.sin();
        let in_sample: Vec<bool> = match &mask {
            Some(m) => m.iter().map(|&v| v != 0).collect(),
            None => slope.iter().map(|&s| s.sin() >= sin_floor).collect(),
        };
        for (b, fit) in fits.iter().enumerate() {
            let (before, after) = (&x[b * n..(b + 1) * n], &out[b * n..(b + 1) * n]);
            let c = check_band(before, after, &cosi, &in_sample, &slope, &aspect, &azi, ns, usable.as_deref());
            if fit.c.is_finite() && c.r_before.is_finite() && c.r_after.is_finite() {
                sum_before += c.r_before.abs();
                sum_after += c.r_after.abs();
                count_r += 1;
            }
            let why = why_not(fit, set.min_sample);
            if !why.is_empty() {
                guards.push(format!("{} {}: copied unchanged ({why})", labels[k], bands.names[b]));
            }
            let num = planes::csv_num;
            fit_rows.push(vec![
                labels[k].clone(),
                bands.names[b].clone(),
                p.str("method").to_string(),
                num(fit.c),
                num(fit.slope),
                num(fit.intercept),
                fit.n.to_string(),
                fit.skipped.to_string(),
                fit.c.is_finite().to_string(),
                why,
                num(sun_z),
                num(sun_a),
                plan[k].name().to_string(),
            ]);
            check_rows.push(vec![
                labels[k].clone(),
                bands.names[b].clone(),
                num(c.r_before),
                num(c.r_after),
                num(c.sunlit_before),
                num(c.shaded_before),
                num(c.sunlit_after),
                num(c.shaded_after),
                c.n_sunlit.to_string(),
                c.n_shaded.to_string(),
            ]);
        }
        // the corrected date, strip by strip, with the quality band as it was
        let mut r = 0;
        while r < h {
            let hh = strip.min(h - r);
            let mut planes_out: Vec<f32> = Vec::with_capacity(names.len() * w * hh);
            for b in 0..nb {
                planes_out.extend_from_slice(&out[b * n + r * w..b * n + (r + hh) * w]);
            }
            if let Some(c) = &codes {
                planes_out.extend(c[r * w..(r + hh) * w].iter().map(|&v| v as f32));
            }
            tifs.write(k, r, hh, &planes_out)?;
            if let Some(ct) = cosi_tifs.as_mut() {
                ct.write(k, r, hh, &cosi[r * w..(r + hh) * w])?;
            }
            r += hh;
        }
    }
    drop(tifs);
    drop(cosi_tifs);

    // ---- the tables and the report
    planes::write_csv(
        &dir.join("topo_c.csv"),
        &[
            "date",
            "band",
            "method",
            "c",
            "slope_m",
            "intercept_b",
            "n",
            "skipped",
            "fitted",
            "why_not",
            "sun_zenith",
            "sun_azimuth",
            "sun_from",
        ],
        &fit_rows,
    )?;
    planes::write_csv(
        &dir.join("validation.csv"),
        &[
            "date",
            "band",
            "r_before",
            "r_after",
            "sunlit_mean_before",
            "shaded_mean_before",
            "sunlit_mean_after",
            "shaded_mean_after",
            "n_sunlit",
            "n_shaded",
        ],
        &check_rows,
    )?;
    let mean_before = if count_r > 0 { sum_before / count_r as f64 } else { f64::NAN };
    let mean_after = if count_r > 0 { sum_after / count_r as f64 } else { f64::NAN };
    let dem_text = match &dem_note {
        Some(note) => format!(
            "{} ({}), warped onto the window's grid ({})\n{}\n{}\n",
            p.str("dem_source"),
            dem_file.display(),
            p.str("dem_resampling"),
            note,
            dem::liability(p.str("dem_source")),
        ),
        None => {
            format!("{} (a file), warped onto the window's grid ({})\n", dem_file.display(), p.str("dem_resampling"))
        }
    };
    let mut report = format!(
        "Topographic correction: {} on {} dates of {nb} bands, window {w} x {h} pixels\n\nDEM: {dem_text}\nSample of the regression: {}{}\nMinimum slope: {} degrees, fewest pixels for a fit: {}\n\nSun of each date:\n",
        match set.method {
            Method::C => "C-correction (Teillet et al. 1982)",
            Method::ScsC => "SCS+C (Soenen et al. 2005)",
        },
        t,
        if set.ndvi { format!("vegetation (NDVI > {})", set.ndvi_min) } else { "every pixel on a slope".into() },
        if has_qa { ", clouds, shadows and snow of the quality band left out" } else { "" },
        set.min_slope,
        set.min_sample,
    );
    for (k, s) in sources.iter().enumerate() {
        report += &format!("  {}: {s}\n", labels[k]);
    }
    report += &format!(
        "\nMean |r(reflectance, cos i)| on the sample over the bands fitted: {mean_before:.4} before, {mean_after:.4} after\n"
    );
    if guards.is_empty() {
        report += "\nEvery band of every date was fitted and corrected.\n";
    } else {
        report += &format!("\nBands copied unchanged ({}):\n", guards.len());
        for g in guards.iter().take(200) {
            report += &format!("  {g}\n");
        }
    }
    report += "\nPixels the correction leaves as they are: cos i + c below 0.01, a non-positive numerator, a sun zenith outside 0-90 degrees, a DEM cell next to a missing one, a missing reflectance.\n";
    report += "The quality band of the series is kept as the last band of each file and is not applied to the corrected values.\n";
    std::fs::write(dir.join("report.txt"), report).map_err(|e| e.to_string())?;
    let record = json!({
        "method": p.str("method"), "dem_source": p.str("dem_source"), "dem": dem_file.to_string_lossy(),
        "notice": dem_note, "sample": p.str("sample"), "min_slope": set.min_slope, "min_sample": set.min_sample,
        "sun": sources, "bands": bands.names, "mean_abs_r_before": mean_before, "mean_abs_r_after": mean_after,
        "bands_copied": guards.len(),
    });
    std::fs::write(dir.join("topocorr.json"), serde_json::to_string_pretty(&record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    let mut output = json!({
        "id": "corrected", "name": "Topographically corrected", "unit": "as the series",
        "series": true, "path": out_dir.to_string_lossy(),
    });
    if has_qa {
        output["qa_band"] = json!(nb + 1);
        output["qa_rule"] = json!(spec.qa_rule);
    }
    progress(1.0, "done");
    Ok(JobResult { outputs: vec![output], window })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearson_of_a_line_is_one() {
        let r = pearson((0..50).map(|i| (f64::from(i), 3.0 + 2.0 * f64::from(i))));
        assert!((r - 1.0).abs() < 1e-12);
        assert!(pearson((0..50).map(|i| (1.0, f64::from(i)))).is_nan());
    }

    #[test]
    fn angles_wrap_round() {
        assert!((angle_between(350.0, 10.0) - 20.0).abs() < 1e-12);
        assert!((angle_between(10.0, 190.0) - 180.0).abs() < 1e-12);
    }

    /// Zeit's default sample (the slope alone) is no mask; a quality band or the vegetation make one with the same slope
    /// test.
    #[test]
    fn the_sample_is_zeits_default_or_a_mask_with_the_same_slope_test() {
        let slope = [0.0f32, 0.05, 0.2, 0.3];
        assert!(sample_mask(&slope, 0.1, None, None).is_none());
        let usable = [true, true, false, true];
        let m = sample_mask(&slope, 0.1, Some(&usable), None).unwrap();
        assert_eq!(m, vec![0, 0, 0, 1]);
        let ndvi = [0.9, 0.9, 0.9, 0.2];
        let m = sample_mask(&slope, 0.1, None, Some((&ndvi, 0.5))).unwrap();
        assert_eq!(m, vec![0, 0, 1, 0]);
    }

    #[test]
    fn a_band_copied_says_why() {
        let nan = f64::NAN;
        let f = |c, slope, n| Fit { c, slope, intercept: nan, n, skipped: 0 };
        assert!(why_not(&f(0.5, 1.0, 500), 100).is_empty());
        assert!(why_not(&f(nan, nan, 10), 100).contains("fewer than 100"));
        assert!(why_not(&f(nan, nan, 500), 100).contains("no spread"));
        assert!(why_not(&f(nan, -0.2, 500), 100).contains("does not rise"));
    }

    /// A geographic CRS is refused with the hint to reproject.
    #[test]
    fn a_geographic_series_is_refused() {
        let mem = gdal::DriverManager::get_driver_by_name("MEM").unwrap();
        let mut ds = mem.create_with_band_type::<f32, _>("", 4, 4, 1).unwrap();
        ds.set_geo_transform(&[-46.0, 0.001, 0.0, -22.0, 0.0, -0.001]).unwrap();
        ds.set_projection(&gdal::spatial_ref::SpatialRef::from_epsg(4326).unwrap().to_wkt().unwrap()).unwrap();
        let e = window_grid(&ds, [0, 0, 4, 4]).unwrap_err();
        assert!(e.contains("geographic CRS") && e.contains("Reproject"), "{e}");
        let mut ds = mem.create_with_band_type::<f32, _>("", 4, 4, 1).unwrap();
        ds.set_geo_transform(&[350000.0, 30.0, 0.0, 7510000.0, 0.0, -30.0]).unwrap();
        ds.set_projection(&gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap()).unwrap();
        let (grid, unit) = window_grid(&ds, [1, 1, 3, 4]).unwrap();
        assert_eq!((grid.w, grid.h, unit), (2, 3, 1.0));
        assert_eq!(grid.gt[0], 350030.0);
    }
}
