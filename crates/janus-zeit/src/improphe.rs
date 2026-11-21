//! ImproPhe (Tools > Data preparation > ImproPhe (FORCE)), as Zeit's improphe: a coarse series sharpened to the
//! pixels of the run's own, finer one (FORCE's CF- and L2-ImproPhe; Frantz et al. 2016). The run's series is the
//! fine one (every band of each date, the quality band applied) and the output is on its grid and window; the coarse
//! series is another one (`JobSpec::other`), put on the fine grid by nearest neighbour as Zeit does. The principal
//! components are of the whole window and the circle of a pixel reaches beyond any strip, so a raster run holds the
//! window's values (the fine series as float32, the coarse bands, the result) and computes it at once.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Dataset;
use zeit_core::improphe::{self as core, Params as CoreParams};

use crate::job::{
    ChunkOut, Estimate, Inputs, JobResult, JobSpec, OtherSeries, Outputs, active_outputs, qa_usable, read,
    rows_per_chunk,
};
use crate::manifest::Tool;
use crate::numeric::civil;
use crate::pixel::Params;

/// A run needing more memory than this (bytes) is refused.
const MAX_BYTES: f64 = 6.0 * (1u64 << 30) as f64;

const NO_COARSE: &str =
    "ValueError: choose the coarse series to sharpen (another open series, in the same CRS as this one)";

/// Days of the year before each month in FORCE's calendar (Zeit's `_CUM`: 29 February is day 60, with 1 March).
const CUM: [i32; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// The seasonal windows' limits from the parameter's text ("1,91,181,271,365"), checked as Zeit checks them.
fn seasons(text: &str) -> Result<Vec<i32>, String> {
    let limits: Vec<i32> = text
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<i32>().map_err(|_| {
                format!("ValueError: the seasonal windows are days of the year separated by commas, got {text:?}")
            })
        })
        .collect::<Result<_, _>>()?;
    if limits.len() < 2 || limits.iter().any(|&s| !(1..=365).contains(&s)) {
        return Err("ValueError: seasons must be at least two integer days of the year in 1..365".into());
    }
    Ok(limits)
}

/// FORCE's `(int)(size / RESOLUTION)`: the radius in pixels (a size that is not a multiple of the pixel size is
/// truncated, as Zeit does), at least `minimum`.
fn radius(size: f64, res: f64, what: &str, minimum: usize) -> Result<usize, String> {
    let r = size / res;
    let k = (r + 1e-9).floor();
    if !k.is_finite() || k < minimum as f64 {
        return Err(format!(
            "ValueError: {what}={size} is {r:.2} pixels of {res}: the radius must be at least {minimum} pixel(s)"
        ));
    }
    Ok(k as usize)
}

/// The settings of a run, checked.
struct Settings {
    dwin: Vec<i32>,
    min_var: f32,
    force: bool,
    force_pca: bool,
}

fn settings(p: &Params) -> Result<Settings, String> {
    let pca = match p.str("pca") {
        "force" => true,
        "principal" => false,
        "default" | "" => p.bool("force"),
        o => return Err(format!("ValueError: pca must be 'default', 'principal' or 'force', got {o:?}")),
    };
    let var = p.f64("pc_variance");
    if !(var > 0.0 && var <= 1.0) {
        return Err(format!("ValueError: pc_variance must be in (0, 1], got {var}"));
    }
    Ok(Settings { dwin: seasons(p.str("seasons"))?, min_var: var as f32, force: p.bool("force"), force_pca: pca })
}

/// The decimal-year-or-date of a layer as its year (the whole years of a field's layers).
fn layer_years(other: &OtherSeries) -> Vec<i64> {
    match &other.days {
        Some(d) if d.len() == other.years.len() => d.iter().map(|&d| civil(d).0).collect(),
        _ => other.years.iter().map(|y| (y + 1e-9).floor() as i64).collect(),
    }
}

