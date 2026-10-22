//! The BFAST family (zeit_bridge/tool_bfast.py): classic BFAST, BFAST Lite and BFAST Monitor of zeit-core on the
//! regular time axis Zeit puts the series on. Zeit returns the breaks, not the models: the models drawn are
//! fitted again by least squares, as the bridge does (piecewise linear trend + harmonic season), and match its
//! to rounding (numpy's LAPACK); the breaks, their dates and the rows are the same bits.

use std::f64::consts::PI;

use zeit_core::{bfast as bf, bfast_lite as bfl, bfast_monitor as bfm};

use crate::numeric::{decimal_year, decimal_year_days, linspace, lstsq, regular_time};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::upstream::np_interp;

fn not_enough(name: &str, n_valid: usize, need: i64) -> PixelResult {
    PixelResult { overlays: vec![], rows: vec![row(name, format!("not enough data ({n_valid} valid, need {need})"))] }
}

/// "YYYY-MM" of a decimal year (approximate, for the rows).
pub(crate) fn fmt_year(y: f64) -> String {
    let yr = (y + 1e-9).floor();
    let month = (((y - yr) * 12.0 + 1e-6) as i64 + 1).min(12);
    format!("{}-{month:02}", yr as i64)
}

/// A metric as Zeit's Dataset keeps it (float32), back in float64.
fn f32_of(v: f64) -> f64 {
    f64::from(v as f32)
}

/// The decimal year of each observation of the series as Zeit dates its breaks (zeit/_series_api.py
/// `_with_break_dates`): its dates, or the regular axis to the day without them.
fn obs_dates(input: &PixelInput, start: f64, frequency: i32) -> Vec<f64> {
    let n = input.years.len();
    match &input.days {
        Some(d) => d.iter().map(|&x| decimal_year(x)).collect(),
        None => {
            (0..n).map(|i| decimal_year(decimal_year_days(start + i as f64 / f64::from(frequency)) as i64)).collect()
        }
    }
}

/// The date of an observation index of Zeit's output (float32), NaN when there is none.
fn take(dates: &[f64], idx: f64) -> f64 {
    if idx.is_finite() && idx >= 0.0 && idx < dates.len() as f64 { dates[idx as usize] } else { f64::NAN }
}

/// Date and (signed) magnitude of the largest jump (the first of the largest |magnitude|).
fn largest(mags: &[f64], dates: &[f64]) -> (f64, f64) {
    let score: Vec<f64> =
        mags.iter().zip(dates).map(|(m, d)| if m.is_finite() && d.is_finite() { m.abs() } else { -1.0 }).collect();
    let mut k = 0;
    for (i, &s) in score.iter().enumerate() {
        if s > score[k] {
            k = i;
        }
    }
    if score.get(k).is_some_and(|&s| s >= 0.0) { (dates[k], mags[k]) } else { (f64::NAN, f64::NAN) }
}

/// Date of the first valid observation after index idx (the last one before a break).
fn first_after(vals: &[f64], years: &[f64], idx: f64) -> f64 {
    (idx as i64 + 1..vals.len() as i64)
        .map(|t| t as usize)
        .find(|&t| vals[t].is_finite())
        .map_or(f64::NAN, |t| years[t])
}

/// Sorted (as Python sorts tuples of floats).
fn sort_pairs(v: &mut [(f64, f64)]) {
    v.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
}

// ---------------------------------------------------------------------------------------------------------------
// The models on the chart
// ---------------------------------------------------------------------------------------------------------------

/// Regular time of each observation (the bridge's `_axis`: Janus's first date, one step per observation).
fn axis(input: &PixelInput) -> Vec<f64> {
    let (start, per_year) = (input.years[0], input.per_year() as f64);
    (0..input.years.len()).map(|i| start + i as f64 / per_year).collect()
}

fn harmonics(u: f64, order: usize) -> Vec<f64> {
    (1..=order).flat_map(|k| [(2.0 * PI * k as f64 * u).sin(), (2.0 * PI * k as f64 * u).cos()]).collect()
}

/// The harmonic order of the drawn models: capped at half the observations per year.
fn order(p: &Params, input: &PixelInput) -> usize {
    let half = input.per_year() / 2;
    p.int("order").min(if half == 0 { 1 } else { half }).max(1) as usize
}

