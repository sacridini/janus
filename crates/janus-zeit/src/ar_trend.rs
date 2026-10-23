//! AR(1) trend (zeit.ar_trend): remotePARTS's fitAR on the series, a linear trend with AR(1) errors by REML
//! (Ives et al. 2021), its line (with the harmonics) drawn. Its slope map is what PARTS tests over a region.

use zeit_core::ar_reml::{self, Gaps};

use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// The design shared by the pixels (dates x q, by column): a mean, the trend in time steps (remotePARTS's
/// t = 1, 2, ...) or in years, and the harmonics' cosines and sines.
pub fn design(p: &Params, input: &PixelInput) -> (Vec<f64>, usize) {
    design_of(input, p.str("time") == "year", p.int("harmonics").max(0) as usize)
}

/// The design of `input`'s dates: the trend per year of the dates or per time step, and `harmonics` pairs.
pub fn design_of(input: &PixelInput, years: bool, harmonics: usize) -> (Vec<f64>, usize) {
    let n = input.years.len();
    let t: Vec<f64> = if years { input.years.clone() } else { (1..=n).map(|i| i as f64).collect() };
    let per_year = input.per_year() as f64;
    let phase: Vec<f64> =
        (0..n).map(|i| 2.0 * std::f64::consts::PI * if years { input.years[i] } else { i as f64 / per_year }).collect();
    let mut x = vec![1.0; n];
    x.extend_from_slice(&t);
    for k in 1..=harmonics {
        x.extend(phase.iter().map(|ph| (k as f64 * ph).cos()));
        x.extend(phase.iter().map(|ph| (k as f64 * ph).sin()));
    }
    (x, 2 + 2 * harmonics)
}

fn gaps(p: &Params) -> Result<Gaps, String> {
    match p.str("gaps") {
        "exact" => Ok(Gaps::Exact),
        "ignore" => Ok(Gaps::Ignore),
        m => Err(format!("ValueError: Unknown gaps {m:?}")),
    }
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let (x, q) = design(p, input);
    let n_valid = input.values.iter().filter(|v| v.is_finite()).count();
    let min = (p.int("min_valid").max(0) as usize).max(q + 2);
    if n_valid < min {
        return Ok(PixelResult {
            overlays: vec![],
            rows: vec![row("AR(1) trend", format!("not enough data ({n_valid} valid)"))],
        });
    }
    let Some(fit) = ar_reml::fit(&input.values, &x, q, gaps(p)?) else {
        // the harmonics of annual dates are constant
        let why = "the model's terms are not independent on these dates (harmonics need sub-annual dates)";
        return Ok(PixelResult { overlays: vec![], rows: vec![row("AR(1) trend", why)] });
    };
    let alpha = p.f64("alpha");
    let fmt = |v: f64| if v.is_finite() { g(v, 5) } else { "-".into() };
    let unit = if p.str("time") == "year" { "/yr" } else { "/step" };
    let (slope, sp) = (fit.coefficients[1], fit.p[1]);
    let significant = sp < alpha;
    let rows = vec![
        row(
            "Trend",
            if !significant || slope == 0.0 {
                "no trend"
            } else if slope > 0.0 {
                "increasing"
            } else {
                "decreasing"
            },
        ),
        row(format!("Significant (alpha {})", g(alpha, 6)), if significant { "yes" } else { "no" }),
        row("p-value", fmt(sp)),
        row("Slope", fmt(slope) + unit),
        row("Std. error", fmt(fit.se[1]) + unit),
        row("t", fmt(fit.t[1])),
        row("AR(1) rho", fmt(fit.rho)),
        row("Residual SD", fmt(fit.mse.sqrt())),
    ];
    // the fitted model at each date: the line, with the harmonics when there are
    let n = input.years.len();
    let ys: Vec<Option<f64>> =
        (0..n).map(|t| Some((0..q).map(|j| x[j * n + t] * fit.coefficients[j]).sum::<f64>())).collect();
    let overlays = vec![Overlay::new("line", "AR(1) trend", input.years.clone(), ys)];
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: the fit of every pixel; the slope also where it is significant.
pub fn chunk(p: &Params, chunk: &crate::job::Chunk, ctx: &crate::job::JobCtx) -> Result<crate::job::ChunkOut, String> {
    let (x, q) = design(p, &ctx.input);
    let n = chunk.pixels();
    let out = ar_reml::batch(
        &chunk.by_pixel(&chunk.stack),
        n,
        chunk.t,
        &x,
        q,
        gaps(p)?,
        p.int("min_valid").max(0) as usize,
        ctx.n_jobs,
    );
    // coefficients, se, t, p (q each), rho, mse, loglik (each [pixels])
    let metric = |k: usize| -> Vec<f64> { out[k * n..(k + 1) * n].to_vec() };
    let (slope, sp) = (metric(1), metric(3 * q + 1));
    let alpha = p.f64("alpha");
    let mut res = crate::job::ChunkOut::new();
    res.insert(
        "sig_slope".into(),
        slope.iter().zip(&sp).map(|(&s, &pv)| if pv < alpha { s } else { f64::NAN }).collect(),
    );
    res.insert("slope".into(), slope);
    res.insert("p".into(), sp);
    res.insert("t".into(), metric(2 * q + 1));
    res.insert("se".into(), metric(q + 1));
    res.insert("rho".into(), metric(4 * q));
    res.insert("intercept".into(), metric(0));
    res.insert("sd".into(), metric(4 * q + 1).iter().map(|v| v.sqrt()).collect());
    Ok(res)
}
