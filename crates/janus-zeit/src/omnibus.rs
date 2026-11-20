//! The omnibus change test of a Sentinel-1 series (zeit.omnibus; Conradsen et al. 2016, Canty et al. 2020, Conradsen
//! et al. 2024): every pixel's series of VV (and VH) backscatter is tested for the equality of all its dates, and where
//! it is rejected, factorized into the tests of each date against those before it, which dates the changes. The test
//! is of one pixel at a time, so it runs as the other per-pixel tools do (strips of the window, a chart for a pin);
//! the core is zeit-core's `omnibus_batch`, which `zeit.omnibus` calls.
//!
//! The core takes linear power. `scale` says what the bands are: linear, dB (converted by 10^(x/10)), or
//! automatic: dB when the median of the finite values of the mapped bands is negative (linear power is positive; dB
//! backscatter of land and water is negative), decided once per run from its first strip, as the reflectance units of
//! the optical tools are. The dates in the maps are decimal years of the dates (the series' own, as the other change
//! tools give theirs), the directions the classes 1 none, 2 brightening, 3 darkening, 4 indefinite (Zeit's 0-3 plus
//! one: a class map has no class 0); the direction at each date keeps Zeit's codes.

use std::path::Path;

use serde_json::{Value, json};
use zeit_core::omnibus::{Approx, OmnibusOptions, omnibus_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::numeric::median;
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// The role of the VV band (required) and of the VH band (optional).
pub const VV: &str = "vv";
pub const VH: &str = "vh";

/// The planes of `omnibus_batch`, by the index the core writes them at.
const FIRST_CHANGE: usize = 0;
const LAST_CHANGE: usize = 1;
const N_CHANGES: usize = 2;
const FIRST_DIRECTION: usize = 3;
const LAST_DIRECTION: usize = 4;
const GREATEST_CHANGE: usize = 5;
const P_CHANGE: usize = 6;
const N_DATES: usize = 7;

/// The direction codes the core writes (0 none, 1 brightening, 2 darkening, 3 indefinite).
pub const DIRECTIONS: [&str; 4] = ["none", "brightening", "darkening", "indefinite"];

/// The bands of the series the test reads: VV, and VH when it is mapped.
pub fn roles(bands: &std::collections::BTreeMap<String, Vec<f64>>) -> Vec<&'static str> {
    [VV, VH].into_iter().filter(|r| bands.contains_key(*r)).collect()
}

/// The parameters of the tool as zeit.omnibus has them.
pub struct Settings {
    pub enl: f64,
    pub alpha: f64,
    pub approx: Approx,
}

pub fn settings(p: &Params) -> Result<Settings, String> {
    let enl = p.f64("enl");
    if enl.is_nan() || enl <= 0.0 {
        return Err(format!("ValueError: enl= must be positive, got {enl}"));
    }
    let alpha = p.f64("alpha");
    if !(alpha > 0.0 && alpha < 1.0) {
        return Err(format!("ValueError: alpha= must be between 0 and 1, got {alpha}"));
    }
    let approx = match p.str("approximation") {
        "improved" => Approx::Improved,
        "chi2" => Approx::Chi2,
        other => return Err(format!("ValueError: approximation= must be 'improved' or 'chi2', got {other:?}")),
    };
    Ok(Settings { enl, alpha, approx })
}

/// Whether the bands are in dB: the parameter, else the sign of the median of their finite values (decided once per
/// run).
pub fn is_db(p: &Params, bands: &[&[f64]], ctx: &JobCtx) -> bool {
    match p.str("scale") {
        "db" => true,
        "linear" => false,
        _ => ctx.once("omnibus_scale", || decide(bands)) == "db",
    }
}

fn decide(bands: &[&[f64]]) -> String {
    let mut v: Vec<f64> = bands.iter().flat_map(|b| b.iter().copied()).filter(|x| x.is_finite()).collect();
    if v.len() > 200_000 {
        let step = v.len() / 200_000;
        v = v.into_iter().step_by(step).collect();
    }
    (if !v.is_empty() && median(&v) < 0.0 { "db" } else { "linear" }).to_string()
}

