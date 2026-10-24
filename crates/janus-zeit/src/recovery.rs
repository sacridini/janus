//! Recovery (zeit.recovery_metrics on zeit.landtrendr): LandTrendr's segmentation of the annual series, the
//! disturbance it maps (zeit.extract_events' event), and how the fitted trajectory came back after it: the
//! years to recovery, R80P, RRI, the absolute and year-on-year recovery (White et al. 2017; Frazier et al. 2018).
//! A tool after the bridge: its reference is zeit-core's recovery module, which zeit.recovery_metrics calls.

use zeit_core::recovery::{self, Basis, Params as RecParams, Recovery};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::landtrendr::{Segmentation, annual_years, event, segment_many};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::upstream::Event;

fn rec_params(p: &Params) -> Result<RecParams, String> {
    let basis = p.str("basis");
    let basis = Basis::parse(basis).ok_or_else(|| format!("ValueError: basis {basis:?}"))?;
    let loss = p.str("event_type") == "loss";
    if !loss && basis == Basis::PreValue {
        return Err("ValueError: basis=\"pre_value\" (a ratio of the pre-disturbance value) is for losses; use \
                    basis=\"magnitude\" for gains"
            .into());
    }
    Ok(RecParams {
        target: p.f64("target"),
        horizon: p.int("horizon") as i32,
        pre_years: p.int("pre_years") as i32,
        basis,
        loss,
    })
}

/// A segmentation's mapped event, its fitted trajectory at `years` and the recovery after it (NaN without an
/// event), as zeit.recovery_metrics measures them: the vertices as LandTrendr's Dataset keeps them.
fn measure(
    p: &Params,
    rp: &RecParams,
    years: &[i32],
    seg: &Segmentation,
) -> Result<(Event, Vec<f64>, Recovery), String> {
    let e = event(p, seg)?;
    let vx: Vec<f64> = seg.vertex_year.iter().map(|&y| f64::from(y)).collect();
    let vv: Vec<f64> = seg.vertex_value.iter().map(|&v| if v.is_nan() { 0.0 } else { f64::from(v) }).collect();
    let fit = recovery::trajectory(&vx, &vv, seg.n_vertices as usize, years);
    let r = if e.yod > 0 {
        recovery::metrics(years, &fit, i32::from(e.yod), i32::from(e.yod) + i32::from(e.duration), rp)
    } else {
        Recovery::default()
    };
    Ok((e, fit, r))
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let rp = rec_params(p)?;
    if input.values.iter().filter(|v| v.is_finite()).count() < 2 {
        return Ok(PixelResult { overlays: vec![], rows: vec![row("Disturbance", "-")] });
    }
    let years = annual_years(input)?;
    let seg = segment_many(p, &years, &input.values, 1, 1).remove(0);
    let (e, fit, r) = measure(p, &rp, &years, &seg)?;
    let n = (seg.n_vertices as usize).min(seg.vertex_year.len());
    let vx: Vec<f64> = seg.vertex_year[..n].iter().map(|&y| f64::from(y)).collect();
    let vy: Vec<Option<f64>> = seg.vertex_value[..n].iter().map(|&v| v.is_finite().then_some(f64::from(v))).collect();
    let mut overlays = vec![
        Overlay::new("line", "LandTrendr fit", vx.clone(), vy.clone()),
        Overlay::new("markers", "LandTrendr vertices", vx, vy),
    ];
    if e.yod == 0 {
        let kind = if rp.loss { "loss" } else { "gain" };
        return Ok(PixelResult { overlays, rows: vec![row("Disturbance", format!("no {kind} event"))] });
    }
    let fmt = |v: f64| if v.is_finite() { g(v, 5) } else { "-".into() };
    let (start, trough) = (i32::from(e.yod), i32::from(e.yod) + i32::from(e.duration));
    let k = rp.horizon;
    let pct = g(rp.target * 100.0, 4);
    let level = r.level(&rp);
    let mut rows = vec![
        row(format!("Disturbance ({})", p.str("sort_by")), format!("{start}-{trough}")),
        row("Pre-disturbance value", fmt(r.pre_val)),
        row("Value at the trough", fmt(r.dist_val)),
        row("Magnitude", fmt(r.magnitude)),
        row(
            "Years to recovery (Y2R)",
            if r.y2r.is_finite() {
                format!("{} ({})", r.y2r, trough + r.y2r as i32)
            } else {
                format!("not recovered by {}", years[years.len() - 1])
            },
        ),
        row(format!("Value at +{k} years"), fmt(r.post_val)),
        row(format!("Absolute recovery (ARI, +{k})"), fmt(r.ari)),
        row(format!("RRI (+{k})"), fmt(r.rri)),
        row(format!("R{pct}P (+{k})"), fmt(r.r80p)),
        row(format!("YrYr (+{k})"), fmt(r.yryr) + "/yr"),
    ];
    if !r.post_val.is_finite() && r.dist_val.is_finite() {
        rows.push(row("", format!("the series ends before {}", trough + k)));
    }
    // the recovery level from the trough to the last year, dashed; the year it was reached
    if level.is_finite() {
        let what = match rp.basis {
            Basis::PreValue => format!("{pct}% of pre-disturbance"),
            Basis::Magnitude => format!("{pct}% of the loss regained"),
        };
        let x = vec![f64::from(trough), f64::from(years[years.len() - 1])];
        let mut o = Overlay::new("line", &format!("Recovery level ({what})"), x, vec![Some(level); 2]);
        o.style = Some("dashed".into());
        o.weight = Some(1.5);
        overlays.push(o);
        if r.y2r.is_finite() && r.y2r > 0.0 {
            overlays.push(Overlay::new("vlines", "Recovered", vec![f64::from(trough) + r.y2r], vec![]));
        }
    }
    // the trajectory after the trough, up to the horizon: what the metrics at +k measure
    let span: Vec<usize> = (0..years.len()).filter(|&i| years[i] >= trough && years[i] <= trough + k).collect();
    if span.len() >= 2 {
        let mut o = Overlay::new(
            "line",
            "Recovery window",
            span.iter().map(|&i| f64::from(years[i])).collect(),
            span.iter().map(|&i| fit[i].is_finite().then_some(fit[i])).collect(),
        );
        o.weight = Some(4.0);
        o.alpha = Some(0.5);
        overlays.push(o);
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: the recovery after every pixel's event (NaN where there is none).
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let rp = rec_params(p)?;
    let years = annual_years(&ctx.input)?;
    let segs = segment_many(p, &years, &chunk.by_pixel(&chunk.stack), chunk.pixels(), ctx.n_jobs);
    let measured: Vec<(Event, Recovery)> =
        segs.iter().map(|s| measure(p, &rp, &years, s).map(|(e, _, r)| (e, r))).collect::<Result<_, _>>()?;
    let mut res = ChunkOut::new();
    let mut put = |id: &str, f: fn(&Recovery) -> f64| {
        res.insert(id.into(), measured.iter().map(|(_, r)| f(r)).collect());
    };
    put("y2r", |r| r.y2r);
    put("r80p", |r| r.r80p);
    put("rri", |r| r.rri);
    put("ari", |r| r.ari);
    put("yryr", |r| r.yryr);
    put("magnitude", |r| r.magnitude);
    put("pre_val", |r| r.pre_val);
    res.insert(
        "yod".into(),
        measured.iter().map(|(e, _)| if e.yod == 0 { f64::NAN } else { f64::from(e.yod) }).collect(),
    );
    Ok(res)
}
