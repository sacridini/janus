//! The direction of change: a PCA of the difference vectors e_t - e_t0 of the pixels that changed (cosine distance
//! above a threshold), the first three drawn as RGB, so kinds of change come apart in colour (forest to pasture, to
//! water...); and how much each pixel's change is like a pin's.

use std::sync::Mutex;
use std::time::Instant;

use rayon::ThreadPool;

use crate::eigen::top_eigen;
use crate::parallel_for;
use crate::simd;
use crate::store::EmbeddingStore;
use crate::views::par_chunks;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The principal directions of the changes of the year pairs fitted: the same colour for the same kind of change in
/// every pair.
#[derive(Clone, Debug, Default)]
pub struct DirectionBasis {
    pub d: usize,
    /// the mean difference (d) and three components (3 x d, unit length, the largest loading positive)
    pub mean: Vec<f32>,
    pub comps: Vec<f32>,
    pub eig: Vec<f64>,
    pub total: f64,
    /// the cosine distance above which a pixel changed (given, or the automatic one)
    pub threshold: f32,
    /// the automatic threshold: the median of the distances plus 4 times their MAD (x 1.4826), the changes
    /// unusual for the layer
    pub auto: f32,
    /// differences fitted, the share of the pixels looked at that changed, the year pairs
    pub samples: usize,
    pub changed: f64,
    pub pairs: usize,
    /// each component's stretch (percentiles of the sample)
    pub lo: [f32; 3],
    pub hi: [f32; 3],
    pub ms: f64,
}

/// About 262144 pixels spread over the pairs (each pair from another offset), valid in both years: (year, year
/// compared with, pixel, cosine distance).
fn candidates(s: &EmbeddingStore, pairs: &[(usize, usize)]) -> Vec<(usize, usize, usize, f32)> {
    let d = s.d;
    let pairs: Vec<(usize, usize)> =
        pairs.iter().copied().filter(|&(t, t0)| t != t0 && s.year_ready(t) && s.year_ready(t0)).collect();
    let mut out = Vec::new();
    if pairs.is_empty() {
        return out;
    }
    let n = s.pixels();
    let step = (n * pairs.len()).div_ceil(262_144).max(1);
    let (mut a, mut b) = (vec![0f32; d], vec![0f32; d]);
    for &(t, t0) in &pairs {
        for p in (t % step..n).step_by(step) {
            if !s.valid(t, p) || !s.valid(t0, p) || s.norm(t, p) <= 0.0 || s.norm(t0, p) <= 0.0 {
                continue;
            }
            s.dequant(t, p, &mut a);
            s.dequant(t0, p, &mut b);
            let dot: f64 = a.iter().zip(&b).map(|(&x, &y)| f64::from(x) * f64::from(y)).sum();
            let dist = 1.0 - dot / (f64::from(s.norm(t, p)) * f64::from(s.norm(t0, p)));
            out.push((t, t0, p, dist as f32));
        }
    }
    out
}

/// The median of the distances plus 4 times their MAD (scaled to a standard deviation).
fn auto_threshold(c: &[(usize, usize, usize, f32)]) -> f32 {
    if c.is_empty() {
        return 0.1;
    }
    let mut v: Vec<f32> = c.iter().map(|x| x.3).collect();
    let m = v.len() / 2;
    let med = *v.select_nth_unstable_by(m, f32::total_cmp).1;
    let mut dev: Vec<f32> = v.iter().map(|x| (x - med).abs()).collect();
    let mad = *dev.select_nth_unstable_by(m, f32::total_cmp).1;
    med + 4.0 * 1.4826 * mad
}

/// Up to 65536 difference vectors (n x d) of the candidates above `threshold`, and the share of them above it.
fn sample_changes(s: &EmbeddingStore, c: &[(usize, usize, usize, f32)], threshold: f32) -> (Vec<f32>, f64) {
    let d = s.d;
    let above: Vec<&(usize, usize, usize, f32)> = c.iter().filter(|x| x.3 > threshold).collect();
    let changed = above.len();
    let share = if c.is_empty() { 0.0 } else { changed as f64 / c.len() as f64 };
    let keep = changed.min(65536);
    let mut out = Vec::with_capacity(keep * d);
    let (mut a, mut b) = (vec![0f32; d], vec![0f32; d]);
    for j in 0..keep {
        let &(t, t0, p, _) = above[j * changed / keep];
        s.dequant(t, p, &mut a);
        s.dequant(t0, p, &mut b);
        out.extend(a.iter().zip(&b).map(|(x, y)| x - y));
    }
    (out, share)
}

