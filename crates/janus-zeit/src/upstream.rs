//! What Zeit 2.0 still does in Python and Janus needs, written to move into zeit-core as it is (RUST_MIGRATION.md:
//! "a lógica de algoritmo vai para o zeit-core"). Each function says the Python it reproduces, bit for bit.

/// How `extract_events` picks a pixel's event among its segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortBy {
    Greatest = 1,
    Newest = 2,
    Fastest = 3,
    Longest = 4,
    Dsnr = 5,
}

impl SortBy {
    pub fn parse(s: &str) -> Option<SortBy> {
        Some(match s.to_ascii_lowercase().as_str() {
            "greatest" => SortBy::Greatest,
            "newest" => SortBy::Newest,
            "fastest" => SortBy::Fastest,
            "longest" => SortBy::Longest,
            "dsnr" => SortBy::Dsnr,
            _ => return None,
        })
    }
}

/// One pixel's event (zeros: none), in the kernel's types.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Event {
    pub yod: u16,
    pub magnitude: f32,
    pub duration: u16,
    pub pre_val: f32,
    pub post_val: f32,
    pub rate: f32,
    pub dsnr: f32,
}

/// zeit/metrics.py `_extract_py` on one pixel: the event of LandTrendr's vertices (years, then values; 0 past
/// the last vertex, the values' NaN as 0) a pixel keeps. In float32 as the numba kernel, the thresholds in
/// float64.
#[allow(clippy::too_many_arguments)]
pub fn landtrendr_event(
    years: &[f32],
    values: &[f32],
    rmse: f32,
    is_loss: bool,
    sort_by: SortBy,
    min_mag: f64,
    min_dur: f64,
    pre_thresh: f64,
) -> Event {
    let mut best_score = -999999.0f64;
    let mut out = Event::default();
    let max_v = years.len();
    for i in 0..max_v.saturating_sub(1) {
        let (start_year, end_year) = (years[i], years[i + 1]);
        if start_year == 0.0 || end_year == 0.0 {
            break; // no more valid vertices
        }
        let duration = end_year - start_year;
        if duration <= 0.0 {
            continue;
        }
        let (pre_val, post_val) = (values[i], values[i + 1]);
        let magnitude = if is_loss {
            if pre_thresh > 0.0 && f64::from(pre_val) < pre_thresh {
                continue;
            }
            pre_val - post_val
        } else {
            if pre_thresh > 0.0 && f64::from(pre_val) > pre_thresh {
                continue;
            }
            post_val - pre_val
        };
        if magnitude <= 0.0 || f64::from(magnitude) < min_mag {
            continue; // a flat segment is no event
        }
        if f64::from(duration) < min_dur {
            continue;
        }
        let rate = magnitude / duration;
        let dsnr = if rmse > 0.0 { magnitude / rmse } else { 0.0 };
        let score = f64::from(match sort_by {
            SortBy::Greatest => magnitude,
            SortBy::Newest => start_year,
            SortBy::Fastest => rate,
            SortBy::Longest => duration,
            SortBy::Dsnr => dsnr,
        });
        if score > best_score {
            best_score = score;
            out = Event { yod: start_year as u16, magnitude, duration: duration as u16, pre_val, post_val, rate, dsnr };
        }
    }
    out
}

/// numpy's sum of a contiguous float64 array (`np.sum`: pairwise_sum, 8 accumulators in blocks of up to 128).
pub fn np_sum(a: &[f64]) -> f64 {
    pairwise(a)
}

fn pairwise(a: &[f64]) -> f64 {
    {
        let n = a.len();
        if n < 8 {
            a.iter().fold(-0.0, |s, &v| s + v)
        } else if n <= 128 {
            let mut r = [0.0; 8];
            r.copy_from_slice(&a[..8]);
            let mut i = 8;
            while i < n - n % 8 {
                for j in 0..8 {
                    r[j] += a[i + j];
                }
                i += 8;
            }
            let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
            while i < n {
                res += a[i];
                i += 1;
            }
            res
        } else {
            let mut n2 = n / 2;
            n2 -= n2 % 8;
            pairwise(&a[..n2]) + pairwise(&a[n2..])
        }
    }
}

