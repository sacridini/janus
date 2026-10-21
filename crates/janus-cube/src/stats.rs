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

/// Statistics of a set of values (an ROI at one date).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SampleStats {
    pub n: usize,
    pub mean: f32,
    pub std: f32,
    pub p10: f32,
    pub p25: f32,
    pub p50: f32,
    pub p75: f32,
    pub p90: f32,
}

/// Reorders `v` (its NaN values dropped).
pub fn sample_stats(v: &mut Vec<f32>) -> SampleStats {
    v.retain(|x| !x.is_nan());
    let n = v.len();
    if n == 0 {
        let nan = f32::NAN;
        return SampleStats { n, mean: nan, std: nan, p10: nan, p25: nan, p50: nan, p75: nan, p90: nan };
    }
    let mean = (v.iter().map(|&x| f64::from(x)).sum::<f64>() / n as f64) as f32;
    let sq: f64 = v.iter().map(|&x| f64::from(x - mean) * f64::from(x - mean)).sum();
    let std = if n > 1 { (sq / (n - 1) as f64).sqrt() as f32 } else { 0.0 };
    let mut q = |p: f64| *v.select_nth_unstable_by((p * (n - 1) as f64) as usize, f32::total_cmp).1;
    SampleStats { n, mean, std, p10: q(0.10), p25: q(0.25), p50: q(0.50), p75: q(0.75), p90: q(0.90) }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_sample() {
        let mut v: Vec<f32> = (0..=100).map(|i| i as f32).chain([f32::NAN]).collect();
        let s = sample_stats(&mut v);
        assert_eq!((s.n, s.p10, s.p50, s.p90), (101, 10.0, 50.0, 90.0));
        assert_eq!(s.mean, 50.0);
    }
}
