//! S-CCD (zeit.sccd): CCDC whose harmonic models, once started, are carried forward by a Kalman filter (Ye et al.
//! 2021) instead of refitted, on the mapped surface reflectance (x 10000) with the quality band's Fmask codes: the
//! segments and their breaks, and the near-real-time state at the end of the series (the latest observations
//! that depart from the model, a change not yet confirmed). A tool after the bridge: its reference is zeit-core's
//! sccd module, which zeit.sccd calls.

use std::collections::BTreeMap;

use zeit_core::sccd::{self, Params as SParams, status};

use crate::bands::{LANDSAT_ROLES, fmask_codes, from_x10000, reflectance_units, to_x10000};
use crate::ccdc::{EPOCH_ORDINAL, UNIT_LABELS, UNITS, fmt, missing, ordinal_decimal_year, shown_value};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// Green..SWIR2: the detection bands of zeit.sccd with six bands.
const DETECT: [usize; 5] = [1, 2, 3, 4, 5];
const MODES: [&str; 3] = ["No model", "Monitoring", "Waiting for observations"];

/// The tool's parameters as zeit.sccd's.
fn sccd_params(p: &Params) -> SParams {
    SParams {
        conse: p.int("conse") as i32,
        prob_threshold: p.f64("prob_threshold"),
        outlier_prob: p.f64("outlier_prob"),
        max_angle: p.f64("max_angle"),
        ..Default::default()
    }
}

