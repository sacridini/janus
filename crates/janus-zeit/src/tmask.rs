//! Tmask (zeit_bridge/tool_tmask.py): zeit.tmask's robust harmonic screening of the green and SWIR1 series, the
//! observations it flags (cloud or shadow) marked on the chart; a raster run maps how many it flags and writes
//! the shown series without them.

use crate::bands::{landsat_chunk, missing_roles, series_overlay};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Params, PixelInput, PixelResult, row};

const ROLES: [&str; 2] = ["green", "swir1"];

/// Python's date.toordinal() of 1970-01-01.
const UNIX_EPOCH_ORDINAL: i64 = 719163;

/// Why the tool cannot run on this series, if it cannot.
fn check(input: &PixelInput) -> Option<String> {
    let missing = missing_roles(input, &ROLES);
    if !missing.is_empty() {
        return Some(format!("map the bands first: {}", missing.join(", ")));
    }
    input.days.is_none().then(|| "needs real dates".into())
}

/// (flagged, valid) of each observation of a chunk ([T][pixels]): valid = both bands have a value the quality
/// band keeps.
fn flags(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<(Vec<bool>, Vec<bool>), String> {
    let (cube, _) = landsat_chunk(p, chunk, ctx, &ROLES, true)?;
    let (t, n) = (chunk.t, chunk.pixels());
    let valid: Vec<bool> = (0..t * n).map(|i| cube.iter().all(|b| b[i].is_finite() && b[i] > 0.0)).collect();
    // zeit.tmask: the dates as Python ordinals, at most 5 valid observations left as they are
    let days: Vec<f64> =
        ctx.input.days.as_ref().ok_or("needs real dates")?.iter().map(|&d| (d + UNIX_EPOCH_ORDINAL) as f64).collect();
    let (green, swir) = (chunk.by_pixel(&cube[0]), chunk.by_pixel(&cube[1]));
    let clear = zeit_core::tmask::tmask_batch(&green, &swir, &days, n, 10000.0, 5, 0.04, 0.04, ctx.n_jobs);
    // [pixels][T] -> [T][pixels]
    let flagged = (0..t * n).map(|i| valid[i] && clear[(i % n) * t + i / n] == 0).collect();
    Ok((flagged, valid))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Tmask", why)] });
    }
    let (flagged, valid) = flags(p, &input.as_chunk(), &JobCtx::for_pixel(input))?;
    let (n_valid, n_flag) = (valid.iter().filter(|&&v| v).count(), flagged.iter().filter(|&&f| f).count());
    let mut rows =
        vec![row("Valid observations", n_valid.to_string()), row("Flagged (cloud or shadow)", n_flag.to_string())];
    if n_valid <= 5 {
        rows.push(row("Tmask", "not screened (5 valid observations or fewer)"));
    }
    let shown: Vec<f64> = input.values.iter().zip(&flagged).map(|(&v, &f)| if f { v } else { f64::NAN }).collect();
    let overlays = if shown.iter().any(|v| v.is_finite()) {
        vec![series_overlay("Tmask: cloud or shadow", input, &shown, "markers")]
    } else {
        vec![]
    };
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let ctx_input = PixelInput { bands: chunk.bands.clone(), ..ctx.input.clone() };
    if let Some(why) = check(&ctx_input) {
        return Err(format!("ValueError: Tmask: {why}"));
    }
    let (flagged, valid) = flags(p, chunk, ctx)?;
    let (t, n) = (chunk.t, chunk.pixels());
    let count = |a: &[bool], px: usize| (0..t).filter(|&i| a[i * n + px]).count() as f64;
    let mut res = ChunkOut::new();
    let (nv, nf): (Vec<f64>, Vec<f64>) = (0..n).map(|px| (count(&valid, px), count(&flagged, px))).unzip();
    res.insert("n_flagged".into(), (0..n).map(|px| if nv[px] > 0.0 { nf[px] } else { f64::NAN }).collect());
    res.insert(
        "flagged_share".into(),
        (0..n).map(|px| if nv[px] > 0.0 { 100.0 * nf[px] / nv[px] } else { f64::NAN }).collect(),
    );
    if p.bool("write_series") {
        res.insert(
            "series".into(),
            chunk.stack.iter().zip(&flagged).map(|(&v, &f)| if f { f64::NAN } else { v }).collect(),
        );
    }
    Ok(res)
}