/// The direction basis of the year pairs `pairs` (year, year compared with); a negative `threshold`: the automatic
/// one.
pub fn fit_direction(
    s: &EmbeddingStore,
    pairs: &[(usize, usize)],
    threshold: f32,
    stretch_pct: f32,
    pool: &ThreadPool,
) -> DirectionBasis {
    let t0 = Instant::now();
    let d = s.d;
    let cand = candidates(s, pairs);
    let auto = auto_threshold(&cand);
    let threshold = if threshold < 0.0 { auto } else { threshold };
    let (x, changed) = sample_changes(s, &cand, threshold);
    let n = x.len() / d.max(1);
    let mut b = DirectionBasis { d, threshold, auto, samples: n, changed, pairs: pairs.len(), ..Default::default() };
    if n < 3 || d < 3 {
        b.ms = ms(t0);
        return b;
    }
    let mean: Vec<f64> = (0..d).map(|i| (0..n).map(|j| f64::from(x[j * d + i])).sum::<f64>() / n as f64).collect();
    // the covariance in double, the rows split among the threads
    let cov = Mutex::new(vec![0f64; d * d]);
    parallel_for(pool, n, 2048, &|lo, hi| {
        let (mut local, mut c) = (vec![0f64; d * d], vec![0f64; d]);
        for j in lo..hi {
            for i in 0..d {
                c[i] = f64::from(x[j * d + i]) - mean[i];
            }
            for r in 0..d {
                for q in r..d {
                    local[r * d + q] += c[r] * c[q];
                }
            }
        }
        let mut cv = cov.lock().unwrap();
        for (a, v) in cv.iter_mut().zip(&local) {
            *a += v;
        }
    });
    let mut cd = cov.into_inner().unwrap();
    for r in 0..d {
        for q in r..d {
            cd[r * d + q] /= (n - 1) as f64;
            cd[q * d + r] = cd[r * d + q];
        }
    }
    b.total = (0..d).map(|i| cd[i * d + i]).sum();
    let (vecs, eig, _) = top_eigen(&cd, d, 3);
    b.eig = eig;
    b.mean = mean.iter().map(|&m| m as f32).collect();
    b.comps = vec![0.0; 3 * d];
    for c in 0..3 {
        let v = &vecs[c * d..(c + 1) * d];
        let big = (0..d).fold(0, |m, i| if v[i].abs() > v[m].abs() { i } else { m });
        let sign = if v[big] < 0.0 { -1.0 } else { 1.0 };
        for i in 0..d {
            b.comps[c * d + i] = (sign * v[i]) as f32;
        }
    }
    // the stretch: percentiles of the sample's scores
    let p = stretch_pct.clamp(0.0, 49.0) / 100.0;
    let (i0, i1) = ((p * (n - 1) as f32) as usize, ((1.0 - p) * (n - 1) as f32) as usize);
    for c in 0..3 {
        let mut col: Vec<f32> =
            (0..n).map(|j| (0..d).map(|i| (x[j * d + i] - b.mean[i]) * b.comps[c * d + i]).sum::<f32>()).collect();
        b.lo[c] = *col.select_nth_unstable_by(i0, f32::total_cmp).1;
        let hi = *col[i0..].select_nth_unstable_by(i1 - i0, f32::total_cmp).1;
        b.hi[c] = hi.max(b.lo[c] + 1e-12);
    }
    b.ms = ms(t0);
    b
}

/// The grey of the pixels that did not change (at or below the threshold) in the direction view.
pub const UNCHANGED: [u8; 4] = [48, 48, 48, 255];

