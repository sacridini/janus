//! A series' cycles in time (Analysis > Cycles): its continuous wavelet transform with the Morlet wavelet, after
//! Torrence & Compo (1998, "A practical guide to wavelet analysis", BAMS 79), as their code computes it. The dates
//! are put on a regular grid first (the series' usual step; a gap's values interpolated linearly, the columns across
//! a gap longer than two steps marked), the mean (and the linear trend) taken out. Power is |W|² over the series'
//! variance (white noise: 1 on average); the cone of influence is where the series' ends weigh on it; a power is
//! significant at 95 % above red noise (an AR(1) process of the series' lag-1 autocorrelation). The global spectrum
//! is the power's mean over time, its 95 % level with the degrees of freedom of a time average (their eq. 23).

use std::f64::consts::PI;

const DAY: f64 = 86400.0;
/// Days a year (the axis' labels and the tests).
pub const YEAR_DAYS: f64 = 365.2425;
/// The Morlet wavelet's frequency, and its Fourier period for a scale of 1.
const OMEGA0: f64 = 6.0;
/// Scales per octave: 1 / DJ.
pub const DJ: f64 = 0.125;
/// The chi-square of 2 degrees of freedom at 95 %, over 2 (a local power's significance).
const CHI2_95_2: f64 = 5.991_464_547_107_979 / 2.0;
/// The decorrelation factor of the Morlet wavelet for time averages (their table 2).
const GAMMA: f64 = 2.32;

/// The series' scalogram: `power[j * n + i]` at scale j (period `periods[j]`) and step i.
#[derive(Clone, Debug)]
pub struct Scalogram {
    /// the grid: its first time (Unix seconds), its step (seconds) and length
    pub t0: f64,
    pub dt: f64,
    pub n: usize,
    /// each scale's Fourier period, in days (2^(j DJ) apart)
    pub periods: Vec<f64>,
    pub power: Vec<f32>,
    /// each scale's power at the 95 % level of red noise (power above: significant)
    pub signif: Vec<f64>,
    /// at each step: the period (days) above which the series' ends weigh on the power
    pub coi: Vec<f64>,
    /// the steps whose value was interpolated across a gap longer than two steps (or held past the series' ends)
    pub filled: Vec<bool>,
    /// the power's mean over time at each scale, and its 95 % level
    pub global: Vec<f64>,
    pub global_signif: Vec<f64>,
    /// the lag-1 autocorrelation of the red noise
    pub alpha: f64,
    /// the series' variance (power's unit) and how many dates had a value
    pub variance: f64,
    pub observations: usize,
}

impl Scalogram {
    pub fn at(&self, j: usize, i: usize) -> f32 {
        self.power[j * self.n + i]
    }

    /// The time (Unix seconds) of step i.
    pub fn time(&self, i: usize) -> f64 {
        self.t0 + i as f64 * self.dt
    }
}

/// The Morlet wavelet's Fourier period for a scale of 1.
fn fourier_factor() -> f64 {
    4.0 * PI / (OMEGA0 + (2.0 + OMEGA0 * OMEGA0).sqrt())
}

/// In-place FFT of `re` + i `im` (length a power of 2); `inverse` divides by the length, as numpy's ifft.
fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let a = sign * 2.0 * PI / len as f64;
        let (wr, wi) = (a.cos(), a.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0, 0.0);
            for k in 0..len / 2 {
                let (p, q) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[q] * cr - im[q] * ci, re[q] * ci + im[q] * cr);
                (re[q], im[q]) = (re[p] - tr, im[p] - ti);
                re[p] += tr;
                im[p] += ti;
                (cr, ci) = (cr * wr - ci * wi, cr * wi + ci * wr);
            }
        }
        len <<= 1;
    }
    if inverse {
        let f = 1.0 / n as f64;
        re.iter_mut().chain(im.iter_mut()).for_each(|v| *v *= f);
    }
}

/// ln Γ(a), Lanczos' (g = 7, 9 terms), for a > 0.
fn ln_gamma(a: f64) -> f64 {
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    let a = a - 1.0;
    let t = a + 7.5;
    let s = C[0] + C[1..].iter().enumerate().map(|(k, c)| c / (a + k as f64 + 1.0)).sum::<f64>();
    0.5 * (2.0 * std::f64::consts::PI).ln() + (a + 0.5) * t.ln() - t + s.ln()
}

