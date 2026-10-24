//! CCDC (zeit_bridge/tool_ccdc.py): zeit-core's port of the original MATLAB CCDC on the mapped surface reflectance
//! (x 10000, brightness temperature in deg C x 100) with the quality band's Fmask codes; the segments' models
//! drawn in the shown series' units, the breaks' magnitudes over the detection bands; and the synthetic image of a
//! date (ccdc_synthetic). zeit.ccdc's glue (zeit/_ccdc_api.py: the dates, the result's layout) is here.

use std::f64::consts::PI;

use crate::bands::{LANDSAT_ROLES, fmask_codes, from_x10000, reflectance_units, to_x10000};
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::numeric::{civil, decimal_year, median, year_start};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// Python's date.toordinal() of 1970-01-01.
pub(crate) const EPOCH_ORDINAL: i64 = 719163;
/// Green..SWIR2: Zeit's detection bands with 6 bands or more.
const DETECT: [usize; 5] = [1, 2, 3, 4, 5];
pub(crate) const UNITS: [&str; 4] = ["auto", "x10000", "unit", "landsat_c2"];
pub(crate) const UNIT_LABELS: [&str; 4] =
    ["Automatic (from the values)", "Reflectance x 10000", "Reflectance 0-1", "Landsat C2 L2 digital numbers"];

fn median_finite(v: &[f64]) -> f64 {
    let f: Vec<f64> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() { 0.0 } else { median(&f) }
}

/// Brightness temperature -> deg C x 100 (Kelvin, deg C, C2 ST digital numbers or already x 100).
fn thermal_to_c100(v: &[f64], mode: &str) -> Vec<f64> {
    let mut med = median_finite(v);
    let mut v = v.to_vec();
    if mode == "landsat_c2" && med > 1000.0 {
        v.iter_mut().for_each(|x| *x = *x * 0.00341802 + 149.0);
        med = med * 0.00341802 + 149.0;
    }
    if 150.0 < med && med < 400.0 {
        v.iter().map(|x| (x - 273.15) * 100.0).collect()
    } else if -100.0 < med && med <= 150.0 {
        v.iter().map(|x| x * 100.0).collect()
    } else {
        v
    }
}

/// The inverse of `thermal_to_c100`, the units recovered from the shown thermal values `reference`.
fn thermal_from_c100(v: f64, reference: &[f64], mode: &str) -> f64 {
    let med = median_finite(reference);
    if mode == "landsat_c2" && med > 1000.0 {
        (v / 100.0 + 273.15 - 149.0) / 0.00341802
    } else if 150.0 < med && med < 400.0 {
        v / 100.0 + 273.15
    } else if -100.0 < med && med <= 150.0 {
        v / 100.0
    } else {
        v
    }
}

/// The harmonic terms on the time axis of Zeit's coefficients (ordinal day + 366).
fn terms(ordinal: f64) -> [f64; 8] {
    let t = ordinal + 366.0;
    let w = 2.0 * PI / 365.25;
    [
        1.0,
        t,
        (w * t).cos(),
        (w * t).sin(),
        (2.0 * w * t).cos(),
        (2.0 * w * t).sin(),
        (3.0 * w * t).cos(),
        (3.0 * w * t).sin(),
    ]
}

fn model_at(coefs: &[f64], ordinal: f64) -> f64 {
    terms(ordinal).iter().zip(coefs).map(|(a, b)| a * b).sum()
}

/// "YYYY-MM-DD" of a Python ordinal day; "-" for none.
pub(crate) fn fmt(ordinal: i64) -> String {
    if ordinal <= 0 {
        return "-".into();
    }
    let (y, m, d) = civil(ordinal - EPOCH_ORDINAL);
    format!("{y:04}-{m:02}-{d:02}")
}

/// One pixel's segments as zeit.ccdc's Dataset keeps them: dates as ordinal days (0 none), the models'
/// coefficients (float32) per band.
struct Segments {
    count: usize,
    start: Vec<i64>,
    end: Vec<i64>,
    brk: Vec<i64>,
    /// [segment][band][8]
    coefs: Vec<Vec<[f64; 8]>>,
}