/// What the core made for a chunk.
pub struct Computed {
    /// the eight planes, (8, pixels), the dates still indices of the series' dates
    pub planes: Vec<f32>,
    /// the direction at each date, (dates, pixels)
    pub per_date: Option<Vec<f32>>,
    pub db: bool,
    pub bands: Vec<&'static str>,
}

/// The core on a chunk's bands (linear power: dB converted first).
pub fn compute(p: &Params, chunk: &Chunk, ctx: &JobCtx, per_date: bool) -> Result<Computed, String> {
    let s = settings(p)?;
    if !chunk.bands.contains_key(VV) {
        return Err("ValueError: Omnibus: map the band of VV first".into());
    }
    let bands = roles(&chunk.bands);
    let (t, n) = (chunk.t, chunk.pixels());
    let arrays: Vec<&[f64]> = bands.iter().map(|r| chunk.bands[*r].as_slice()).collect();
    let db = is_db(p, &arrays, ctx);
    // (date, band, pixel), linear
    let mut x = Vec::with_capacity(t * bands.len() * n);
    for d in 0..t {
        for a in &arrays {
            let plane = &a[d * n..(d + 1) * n];
            if db {
                x.extend(plane.iter().map(|&v| if v.is_finite() { 10f64.powf(v / 10.0) } else { f64::NAN }));
            } else {
                x.extend_from_slice(plane);
            }
        }
    }
    let o = OmnibusOptions { enl: s.enl, alpha: s.alpha, approx: s.approx, p: bands.len() };
    let mut planes = vec![0f32; 8 * n];
    let mut per = per_date.then(|| vec![0f32; t * n]);
    let idx: Vec<usize> = (0..bands.len()).collect();
    omnibus_batch(&x, bands.len(), n, t, &idx, &o, &[], &mut planes, per.as_deref_mut(), ctx.n_jobs);
    Ok(Computed { planes, per_date: per, db, bands })
}

/// The decimal year of a date index (as float32, as Zeit's planes hold them), NaN for none.
fn year_of(years: &[f64], index: f32) -> f64 {
    if index.is_finite() { years.get(index as usize).map_or(f64::NAN, |&y| f64::from(y as f32)) } else { f64::NAN }
}

/// A raster run's chunk: the dates of the first, last and strongest change as decimal years, the number of changes,
/// the direction of the first and the last (classes 1-4), the p-value of the whole series and the valid dates, and
/// with `per_date` the direction at each date.
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let c = compute(p, chunk, ctx, p.bool("per_date"))?;
    let n = chunk.pixels();
    let plane = |k: usize| &c.planes[k * n..(k + 1) * n];
    let date = |k: usize| plane(k).iter().map(|&v| year_of(&ctx.input.years, v)).collect::<Vec<f64>>();
    let class =
        |k: usize| plane(k).iter().map(|&v| if v.is_finite() { f64::from(v) + 1.0 } else { f64::NAN }).collect();
    let plain = |k: usize| plane(k).iter().map(|&v| f64::from(v)).collect::<Vec<f64>>();
    let mut res = ChunkOut::new();
    res.insert("first_change".into(), date(FIRST_CHANGE));
    res.insert("last_change".into(), date(LAST_CHANGE));
    res.insert("greatest_change".into(), date(GREATEST_CHANGE));
    res.insert("n_changes".into(), plain(N_CHANGES));
    res.insert("first_direction".into(), class(FIRST_DIRECTION));
    res.insert("last_direction".into(), class(LAST_DIRECTION));
    res.insert("p_change".into(), plain(P_CHANGE));
    res.insert("n_dates".into(), plain(N_DATES));
    if let Some(per) = &c.per_date {
        res.insert("per_date".into(), per.iter().map(|&v| f64::from(v)).collect());
    }
    Ok(res)
}