/// Segments [a, b) of indices 0..n from the indices where segments start.
fn boundaries(n: usize, starts: &[i64]) -> Vec<(usize, usize)> {
    let mut cuts: Vec<usize> = starts.iter().filter(|&&s| 0 < s && s < n as i64).map(|&s| s as usize).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut edges = vec![0];
    edges.extend(cuts);
    edges.push(n);
    edges.windows(2).map(|w| (w[0], w[1])).collect()
}

/// Index of the first observation on or after decimal year d.
fn index_of_date(years: &[f64], d: f64) -> i64 {
    years.iter().position(|&y| y >= d - 1e-9).unwrap_or(years.len()) as i64
}

/// Fractional indices across [a, b - 1], their regular times and their dates (between observations).
fn dense(a: usize, b: usize, input: &PixelInput) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let s = if b - a < 2 { vec![a as f64] } else { linspace(a as f64, (b - 1) as f64, 600.min((b - 1 - a) * 6 + 1)) };
    let index: Vec<f64> = (0..input.years.len()).map(|i| i as f64).collect();
    let x = s.iter().map(|&si| np_interp(si, &index, &input.years)).collect();
    let (start, per_year) = (input.years[0], input.per_year() as f64);
    let u = s.iter().map(|&si| start + si / per_year).collect();
    (s, u, x)
}

