//! Nadir BRDF-adjusted reflectance (NBAR, the c-factor of Roy et al. 2016 and 2017): zeit.nbar, each mapped band
//! multiplied by the ratio of the Ross-Thick / Li-Sparse-Reciprocal model of MODIS at nadir view and the normalized
//! solar zenith to the model at the pixel's own sun and view angles, by zeit-core's `nbar_bands` (float64: Janus
//! reads its bands as float64, and the maps are written as float32). The four angle bands (`sza`, `saa`, `vza`,
//! `vaa`) are bands of each date like the others, in degrees or in hundredths of a degree (`angle_scale`); the
//! bands are not scaled to anything (the c-factor is a ratio), so the result is in the input's units.
//!
//! The normalized solar zenith of Zeit's default (`sza="scene"`) is each date's mean observed solar zenith over the
//! pixels whose four angles are valid, which a raster run in strips cannot know from one strip: `run_job` reads the
//! angle bands once over the whole window first (`scene_means`, strip by strip, the sums and counts added) and gives
//! the means to every strip through the context's model. Without them (a pixel run, the estimate's samples) the
//! mean is that of the chunk itself: a pixel's own solar zenith.

use serde_json::Value;
use zeit_core::brdf::{Geometry, ROLES, RossLi, nbar_bands, role};

use crate::bands::series_overlay;
use crate::job::{Chunk, ChunkOut, Inputs, JobCtx, JobSpec};
use crate::pixel::{Params, PixelInput, PixelResult, g, row};

/// The angle bands' roles, in the order of the core's geometry.
pub const ANGLES: [&str; 4] = ["sza", "saa", "vza", "vaa"];

/// The factor that takes the angle bands' values to degrees.
fn angle_scale(p: &Params) -> f64 {
    if p.str("angle_scale") == "hundredths" { 0.01 } else { 1.0 }
}

/// The roles with parameters among those mapped, in the core's order.
fn reflectance_roles<'a>(mapped: impl Iterator<Item = &'a String> + Clone) -> Vec<&'static str> {
    ROLES.iter().copied().filter(|r| mapped.clone().any(|m| m == r)).collect()
}

/// What is mapped and asked for, checked: the bands to normalize (the core's order), or why the tool cannot run.
fn plan<'a>(p: &Params, mapped: impl Iterator<Item = &'a String> + Clone) -> Result<Vec<&'static str>, String> {
    let missing: Vec<&str> = ANGLES.iter().copied().filter(|a| !mapped.clone().any(|m| m == a)).collect();
    if !missing.is_empty() {
        return Err(format!("map the angle bands first: {}", missing.join(", ")));
    }
    let roles = reflectance_roles(mapped);
    if roles.is_empty() {
        return Err(format!("map at least one band to normalize ({})", ROLES.join(", ")));
    }
    match p.str("sza") {
        "scene" => {}
        "fixed" => {
            let v = p.f64("sza_fixed");
            if !(0.0..90.0).contains(&v) {
                return Err(format!("the fixed solar zenith must be 0 to 90 degrees, got {}", g(v, 6)));
            }
        }
        s => return Err(format!("sza must be 'scene' or 'fixed', got {s:?}")),
    }
    Ok(roles)
}

/// The bands a run normalizes, from the bands its spec maps (the outputs follow them), or the error that stops it.
pub(crate) fn prepare(p: &Params, spec: &JobSpec) -> Result<Vec<&'static str>, String> {
    let keys: Vec<String> = spec.bands.keys().cloned().collect();
    plan(p, keys.iter()).map_err(|e| format!("ValueError: NBAR: {e}"))
}

/// The angles of a window as `scene_means` and a chunk's own mean read them: for each date, the sum of the solar
/// zenith and the count of the pixels whose four angles are finite and whose solar and view zeniths are in 0-90
/// degrees (zeit's `_scene_sza`).
fn accumulate(angles: &std::collections::BTreeMap<String, Vec<f64>>, scale: f64, sum: &mut [f64], count: &mut [usize]) {
    let n = angles["sza"].len() / sum.len().max(1);
    let at = |name: &str, k: usize, i: usize| angles[name][k * n + i] * scale;
    for k in 0..sum.len() {
        for i in 0..n {
            let (sza, saa, vza, vaa) = (at("sza", k, i), at("saa", k, i), at("vza", k, i), at("vaa", k, i));
            let finite = sza.is_finite() && saa.is_finite() && vza.is_finite() && vaa.is_finite();
            if finite && (0.0..90.0).contains(&sza) && (0.0..90.0).contains(&vza) {
                sum[k] += sza;
                count[k] += 1;
            }
        }
    }
}

