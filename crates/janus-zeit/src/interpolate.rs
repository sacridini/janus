//! Interpolation to a regular grid of dates (FORCE's TSA interpolation, Frantz 2019), as zeit.interpolate with its
//! default grid: from the run's first date, every `step` days, `floor((last - first + 1) / step)` dates. The
//! series' values at the new dates are zeit-core's `interp_batch`; the new dates are the output's bands (the tool
//! decides them once per job, `bands`), each labelled with its ISO date.

use zeit_core::tsa::interp::{Method, interp_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::manifest::Output;
use crate::numeric::{civil, decimal_year};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, row};

/// The new dates: days since 1970-01-01, and the times in days since the first date of the series (the series' and
/// the grid's, as `interp_batch` takes them).
struct Grid {
    days: Vec<i64>,
    t_in: Vec<f64>,
    t_out: Vec<f64>,
}

fn grid(p: &Params, input: &PixelInput) -> Result<Grid, String> {
    let days = input.days.as_ref().filter(|d| !d.is_empty()).ok_or(
        "ValueError: interpolate needs dated series (this one has decimal years only): open a series whose dates are \
         read from its names or its time coordinate",
    )?;
    let step = p.int("step");
    if step < 1 {
        return Err(format!("ValueError: step must be a positive number of days, got {step}"));
    }
    let (first, last) = (days[0], days[days.len() - 1]);
    let n = (last - first + 1).div_euclid(step);
    if n < 1 {
        return Err(format!(
            "ValueError: no date in the grid: the series spans {} days, the step is {step}",
            last - first + 1
        ));
    }
    let out: Vec<i64> = (0..n).map(|k| first + k * step).collect();
    Ok(Grid {
        t_in: days.iter().map(|&d| (d - first) as f64).collect(),
        t_out: out.iter().map(|&d| (d - first) as f64).collect(),
        days: out,
    })
}

/// The method with its parameters, checked as zeit.interpolate checks them.
fn method(p: &Params) -> Result<Method, String> {
    Ok(match p.str("method") {
        "linear" => Method::Linear,
        "moving" => {
            let window = p.f64("window");
            if window.is_nan() || window < 0.0 {
                return Err(format!("ValueError: window must not be negative, got {}", p.f64("window")));
            }
            Method::Moving { window }
        }
        "rbf" => {
            let sigma =
                p.str("sigma").split(',').map(|s| s.trim().parse::<f64>()).collect::<Result<Vec<f64>, _>>().map_err(
                    |_| format!("ValueError: sigma must be days separated by commas, got {:?}", p.str("sigma")),
                )?;
            if sigma.iter().any(|&x| x <= 0.0 || !x.is_finite()) || sigma.windows(2).any(|w| w[1] < w[0]) {
                return Err(format!("ValueError: sigma must be positive and increasing, got {:?}", p.str("sigma")));
            }
            let cutoff = p.f64("cutoff");
            if !(cutoff > 0.0 && cutoff <= 1.0) {
                return Err(format!("ValueError: cutoff must be in (0, 1], got {cutoff}"));
            }
            Method::Rbf { sigma, cutoff }
        }
        "harmonic" => {
            let modes = p.int("modes");
            if !(1..=3).contains(&modes) {
                return Err(format!("ValueError: modes must be 1, 2 or 3, got {modes}"));
            }
            Method::Harmonic {
                modes: modes as usize,
                trend: p.bool("trend"),
                fit: (f64::NEG_INFINITY, f64::INFINITY),
                period: 365.25,
            }
        }
        m => return Err(format!("ValueError: method must be one of rbf, linear, moving, harmonic, got {m:?}")),
    })
}

/// The labels of the output's bands: the ISO dates of the grid.
pub fn bands(_o: &Output, p: &Params, input: &PixelInput) -> Result<Vec<String>, String> {
    method(p)?;
    Ok(grid(p, input)?
        .days
        .iter()
        .map(|&d| {
            let (y, m, dd) = civil(d);
            format!("{y:04}-{m:02}-{dd:02}")
        })
        .collect())
}

/// Every pixel's series on the grid, [pixels][dates].
fn run(
    p: &Params,
    values: &[f64],
    n_pixels: usize,
    input: &PixelInput,
    n_jobs: i32,
) -> Result<(Grid, Vec<f64>), String> {
    let method = method(p)?;
    let g = grid(p, input)?;
    let out = interp_batch(values, n_pixels, g.t_in.len(), &g.t_in, &g.t_out, &method, n_jobs)
        .map_err(|e| format!("ValueError: {e}"))?;
    Ok((g, out))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let n_valid = input.values.iter().filter(|v| v.is_finite()).count();
    let (g, out) = run(p, &input.values, 1, input, 1)?;
    let label = match p.str("method") {
        "rbf" => "radial basis functions",
        "linear" => "linear",
        "moving" => "moving average",
        _ => "harmonic",
    };
    let empty = out.iter().filter(|v| !v.is_finite()).count();
    let rows = vec![
        row("Method", label),
        row("Grid dates", format!("{} (every {} days)", g.days.len(), p.int("step"))),
        row("Valid observations", format!("{n_valid} of {}", input.values.len())),
        row("Dates without a value", empty.to_string()),
    ];
    let x: Vec<f64> = g.days.iter().map(|&d| decimal_year(d)).collect();
    let y: Vec<Option<f64>> = out.iter().map(|&v| v.is_finite().then_some(v)).collect();
    Ok(PixelResult { overlays: vec![Overlay::new("line", &format!("Interpolated ({label})"), x, y)], rows })
}

/// A raster run's chunk: every pixel's series on the new grid, [dates][pixels].
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let n = chunk.pixels();
    let (g, out) = run(p, &chunk.by_pixel(&chunk.stack), n, &ctx.input, ctx.n_jobs)?;
    let m = g.days.len();
    // [pixels][dates] -> [dates][pixels]
    let mut bands = vec![0.0; n * m];
    for px in 0..n {
        for k in 0..m {
            bands[k * n + px] = out[px * m + k];
        }
    }
    let mut res = ChunkOut::new();
    res.insert("series".into(), bands);
    Ok(res)
}
