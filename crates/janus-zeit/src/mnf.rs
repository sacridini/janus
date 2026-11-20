//! The Minimum Noise Fraction transform (Tools > Spectral analysis > MNF), as Zeit's `mnf`: the bands of a date (or of
//! every date, one transform) turned into components ordered by signal-to-noise ratio, the noise being estimated from
//! the differences between neighbouring pixels. The statistics come from the whole window, read in the blocks of rows
//! zeit-core's own `mnf_stats_batch` cuts a cube in (with the row below each as its halo), and merged in the same
//! order, so the transform has the same bits as the one Zeit fits; the transform is then applied strip by strip.
//! Writes the components (a map each; a series each with the statistics of all dates), the multiband file of them, the
//! table of eigenvalues, the transform as Zeit's JSON and, on request, the denoised bands as a series of their own.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use zeit_core::mnf::{self as zm, MnfError, MnfStats, MnfTransform, Noise, Scale, StatsOptions};

use crate::job::{ChunkOut, Estimate, Inputs, JobResult, JobSpec, Outputs};
use crate::manifest::Tool;
use crate::pixel::Params;
use crate::planes::{self, Tifs};

fn noise_of(id: &str) -> Noise {
    match id {
        "diff_h" => Noise::DiffH,
        "diff_v" => Noise::DiffV,
        "diff_diag" => Noise::DiffDiag,
        "diff_antidiag" => Noise::DiffAntidiag,
        _ => Noise::DiffHv,
    }
}

/// The names Zeit gives the directions of an estimator, in the order of the counts of pairs.
fn directions(id: &str) -> &'static [&'static str] {
    match id {
        "diff_h" => &["h"],
        "diff_v" => &["v"],
        "diff_diag" => &["diag"],
        "diff_antidiag" => &["antidiag"],
        _ => &["h", "v"],
    }
}

fn explain(e: MnfError) -> String {
    format!("ValueError: {e}")
}

/// What a run decides, from the parameters.
#[derive(Clone, Debug)]
pub struct Settings {
    pub all_dates: bool,
    pub noise: String,
    pub n_components: usize,
    pub components: bool,
    pub denoised: bool,
    pub k: usize,
    pub snr_min: f64,
    pub scale: Scale,
    pub fraction: f64,
    pub seed: u64,
}

pub fn settings(p: &Params) -> Result<Settings, String> {
    let fraction = p.f64("sample_fraction");
    if !(fraction > 0.0 && fraction <= 1.0) {
        return Err(format!("ValueError: sample_fraction must be in (0, 1], got {fraction}"));
    }
    let snr_min = p.f64("snr_min");
    if snr_min < -1.0 {
        return Err("ValueError: snr_min must be at least -1 (the SNR of a component cannot be lower)".into());
    }
    let output = p.str("output");
    Ok(Settings {
        all_dates: p.str("scope") == "all_dates",
        noise: p.str("noise").to_string(),
        n_components: p.int("n_components").max(0) as usize,
        components: output != "denoised",
        denoised: output != "components",
        k: p.int("k").max(0) as usize,
        snr_min,
        scale: if p.str("scale") == "unit" { Scale::Unit } else { Scale::Noise },
        fraction,
        seed: p.int("seed").max(0) as u64,
    })
}

/// The date a script transforms: the parameter's, else the map's, else the last.
pub fn date_of_input(input: &crate::pixel::PixelInput, p: &Params) -> usize {
    let t = input.years.len();
    let by_map = input.map_year.filter(|_| t > 0).map(|y| planes::nearest_date(&input.years, y));
    planes::date_index(&input.years, p.f64("date"), by_map.unwrap_or(t.saturating_sub(1)))
}

/// The date a run transforms: the parameter's, else the map's, else the last.
pub fn date_of(spec: &JobSpec, p: &Params) -> usize {
    let t = spec.years.len();
    let by_map = spec.map_year.filter(|_| t > 0).map(|y| planes::nearest_date(&spec.years, y));
    planes::date_param(spec, p.f64("date"), by_map.unwrap_or(t.saturating_sub(1)))
}

/// The components kept by the denoising: `k`, else those with at least the SNR.
pub fn denoise_k(s: &Settings, tr: &MnfTransform) -> Result<usize, String> {
    if s.k > 0 {
        if s.k > tr.m {
            return Err(format!(
                "ValueError: k={}: the transform has {} components, k must be from 0 to {}",
                s.k, tr.m, tr.m
            ));
        }
        Ok(s.k)
    } else {
        Ok(tr.n_with_snr(s.snr_min))
    }
}