/// Each pixel valid in years t and t0 through `f(p, pa, pb, cos, out)`: pa, pb the projections of each year's codes
/// on `w` (k <= 4 rows), cos the two vectors' cosine; `out` the pixel's `per` values.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn pixel_pairs<T: Send>(
    s: &EmbeddingStore,
    t: usize,
    t0: usize,
    w: &[f32],
    k: usize,
    out: &mut [T],
    per: usize,
    pool: &ThreadPool,
    f: &(dyn Fn(usize, &[f32], &[f32], f32, &mut [T]) + Sync),
) {
    let d = s.d;
    let (Some(r), Some(a), Some(c)) = (s.ranges(), s.year(t), s.year(t0)) else { return };
    let zero = vec![0f32; k];
    par_chunks(pool, out, per, 8192, &|i0, o| {
        const M: usize = 1024;
        let (mut pa, mut pb, mut dot) = (vec![0f32; M * k], vec![0f32; M * k], vec![0f32; M]);
        let m_all = o.len() / per;
        let mut j = 0;
        while j < m_all {
            let m = M.min(m_all - j);
            let (lo, hi) = ((i0 + j) * d, (i0 + j + m) * d);
            simd::project(&a.q[lo..hi], m, d, w, k, &zero, &mut pa);
            simd::project(&c.q[lo..hi], m, d, w, k, &zero, &mut pb);
            simd::dot_dequant(&a.q[lo..hi], &c.q[lo..hi], m, d, &r.scale, &r.offset, &mut dot);
            for i in 0..m {
                let p = i0 + j + i;
                if a.valid[p] == 0 || c.valid[p] == 0 || a.norm[p] <= 0.0 || c.norm[p] <= 0.0 {
                    continue;
                }
                let cos = dot[i] / (a.norm[p] * c.norm[p]);
                f(p, &pa[i * k..(i + 1) * k], &pb[i * k..(i + 1) * k], cos, &mut o[(j + i) * per..(j + i + 1) * per]);
            }
            j += m;
        }
    });
}

