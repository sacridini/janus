//! CODED (zeit_bridge/tool_coded.py): Continuous Degradation Detection on the NDFI of every observation. zeit.coded's
//! glue (zeit/_coded.py: the unmixing, the result's layout and the forest rule without training points) is
//! here, its engine is zeit-core's.

use crate::bands::{series_overlay, shown_is_index};
use crate::bfast::fmt_year;
use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::ndfi::{check, f3, unmix, unmix_chunk};
use crate::numeric::{decimal_year, decimal_year_days};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, row};

const STRATA: [&str; 5] = ["Forest", "Non-forest", "Degradation", "Deforestation", "Disturbance"];

fn kind_name(k: u8) -> &'static str {
    match k {
        1 => "degradation",
        2 => "deforestation",
        3 => "disturbance (too few observations after it to tell)",
        _ => "change",
    }
}

/// One change of a pixel, as zeit.coded's Dataset keeps it.
struct Change {
    /// decimal year of its first observation beyond the threshold (through a date: to the day), NaN for none
    t_change: f64,
    ndfi_change: f32,
    post_ndfi: f32,
    /// 1 degradation, 2 deforestation, 3 disturbance, 0 none
    kind: u8,
}

/// zeit.coded of one pixel without training points: (changes, training mean NDFI and RMSE (float32), training
/// observations, forest, stratum).
struct Coded {
    changes: Vec<Change>,
    ndfi_mean: f32,
    rmse: f32,
    n_train: u16,
    forest: bool,
    strata: u8,
    train_start: f64,
}

/// zeit.coded of every pixel of a chunk.
fn run_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<Vec<Coded>, String> {
    let fr = unmix_chunk(p, chunk, ctx)?;
    let days = ctx.input.days.as_ref().ok_or("ValueError: needs real dates")?;
    let t: Vec<f64> = days.iter().map(|&d| decimal_year(d)).collect();
    let start = p.f64("monitor_start");
    let train_years = p.f64("train_years");
    let train_start = if start > 0.0 { start - train_years } else { t[0] };
    let direction = p.str("direction");
    if direction != "loss" && direction != "both" {
        return Err(format!("ValueError: direction must be 'loss' or 'both', got {direction:?}"));
    }
    let max_events = p.int("max_events").max(0) as usize;
    let params = zeit_core::coded::Params {
        train_start,
        train_years,
        consec: p.int("consec") as i32,
        thresh: p.f64("thresh"),
        min_years: p.f64("min_years"),
        min_obs: p.int("min_obs") as i32,
        loss_only: direction == "loss",
        max_events: max_events as i32,
    };
    let n = chunk.pixels();
    let f64s = |v: &[f32]| -> Vec<f64> { chunk.by_pixel(&v.iter().map(|&x| f64::from(x)).collect::<Vec<_>>()) };
    let ndfi = f64s(&fr.ndfi);
    // pixels x features x T
    let feats: Vec<Vec<f64>> = ["gv", "npv", "soil", "shade"].iter().map(|f| f64s(fr.get(f))).collect();
    let mut features = Vec::with_capacity(n * 4 * chunk.t);
    for px in 0..n {
        for f in &feats {
            features.extend_from_slice(&f[px * chunk.t..(px + 1) * chunk.t]);
        }
    }
    let out =
        zeit_core::coded::coded_batch(&t, &ndfi, &features, n, 4, &params, ctx.n_jobs).map_err(|e| e.to_string())?;
    // the layout (zeit/_coded.py `_layout`): the training model, then each change
    let nf = 4;
    let (header, per_event) = (3 + 1 + 1 + 3 * nf + 1, 3 + 3 + 1 + 3 * nf);
    let size = out.len() / n.max(1);
    let forest_ndfi = p.f64("forest_ndfi");
    Ok((0..n)
        .map(|px| {
            let row = &out[px * size..(px + 1) * size];
            let train_c = row[0];
            let changes: Vec<Change> = (0..max_events)
                .map(|e| {
                    let ev = &row[header + e * per_event..header + (e + 1) * per_event];
                    let days = decimal_year_days(ev[0]);
                    let t_change = if days.is_finite() { decimal_year(days as i64) } else { f64::NAN };
                    let (happened, has_post) = (!t_change.is_nan(), !ev[3].is_nan());
                    let kind = if happened && has_post && ev[3] >= forest_ndfi {
                        1
                    } else if happened && has_post {
                        2
                    } else if happened {
                        3
                    } else {
                        0
                    };
                    Change { t_change, ndfi_change: ev[2] as f32, post_ndfi: ev[3] as f32, kind }
                })
                .collect();
            let forest = train_c >= forest_ndfi && !train_c.is_nan();
            let first = changes.first().map_or(0, |c| c.kind);
            let strata = if !train_c.is_nan() || first > 0 {
                if first > 0 {
                    first + 2
                } else if forest {
                    1
                } else {
                    2
                }
            } else {
                0
            };
            let n_train = if row[4].is_nan() { 0 } else { row[4] as u16 };
            Coded { changes, ndfi_mean: train_c as f32, rmse: row[3] as f32, n_train, forest, strata, train_start }
        })
        .collect())
}

