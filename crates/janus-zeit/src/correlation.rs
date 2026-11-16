//! Lagged correlation (Tools > Temporal dynamics > Lagged correlation), as Zeit's `lagged_correlation`: the correlation
//! of every pixel's series with a driver (a map series of another grid, one series of a file, or the mean of the
//! driver's window) at several lags, a significance that accounts for the series' autocorrelation (the effective number
//! of pairs of Bretherton et al. 1999) and the false discovery rate of Benjamini and Hochberg (1995) over the map.
//!
//! At lag `k` the pairs are `(x_t, y_{t-k})`: the driver leads. Everything but the false discovery rate is pixel-local, so
//! the window is read in strips of rows, the correlation computed by zeit-core's `lagged` and every output but the
//! last two written as the strip is done; the best lag's adjusted p-value is kept (float32, four bytes a pixel) for the
//! second pass, which writes `q` and `significant` without reading anything again. The driver is a warped VRT of the other
//! series on the run's grid (GDALWarp, nearest neighbour or bilinear), read strip by strip like x.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gdal::Dataset;
use serde_json::{Value, json};
use zeit_core::anomaly::{
    AnomalySettings, Seasonal, anomalies_cube, civil_from_days, days_from_civil, days_in_month, monthly_means,
};
use zeit_core::correlation::{Best, Lag, LagSettings, Match, Method, YRef, fdr_bh, lagged, match_index};

use crate::job::{
    ChunkOut, Estimate, Inputs, JobResult, JobSpec, OtherSeries, Outputs, active_outputs, for_strips, read,
    rows_per_chunk,
};
use crate::manifest::{Output, Tool};
use crate::pixel::{Params, PixelInput};
use crate::warp::{Format, Grid, WarpOptions, warp};

const NO_DRIVER: &str = "ValueError: choose the driver series (another open series: its grid and CRS may differ), or \
                         a CSV file of the driver in 'The driver is'";

/// What a run decides, from the tool's parameters.
#[derive(Clone, Debug)]
struct Settings {
    lags: Vec<i32>,
    /// calendar months (both averaged to months first), else the dates of x
    month: bool,
    matching: Match,
    anomaly: Option<AnomalySettings>,
    lag: LagSettings,
    alpha: f64,
    resampling: &'static str,
    mode: Mode,
}

/// Where the driver comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// each pixel's own series on the run's grid
    Pixel,
    /// the mean of the driver series over the window, one series for every pixel
    WindowMean,
    /// a CSV of dates and values
    File,
}

fn settings(p: &Params) -> Result<Settings, String> {
    let lags = p.int("lags");
    if !(0..=120).contains(&lags) {
        return Err(format!("ValueError: lags must be between 0 and 120, got {lags}"));
    }
    let alpha = p.f64("alpha_fdr");
    if !(alpha > 0.0 && alpha < 1.0) {
        return Err(format!("ValueError: alpha_fdr must be in (0, 1), got {alpha}"));
    }
    let tolerance = p.f64("tolerance");
    if tolerance.is_nan() || tolerance < 0.0 {
        return Err(format!("ValueError: the tolerance must be at least 0 days, got {tolerance}"));
    }
    let matching = match p.str("match") {
        "exact" => Match::Exact,
        "month" => Match::SameMonth,
        "year" => Match::SameYear,
        _ => Match::Nearest(tolerance),
    };
    let anomaly = match p.str("anomaly") {
        "none" => None,
        "mean" => Some(Seasonal::Mean),
        "harmonic" => Some(Seasonal::Harmonic { order: 2 }),
        _ => Some(Seasonal::Monthly { min_per_month: 2 }),
    }
    .map(|seasonal| AnomalySettings { seasonal, base: None, detrend: false, standardize: false });
    let lag = LagSettings {
        method: if p.str("method") == "spearman" { Method::Spearman } else { Method::Pearson },
        min_pairs: p.int("min_pairs").max(1) as usize,
        effective_n: p.bool("effective_n"),
        best: match p.str("best") {
            "positive" => Best::Positive,
            "negative" => Best::Negative,
            _ => Best::Absolute,
        },
        sidak: p.bool("sidak"),
    };
    let mode = match p.str("y_mode") {
        "window_mean" => Mode::WindowMean,
        "file" => Mode::File,
        _ => Mode::Pixel,
    };
    Ok(Settings {
        lags: (0..=lags as i32).collect(),
        month: p.str("step") != "dates",
        matching,
        anomaly,
        lag,
        alpha,
        resampling: if p.str("resampling") == "bilinear" { "bilinear" } else { "near" },
        mode,
    })
}

