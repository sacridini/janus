//! EWMACD (zeit.ewmacd): every observation of the series against a harmonic model of its training years, through
//! EWMA control charts (Brooks et al. 2014): when the pixel left its limits (a disturbance or a gain), how far, and
//! when it came back. A tool after the bridge: its reference is zeit-core's ewmacd module, which zeit.ewmacd calls.

use std::f64::consts::PI;

use zeit_core::ewmacd::{self, Ewmacd, Params as EParams};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::numeric::{civil, decimal_year, year_start};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// The calendar year, day of the year and decimal year of each date.
pub(crate) struct Dates {
    pub years: Vec<i32>,
    pub doys: Vec<f64>,
    pub times: Vec<f64>,
}

pub(crate) fn dates(input: &PixelInput) -> Result<Dates, String> {
    let days = input
        .days
        .as_ref()
        .ok_or("ValueError: EWMACD needs the dates of the series (its model is of the day of the year)")?;
    let years: Vec<i32> = days.iter().map(|&d| civil(d).0 as i32).collect();
    let doys = days.iter().zip(&years).map(|(&d, &y)| (d - year_start(i64::from(y)) + 1) as f64).collect();
    let times = days.iter().map(|&d| decimal_year(d)).collect();
    Ok(Dates { years, doys, times })
}

/// The tool's parameters as zeit.ewmacd's; `train_end` 0: three years of training (Brooks' default), at most up to
/// the last year.
pub(crate) fn ewmacd_params(p: &Params, d: &Dates) -> Result<EParams, String> {
    let (first, last) = (d.years[0], d.years[d.years.len() - 1]);
    let te = p.int("train_end") as i32;
    let train_end = if te <= 0 { (first + 3).min(last) } else { te };
    if train_end <= first {
        return Err(format!(
            "ValueError: the training years end before {train_end}, which is not after the first year ({first}): \
             set Training ends before (year) later"
        ));
    }
    let lambda = p.f64("lambda");
    if !(lambda > 0.0 && lambda <= 1.0) {
        return Err(format!("ValueError: lmbda must be in (0, 1], got {lambda}"));
    }
    Ok(EParams {
        train_start: first,
        train_end,
        harmonics: p.int("order").max(1) as usize,
        xbar_train: p.f64("xbar_train"),
        xbar_monitor: p.f64("xbar_monitor"),
        lambda,
        sigmas: p.f64("sigmas"),
        rounding: true,
        persistence: p.int("persistence").max(1) as usize,
        low: f64::NEG_INFINITY,
    })
}

/// The model at decimal year `t`.
fn model(beta: &[f64], t: f64) -> f64 {
    let y = t.floor();
    let days = if crate::numeric::is_leap(y as i64) { 366.0 } else { 365.0 };
    let th = 2.0 * PI * ((t - y) * days + 1.0) / 365.0;
    let k = (beta.len() - 1) / 2;
    beta[0]
        + (0..k)
            .map(|j| beta[1 + j] * ((j + 1) as f64 * th).sin() + beta[1 + k + j] * ((j + 1) as f64 * th).cos())
            .sum::<f64>()
}

const STATES: [&str; 4] = ["stable", "disturbed", "recovered (disturbed, then back)", "gain"];

