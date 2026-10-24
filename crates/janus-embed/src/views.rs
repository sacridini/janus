//! Principal components, similarity and change (src/embedding.cpp): a sample of the store's vectors, its PCA
//! (covariance by the SIMD kernel, then the leading eigenvectors), and a year drawn as an image.

use std::sync::Mutex;
use std::time::Instant;

use rayon::ThreadPool;

use crate::eigen::{Rng, top_eigen};
use crate::parallel_for;
use crate::simd;
use crate::store::EmbeddingStore;

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

/// The principal components of a set of vectors.
#[derive(Clone, Debug, Default)]
pub struct PcaBasis {
    pub d: usize,
    pub k: usize,
    /// d
    pub mean: Vec<f32>,
    /// k x d, unit length; the sign fixed: the largest loading positive
    pub comps: Vec<f32>,
    /// the variance along each component
    pub eig: Vec<f64>,
    /// the total variance (the sum over the dimensions)
    pub total: f64,
    pub samples: usize,
    pub iterations: usize,
    /// years the sample was drawn from
    pub years: usize,
    pub ms: f64,
    pub cov_ms: f64,
    pub eig_ms: f64,
    /// the components drawn as R, G, B
    pub shown: [usize; 3],
    /// their stretch (percentiles of the sample)
    pub lo: [f32; 3],
    pub hi: [f32; 3],
}

/// Up to `max_n` valid (year, pixel) pairs: every year (`all_years`) or year t, inside `rect` (store pixels x0, y0,
/// x1, y1; None: everywhere) and, with `mask` (one cell per pixel of rect, row by row), where it is set; evenly
/// spread, with a fixed seed (the same request gives the same sample).
pub fn sample_vectors(
    s: &EmbeddingStore,
    rect: Option<[usize; 4]>,
    max_n: usize,
    all_years: bool,
    t: usize,
    mask: Option<&[bool]>,
) -> Vec<(usize, u32)> {
    let years: Vec<usize> = (0..s.t_n).filter(|&y| (all_years || y == t) && s.year_ready(y)).collect();
    let [x0, y0, x1, y1] =
        rect.map_or([0, 0, s.w, s.h], |r| [r[0].min(s.w), r[1].min(s.h), r[2].min(s.w), r[3].min(s.h)]);
    let rw = x1.saturating_sub(x0);
    let area = rw * y1.saturating_sub(y0);
    let mask = mask.filter(|m| m.len() == area);
    let total = area * years.len();
    let mut out = Vec::new();
    if total == 0 || max_n == 0 {
        return out;
    }
    let at = |i: usize| {
        let (y, a) = (years[i / area], i % area);
        (y, ((y0 + a / rw) * s.w + x0 + a % rw) as u32)
    };
    let usable = |i: usize, yp: (usize, u32)| mask.is_none_or(|m| m[i % area]) && s.valid(yp.0, yp.1 as usize);
    if total <= max_n {
        // every valid vector
        out.extend((0..total).map(|i| (i, at(i))).filter(|&(i, yp)| usable(i, yp)).map(|(_, yp)| yp));
        return out;
    }
    // evenly spread with a fixed jitter (the same request, the same sample)
    let mut rng = Rng::new(12345);
    let step = total as f64 / max_n as f64;
    out.reserve(max_n);
    for k in 0..max_n {
        let mut i = ((k as f64 + rng.uniform()) * step) as usize;
        for _ in 0..8 {
            if i >= total {
                break;
            }
            let yp = at(i);
            if usable(i, yp) {
                out.push(yp);
                break;
            }
            i += ((step / 8.0) as usize).max(1);
        }
    }
    out
}

