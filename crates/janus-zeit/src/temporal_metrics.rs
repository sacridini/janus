//! Temporal metrics of FORCE's TSA (STM and fold, Frantz 2019), as zeit.temporal_metrics: the statistics of each
//! pixel's series over all of it (`temporal_metrics`, a map per metric) or by period (`fold`, a series per
//! metric whose bands are the periods: the years from the first date's to the last's, the quarters, months, weeks
//! or days of the year, all of them, the empty ones NaN and 0 for the count). The statistics are zeit-core's
//! `stm_batch` and `fold_batch` with ddof = 1, as zeit.temporal_metrics' default.

use zeit_core::tsa::stats::{Metric, fold_batch, stm_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::manifest::{Output, manifest};
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};

/// The days before each month in a year of 365 days (zeit/_tsa.py `_CUM`): 29 February and 1 March share day 60.
const CUM: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The chosen metrics: each option once, in the order given, with its zeit-core metric.
struct Chosen {
    options: Vec<String>,
    metrics: Vec<Metric>,
}

/// The options of a multi parameter, each once, in the order given.
pub(crate) fn unique_options(p: &Params, id: &str) -> Vec<String> {
    let mut options: Vec<String> = vec![];
    if let Some(a) = p.0.get(id).and_then(|v| v.as_array()) {
        for v in a {
            if let Some(s) = v.as_str()
                && !options.iter().any(|o| o == s)
            {
                options.push(s.to_string());
            }
        }
    }
    options
}

fn chosen(p: &Params) -> Result<Chosen, String> {
    let options = unique_options(p, "metrics");
    if options.is_empty() {
        return Err("ValueError: Provide at least one metric, e.g. ['median', 'p10', 'p90'].".into());
    }
    let metrics =
        options.iter().map(|o| Metric::parse(o).map_err(|e| format!("ValueError: {e}"))).collect::<Result<_, _>>()?;
    Ok(Chosen { options, metrics })
}

/// The label of a metric option in the tool's descriptor.
pub(crate) fn metric_label(tool: &str, option: &str) -> String {
    manifest()
        .tool(tool)
        .and_then(|t| t.params.iter().find(|q| q.id == "metrics" || q.id == "metric"))
        .and_then(|q| Some((q.options.as_ref()?, q.labels.as_ref()?)))
        .and_then(|(options, labels)| labels.get(options.iter().position(|x| x == option)?).cloned())
        .unwrap_or_else(|| option.to_string())
}

/// The periods of a fold: the group of each date (an index into the labels), the labels, and, by year, the x of
/// each year on the chart (decimal years: its middle).
pub(crate) struct Periods {
    pub group: Vec<i32>,
    pub labels: Vec<String>,
    pub years: Vec<f64>,
    /// zeit's coordinate of each period: the year, or 1-4, 1-12, 1-52, 1-365
    pub coord: Vec<f64>,
}

/// zeit/_tsa.py `_periods` (31-48): by year, the years from the first date's to the last's; else 1-4, 1-12, 1-52
/// (`min(ceil(doy / 7), 52)`) or 1-365, doy counting 365 days.
pub(crate) fn periods(by: &str, input: &PixelInput) -> Result<Periods, String> {
    if !["year", "quarter", "month", "week", "doy"].contains(&by) {
        return Err(format!(
            "ValueError: by must be None or one of ('year', 'quarter', 'month', 'week', 'doy'), got {by:?}"
        ));
    }
    let days = input.days.as_ref().filter(|d| !d.is_empty()).ok_or(
        "ValueError: fold needs dated series (this one has decimal years only): open a series whose dates are read \
         from its names or its time coordinate",
    )?;
    let ymd: Vec<(i64, u32, u32)> = days.iter().map(|&d| crate::numeric::civil(d)).collect();
    if by == "year" {
        let lo = ymd.iter().map(|c| c.0).min().unwrap();
        let hi = ymd.iter().map(|c| c.0).max().unwrap();
        return Ok(Periods {
            group: ymd.iter().map(|c| (c.0 - lo) as i32).collect(),
            labels: (lo..=hi).map(|y| y.to_string()).collect(),
            years: (lo..=hi).map(|y| y as f64 + 0.5).collect(),
            coord: (lo..=hi).map(|y| y as f64).collect(),
        });
    }
    let n = match by {
        "quarter" => 4,
        "month" => 12,
        "week" => 52,
        _ => 365,
    };
    let number = |c: &(i64, u32, u32)| -> i64 {
        let doy = CUM[c.1 as usize - 1] + i64::from(c.2);
        match by {
            "quarter" => i64::from((c.1 - 1) / 3 + 1),
            "month" => i64::from(c.1),
            "week" => ((doy + 6) / 7).min(52),
            _ => doy,
        }
    };
    let labels = (1..=n)
        .map(|k| match by {
            "quarter" => format!("Q{k}"),
            "month" => MONTHS[k - 1].to_string(),
            "week" => format!("week {k:02}"),
            _ => format!("doy {k:03}"),
        })
        .collect();
    Ok(Periods {
        group: ymd.iter().map(|c| (number(c) - 1) as i32).collect(),
        labels,
        years: vec![],
        coord: (1..=n).map(|k| k as f64).collect(),
    })
}