/// What CCDC gets of a chunk: the bands in Zeit's order (x 10000; the thermal band in deg C x 100), each
/// [T][pixels]; the Fmask codes, a date where a band has no value unusable (4, as a cloud); the roles, the unit
/// mode and the chunk's thermal values as they came (their units).
struct Prepared {
    vals: Vec<Vec<f64>>,
    qa: Vec<i32>,
    roles: Vec<&'static str>,
    mode: String,
    thermal: Vec<f64>,
}

fn prepare(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Prepared {
    let mut roles: Vec<&'static str> = LANDSAT_ROLES.to_vec();
    let optical: Vec<&[f64]> = LANDSAT_ROLES.iter().map(|r| chunk.bands[*r].as_slice()).collect();
    let mode = ctx.once("units", || reflectance_units(p, &optical));
    let mut vals: Vec<Vec<f64>> = optical.iter().map(|b| b.iter().map(|&v| to_x10000(v, &mode)).collect()).collect();
    let thermal = chunk.bands.get("thermal").cloned().unwrap_or_default();
    if !thermal.is_empty() {
        roles.push("thermal");
        vals.push(thermal_to_c100(&thermal, &mode));
    }
    let mut qa = fmask_codes(&chunk.bands, chunk.qa.as_deref(), ctx.input.qa_rule.as_deref()).unwrap_or_default();
    for (i, q) in qa.iter_mut().enumerate() {
        if !vals.iter().all(|b| b[i].is_finite()) {
            *q = 4;
        }
    }
    Prepared { vals, qa, roles, mode, thermal }
}

/// zeit.ccdc of every pixel of a prepared chunk.
fn segments(p: &Params, ctx: &JobCtx, pr: &Prepared, t: usize, n: usize) -> Result<Vec<Segments>, String> {
    let s_max = p.int("max_segments").max(0) as usize;
    let nb = pr.vals.len();
    // pixels x bands x T; a date without a value is 0, out of the valid range: kept as one that cannot be used
    let mut values = Vec::with_capacity(n * nb * t);
    let mut qa = Vec::with_capacity(n * t);
    for px in 0..n {
        for b in &pr.vals {
            values.extend((0..t).map(|i| b[i * n + px]).map(|v| if v.is_finite() { v } else { 0.0 }));
        }
        qa.extend((0..t).map(|i| pr.qa[i * n + px]));
    }
    let dates: Vec<i32> =
        ctx.input.days.as_ref().ok_or("needs real dates")?.iter().map(|&d| (d + EPOCH_ORDINAL) as i32).collect();
    let num_c: i32 = p.str("num_c").parse().map_err(|_| "ValueError: num_c")?;
    let params = zeit_core::ccdc::Params {
        conseq_anom: p.int("conseq_anom") as i32,
        chi2_prob_threshold: p.f64("chi2_prob_threshold"),
        tmax_cg_prob_threshold: p.f64("tmax_cg_prob_threshold"),
        num_c,
        thermal_band: pr.roles.iter().position(|r| *r == "thermal").map_or(-1, |i| i as i32),
        ..Default::default()
    };
    let (segs, counts) = zeit_core::ccdc::fit_ccdc_batch(&values, &qa, &dates, n, nb, &params, s_max, true, ctx.n_jobs)
        .map_err(|e| format!("ValueError: {e}"))?;
    let pps = 3 + nb * 9;
    Ok((0..n)
        .map(|px| {
            let count = counts[px].max(0) as usize;
            let date = |v: f64, k: usize| if k < count && v > 0.0 { v as i64 } else { 0 };
            let mut out = Segments { count, start: vec![], end: vec![], brk: vec![], coefs: vec![] };
            for k in 0..s_max {
                let seg = &segs[(px * s_max + k) * pps..(px * s_max + k + 1) * pps];
                out.start.push(date(seg[0], k));
                out.end.push(date(seg[1], k));
                out.brk.push(date(seg[2], k));
                out.coefs.push(
                    (0..nb)
                        .map(|b| {
                            let mut c = [f64::NAN; 8];
                            for (i, ci) in c.iter_mut().enumerate() {
                                if k < count {
                                    *ci = f64::from(seg[3 + b * 9 + 1 + i] as f32);
                                }
                            }
                            c
                        })
                        .collect(),
                );
            }
            out
        })
        .collect())
}

/// Per break: the jump between the two segments' models at it over the detection bands, and the NDVI change.
fn jumps(sg: &Segments) -> (Vec<f64>, Vec<f64>) {
    let s_max = sg.brk.len();
    let (mut mag, mut dndvi) = (vec![f64::NAN; s_max], vec![f64::NAN; s_max]);
    for s in 0..s_max.saturating_sub(1) {
        if !(sg.brk[s] > 0 && s + 1 < sg.count) {
            continue;
        }
        let at = sg.brk[s] as f64;
        let before: Vec<f64> = sg.coefs[s].iter().map(|c| model_at(c, at)).collect();
        let after: Vec<f64> = sg.coefs[s + 1].iter().map(|c| model_at(c, at)).collect();
        mag[s] = DETECT.iter().map(|&b| (after[b] - before[b]).powi(2)).sum::<f64>().sqrt();
        let nd0 = (before[3] - before[2]) / (before[3] + before[2]);
        let nd1 = (after[3] - after[2]) / (after[3] + after[2]);
        dndvi[s] = nd1 - nd0;
    }
    (mag, dndvi)
}

/// The model in the units of the shown series (one band or a normalized difference).
fn shown_curve(coefs: &[[f64; 8]], pr: &Prepared, input: &PixelInput, t: &[f64]) -> Option<Vec<f64>> {
    let band = |role: &str| -> Option<Vec<f64>> {
        let b = pr.roles.iter().position(|r| *r == role)?;
        Some(
            t.iter()
                .map(|&x| {
                    let v = model_at(&coefs[b], x);
                    if role == "thermal" {
                        thermal_from_c100(v, &pr.thermal, &pr.mode)
                    } else {
                        from_x10000(v, &pr.mode)
                    }
                })
                .collect(),
        )
    };
    let shown = input.shown.as_ref()?;
    if let Some(nd) = shown.nd.as_ref().filter(|nd| nd.len() == 2) {
        let (a, b) = (band(&nd[0])?, band(&nd[1])?);
        return Some(a.iter().zip(&b).map(|(a, b)| if a + b != 0.0 { (a - b) / (a + b) } else { f64::NAN }).collect());
    }
    shown.band.as_deref().and_then(band)
}

pub(crate) fn ordinal_decimal_year(o: f64) -> f64 {
    if o.is_finite() && o > 0.0 { decimal_year(o as i64 - EPOCH_ORDINAL) } else { f64::NAN }
}

pub(crate) fn missing(input: &PixelInput) -> Vec<&'static str> {
    LANDSAT_ROLES.iter().copied().filter(|r| !input.bands.contains_key(*r)).collect()
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let fail = |why: String| Ok(PixelResult { overlays: vec![], rows: vec![row("CCDC", why)] });
    let miss = missing(input);
    if !miss.is_empty() {
        return fail(format!("map the bands first: {}", miss.join(", ")));
    }
    let Some(days) = &input.days else { return fail("needs real dates".into()) };
    let ctx = JobCtx::for_pixel(input);
    let pr = prepare(p, &input.as_chunk(), &ctx);
    let t = pr.qa.len();
    let n_obs = (0..t).filter(|&i| pr.vals.iter().all(|b| b[i].is_finite())).count();
    let n_clear = pr.qa.iter().filter(|&&q| q < 2).count();
    if n_clear < 12 {
        return fail(format!("not enough clear observations ({n_clear}, need 12)"));
    }
    let sg = segments(p, &ctx, &pr, t, 1)?.remove(0);
    let (mag, dndvi) = jumps(&sg);
    let n = sg.count.min(sg.start.len());
    let ordinals: Vec<i64> = days.iter().map(|&d| d + EPOCH_ORDINAL).collect();
    let mut rows = vec![
        row("Segments", sg.count.to_string()),
        row("Observations", format!("{n_obs} ({n_clear} clear)")),
        row("Units", UNIT_LABELS[UNITS.iter().position(|u| *u == pr.mode).unwrap_or(1)]),
    ];
    let (mut overlays, mut breaks) = (vec![], vec![]);
    for k in 0..n {
        let (t0, t1, tb) = (sg.start[k], sg.end[k], sg.brk[k]);
        let clear = (0..t).filter(|&i| ordinals[i] >= t0 && ordinals[i] <= t1 && pr.qa[i] < 2).count();
        rows.push(row(format!("Segment {}", k + 1), format!("{} to {}, {clear} clear obs", fmt(t0), fmt(t1))));
        if tb > 0 {
            let (m, d) = (mag[k], dndvi[k]);
            rows.push(row(
                format!("  break {}", fmt(tb)),
                if m.is_finite() {
                    format!(
                        "magnitude {} (refl x 10000), NDVI change {}",
                        g(m, 4),
                        if d.is_nan() { "+nan".into() } else { format!("{d:+.3}") }
                    )
                } else {
                    "end of the series' model".into()
                },
            ));
            breaks.push(ordinal_decimal_year(tb as f64));
        }
        if t1 > t0 {
            let num = ((t1 - t0) / 8 + 1).clamp(2, 400) as usize;
            let ts = crate::numeric::linspace(t0 as f64, t1 as f64, num);
            if let Some(y) = shown_curve(&sg.coefs[k], &pr, input, &ts) {
                overlays.push(Overlay::new(
                    "line",
                    &format!("CCDC segment {}", k + 1),
                    ts.iter().map(|&o| ordinal_decimal_year(o)).collect(),
                    y.iter().map(|&v| v.is_finite().then_some(v)).collect(),
                ));
            }
        }
    }
    if !breaks.is_empty() {
        overlays.push(Overlay::new("vlines", "CCDC breaks", breaks, vec![]));
    }
    Ok(PixelResult { overlays, rows })
}

