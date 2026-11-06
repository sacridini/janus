//! Polar phenology metrics of FORCE's TSA (POL, Frantz's polar-hl.c after Brooks et al. 2020), as
//! zeit.polar_metrics (zeit/_tsa.py 785-964): on a regular series (a constant step of whole days over at least 2
//! calendar years, as the output of Interpolate) each pixel's metrics per year, from zeit-core's `polar_batch`
//! called with the same arguments: the days in FORCE's calendar of 365 days counted from the first year (`ce`, 29
//! February sharing day 60 with 1 March), the step in days, the three thresholds and `adaptive`. The output of
//! each metric has one band per calendar year from the first date's to the last's (the first or the last may be
//! empty: a season belongs to the year it ends in). The date metrics are days since 1 January of the band's year.

use zeit_core::tsa::polar::{self, polar_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::manifest::{Output, manifest};
use crate::numeric::civil;
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::temporal_metrics::unique_options;

/// The days before each month in a year of 365 days (zeit/_tsa.py `_CUM`).
const CUM: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// The metrics that are the date of an event of the season (what the chart marks with a vertical line): the
/// others of days (DPY, DPV) are shifts, not events.
const EVENTS: [&str; 9] = ["DEM", "DLM", "DPS", "DSS", "DMS", "DES", "DEV", "DAV", "DLV"];

/// What a run is asked: the series' calendar and the chosen metrics.
struct Setup {
    /// FORCE's day of each step: `year * 365 + doy` counted from the first year
    ce: Vec<i64>,
    step: i32,
    first_year: i64,
    ny: usize,
    options: Vec<String>,
    metrics: Vec<usize>,
    start: f64,
    mid: f64,
    end: f64,
    adaptive: bool,
}

fn setup(p: &Params, input: &PixelInput) -> Result<Setup, String> {
    let options = unique_options(p, "metrics");
    if options.is_empty() {
        return Err("ValueError: provide at least one metric".into());
    }
    let unknown: Vec<&String> = options.iter().filter(|o| polar::parse(o).is_err()).collect();
    if !unknown.is_empty() {
        return Err(format!("ValueError: unknown polar metrics {unknown:?}: use some of {}", polar::NAMES.join(", ")));
    }
    let metrics = polar::parse_all(&options).map_err(|e| format!("ValueError: {e}"))?;
    let (start, mid, end) = (p.f64("start"), p.f64("mid"), p.f64("end"));
    for (label, v) in [("start", start), ("mid", mid), ("end", end)] {
        if !(v > 0.0 && v < 1.0) {
            return Err(format!("ValueError: {label} must be between 0 and 1, got {v:?}"));
        }
    }
    let days = input.days.as_ref().filter(|d| !d.is_empty()).ok_or(
        "ValueError: polar_metrics needs dated series (this one has decimal years only): open a series whose dates \
         are read from its names or its time coordinate",
    )?;
    if days.len() < 3 {
        return Err("ValueError: polar_metrics needs a date for every step and at least 3 steps".into());
    }
    let gap = days[1] - days[0];
    if gap <= 0 || days.windows(2).any(|w| w[1] - w[0] != gap) {
        return Err("ValueError: polar_metrics needs a regular series, a constant step of whole days (the dates are \
                    not evenly spaced): run Interpolate (regular grid) first and run Polar phenology on its result"
            .into());
    }
    let ymd: Vec<(i64, u32, u32)> = days.iter().map(|&d| civil(d)).collect();
    let first_year = ymd[0].0;
    let ny = (ymd[ymd.len() - 1].0 - first_year + 1) as usize;
    if ny < 2 {
        return Err("ValueError: polar_metrics needs at least 2 calendar years: a season runs from one start of the \
                    phenological year to the next"
            .into());
    }
    let ce = ymd.iter().map(|c| (c.0 - first_year) * 365 + CUM[c.1 as usize - 1] + i64::from(c.2)).collect();
    Ok(Setup { ce, step: gap as i32, first_year, ny, options, metrics, start, mid, end, adaptive: p.bool("adaptive") })
}

impl Setup {
    /// The metrics of every pixel: [pixels][metrics][years].
    fn run(&self, values: &[f64], n: usize, n_jobs: i32) -> Result<Vec<f64>, String> {
        let (out, ny) = polar_batch(
            values,
            n,
            self.ce.len(),
            &self.ce,
            self.step,
            self.start,
            self.mid,
            self.end,
            self.adaptive,
            &self.metrics,
            n_jobs,
        )
        .map_err(|e| format!("ValueError: {e}"))?;
        debug_assert_eq!(ny, self.ny);
        Ok(out)
    }

    fn year(&self, k: usize) -> i64 {
        self.first_year + k as i64
    }
}

/// The label of a metric option in the tool's descriptor.
fn metric_label(option: &str) -> String {
    manifest()
        .tool("polar_metrics")
        .and_then(|t| t.params.iter().find(|q| q.id == "metrics"))
        .and_then(|q| Some((q.options.as_ref()?, q.labels.as_ref()?)))
        .and_then(|(options, labels)| labels.get(options.iter().position(|x| x == option)?).cloned())
        .unwrap_or_else(|| option.to_string())
}

/// The labels of the outputs' bands: the calendar years, from the first date's to the last's.
pub fn bands(_o: &Output, p: &Params, input: &PixelInput) -> Result<Vec<String>, String> {
    let s = setup(p, input)?;
    Ok((0..s.ny).map(|k| s.year(k).to_string()).collect())
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let s = setup(p, input)?;
    let (m, ny) = (s.metrics.len(), s.ny);
    let out = s.run(&input.values, 1, 1)?;
    let mut rows = vec![
        row(
            "Valid observations",
            format!("{} of {}", input.values.iter().filter(|v| v.is_finite()).count(), input.values.len()),
        ),
        row("Step", format!("{} days, {ny} years", s.step)),
    ];
    let mut overlays = vec![];
    for (k, o) in s.options.iter().enumerate() {
        let label = metric_label(o);
        let vals = &out[k * ny..(k + 1) * ny];
        debug_assert_eq!(out.len(), m * ny);
        let with_data: Vec<usize> = (0..ny).filter(|&i| vals[i].is_finite()).collect();
        for &i in with_data.iter().take(12) {
            rows.push(row(format!("{o}, {}", s.year(i)), g(vals[i], 6)));
        }
        if with_data.len() > 12 {
            rows.push(row(format!("{o}, other years"), format!("{} more with a value", with_data.len() - 12)));
        }
        if with_data.is_empty() {
            rows.push(row(o.to_ascii_uppercase(), "no value in any year"));
        }
        // a date metric: a vertical line at each year's date (decimal years, 365.25 days a year)
        if EVENTS.contains(&o.to_ascii_uppercase().as_str()) && !with_data.is_empty() {
            let x = with_data.iter().map(|&i| s.year(i) as f64 + vals[i] / 365.25).collect();
            overlays.push(Overlay::new("vlines", &label, x, vec![]));
        }
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: a series of each chosen metric, one band per year, [years][pixels].
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let s = setup(p, &ctx.input)?;
    let (n, m, ny) = (chunk.pixels(), s.metrics.len(), s.ny);
    let out = s.run(&chunk.by_pixel(&chunk.stack), n, ctx.n_jobs)?;
    let mut res = ChunkOut::new();
    for (k, o) in s.options.iter().enumerate() {
        // [pixels][metrics][years] -> [years][pixels]
        let mut bands = vec![0.0; ny * n];
        for px in 0..n {
            for yi in 0..ny {
                bands[yi * n + px] = out[(px * m + k) * ny + yi];
            }
        }
        res.insert(format!("p_{o}"), bands);
    }
    Ok(res)
}