/// Whether the coarse series is a field with a layer a year (CF-ImproPhe) or scenes (L2-ImproPhe), from the
/// parameter `by` as Zeit's `_layout` reads it.
fn by_year(other: &OtherSeries, by: &str) -> Result<bool, String> {
    let years = layer_years(other);
    let mut unique = years.clone();
    unique.sort_unstable();
    unique.dedup();
    match by {
        "year" => {
            if unique.len() != years.len() {
                return Err(format!(
                    "ValueError: a field with a layer a year needs different years, the coarse series' are {years:?}"
                ));
            }
            Ok(true)
        }
        "scenes" => Ok(false),
        "auto" | "" => {
            if other.days.as_ref().is_some_and(|d| d.len() == years.len()) {
                if unique.len() < years.len() {
                    return Ok(false);
                }
                return Err("ValueError: the coarse series has one date a year: say if it is a field with a layer a \
                            year (CF-ImproPhe) or the scenes of a sensor (L2-ImproPhe)"
                    .into());
            }
            if other.years.iter().all(|y| y.fract() == 0.0 && (1900.0..=2300.0).contains(y))
                && unique.len() == years.len()
            {
                return Ok(true);
            }
            Err("ValueError: cannot tell what the coarse series is: say if it is a field with a layer a year \
                 (CF-ImproPhe) or the scenes of a sensor (L2-ImproPhe)"
                .into())
        }
        o => Err(format!("ValueError: by must be 'auto', 'year' or 'scenes', got {o:?}")),
    }
}

/// The labels of the output's bands: the coarse series' dates (each of its bands after its date when it has several).
pub(crate) fn bands(spec: &JobSpec, _p: &Params) -> Result<Vec<String>, String> {
    let other = spec.other.as_ref().ok_or(NO_COARSE)?;
    let n = other.years.len();
    let dates: Vec<String> = match (&other.labels, &other.days) {
        (Some(l), _) if l.len() == n => l.clone(),
        (_, Some(d)) if d.len() == n => d
            .iter()
            .map(|&d| {
                let (y, m, dd) = civil(d);
                format!("{y:04}-{m:02}-{dd:02}")
            })
            .collect(),
        _ => other.years.iter().map(|y| format!("{y:.4}")).collect(),
    };
    if other.inputs.len() <= 1 {
        return Ok(dates);
    }
    let name = |f: usize| {
        other.band_names.get(f).filter(|s| !s.is_empty()).cloned().unwrap_or_else(|| format!("band {}", f + 1))
    };
    Ok(dates
        .iter()
        .flat_map(|d| (0..other.inputs.len()).map(move |f| (d, f)))
        .map(|(d, f)| format!("{d} {}", name(f)))
        .collect())
}

/// Whether two CRS (WKT) are the same, ignoring the axis order of geographic ones.
pub(crate) fn same_crs(a: &str, b: &str) -> Result<bool, String> {
    use gdal::spatial_ref::SpatialRef;
    if a == b {
        return Ok(true);
    }
    let (sa, sb) =
        (SpatialRef::from_wkt(a).map_err(|e| e.to_string())?, SpatialRef::from_wkt(b).map_err(|e| e.to_string())?);
    let option = std::ffi::CString::new("CRITERION=EQUIVALENT_EXCEPT_AXIS_ORDER_GEOGCRS").expect("no NUL");
    let options = [option.as_ptr(), std::ptr::null()];
    // SAFETY: both handles live through the call, the options are a NULL-terminated list of C strings.
    Ok(unsafe { gdal_sys::OSRIsSameEx(sa.to_c_hsrs(), sb.to_c_hsrs(), options.as_ptr()) } != 0)
}