/// The date to predict as an ordinal day (the parameter, else the map's date, else the last date), and its
/// decimal year.
fn target_ordinal(p: &Params, input: &PixelInput) -> (i64, f64) {
    let mut y = p.f64("date");
    if y == 0.0 {
        y = input.map_year.unwrap_or(0.0);
    }
    if y == 0.0 {
        y = input.years[input.years.len() - 1];
    }
    let year = y.floor() as i64;
    let first = year_start(year);
    let days = year_start(year + 1) - first;
    (first + EPOCH_ORDINAL + ((y - year as f64) * days as f64).round_ties_even() as i64, y)
}

pub fn synthetic_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let mut res = pixel(p, input)?;
    if !missing(input).is_empty() || input.days.is_none() || !res.rows.iter().any(|r| r[0] == "Segments") {
        return Ok(res);
    }
    let ctx = JobCtx::for_pixel(input);
    let pr = prepare(p, &input.as_chunk(), &ctx);
    let sg = segments(p, &ctx, &pr, pr.qa.len(), 1)?.remove(0);
    let (target, year) = target_ordinal(p, input);
    let k = segment_at(&sg, target);
    let v: Vec<f64> =
        sg.coefs[k].iter().map(|c| if sg.count == 0 { f64::NAN } else { model_at(c, target as f64) }).collect();
    let by_role: Vec<(&str, f64)> = pr
        .roles
        .iter()
        .zip(&v)
        .map(|(&r, &x)| {
            (r, if r == "thermal" { thermal_from_c100(x, &pr.thermal, &pr.mode) } else { from_x10000(x, &pr.mode) })
        })
        .collect();
    let mut rows = vec![row("Synthetic image of", format!("{} (segment {})", fmt(target), k + 1))];
    rows.extend(by_role.iter().map(|(r, x)| row(format!("  {r}"), g(*x, 5))));
    let shown = shown_value(&by_role, input);
    let mut overlays = vec![Overlay::new("vlines", "Synthetic date", vec![year], vec![])];
    if let Some(s) = shown.filter(|s| s.is_finite()) {
        rows.push(row("  shown series", g(s, 5)));
        overlays.push(Overlay::new("markers", "Synthetic value", vec![year], vec![Some(s)]));
    }
    rows.append(&mut res.rows);
    res.rows = rows;
    res.overlays.extend(overlays);
    Ok(res)
}