/// The statistics of the dates `dates` of the window, in zeit-core's blocks and order. `date0` is the first date's
/// place in the cube Zeit would cut it from (the key of the sample of pixels).
#[allow(clippy::too_many_arguments)]
fn gather(
    inputs: &mut Inputs,
    roles: &[String],
    dates: &[usize],
    date0: usize,
    window: [usize; 4],
    base: &StatsOptions,
    active: Option<&[u8]>,
    n_jobs: i32,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<MnfStats, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let nb = roles.len();
    let rpb = zm::rows_per_block(w, nb).clamp(1, h.max(1));
    let opts = StatsOptions {
        noise: base.noise.clone(),
        mask: base.mask,
        noise_region: base.noise_region,
        missing: base.missing,
        sample_fraction: base.sample_fraction,
        seed: base.seed,
        date0,
        active,
    };
    let blocks: Vec<(usize, usize, usize)> =
        (0..dates.len()).flat_map(|t| (0..h).step_by(rpb).map(move |r0| (t, r0, rpb.min(h - r0)))).collect();
    let template = MnfStats::new(nb, &opts.noise);
    let mut total = template.clone();
    let wave = zeit_core::parallel::threads(n_jobs).clamp(1, 8);
    let mut done = 0;
    for group in blocks.chunks(wave) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let mut data = vec![];
        for &(ti, r0, rows) in group {
            let halo = usize::from(r0 + rows < h);
            let n_all = (rows + halo) * w;
            let planes = inputs.read_dates(roles, &[dates[ti]], x0, y0 + r0, w, rows + halo)?.swap_remove(0);
            let mut x = Vec::with_capacity(nb * rows * w);
            let mut hl = Vec::with_capacity(nb * w);
            for b in 0..nb {
                let plane = &planes[b * n_all..(b + 1) * n_all];
                x.extend_from_slice(&plane[..rows * w]);
                if halo == 1 {
                    hl.extend_from_slice(&plane[rows * w..]);
                }
            }
            data.push((ti, r0, rows, x, (halo == 1).then_some(hl)));
        }
        let parts: Vec<MnfStats> = std::thread::scope(|s| {
            let running: Vec<_> = data
                .iter()
                .map(|(ti, r0, rows, x, hl)| {
                    let (opts, template) = (&opts, &template);
                    s.spawn(move || {
                        let mut part = template.empty_like();
                        part.add_block(x, hl.as_deref(), *ti, *r0, *rows, h, w, opts);
                        part
                    })
                })
                .collect();
            running.into_iter().filter_map(|r| r.join().ok()).collect()
        });
        if parts.len() != group.len() {
            return Err("the statistics of a block failed".into());
        }
        for part in &parts {
            total.merge(part);
        }
        done += group.len();
        progress(done as f64 / blocks.len() as f64);
    }
    Ok(total)
}

/// The fitted statistics of a run (the retry without the bands that have no value, as Zeit's `mnf_stats_batch`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn fit_stats(
    inputs: &mut Inputs,
    roles: &[String],
    dates: &[usize],
    date0: usize,
    window: [usize; 4],
    base: &StatsOptions,
    n_jobs: i32,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<MnfStats, String> {
    let first = gather(inputs, roles, dates, date0, window, base, base.active, n_jobs, cancel, progress)?;
    let empty: Vec<u8> = first.band_valid.iter().map(|&v| u8::from(v > 0)).collect();
    if first.n == 0 && empty.contains(&0) && empty.contains(&1) {
        let both: Vec<u8> = match base.active {
            Some(a) => a.iter().zip(&empty).map(|(&p, &q)| p & q).collect(),
            None => empty,
        };
        let mut again = gather(inputs, roles, dates, date0, window, base, Some(&both), n_jobs, cancel, progress)?;
        again.band_valid = first.band_valid;
        return Ok(again);
    }
    Ok(first)
}

