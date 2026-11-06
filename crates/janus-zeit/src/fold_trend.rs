//! FORCE's trend and CAT analyses of a fold (Frantz 2019), as zeit.linear_trend and zeit.cat over
//! zeit.temporal_metrics with a period (`by`, one statistic): the statistic of each pixel's series by period
//! (zeit-core's `fold_batch`, ddof = 1, every period from the first year to the last, or every quarter, month,
//! week or day of the year, the empty ones NaN and 0 for the count), then `trend_batch` (the OLS line over the
//! periods, x their position 0..n-1) or `cat_batch` (the greatest change and the trend before and after it) on
//! the fold. The change's time is the period's coordinate as zeit's `change_time`: the year, or the number of the
//! quarter, month, week or day. A raster run writes a map per chosen statistic.

use zeit_core::tsa::stats::{Metric, fold_batch};
use zeit_core::tsa::trend::{CAT_LEN, TREND_LEN, Tail, cat_batch, trend_batch};

use crate::job::{Chunk, ChunkOut, JobCtx};
use crate::manifest::manifest;
use crate::pixel::{Overlay, Params, PixelInput, PixelResult, g, row};
use crate::temporal_metrics::{Periods, metric_label, periods, unique_options};

/// zeit/_tsa.py `TREND_NAMES` (586).
const TREND_NAMES: [&str; TREND_LEN] =
    ["mean", "offset", "slope", "pct_gain", "abs_gain", "rsq", "sig", "rmse", "mae", "maxe", "num", "len"];

#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Trend,
    Cat,
}

impl Tool {
    fn id(self) -> &'static str {
        match self {
            Tool::Trend => "linear_trend",
            Tool::Cat => "cat",
        }
    }

    /// The width of a pixel's row of statistics.
    fn width(self) -> usize {
        match self {
            Tool::Trend => TREND_LEN,
            Tool::Cat => CAT_LEN,
        }
    }

    /// The prefix of a chunk's output ids (tools.json).
    fn prefix(self) -> &'static str {
        match self {
            Tool::Trend => "t",
            Tool::Cat => "c",
        }
    }

    /// The statistics' names in the order of the row (zeit's `CAT_NAMES` (587) with `change_time` for
    /// `change_index`).
    fn names(self) -> Vec<String> {
        match self {
            Tool::Trend => TREND_NAMES.iter().map(|s| s.to_string()).collect(),
            Tool::Cat => {
                let mut v: Vec<String> = ["change", "loss", "change_time"].iter().map(|s| s.to_string()).collect();
                for part in ["total", "before", "after"] {
                    v.extend(TREND_NAMES.iter().map(|n| format!("{part}_{n}")));
                }
                v
            }
        }
    }
}

/// What a run is asked: the fold and the trend's parameters.
struct Setup {
    tool: Tool,
    metric: Metric,
    option: String,
    by: String,
    per: Periods,
    conf: f64,
    tail: Tail,
    penalty: bool,
    /// the chosen statistics: their names and their place in the row
    chosen: Vec<(String, usize)>,
}

