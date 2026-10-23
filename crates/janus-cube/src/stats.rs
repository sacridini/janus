//! Statistics of a pixel's series and of an ROI's values at a date (src/stats.cpp), NaN values left out. The
//! series' x is the time in years (or the index), so slopes come out per year.

/// The k-th smallest of `v` (reordered), as std::nth_element leaves it.
fn nth(v: &mut [f64], k: usize) -> f64 {
    *v.select_nth_unstable_by(k, f64::total_cmp).1
}

/// The median as the C++ takes it: the middle element, and for an even count its mean with the largest of the
/// lower half.
fn median(v: &mut [f64]) -> f64 {
    let n = v.len();
    let m = nth(v, n / 2);
    if n.is_multiple_of(2) { 0.5 * (m + v[..n / 2].iter().copied().fold(f64::NEG_INFINITY, f64::max)) } else { m }
}

/// The `q_lo` and `q_hi` quantiles of `data` from about `max_samples` of its values taken evenly, NaN left out
/// (src/stats.cpp, samplePercentiles): 0-1 without values, `hi` above `lo`.
pub fn sample_percentiles(data: &[f32], max_samples: usize, q_lo: f64, q_hi: f64) -> (f32, f32) {
    let step = (data.len() / max_samples.max(1)).max(1);
    let mut s: Vec<f32> = data.iter().step_by(step).copied().filter(|v| !v.is_nan()).collect();
    if s.is_empty() {
        return (0.0, 1.0);
    }
    let mut q = |p: f64| {
        let k = (p * (s.len() - 1) as f64) as usize;
        *s.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let (lo, mut hi) = (q(q_lo), q(q_hi));
    if hi.is_nan() || hi <= lo {
        hi = lo + 1e-6;
    }
    (lo, hi)
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SeriesStats {
    pub n: usize,
    pub mean: f64,
    pub median: f64,
    pub std: f64,
    pub cv: f64,
    pub min: f64,
    pub max: f64,
    pub arg_min: Option<usize>,
    pub arg_max: Option<usize>,
    pub ols_slope: f64,
    pub ols_intercept: f64,
    pub r2: f64,
    pub sen_slope: f64,
    pub sen_intercept: f64,
    /// Largest decrease between consecutive valid observations (previous - next, > 0) and the date of the later
    /// one; 0 and none if it never decreases, NaN with fewer than 2 observations. The GPU statistics' rule.
    pub drop_mag: f64,
    pub arg_drop: Option<usize>,
}

/// The largest drop alone (see SeriesStats::drop_mag).
pub fn largest_drop(v: &[f32]) -> (f64, Option<usize>) {
    let (mut mag, mut at, mut n, mut last) = (0.0, None, 0, 0f32);
    for (i, &x) in v.iter().enumerate() {
        if x.is_nan() {
            continue;
        }
        if n > 0 && f64::from(last) - f64::from(x) > mag {
            mag = f64::from(last) - f64::from(x);
            at = Some(i);
        }
        n += 1;
        last = x;
    }
    if n < 2 { (f64::NAN, at) } else { (mag, at) }
}

pub fn series_stats(x_all: &[f64], v_all: &[f32]) -> SeriesStats {
    let mut s = SeriesStats::default();
    let (mut x, mut v, mut idx) = (Vec::new(), Vec::new(), Vec::new());
    for (i, (&xi, &vi)) in x_all.iter().zip(v_all).enumerate() {
        if !vi.is_nan() {
            x.push(xi);
            v.push(f64::from(vi));
            idx.push(i);
        }
    }
    let n = v.len();
    s.n = n;
    (s.drop_mag, s.arg_drop) = largest_drop(v_all);
    if n == 0 {
        return s;
    }
    let (mut sum, mut sx) = (0.0, 0.0);
    (s.min, s.max) = (v[0], v[0]);
    (s.arg_min, s.arg_max) = (Some(idx[0]), Some(idx[0]));
    for i in 0..n {
        sum += v[i];
        sx += x[i];
        if v[i] < s.min {
            s.min = v[i];
            s.arg_min = Some(idx[i]);
        }
        if v[i] > s.max {
            s.max = v[i];
            s.arg_max = Some(idx[i]);
        }
    }
    s.mean = sum / n as f64;
    let mx = sx / n as f64;
    s.median = median(&mut v.clone());
    let (mut sxx, mut sxy, mut syy) = (0.0, 0.0, 0.0);
    for i in 0..n {
        let (dx, dy) = (x[i] - mx, v[i] - s.mean);
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    s.std = if n > 1 { (syy / (n - 1) as f64).sqrt() } else { 0.0 };
    s.cv = if s.mean != 0.0 { s.std / s.mean.abs() } else { 0.0 };
    if sxx > 0.0 {
        s.ols_slope = sxy / sxx;
        s.ols_intercept = s.mean - s.ols_slope * mx;
        s.r2 = if syy > 0.0 { (sxy * sxy) / (sxx * syy) } else { 0.0 };
    }
    // Sen's slope: O(n²), trivial for series up to a few thousand (Mann-Kendall is a Zeit tool).
    if n >= 3 {
        let mut slopes = Vec::with_capacity(n * (n - 1) / 2);
        for i in 0..n - 1 {
            for j in i + 1..n {
                if x[j] != x[i] {
                    slopes.push((v[j] - v[i]) / (x[j] - x[i]));
                }
            }
        }
        if !slopes.is_empty() {
            s.sen_slope = median(&mut slopes);
            // the intercept: median of v - slope x, keeping Sen's robustness
            let mut r: Vec<f64> = (0..n).map(|i| v[i] - s.sen_slope * x[i]).collect();
            s.sen_intercept = nth(&mut r, n / 2);
        }
    }
    s
}

/// Quantiles a SampleStats keeps: every 2.5 % from the minimum to the maximum (2.5 % of the values between two
/// consecutive ones: their spacing is the values' density).
pub const QUANTILES: usize = 41;

/// Statistics of a set of values (an ROI at one date).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleStats {
    pub n: usize,
    pub mean: f32,
    pub std: f32,
    pub p10: f32,
    pub p25: f32,
    pub p50: f32,
    pub p75: f32,
    pub p90: f32,
    /// the k-th: the quantile k / (QUANTILES - 1)
    pub q: [f32; QUANTILES],
}

impl Default for SampleStats {
    fn default() -> Self {
        let nan = f32::NAN;
        SampleStats { n: 0, mean: nan, std: nan, p10: nan, p25: nan, p50: nan, p75: nan, p90: nan, q: [nan; QUANTILES] }
    }
}

/// Reorders `v` (its NaN values dropped).
pub fn sample_stats(v: &mut Vec<f32>) -> SampleStats {
    v.retain(|x| !x.is_nan());
    let n = v.len();
    if n == 0 {
        return SampleStats::default();
    }
    let mean = (v.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64) as f32;
    let sq: f64 = v.iter().map(|&x| f64::from(x - mean) * f64::from(x - mean)).sum();
    let std = if n > 1 { (sq / (n - 1) as f64).sqrt() as f32 } else { 0.0 };
    // the quantiles in increasing order, each selected among the values above the one before (p: the value at
    // index p (n - 1), rounded down)
    let mut q = [f32::NAN; QUANTILES];
    let mut lo = 0;
    for (k, x) in q.iter_mut().enumerate() {
        let i = (k as f64 / (QUANTILES - 1) as f64 * (n - 1) as f64) as usize;
        *x = *v[lo..].select_nth_unstable_by(i - lo, f32::total_cmp).1;
        lo = i;
    }
    SampleStats { n, mean, std, p10: q[4], p25: q[10], p50: q[20], p75: q[30], p90: q[36], q }
}

/// The civil year and the day of that year (1 January = 1) of a time in Unix seconds.
pub fn year_and_day(unix_seconds: f64) -> (i32, i32) {
    let days = (unix_seconds / 86400.0).floor() as i64;
    let (y, _, _) = crate::dates::civil_from_days(days);
    (y, (days - crate::dates::days_from_civil(y, 1, 1)) as i32 + 1)
}

/// The year and the day of a seasonal year that starts on day `start` (1: 1 January; e.g. 182, 1 July, a
/// hydrological year or a southern summer's crop seen whole): the year it starts in and the day from its start
/// (1 to 366).
pub fn season_year_and_day(unix_seconds: f64, start: i32) -> (i32, i32) {
    let (y, d) = year_and_day(unix_seconds);
    let d = d - (start.clamp(1, 366) - 1);
    if d < 1 { (y - 1, d + 366) } else { (y, d) }
}

/// Observations per year of a series of times (Unix seconds), from the median spacing; 1 for one or none.
pub fn observations_per_year(t: &[f64]) -> i32 {
    let mut gaps: Vec<f64> = t.windows(2).filter(|w| w[1] > w[0]).map(|w| w[1] - w[0]).collect();
    if gaps.is_empty() {
        return 1;
    }
    let k = gaps.len() / 2;
    let days = *gaps.select_nth_unstable_by(k, f64::total_cmp).1 / 86400.0;
    if days > 0.0 { ((365.25 / days).round() as i32).max(1) } else { 1 }
}

/// The calendar month (0 January to 11 December) of a time in Unix seconds.
pub fn month_of(unix_seconds: f64) -> usize {
    let (_, m, _) = crate::dates::civil_from_days((unix_seconds / 86400.0).floor() as i64);
    (m as usize).clamp(1, 12) - 1
}

/// The climatology of a dated series: the mean of each calendar month over every year, NaN for a month without
/// observations. A value minus its month's mean is its anomaly without the seasonal cycle.
pub fn monthly_climatology(t: &[f64], v: &[f32]) -> [f64; 12] {
    let (mut sum, mut n) = ([0f64; 12], [0usize; 12]);
    for (&x, &y) in t.iter().zip(v) {
        if y.is_finite() {
            let m = month_of(x);
            sum[m] += f64::from(y);
            n[m] += 1;
        }
    }
    std::array::from_fn(|m| if n[m] > 0 { sum[m] / n[m] as f64 } else { f64::NAN })
}

/// The running sum of `v` (CUSUM): NaN where `v` is, the sum going on past it.
pub fn cusum(v: &[f64]) -> Vec<f64> {
    let mut s = 0.0;
    v.iter()
        .map(|&x| {
            if x.is_finite() {
                s += x;
                s
            } else {
                f64::NAN
            }
        })
        .collect()
}

/// A series by year and part of the year: `bins` parts of 366 days, the mean of the observations in each cell; the
/// years start on day `start` (season_year_and_day).
#[derive(Clone, Debug, Default)]
pub struct SeasonalGrid {
    pub year0: i32,
    pub years: usize,
    pub bins: usize,
    /// [year][bin], NaN = no observation
    pub mean: Vec<f32>,
    /// [year][bin] observations
    pub n: Vec<usize>,
}

pub fn seasonal_grid(t: &[f64], v: &[f32], bins: usize, start: i32) -> SeasonalGrid {
    let bins = bins.max(1);
    let mut g = SeasonalGrid { bins, ..Default::default() };
    let yd: Vec<(i32, i32)> = t.iter().take(v.len()).map(|&x| season_year_and_day(x, start)).collect();
    let (Some(y0), Some(y1)) = (yd.iter().map(|d| d.0).min(), yd.iter().map(|d| d.0).max()) else { return g };
    g.year0 = y0;
    g.years = (y1 - y0 + 1) as usize;
    let mut sum = vec![0f64; g.years * bins];
    g.n = vec![0; sum.len()];
    for (&(y, d), &x) in yd.iter().zip(v) {
        if !x.is_finite() {
            continue;
        }
        let k = (y - y0) as usize * bins + ((d as usize - 1) * bins / 366).min(bins - 1);
        sum[k] += f64::from(x);
        g.n[k] += 1;
    }
    g.mean = sum.iter().zip(&g.n).map(|(&s, &n)| if n > 0 { (s / n as f64) as f32 } else { f32::NAN }).collect();
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++'s --selftest-ui stage 75 (selfTestAnalysis(0)).
    #[test]
    fn calendar() {
        for (t, y, d) in [
            (951868800.0, 2000, 61),
            (1009756800.0, 2001, 365),
            (1104451200.0, 2004, 366),
            (-86400.0, 1969, 365),
            (1262304000.0 + 43200.0, 2010, 1),
        ] {
            assert_eq!(year_and_day(t), (y, d), "{t}");
        }
        // monthly, 2009-2011, on the 15th: value = month + 100 x year index
        let start = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let jan1 = [1230768000.0, 1262304000.0, 1293840000.0];
        let (mut times, mut values) = (Vec::new(), Vec::new());
        for (y, j) in jan1.iter().enumerate() {
            for (m, s) in start.iter().enumerate() {
                times.push(j + f64::from(s + 14) * 86400.0);
                values.push((m + 1 + 100 * y) as f32);
            }
        }
        assert_eq!(observations_per_year(&times), 12);
        assert_eq!(observations_per_year(&jan1), 1);
        let g = seasonal_grid(&times, &values, 12, 1);
        assert_eq!((g.year0, g.years, g.bins), (2009, 3, 12));
        for y in 0..3 {
            for m in 0..12 {
                assert_eq!(g.mean[y * 12 + m], (m + 1 + 100 * y) as f32);
                assert_eq!(g.n[y * 12 + m], 1);
            }
        }
        // years from 1 July: 2008/09 (January to June 2009) to 2011/12; July first, January seventh
        assert_eq!(season_year_and_day(jan1[0], 182), (2008, 186));
        assert_eq!(season_year_and_day(jan1[0] + 181.0 * 86400.0, 182), (2009, 1));
        let g = seasonal_grid(&times, &values, 12, 182);
        assert_eq!((g.year0, g.years), (2008, 4));
        assert_eq!(g.mean[12], 7.0); // 2009/10's first part: July 2009
        assert_eq!(g.mean[12 + 6], 101.0); // and its seventh: January 2010
    }

    #[test]
    fn a_series() {
        let x = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0];
        let v = [1.0, 2.0, f32::NAN, 4.0, 3.0, 6.0];
        let s = series_stats(&x, &v);
        assert_eq!(s.n, 5);
        assert!((s.mean - 3.2).abs() < 1e-12);
        // even count would be the mean of the two middle values; odd: 3
        assert_eq!(s.median, 3.0);
        assert_eq!((s.arg_min, s.arg_max), (Some(0), Some(5)));
        assert_eq!((s.drop_mag, s.arg_drop), (1.0, Some(4)));
        // Sen: median of the 10 pairwise slopes
        assert!((s.sen_slope - 1.0).abs() < 1e-12);
        let (m, at) = largest_drop(&[f32::NAN, 2.0]);
        assert!(m.is_nan() && at.is_none());
    }

    #[test]
    fn climatology_and_cusum() {
        // monthly, 2009-2010, on the 15th: value = month (a seasonal cycle) + 10 the second year
        let start = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let (mut t, mut v) = (Vec::new(), Vec::new());
        for (y, jan1) in [1230768000.0, 1262304000.0].iter().enumerate() {
            for (m, s) in start.iter().enumerate() {
                t.push(jan1 + f64::from(s + 14) * 86400.0);
                v.push(m as f32 + 10.0 * y as f32);
            }
        }
        assert_eq!(month_of(t[0]), 0);
        assert_eq!(month_of(t[23]), 11);
        v[13] = f32::NAN; // February of the second year
        let c = monthly_climatology(&t, &v);
        assert_eq!(c[0], 5.0);
        assert_eq!(c[1], 1.0);
        // the anomalies: -5 the first year, +5 the second (the cycle gone); their running sum falls, then rises
        let a: Vec<f64> = t.iter().zip(&v).map(|(&x, &y)| f64::from(y) - c[month_of(x)]).collect();
        let s = cusum(&a);
        assert_eq!(s[11], -55.0);
        assert!(s[13].is_nan());
        assert_eq!(s[23], 0.0);
    }

    #[test]
    fn a_sample() {
        let mut v: Vec<f32> = (0..=100).map(|i| i as f32).chain([f32::NAN]).collect();
        let s = sample_stats(&mut v);
        assert_eq!((s.n, s.p10, s.p50, s.p90), (101, 10.0, 50.0, 90.0));
        assert_eq!(s.mean, 50.0);
        assert_eq!((s.q[0], s.q[1], s.q[39], s.q[40]), (0.0, 2.0, 97.0, 100.0));
        // two groups (a cleared half): the quantiles crowd at each, a gap between them
        let mut v: Vec<f32> = (0..1000).map(|i| if i % 2 == 0 { 0.2 } else { 0.8 } + (i % 7) as f32 * 0.001).collect();
        let s = sample_stats(&mut v);
        assert!(s.q[19] < 0.21 && s.q[21] > 0.79);
    }
}