/// The table of eigenvalues, as `MNFTransform.table()`: component, eigenvalue, SNR, SNR in dB, noise fraction, share
/// of the signal, its running sum, the lag-1 autocorrelation of the estimator's direction, and a flag.
pub fn eigen_table(tr: &MnfTransform, given: bool) -> Vec<Vec<String>> {
    let lam = &tr.eigenvalues;
    let positive: Vec<f64> = lam.iter().map(|l| (l - 1.0).max(0.0)).collect();
    let total: f64 = positive.iter().sum();
    let mut cumulative = 0.0;
    (0..lam.len())
        .map(|i| {
            let snr = lam[i] - 1.0;
            let share = if total > 0.0 { positive[i] / total } else { 0.0 };
            cumulative += share;
            let flag = if tr.is_degenerate(i) {
                "degenerate"
            } else if tr.floored.get(i).copied().unwrap_or(false) {
                "floored_noise"
            } else {
                ""
            };
            let db = if snr > 0.0 { 10.0 * snr.log10() } else { f64::NAN };
            let maf = if given { f64::NAN } else { 1.0 - 1.0 / lam[i] };
            vec![
                (i + 1).to_string(),
                planes::csv_num(lam[i]),
                planes::csv_num(snr),
                planes::csv_num(db),
                planes::csv_num(1.0 / lam[i]),
                planes::csv_num(share),
                planes::csv_num(cumulative),
                planes::csv_num(maf),
                flag.to_string(),
            ]
        })
        .collect()
}

const EIGEN_HEADER: [&str; 9] = [
    "component",
    "eigenvalue",
    "snr",
    "snr_db",
    "noise_fraction",
    "signal_share",
    "cumulative_signal_share",
    "maf_autocorrelation",
    "flag",
];

