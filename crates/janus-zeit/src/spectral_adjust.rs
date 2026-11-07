//! Spectral adjustment to Sentinel-2A (FORCE; Scheffler, Frantz and Segl 2020, RSE 241:111723): zeit.spectral_adjust,
//! the six bands of Landsat 4-9 or MODIS (blue, green, red, NIR, SWIR1, SWIR2) of every date predicted as the ten
//! bands of Sentinel-2A, by zeit-core's `spectral_adjust_batch`. The bands are reflectance x 10000 as the other
//! multiband tools read them (`units`; the observations the quality band rejects left out), and so are the ten
//! series written, as Zeit's on the Python snippet's `bands_cube` with `scale=10000`.
//!
//! Each date's sensor is the `sensor` parameter's, or, as `auto`, that of its scene (`sensors`). Zeit passes the
//! dates of Sentinel-2 through, which needs its ten bands, among them the red edge ones Janus does not map to a
//! role: those dates are refused, and a series of one Landsat sensor is adjusted by naming it.

use zeit_core::specadjust::{N_DST, N_SRC, Quirk, Sensor, spectral_adjust_batch_as};

use crate::bands::{LANDSAT_ROLES, landsat_chunk, missing_roles, series_overlay};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Params, PixelInput, PixelResult, row};
use crate::sensors::date_text;

/// The ten Sentinel-2A bands, in the order of the core's output: the output ids, and the names of the bands.
pub const TARGET: [(&str, &str); N_DST] = [
    ("blue", "Blue"),
    ("green", "Green"),
    ("red", "Red"),
    ("rededge1", "Red edge 1 (B5)"),
    ("rededge2", "Red edge 2 (B6)"),
    ("rededge3", "Red edge 3 (B7)"),
    ("nir", "Broad NIR (B8)"),
    ("nir08", "NIR (B8A)"),
    ("swir16", "SWIR1 (B11)"),
    ("swir22", "SWIR2 (B12)"),
];

/// Why the tool cannot run on this series, if it cannot.
fn check(input: &PixelInput) -> Option<String> {
    let missing = missing_roles(input, &LANDSAT_ROLES);
    if !missing.is_empty() {
        return Some(format!("map the bands first: {}", missing.join(", ")));
    }
    input.days.is_none().then(|| "needs real dates".into())
}

/// The sensor of every date: the parameter's, or each date's own.
fn sensors(p: &Params, t: usize, input: &PixelInput) -> Result<Vec<Sensor>, String> {
    let given = p.str("sensor");
    if given != "auto" {
        return Ok(vec![Sensor::parse(given)?; t]);
    }
    if input.sensors.len() != t {
        return Err("ValueError: spectral_adjust needs each date's sensor: Janus could not read them (from the STAC \
             platform or the file names); say which sensor the series is of"
            .into());
    }
    input
        .sensors
        .iter()
        .enumerate()
        .map(|(k, s)| match s.as_str() {
            "SEN2" => Err(format!(
                "ValueError: date {} is Sentinel-2: Zeit passes it through with its ten bands, which Janus does not map \
                 (no red edge roles); adjust a Landsat or MODIS series, or name its sensor",
                date_text(input, k)
            )),
            "" => Err(format!(
                "ValueError: unknown sensor of the date {}: the known sensors are Landsat 4, 5, 7, 8, 9 and MODIS Terra \
                 (MOD01) and Aqua (MOD02); say which one the series is of",
                date_text(input, k)
            )),
            name => Sensor::parse(name),
        })
        .collect()
}

/// The ten bands of a chunk ([band][T][pixels], NaN where a band of the date is missing), and the sensors.
fn compute(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<(Vec<Vec<f64>>, Vec<Sensor>), String> {
    let (t, n) = (chunk.t, chunk.pixels());
    let sensors = sensors(p, t, &ctx.input)?;
    let (cube, _) = landsat_chunk(p, chunk, ctx, &LANDSAT_ROLES, true)?;
    let quirk = if p.bool("force_search") { Quirk::Force } else { Quirk::Fixed };
    let mut out = vec![vec![0.0; t * n]; N_DST];
    let mut x = vec![0.0; n * N_SRC];
    let mut y = vec![0.0; n * N_DST];
    for k in 0..t {
        // pixels x 6
        for px in 0..n {
            for (b, band) in cube.iter().enumerate() {
                x[px * N_SRC + b] = band[k * n + px];
            }
        }
        spectral_adjust_batch_as(&x, sensors[k], &mut y, ctx.n_jobs, quirk);
        for px in 0..n {
            for (b, band) in out.iter_mut().enumerate() {
                band[k * n + px] = y[px * N_DST + b];
            }
        }
    }
    Ok((out, sensors))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Spectral adjustment", why)] });
    }
    let (bands, sensors) = compute(p, &input.as_chunk(), &JobCtx::for_pixel(input))?;
    let valid = bands[0].iter().filter(|v| v.is_finite()).count();
    let mut seen: Vec<(&str, usize)> = vec![];
    for s in &sensors {
        match seen.iter_mut().find(|(n, _)| *n == s.name()) {
            Some((_, k)) => *k += 1,
            None => seen.push((s.name(), 1)),
        }
    }
    let rows = vec![
        row("Valid observations", format!("{valid} of {}", input.values.len())),
        row(
            "Sensors",
            seen.iter()
                .map(|(n, k)| format!("{n} ({k} date{})", if *k == 1 { "" } else { "s" }))
                .collect::<Vec<_>>()
                .join(", "),
        ),
        row("Closest cluster search", if p.bool("force_search") { "FORCE's (skips cluster 9)" } else { "as meant" }),
    ];
    let overlays = TARGET
        .iter()
        .zip(&bands)
        .map(|((_, name), values)| series_overlay(&format!("Sentinel-2A: {name}"), input, values, "line"))
        .collect();
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: the ten bands as series ([T][pixels]), under the ids of `TARGET`.
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let with_bands = PixelInput { bands: chunk.bands.clone(), ..ctx.input.clone() };
    if let Some(why) = check(&with_bands) {
        return Err(format!("ValueError: Spectral adjustment: {why}"));
    }
    let (bands, _) = compute(p, chunk, ctx)?;
    let mut res = ChunkOut::new();
    for ((id, _), values) in TARGET.iter().zip(bands) {
        res.insert((*id).to_string(), values);
    }
    Ok(res)
}
