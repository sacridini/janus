//! Standardization along time (FORCE's STANDARDIZE), as zeit.standardize with its defaults (ddof = 1): each
//! series centred on the mean of its valid observations, or also divided by their deviation; a series with fewer
//! than 2 valid observations stays as it is, a constant one gives NaN when normalized. The result is a series on
//! the run's dates (zeit-core's `standardize_batch`).

use zeit_core::tsa::stats::{Metric, Standardize, series_metrics, standardize_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

fn method(p: &Params) -> Result<Standardize, String> {
    match p.str("method") {
        m @ ("normalize" | "center") => Standardize::parse(m).map_err(|e| format!("ValueError: {e}")),
        m => Err(format!("ValueError: method must be 'normalize' or 'center', got {m:?}")),
    }
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let method = method(p)?;
    let out = standardize_batch(&input.values, 1, input.values.len(), method, 1.0, 1);
    let st = series_metrics(&input.values, &[Metric::Mean, Metric::Std, Metric::Count], 1.0);
    let label = if method == Standardize::Normalize { "Standardized (z-score)" } else { "Centred" };
    let rows = vec![
        row("Valid observations", format!("{} of {}", st[2], input.values.len())),
        row("Mean", g(st[0], 6)),
        row("Standard deviation", g(st[1], 6)),
    ];
    let y = out.iter().map(|&v| v.is_finite().then_some(v)).collect();
    Ok(PixelResult { overlays: vec![Overlay::new("line", label, input.years.clone(), y)], rows })
}

/// A raster run's chunk: every pixel's standardized series, [dates][pixels].
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let method = method(p)?;
    let (n, t) = (chunk.pixels(), chunk.t);
    let out = standardize_batch(&chunk.by_pixel(&chunk.stack), n, t, method, 1.0, ctx.n_jobs);
    let mut bands = vec![0.0; n * t];
    for px in 0..n {
        for i in 0..t {
            bands[i * n + px] = out[px * t + i];
        }
    }
    let mut res = ChunkOut::new();
    res.insert("series".into(), bands);
    Ok(res)
}