fn line(label: &str, x: Vec<f64>, y: &[f64], styled: bool) -> Overlay {
    let mut o = Overlay::new("line", label, x, y.iter().map(|&v| v.is_finite().then_some(v)).collect());
    if styled {
        o.weight = Some(1.5);
        o.alpha = Some(0.7);
    }
    o
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Trend + harmonic fitted on the valid observations in [a, b) (the trend alone for a short segment); None if
/// too few.
fn segment_model(vals: &[f64], u: &[f64], a: usize, b: usize, order: usize) -> Option<Vec<f64>> {
    let ok: Vec<usize> = (a..b).filter(|&t| vals[t].is_finite()).collect();
    let mut cols = 2 + 2 * order;
    if ok.len() < cols + 1 {
        cols = 2;
        if ok.len() < 3 {
            return None;
        }
    }
    let rows: Vec<Vec<f64>> = ok
        .iter()
        .map(|&t| {
            let mut r = vec![1.0, u[t]];
            r.extend(harmonics(u[t], order));
            r.truncate(cols);
            r
        })
        .collect();
    let y: Vec<f64> = ok.iter().map(|&t| vals[t]).collect();
    Some(lstsq(&rows, &y))
}

/// The model and its trend at times u.
fn eval(beta: &[f64], u: &[f64], order: usize) -> (Vec<f64>, Vec<f64>) {
    u.iter()
        .map(|&t| {
            let mut r = vec![1.0, t];
            r.extend(harmonics(t, order));
            r.truncate(beta.len());
            (dot(&r, beta), dot(&r[..2], &beta[..2]))
        })
        .unzip()
}

/// BFAST: a trend per trend segment and a season per season segment, fitted together; the model and the trend,
/// broken at the trend breaks.
fn bfast_model_overlays(
    vals: &[f64],
    input: &PixelInput,
    order: usize,
    trend_starts: &[i64],
    season_starts: &[i64],
) -> Vec<Overlay> {
    let t = vals.len();
    let u = axis(input);
    let tseg = boundaries(t, trend_starts);
    let sseg = boundaries(t, season_starts);
    let design = |uu: f64, ss: f64| -> Vec<f64> {
        let mut cols = vec![];
        for &(a, b) in &tseg {
            let m = f64::from(u8::from(ss >= a as f64 && ss < b as f64));
            cols.push(m);
            cols.push(m * uu);
        }
        let h = harmonics(uu, order);
        for &(a, b) in &sseg {
            let m = ss >= a as f64 && ss < b as f64;
            cols.extend(h.iter().map(|&x| x * f64::from(u8::from(m))));
        }
        cols
    };
    let nt = 2 * tseg.len();
    let ok: Vec<usize> = (0..t).filter(|&i| vals[i].is_finite()).collect();
    let ncols = nt + 2 * order * sseg.len();
    if ok.len() < ncols + 1 {
        return vec![];
    }
    let rows: Vec<Vec<f64>> = ok.iter().map(|&i| design(u[i], i as f64)).collect();
    let y: Vec<f64> = ok.iter().map(|&i| vals[i]).collect();
    let beta = lstsq(&rows, &y);
    let mut out = vec![];
    for &(a, b) in &tseg {
        let (s, uu, x) = dense(a, b, input);
        let d: Vec<Vec<f64>> = s.iter().zip(&uu).map(|(&si, &ui)| design(ui, si.min(b as f64 - 1e-6))).collect();
        let model: Vec<f64> = d.iter().map(|r| dot(r, &beta)).collect();
        let trend: Vec<f64> = d.iter().map(|r| dot(&r[..nt], &beta[..nt])).collect();
        out.push(line("BFAST model", x.clone(), &model, false));
        out.push(line("BFAST trend", x, &trend, true));
    }
    out
}

fn vlines(label: &str, x: Vec<f64>) -> Overlay {
    Overlay::new("vlines", label, x, vec![])
}

// ---------------------------------------------------------------------------------------------------------------
// BFAST (classic)
// ---------------------------------------------------------------------------------------------------------------

pub fn bfast_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let vals = &input.values;
    let n_valid = vals.iter().filter(|v| v.is_finite()).count();
    let per_year = input.per_year();
    let need = p.int("min_valid").max(2 * per_year + 1);
    if (n_valid as i64) < need || vals.len() as i64 <= 2 * per_year {
        return Ok(not_enough("BFAST", n_valid, need));
    }
    let days = input.days.as_ref().ok_or("ValueError: the time axis has no dates: pass dates=")?;
    let (start, frequency) = regular_time(days)?;
    let (k, ks) = (p.int("max_breaks_trend").max(0) as usize, p.int("max_breaks_season").max(0) as usize);
    let params = bf::Params {
        start_time: start,
        frequency,
        h: p.f64("h"),
        max_breaks_trend: k as i32,
        max_breaks_season: ks as i32,
        max_iter: p.int("max_iter") as i32,
        level: p.f64("level"),
        min_valid: p.int("min_valid") as i32,
    };
    let out: Vec<f64> =
        bf::batch(vals, 1, vals.len(), p.int("order") as i32, &params, 1).into_iter().map(f32_of).collect();
    // n_trend_breaks, n_season_breaks, magnitude, time, n_iter, n_valid, valid, then the indices
    if out[6] != 1.0 {
        return Ok(not_enough("BFAST", n_valid, need));
    }
    let dates = obs_dates(input, start, frequency);
    let sidx = &out[7 + k..7 + k + ks];
    let tmag = &out[7 + k + ks..7 + 2 * k + ks];
    let tdate: Vec<f64> = out[7 + 2 * k + ks..7 + 3 * k + ks].iter().map(|&i| take(&dates, i)).collect();
    let mut trend: Vec<(f64, f64)> =
        tdate.iter().zip(tmag).filter(|(d, _)| d.is_finite()).map(|(&d, &m)| (d, m)).collect();
    sort_pairs(&mut trend);
    let tx: Vec<f64> = trend.iter().map(|t| t.0).collect();
    let mut sx: Vec<f64> = sidx
        .iter()
        .filter(|i| i.is_finite())
        .map(|&i| first_after(vals, &input.years, i))
        .filter(|x| x.is_finite())
        .collect();
    sx.sort_by(f64::total_cmp);
    let mut rows = vec![row("Trend breaks", tx.len().to_string())];
    for &(x, j) in &trend {
        rows.push(row(
            format!("  {}", fmt_year(x)),
            if j.is_finite() { format!("jump {}", g(j, 5)) } else { "-".into() },
        ));
    }
    rows.push(row("Season breaks", sx.len().to_string()));
    let joined = sx.iter().map(|&x| fmt_year(x)).collect::<Vec<_>>().join(", ");
    rows.push(row("  dates", if joined.is_empty() { "-".into() } else { joined }));
    let (date, mag) = largest(tmag, &tdate);
    if date.is_finite() {
        rows.push(row("Largest trend jump", format!("{} at {}", g(mag, 5), fmt_year(date))));
    }
    rows.push(row("Iterations", (out[4] as i64).to_string()));
    let trend_starts: Vec<i64> = tx.iter().map(|&x| index_of_date(&input.years, x)).collect();
    let season_starts: Vec<i64> = sidx.iter().filter(|i| i.is_finite()).map(|&i| i as i64 + 1).collect();
    let mut overlays = bfast_model_overlays(vals, input, order(p, input), &trend_starts, &season_starts);
    if !tx.is_empty() {
        overlays.push(vlines("BFAST trend breaks", tx));
    }
    if !sx.is_empty() {
        overlays.push(vlines("BFAST season breaks", sx));
    }
    Ok(PixelResult { overlays, rows })
}

