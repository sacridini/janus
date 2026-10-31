//! The Phase mode's numbers on the CPU, as display.wgsl's `harmonic` and `hsv` compute them: the map's status bar,
//! its histogram and the tests read them.

/// A series' annual harmonic `v = m + a cos(θ) + b sin(θ)`, θ the date's angle in its year (`times`: years from
/// the first date, NaN a date left out; `offset` the first date's fraction of its year), fitted by least squares over the dates with a value: the peak as a fraction of the year,
/// the peak-to-peak amplitude and the mean. None with fewer than 6 dates or dates not spread over the year.
pub fn harmonic(times: &[f32], offset: f32, value: impl Fn(usize) -> f32) -> Option<[f32; 3]> {
    let (mut v0, mut c) = (0.0f64, 0.0f64);
    let mut s = [0.0f64; 8]; // cos, sin, cos², sin², cos·sin, v, v·cos, v·sin
    for (t, &y) in times.iter().enumerate() {
        let v = value(t);
        if y.is_nan() || v.is_nan() {
            continue;
        }
        if c == 0.0 {
            v0 = f64::from(v);
        }
        let v = f64::from(v) - v0;
        // the sum in f32, as the shader's
        let th = std::f64::consts::TAU * f64::from(y + offset).rem_euclid(1.0);
        let (si, co) = th.sin_cos();
        c += 1.0;
        for (k, x) in [co, si, co * co, si * si, co * si, v, v * co, v * si].into_iter().enumerate() {
            s[k] += x;
        }
    }
    if c < 6.0 {
        return None;
    }
    let s = s.map(|x| x / c);
    let m = [[1.0, s[0], s[1]], [s[0], s[2], s[4]], [s[1], s[4], s[3]]];
    let r = [s[5], s[6], s[7]];
    let det = |m: [[f64; 3]; 3]| {
        m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
    };
    let d = det(m);
    if d.is_nan() || d < 0.02 {
        return None;
    }
    // Cramer's rule: the matrix is symmetric, column k replaced by r
    let with = |k: usize| {
        let mut a = m;
        for (i, row) in a.iter_mut().enumerate() {
            row[k] = r[i];
        }
        det(a) / d
    };
    let (mean, a, b) = (with(0), with(1), with(2));
    let peak = (b.atan2(a) / std::f64::consts::TAU + 1.0).rem_euclid(1.0);
    Some([peak as f32, (2.0 * a.hypot(b)) as f32, (mean + v0) as f32])
}

/// HSV (each 0-1) to RGB (0-1), as the shader's.
pub fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    [5.0f32, 3.0, 1.0].map(|k| {
        let p = (k + h * 6.0).rem_euclid(6.0);
        v - v * s * p.min(4.0 - p).clamp(0.0, 1.0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harmonic_finds_the_peak() {
        // 23 dates a year over 3 years, a peak on day 250 (of 365.25), a mean of 0.5 and an amplitude of 0.6
        // (the first date on day 110)
        let (offset, peak) = (110.0 / 365.25, 250.0 / 365.25);
        let times: Vec<f32> = (0..69).map(|i| i as f32 / 23.0).collect();
        let v = |t: usize| {
            0.5 + 0.3 * (std::f32::consts::TAU * ((times[t] + offset).rem_euclid(1.0) - peak)).cos()
                + if t.is_multiple_of(5) { f32::NAN } else { 0.0 }
        };
        let [p, amp, mean] = harmonic(&times, offset, v).unwrap();
        assert!((p - peak).abs() < 1e-4, "{p} vs {peak}");
        assert!((amp - 0.6).abs() < 1e-4, "{amp}");
        assert!((mean - 0.5).abs() < 1e-4, "{mean}");
        // dates in one month only: no fit
        let one: Vec<f32> = (0..10).map(|i| i as f32 * 0.002).collect();
        assert!(harmonic(&one, 0.5, |_| 0.4).is_none());
        // the hue circle's thirds
        assert_eq!(hsv(0.0, 1.0, 1.0), [1.0, 0.0, 0.0]);
        assert_eq!(hsv(1.0 / 3.0, 1.0, 1.0).map(|x| x.round()), [0.0, 1.0, 0.0]);
        assert_eq!(hsv(0.5, 0.0, 0.5), [0.5, 0.5, 0.5]);
    }
}