/// The shown series' value of the synthetic bands (its band, or the normalized difference of two).
pub(crate) fn shown_value(by_role: &[(&str, f64)], input: &PixelInput) -> Option<f64> {
    let value = |role: &str| by_role.iter().find(|(r, _)| *r == role).map(|x| x.1);
    let s = input.shown.as_ref()?;
    if let Some(nd) = s.nd.as_ref().filter(|nd| nd.len() == 2) {
        let (a, b) = (value(&nd[0])?, value(&nd[1])?);
        return Some(if a + b != 0.0 { (a - b) / (a + b) } else { f64::NAN });
    }
    value(s.band.as_deref()?)
}

/// The segment of a pixel covering an ordinal date: before the first, the first; after the last, the last.
fn segment_at(sg: &Segments, target: i64) -> usize {
    let used = |k: usize| k < sg.count;
    let s_max = sg.start.len();
    let covers = (0..s_max).find(|&k| used(k) && sg.start[k] <= target && sg.end[k] >= target);
    let started = (0..s_max).rev().find(|&k| used(k) && sg.start[k] <= target);
    covers.or(started).unwrap_or(0)
}

fn check_chunk(chunk: &Chunk, ctx: &JobCtx) -> Result<(), String> {
    if LANDSAT_ROLES.iter().any(|r| !chunk.bands.contains_key(*r)) || ctx.input.days.is_none() {
        return Err(format!("ValueError: CCDC needs the bands {} mapped and real dates", LANDSAT_ROLES.join(", ")));
    }
    Ok(())
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    check_chunk(chunk, ctx)?;
    let pr = prepare(p, chunk, ctx);
    let all = segments(p, ctx, &pr, chunk.t, chunk.pixels())?;
    let names = ["n_breaks", "largest_break", "magnitude", "ndvi_change", "first_break", "last_break", "n_segments"];
    let mut cols: Vec<Vec<f64>> = vec![Vec::with_capacity(all.len()); names.len()];
    for sg in &all {
        let (mag, dndvi) = jumps(sg);
        let fitted = sg.count > 0;
        let has: Vec<bool> = sg.brk.iter().map(|&b| b > 0).collect();
        let years: Vec<f64> =
            sg.brk.iter().map(|&b| if b > 0 { ordinal_decimal_year(b as f64) } else { f64::NAN }).collect();
        let mut k = 0;
        let score: Vec<f64> = mag.iter().map(|&m| if m.is_finite() { m } else { -1.0 }).collect();
        for (i, &s) in score.iter().enumerate() {
            if s > score[k] {
                k = i;
            }
        }
        let big = score.get(k).is_some_and(|&s| s >= 0.0);
        let first = has.iter().position(|&h| h).map_or(f64::NAN, |i| years[i]);
        let last = has.iter().rposition(|&h| h).map_or(f64::NAN, |i| years[i]);
        let values = [
            if fitted { has.iter().filter(|&&h| h).count() as f64 } else { f64::NAN },
            if big { years[k] } else { f64::NAN },
            if big { mag[k] } else { f64::NAN },
            if big { dndvi[k] } else { f64::NAN },
            first,
            last,
            if fitted { sg.count as f64 } else { f64::NAN },
        ];
        for (c, v) in cols.iter_mut().zip(values) {
            c.push(v);
        }
    }
    Ok(names.iter().map(|n| n.to_string()).zip(cols).collect())
}

