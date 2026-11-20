//! Change between two dates over the whole window (Tools > Change detection and Data preparation), on zeit-core:
//! the change vector analysis (`cva`), the iteratively reweighted MAD (`imad`) and the relative radiometric
//! normalization from its invariant pixels (`radcal`), as Zeit's `cva`, `imad` and `radcal`. Each fits what it needs
//! (a threshold, the canonical analysis, a gain and an offset a band) on the lattice sample of the whole window, as
//! Zeit does (every s-th row and column from the window's first), and then applies it to every pixel, strip by strip.
//! The tables go to the run's folder.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use zeit_core::cva::{self, CvaOptions, Direction};
use zeit_core::imad::{self, ImadFit, ImadOptions, Penalty};

use crate::job::{ChunkOut, Estimate, Inputs, JobResult, JobSpec, Outputs};
use crate::manifest::{Output, Tool};
use crate::pixel::Params;
use crate::planes::{self, BandSet, Tifs};

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    match spec.tool.as_str() {
        "cva" => run_cva(spec, tool, p, progress, cancel),
        "imad" => run_imad(spec, tool, p, progress, cancel),
        _ => run_radcal(spec, tool, p, progress, cancel),
    }
}

/// The run's time: reading both dates twice (the sample, then every pixel) and the arithmetic of each pixel.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let nb = spec.bands.keys().filter(|k| k.starts_with('b')).count().max(2) as f64;
    let (per_band, dates) = match spec.tool.as_str() {
        "cva" => (2e-8, 2.0),
        "imad" => (6e-8 + 4e-9 * nb, 2.0),
        _ => (3e-8, spec.years.len().max(2) as f64),
    };
    let _ = p;
    Ok(Estimate { sec_per_px: dates * nb * per_band, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 })
}

/// The two dates of a run: `before` (0: the first date) and `after` (0: the last).
fn two_dates(spec: &JobSpec, p: &Params) -> Result<(usize, usize), String> {
    let t = spec.years.len();
    let a = planes::date_param(spec, p.f64("before"), 0);
    let b = planes::date_param(spec, p.f64("after"), t.saturating_sub(1));
    if a == b {
        return Err("ValueError: the two dates are the same one: choose two different dates".into());
    }
    Ok((a, b))
}