/// The coarse series on the window of the fine grid: `[date][band of a date][pixel]` float32, NaN where it has no
/// value (its NoData, outside its extent), nearest neighbour as FORCE's (and Zeit's) read: each fine pixel takes the
/// coarse pixel its centre falls in.
fn coarse_on_window(
    other: &OtherSeries,
    fine: &Dataset,
    window: [usize; 4],
    cancel: &AtomicBool,
) -> Result<Vec<f32>, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, nf) = (other.years.len(), other.inputs.len());
    let fg = fine.geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    let mut out = vec![f32::NAN; t * nf * w * h];
    for (f, path) in other.inputs.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let ds = Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
        if ds.raster_count() != t {
            return Err(format!(
                "ValueError: the coarse series' band {} has {} bands but {t} dates were given",
                f + 1,
                ds.raster_count()
            ));
        }
        let cg = ds.geo_transform().map_err(|e| format!("ValueError: the coarse series has no geotransform ({e})"))?;
        if cg[2] != 0.0 || cg[4] != 0.0 || cg[1] == 0.0 || cg[5] == 0.0 {
            return Err("ValueError: the coarse series' grid is rotated: reproject it first".into());
        }
        let (cw, ch) = ds.raster_size();
        let res = fg[1].abs();
        let same_grid = (cw, ch) == fine.raster_size()
            && [0, 3].iter().all(|&k| (cg[k] - fg[k]).abs() <= 1e-3 * res)
            && [1, 5].iter().all(|&k| (cg[k] - fg[k]).abs() <= 1e-9 * res);
        if f == 0 && !same_grid {
            let (a, b) = (fine.projection(), ds.projection());
            if a.is_empty() || b.is_empty() {
                return Err(
                    "ValueError: the coarse series is not on the grid of this one and one of them has no CRS".into()
                );
            }
            if !same_crs(&a, &b)? {
                return Err("ValueError: the coarse series and this one are in different CRS: reproject one first \
                            (Tools > Data preparation > Reproject, resample, clip, to a layer's grid); ImproPhe needs the same CRS, as FORCE's datacube has"
                    .into());
            }
        }
        // the coarse pixel of each fine column and row (the pixel centre's), None outside
        let cols: Vec<Option<usize>> = (0..w)
            .map(|c| {
                let x = fg[0] + (x0 + c) as f64 * fg[1] + 0.5 * fg[1];
                let s = ((x - cg[0]) / cg[1]).floor();
                (s >= 0.0 && s < cw as f64).then_some(s as usize)
            })
            .collect();
        let rows: Vec<Option<usize>> = (0..h)
            .map(|r| {
                let y = fg[3] + (y0 + r) as f64 * fg[5] + 0.5 * fg[5];
                let s = ((y - cg[3]) / cg[5]).floor();
                (s >= 0.0 && s < ch as f64).then_some(s as usize)
            })
            .collect();
        let range = |v: &[Option<usize>]| {
            v.iter()
                .flatten()
                .fold(None, |m: Option<(usize, usize)>, &s| Some(m.map_or((s, s), |(a, b)| (a.min(s), b.max(s)))))
        };
        let (Some((c0, c1)), Some((r0, r1))) = (range(&cols), range(&rows)) else { continue };
        let (sw, sh) = (c1 - c0 + 1, r1 - r0 + 1);
        let src = read(&ds, 1..=t, c0, r0, sw, sh, false)?;
        for d in 0..t {
            let dst = &mut out[(d * nf + f) * w * h..(d * nf + f + 1) * w * h];
            let plane = &src[d * sw * sh..(d + 1) * sw * sh];
            for (r, sr) in rows.iter().enumerate() {
                let Some(sr) = sr else { continue };
                for (c, sc) in cols.iter().enumerate() {
                    if let Some(sc) = sc {
                        dst[r * w + c] = plane[(sr - r0) * sw + (sc - c0)] as f32;
                    }
                }
            }
        }
    }
    Ok(out)
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let other = spec.other.as_ref().ok_or(NO_COARSE)?;
    if other.inputs.is_empty() || other.years.is_empty() {
        return Err(NO_COARSE.into());
    }
    let days = spec
        .days
        .as_ref()
        .filter(|d| d.len() == spec.years.len())
        .ok_or("ValueError: ImproPhe needs a series with real dates (the seasonal windows are days of the year)")?;
    let set = settings(p)?;
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = w * h;
    let (t, nb) = (spec.years.len(), spec.bands.len());
    if nb == 0 {
        return Err("ValueError: ImproPhe reads every band of each date: this series has none".into());
    }
    // the radii in fine pixels
    let g =
        inputs.input.ds().geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    if g[2] != 0.0 || g[4] != 0.0 {
        return Err("ValueError: the series' grid is rotated: ImproPhe's radii are in pixels of a north-up grid".into());
    }
    let res = g[1].abs();
    if (g[1].abs() - g[5].abs()).abs() > 1e-6 * res {
        return Err(format!(
            "ValueError: the pixels are not square ({} x {}): ImproPhe's radii are in pixels",
            g[1].abs(),
            g[5].abs()
        ));
    }
    let kh = radius(p.f64("kernel_size"), res, "kernel_size", 1)?;
    let ksd = radius(p.f64("texture_size"), res, "texture_size", 0)?;
    let year_mode = by_year(other, p.str("by"))?;
    let (nl, nf) = (other.years.len(), other.inputs.len());
    let nc = nl * nf;
    let nwin = set.dwin.len() - 1;
    let bytes =
        n as f64 * (4.0 * (t * nb) as f64 + 8.0 * nc as f64 + 12.0 * (nwin * nb) as f64 + 4.0 * nf as f64 + 16.0);
    if bytes > MAX_BYTES {
        return Err(format!(
            "ValueError: the window ({w} x {h} pixels, {t} dates of {nb} bands, {nc} coarse bands) needs about {:.1} GB of memory, \
             over the {:.0} GB limit: choose the ROI or the visible area",
            bytes / (1u64 << 30) as f64,
            MAX_BYTES / (1u64 << 30) as f64
        ));
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let outs_wanted = active_outputs(tool, p);
    let mut outs = Outputs::new(tool, &outs_wanted, &dir, inputs.input.ds(), window, spec)?;

    // the fine series, [date][band][pixel] float32, NaN where missing (its NoData, a date the quality band refuses)
    let mut x4 = vec![0f32; t * nb * n];
    let strip = rows_per_chunk(tool, w);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = strip.min(h - r);
        let chunk = inputs.read(x0, y0 + r, w, hh)?;
        let np = chunk.pixels();
        let rule = spec.qa_rule.as_deref();
        for (b, plane) in chunk.bands.values().enumerate() {
            for d in 0..t {
                let dst = &mut x4[(d * nb + b) * n + r * w..(d * nb + b) * n + r * w + np];
                for (k, v) in dst.iter_mut().enumerate() {
                    let x = plane[d * np + k];
                    let bad = chunk.qa.as_ref().is_some_and(|q| !qa_usable(rule, q[d * np + k]));
                    *v = if bad { f32::NAN } else { x as f32 };
                }
            }
        }
        r += hh;
        progress(0.1 * r as f64 / h as f64, &format!("reading the fine series, rows {r}/{h}"));
    }
    progress(0.1, "putting the coarse series on the fine grid");
    let mr = coarse_on_window(other, inputs.input.ds(), window, cancel)?;
    drop(inputs);
    progress(0.15, &format!("{nl} layer(s) of {nf} band(s), radius {kh} px, texture radius {ksd} px"));

    // the components of the fine data (of the layer's year, or of the whole series) and the prediction of each layer
    let doy: Vec<i32> = days
        .iter()
        .map(|&d| {
            let (_, m, dd) = civil(d);
            CUM[m as usize - 1] + dd as i32
        })
        .collect();
    let fine_years: Vec<i32> = days.iter().map(|&d| civil(d).0 as i32).collect();
    let cp = CoreParams {
        dwin: set.dwin.clone(),
        h: kh,
        ksd,
        min_var: set.min_var,
        force: set.force,
        force_pca: set.force_pca,
    };
    let n_jobs = spec.n_jobs();
    let mut out = vec![0f32; nc * n];
    let years = layer_years(other);
    let mut shared = None;
    let mut components = vec![];
    for (i, &layer_year) in years.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let msg = |what: &str| format!("layer {}/{nl}: {what}", i + 1);
        let (a, b) = (i * nf * n, (i + 1) * nf * n);
        if year_mode {
            progress(0.15 + 0.8 * i as f64 / nl as f64, &msg("the components of the year's scenes"));
            let year = i32::try_from(layer_year).map_err(|_| "ValueError: a year out of range".to_string())?;
            match core::prepare_fine(&x4, t, nb, h, w, &doy, &fine_years, Some(year), &cp, n_jobs) {
                None => {
                    out[a..b].copy_from_slice(&mr[a..b]);
                    components.push(0);
                }
                Some(prep) => {
                    components.push(prep.npc);
                    progress(0.15 + 0.8 * (i as f64 + 0.5) / nl as f64, &msg("the prediction"));
                    core::predict(&prep, &mr[a..b], nf, h, w, &cp, &mut out[a..b], n_jobs);
                }
            }
        } else {
            if shared.is_none() {
                progress(0.15, "the components of all the fine scenes");
                shared = Some(core::prepare_fine(&x4, t, nb, h, w, &doy, &fine_years, None, &cp, n_jobs));
                components.push(shared.as_ref().and_then(|s| s.as_ref()).map_or(0, |s| s.npc));
            }
            progress(0.15 + 0.8 * i as f64 / nl as f64, &msg("the prediction"));
            match shared.as_ref().and_then(|s| s.as_ref()) {
                None => out[a..b].copy_from_slice(&mr[a..b]),
                Some(prep) => core::predict(prep, &mr[a..b], nf, h, w, &cp, &mut out[a..b], n_jobs),
            }
        }
    }
    drop(x4);
    drop(mr);
    progress(
        0.95,
        &format!("writing ({} components)", components.iter().map(usize::to_string).collect::<Vec<_>>().join(", ")),
    );

    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = strip.min(h - r);
        let mut res = ChunkOut::new();
        let mut a = Vec::with_capacity(nc * hh * w);
        for b in 0..nc {
            a.extend(out[b * n + r * w..b * n + (r + hh) * w].iter().map(|&v| f64::from(v)));
        }
        res.insert("improphe".into(), a);
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.95 + 0.05 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }
    let outputs = outs.describe(p, None);
    drop(outs);
    Ok(JobResult { outputs, window })
}