/// zeit/_tsa.py `_check_trend` (591-ish): conf in (0, 1) and a known tail.
fn setup(tool: Tool, p: &Params, input: &PixelInput) -> Result<Setup, String> {
    let conf = p.f64("conf");
    if !(conf > 0.0 && conf < 1.0) {
        return Err(format!("ValueError: conf must be between 0 and 1, got {conf:?}"));
    }
    let tail = match p.str("tail") {
        t @ ("two" | "left" | "right") => Tail::parse(t)?,
        t => return Err(format!("ValueError: tail must be one of ('two', 'left', 'right'), got {t:?}")),
    };
    let option = p.str("metric").to_string();
    let metric = Metric::parse(&option).map_err(|e| format!("ValueError: {e}"))?;
    let by = p.str("by").to_string();
    let per = periods(&by, input)?;
    let names = tool.names();
    let wanted = unique_options(p, "statistics");
    if wanted.is_empty() {
        return Err("ValueError: Provide at least one statistic, e.g. ['slope', 'sig'].".into());
    }
    let chosen = wanted
        .into_iter()
        .map(|s| {
            let k = names.iter().position(|n| *n == s).ok_or_else(|| format!("ValueError: unknown statistic {s:?}"))?;
            Ok((s, k))
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(Setup { tool, metric, option, by, per, conf, tail, penalty: p.bool("penalty"), chosen })
}

impl Setup {
    /// The fold of every pixel's series (`values`, [pixels][t]): [pixels][periods].
    fn fold(&self, values: &[f64], n: usize, t: usize, n_jobs: i32) -> Vec<f64> {
        fold_batch(values, n, t, &self.per.group, self.per.labels.len(), &[self.metric], 1.0, n_jobs)
    }

    /// The trend's or CAT's statistics of every pixel: [pixels][width], the change index of CAT at 2 turned
    /// into the period's coordinate (NaN where there is none), as zeit.cat's `_take` (zeit/_tsa.py 746-760).
    fn rows(&self, fold: &[f64], n: usize, n_jobs: i32) -> Result<Vec<f64>, String> {
        let ng = self.per.labels.len();
        match self.tool {
            Tool::Trend => trend_batch(fold, n, ng, self.conf, self.tail, n_jobs),
            Tool::Cat => {
                let mut out = cat_batch(fold, n, ng, self.penalty, self.conf, self.tail, n_jobs)?;
                for r in out.chunks_mut(CAT_LEN) {
                    r[2] = if r[2].is_finite() { self.per.coord[r[2] as usize] } else { f64::NAN };
                }
                Ok(out)
            }
        }
    }
}

/// The label of a statistic option in the tool's descriptor.
fn stat_label(tool: Tool, option: &str) -> String {
    manifest()
        .tool(tool.id())
        .and_then(|t| t.params.iter().find(|q| q.id == "statistics"))
        .and_then(|q| Some((q.options.as_ref()?, q.labels.as_ref()?)))
        .and_then(|(options, labels)| labels.get(options.iter().position(|x| x == option)?).cloned())
        .unwrap_or_else(|| option.to_string())
}

pub fn pixel_trend(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    pixel(Tool::Trend, p, input)
}

pub fn pixel_cat(p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    pixel(Tool::Cat, p, input)
}

pub fn chunk_trend(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    run_chunk(Tool::Trend, p, chunk, ctx)
}

pub fn chunk_cat(p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    run_chunk(Tool::Cat, p, chunk, ctx)
}

/// A line over the periods `lo..hi` of the fit `offset + slope * (k - lo)`, as the fit is made (x the position of
/// the period, the offset the fit at the first one); None where the fit has no slope.
fn fit_line(s: &Setup, label: &str, offset: f64, slope: f64, lo: usize, hi: usize) -> Option<Overlay> {
    if !(offset.is_finite() && slope.is_finite()) || hi <= lo {
        return None;
    }
    let x = s.per.years[lo..hi].to_vec();
    let y = (lo..hi).map(|k| Some(offset + slope * (k - lo) as f64)).collect();
    let mut line = Overlay::new("line", label, x, y);
    line.weight = Some(2.0);
    Some(line)
}

fn pixel(tool: Tool, p: &Params, input: &PixelInput) -> Result<PixelResult, String> {
    let s = setup(tool, p, input)?;
    let (t, ng) = (input.values.len(), s.per.labels.len());
    let fold = s.fold(&input.values, 1, t, 1);
    let r = s.rows(&fold, 1, 1)?;
    let mut rows =
        vec![row("Valid observations", format!("{} of {}", input.values.iter().filter(|v| v.is_finite()).count(), t))];
    for (name, k) in &s.chosen {
        rows.push(row(stat_label(tool, name), g(r[*k], 6)));
    }
    let mut overlays = vec![];
    if s.by == "year" {
        let label = format!("{} by year", metric_label(tool.id(), &s.option));
        let y = fold.iter().map(|&v| v.is_finite().then_some(v)).collect();
        overlays.push(Overlay::new("markers", &label, s.per.years.clone(), y));
        match tool {
            Tool::Trend => {
                overlays.extend(fit_line(&s, "Linear trend", r[1], r[2], 0, ng));
            }
            Tool::Cat => {
                // the parts as cat_batch fits them: before, steps 0..change - 1; after, change..n
                let change = r[2].is_finite().then(|| (r[2] - s.per.coord[0]) as usize);
                if let Some(f) = change {
                    overlays.extend(fit_line(
                        &s,
                        "Before the change",
                        r[3 + TREND_LEN + 1],
                        r[3 + TREND_LEN + 2],
                        0,
                        f.saturating_sub(1),
                    ));
                    overlays.extend(fit_line(
                        &s,
                        "After the change",
                        r[3 + 2 * TREND_LEN + 1],
                        r[3 + 2 * TREND_LEN + 2],
                        f,
                        ng,
                    ));
                    overlays.push(Overlay::new("vlines", "Greatest change", vec![s.per.years[f]], vec![]));
                }
            }
        }
    }
    Ok(PixelResult { overlays, rows })
}

/// A raster run's chunk: a map of each chosen statistic, [pixels].
fn run_chunk(tool: Tool, p: &Params, chunk: &Chunk, ctx: &JobCtx) -> Result<ChunkOut, String> {
    let s = setup(tool, p, &ctx.input)?;
    let n = chunk.pixels();
    let fold = s.fold(&chunk.by_pixel(&chunk.stack), n, chunk.t, ctx.n_jobs);
    let r = s.rows(&fold, n, ctx.n_jobs)?;
    let w = tool.width();
    let mut res = ChunkOut::new();
    for (name, k) in &s.chosen {
        res.insert(format!("{}_{name}", tool.prefix()), (0..n).map(|px| r[px * w + k]).collect());
    }
    Ok(res)
}