/// The lattice samples of both dates: [date][band][sample pixel], and the stride.
#[allow(clippy::too_many_arguments)]
fn samples(
    inputs: &mut Inputs,
    bands: &BandSet,
    dates: &[usize],
    window: [usize; 4],
    sample: i64,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<(planes::Lattice, usize), String> {
    let (w, h) = (window[2] - window[0], window[3] - window[1]);
    let stride = planes::lattice_stride(w, h, sample);
    let (s, _) = planes::lattice(inputs, &bands.roles, dates, window, stride, cancel, progress)?;
    Ok((s, stride))
}

fn json_f64(v: f64) -> Value {
    if v.is_finite() { json!(v) } else { Value::Null }
}

// ---------------------------------------------------------------------------------------------------------------
// CVA
// ---------------------------------------------------------------------------------------------------------------

const SIGN_LABELS: [&str; 8] = [
    "Darker in all bands (water, flooding, wetting, shadow)",
    "Red up, NIR and SWIR down (turbid water, other)",
    "Vegetation gain (greening, regrowth)",
    "Visible and NIR up, SWIR down (other)",
    "NIR down, SWIR up (burn scar, drying vegetation)",
    "Vegetation loss (clearing to soil)",
    "NIR and SWIR up, red down (other)",
    "Brighter in all bands (bare soil, built-up, bright surfaces)",
];

const POLAR_LABELS_4: [&str; 4] = [
    "Brighter and greener (both bands up)",
    "Darker and greener",
    "Darker and less green (both bands down)",
    "Brighter and less green (vegetation loss to soil)",
];

/// The labels of the sectors 1.. of a direction.
pub fn sector_labels(direction: &Direction) -> Vec<String> {
    match direction {
        Direction::Sign { .. } => SIGN_LABELS.iter().map(|s| (*s).to_string()).collect(),
        Direction::Polar { n_sectors: 4, .. } => POLAR_LABELS_4.iter().map(|s| (*s).to_string()).collect(),
        Direction::Polar { n_sectors, .. } => {
            let step = 360.0 / *n_sectors as f64;
            (1..=*n_sectors)
                .map(|k| format!("angle {}-{} degrees", ((k - 1) as f64 * step) as i64, (k as f64 * step) as i64))
                .collect()
        }
        Direction::C2va => vec![],
    }
}

/// The position among the used bands of a band the text names (a name, or its number in the date's file).
fn position(spec: &JobSpec, bands: &BandSet, token: &str) -> Option<usize> {
    let token = token.trim();
    bands
        .names
        .iter()
        .position(|n| n == token)
        .or_else(|| token.parse::<usize>().ok().and_then(|n| bands.roles.iter().position(|r| *r == format!("b{n:03}"))))
        .or_else(|| spec.band_names.get(token).and_then(|n| bands.names.iter().position(|b| b == n)))
}

/// The direction of a run from the parameters and the bands: "auto" is the sign sectors when red, NIR and SWIR1
/// are mapped among the bands, else the angle to the diagonal.
pub(crate) fn direction_of(spec: &JobSpec, bands: &BandSet, p: &Params) -> Result<Direction, String> {
    let role = |r: &str| spec.band_names.get(r).and_then(|n| bands.names.iter().position(|b| b == n));
    let signs = || Some(Direction::Sign { red: role("red")?, nir: role("nir")?, swir1: role("swir1")? });
    let nb = bands.names.len();
    match p.str("direction") {
        "sign" => signs().ok_or_else(|| {
            "ValueError: direction 'sign' needs the bands red, NIR and SWIR1 mapped (the Bands of the series) and among \
             the bands used"
                .to_string()
        }),
        "polar" => {
            let n_sectors = p.int("n_sectors").max(1) as usize;
            let (b1, b2) = match p.str("polar_bands").trim() {
                "" if nb >= 2 => (0, 1),
                "" => return Err("ValueError: direction 'polar' needs two bands".into()),
                text => {
                    let picks: Vec<usize> = text
                        .split(',')
                        .map(|t| position(spec, bands, t).ok_or_else(|| format!("ValueError: polar_bands: '{}' is not one of the bands used", t.trim())))
                        .collect::<Result<_, _>>()?;
                    if picks.len() != 2 || picks[0] == picks[1] {
                        return Err("ValueError: polar_bands must be two different bands".into());
                    }
                    (picks[0], picks[1])
                }
            };
            Ok(Direction::Polar { b1, b2, n_sectors })
        }
        "c2va" if nb >= 2 => Ok(Direction::C2va),
        "c2va" => Err("ValueError: direction 'c2va' needs at least two bands".into()),
        _ => match signs() {
            Some(d) => Ok(d),
            None if nb >= 2 => Ok(Direction::C2va),
            None => Err("ValueError: CVA needs at least two bands for a direction".into()),
        },
    }
}

/// What the threshold is and how it was found.
pub struct Threshold {
    pub value: f64,
    pub info: Value,
    pub scales: Vec<f64>,
    pub n_sample: usize,
}

/// The scale of each band and the threshold, from the lattice samples of the two dates (`x`, `y`: [band][pixel]).
pub fn cva_threshold(x: &[Vec<f64>], y: &[Vec<f64>], p: &Params, stride: usize) -> Result<Threshold, String> {
    let nb = x.len();
    let count = x.first().map_or(0, Vec::len);
    let valid: Vec<usize> = (0..count).filter(|&i| x.iter().chain(y).all(|b| b[i].is_finite())).collect();
    let m = valid.len();
    let pick = |a: &[Vec<f64>]| -> Vec<f64> { a.iter().flat_map(|b| valid.iter().map(|&i| b[i])).collect() };
    let (x1, x2) = (pick(x), pick(y));
    let use_bands: Vec<usize> = (0..nb).collect();
    let (rho, scales) = cva::cva_sample(&x1, &x2, nb, m.max(1), 1, &use_bands, p.str("scale") == "std", 1, &[]);
    if rho.len() < 20 {
        return Err(format!("ValueError: CVA: only {} valid pixels in the sample", rho.len()));
    }
    let (value, mut info) = match p.str("threshold") {
        "em" => {
            let (t, fit) =
                cva::em_threshold(&rho, p.f64("em_init"), 500, 1e-9).map_err(|e| format!("ValueError: {e}"))?;
            (
                t,
                json!({ "method": "em", "p": fit.p, "mu": fit.mu, "sigma": fit.sigma,
                        "iterations": fit.iterations, "converged": fit.converged }),
            )
        }
        "fixed" => (p.f64("value"), json!({ "method": "fixed" })),
        _ => {
            let up = cva::percentile(&rho, 0.999);
            let (t, sb) = cva::otsu(&rho, 1024, up);
            (t, json!({ "method": "otsu", "between_class_variance": sb, "bin_width": up / 1024.0 }))
        }
    };
    info["threshold"] = json!(value);
    info["n_sample"] = json!(rho.len());
    info["stride"] = json!(stride);
    Ok(Threshold { value, info, scales, n_sample: rho.len() })
}

/// The area of a pixel in hectares, where the grid is in metres.
fn pixel_hectares(src: &gdal::Dataset) -> Option<f64> {
    let g = src.geo_transform().ok()?;
    let srs = gdal::spatial_ref::SpatialRef::from_wkt(&src.projection()).ok()?;
    srs.is_projected().then(|| (g[1] * g[5]).abs() / 1e4)
}

fn run_cva(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let bands = planes::band_set(spec, p.str("bands"), 1, "CVA")?;
    let nb = bands.roles.len();
    let (ia, ib) = two_dates(spec, p)?;
    let direction = direction_of(spec, &bands, p)?;
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let n_jobs = spec.n_jobs();

    // ---- the threshold, on the lattice sample
    progress(0.0, "the sample of the magnitudes");
    let (s, stride) =
        samples(&mut inputs, &bands, &[ia, ib], window, p.int("sample"), cancel, &|f| progress(0.4 * f, "the sample"))?;
    let th = cva_threshold(&s[0], &s[1], p, stride)?;
    drop(s);
    if p.str("scale") == "std" && th.scales.contains(&0.0) {
        progress(0.4, "a band's difference is constant: its weight is 0");
    }
    progress(0.4, &format!("threshold {:.4} from {} pixels", th.value, th.n_sample));

    // ---- every pixel
    let sectors = sector_labels(&direction);
    let has_sector = !sectors.is_empty();
    let is_sign = matches!(direction, Direction::Sign { .. });
    let wanted: Vec<Output> = tool
        .outputs
        .iter()
        .filter(|o| match o.id.as_str() {
            "direction" => !is_sign,
            "sector" => has_sector,
            _ => true,
        })
        .cloned()
        .collect();
    let mut outs = Outputs::new(tool, &wanted, &dir, inputs.input.ds(), window, spec)?;
    let opts = CvaOptions {
        bands: &(0..nb).collect::<Vec<_>>(),
        scale: &th.scales,
        direction: direction.clone(),
        threshold: th.value,
        c2va_edges: None,
    };
    let n_classes = if has_sector { sectors.len() + 1 } else { 2 };
    let (mut count, mut sum_mag) = (vec![0u64; n_classes], vec![0f64; n_classes]);
    let rows = planes::strip_rows(2, nb, w, h);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let np = w * hh;
        let strip = inputs.read_dates(&bands.roles, &[ia, ib], x0, y0 + r, w, hh)?;
        let mut m = [vec![0f32; np], vec![0f32; np], vec![0f32; np], vec![0f32; np]];
        let [mag, dr, chg, sec] = &mut m;
        cva::cva_batch(&strip[0], &strip[1], nb, np, &opts, &[], mag, dr, chg, sec, n_jobs);
        for i in 0..np {
            let class = if has_sector { sec[i] } else { chg[i] };
            if class.is_finite() && (class as usize) < n_classes {
                count[class as usize] += 1;
                sum_mag[class as usize] += f64::from(mag[i]);
            }
        }
        let f = |a: &[f32], add: f64| -> Vec<f64> { a.iter().map(|&v| f64::from(v) + add).collect() };
        let mut res = ChunkOut::new();
        res.insert("magnitude".into(), f(mag, 0.0));
        res.insert("direction".into(), f(dr, 0.0));
        res.insert("change".into(), f(chg, 1.0));
        res.insert("sector".into(), f(sec, 1.0));
        res.retain(|k, _| wanted.iter().any(|o| &o.id == k));
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.4 + 0.6 * r as f64 / h as f64, &format!("rows {r}/{h}"));
    }

    // ---- the sectors' table and the threshold's record
    let labels: Vec<String> = if has_sector {
        std::iter::once("No change".to_string()).chain(sectors.iter().cloned()).collect()
    } else {
        vec!["No change".into(), "Change".into()]
    };
    let hectares = pixel_hectares(inputs.input.ds());
    let rows_csv: Vec<Vec<String>> = (0..n_classes)
        .map(|k| {
            vec![
                k.to_string(),
                labels[k].clone(),
                count[k].to_string(),
                hectares.map_or(String::new(), |a| planes::csv_num(count[k] as f64 * a)),
                if count[k] > 0 { planes::csv_num(sum_mag[k] / count[k] as f64) } else { String::new() },
            ]
        })
        .collect();
    planes::write_csv(
        &dir.join("cva_sectors.csv"),
        &["sector", "label", "pixels", "area_ha", "mean_magnitude"],
        &rows_csv,
    )?;
    let record = json!({
        "threshold": th.info, "bands": bands.names, "scale": th.scales,
        "scale_mode": p.str("scale"),
        "dates": [crate::job::date_labels(spec)[ia], crate::job::date_labels(spec)[ib]],
        "sectors": (0..n_classes).map(|k| json!({
            "sector": k, "label": labels[k], "pixels": count[k],
            "area_ha": hectares.map(|a| count[k] as f64 * a),
            "mean_magnitude": if count[k] > 0 { json!(sum_mag[k] / count[k] as f64) } else { Value::Null },
        })).collect::<Vec<_>>(),
    });
    std::fs::write(dir.join("cva.json"), serde_json::to_string(&record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut extra = serde_json::Map::new();
    if has_sector {
        extra.insert("sector".into(), json!({ "classes": labels }));
    }
    let model = json!({ "outputs": Value::Object(extra) });
    let outputs = outs.describe(p, Some(&model));
    drop(outs);
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}

// ---------------------------------------------------------------------------------------------------------------
// iMAD
// ---------------------------------------------------------------------------------------------------------------

/// The options of the iterations of a run.
pub fn imad_options(p: &Params) -> ImadOptions {
    ImadOptions {
        max_iter: p.int("max_iter").max(1) as usize,
        tol: p.f64("tol"),
        lam: if p.0.contains_key("lam") { p.f64("lam") } else { 0.0 },
        penalty: if p.str("penalty") == "curvature" { Penalty::Curvature } else { Penalty::Ridge },
        weighted_sd: p.str("standardize") != "all",
    }
}

/// The iterations on the valid pixels of the two samples (`x`, `y`: [band][pixel]).
pub fn imad_fit_samples(
    x: &[Vec<f64>],
    y: &[Vec<f64>],
    opts: &ImadOptions,
    n_jobs: i32,
) -> Result<(ImadFit, Vec<f64>), String> {
    let n = x.len();
    let z = planes::valid_rows(x, y);
    let rows = z.len() / (2 * n).max(1);
    if rows < 2 * (2 * n) + 2 {
        return Err(format!("ValueError: iMAD: only {rows} valid pixels in the sample for {n} bands"));
    }
    let fit = imad::imad_fit(&z, n, opts, n_jobs).map_err(|e| format!("ValueError: iMAD: {e}"))?;
    Ok((fit, z))
}

/// A fit's record, as Zeit's `attrs["imad"]`.
pub fn imad_summary(fit: &ImadFit, opts: &ImadOptions, p: &Params, bands: &[String], stride: usize) -> Value {
    let (lx, ly) = imad::loadings(fit);
    let t = &fit.transform;
    json!({
        "bands": bands,
        "rho": fit.rho_history,
        "max_delta": fit.max_delta.iter().map(|&v| json_f64(v)).collect::<Vec<_>>(),
        "rho_final": fit.cca.rho,
        "iterations": fit.iterations, "converged": fit.converged,
        "tol": opts.tol, "lam": opts.lam,
        "penalty": if opts.penalty == Penalty::Curvature { "curvature" } else { "ridge" },
        "standardize": if opts.weighted_sd { "weighted" } else { "all" },
        "alpha": if p.0.contains_key("alpha") { p.f64("alpha") } else { 0.01 },
        "n_stats_pixels": fit.n_pixels, "stride": stride,
        "mean": t.mean, "a": t.a, "b": t.b, "sd": t.sd,
        "loadings_x": lx, "loadings_y": ly,
        "mad_order": "MAD1 is the pair of the smallest canonical correlation (the largest variance)",
        "n_bands": bands.len(),
        "warnings": fit.warnings,
    })
}

/// The correlations of every iteration as a table: the iteration, `rho_1..rho_N`, the largest change.
fn rho_table(fit: &ImadFit) -> (Vec<String>, Vec<Vec<String>>) {
    let n = fit.transform.n;
    let mut header = vec!["iteration".to_string()];
    header.extend((1..=n).map(|i| format!("rho_{i}")));
    header.push("max_delta".into());
    let rows = fit
        .rho_history
        .iter()
        .enumerate()
        .map(|(k, rho)| {
            let mut r = vec![(k + 1).to_string()];
            r.extend(rho.iter().map(|&v| planes::csv_num(v)));
            r.push(planes::csv_num(fit.max_delta[k]));
            r
        })
        .collect();
    (header, rows)
}

fn run_imad(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let bands = planes::band_set(spec, p.str("bands"), 1, "iMAD")?;
    let n = bands.roles.len();
    let (ia, ib) = two_dates(spec, p)?;
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let n_jobs = spec.n_jobs();
    let alpha = p.f64("alpha");
    let opts = imad_options(p);

    progress(0.0, "the sample");
    let (s, stride) =
        samples(&mut inputs, &bands, &[ia, ib], window, p.int("sample"), cancel, &|f| progress(0.2 * f, "the sample"))?;
    let (fit, z) = imad_fit_samples(&s[0], &s[1], &opts, n_jobs)?;
    drop((s, z));
    progress(
        0.4,
        &format!("{} iterations, {}", fit.iterations, if fit.converged { "converged" } else { "not converged" }),
    );

    // ---- every pixel
    let k_show = (p.int("n_mad").max(0) as usize).min(n);
    let mut wanted: Vec<Output> = vec![];
    for o in &tool.outputs {
        if o.id == "mad" {
            for k in 1..=k_show {
                let mut q = o.clone();
                q.id = format!("mad_{k}");
                q.name = q.name.replace("{}", &k.to_string());
                q.each = None;
                wanted.push(q);
            }
        } else {
            wanted.push(o.clone());
        }
    }
    let mut outs = Outputs::new(tool, &wanted, &dir, inputs.input.ds(), window, spec)?;
    let mut names: Vec<String> = (1..=n).map(|i| format!("MAD{i}")).collect();
    names.extend(["chi2".into(), "p_nochange".into(), "change".into()]);
    let mut all = Tifs::create(vec![dir.join("imad.tif")], &names, inputs.input.ds(), window, spec)?;
    let rows = planes::strip_rows(2, n, w, h);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let np = w * hh;
        let strip = inputs.read_dates(&bands.roles, &[ia, ib], x0, y0 + r, w, hh)?;
        let (mut mad, mut chi2, mut pnc) = (vec![0f32; n * np], vec![0f32; np], vec![0f32; np]);
        imad::imad_apply(&strip[0], &strip[1], n, np, &fit.transform, &[], &mut mad, &mut chi2, &mut pnc, None, n_jobs);
        let change: Vec<f32> = pnc
            .iter()
            .map(|&v| if v.is_nan() { f32::NAN } else { f32::from(u8::from(f64::from(v) < alpha)) })
            .collect();
        let mut planes_out = mad.clone();
        planes_out.extend_from_slice(&chi2);
        planes_out.extend_from_slice(&pnc);
        planes_out.extend_from_slice(&change);
        all.write(0, r, hh, &planes_out)?;
        let f = |a: &[f32], add: f64| -> Vec<f64> { a.iter().map(|&v| f64::from(v) + add).collect() };
        let mut res = ChunkOut::new();
        for k in 1..=k_show {
            res.insert(format!("mad_{k}"), f(&mad[(k - 1) * np..k * np], 0.0));
        }
        res.insert("chi2".into(), f(&chi2, 0.0));
        res.insert("p_nochange".into(), f(&pnc, 0.0));
        res.insert("change".into(), f(&change, 1.0));
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.4 + 0.6 * r as f64 / h as f64, &format!("rows {r}/{h}"));
    }
    drop(all);

    let (header, rows_csv) = rho_table(&fit);
    let header: Vec<&str> = header.iter().map(String::as_str).collect();
    planes::write_csv(&dir.join("imad_rho.csv"), &header, &rows_csv)?;
    let summary = imad_summary(&fit, &opts, p, &bands.names, stride);
    std::fs::write(dir.join("imad.json"), serde_json::to_string(&summary).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let outputs = outs.describe(p, None);
    drop(outs);
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}