/// The run's time: the fine series read and averaged, then for each layer the neighbourhood step (a pixel's circle
/// of radius h) and the textures, per pixel (the constants below, measured by a test of tests/improphe.rs).
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let ds = Dataset::open(&spec.input).map_err(|e| format!("{}: {e}", spec.input))?;
    let g = ds.geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    let (iw, ih) = ds.raster_size();
    let [x0, y0, x1, y1] = spec.window.unwrap_or([0, 0, iw, ih]);
    let (w, h) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let res = g[1].abs().max(f64::MIN_POSITIVE);
    let kh = radius(p.f64("kernel_size"), res, "kernel_size", 1)? as f64;
    let ksd = radius(p.f64("texture_size"), res, "texture_size", 0)? as f64;
    let (t, nb) = (spec.years.len().max(1) as f64, spec.bands.len().max(1) as f64);
    let (nl, nf) =
        spec.other.as_ref().map_or((1.0, 1.0), |o| (o.years.len().max(1) as f64, o.inputs.len().max(1) as f64));
    let nwin = (p.str("seasons").split(',').count().max(2) - 1) as f64;
    // a circle cut by the window's edge has fewer pixels
    let reach = kh.min(w.min(h) / 2.0).max(1.0);
    let disc = std::f64::consts::PI * reach * reach;
    let year_mode = by_year_hint(spec, p);
    let per_prepare = if year_mode { nl } else { 1.0 };
    let sec_per_px = K_DISC * disc * nl
        + K_BAND * disc * nl * (nf - 1.0)
        + K_TEXTURE * (2.0 * ksd + 1.0).powi(2) * nl * nf
        + K_FINE * t * nb
        + per_prepare * K_PREPARE * (nwin * nb).powi(2);
    Ok(Estimate { sec_per_px, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 })
}