/// `np.interp(x, xp, fp)` (xp increasing): linear between the known points, their end values beyond.
pub fn np_interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if x.is_nan() {
        return x;
    }
    if x < xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    // the last j with xp[j] <= x
    let j = xp.partition_point(|&v| v <= x) - 1;
    if x == xp[j] {
        return fp[j];
    }
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    let r = slope * (x - xp[j]) + fp[j];
    if r.is_nan() {
        let r2 = slope * (x - xp[j + 1]) + fp[j + 1];
        if r2.is_nan() && fp[j] == fp[j + 1] {
            return fp[j];
        }
        return r2;
    }
    r
}

/// The least-squares polynomial of `order` through (t, v), evaluated at `at` (Householder QR on the
/// Vandermonde of t scaled to [-1, 1]: numpy's polyfit / scipy's savgol fit the same polynomial).
fn poly_fit_eval(t: &[f64], v: &[f64], order: usize, at: &[f64]) -> Vec<f64> {
    let (m, k) = (t.len(), order + 1);
    let (lo, hi) = (t[0], t[m - 1]);
    let (mid, half) = ((lo + hi) * 0.5, ((hi - lo) * 0.5).max(1e-300));
    let s = |x: f64| (x - mid) / half;
    // columns of the m x k design matrix, then QR in place
    let mut a: Vec<Vec<f64>> = (0..k).map(|j| t.iter().map(|&x| s(x).powi(j as i32)).collect()).collect();
    let mut b = v.to_vec();
    for j in 0..k {
        let norm = a[j][j..].iter().map(|x| x * x).sum::<f64>().sqrt();
        if norm == 0.0 {
            continue;
        }
        let alpha = if a[j][j] > 0.0 { -norm } else { norm };
        let mut u: Vec<f64> = a[j][j..].to_vec();
        u[0] -= alpha;
        let un = u.iter().map(|x| x * x).sum::<f64>();
        if un == 0.0 {
            continue;
        }
        for col in a.iter_mut().skip(j) {
            let d = 2.0 * u.iter().zip(&col[j..]).map(|(p, q)| p * q).sum::<f64>() / un;
            for (c, &ui) in col[j..].iter_mut().zip(&u) {
                *c -= d * ui;
            }
        }
        let d = 2.0 * u.iter().zip(&b[j..]).map(|(p, q)| p * q).sum::<f64>() / un;
        for (c, &ui) in b[j..].iter_mut().zip(&u) {
            *c -= d * ui;
        }
    }
    // R c = Q^T b
    let mut c = vec![0.0; k];
    for j in (0..k).rev() {
        let mut r = b[j];
        for (i, ci) in c.iter().enumerate().skip(j + 1) {
            r -= a[i][j] * ci;
        }
        c[j] = if a[j][j] != 0.0 { r / a[j][j] } else { 0.0 };
    }
    at.iter().map(|&x| c.iter().rev().fold(0.0, |acc, &cj| acc * s(x) + cj)).collect()
}

/// `scipy.signal.savgol_filter(v, window, polyorder)` (mode "interp"): each value the local polynomial's at
/// its centre, the first and last window/2 values those of the polynomial fitted to the first and last window.
/// The same polynomials as scipy's, to rounding (scipy solves them with LAPACK).
pub fn savgol(v: &[f64], window: usize, polyorder: usize) -> Vec<f64> {
    let n = v.len();
    let half = window / 2;
    let t: Vec<f64> = (0..window).map(|i| i as f64).collect();
    // the centre's weights: the fit of each unit vector, evaluated at the centre
    let weights: Vec<f64> = (0..window)
        .map(|i| {
            let mut e = vec![0.0; window];
            e[i] = 1.0;
            poly_fit_eval(&t, &e, polyorder, &[half as f64])[0]
        })
        .collect();
    let mut out = vec![0.0; n];
    for i in half..n - half {
        out[i] = weights.iter().zip(&v[i - half..]).map(|(w, x)| w * x).sum();
    }
    let at: Vec<f64> = (0..half).map(|i| i as f64).collect();
    out[..half].copy_from_slice(&poly_fit_eval(&t, &v[..window], polyorder, &at));
    let at: Vec<f64> = (window - half..window).map(|i| i as f64).collect();
    out[n - half..].copy_from_slice(&poly_fit_eval(&t, &v[n - window..], polyorder, &at));
    out
}