// ---------------------------------------------------------------------------------------------------------------
// Radiometric normalization
// ---------------------------------------------------------------------------------------------------------------

/// The 1/`holdout` of the invariant pixels kept for the tests: every `every`-th (0: none).
pub fn holdout_every(holdout: f64) -> Result<usize, String> {
    if !(0.0..1.0).contains(&holdout) {
        return Err(format!("ValueError: holdout must be in [0, 1), got {holdout}"));
    }
    Ok(if holdout == 0.0 { 0 } else { ((1.0 / holdout).round() as usize).max(2) })
}

fn run_radcal(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let bands = planes::band_set(spec, p.str("bands"), 1, "the normalization")?;
    let n = bands.roles.len();
    let t = spec.years.len();
    let reference = planes::date_param(spec, p.f64("reference"), 0);
    let targets: Vec<usize> = if p.f64("target") > 0.0 {
        let k = planes::date_param(spec, p.f64("target"), 0);
        if k == reference {
            return Err("ValueError: the date to normalize is the reference".into());
        }
        vec![k]
    } else {
        (0..t).filter(|&k| k != reference).collect()
    };
    if targets.is_empty() {
        return Err("ValueError: there is no date to normalize but the reference".into());
    }
    let threshold = p.f64("threshold");
    if !(0.5..1.0).contains(&threshold) {
        return Err(format!("ValueError: threshold must be in [0.5, 1), got {threshold}"));
    }
    let every = holdout_every(p.f64("holdout"))?;
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let dir = PathBuf::from(&spec.output_dir);
    let out_dir = dir.join("normalized");
    std::fs::create_dir_all(&out_dir).map_err(|e| format!("{}: {e}", out_dir.display()))?;
    let n_jobs = spec.n_jobs();
    let opts = imad_options(p);
    let labels = crate::job::date_labels(spec);
    let nt = targets.len();

    // ---- the fit of each date to the reference, on the lattice sample
    let stride = planes::lattice_stride(w, h, p.int("sample"));
    let (refs, _) = planes::lattice(&mut inputs, &bands.roles, &[reference], window, stride, cancel, &|_| {})?;
    let mut fits: Vec<(usize, Vec<imad::BandFit>, usize)> = vec![];
    let mut summaries = vec![];
    for (i, &k) in targets.iter().enumerate() {
        progress(0.5 * i as f64 / nt as f64, &format!("the invariant pixels of {} ({}/{nt})", labels[k], i + 1));
        let (tgt, _) = planes::lattice(&mut inputs, &bands.roles, &[k], window, stride, cancel, &|_| {})?;
        let (fit, z) = imad_fit_samples(&refs[0], &tgt[0], &opts, n_jobs)?;
        let (rows, invariant) = imad::radcal_fit(&z, n, &fit.transform, threshold, every, n_jobs)
            .map_err(|e| format!("ValueError: radcal: {} : {e}", labels[k]))?;
        let mut summary = imad_summary(&fit, &opts, p, &bands.names, stride);
        summary["date"] = json!(labels[k]);
        summary["n_invariant"] = json!(invariant);
        summaries.push(summary);
        fits.push((k, rows, invariant));
    }
    drop(refs);

    // ---- the whole series, a file a date: the dates fitted normalized, the others copied
    let file_names = planes::date_file_names(spec);
    let paths: Vec<PathBuf> = (0..t).map(|k| out_dir.join(format!("{}.tif", file_names[k]))).collect();
    let mut tifs = Tifs::create(paths, &bands.names, inputs.input.ds(), window, spec)?;
    let rows = planes::strip_rows(1, n, w, h);
    for (k, label) in labels.iter().enumerate().take(t) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let gain: Option<Vec<(f64, f64)>> =
            fits.iter().find(|(d, _, _)| *d == k).map(|(_, rows, _)| rows.iter().map(|b| (b.fit.a, b.fit.b)).collect());
        let mut r = 0;
        while r < h {
            let hh = rows.min(h - r);
            let np = w * hh;
            let plane = inputs.read_dates(&bands.roles, &[k], x0, y0 + r, w, hh)?.swap_remove(0);
            let out: Vec<f32> = match &gain {
                Some(g) => plane.iter().enumerate().map(|(i, &v)| (g[i / np].0 + g[i / np].1 * v) as f32).collect(),
                None => plane.iter().map(|&v| v as f32).collect(),
            };
            tifs.write(k, r, hh, &out)?;
            r += hh;
        }
        progress(0.5 + 0.5 * (k + 1) as f64 / t as f64, &format!("writing {label}"));
    }
    drop(tifs);

    // ---- the table of the fits and the record
    let mut rows_csv = vec![];
    for (k, rows, invariant) in &fits {
        for (j, b) in rows.iter().enumerate() {
            let tt = b.test;
            let g =
                |f: &dyn Fn(&imad::HoldoutTest) -> f64| tt.as_ref().map_or(String::new(), |x| planes::csv_num(f(x)));
            rows_csv.push(vec![
                labels[*k].clone(),
                bands.names[j].clone(),
                planes::csv_num(b.fit.a),
                planes::csv_num(b.fit.se_a),
                planes::csv_num(b.fit.b),
                planes::csv_num(b.fit.se_b),
                planes::csv_num(b.fit.r),
                planes::csv_num(b.fit.rmse),
                b.n_train.to_string(),
                b.n_test.to_string(),
                invariant.to_string(),
                g(&|x| x.mean_target),
                g(&|x| x.mean_ref),
                g(&|x| x.mean_norm),
                g(&|x| x.t),
                g(&|x| x.p_t),
                g(&|x| x.var_target),
                g(&|x| x.var_ref),
                g(&|x| x.var_norm),
                g(&|x| x.f),
                g(&|x| x.p_f),
            ]);
        }
    }
    planes::write_csv(
        &dir.join("radcal_fits.csv"),
        &[
            "date",
            "band",
            "a",
            "se_a",
            "b",
            "se_b",
            "r",
            "rmse",
            "n_train",
            "n_test",
            "n_invariant",
            "mean_target",
            "mean_ref",
            "mean_norm",
            "t",
            "p_t",
            "var_target",
            "var_ref",
            "var_norm",
            "F",
            "p_F",
        ],
        &rows_csv,
    )?;
    let record =
        json!({ "reference": labels[reference], "threshold": threshold, "holdout_every": every, "imad": summaries });
    std::fs::write(dir.join("radcal.json"), serde_json::to_string(&record).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let outputs = vec![json!({
        "id": "normalized", "name": format!("Normalized to {}", labels[reference]), "unit": "as the input",
        "series": true, "path": out_dir.to_string_lossy(),
    })];
    let _ = tool;
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}