fn means(sum: &[f64], count: &[usize]) -> Vec<f64> {
    sum.iter().zip(count).map(|(&s, &c)| if c > 0 { s / c as f64 } else { f64::NAN }).collect()
}

/// Each date's mean observed solar zenith over the window `window` (x0, y0, x1, y1), the angle bands read in strips
/// of `rows` rows; NaN for a date without a valid pixel.
pub(crate) fn scene_means(
    inputs: &mut Inputs,
    p: &Params,
    window: [usize; 4],
    rows: usize,
    progress: &(dyn Fn(f64) + Sync),
) -> Result<Vec<f64>, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let t = inputs.dates();
    let scale = angle_scale(p);
    let (mut sum, mut count) = (vec![0.0; t], vec![0usize; t]);
    for r in (0..h).step_by(rows.max(1)) {
        let hh = rows.max(1).min(h - r);
        let angles = inputs.read_roles(&ANGLES, x0, y0 + r, w, hh)?;
        accumulate(&angles, scale, &mut sum, &mut count);
        progress((r + hh) as f64 / h as f64);
    }
    Ok(means(&sum, &count))
}

/// The normalized solar zenith of each date of a chunk: the fixed angle, or the scene means the run read (the
/// context's model), or, without them, the chunk's own.
fn normalized(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Vec<f64> {
    let t = chunk.t;
    if p.str("sza") == "fixed" {
        return vec![p.f64("sza_fixed"); t];
    }
    let given = ctx.model.as_ref().and_then(|m| m.get("nbar_sza")).and_then(Value::as_array).filter(|a| a.len() == t);
    if let Some(a) = given {
        return a.iter().map(|v| v.as_f64().unwrap_or(f64::NAN)).collect();
    }
    let angles: std::collections::BTreeMap<String, Vec<f64>> =
        ANGLES.iter().filter_map(|a| Some((a.to_string(), chunk.bands.get(*a)?.clone()))).collect();
    let (mut sum, mut count) = (vec![0.0; t], vec![0usize; t]);
    if angles.len() == ANGLES.len() {
        accumulate(&angles, angle_scale(p), &mut sum, &mut count);
    }
    means(&sum, &count)
}

/// What `compute` gives.
struct Computed {
    /// the bands normalized, in the core's order
    roles: Vec<&'static str>,
    /// [band][date x pixels]
    nbar: Vec<Vec<f64>>,
    /// the c-factors, the same shape (empty unless asked)
    cfactor: Vec<Vec<f64>>,
    /// the normalized solar zenith of each date
    sza: Vec<f64>,
}

/// The NBAR of a chunk's bands, NaN on a date whose normalized zenith is not known, where a band or an angle is
/// missing, and where a zenith is outside 0-90 degrees.
fn compute(p: &Params, chunk: &Chunk, ctx: &JobCtx, with_c: bool) -> Result<Computed, String> {
    let roles = plan(p, chunk.bands.keys())?;
    let (t, n) = (chunk.t, chunk.pixels());
    let scale = angle_scale(p);
    let sza_norm = normalized(p, chunk, ctx);
    let coefs: Vec<RossLi> = roles.iter().map(|r| role(r).expect("a role of the core")).collect();
    let nb = roles.len();
    let mut nbar = vec![vec![f64::NAN; t * n]; nb];
    let mut cfactor = if with_c { vec![vec![f64::NAN; t * n]; nb] } else { vec![] };
    let degrees = |name: &str, k: usize| -> Vec<f64> {
        chunk.bands[name][k * n..(k + 1) * n].iter().map(|v| v * scale).collect()
    };
    for k in 0..t {
        let norm = sza_norm[k];
        if !norm.is_finite() || n == 0 {
            continue;
        }
        let refl: Vec<f64> = roles.iter().flat_map(|r| chunk.bands[*r][k * n..(k + 1) * n].iter().copied()).collect();
        let (sza, saa, vza, vaa) = (degrees("sza", k), degrees("saa", k), degrees("vza", k), degrees("vaa", k));
        let geo = Geometry { sza: &sza, saa: &saa, vza: &vza, vaa: &vaa };
        let mut out = vec![0.0f64; nb * n];
        let mut c = vec![0.0f64; if with_c { nb * n } else { 0 }];
        nbar_bands(&refl, &geo, &coefs, norm, &mut out, with_c.then_some(&mut c[..]), ctx.n_jobs);
        for b in 0..nb {
            nbar[b][k * n..(k + 1) * n].copy_from_slice(&out[b * n..(b + 1) * n]);
            if with_c {
                cfactor[b][k * n..(k + 1) * n].copy_from_slice(&c[b * n..(b + 1) * n]);
            }
        }
    }
    Ok(Computed { roles, nbar, cfactor, sza: sza_norm })
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Err(why) = plan(p, input.bands.keys()) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("NBAR", why)] });
    }
    let c = compute(p, &input.as_chunk(), &JobCtx::for_pixel(input), true)?;
    let valid = c.nbar[0].iter().filter(|v| v.is_finite()).count();
    let finite: Vec<f64> = c.sza.iter().copied().filter(|v| v.is_finite()).collect();
    let mean_sza = finite.iter().sum::<f64>() / finite.len().max(1) as f64;
    let zenith = if finite.is_empty() {
        "no date with a valid solar zenith".to_string()
    } else if p.str("sza") == "fixed" {
        format!("{} degrees", g(p.f64("sza_fixed"), 6))
    } else {
        // no scene on the chart: each date is brought to nadir at its own sun
        format!(
            "the pixel's own on each date, {} degrees on average (a scene's mean needs the raster run)",
            g(mean_sza, 5)
        )
    };
    let mut rows = vec![
        row("Valid observations", format!("{valid} of {}", input.values.len())),
        row("Normalized solar zenith", zenith),
    ];
    let mut overlays = vec![];
    for ((role, values), cf) in c.roles.iter().zip(&c.nbar).zip(&c.cfactor) {
        let ok: Vec<f64> = cf.iter().copied().filter(|v| v.is_finite()).collect();
        let mean =
            if ok.is_empty() { "no valid angles".to_string() } else { g(ok.iter().sum::<f64>() / ok.len() as f64, 6) };
        rows.push(row(format!("Mean c-factor ({role})"), mean));
        overlays.push(series_overlay(&format!("NBAR {role}"), input, values, "line"));
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: one series per band (`nbar_{role}`, [T][pixels]) and, when asked, its c-factors
/// (`cfactor_{role}`).
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let with_c = p.bool("cfactor");
    let mut c = compute(p, chunk, ctx, with_c).map_err(|e| format!("ValueError: NBAR: {e}"))?;
    let mut res = ChunkOut::new();
    for (k, role) in c.roles.iter().enumerate() {
        res.insert(format!("nbar_{role}"), std::mem::take(&mut c.nbar[k]));
        if with_c {
            res.insert(format!("cfactor_{role}"), std::mem::take(&mut c.cfactor[k]));
        }
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::Path;

    use serde_json::json;

    use super::*;

    fn data(name: &str) -> String {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/nbar").join(name).to_string_lossy().to_string()
    }

    fn spec(unit: &str, params: Value) -> JobSpec {
        let mut spec: JobSpec = serde_json::from_str(&std::fs::read_to_string(data("spec.json")).unwrap()).unwrap();
        spec.input = data("input.tif");
        spec.params = serde_json::from_value(params).unwrap();
        for r in ["red", "nir"].into_iter().chain(ANGLES) {
            let file = if ANGLES.contains(&r) { format!("{r}_{unit}.tif") } else { format!("{r}.tif") };
            spec.bands.insert(r.to_string(), data(&file));
        }
        spec
    }

    /// The scene means of a window read in strips of any height are the means over the window as Zeit takes them
    /// (the fixtures' `nbar_sza`), also when the strips split the image unevenly and the last one is short.
    #[test]
    fn scene_means_do_not_depend_on_the_strips() {
        let fixtures: Value = serde_json::from_str(&std::fs::read_to_string(data("nbar.json")).unwrap()).unwrap();
        let mut compared = 0;
        for (unit, case) in [("deg", 0), ("cd", 2)] {
            let want: Vec<f64> = fixtures["cases"][case]["sza"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap_or(f64::NAN))
                .collect();
            let p = Params::with_defaults(
                "nbar",
                &serde_json::from_value::<BTreeMap<_, _>>(json!({
                    "angle_scale": if unit == "cd" { "hundredths" } else { "degrees" }
                }))
                .unwrap(),
            )
            .unwrap();
            let spec = spec(unit, json!({}));
            let window = spec.window.unwrap();
            for rows in [1, 3, 5, 7, 16, 100] {
                let mut inputs = Inputs::new(&spec).unwrap();
                let got = scene_means(&mut inputs, &p, window, rows, &|_| {}).unwrap();
                assert_eq!(got.len(), want.len());
                for (k, (g, w)) in got.iter().zip(&want).enumerate() {
                    assert!(
                        (g.is_nan() && w.is_nan()) || (g - w).abs() <= 1e-9 * w.abs(),
                        "{unit}, strips of {rows} rows, date {k}: {g} vs {w}"
                    );
                    compared += usize::from(g.is_finite());
                }
            }
        }
        assert!(compared > 50);
    }
}