/// The labels of the bands of `r_lags`: one for each lag.
pub(crate) fn bands(_o: &Output, p: &Params, _input: &PixelInput) -> Result<Vec<String>, String> {
    let s = settings(p)?;
    let unit = if s.month { ["month", "months"] } else { ["date", "dates"] };
    Ok(s.lags.iter().map(|&k| format!("lag {k} {}", unit[usize::from(k != 1)])).collect())
}

// ---------------------------------------------------------------------------------------------------------------
// The driver
// ---------------------------------------------------------------------------------------------------------------

/// A calendar date from a CSV: "2020-03-15", "2020/3/15", "2020-03" (the first of the month) or with a time after it.
fn parse_date(text: &str) -> Option<i64> {
    let head = text.trim().trim_matches('"');
    let head = head.split(['T', ' ']).next()?;
    let parts: Vec<&str> = head.split(['-', '/']).collect();
    let num = |s: &str| s.trim().parse::<u32>().ok();
    let year: i32 = parts.first()?.trim().parse().ok().filter(|y| (1..=9999).contains(y))?;
    let month = num(parts.get(1)?)?;
    let day = match parts.get(2) {
        Some(d) => num(d)?,
        None => 1,
    };
    if parts.len() > 3 || !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    Some(days_from_civil(year, month, day))
}

/// The dates and values of a CSV (a header line, if any, is skipped; the values empty or "NA" are missing), by date.
fn parse_series(text: &str) -> Result<(Vec<i64>, Vec<f64>), String> {
    let mut rows: Vec<(i64, f64)> = vec![];
    for (n, line) in text.lines().enumerate() {
        let line = line.trim().trim_start_matches('\u{feff}');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = if line.contains([',', ';', '\t']) {
            line.split([',', ';', '\t']).collect()
        } else {
            line.split_whitespace().collect()
        };
        let Some(date) = fields.first().and_then(|f| parse_date(f)) else {
            if rows.is_empty() {
                continue; // a header
            }
            return Err(format!("ValueError: line {}: '{}' is not a date (2020-03-15 or 2020-03)", n + 1, fields[0]));
        };
        let value = fields.get(1).map(|v| v.trim().trim_matches('"')).unwrap_or("");
        let v = match value.to_ascii_lowercase().as_str() {
            "" | "na" | "nan" | "null" | "none" => f64::NAN,
            s => s.parse::<f64>().map_err(|_| format!("ValueError: line {}: '{value}' is not a number", n + 1))?,
        };
        rows.push((date, if v.is_finite() { v } else { f64::NAN }));
    }
    if rows.len() < 3 {
        return Err("ValueError: the driver's CSV needs at least 3 rows of a date and a value".into());
    }
    rows.sort_by_key(|r| r.0);
    Ok(rows.into_iter().unzip())
}

/// The driver's series on the run's grid and window: a warped VRT.
struct Driver {
    ds: Dataset,
    days: Vec<i64>,
    path: PathBuf,
}

fn driver_days(other: &OtherSeries) -> Result<Vec<i64>, String> {
    match &other.days {
        Some(d) if d.len() == other.years.len() && !d.is_empty() => {
            if d.windows(2).any(|w| w[1] < w[0]) {
                return Err("ValueError: the driver series' dates must be in ascending order".into());
            }
            Ok(d.clone())
        }
        _ => Err("ValueError: the driver series needs real dates (to be matched to the series' dates)".into()),
    }
}

