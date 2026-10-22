//! numpy's pieces the bridge's glue uses: least squares (`np.linalg.lstsq`), `np.linspace`, decimal years and
//! the regular time axis of Zeit's BFAST functions.

/// `np.linalg.lstsq(a, b, rcond=None)[0]`: the minimum-norm least-squares solution of a x = b (`a`: m rows of k
/// values), singular values below eps * max(m, k) * the largest left out, as LAPACK's gelsd. By a one-sided
/// Jacobi SVD: the same solution to rounding.
pub fn lstsq(a: &[Vec<f64>], b: &[f64]) -> Vec<f64> {
    let m = a.len();
    let k = a.first().map_or(0, Vec::len);
    if m == 0 || k == 0 {
        return vec![0.0; k];
    }
    // columns of A (u) and the rotations (v): A V = U S
    let mut u: Vec<Vec<f64>> = (0..k).map(|j| a.iter().map(|r| r[j]).collect()).collect();
    let mut v: Vec<Vec<f64>> = (0..k).map(|j| (0..k).map(|i| f64::from(u8::from(i == j))).collect()).collect();
    for _sweep in 0..60 {
        let mut rotated = false;
        for p in 0..k {
            for q in p + 1..k {
                let (alpha, beta, gamma) =
                    u[p].iter().zip(&u[q]).fold((0.0, 0.0, 0.0), |(a, b, g), (x, y)| (a + x * x, b + y * y, g + x * y));
                if gamma == 0.0 || gamma.abs() <= 1e-15 * (alpha * beta).sqrt() {
                    continue;
                }
                rotated = true;
                let zeta = (beta - alpha) / (2.0 * gamma);
                let t = zeta.signum() / (zeta.abs() + (1.0 + zeta * zeta).sqrt());
                let t = if zeta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (1.0 + t * t).sqrt();
                let s = c * t;
                let rotate = |w: &mut [Vec<f64>]| {
                    let (lo, hi) = w.split_at_mut(q);
                    for (x, y) in lo[p].iter_mut().zip(hi[0].iter_mut()) {
                        (*x, *y) = (c * *x - s * *y, s * *x + c * *y);
                    }
                };
                rotate(&mut u);
                rotate(&mut v);
            }
        }
        if !rotated {
            break;
        }
    }
    let sigma: Vec<f64> = u.iter().map(|c| c.iter().map(|x| x * x).sum::<f64>().sqrt()).collect();
    let smax = sigma.iter().copied().fold(0.0, f64::max);
    let cut = f64::EPSILON * m.max(k) as f64 * smax;
    // x = V S^-1 U^T b over the kept singular values
    let mut x = vec![0.0; k];
    for j in 0..k {
        if sigma[j] <= cut || sigma[j] == 0.0 {
            continue;
        }
        let coef = u[j].iter().zip(b).map(|(ui, bi)| ui * bi).sum::<f64>() / (sigma[j] * sigma[j]);
        for i in 0..k {
            x[i] += v[j][i] * coef;
        }
    }
    x
}

/// `np.linspace(start, stop, num)`.
pub fn linspace(start: f64, stop: f64, num: usize) -> Vec<f64> {
    if num == 0 {
        return vec![];
    }
    if num == 1 {
        return vec![start];
    }
    let div = (num - 1) as f64;
    let delta = stop - start;
    let step = delta / div;
    let mut y: Vec<f64> =
        (0..num).map(|i| if step == 0.0 { i as f64 / div * delta + start } else { i as f64 * step + start }).collect();
    y[num - 1] = stop;
    y
}

/// Days since 1970-01-01 of January 1st of `year`.
pub fn year_start(year: i64) -> i64 {
    // Howard Hinnant's days_from_civil, for March 1st based years
    let y = year - 1;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = 306; // January 1st is day 306 of the March-based year before
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

pub fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

/// A day as a decimal year (year + (day of the year - 1) / days in the year): zeit's fractional_years and
/// the bridge's decimal_years.
pub fn decimal_year(days: i64) -> f64 {
    let y = i64::from(crate::pixel::civil_year(days));
    let doy = days - year_start(y);
    y as f64 + doy as f64 / if is_leap(y) { 366.0 } else { 365.0 }
}

/// Days since 1970 of a decimal year, to the day (zeit/metrics.py `_decimal_year_days`).
pub fn decimal_year_days(v: f64) -> f64 {
    if !v.is_finite() {
        return f64::NAN;
    }
    let year = v.floor();
    let (start, end) = (year_start(year as i64), year_start(year as i64 + 1));
    start as f64 + ((v - year) * (end - start) as f64).round_ties_even()
}

/// The median of a sorted copy (np.median: the mean of the two middle values of an even count).
pub fn median(v: &[f64]) -> f64 {
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// The regular time axis of R's `ts` Zeit puts a dated series on (zeit/_series_api.py `_regular_time`): the
/// frequency from the median spacing of the dates, the start from the first date rounded to a step.
pub fn regular_time(days: &[i64]) -> Result<(f64, i32), String> {
    if days.len() < 2 {
        return Err("ValueError: a single date: pass frequency=".into());
    }
    let gaps: Vec<f64> = days.windows(2).map(|w| ((w[1] - w[0]) * 86400) as f64).collect();
    let step = median(&gaps) / 86400.0;
    let frequency = ((365.25 / step).round_ties_even() as i64).max(1);
    let first = decimal_year(days[0]);
    let year = first.floor();
    let start = year + ((first - year) * frequency as f64).round_ties_even() / frequency as f64;
    Ok((start, frequency as i32))
}