/// The endmembers of Souza et al. (2005) for Landsat's blue, green, red, NIR, SWIR1 and SWIR2, with CODED's
/// cloud (zeit/_sma.py SOUZA_2005, reflectance x 10000), in the table's order.
pub const SOUZA_2005: [(&str, [f64; 6]); 5] = [
    ("gv", [500.0, 900.0, 400.0, 6100.0, 3000.0, 1000.0]),
    ("shade", [0.0; 6]),
    ("npv", [1400.0, 1700.0, 2200.0, 3000.0, 5500.0, 3000.0]),
    ("soil", [2000.0, 3000.0, 3400.0, 5800.0, 6000.0, 5800.0]),
    ("cloud", [9000.0, 9600.0, 8000.0, 7800.0, 7200.0, 6500.0]),
];

/// The fractions of every observation (float32, as zeit.unmix's Dataset), NaN where it was left out.
#[derive(Clone, Debug, Default)]
pub struct Fractions {
    pub gv: Vec<f32>,
    pub shade: Vec<f32>,
    pub npv: Vec<f32>,
    pub soil: Vec<f32>,
    pub cloud: Vec<f32>,
    pub rmse: Vec<f32>,
    pub ndfi: Vec<f32>,
}

impl Fractions {
    pub fn get(&self, name: &str) -> &[f32] {
        match name {
            "gv" => &self.gv,
            "shade" => &self.shade,
            "npv" => &self.npv,
            "soil" => &self.soil,
            "cloud" => &self.cloud,
            "rmse" => &self.rmse,
            _ => &self.ndfi,
        }
    }
}

/// `zeit.unmix(cube, "souza2005", scale=10000.0, cloud_threshold=...)` (zeit/_sma.py) of each observation:
/// `bands` blue..swir2 (reflectance x 10000, one value per observation each), fully constrained; the NDFI of
/// Souza et al. (2005) in float32, every fraction NaN where the cloud fraction is above `cloud_threshold`.
pub fn unmix_souza(bands: &[Vec<f64>], cloud_threshold: Option<f64>) -> Fractions {
    let t = bands.first().map_or(0, Vec::len);
    // the table in reflectance (x 10000 / 10000), endmember x band
    let e: Vec<f64> = SOUZA_2005.iter().flat_map(|(_, v)| v.iter().map(|x| x / 10000.0)).collect();
    let flat: Vec<f64> = (0..t).flat_map(|i| bands.iter().map(move |b| b[i] / 10000.0)).collect();
    let out = zeit_core::sma::unmix_batch(&flat, t, 6, &e, 5, true, true, 1000.0, 1);
    let col = |j: usize| -> Vec<f32> { (0..t).map(|i| out[i * 6 + j] as f32).collect() };
    let mut f =
        Fractions { gv: col(0), shade: col(1), npv: col(2), soil: col(3), cloud: col(4), rmse: col(5), ndfi: vec![] };
    // ndfi_of: in float32, GVs = GV / (1 - shade)
    f.ndfi = (0..t)
        .map(|i| {
            let gvs = f.gv[i] / (1.0 - f.shade[i]);
            let v = (gvs - (f.npv[i] + f.soil[i])) / (gvs + f.npv[i] + f.soil[i]);
            if v.is_finite() { v } else { f32::NAN }
        })
        .collect();
    if let Some(ct) = cloud_threshold {
        // the threshold compared in float32 (numpy's weak Python scalar)
        let ct = ct as f32;
        for i in 0..t {
            if !(f.cloud[i] <= ct) {
                for v in [&mut f.gv, &mut f.shade, &mut f.npv, &mut f.soil, &mut f.cloud, &mut f.rmse, &mut f.ndfi] {
                    v[i] = f32::NAN;
                }
            }
        }
    }
    f
}