/// The driver's first band of each date, warped onto the grid of `input`.
fn warped_driver(other: &OtherSeries, input: &Dataset, dir: &Path, s: &Settings) -> Result<Driver, String> {
    let src = other.inputs.first().ok_or(NO_DRIVER)?;
    let days = driver_days(other)?;
    let (w, h) = input.raster_size();
    let gt = input.geo_transform().map_err(|e| format!("ValueError: the series has no geotransform ({e})"))?;
    let wkt = input.projection();
    let opts = WarpOptions {
        grid: Some(Grid { wkt, gt, w, h }),
        resampling: s.resampling.into(),
        dst_nodata: Some(f64::NAN),
        format: Format::Vrt,
        threads: 1,
        ..Default::default()
    };
    let path = dir.join("lagged_correlation_driver.vrt");
    warp(src, &path.to_string_lossy(), &opts, &|_| true)
        .map_err(|e| format!("ValueError: the driver series could not be put on the series' grid: {e}"))?;
    let ds = Dataset::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if ds.raster_count() != days.len() {
        return Err(format!(
            "ValueError: the driver has {} bands but {} dates were given",
            ds.raster_count(),
            days.len()
        ));
    }
    Ok(Driver { ds, days, path })
}

/// The mean of the driver over the window, at each date (NaN where none of its pixels has a value).
fn window_mean(
    d: &Driver,
    window: [usize; 4],
    rows: usize,
    cancel: &AtomicBool,
    progress: &dyn Fn(f64),
) -> Result<Vec<f64>, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let t = d.days.len();
    let (mut sum, mut count) = (vec![0.0; t], vec![0usize; t]);
    let mut r = 0;
    while r < h {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let a = read(&d.ds, 1..=t, x0, y0 + r, w, hh, false)?;
        for (i, plane) in a.chunks(w * hh).enumerate() {
            for v in plane.iter().filter(|v| v.is_finite()) {
                sum[i] += v;
                count[i] += 1;
            }
        }
        r += hh;
        progress(r as f64 / h as f64);
    }
    Ok(sum.iter().zip(&count).map(|(s, &c)| if c > 0 { s / c as f64 } else { f64::NAN }).collect())
}

// ---------------------------------------------------------------------------------------------------------------
// The axis of the correlation: months or dates
// ---------------------------------------------------------------------------------------------------------------

/// The calendar month of a day as `year * 12 + month - 1`.
fn month_of(day: i64) -> i64 {
    let (y, m, _) = civil_from_days(day);
    i64::from(y) * 12 + i64::from(m) - 1
}

/// The 15th of the month `index` (`year * 12 + month - 1`), as days since 1970-01-01.
fn mid_month(index: i64) -> i64 {
    days_from_civil(index.div_euclid(12) as i32, index.rem_euclid(12) as u32 + 1, 15)
}

/// The contiguous months from `first` to `last`.
struct Months {
    first: i64,
    count: usize,
    /// the 15th of each, the dates the anomalies of a monthly series are made on
    mid: Vec<i64>,
}

impl Months {
    fn new(x_days: &[i64], y_days: &[i64]) -> Months {
        let all = x_days.iter().chain(y_days).map(|&d| month_of(d));
        let (first, last) = all.fold((i64::MAX, i64::MIN), |(a, b), m| (a.min(m), b.max(m)));
        let count = (last - first + 1) as usize;
        Months { first, count, mid: (0..count as i64).map(|i| mid_month(first + i)).collect() }
    }

    /// `values` (`t` dates x `n` pixels) as monthly means on the grid of months, NaN where a month has none.
    fn aggregate(&self, values: &[f64], t: usize, n: usize, days: &[i64]) -> Vec<f64> {
        let (months, means) = monthly_means(values, t, n, days);
        let mut out = vec![f64::NAN; self.count * n];
        for (k, &m) in months.iter().enumerate() {
            let row = (m - self.first) as usize;
            out[row * n..(row + 1) * n].copy_from_slice(&means[k * n..(k + 1) * n]);
        }
        out
    }
}

/// What the correlation runs over, decided once: the rows of x and y, and each lag's row of y for each row of x.
struct Axis {
    /// x's dates (in months mode, the 15th of each month of the grid are the dates of the anomalies)
    months: Option<Months>,
    x_days: Vec<i64>,
    y_days: Vec<i64>,
    /// lags x rows of x
    index: Vec<Vec<i32>>,
    /// the rows of x's strip after the axis is applied
    rows_x: usize,
    rows_y: usize,
}