/// The six bands of a chunk as reflectance x 10000, each [T][pixels], the Fmask codes and the unit mode.
fn prepare(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> (Vec<Vec<f64>>, Vec<i32>, String) {
    let optical: Vec<&[f64]> = LANDSAT_ROLES.iter().map(|r| chunk.bands[*r].as_slice()).collect();
    let mode = ctx.once("units", || reflectance_units(p, &optical));
    let vals = optical.iter().map(|b| b.iter().map(|&v| to_x10000(v, &mode)).collect()).collect();
    let six: BTreeMap<String, Vec<f64>> =
        LANDSAT_ROLES.iter().map(|r| (r.to_string(), chunk.bands[*r].clone())).collect();
    let qa = fmask_codes(&six, chunk.qa.as_deref(), ctx.input.qa_rule.as_deref()).unwrap_or_default();
    (vals, qa, mode)
}

fn ordinals(ctx: &JobCtx) -> Result<Vec<i32>, String> {
    Ok(ctx.input.days.as_ref().ok_or("needs real dates")?.iter().map(|&d| (d + EPOCH_ORDINAL) as i32).collect())
}

/// The change of a break over the detection bands: the norm of its bands' median departures (refl x 10000).
fn norm(magnitude: &[f64]) -> f64 {
    DETECT.iter().map(|&b| magnitude[b].powi(2)).sum::<f64>().sqrt()
}

/// The alert of a pixel at its last date: 1 stable (monitored, no anomaly), 2 a change in progress (monitored, the
/// latest observations anomalous), 3 waiting for enough observations for a model after a change, 4 no model.
fn state(mode: i32, anomalies: f64, segments: usize) -> f64 {
    match mode {
        1 if anomalies > 0.0 => 2.0,
        1 => 1.0,
        2 if segments > 0 => 3.0,
        _ => 4.0,
    }
}

fn decimal(ordinal: i32) -> f64 {
    if ordinal > 0 { ordinal_decimal_year(f64::from(ordinal)) } else { f64::NAN }
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let fail = |why: String| Ok(PixelResult { overlays: vec![], rows: vec![row("S-CCD", why)] });
    let miss = missing(input);
    if !miss.is_empty() {
        return fail(format!("map the bands first: {}", miss.join(", ")));
    }
    if input.days.is_none() {
        return fail("needs real dates".into());
    }
    let ctx = JobCtx::for_pixel(input);
    let (vals, qa, mode) = prepare(p, &input.as_chunk(), &ctx);
    let t = qa.len();
    let n_clear = qa.iter().filter(|&&q| q < 2).count();
    let r = sccd::sccd(&ordinals(&ctx)?, &vals, &qa, &sccd_params(p)).map_err(|e| format!("ValueError: {e}"))?;
    let (nb, conse) = (vals.len(), p.int("conse"));
    let mon = &r.monitor;
    let mut rows = vec![
        row("Segments", r.segments.len().to_string()),
        row("Observations", format!("{} ({n_clear} clear)", qa.iter().filter(|&&q| q != 255).count())),
        row("Units", UNIT_LABELS[UNITS.iter().position(|u| *u == mode).unwrap_or(1)]),
    ];
    let mut breaks = vec![];
    for (k, s) in r.segments.iter().enumerate() {
        rows.push(row(
            format!("Segment {}", k + 1),
            format!("{} to {}, {} obs", fmt(i64::from(s.t_start)), fmt(i64::from(s.t_end)), s.num_obs),
        ));
        if s.t_break > 0 {
            rows.push(row(
                format!("  break {}", fmt(i64::from(s.t_break))),
                format!("magnitude {} (refl x 10000)", g(norm(&s.magnitude), 4)),
            ));
            breaks.push(decimal(s.t_break));
        }
    }
    rows.push(row("Monitoring", MODES[mon.mode.clamp(0, 2) as usize]));
    if mon.mode == 1 {
        rows.push(row(
            "Latest anomalies",
            if mon.n_anomalies > 0 {
                format!("{} of {conse}, since {}", mon.n_anomalies, fmt(i64::from(mon.anomaly_start)))
            } else {
                format!("0 of {conse}")
            },
        ));
        rows.push(row("Last statistic", format!("{} (change above {})", g(mon.last_statistic, 4), g(r.threshold, 4))));
    }
    // each band's filtered model (trend + annual + semiannual) where an observation entered it, in the shown units
    let fitted = |i: usize| -> Option<f64> {
        let by_role: Vec<(&str, f64)> = LANDSAT_ROLES
            .iter()
            .enumerate()
            .map(|(b, &role)| {
                let k = i * nb + b;
                (role, from_x10000(r.trace.trend[k] + r.trace.annual[k] + r.trace.semiannual[k], &mode))
            })
            .collect();
        shown_value(&by_role, input).filter(|v| v.is_finite())
    };
    let mut order: Vec<usize> = (0..t).collect();
    order.sort_by(|&a, &b| input.years[a].total_cmp(&input.years[b]));
    let mut overlays = vec![];
    for (k, s) in r.segments.iter().enumerate() {
        let (mut x, mut y) = (vec![], vec![]);
        let day = |i: usize| input.days.as_ref().map_or(0, |d| d[i] + EPOCH_ORDINAL);
        for &i in &order {
            let d = day(i);
            if r.trace.status[i] == status::MODEL && d >= i64::from(s.t_start) && d <= i64::from(s.t_end) {
                x.push(input.years[i]);
                y.push(fitted(i));
            }
        }
        if !x.is_empty() {
            overlays.push(Overlay::new("line", &format!("S-CCD segment {}", k + 1), x, y));
        }
    }
    let marks = |want: &dyn Fn(usize) -> bool| -> (Vec<f64>, Vec<Option<f64>>) {
        order
            .iter()
            .filter(|&&i| want(i))
            .map(|&i| (input.years[i], input.values.get(i).copied().filter(|v| v.is_finite())))
            .unzip()
    };
    let (x, y) = marks(&|i| r.trace.status[i] == status::OUTLIER);
    if !x.is_empty() {
        overlays.push(Overlay::new("markers", "S-CCD outliers", x, y));
    }
    // the latest observations beyond the change threshold, not yet a change
    let (x, y) = marks(&|i| r.trace.status[i] == status::PEEK && r.trace.statistic[i] > r.threshold);
    if !x.is_empty() {
        overlays.push(Overlay::new("markers", "S-CCD anomalies (not yet a change)", x, y));
    }
    if !breaks.is_empty() {
        overlays.push(Overlay::new("vlines", "S-CCD breaks", breaks, vec![]));
    }
    Ok(PixelResult { overlays, rows })
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    if LANDSAT_ROLES.iter().any(|r| !chunk.bands.contains_key(*r)) || ctx.input.days.is_none() {
        return Err(format!("ValueError: S-CCD needs the bands {} mapped and real dates", LANDSAT_ROLES.join(", ")));
    }
    let (vals, qa, _) = prepare(p, chunk, ctx);
    let (t, n, nb) = (chunk.t, chunk.pixels(), vals.len());
    // pixels x bands x T, and pixels x T
    let mut values = Vec::with_capacity(n * nb * t);
    let mut q = Vec::with_capacity(n * t);
    for px in 0..n {
        for b in &vals {
            values.extend((0..t).map(|i| b[i * n + px]));
        }
        q.extend((0..t).map(|i| qa[i * n + px]));
    }
    let s_max = p.int("max_segments").max(1) as usize;
    let out = sccd::sccd_batch(&values, &q, &ordinals(ctx)?, n, nb, &sccd_params(p), s_max, ctx.n_jobs)
        .map_err(|e| format!("ValueError: {e}"))?;
    let (pps, width) = (sccd::segment_width(nb), s_max * sccd::segment_width(nb) + 1 + sccd::monitor_width(nb));
    let names = [
        "n_breaks",
        "first_break",
        "last_break",
        "largest_break",
        "magnitude",
        "state",
        "n_anomalies",
        "anomaly_start",
    ];
    let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(n); names.len()];
    for px in 0..n {
        let r = &out[px * width..(px + 1) * width];
        let count = r[s_max * pps] as usize;
        let mon = &r[s_max * pps + 1..];
        // each break: its date and change over the detection bands
        let brk: Vec<(f64, f64)> = (0..count)
            .map(|k| &r[k * pps..(k + 1) * pps])
            .filter(|s| s[2] > 0.0)
            .map(|s| {
                let mag: Vec<f64> = (0..nb).map(|b| s[5 + b * 8 + 1]).collect();
                (decimal(s[2] as i32), norm(&mag))
            })
            .collect();
        let largest = brk.iter().copied().reduce(|a, b| if b.1 > a.1 { b } else { a });
        let fitted = count > 0;
        let values = [
            if fitted { brk.len() as f64 } else { f64::NAN },
            brk.first().map_or(f64::NAN, |b| b.0),
            brk.last().map_or(f64::NAN, |b| b.0),
            largest.map_or(f64::NAN, |b| b.0),
            largest.map_or(f64::NAN, |b| b.1),
            state(mon[0] as i32, mon[1], count),
            if mon[0] == 1.0 { mon[1] } else { f64::NAN },
            if mon[0] == 1.0 && mon[1] > 0.0 { decimal(mon[2] as i32) } else { f64::NAN },
        ];
        for (c, v) in cols.iter_mut().zip(values) {
            c.push(v);
        }
    }
    Ok(names.iter().map(|n| n.to_string()).zip(cols).collect())
}