/// Whether the run is likely a field with a layer a year (the estimate does not refuse what a run would).
fn by_year_hint(spec: &JobSpec, p: &Params) -> bool {
    spec.other.as_ref().is_some_and(|o| by_year(o, p.str("by")).unwrap_or(false))
}

// The constants of `estimate`, seconds, measured by `measure_the_constants` (tests/improphe.rs) on 300 x 300 pixels
// with four threads, radii of 10, 20 and 40 pixels, one and four scenes of one and two bands:
// each neighbourhood step costs about 0.75 ns a pixel of the circle (the circle cut by the window's edge counted),
// the texture of a coarse band 1.5 ns a pixel of its window, reading the fine series and its seasonal means 18 ns a
// date and band, the components 3 ns the squared number of seasonal bands. The estimate was within 25% of every
// measured run.
const K_DISC: f64 = 7.5e-10;
const K_BAND: f64 = 1.0e-10;
const K_TEXTURE: f64 = 1.5e-9;
const K_FINE: f64 = 1.8e-8;
const K_PREPARE: f64 = 3.0e-9;

#[cfg(test)]
mod tests {
    use super::*;

    fn other(days: Option<Vec<i64>>, years: Vec<f64>) -> OtherSeries {
        OtherSeries { name: "c".into(), inputs: vec!["a".into()], years, days, ..Default::default() }
    }