/// The regularized lower incomplete gamma P(a, x): its series below a + 1, its continued fraction above.
fn gamma_p(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let front = (a * x.ln() - x - ln_gamma(a)).exp();
    if x < a + 1.0 {
        let (mut term, mut sum) = (1.0 / a, 1.0 / a);
        for k in 1..500 {
            term *= x / (a + k as f64);
            sum += term;
            if term.abs() < sum.abs() * 1e-16 {
                break;
            }
        }
        sum * front
    } else {
        // Lentz's
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for k in 1..500 {
            let an = -(k as f64) * (k as f64 - a);
            b += 2.0;
            d = an * d + b;
            d = if d.abs() < tiny { tiny } else { d };
            c = b + an / c;
            c = if c.abs() < tiny { tiny } else { c };
            d = 1.0 / d;
            let step = d * c;
            h *= step;
            if (step - 1.0).abs() < 1e-16 {
                break;
            }
        }
        1.0 - front * h
    }
}

/// The chi-square's quantile at 95 % over its degrees of freedom: P(dof / 2, q / 2) = 0.95 solved by bisection.
fn chi2_95_over_dof(dof: f64) -> f64 {
    if (dof - 2.0).abs() < 1e-12 {
        return CHI2_95_2;
    }
    let a = dof / 2.0;
    let (mut lo, mut hi) = (0.0, dof + 20.0 * dof.sqrt() + 20.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if gamma_p(a, mid / 2.0) < 0.95 {
            lo = mid;
        } else {
            hi = mid;
        }
        if hi - lo < 1e-13 * hi {
            break;
        }
    }
    0.5 * (lo + hi) / dof
}

/// The series on a regular grid of the dates' usual step: (first time, step, values, filled), the values between
/// two observations interpolated linearly, held past the first and last.
pub fn regular_grid(times: &[f64], values: &[f32]) -> Option<(f64, f64, Vec<f64>, Vec<bool>)> {
    let obs: Vec<(f64, f64)> =
        times.iter().zip(values).filter(|(_, v)| v.is_finite()).map(|(&t, &v)| (t, f64::from(v))).collect();
    if obs.len() < 2 {
        return None;
    }
    let mut gaps: Vec<f64> = times.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).collect();
    if gaps.is_empty() {
        return None;
    }
    // the usual gap: the mean of those within half of the median (months of 28 to 31 days: 30.4, not 31), the step
    // then fitted for the grid to end on the last date
    let k = gaps.len() / 2;
    let median = *gaps.select_nth_unstable_by(k, f64::total_cmp).1;
    let usual: Vec<f64> = gaps.iter().copied().filter(|g| (g - median).abs() <= 0.5 * median).collect();
    let mean = (usual.iter().sum::<f64>() / usual.len().max(1) as f64).max(DAY);
    let (t0, t1) = (times[0], times[times.len() - 1]);
    let n = ((t1 - t0) / mean).round() as usize + 1;
    let dt = if n > 1 { (t1 - t0) / (n - 1) as f64 } else { mean };
    let (mut x, mut filled) = (Vec::with_capacity(n), Vec::with_capacity(n));
    let mut k = 0;
    for i in 0..n {
        let t = t0 + i as f64 * dt;
        while k + 1 < obs.len() && obs[k + 1].0 <= t {
            k += 1;
        }
        let (a, b) = (obs[k], obs[(k + 1).min(obs.len() - 1)]);
        if t <= obs[0].0 {
            x.push(obs[0].1);
            filled.push(obs[0].0 - t > dt);
        } else if t >= obs[obs.len() - 1].0 {
            x.push(obs[obs.len() - 1].1);
            filled.push(t - obs[obs.len() - 1].0 > dt);
        } else {
            let f = if b.0 > a.0 { (t - a.0) / (b.0 - a.0) } else { 0.0 };
            x.push(a.1 + f * (b.1 - a.1));
            // a date missing is a gap of two steps: not marked
            filled.push(b.0 - a.0 > 2.5 * dt);
        }
    }
    Some((t0, dt, x, filled))
}