/// The change from year t0 to year t as RGBA8: the changed pixels in the colours of their direction, the others
/// grey, transparent where a year has no vector.
pub fn render_direction(s: &EmbeddingStore, t: usize, t0: usize, b: &DirectionBasis, pool: &ThreadPool) -> Vec<u8> {
    let (d, n) = (s.d, s.pixels());
    let mut rgba = vec![0u8; n * 4];
    let Some(r) = s.ranges() else { return rgba };
    if b.samples < 3 || b.comps.len() != 3 * d {
        return rgba;
    }
    // e_t - e_t0 = (q_t - q_t0) * scale (the offsets cancel): each year's codes projected on comps * scale, the
    // difference taken; then the mean and the stretch
    let (mut w, mut bias) = (vec![0f32; 3 * d], [0f32; 3]);
    for c in 0..3 {
        let k = 1.0 / (b.hi[c] - b.lo[c]);
        for i in 0..d {
            w[c * d + i] = b.comps[c * d + i] * r.scale[i] * k;
            bias[c] -= b.mean[i] * b.comps[c * d + i] * k;
        }
        bias[c] -= b.lo[c] * k;
    }
    let thr = b.threshold;
    pixel_pairs(s, t, t0, &w, 3, &mut rgba, 4, pool, &|_, pa, pb, cos, px| {
        if 1.0 - cos <= thr {
            px.copy_from_slice(&UNCHANGED);
            return;
        }
        for c in 0..3 {
            px[c] = ((pa[c] - pb[c] + bias[c]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        px[3] = 255;
    });
    rgba
}

/// The difference vector e_t - e_t0 of pixel p (None where a year has no vector).
pub fn change_vector(s: &EmbeddingStore, t: usize, t0: usize, p: usize) -> Option<Vec<f32>> {
    if !s.year_ready(t) || !s.year_ready(t0) || !s.valid(t, p) || !s.valid(t0, p) {
        return None;
    }
    let (mut a, mut b) = (vec![0f32; s.d], vec![0f32; s.d]);
    s.dequant(t, p, &mut a);
    s.dequant(t0, p, &mut b);
    Some(a.iter().zip(&b).map(|(x, y)| x - y).collect())
}

/// How much each pixel's change from year t0 to year t is like `reference` (a difference vector): the cosine of the
/// two differences where the pixel changed (cosine distance above `threshold`), NaN elsewhere.
pub fn render_change_like(
    s: &EmbeddingStore,
    t: usize,
    t0: usize,
    reference: &[f32],
    threshold: f32,
    pool: &ThreadPool,
) -> Vec<f32> {
    let (d, n) = (s.d, s.pixels());
    let mut out = vec![f32::NAN; n];
    let (Some(r), Some(a), Some(c)) = (s.ranges(), s.year(t), s.year(t0)) else { return out };
    let rn = reference.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
    if reference.len() != d || rn <= 0.0 {
        return out;
    }
    let w: Vec<f32> = (0..d).map(|i| (f64::from(reference[i]) / rn) as f32 * r.scale[i]).collect();
    pixel_pairs(s, t, t0, &w, 1, &mut out, 1, pool, &|p, pa, pb, cos, o| {
        if 1.0 - cos <= threshold {
            return;
        }
        // |e_t - e_t0|^2 = |e_t|^2 + |e_t0|^2 - 2 e_t . e_t0
        let (na, nc) = (a.norm[p], c.norm[p]);
        let dn = (na * na + nc * nc - 2.0 * cos * na * nc).max(0.0).sqrt();
        if dn > 0.0 {
            o[0] = (pa[0] - pb[0]) / dn;
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eigen::Rng;

    /// Two years of 40 x 40: x < 28 unchanged, 28..34 moved along one direction, 34..40 along another (change is
    /// the exception, as the automatic threshold expects).
    fn two_kinds() -> (EmbeddingStore, Vec<f32>, Vec<f32>) {
        let d = 32;
        let mut rng = Rng::new(5);
        let base: Vec<f32> = (0..d).map(|_| rng.normal() as f32).collect();
        let u: Vec<f32> = (0..d).map(|i| if i < 16 { 1.5 } else { 0.0 }).collect();
        let v: Vec<f32> = (0..d).map(|i| if i >= 16 { 1.5 } else { 0.0 }).collect();
        let (u2, v2) = (u.clone(), v.clone());
        let mut noise = Rng::new(9);
        let s = EmbeddingStore::synthetic(40, 40, 2, d, 6.0, move |t, p, out| {
            let x = p % 40;
            for i in 0..d {
                let shift = if t == 0 {
                    0.0
                } else if x >= 34 {
                    v2[i]
                } else if x >= 28 {
                    u2[i]
                } else {
                    0.0
                };
                out[i] = base[i] + shift + 0.05 * noise.normal() as f32;
            }
        });
        (s, u, v)
    }

    #[test]
    fn kinds_of_change_in_their_colours() {
        let (s, u, _) = two_kinds();
        let pool = crate::thread_pool(2);
        let b = fit_direction(&s, &[(1, 0)], 0.05, 2.0, &pool);
        assert!(b.samples > 400, "{}", b.samples);
        assert!((b.changed - 12.0 / 40.0).abs() < 0.05, "{}", b.changed);
        let auto = fit_direction(&s, &[(1, 0)], -1.0, 2.0, &pool);
        assert!(
            auto.threshold > 0.0 && (auto.changed - 12.0 / 40.0).abs() < 0.05,
            "{} {}",
            auto.threshold,
            auto.changed
        );
        let rgba = render_direction(&s, 1, 0, &b, &pool);
        let px = |x: usize, y: usize| &rgba[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4];
        assert_eq!(px(3, 20), UNCHANGED);
        // the two kinds apart along the first component (red), each alike in it (the other two: the noise)
        let (m, r) = (px(30, 10)[0], px(37, 30)[0]);
        assert!(m.abs_diff(r) > 150, "{m} {r}");
        assert!(m.abs_diff(px(31, 35)[0]) < 30 && r.abs_diff(px(38, 2)[0]) < 30);
        // like the middle's change: about 1 there, about 0 on the right, NaN where unchanged
        let like = render_change_like(&s, 1, 0, &u, 0.05, &pool);
        assert!(like[10 * 40 + 30] > 0.95, "{}", like[10 * 40 + 30]);
        assert!(like[30 * 40 + 37].abs() < 0.2, "{}", like[30 * 40 + 37]);
        assert!(like[20 * 40 + 3].is_nan());
        assert!(change_vector(&s, 1, 0, 30).is_some());
    }
}