// ---------------------------------------------------------------------------------------------------------------
// BFAST Lite
// ---------------------------------------------------------------------------------------------------------------

pub fn lite_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let vals = &input.values;
    let n_valid = vals.iter().filter(|v| v.is_finite()).count();
    let need = p.int("min_valid");
    if (n_valid as i64) < need {
        return Ok(not_enough("BFAST Lite", n_valid, need));
    }
    let days = input.days.as_ref().ok_or("ValueError: the time axis has no dates: pass dates=")?;
    let (start, frequency) = regular_time(days)?;
    let k = p.int("max_breaks").max(0) as usize;
    let out: Vec<f64> = bfl::batch(
        vals,
        1,
        vals.len(),
        start,
        frequency,
        p.int("order") as i32,
        p.f64("h"),
        k as i32,
        p.int("min_valid") as i32,
        1,
    )
    .into_iter()
    .map(f32_of)
    .collect();
    // n_breaks, rss, lwz, n_valid, valid, then the indices, magnitudes and first indices after
    if out[4] != 1.0 {
        return Ok(not_enough("BFAST Lite", n_valid, need));
    }
    let dates = obs_dates(input, start, frequency);
    let mags = &out[5 + k..5 + 2 * k];
    let bdates: Vec<f64> = out[5 + 2 * k..5 + 3 * k].iter().map(|&i| take(&dates, i)).collect();
    let mut breaks: Vec<(f64, f64)> =
        bdates.iter().zip(mags).filter(|(d, _)| d.is_finite()).map(|(&d, &m)| (d, m)).collect();
    sort_pairs(&mut breaks);
    let mut rows = vec![row("Breaks", breaks.len().to_string())];
    for &(x, j) in &breaks {
        rows.push(row(
            format!("  {}", fmt_year(x)),
            if j.is_finite() { format!("jump {}", g(j, 5)) } else { "-".into() },
        ));
    }
    rows.push(row("LWZ", g(out[2], 5)));
    rows.push(row("Valid observations", n_valid.to_string()));
    let bx: Vec<f64> = breaks.iter().map(|b| b.0).collect();
    // the model: trend + harmonic, every coefficient breaking at once (bfastlite's)
    let (u, order) = (axis(input), order(p, input));
    let starts: Vec<i64> = bx.iter().map(|&x| index_of_date(&input.years, x)).collect();
    let mut overlays = vec![];
    for (a, b) in boundaries(vals.len(), &starts) {
        if let Some(beta) = segment_model(vals, &u, a, b, order) {
            let (_, uu, x) = dense(a, b, input);
            let (model, trend) = eval(&beta, &uu, order);
            overlays.push(line("BFAST Lite model", x.clone(), &model, false));
            overlays.push(line("BFAST Lite trend", x, &trend, true));
        }
    }
    if !bx.is_empty() {
        overlays.push(vlines("BFAST Lite breaks", bx));
    }
    Ok(PixelResult { overlays, rows })
}

// ---------------------------------------------------------------------------------------------------------------
// BFAST Monitor
// ---------------------------------------------------------------------------------------------------------------

/// First observation of the monitoring period (index into the series).
fn monitor_index(p: &Params, input: &PixelInput) -> usize {
    let years = &input.years;
    let t = years.len();
    let ms = p.f64("monitor_start");
    let i = if ms <= 0.0 {
        (0.75 * t as f64).floor() as usize
    } else {
        years.iter().position(|&y| y >= ms - 1e-9).unwrap_or(t - 1)
    };
    // at least one year (one cycle) to monitor when the series allows it
    i.max(1).min((t as i64 - input.per_year()).max(1) as usize)
}