pub fn synthetic_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    check_chunk(chunk, ctx)?;
    let pr = prepare(p, chunk, ctx);
    let all = segments(p, ctx, &pr, chunk.t, chunk.pixels())?;
    let (target, _) = target_ordinal(p, &ctx.input);
    let n = all.len();
    let mut res: ChunkOut =
        pr.roles.iter().filter(|r| **r != "thermal").map(|r| (r.to_string(), Vec::with_capacity(n))).collect();
    let mut shown = Vec::with_capacity(n);
    for sg in &all {
        let k = segment_at(sg, target);
        let by_role: Vec<(&str, f64)> = pr
            .roles
            .iter()
            .zip(&sg.coefs[k])
            .map(|(&r, c)| {
                let x = if sg.count == 0 { f64::NAN } else { model_at(c, target as f64) };
                (r, if r == "thermal" { thermal_from_c100(x, &pr.thermal, &pr.mode) } else { from_x10000(x, &pr.mode) })
            })
            .collect();
        for (r, x) in &by_role {
            if let Some(v) = res.get_mut(*r) {
                v.push(*x);
            }
        }
        shown.push(shown_value(&by_role, &ctx.input).unwrap_or(f64::NAN));
    }
    res.insert("shown".into(), shown);
    Ok(res)
}
