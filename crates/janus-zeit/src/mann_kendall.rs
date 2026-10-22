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
