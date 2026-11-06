//! Clear-sky observations of FORCE's TSA (CSO, Frantz 2019), as zeit.clear_observations with its default start and
//! end (zeit/_tsa.py 473-582): the windows of `months` months begin on the first day of the first date's month and
//! go on until the one that holds the last date, which is kept whole; an observation is clear where its value is
//! finite (after the run's quality masking); `count` is the clear observations of a window, any other statistic is
//! that of the intervals in days between them (from the start of the window to the first, between consecutive ones,
//! from the last to the end of the window). The statistics are zeit-core's `cso_batch`; the output of each has one
//! band per window, labelled with its first month ("YYYY-MM").

use zeit_core::tsa::cso::cso_batch;
use zeit_core::tsa::stats::Metric;

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::manifest::{Output, manifest};
use crate::numeric::{civil, is_leap, year_start};
use crate::pixel::{Params, PixelInput, PixelResult, g, row};
use crate::temporal_metrics::unique_options;

/// The days before each month in a year of 365 days (zeit/_tsa.py `_CUM`).
const CUM: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

/// What a run is asked: the windows and the chosen statistics.
struct Setup {
    /// the first day of each month the windows start on, and the end of the last: days since 1970-01-01
    edges: Vec<i64>,
    /// the first month of each window, "YYYY-MM"
    labels: Vec<String>,
    /// the times of the dates, days
    t: Vec<f64>,
    options: Vec<String>,
    metrics: Vec<Metric>,
}

/// Days since 1970-01-01 of the first day of a month (`year`, 1..=12).
fn first_of_month(year: i64, month: u32) -> i64 {
    year_start(year) + CUM[month as usize - 1] + i64::from(month > 2 && is_leap(year))
}

fn setup(p: &Params, input: &PixelInput) -> Result<Setup, String> {
    let months = p.int("months");
    if !(1..=12).contains(&months) {
        return Err(format!("ValueError: months must be an integer from 1 to 12, got {months}"));
    }
    let options = unique_options(p, "statistics");
    if options.is_empty() {
        return Err("ValueError: Provide at least one statistic, e.g. ['count', 'max'].".into());
    }
    let metrics =
        options.iter().map(|o| Metric::parse(o).map_err(|e| format!("ValueError: {e}"))).collect::<Result<_, _>>()?;
    let days = input.days.as_ref().filter(|d| !d.is_empty()).ok_or(
        "ValueError: clear_observations needs dated series (this one has decimal years only): open a series whose \
         dates are read from its names or its time coordinate",
    )?;
    if days.windows(2).any(|w| w[1] < w[0]) {
        return Err("ValueError: clear_observations needs dates that do not decrease".into());
    }
    let (first, last) = (civil(days[0]), civil(days[days.len() - 1]));
    let month0 = first.0 * 12 + i64::from(first.1) - 1;
    let n_windows = (last.0 * 12 + i64::from(last.1) - 1 - month0).div_euclid(months) + 1;
    let bounds: Vec<i64> = (0..=n_windows).map(|k| month0 + months * k).collect();
    let edges = bounds.iter().map(|b| first_of_month(b.div_euclid(12), b.rem_euclid(12) as u32 + 1)).collect();
    let labels = bounds[..n_windows as usize]
        .iter()
        .map(|b| format!("{:04}-{:02}", b.div_euclid(12), b.rem_euclid(12) + 1))
        .collect();
    Ok(Setup { edges, labels, t: days.iter().map(|&d| d as f64).collect(), options, metrics })
}

impl Setup {
    fn windows(&self) -> Vec<(f64, f64)> {
        self.edges.windows(2).map(|w| (w[0] as f64, w[1] as f64)).collect()
    }

    /// The statistics of every pixel: [pixels][metrics][windows].
    fn run(&self, values: &[f64], n: usize, n_jobs: i32) -> Result<Vec<f64>, String> {
        cso_batch(values, n, self.t.len(), &self.t, &self.windows(), &self.metrics, n_jobs)
            .map_err(|e| format!("ValueError: {e}"))
    }
}

/// The label of a statistic option in the tool's descriptor.
fn stat_label(option: &str) -> String {
    manifest()
        .tool("clear_observations")
        .and_then(|t| t.params.iter().find(|q| q.id == "statistics"))
        .and_then(|q| Some((q.options.as_ref()?, q.labels.as_ref()?)))
        .and_then(|(options, labels)| labels.get(options.iter().position(|x| x == option)?).cloned())
        .unwrap_or_else(|| option.to_string())
}

/// The labels of the outputs' bands: the first month of each window.
pub fn bands(_o: &Output, p: &Params, input: &PixelInput) -> Result<Vec<String>, String> {
    Ok(setup(p, input)?.labels)
}

pub fn pixel(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let s = setup(p, input)?;
    let nw = s.labels.len();
    let m = s.metrics.len();
    let out = s.run(&input.values, 1, 1)?;
    let mut rows = vec![
        row(
            "Valid observations",
            format!("{} of {}", input.values.iter().filter(|v| v.is_finite()).count(), input.values.len()),
        ),
        row("Windows", nw.to_string()),
    ];
    // the summary: the clear observations per window and the longest gap, whatever statistics are chosen
    let summary = Setup { metrics: vec![Metric::Count, Metric::Max], ..setup(p, input)? };
    let sum = summary.run(&input.values, 1, 1)?;
    let counts: Vec<f64> = (0..nw).map(|w| sum[w]).collect();
    let longest = (0..nw).map(|w| sum[nw + w]).fold(f64::NAN, f64::max);
    rows.push(row("Clear observations per window", g(counts.iter().sum::<f64>() / nw as f64, 6)));
    rows.push(row("Longest gap (days)", g(longest, 6)));
    // a short table: the first twelve windows of each chosen statistic
    for (k, o) in s.options.iter().enumerate() {
        let label = stat_label(o);
        let vals = &out[k * nw..(k + 1) * nw];
        debug_assert_eq!(out.len(), m * nw);
        for (name, v) in s.labels.iter().zip(vals).take(12) {
            rows.push(row(format!("{label}, {name}"), g(*v, 6)));
        }
        if nw > 12 {
            rows.push(row(format!("{label}, other windows"), format!("{} more", nw - 12)));
        }
    }
    Ok(PixelResult { overlays: vec![], rows })
}

/// A raster run's chunk: a series of each chosen statistic, one band per window, [windows][pixels].
pub fn chunk(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let s = setup(p, &ctx.input)?;
    let (n, m, nw) = (chunk.pixels(), s.metrics.len(), s.labels.len());
    let out = s.run(&chunk.by_pixel(&chunk.stack), n, ctx.n_jobs)?;
    let mut res = ChunkOut::new();
    for (k, o) in s.options.iter().enumerate() {
        // [pixels][metrics][windows] -> [windows][pixels]
        let mut bands = vec![0.0; nw * n];
        for px in 0..n {
            for w in 0..nw {
                bands[w * n + px] = out[(px * m + k) * nw + w];
            }
        }
        res.insert(format!("o_{o}"), bands);
    }
    Ok(res)
}