/// The transform as Zeit's `MNFTransform.to_json` writes it (`"format": "zeit-mnf"`, version 1).
pub fn transform_json(
    tr: &MnfTransform,
    names: &[String],
    noise: &str,
    region: &str,
    s: &Settings,
    date: Option<&str>,
) -> Value {
    let dropped: Vec<Value> = tr
        .dropped
        .iter()
        .map(|(j, r)| {
            json!({ "band": j, "reason": match r { zm::Dropped::Constant => "constant", zm::Dropped::Empty => "empty" } })
        })
        .collect();
    let pairs: serde_json::Map<String, Value> =
        directions(noise).iter().zip(&tr.n_pairs).map(|(d, &m)| ((*d).to_string(), json!(m))).collect();
    json!({
        "format": "zeit-mnf", "version": 1, "zeit": crate::ZEIT_VERSION,
        "bands": names, "kept": tr.kept, "dropped": dropped,
        "estimator": noise, "noise_region": region,
        "sample_fraction": s.fraction, "seed": s.seed,
        "per_date": false, "date": date,
        "n_pixels": tr.n_pixels, "n_pairs": pairs,
        "mean": tr.mean, "cov": tr.cov, "noise_cov": tr.noise_cov,
        "eigenvalues": tr.eigenvalues, "forward": tr.forward, "inverse": tr.inverse,
        "correlation": tr.correlation,
        "eps_noise": zm::EPS_NOISE, "noise_floored": tr.noise_floored,
        "floored": tr.floored,
    })
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let s = settings(p)?;
    let bands = planes::band_set(spec, p.str("bands"), 2, "the MNF")?;
    let nb = bands.roles.len();
    let mut inputs = Inputs::new(spec)?;
    let window = planes::window_of(spec, &inputs);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let n = w * h;
    if n == 0 {
        return Err("ValueError: the window has no pixel".into());
    }
    let t = spec.years.len();
    let date = date_of(spec, p);
    let dates: Vec<usize> = if s.all_dates { (0..t).collect() } else { vec![date] };
    let date0 = if s.all_dates { 0 } else { date };
    let n_jobs = spec.n_jobs();
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // ---- the noise region
    let region_file = p.str("noise_region");
    let region = if region_file.is_empty() {
        None
    } else {
        Some(planes::region_mask(region_file, inputs.input.ds(), window, &dir, cancel)?)
    };
    let region_kind = if region.is_some() { "mask" } else { "whole" };
    let mut base = StatsOptions::new(noise_of(&s.noise));
    base.noise_region = region.as_deref();
    base.sample_fraction = s.fraction;
    base.seed = s.seed;

    // ---- pass 1: the statistics of the whole window, then the transform
    progress(0.0, "the statistics of the window");
    let stats = fit_stats(&mut inputs, &bands.roles, &dates, date0, window, &base, n_jobs, cancel, &|f| {
        progress(0.45 * f, "the statistics of the window")
    })?;
    let tr = zm::mnf_fit(&stats).map_err(explain)?;
    let m = tr.m;
    let k_out = if s.n_components == 0 { m } else { s.n_components.min(m) };
    let k_den = if s.denoised { denoise_k(&s, &tr)? } else { 0 };
    progress(0.45, &format!("{m} components from {} pixels", tr.n_pixels));

    // ---- the files of the run
    let comp_template = tool.outputs.iter().find(|o| o.id == "mnf").cloned().ok_or("no mnf output")?;
    let mut wanted = vec![];
    if s.components {
        for k in 1..=k_out {
            let mut o = comp_template.clone();
            o.id = format!("mnf_{k}");
            o.name = o.name.replace("{}", &k.to_string());
            o.each = None;
            o.series = s.all_dates.then_some(true);
            wanted.push(o);
        }
    }
    let mut outs = Outputs::new(tool, &wanted, &dir, inputs.input.ds(), window, spec)?;
    let comp_names: Vec<String> = (1..=k_out).map(|k| format!("mnf{k}")).collect();
    let mut multi = if s.components && !s.all_dates {
        Some(Tifs::create(vec![dir.join("mnf_components.tif")], &comp_names, inputs.input.ds(), window, spec)?)
    } else {
        None
    };
    let file_names = planes::date_file_names(spec);
    let series_dir = dir.join("denoised");
    let mut denoised = if s.denoised {
        std::fs::create_dir_all(&series_dir).map_err(|e| format!("{}: {e}", series_dir.display()))?;
        let paths = dates.iter().map(|&d| series_dir.join(format!("{}.tif", file_names[d]))).collect();
        Some(Tifs::create(paths, &bands.names, inputs.input.ds(), window, spec)?)
    } else {
        None
    };

    // ---- pass 2: the transform applied to every pixel
    let rows = planes::strip_rows(dates.len(), nb, w, h);
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let np = w * hh;
        let strip = inputs.read_dates(&bands.roles, &dates, x0, y0 + r, w, hh)?;
        let mut res = ChunkOut::new();
        let mut per_comp: Vec<Vec<f64>> = vec![Vec::with_capacity(dates.len() * np); k_out];
        for (di, x) in strip.iter().enumerate() {
            if s.components {
                let mut y = vec![0f32; k_out * np];
                zm::mnf_forward_batch(x, nb, np, &tr, k_out, s.scale, &[], None, &mut y, n_jobs);
                for (k, plane) in per_comp.iter_mut().enumerate() {
                    plane.extend(y[k * np..(k + 1) * np].iter().map(|&v| f64::from(v)));
                }
                if let Some(mt) = &mut multi {
                    mt.write(0, r, hh, &y)?;
                }
            }
            if let Some(dt) = &mut denoised {
                let mut out = vec![0f32; nb * np];
                zm::mnf_denoise_batch(x, nb, np, &tr, k_den, &[], None, &mut out, n_jobs);
                dt.write(di, r, hh, &out)?;
            }
        }
        if s.components {
            for (k, plane) in per_comp.into_iter().enumerate() {
                res.insert(format!("mnf_{}", k + 1), plane);
            }
            outs.write(&res, r, hh)?;
        }
        r += hh;
        progress(0.45 + 0.5 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }
    drop(multi);
    drop(denoised);

    // ---- the tables, and what the app shows of each component
    let given = false;
    planes::write_csv(&dir.join("mnf_eigenvalues.csv"), &EIGEN_HEADER, &eigen_table(&tr, given))?;
    let label = (!s.all_dates).then(|| crate::job::date_labels(spec)[date].clone());
    let doc = transform_json(&tr, &bands.names, &s.noise, region_kind, &s, label.as_deref());
    let text = serde_json::to_string(&doc).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("mnf_transform.json"), text).map_err(|e| e.to_string())?;
    let mut extra = serde_json::Map::new();
    for k in 1..=if s.components { k_out } else { 0 } {
        let snr = tr.eigenvalues[k - 1] - 1.0;
        extra.insert(
            format!("mnf_{k}"),
            json!({ "name": format!("MNF {k}: SNR {snr:.2}"), "eigenvalue": tr.eigenvalues[k - 1], "snr": snr }),
        );
    }
    let model = json!({ "outputs": Value::Object(extra) });
    let mut outputs = outs.describe(p, Some(&model));
    drop(outs);
    if s.denoised {
        outputs.push(json!({
            "id": "denoised", "name": format!("Denoised bands (MNF, {k_den} components)"),
            "unit": "as the input", "series": true, "path": series_dir.to_string_lossy(),
        }));
    }
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}

/// The run's time: the two passes over the window, the statistics (about `nb^2` operations a pixel) and the transform.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let nb = spec.bands.keys().filter(|k| k.starts_with('b')).count().max(2) as f64;
    let dates = if p.str("scope") == "all_dates" { spec.years.len().max(1) as f64 } else { 1.0 };
    Ok(Estimate {
        sec_per_px: dates * (4e-9 * nb * nb + 2e-8 * nb),
        sec_per_chunk: 0.0,
        chunk_cells: cells,
        sec_fixed: 0.0,
    })
}