fn run(p: &Params, input: &PixelInput) -> Result<Coded, String> {
    Ok(run_chunk(p, &input.as_chunk(), &JobCtx::for_pixel(input))?.remove(0))
}

pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let with_bands = PixelInput { bands: chunk.bands.clone(), ..ctx.input.clone() };
    if let Some(why) = check(&with_bands) {
        return Err(format!("ValueError: CODED: {why}"));
    }
    let all = run_chunk(p, chunk, ctx)?;
    let mut res = ChunkOut::new();
    let mut put = |id: &str, f: &dyn Fn(&Coded) -> f64| {
        res.insert(id.into(), all.iter().map(f).collect());
    };
    fn found(c: &Coded) -> Vec<&Change> {
        c.changes.iter().filter(|ch| ch.t_change.is_finite()).collect()
    }
    put("strata", &|c| if c.strata > 0 { f64::from(c.strata) } else { f64::NAN });
    put("first_change", &|c| found(c).first().map_or(f64::NAN, |ch| ch.t_change));
    put("first_ndfi_change", &|c| found(c).first().map_or(f64::NAN, |ch| f64::from(ch.ndfi_change)));
    put("last_change", &|c| found(c).last().map_or(f64::NAN, |ch| ch.t_change));
    put("n_events", &|c| {
        let k = found(c).len();
        if c.ndfi_mean.is_finite() || k > 0 { k as f64 } else { f64::NAN }
    });
    put("ndfi_mean", &|c| f64::from(c.ndfi_mean));
    Ok(res)
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if let Some(why) = check(input) {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("CODED", why)] });
    }
    let c = run(p, input)?;
    let (mean, rmse) = (f64::from(c.ndfi_mean), f64::from(c.rmse));
    let mut rows = vec![];
    if !mean.is_finite() {
        rows.push(row("Training", "no model (too few clear observations in the training period)"));
    } else {
        let what = if c.forest { "forest" } else { "not forest" };
        rows.push(row("Training", format!("{what}, mean NDFI {}, RMSE {}", f3(mean), f3(rmse))));
        rows.push(row("Training observations", c.n_train.to_string()));
    }
    let stratum = c.strata as usize;
    rows.push(row("Stratum", if (1..=STRATA.len()).contains(&stratum) { STRATA[stratum - 1] } else { "-" }));
    let mut breaks = vec![];
    for ch in &c.changes {
        if !ch.t_change.is_finite() {
            continue;
        }
        breaks.push(ch.t_change);
        let d = f64::from(ch.ndfi_change);
        let mut text = format!(
            "{}, NDFI change {}",
            kind_name(ch.kind),
            if d.is_nan() { "+nan".into() } else { format!("{d:+.3}") }
        );
        if ch.post_ndfi.is_finite() {
            text += &format!(", NDFI after {}", f3(f64::from(ch.post_ndfi)));
        }
        rows.push(row(format!("Change {}", fmt_year(ch.t_change)), text));
    }
    if breaks.is_empty() {
        rows.push(row("Changes", "none"));
    }
    let mut overlays = vec![];
    if shown_is_index(input) {
        // the NDFI (zeit.unmix again, as the bridge does)
        let ndfi: Vec<f64> = unmix(p, input)?.ndfi.iter().map(|&v| f64::from(v)).collect();
        overlays.push(series_overlay("NDFI", input, &ndfi, "markers"));
        if mean.is_finite() {
            let t0 = c.train_start;
            let t1 = t0 + p.f64("train_years");
            overlays.push(Overlay::new("line", "CODED training mean NDFI", vec![t0, t1], vec![Some(mean), Some(mean)]));
        }
    }
    if !breaks.is_empty() {
        overlays.push(Overlay::new("vlines", "CODED changes", breaks, vec![]));
    }
    Ok(PixelResult { overlays, rows })
}