/// What Zeit keeps in `attrs["omnibus"]`, and how the run read its bands.
pub fn record(p: &Params, years: &[f64], ctx: &JobCtx, with_vh: bool) -> Value {
    let scale = match p.str("scale") {
        "auto" => {
            ctx.cache.lock().ok().and_then(|c| c.get("omnibus_scale").cloned()).unwrap_or_else(|| "linear".into())
        }
        other => other.to_string(),
    };
    json!({
        "enl": p.f64("enl"),
        "alpha": p.f64("alpha"),
        "approximation": p.str("approximation"),
        "bands": if with_vh { vec![VV, VH] } else { vec![VV] },
        "scale": scale,
        "time_units": "decimal year",
        "dates": years,
        "n_dates": years.len(),
        "direction_codes": { "0": "none", "1": "brightening", "2": "darkening", "3": "indefinite" },
    })
}

/// The run's record, `omnibus.json` in its folder (Zeit's `attrs["omnibus"]`, with the scale the bands were read in).
pub fn write_record(dir: &Path, p: &Params, years: &[f64], ctx: &JobCtx, with_vh: bool) -> Result<(), String> {
    let path = dir.join("omnibus.json");
    let text = serde_json::to_string_pretty(&record(p, years, ctx, with_vh)).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

fn date_text(input: &PixelInput, year: f64) -> String {
    if !year.is_finite() {
        return "none".into();
    }
    if input.days.is_some() {
        let days = crate::numeric::decimal_year_days(year) as i64;
        let (y, m, d) = crate::numeric::civil(days);
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        g(year, 6)
    }
}

fn direction_name(plane_value: f32) -> &'static str {
    if plane_value.is_finite() { DIRECTIONS[(plane_value as usize).min(3)] } else { "-" }
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if !input.bands.contains_key(VV) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Omnibus", "map the band of VV first")] });
    }
    let c = compute(p, &input.as_chunk(), &JobCtx::for_pixel(input), true)?;
    let s = settings(p)?;
    let plane = |k: usize| c.planes[k];
    let mut rows = vec![
        row("Bands", if c.bands.len() == 2 { "VV and VH (dual polarization)" } else { "VV (single polarization)" }),
        row("Scale", if c.db { "dB, converted to linear power" } else { "linear power" }),
        row("ENL / alpha", format!("{} / {}", g(s.enl, 4), g(s.alpha, 4))),
    ];
    let valid = plane(N_DATES);
    if !valid.is_finite() {
        rows.push(row("Omnibus", "fewer than two valid dates (both bands positive)"));
        return Ok(PixelResult { overlays: vec![], rows });
    }
    let when = |k: usize| date_text(input, year_of(&input.years, plane(k)));
    rows.extend([
        row("Valid dates", format!("{} of {}", valid as usize, input.years.len())),
        row("P-value (all dates equal)", g(f64::from(plane(P_CHANGE)), 4)),
        row("Changes", format!("{}", plane(N_CHANGES) as usize)),
        row("First change", format!("{} ({})", when(FIRST_CHANGE), direction_name(plane(FIRST_DIRECTION)))),
        row("Last change", format!("{} ({})", when(LAST_CHANGE), direction_name(plane(LAST_DIRECTION)))),
        row("Strongest change", when(GREATEST_CHANGE)),
    ]);
    // every change the sequential test found, by its direction
    let mut overlays = vec![];
    if let Some(per) = &c.per_date {
        for (code, name) in DIRECTIONS.iter().enumerate().skip(1) {
            let at: Vec<f64> = per
                .iter()
                .zip(&input.years)
                .filter(|(v, _)| **v == code as f32)
                .map(|(_, &y)| f64::from(y as f32))
                .collect();
            if !at.is_empty() {
                overlays.push(Overlay::new("vlines", &format!("Omnibus change ({name})"), at, vec![]));
            }
        }
    }
    Ok(PixelResult { overlays, rows })
}