/// The PCA of a sample: the covariance in parallel with the SIMD kernel, then top_eigen; `shown` and
/// `stretch_pct` set the drawn components and their percentile stretch.
pub fn fit_pca(
    s: &EmbeddingStore,
    sample: &[(usize, u32)],
    k: usize,
    shown: [usize; 3],
    stretch_pct: f32,
    pool: &ThreadPool,
) -> PcaBasis {
    let t0 = Instant::now();
    let d = s.d;
    let dp = (d + 3) & !3; // syrk works on rows in fours
    let mut b = PcaBasis { d, k: k.clamp(1, d), shown, samples: sample.len(), ..Default::default() };
    let n = sample.len();
    let Some(r) = s.ranges() else { return b };
    if n < 2 {
        return b;
    }
    // the mean: integer sums of the Int8 codes, dequantised once (s * sum q + n * o)
    let qsum = Mutex::new(vec![0i64; d]);
    parallel_for(pool, n, 8192, &|lo, hi| {
        let mut part = vec![0i32; d]; // 8192 x 127 fits in 32 bits
        for &(t, p) in &sample[lo..hi] {
            for (a, &q) in part.iter_mut().zip(s.vec(t, p as usize)) {
                *a += i32::from(q);
            }
        }
        let mut qs = qsum.lock().unwrap();
        for (a, &x) in qs.iter_mut().zip(&part) {
            *a += i64::from(x);
        }
    });
    let qsum = qsum.into_inner().unwrap();
    let mean: Vec<f64> =
        (0..d).map(|i| f64::from(r.scale[i]) * qsum[i] as f64 / n as f64 + f64::from(r.offset[i])).collect();
    b.mean = mean.iter().map(|&m| m as f32).collect();

    // the covariance: blocks of 256 centred vectors transposed to D rows, X X^T by the SIMD kernel in float, each
    // block's result added in double
    let tc = Instant::now();
    const BLOCK: usize = 256;
    let cov = Mutex::new(vec![0f64; dp * dp]);
    let sh: Vec<f32> = (0..d).map(|i| (f64::from(r.offset[i]) - mean[i]) as f32).collect();
    parallel_for(pool, n.div_ceil(BLOCK), 4, &|lo, hi| {
        let (mut x, mut cb) = (vec![0f32; dp * BLOCK], vec![0f32; dp * dp]);
        let mut local = vec![0f64; dp * dp];
        for blk in lo..hi {
            x.fill(0.0);
            let (i0, i1) = (blk * BLOCK, n.min(blk * BLOCK + BLOCK));
            for i in i0..i1 {
                let (t, p) = sample[i];
                for (dd, &q) in s.vec(t, p as usize).iter().enumerate() {
                    x[dd * BLOCK + i - i0] = f32::from(q) * r.scale[dd] + sh[dd];
                }
            }
            cb.fill(0.0);
            simd::syrk(&x, dp, BLOCK, BLOCK, &mut cb);
            for row in 0..d {
                for c in row..d {
                    local[row * dp + c] += f64::from(cb[row * dp + c]);
                }
            }
        }
        let mut cv = cov.lock().unwrap();
        for (a, x) in cv.iter_mut().zip(&local) {
            *a += x;
        }
    });
    let cov = cov.into_inner().unwrap();
    let mut cd = vec![0f64; d * d];
    for row in 0..d {
        for c in row..d {
            let v = cov[row * dp + c] / (n - 1) as f64;
            cd[row * d + c] = v;
            cd[c * d + row] = v;
        }
    }
    b.cov_ms = ms(tc);
    b.total = (0..d).map(|i| cd[i * d + i]).sum();

    // the leading eigenvectors; each one's sign fixed (its largest loading positive), so the colours do not flip
    // when the sample changes a little
    let te = Instant::now();
    let (vecs, eig, it) = top_eigen(&cd, d, b.k);
    (b.eig, b.iterations) = (eig, it);
    b.eig_ms = ms(te);
    b.comps = vec![0.0; b.k * d];
    for c in 0..b.k {
        let v = &vecs[c * d..(c + 1) * d];
        let big = (0..d).fold(0, |m, i| if v[i].abs() > v[m].abs() { i } else { m });
        let sign = if v[big] < 0.0 { -1.0 } else { 1.0 };
        for i in 0..d {
            b.comps[c * d + i] = (sign * v[i]) as f32;
        }
    }

    // the stretch of the shown components: percentiles of the sample's projections
    for c in b.shown.iter_mut() {
        *c = (*c).min(b.k - 1);
    }
    let (mut w, mut bias) = (vec![0f32; 3 * d], [0f32; 3]);
    for c in 0..3 {
        for i in 0..d {
            let cdv = b.comps[b.shown[c] * d + i];
            w[c * d + i] = cdv * r.scale[i];
            bias[c] += (r.offset[i] - b.mean[i]) * cdv;
        }
    }
    let mut proj = vec![0f32; n * 3];
    par_chunks(pool, &mut proj, 3, 4096, &|i0, out| {
        // gathered, then one kernel call per 512
        let mut g = vec![0i8; 512 * d];
        let m_all = out.len() / 3;
        let mut j = 0;
        while j < m_all {
            let m = 512.min(m_all - j);
            for i in 0..m {
                let (t, p) = sample[i0 + j + i];
                g[i * d..(i + 1) * d].copy_from_slice(s.vec(t, p as usize));
            }
            simd::project(&g, m, d, &w, 3, &bias, &mut out[j * 3..(j + m) * 3]);
            j += m;
        }
    });
    let p = stretch_pct.clamp(0.0, 49.0) / 100.0;
    let (i0, i1) = ((p * (n - 1) as f32) as usize, ((1.0 - p) * (n - 1) as f32) as usize);
    for c in 0..3 {
        let mut col: Vec<f32> = (0..n).map(|i| proj[i * 3 + c]).collect();
        b.lo[c] = *col.select_nth_unstable_by(i0, f32::total_cmp).1;
        let hi = *col[i0..].select_nth_unstable_by(i1 - i0, f32::total_cmp).1; // above i0: a smaller range
        b.hi[c] = hi.max(b.lo[c] + 1e-12);
    }
    b.ms = ms(t0);
    b
}