fn year_text(t: f64) -> String {
    if t.is_finite() {
        let days = crate::numeric::decimal_year_days(t) as i64;
        let (y, m, d) = civil(days);
        format!("{y:04}-{m:02}-{d:02} ({})", g(t, 6))
    } else {
        "none".into()
    }
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let d = dates(input)?;
    let ep = ewmacd_params(p, &d)?;
    let e: Ewmacd = ewmacd::ewmacd(&d.years, &d.doys, &d.times, &input.values, &ep);
    let x = &input.years;
    let mut overlays = vec![];
    let mut rows = vec![row("Training", format!("{}-{}", ep.train_start, ep.train_end - 1))];
    if e.beta.is_empty() || e.flags.iter().all(|f| f.is_nan()) {
        rows.push(row(
            "EWMACD",
            format!(
                "too few valid dates in the training years to fit the model ({} coefficients)",
                ep.n_coefficients()
            ),
        ));
        return Ok(PixelResult { overlays, rows });
    }
    // the model over the whole series, 24 points a year
    let (t0, t1) = (x[0], x[x.len() - 1]);
    let n = (((t1 - t0) * 24.0).ceil() as usize).max(2);
    let xs: Vec<f64> = (0..=n).map(|i| t0 + (t1 - t0) * i as f64 / n as f64).collect();
    let ys = xs.iter().map(|&t| Some(model(&e.beta, t))).collect();
    let mut m = Overlay::new("line", "Harmonic model (training years)", xs, ys);
    m.weight = Some(1.0);
    m.alpha = Some(0.6);
    overlays.push(m);
    // the EWMA of the residuals over the model, and the control limits: out of control below or above them
    let charted: Vec<usize> = (0..x.len()).filter(|&i| e.charted[i]).collect();
    let cx: Vec<f64> = charted.iter().map(|&i| x[i]).collect();
    let mut ew = Overlay::new(
        "line",
        "EWMA (on the model)",
        cx.clone(),
        charted.iter().map(|&i| Some(e.fitted[i] + e.ewma[i])).collect(),
    );
    ew.weight = Some(2.0);
    overlays.push(ew);
    for (label, sign) in [("Lower control limit", -1.0), ("Upper control limit", 1.0)] {
        let mut o = Overlay::new(
            "line",
            label,
            cx.clone(),
            charted.iter().map(|&i| Some(e.fitted[i] + sign * e.limit[i])).collect(),
        );
        o.style = Some("dashed".into());
        o.weight = Some(1.0);
        o.alpha = Some(0.6);
        overlays.push(o);
    }
    // the dates the X-bar charts screened out (clouds, shadows, gross anomalies)
    let screened: Vec<usize> = (0..x.len())
        .filter(|&i| !e.charted[i] && input.values[i].is_finite() && d.years[i] >= ep.train_start)
        .collect();
    if !screened.is_empty() {
        overlays.push(Overlay::new(
            "markers",
            "Screened (X-bar)",
            screened.iter().map(|&i| x[i]).collect(),
            screened.iter().map(|&i| Some(input.values[i])).collect(),
        ));
    }
    let monitor = f64::from(ep.train_end);
    overlays.push(Overlay::new("vlines", "Monitoring start", vec![monitor], vec![]));
    let ev = e.events;
    for (label, t) in [("Disturbance", ev.break_time), ("Recovered", ev.recovery_time), ("Gain", ev.gain_time)] {
        if t.is_finite() {
            overlays.push(Overlay::new("vlines", label, vec![t], vec![]));
        }
    }
    let last = e.flags[e.flags.len() - 1];
    rows.extend([
        row("Model R² / RMSE", format!("{} / {}", g(e.r2, 4), g(e.rmse, 4))),
        row("Residual sigma", g(e.sigma, 5)),
        row(
            "Charted dates",
            format!(
                "{} of {} ({} screened)",
                charted.len(),
                input.values.iter().filter(|v| v.is_finite()).count(),
                screened.len()
            ),
        ),
        row("Disturbance", year_text(ev.break_time)),
        row(
            "Severity",
            if ev.severity.is_finite() { format!("{} x the control limit", g(ev.severity, 4)) } else { "-".into() },
        ),
        row("Recovered", year_text(ev.recovery_time)),
        row("Gain", year_text(ev.gain_time)),
        row("State (last date)", if ev.state.is_finite() { STATES[ev.state as usize].to_string() } else { "-".into() }),
        row("Last flag", g(last, 4)),
    ]);
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: the events of every pixel, its state (1 stable, 2 disturbed, 3 recovered, 4 gain: the
/// classes' order) and its last flag.
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let d = dates(&ctx.input)?;
    let ep = ewmacd_params(p, &d)?;
    let n = chunk.pixels();
    let (flags, maps) = ewmacd::batch(&chunk.by_pixel(&chunk.stack), n, &d.years, &d.doys, &d.times, &ep, ctx.n_jobs);
    let map = |k: usize| maps[k * n..(k + 1) * n].to_vec();
    let f32s = |v: Vec<f64>| v.into_iter().map(|x| f64::from(x as f32)).collect::<Vec<f64>>();
    let mut res = ChunkOut::new();
    res.insert("break_date".into(), map(0));
    res.insert("severity".into(), f32s(map(1)));
    res.insert("recovery_date".into(), map(2));
    res.insert("gain_date".into(), map(3));
    res.insert("state".into(), map(4).into_iter().map(|s| s + 1.0).collect());
    res.insert("last_flag".into(), (0..n).map(|k| flags[k * chunk.t + chunk.t - 1]).collect());
    res.insert("sigma".into(), f32s(map(5)));
    Ok(res)
}
