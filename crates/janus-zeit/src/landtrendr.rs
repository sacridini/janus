//! LandTrendr (zeit_bridge/tool_landtrendr.py): zeit.landtrendr's segmentation of the annual series, its vertices
//! drawn, and zeit.extract_events' event of the pixel listed; a raster run maps every pixel's event.

use zeit_core::landtrendr::{Params as LtParams, fit_trajectory_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::upstream::{Event, SortBy, landtrendr_event};

/// zeit.landtrendr on one pixel (zeit/_lt.py `_segment_block`): the vertices' years and values as its Dataset
/// keeps them (int16 and float32, 0 and NaN past the last), their count and the fit's RMSE (float32).
struct Segmentation {
    vertex_year: Vec<i16>,
    vertex_value: Vec<f32>,
    n_vertices: u8,
    rmse: f32,
}

fn lt_params(p: &Params) -> LtParams {
    LtParams {
        max_segments: p.int("max_segments") as i32,
        pval_threshold: p.f64("pval_threshold"),
        prevent_fast_recovery: true,
        recovery_threshold: p.f64("recovery_threshold"),
        spike_threshold: p.f64("spike_threshold"),
        best_model_proportion: p.f64("best_model_proportion"),
        vertex_count_overshoot: p.int("vertex_count_overshoot") as i32,
        min_observations_needed: p.int("min_observations_needed") as i32,
        modifier: if p.str("event_type") == "gain" { 1.0 } else { -1.0 },
    }
}

/// The segmentation of every pixel of `values` (pixels x T).
fn segment_many(p: &Params, years: &[i32], values: &[f64], pixels: usize, n_jobs: i32) -> Vec<Segmentation> {
    let params = lt_params(p);
    let max_v = (params.max_segments as i64 + 1).max(0) as usize;
    let (vertices, counts, rmse) = fit_trajectory_batch(values, pixels, years, &params, f64::NAN, n_jobs);
    (0..pixels)
        .map(|px| {
            let count = counts[px].max(0) as usize;
            let v = &vertices[px * 2 * max_v..(px + 1) * 2 * max_v];
            let present = |i: usize| i < count;
            Segmentation {
                vertex_year: (0..max_v).map(|i| if present(i) { v[2 * i] as f32 as i16 } else { 0 }).collect(),
                vertex_value: (0..max_v).map(|i| if present(i) { v[2 * i + 1] as f32 } else { f32::NAN }).collect(),
                n_vertices: count as u8,
                rmse: if count > 0 { rmse[px] as f32 } else { f32::NAN },
            }
        })
        .collect()
}

/// The calendar years of the series' dates, each once (zeit.landtrendr's requirement).
fn annual_years(input: &PixelInput) -> Result<Vec<i32>, String> {
    let years = input.calendar_years().ok_or("ValueError: the time axis has no dates: pass years=")?;
    let mut sorted = years.clone();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.len() != years.len() {
        return Err("ValueError: LandTrendr needs one observation per year, but some years repeat; build annual \
                    composites first"
            .into());
    }
    Ok(years)
}

/// zeit.extract_events of a segmentation (metrics.py `_extract_dataset`): the vertex stack in float32, NaN
/// values and the RMSE as 0.
fn event(p: &Params, seg: &Segmentation) -> Result<Event, String> {
    let ys: Vec<f32> = seg.vertex_year.iter().map(|&y| f32::from(y)).collect();
    let vs: Vec<f32> = seg.vertex_value.iter().map(|&v| if v.is_nan() { 0.0 } else { v }).collect();
    let rmse = if seg.rmse.is_nan() { 0.0 } else { seg.rmse };
    let sort_by = p.str("sort_by");
    Ok(landtrendr_event(
        &ys,
        &vs,
        rmse,
        p.str("event_type") == "loss",
        SortBy::parse(sort_by).ok_or_else(|| format!("ValueError: sort_by {sort_by:?}"))?,
        p.f64("min_magnitude"),
        p.int("min_duration") as f64,
        p.f64("pre_val_threshold"),
    ))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    if input.values.iter().filter(|v| v.is_finite()).count() < 2 {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Vertices", "-")] });
    }
    let years = annual_years(input)?;
    let seg = segment_many(p, &years, &input.values, 1, 1).remove(0);
    let n = (seg.n_vertices as usize).min(seg.vertex_year.len());
    let vx: Vec<f64> = seg.vertex_year[..n].iter().map(|&y| f64::from(y)).collect();
    let vvals: Vec<f64> = seg.vertex_value[..n].iter().map(|&v| f64::from(v)).collect();
    let vy: Vec<Option<f64>> = vvals.iter().map(|&v| v.is_finite().then_some(v)).collect();

    // segments in the direction of interest (the event the maps would show is below)
    let event_type = p.str("event_type");
    let loss = event_type == "loss";
    let n_events = vvals.windows(2).filter(|w| (if loss { w[0] - w[1] } else { w[1] - w[0] }) > 0.0).count();
    let mut capitalized = event_type.to_string();
    if let Some(c) = capitalized.get_mut(..1) {
        c.make_ascii_uppercase();
    }
    let mut rows = vec![
        row("Vertices", n.to_string()),
        row("Segments", n.saturating_sub(1).to_string()),
        row(format!("{capitalized} segments"), n_events.to_string()),
    ];
    let sort_by = p.str("sort_by");
    let e = event(p, &seg)?;
    if e.yod > 0 {
        let (y0, dur) = (e.yod, e.duration);
        rows.push(row(
            format!("Mapped event ({sort_by})"),
            format!("{} ({y0}-{})", g(f64::from(e.magnitude), 5), u32::from(y0) + u32::from(dur)),
        ));
        rows.push(row("  pre / post", format!("{} / {}", g(f64::from(e.pre_val), 5), g(f64::from(e.post_val), 5))));
        rows.push(row("  rate", format!("{}/yr", g(f64::from(e.rate), 5))));
        rows.push(row("  DSNR", if e.dsnr.is_finite() { g(f64::from(e.dsnr), 4) } else { "-".into() }));
    } else {
        rows.push(row("Mapped event", "none"));
    }
    Ok(PixelResult {
        overlays: vec![
            Overlay::new("line", "LandTrendr fit", vx.clone(), vy.clone()),
            Overlay::new("markers", "LandTrendr vertices", vx, vy),
        ],
        rows,
    })
}

/// A raster run's chunk: the event of every pixel (NaN where there is none).
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let years = annual_years(&ctx.input)?;
    let segs = segment_many(p, &years, &chunk.by_pixel(&chunk.stack), chunk.pixels(), ctx.n_jobs);
    let events: Vec<Event> = segs.iter().map(|s| event(p, s)).collect::<Result<_, _>>()?;
    let mut res = ChunkOut::new();
    let mut put = |id: &str, f: fn(&Event) -> f64| {
        res.insert(id.into(), events.iter().map(|e| if e.yod == 0 { f64::NAN } else { f(e) }).collect());
    };
    put("yod", |e| f64::from(e.yod));
    put("magnitude", |e| f64::from(e.magnitude));
    put("duration", |e| f64::from(e.duration));
    put("pre_val", |e| f64::from(e.pre_val));
    put("post_val", |e| f64::from(e.post_val));
    put("rate", |e| f64::from(e.rate));
    put("dsnr", |e| f64::from(e.dsnr));
    Ok(res)
}