    #[test]
    fn seasons_are_checked_as_zeits() {
        assert_eq!(seasons("1,91,181,271,365").unwrap(), vec![1, 91, 181, 271, 365]);
        assert_eq!(seasons(" 330 60 330").unwrap(), vec![330, 60, 330]);
        assert!(seasons("1").is_err());
        assert!(seasons("1,400").is_err());
        assert!(seasons("1,a").is_err());
    }

    #[test]
    fn radii_are_truncated_pixels() {
        assert_eq!(radius(2500.0, 10.0, "kernel_size", 1).unwrap(), 250);
        assert_eq!(radius(335.0, 10.0, "texture_size", 0).unwrap(), 33);
        assert_eq!(radius(0.0, 10.0, "texture_size", 0).unwrap(), 0);
        assert!(radius(5.0, 10.0, "kernel_size", 1).is_err());
    }

    #[test]
    fn the_layout_follows_zeit() {
        // 2020-01-01, 2020-07-01, 2021-01-01
        let scenes = other(Some(vec![18262, 18444, 18628]), vec![2020.0, 2020.5, 2021.0]);
        assert!(!by_year(&scenes, "auto").unwrap());
        assert!(by_year(&scenes, "year").is_err());
        let yearly = other(Some(vec![18444, 18809]), vec![2020.5, 2021.5]);
        assert!(by_year(&yearly, "auto").unwrap_err().contains("one date a year"));
        assert!(by_year(&yearly, "year").unwrap());
        assert!(!by_year(&yearly, "scenes").unwrap());
        // no dates: whole years in 1900..2300 are a field with a layer a year
        assert!(by_year(&other(None, vec![2019.0, 2020.0]), "auto").unwrap());
        assert!(by_year(&other(None, vec![0.0, 1.0]), "auto").is_err());
    }
}