/// A view's output split in disjoint chunks, written in parallel.
pub(crate) fn par_chunks<T: Send>(
    pool: &ThreadPool,
    out: &mut [T],
    per: usize,
    chunk: usize,
    f: &(dyn Fn(usize, &mut [T]) + Sync),
) {
    use rayon::prelude::*;
    pool.install(|| out.par_chunks_mut(chunk * per).enumerate().for_each(|(c, o)| f(c * chunk, o)));
}

/// Year t as RGBA8 (store size): the three shown components, stretched; invalid pixels transparent.
pub fn render_pca_rgb(s: &EmbeddingStore, t: usize, b: &PcaBasis, pool: &ThreadPool) -> Vec<u8> {
    let (d, n) = (s.d, s.pixels());
    let mut rgba = vec![0u8; n * 4];
    let (Some(r), Some(year)) = (s.ranges(), s.year(t)) else { return rgba };
    if b.comps.is_empty() {
        return rgba;
    }
    // scale, offset, mean and stretch folded into the weights: 3 dot products per pixel
    let (mut w, mut bias) = (vec![0f32; 3 * d], [0f32; 3]);
    for c in 0..3 {
        let k = 1.0 / (b.hi[c] - b.lo[c]);
        for i in 0..d {
            let cd = b.comps[b.shown[c] * d + i];
            w[c * d + i] = cd * r.scale[i] * k;
            bias[c] += (r.offset[i] - b.mean[i]) * cd * k;
        }
        bias[c] -= b.lo[c] * k;
    }
    par_chunks(pool, &mut rgba, 4, 8192, &|i0, px| {
        let m_all = px.len() / 4;
        let mut out = vec![0f32; 1024 * 3];
        let mut j = 0;
        while j < m_all {
            let m = 1024.min(m_all - j);
            simd::project(&year.q[(i0 + j) * d..(i0 + j + m) * d], m, d, &w, 3, &bias, &mut out);
            for i in 0..m {
                if year.valid[i0 + j + i] == 0 {
                    continue; // transparent
                }
                let p = &mut px[(j + i) * 4..(j + i) * 4 + 4];
                for c in 0..3 {
                    p[c] = (out[i * 3 + c].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
                p[3] = 255;
            }
            j += m;
        }
    });
    rgba
}

/// The cosine similarity of every vector of year t to `reference` (D values); NaN where invalid.
pub fn render_similarity(s: &EmbeddingStore, t: usize, reference: &[f32], pool: &ThreadPool) -> Vec<f32> {
    let (d, n) = (s.d, s.pixels());
    let mut out = vec![f32::NAN; n];
    let (Some(r), Some(year)) = (s.ranges(), s.year(t)) else { return out };
    let rn = reference.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
    if reference.len() != d || rn <= 0.0 {
        return out;
    }
    let w: Vec<f32> = (0..d).map(|i| (f64::from(reference[i]) / rn) as f32 * r.scale[i]).collect();
    let bias: f32 = (0..d).map(|i| (f64::from(reference[i]) / rn) as f32 * r.offset[i]).sum();
    par_chunks(pool, &mut out, 1, 8192, &|i0, o| {
        let mut dot = vec![0f32; 1024];
        let mut j = 0;
        while j < o.len() {
            let m = 1024.min(o.len() - j);
            simd::project(&year.q[(i0 + j) * d..(i0 + j + m) * d], m, d, &w, 1, &[bias], &mut dot);
            for i in 0..m {
                let p = i0 + j + i;
                if year.valid[p] != 0 && year.norm[p] > 0.0 {
                    o[j + i] = dot[i] / year.norm[p];
                }
            }
            j += m;
        }
    });
    out
}

/// The cosine distance (1 - cosine similarity) between years t and t0; NaN where either is invalid.
pub fn render_change(s: &EmbeddingStore, t: usize, t0: usize, pool: &ThreadPool) -> Vec<f32> {
    let (d, n) = (s.d, s.pixels());
    let mut out = vec![f32::NAN; n];
    let (Some(r), Some(a), Some(c)) = (s.ranges(), s.year(t), s.year(t0)) else { return out };
    par_chunks(pool, &mut out, 1, 8192, &|i0, o| {
        let mut dot = vec![0f32; 1024];
        let mut j = 0;
        while j < o.len() {
            let m = 1024.min(o.len() - j);
            let (lo, hi) = ((i0 + j) * d, (i0 + j + m) * d);
            simd::dot_dequant(&a.q[lo..hi], &c.q[lo..hi], m, d, &r.scale, &r.offset, &mut dot);
            for i in 0..m {
                let p = i0 + j + i;
                if a.valid[p] != 0 && c.valid[p] != 0 && a.norm[p] > 0.0 && c.norm[p] > 0.0 {
                    o[j + i] = 1.0 - dot[i] / (a.norm[p] * c.norm[p]);
                }
            }
            j += m;
        }
    });
    out
}

/// The `p_lo` / `p_hi` percentiles of the finite values (a sample of them).
pub fn percentile_range(v: &[f32], p_lo: f32, p_hi: f32) -> (f32, f32) {
    let step = (v.len() / 200_000).max(1);
    let mut f: Vec<f32> = v.iter().step_by(step).copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() {
        return (0.0, 1.0);
    }
    let i0 = (p_lo / 100.0 * (f.len() - 1) as f32) as usize;
    let i1 = (p_hi / 100.0 * (f.len() - 1) as f32) as usize;
    let lo = *f.select_nth_unstable_by(i0, f32::total_cmp).1;
    let hi = *f.select_nth_unstable_by(i1, f32::total_cmp).1;
    (lo, hi.max(lo + 1e-6))
}