/// The labels of `fold`'s bands: the periods.
pub fn fold_bands(_o: &Output, p: &Params, input: &PixelInput) -> Result<Vec<String>, String> {
    chosen(p)?;
    Ok(periods(p.str("by"), input)?.labels)
}

// ---- temporal_metrics -----------------------------------------------------------------------------------------

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let c = chosen(p)?;
    let out = stm_batch(&input.values, 1, input.values.len(), &c.metrics, 1.0, 1);
    let mut rows = vec![row(
        "Valid observations",
        format!("{} of {}", input.values.iter().filter(|v| v.is_finite()).count(), input.values.len()),
    )];
    for (o, v) in c.options.iter().zip(&out) {
        rows.push(row(metric_label("temporal_metrics", o), g(*v, 6)));
    }
    Ok(PixelResult { overlays: vec![], rows })
}

/// A raster run's chunk: a map of each chosen metric, [pixels].
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let c = chosen(p)?;
    let n = chunk.pixels();
    let m = c.metrics.len();
    let out = stm_batch(&chunk.by_pixel(&chunk.stack), n, chunk.t, &c.metrics, 1.0, ctx.n_jobs);
    let mut res = ChunkOut::new();
    for (k, o) in c.options.iter().enumerate() {
        res.insert(format!("m_{o}"), (0..n).map(|px| out[px * m + k]).collect());
    }
    Ok(res)
}

// ---- fold -----------------------------------------------------------------------------------------------------

pub fn fold_pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let c = chosen(p)?;
    let by = p.str("by");
    let per = periods(by, input)?;
    let (m, ng) = (c.metrics.len(), per.labels.len());
    let out = fold_batch(&input.values, 1, input.values.len(), &per.group, ng, &c.metrics, 1.0, 1);
    let mut overlays = vec![];
    let mut rows = vec![row(
        "Valid observations",
        format!("{} of {}", input.values.iter().filter(|v| v.is_finite()).count(), input.values.len()),
    )];
    for (k, o) in c.options.iter().enumerate() {
        let label = metric_label("fold", o);
        let vals = &out[k * ng..(k + 1) * ng];
        debug_assert_eq!(out.len(), m * ng);
        if by == "year" {
            let y = vals.iter().map(|&v| v.is_finite().then_some(v)).collect();
            overlays.push(Overlay::new("line", &format!("{label} by year"), per.years.clone(), y));
        }
        // a short table: the periods with a value, the first twelve
        let with_data: Vec<usize> = (0..ng).filter(|&i| vals[i].is_finite()).collect();
        for &i in with_data.iter().take(12) {
            rows.push(row(format!("{label}, {}", per.labels[i]), g(vals[i], 6)));
        }
        if with_data.len() > 12 {
            rows.push(row(format!("{label}, other periods"), format!("{} more with a value", with_data.len() - 12)));
        }
        if with_data.is_empty() {
            rows.push(row(label, "no value in any period"));
        }
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: a series of each chosen metric, one band per period, [periods][pixels].
pub fn fold_chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let c = chosen(p)?;
    let per = periods(p.str("by"), &ctx.input)?;
    let (n, m, ng) = (chunk.pixels(), c.metrics.len(), per.labels.len());
    let out = fold_batch(&chunk.by_pixel(&chunk.stack), n, chunk.t, &per.group, ng, &c.metrics, 1.0, ctx.n_jobs);
    let mut res = ChunkOut::new();
    for (k, o) in c.options.iter().enumerate() {
        // [pixels][metrics][periods] -> [periods][pixels]
        let mut bands = vec![0.0; ng * n];
        for px in 0..n {
            for gi in 0..ng {
                bands[gi * n + px] = out[(px * m + k) * ng + gi];
            }
        }
        res.insert(format!("f_{o}"), bands);
    }
    Ok(res)
}
