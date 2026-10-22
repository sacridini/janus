//! Mann-Kendall (zeit_bridge/tool_mann_kendall.py): zeit.mann_kendall's test and Sen's slope on the series, Sen's
//! line drawn.

use zeit_core::mann_kendall::{Method, batch};

use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// Seasons per cycle: the parameter, else the series' observations per year.
fn period(p: &Params, input: &PixelInput) -> i64 {
    let given = p.int("period");
    (if given != 0 { given } else { input.per_year() }).max(1)
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let n_valid = input.values.iter().filter(|v| v.is_finite()).count();
    if (n_valid as i64) < p.int("min_valid").max(3) {
        return Ok(PixelResult {
            overlays: vec![],
            rows: vec![row("Mann-Kendall", format!("not enough data ({n_valid} valid)"))],
        });
    }
    let method = match p.str("method") {
        "original" => Method::Original,
        "hamed_rao" => Method::HamedRao,
        "yue_wang" => Method::YueWang,
        "seasonal" => Method::Seasonal,
        m => return Err(format!("ValueError: Unknown method {m:?}")),
    };
    let lag = p.int("lag");
    let period = period(p, input);
    let alpha = p.f64("alpha");
    // zeit.mann_kendall: the batch's metrics as float32
    let out = batch(
        &input.values,
        1,
        input.values.len(),
        method,
        alpha,
        if lag < 0 { -1 } else { lag as i32 },
        period as i32,
        p.int("min_valid") as i32,
        1,
    );
    let r: Vec<f64> = out.iter().map(|&v| f64::from(v as f32)).collect();
    let [trend, h, pv, z, tau, s, var_s, slope, intercept] = r[..] else { return Err("Mann-Kendall".into()) };
    let fmt = |x: f64| if x.is_finite() { g(x, 5) } else { "-".into() };
    let unit = if p.str("method") == "seasonal" {
        "/cycle"
    } else if input.per_year() == 1 {
        "/yr"
    } else {
        "/step"
    };
    let rows = vec![
        row(
            "Trend",
            if trend > 0.0 {
                "increasing"
            } else if trend < 0.0 {
                "decreasing"
            } else {
                "no trend"
            },
        ),
        row(format!("Significant (alpha {})", g(alpha, 6)), if h > 0.5 { "yes" } else { "no" }),
        row("p-value", fmt(pv)),
        row("Z", fmt(z)),
        row("Kendall's tau", fmt(tau)),
        row("Sen's slope", fmt(slope) + unit),
        row("S / Var(S)", format!("{} / {}", fmt(s), fmt(var_s))),
    ];
    let mut overlays = vec![];
    if slope.is_finite() && intercept.is_finite() {
        // Sen's line over the series index (per cycle for the seasonal method)
        let years = &input.years;
        let step = if p.str("method") == "seasonal" { 1.0 / period as f64 } else { 1.0 };
        let xs = vec![years[0], years[years.len() - 1]];
        let ys = vec![Some(intercept), Some(intercept + slope * (years.len() - 1) as f64 * step)];
        overlays.push(Overlay::new("line", "Sen's slope", xs, ys));
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: the test on every pixel; Sen's slope also where the trend is significant.
pub fn chunk(p: &Params, chunk: &crate::job::Chunk, ctx: &crate::job::JobCtx) -> Result<crate::job::ChunkOut, String> {
    let method = match p.str("method") {
        "original" => Method::Original,
        "hamed_rao" => Method::HamedRao,
        "yue_wang" => Method::YueWang,
        "seasonal" => Method::Seasonal,
        m => return Err(format!("ValueError: Unknown method {m:?}")),
    };
    let lag = p.int("lag");
    let n = chunk.pixels();
    let out = batch(
        &chunk.by_pixel(&chunk.stack),
        n,
        chunk.t,
        method,
        p.f64("alpha"),
        if lag < 0 { -1 } else { lag as i32 },
        period(p, &ctx.input) as i32,
        p.int("min_valid") as i32,
        ctx.n_jobs,
    );
    // trend, h, p, z, tau, s, var_s, slope, intercept (each [pixels], float32)
    let metric = |k: usize| -> Vec<f64> { out[k * n..(k + 1) * n].iter().map(|&v| f64::from(v as f32)).collect() };
    let (h, slope) = (metric(1), metric(7));
    let mut res = crate::job::ChunkOut::new();
    res.insert("sig_slope".into(), h.iter().zip(&slope).map(|(&h, &s)| if h > 0.5 { s } else { f64::NAN }).collect());
    res.insert("slope".into(), slope);
    res.insert("p".into(), metric(2));
    res.insert("z".into(), metric(3));
    res.insert("tau".into(), metric(4));
    res.insert("trend".into(), metric(0));
    res.insert("intercept".into(), metric(8));
    Ok(res)
}