impl Axis {
    fn new(s: &Settings, x_days: &[i64], y_days: &[i64]) -> Axis {
        if s.month {
            let m = Months::new(x_days, y_days);
            let count = m.count;
            let index = s
                .lags
                .iter()
                .map(|&k| {
                    (0..count as i64)
                        .map(|i| {
                            if i - i64::from(k) >= 0 && i - i64::from(k) < count as i64 {
                                (i - i64::from(k)) as i32
                            } else {
                                -1
                            }
                        })
                        .collect()
                })
                .collect();
            Axis {
                months: Some(m),
                x_days: x_days.to_vec(),
                y_days: y_days.to_vec(),
                index,
                rows_x: count,
                rows_y: count,
            }
        } else {
            let index = s.lags.iter().map(|&k| match_index(x_days, y_days, s.matching, Lag::Steps(k))).collect();
            Axis {
                months: None,
                x_days: x_days.to_vec(),
                y_days: y_days.to_vec(),
                index,
                rows_x: x_days.len(),
                rows_y: y_days.len(),
            }
        }
    }

    /// A series' (`t` dates x `n` pixels) anomalies on this axis: the monthly means then the anomalies of the months,
    /// or the anomalies of the dates.
    fn prepare(&self, mut values: Vec<f64>, n: usize, y: bool, s: &Settings, n_jobs: i32) -> Vec<f64> {
        let days = if y { &self.y_days } else { &self.x_days };
        let t = days.len();
        let (mut v, rows, on) = match &self.months {
            Some(m) => (m.aggregate(&values, t, n, days), m.count, &m.mid),
            None => (std::mem::take(&mut values), t, days),
        };
        if let Some(a) = &s.anomaly {
            anomalies_cube(&mut v, rows, n, on, a, n_jobs);
        }
        v
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------------------------

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    let s = settings(p)?;
    let x_days = spec
        .days
        .clone()
        .filter(|d| d.len() == spec.years.len())
        .ok_or("ValueError: a lagged correlation needs a series with real dates")?;
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    if t < 12 {
        return Err(format!("ValueError: a lagged correlation needs at least 12 dates, the series has {t}"));
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let rows = rows_per_chunk(tool, w);

    // ---- the driver
    let mut driver: Option<Driver> = None;
    let (y_days, series): (Vec<i64>, Option<Vec<f64>>) = match s.mode {
        Mode::File => {
            let path = p.str("y_file");
            if path.is_empty() {
                return Err("ValueError: choose the driver's CSV file".into());
            }
            let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
            let (d, v) = parse_series(&text)?;
            (d, Some(v))
        }
        Mode::Pixel | Mode::WindowMean => {
            let other = spec.other.as_ref().ok_or(NO_DRIVER)?;
            progress(0.0, "putting the driver on the series' grid");
            let d = warped_driver(other, inputs.input.ds(), &dir, &s)?;
            let days = d.days.clone();
            let mean = if s.mode == Mode::WindowMean {
                Some(window_mean(&d, window, rows, cancel, &|f| {
                    progress(0.1 * f, "the driver's mean over the window")
                })?)
            } else {
                None
            };
            driver = Some(d);
            (days, mean)
        }
    };
    let axis = Axis::new(&s, &x_days, &y_days);
    let n_jobs = spec.n_jobs();
    // the driver as one series for every pixel, on the axis (months or dates) with its anomalies
    let y_series: Option<Vec<f64>> = series.map(|v| axis.prepare(v, 1, true, &s, n_jobs));

    // ---- the outputs: the first pass writes all but `q` and `significant`
    let wanted = active_outputs(tool, p);
    let (late, early): (Vec<Output>, Vec<Output>) =
        wanted.iter().cloned().partition(|o| o.id == "q" || o.id == "significant");
    let mut outs = Outputs::new(tool, &early, &dir, inputs.input.ds(), window, spec)?;
    let nl = s.lags.len();
    let mut best_p = vec![f32::NAN; n];
    let base = if s.mode == Mode::WindowMean { 0.1 } else { 0.0 };
    for_strips(&mut inputs, window, rows, cancel, |r, hh, chunk| {
        let np = chunk.pixels();
        let xs = axis.prepare(chunk.stack, np, false, &s, n_jobs);
        let ys: Option<Vec<f64>> = match (&driver, &y_series) {
            (_, Some(_)) => None,
            (Some(d), None) => {
                let raw = read(&d.ds, 1..=d.days.len(), x0, y0 + r, w, hh, false)?;
                Some(axis.prepare(raw, np, true, &s, n_jobs))
            }
            (None, None) => return Err(NO_DRIVER.into()),
        };
        let y_ref = match (&ys, &y_series) {
            (Some(c), _) => YRef::Cube(c, axis.rows_y),
            (None, Some(v)) => YRef::Series(v),
            (None, None) => return Err(NO_DRIVER.into()),
        };
        let l = lagged(&xs, axis.rows_x, np, y_ref, &axis.index, &s.lag, n_jobs);
        let mut res = ChunkOut::new();
        let none = |k: usize| l.best_lag[k] < 0;
        res.insert("best_r".into(), l.best_r.clone());
        res.insert(
            "best_lag".into(),
            (0..np).map(|k| if none(k) { f64::NAN } else { f64::from(l.best_lag[k]) }).collect(),
        );
        res.insert("best_p".into(), l.best_p.clone());
        res.insert(
            "n_eff".into(),
            (0..np).map(|k| if none(k) { f64::NAN } else { l.n_eff[l.best_lag[k] as usize * np + k] }).collect(),
        );
        res.insert("r_lags".into(), l.r.clone());
        debug_assert_eq!(l.r.len(), nl * np);
        outs.write(&res, r, hh)?;
        for (k, &v) in l.best_p.iter().enumerate() {
            best_p[r * w + k] = v as f32;
        }
        progress(base + (0.85 - base) * (r + hh) as f64 / h as f64, &format!("rows {}/{h}", r + hh));
        Ok(())
    })?;

    // ---- the false discovery rate of the best lags, over the whole window
    progress(0.85, "the false discovery rate");
    let p64: Vec<f64> = best_p.iter().map(|&v| f64::from(v)).collect();
    drop(best_p);
    let (q, p_star) = fdr_bh(&p64, s.alpha);
    drop(p64);
    let mut late_outs = Outputs::new(tool, &late, &dir, inputs.input.ds(), window, spec)?;
    let mut r = 0;
    while r < h {
        let hh = rows.min(h - r);
        let part = &q[r * w..(r + hh) * w];
        let mut res = ChunkOut::new();
        res.insert("q".into(), part.to_vec());
        res.insert(
            "significant".into(),
            part.iter()
                .map(|&v| {
                    if v.is_nan() {
                        f64::NAN
                    } else if v <= s.alpha {
                        2.0
                    } else {
                        1.0
                    }
                })
                .collect(),
        );
        late_outs.write(&res, r, hh)?;
        r += hh;
        progress(0.85 + 0.15 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }

    // ---- what the app shows
    let unit = if s.month { "months" } else { "dates" };
    let model = json!({ "outputs": {
        "best_lag": { "unit": unit },
        "q": { "p_threshold": p_star, "alpha_fdr": s.alpha },
        "significant": { "p_threshold": p_star, "alpha_fdr": s.alpha },
        "r_lags": { "lags": s.lags, "unit": unit },
    }});
    let mut described: Vec<Value> = outs.describe(p, Some(&model));
    described.extend(late_outs.describe(p, Some(&model)));
    let order = |v: &Value| wanted.iter().position(|o| Some(o.id.as_str()) == v["id"].as_str()).unwrap_or(usize::MAX);
    described.sort_by_key(order);
    drop(outs);
    drop(late_outs);
    if let Some(d) = driver {
        let path = d.path.clone();
        drop(d);
        let _ = std::fs::remove_file(path);
    }
    progress(1.0, "done");
    Ok(JobResult { outputs: described, window })
}

/// The run's time: reading the series (and the driver's), then each lag's correlation of every pixel over the axis's
/// rows (Pearson about 20 ns a pair of values, Spearman's ranks about three times that), shared among the threads.
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let s = settings(p)?;
    let t = spec.years.len().max(1) as f64;
    let ty = spec.other.as_ref().map_or(t, |o| o.years.len().max(1) as f64);
    let threads = match spec.n_jobs() {
        n if n > 0 => n as f64,
        _ => std::thread::available_parallelism().map_or(4.0, |n| n.get() as f64),
    };
    let per_pair = if s.lag.method == Method::Spearman { 6e-8 } else { 2e-8 };
    let compute = per_pair * t * s.lags.len() as f64 / threads.min(8.0);
    let reading = 2e-8 * t + if s.mode == Mode::Pixel { 2e-8 * ty } else { 0.0 };
    Ok(Estimate { sec_per_px: compute + reading + 5e-8 * t, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(v: Value) -> Params {
        let given = v.as_object().unwrap().iter().map(|(k, x)| (k.clone(), x.clone())).collect();
        Params::with_defaults("lagged_correlation", &given).unwrap()
    }

    #[test]
    fn a_csv_of_dates_and_values_is_read_with_or_without_a_header() {
        let (d, v) = parse_series("date,oni\n2020-01,0.5\n2020-02-15;NA\n2020-03,-1.25\n2019-12,0.1\n").unwrap();
        assert_eq!(
            d,
            vec![
                days_from_civil(2019, 12, 1),
                days_from_civil(2020, 1, 1),
                days_from_civil(2020, 2, 15),
                days_from_civil(2020, 3, 1)
            ]
        );
        assert_eq!(v[0], 0.1);
        assert!(v[2].is_nan());
        assert_eq!(v[3], -1.25);
        let (d, _) = parse_series("2020/1/5 1\n2020/1/6 2\n2020/1/7 3\n").unwrap();
        assert_eq!(d[0], days_from_civil(2020, 1, 5));
        assert!(parse_series("2020-01,1\n2020-02,x\n2020-03,3\n").is_err());
        assert!(parse_series("2020-01,1\n2020-13,2\n2020-03,3\n").is_err());
        assert!(parse_series("2020-01,1\n").is_err());
    }

    #[test]
    fn the_months_axis_covers_both_series_and_the_lags_shift_the_driver_back() {
        let s = settings(&params(json!({ "lags": 2 }))).unwrap();
        let x: Vec<i64> = (0..24).map(|m| days_from_civil(2020 + m / 12, (m % 12) as u32 + 1, 10)).collect();
        let y: Vec<i64> = (0..30).map(|m| days_from_civil(2019 + (m + 6) / 12, ((m + 6) % 12) as u32 + 1, 1)).collect();
        let a = Axis::new(&s, &x, &y);
        let m = a.months.as_ref().unwrap();
        // y starts in July 2019 and runs 30 months, x covers January 2020 to December 2021
        assert_eq!(m.count, 30);
        assert_eq!(a.index.len(), 3);
        assert_eq!(a.index[0][5], 5);
        assert_eq!(a.index[2][5], 3);
        assert_eq!(a.index[2][1], -1);
        assert_eq!(m.mid[0], days_from_civil(2019, 7, 15));
    }

    #[test]
    fn a_months_lag_of_the_labels_says_month_or_months() {
        let o = Output::default();
        let l = bands(&o, &params(json!({ "lags": 2 })), &PixelInput::default()).unwrap();
        assert_eq!(l, vec!["lag 0 months", "lag 1 month", "lag 2 months"]);
        let l = bands(&o, &params(json!({ "lags": 1, "step": "dates" })), &PixelInput::default()).unwrap();
        assert_eq!(l, vec!["lag 0 dates", "lag 1 date"]);
    }

    #[test]
    fn the_settings_are_checked() {
        assert!(settings(&params(json!({ "alpha_fdr": 0.0 }))).is_err());
        assert!(settings(&params(json!({ "lags": 500 }))).is_err());
        let s =
            settings(&params(json!({ "method": "spearman", "best": "negative", "match": "month", "anomaly": "none" })))
                .unwrap();
        assert_eq!((s.lag.method, s.lag.best, s.matching), (Method::Spearman, Best::Negative, Match::SameMonth));
        assert!(s.anomaly.is_none());
    }
}