pub fn monitor_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let vals = &input.values;
    let i = monitor_index(p, input);
    // the history is the observations before i: on a regular axis from the first date, one step per
    // observation, the monitoring starts halfway between observations i-1 and i
    let (start, freq) = (input.years[0], input.per_year());
    let h: f64 = p.str("h").parse().map_err(|_| "ValueError: h")?;
    let period = p.str("period").parse::<f64>().map_err(|_| "ValueError: period")? as i32;
    let alpha: f64 = p.str("alpha").parse().map_err(|_| "ValueError: alpha")?;
    if ![0.25, 0.5, 1.0].contains(&h) {
        return Err(format!("ValueError: h must be one of 0.25, 0.5, 1.0; got {h}"));
    }
    if ![2, 4, 6, 8, 10].contains(&period) {
        return Err(format!("ValueError: period must be one of 2, 4, 6, 8, 10; got {period}"));
    }
    let out: Vec<f64> = bfm::batch(
        vals,
        1,
        vals.len(),
        start,
        start + (i as f64 - 0.5) / freq as f64,
        freq as i32,
        p.int("order") as i32,
        h,
        period,
        alpha,
        p.int("min_valid") as i32,
        1,
    )
    .map_err(|e| format!("ValueError: {e}"))?
    .into_iter()
    .map(f32_of)
    .collect();
    // breakpoint, breakpoint_idx, magnitude, sigma, n_history, has_break, valid
    let start_x = input.years[i];
    let n_hist = vals[..i].iter().filter(|v| v.is_finite()).count();
    let mut overlays = vec![vlines("Monitoring start", vec![start_x])];
    let mut rows = vec![row("Monitoring from", fmt_year(start_x)), row("History obs.", n_hist.to_string())];
    if out[6] != 1.0 {
        rows.push(row(
            "BFAST Monitor",
            format!("not enough history data (need {} valid and more than the model terms)", p.int("min_valid")),
        ));
        return Ok(PixelResult { overlays, rows });
    }
    // the history's model, and where it goes over the monitoring period (what the observations are compared with)
    let (u, order) = (axis(input), order(p, input));
    if let Some(beta) = segment_model(vals, &u, 0, i, order) {
        let (_, uu, x) = dense(0, i, input);
        overlays.push(line("History model", x, &eval(&beta, &uu, order).0, false));
        let (_, uu, x) = dense(i - 1, vals.len(), input);
        overlays.push(line("Model projected", x, &eval(&beta, &uu, order).0, true));
    }
    let dates = obs_dates(input, start, freq as i32);
    let break_date = if out[5] == 1.0 { take(&dates, out[1]) } else { f64::NAN };
    if break_date.is_finite() {
        overlays.push(vlines("BFAST Monitor break", vec![break_date]));
        rows.push(row("Break", fmt_year(break_date)));
    } else {
        rows.push(row("Break", "none"));
    }
    let (magnitude, sigma) = (out[2], out[3]);
    rows.push(row("Magnitude (median residual)", if magnitude.is_finite() { g(magnitude, 5) } else { "-".into() }));
    rows.push(row("History residual sigma", if sigma.is_finite() { g(sigma, 5) } else { "-".into() }));
    Ok(PixelResult { overlays, rows })
}

// ---------------------------------------------------------------------------------------------------------------
// Raster runs
// ---------------------------------------------------------------------------------------------------------------

use crate::job::{Chunk, ChunkOut, JobCtx};

/// Metric `m` of every pixel of a zeit-core batch (metrics x pixels), as Zeit's Dataset keeps it (float32).
fn metric(out: &[f64], m: usize, n: usize) -> Vec<f64> {
    out[m * n..(m + 1) * n].iter().map(|&v| f32_of(v)).collect()
}

fn days_of(ctx: &JobCtx) -> Result<&Vec<i64>, String> {
    ctx.input.days.as_ref().ok_or_else(|| "ValueError: the time axis has no dates: pass dates=".to_string())
}

pub fn bfast_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let (start, frequency) = regular_time(days_of(ctx)?)?;
    let (k, ks) = (p.int("max_breaks_trend").max(0) as usize, p.int("max_breaks_season").max(0) as usize);
    let params = bf::Params {
        start_time: start,
        frequency,
        h: p.f64("h"),
        max_breaks_trend: k as i32,
        max_breaks_season: ks as i32,
        max_iter: p.int("max_iter") as i32,
        level: p.f64("level"),
        min_valid: p.int("min_valid") as i32,
    };
    let n = chunk.pixels();
    let out = bf::batch(&chunk.by_pixel(&chunk.stack), n, chunk.t, p.int("order") as i32, &params, ctx.n_jobs);
    let dates = obs_dates(&ctx.input, start, frequency);
    let valid = metric(&out, 6, n);
    let mags: Vec<Vec<f64>> = (0..k).map(|j| metric(&out, 7 + k + ks + j, n)).collect();
    let bdates: Vec<Vec<f64>> =
        (0..k).map(|j| metric(&out, 7 + 2 * k + ks + j, n).iter().map(|&i| take(&dates, i)).collect()).collect();
    let (n_trend, n_season) = (metric(&out, 0, n), metric(&out, 1, n));
    let mut res = ChunkOut::new();
    let (mut date, mut mag) = (vec![f64::NAN; n], vec![f64::NAN; n]);
    for px in 0..n {
        let m: Vec<f64> = mags.iter().map(|v| v[px]).collect();
        let d: Vec<f64> = bdates.iter().map(|v| v[px]).collect();
        (date[px], mag[px]) = largest(&m, &d);
    }
    let gate = |v: &[f64]| -> Vec<f64> { (0..n).map(|i| if valid[i] == 1.0 { v[i] } else { f64::NAN }).collect() };
    res.insert("n_trend_breaks".into(), gate(&n_trend));
    res.insert("n_season_breaks".into(), gate(&n_season));
    res.insert("break_date".into(), gate(&date));
    res.insert("magnitude".into(), gate(&mag));
    Ok(res)
}