/// Why the cycles of a series of these dates cannot be shown, if they cannot: a few dates a year and two years
/// at least.
pub fn unsuitable(times: &[f64], values: &[f32]) -> Option<String> {
    let per_year = janus_cube::stats::observations_per_year(times);
    if per_year < 4 {
        return Some(format!(
            "About {per_year} observation{} a year: no cycle within a year to see. Cycles need a few dates a year \
             (monthly or 16-day composites, Sentinel-2, MODIS...).",
            if per_year == 1 { "" } else { "s" }
        ));
    }
    let span = (times[times.len() - 1] - times[0]) / DAY / YEAR_DAYS;
    if span < 2.0 {
        return Some(format!("{span:.1} years of dates: cycles need two years at least."));
    }
    let valid = values.iter().filter(|v| v.is_finite()).count();
    if valid < 16 || (valid as f64) < 0.3 * values.len() as f64 {
        return Some(format!("{valid} of {} dates with a value: too few.", values.len()));
    }
    None
}

/// The scalogram of a series (`times` in Unix seconds, `values` NaN where there is none), the linear trend taken
/// out too if `detrend`; periods from two steps to half the series' length.
pub fn scalogram(times: &[f64], values: &[f32], detrend: bool) -> Result<Scalogram, String> {
    if let Some(why) = unsuitable(times, values) {
        return Err(why);
    }
    let (t0, dt, mut x, filled) = regular_grid(times, values).ok_or("too few dates with a value")?;
    let n = x.len();
    // the mean (and the trend) out
    let mean = x.iter().sum::<f64>() / n as f64;
    x.iter_mut().for_each(|v| *v -= mean);
    if detrend {
        let tm = (n as f64 - 1.0) / 2.0;
        let (sxy, sxx) = x
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(a, b), (i, &v)| (a + (i as f64 - tm) * v, b + (i as f64 - tm).powi(2)));
        let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        x.iter_mut().enumerate().for_each(|(i, v)| *v -= slope * (i as f64 - tm));
    }
    // as their code: the sample variance
    let variance = x.iter().map(|v| v * v).sum::<f64>() / (n as f64 - 1.0).max(1.0);
    if variance <= 0.0 {
        return Err("the series is constant: no cycle".into());
    }
    let (num, den) = x.windows(2).fold((0.0, 0.0), |(a, b), w| (a + w[0] * w[1], b + w[0] * w[0]));
    let alpha = if den > 0.0 { (num / den).clamp(0.0, 0.95) } else { 0.0 };
    // zero-padded to the power of 2 above the nearest one (their pad = 1: no wrap-around), transformed once
    let m = 1usize << (((n as f64).log2() + 0.4999).trunc() as u32 + 1);
    let (mut fre, mut fim) = (x.clone(), vec![0.0; m]);
    fre.resize(m, 0.0);
    fft(&mut fre, &mut fim, false);
    let omega: Vec<f64> = (0..m)
        .map(|k| {
            let k = if k <= m / 2 { k as f64 } else { k as f64 - m as f64 };
            2.0 * PI * k / (m as f64 * dt)
        })
        .collect();
    let ff = fourier_factor();
    let s0 = 2.0 * dt;
    let span = (n - 1) as f64 * dt;
    let scales: Vec<f64> =
        (0..).map(|j| s0 * 2f64.powf(j as f64 * DJ)).take_while(|s| s * ff <= 0.5 * span.max(s0 * ff)).collect();
    let mut power = vec![0f32; scales.len() * n];
    let norm = PI.powf(-0.25);
    let (mut wre, mut wim) = (vec![0.0; m], vec![0.0; m]);
    for (j, &s) in scales.iter().enumerate() {
        let a = (2.0 * PI * s / dt).sqrt() * norm;
        for k in 0..m {
            // the daughter wavelet: positive frequencies only
            let d = if omega[k] > 0.0 { a * (-(s * omega[k] - OMEGA0).powi(2) / 2.0).exp() } else { 0.0 };
            (wre[k], wim[k]) = (fre[k] * d, fim[k] * d);
        }
        fft(&mut wre, &mut wim, true);
        for i in 0..n {
            power[j * n + i] = ((wre[i] * wre[i] + wim[i] * wim[i]) / variance) as f32;
        }
    }
    let periods: Vec<f64> = scales.iter().map(|s| s * ff / DAY).collect();
    // red noise at each scale's frequency (cycles a step), at 95 %
    let red = |s: f64| {
        let f = dt / (s * ff);
        (1.0 - alpha * alpha) / (1.0 + alpha * alpha - 2.0 * alpha * (2.0 * PI * f).cos())
    };
    let signif: Vec<f64> = scales.iter().map(|&s| red(s) * CHI2_95_2).collect();
    let global: Vec<f64> = (0..scales.len())
        .map(|j| power[j * n..(j + 1) * n].iter().map(|&p| f64::from(p)).sum::<f64>() / n as f64)
        .collect();
    let global_signif: Vec<f64> = scales
        .iter()
        .map(|&s| {
            let dof = (2.0 * (1.0 + (n as f64 * dt / (GAMMA * s)).powi(2)).sqrt()).max(2.0);
            red(s) * chi2_95_over_dof(dof)
        })
        .collect();
    let coi: Vec<f64> = (0..n)
        .map(|i| {
            let e = i.min(n - 1 - i) as f64;
            ff / 2f64.sqrt() * dt * e.max(1e-5) / DAY
        })
        .collect();
    let observations = values.iter().filter(|v| v.is_finite()).count();
    Ok(Scalogram {
        t0,
        dt,
        n,
        periods,
        power,
        signif,
        coi,
        filled,
        global,
        global_signif,
        alpha,
        variance,
        observations,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monthly dates (the 15th) over `years` years from 2010, as Unix seconds.
    fn monthly(years: usize) -> Vec<f64> {
        (0..years * 12)
            .map(|k| {
                let (y, m) = (2010 + (k / 12) as i32, (k % 12) as u32 + 1);
                janus_cube::dates::days_from_civil(y, m, 15) as f64 * DAY
            })
            .collect()
    }

    /// A deterministic noise in -1..1.
    fn noise(k: usize) -> f64 {
        let mut h = (k as u64).wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51afd7ed558ccd);
        h ^= h >> 33;
        (h % 2_000_001) as f64 / 1_000_000.0 - 1.0
    }

    fn peak(s: &Scalogram) -> f64 {
        let j = (0..s.global.len()).max_by(|&a, &b| s.global[a].total_cmp(&s.global[b])).unwrap();
        s.periods[j]
    }

    #[test]
    fn chi_square_quantiles() {
        // scipy.stats.chi2.ppf(0.95, dof)
        for (dof, q) in [
            (1.0, 3.841_458_820_694_124),
            (2.5, 6.928_076_113_452_421),
            (3.0, 7.814_727_903_251_179),
            (7.3, 14.502_546_139_547_098),
            (10.0, 18.307_038_053_275_146),
            (100.0, 124.342_113_404_004_07),
            (1000.0, 1_074.679_448_803_441),
        ] {
            let ours = chi2_95_over_dof(dof) * dof;
            assert!((ours - q).abs() < 1e-9 * q, "{dof}: {ours} against {q}");
        }
    }

    #[test]
    fn fft_round_trip() {
        let x: Vec<f64> = (0..16).map(noise).collect();
        let (mut re, mut im) = (x.clone(), vec![0.0; 16]);
        fft(&mut re, &mut im, false);
        // the sum at frequency 0
        assert!((re[0] - x.iter().sum::<f64>()).abs() < 1e-9 && im[0].abs() < 1e-9);
        fft(&mut re, &mut im, true);
        for (a, b) in re.iter().zip(&x) {
            assert!((a - b).abs() < 1e-9);
        }
    }

    #[test]
    fn a_yearly_cycle_peaks_at_a_year() {
        let t = monthly(10);
        let v: Vec<f32> = t.iter().map(|&t| (2.0 * PI * t / (YEAR_DAYS * DAY)).sin() as f32).collect();
        let s = scalogram(&t, &v, true).unwrap();
        let p = peak(&s);
        assert!((p / YEAR_DAYS).log2().abs() <= DJ, "peak at {p} days");
        // significant there, away from the ends
        let j = s.periods.iter().position(|&q| q == p).unwrap();
        assert!(s.at(j, s.n / 2) as f64 > s.signif[j]);
        assert!(s.global[j] > s.global_signif[j]);
    }

    #[test]
    fn two_cycles_a_year_from_when_they_start() {
        // a yearly cycle, then from the fifth year a half-yearly one too (a second crop)
        let t = monthly(10);
        let v: Vec<f32> = t
            .iter()
            .enumerate()
            .map(|(k, &t)| {
                let yr = t / (YEAR_DAYS * DAY);
                let half = if k >= 60 { (4.0 * PI * yr).sin() } else { 0.0 };
                ((2.0 * PI * yr).sin() + half) as f32
            })
            .collect();
        let s = scalogram(&t, &v, true).unwrap();
        let j = (0..s.periods.len()).min_by(|&a, &b| {
            (s.periods[a] / (YEAR_DAYS / 2.0)).log2().abs().total_cmp(&(s.periods[b] / (YEAR_DAYS / 2.0)).log2().abs())
        });
        let j = j.unwrap();
        let before = s.at(j, s.n / 4);
        let after = s.at(j, 3 * s.n / 4);
        assert!(after > 10.0 * before, "half-yearly power before {before}, after {after}");
    }

    #[test]
    fn white_noise_has_power_one() {
        let t = monthly(40);
        let v: Vec<f32> = (0..t.len()).map(|k| noise(k) as f32).collect();
        let s = scalogram(&t, &v, false).unwrap();
        let mean = s.global.iter().sum::<f64>() / s.global.len() as f64;
        assert!((0.5..1.6).contains(&mean), "mean power {mean}");
        assert!(s.alpha < 0.2, "alpha {}", s.alpha);
    }

    #[test]
    fn gaps_and_ends() {
        let t = monthly(6);
        let mut v: Vec<f32> = t.iter().map(|&t| (2.0 * PI * t / (YEAR_DAYS * DAY)).cos() as f32).collect();
        // five months of clouds, and the first date missing
        for x in &mut v[20..25] {
            *x = f32::NAN;
        }
        v[0] = f32::NAN;
        let (_, dt, x, filled) = regular_grid(&t, &v).unwrap();
        // a step a month (30.4 days, not the median's 31), the grid as long as the dates
        assert!((dt / DAY - 30.44).abs() < 0.2, "{} days", dt / DAY);
        assert_eq!(x.len(), t.len());
        assert!(filled[22] && !filled[10] && !filled[40]);
        assert!(x.iter().all(|v| v.is_finite()));
        let s = scalogram(&t, &v, true).unwrap();
        // the cone: nothing at the ends, widest in the middle
        assert!(s.coi[0] < 1.0 && s.coi[s.n / 2] > s.coi[s.n / 4]);
    }

    #[test]
    fn unsuitable_series() {
        // yearly dates
        let t: Vec<f64> = (0..30).map(|y| janus_cube::dates::days_from_civil(1990 + y, 7, 1) as f64 * DAY).collect();
        let v = vec![1.0f32; 30];
        assert!(unsuitable(&t, &v).is_some_and(|w| w.contains("a year")));
        // a year of monthly dates
        let t = monthly(1);
        assert!(unsuitable(&t, &[1.0; 12]).is_some());
        assert!(unsuitable(&monthly(4), &[1.0; 48]).is_none());
    }

    /// The scalogram of the reference series written for tools/wavelet_reference.py (JANUS_WAVELET_DUMP: the
    /// file), which computes it again with numpy after Torrence & Compo's own code.
    #[test]
    #[ignore]
    fn dump_for_the_reference() {
        let Ok(path) = std::env::var("JANUS_WAVELET_DUMP") else { return };
        let t = monthly(12);
        let v: Vec<f32> = t
            .iter()
            .enumerate()
            .map(|(k, &t)| {
                let yr = t / (YEAR_DAYS * DAY);
                let half = if k >= 70 { 0.6 * (4.0 * PI * yr).sin() } else { 0.0 };
                let gap = (30..34).contains(&k) || k == 90;
                if gap { f32::NAN } else { ((2.0 * PI * yr).sin() + half + 0.4 * noise(k) + 0.01 * k as f64) as f32 }
            })
            .collect();
        let s = scalogram(&t, &v, true).unwrap();
        let json = serde_json::json!({
            "times": t, "values": v.iter().map(|&x| if x.is_finite() { Some(f64::from(x)) } else { None }).collect::<Vec<_>>(),
            "dt": s.dt, "n": s.n, "periods": s.periods, "power": s.power, "signif": s.signif, "coi": s.coi,
            "global": s.global, "global_signif": s.global_signif, "alpha": s.alpha, "variance": s.variance,
        });
        std::fs::write(path, json.to_string()).unwrap();
    }
}