pub fn lite_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let (start, frequency) = regular_time(days_of(ctx)?)?;
    let k = p.int("max_breaks").max(0) as usize;
    let n = chunk.pixels();
    let out = bfl::batch(
        &chunk.by_pixel(&chunk.stack),
        n,
        chunk.t,
        start,
        frequency,
        p.int("order") as i32,
        p.f64("h"),
        k as i32,
        p.int("min_valid") as i32,
        ctx.n_jobs,
    );
    let dates = obs_dates(&ctx.input, start, frequency);
    let valid = metric(&out, 4, n);
    let mags: Vec<Vec<f64>> = (0..k).map(|j| metric(&out, 5 + k + j, n)).collect();
    let bdates: Vec<Vec<f64>> =
        (0..k).map(|j| metric(&out, 5 + 2 * k + j, n).iter().map(|&i| take(&dates, i)).collect()).collect();
    let n_breaks = metric(&out, 0, n);
    let (mut date, mut mag, mut first) = (vec![f64::NAN; n], vec![f64::NAN; n], vec![f64::NAN; n]);
    for px in 0..n {
        let m: Vec<f64> = mags.iter().map(|v| v[px]).collect();
        let d: Vec<f64> = bdates.iter().map(|v| v[px]).collect();
        (date[px], mag[px]) = largest(&m, &d);
        // the first break: the earliest date
        if d.iter().any(|x| x.is_finite()) {
            first[px] = d.iter().copied().filter(|x| x.is_finite()).fold(f64::INFINITY, f64::min);
        }
    }
    let gate = |v: &[f64]| -> Vec<f64> { (0..n).map(|i| if valid[i] == 1.0 { v[i] } else { f64::NAN }).collect() };
    let mut res = ChunkOut::new();
    res.insert("n_breaks".into(), gate(&n_breaks));
    res.insert("break_date".into(), gate(&date));
    res.insert("magnitude".into(), gate(&mag));
    res.insert("first_break".into(), gate(&first));
    Ok(res)
}

pub fn monitor_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let input = &ctx.input;
    let i = monitor_index(p, input);
    let (start, freq) = (input.years[0], input.per_year());
    let h: f64 = p.str("h").parse().map_err(|_| "ValueError: h")?;
    let period = p.str("period").parse::<f64>().map_err(|_| "ValueError: period")? as i32;
    let alpha: f64 = p.str("alpha").parse().map_err(|_| "ValueError: alpha")?;
    let n = chunk.pixels();
    let out = bfm::batch(
        &chunk.by_pixel(&chunk.stack),
        n,
        chunk.t,
        start,
        start + (i as f64 - 0.5) / freq as f64,
        freq as i32,
        p.int("order") as i32,
        h,
        period,
        alpha,
        p.int("min_valid") as i32,
        ctx.n_jobs,
    )
    .map_err(|e| format!("ValueError: {e}"))?;
    let dates = obs_dates(input, start, freq as i32);
    let (idx, magnitude, has, valid) = (metric(&out, 1, n), metric(&out, 2, n), metric(&out, 5, n), metric(&out, 6, n));
    let mut res = ChunkOut::new();
    res.insert(
        "break_date".into(),
        (0..n).map(|k| if valid[k] == 1.0 && has[k] == 1.0 { take(&dates, idx[k]) } else { f64::NAN }).collect(),
    );
    res.insert("magnitude".into(), (0..n).map(|k| if valid[k] == 1.0 { magnitude[k] } else { f64::NAN }).collect());
    res.insert("has_break".into(), (0..n).map(|k| if valid[k] == 1.0 { has[k] } else { f64::NAN }).collect());
    Ok(res)
}
