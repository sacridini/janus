//! The kernels of the embedding views (src/embedding_simd.cpp), chosen once at run time: AVX2 + FMA when the CPU
//! (and the OS) has them; else eight independent lanes that the compiler vectorizes for the target (SSE2 on any
//! x86-64, NEON on ARM64). The plain loops are the reference (`force_scalar`, the self-test).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static FORCED_SCALAR: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Scalar,
    Lanes,
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

fn detected() -> Kind {
    static K: OnceLock<Kind> = OnceLock::new();
    *K.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
            return Kind::Avx2;
        }
        Kind::Lanes
    })
}

fn kind() -> Kind {
    if FORCED_SCALAR.load(Ordering::Relaxed) { Kind::Scalar } else { detected() }
}

pub fn name() -> &'static str {
    match kind() {
        Kind::Scalar => "scalar",
        Kind::Lanes => "8 lanes (portable)",
        #[cfg(target_arch = "x86_64")]
        Kind::Avx2 => "AVX2+FMA",
    }
}

/// The plain kernels (the self-test, comparisons).
pub fn force_scalar(on: bool) {
    FORCED_SCALAR.store(on, Ordering::Relaxed);
}

/// C (D x D row-major; the upper triangle with the diagonal is complete) += X X^T, X: D rows of n floats, row
/// stride ld. D % 4 == 0, n % 8 == 0.
pub fn syrk(x: &[f32], d: usize, n: usize, ld: usize, c: &mut [f32]) {
    match kind() {
        Kind::Scalar => syrk_scalar(x, d, n, ld, c),
        Kind::Lanes => syrk_lanes(x, d, n, ld, c),
        #[cfg(target_arch = "x86_64")]
        Kind::Avx2 => unsafe { avx2::syrk(x, d, n, ld, c) },
    }
}

/// out[i*K + k] = bias[k] + sum_b q[i*D + b] * W[k*D + b] for i < n, K <= 4.
pub fn project(q: &[i8], n: usize, d: usize, w: &[f32], k: usize, bias: &[f32], out: &mut [f32]) {
    match kind() {
        Kind::Scalar => project_scalar(q, n, d, w, k, bias, out),
        Kind::Lanes => project_lanes(q, n, d, w, k, bias, out),
        #[cfg(target_arch = "x86_64")]
        Kind::Avx2 => unsafe { avx2::project(q, n, d, w, k, bias, out) },
    }
}

/// out[i] = sum_b (A[i*D+b] s[b] + o[b]) (B[i*D+b] s[b] + o[b]).
pub fn dot_dequant(a: &[i8], b: &[i8], n: usize, d: usize, s: &[f32], o: &[f32], out: &mut [f32]) {
    match kind() {
        Kind::Scalar => dot_dequant_scalar(a, b, n, d, s, o, out),
        Kind::Lanes => dot_dequant_lanes(a, b, n, d, s, o, out),
        #[cfg(target_arch = "x86_64")]
        Kind::Avx2 => unsafe { avx2::dot_dequant(a, b, n, d, s, o, out) },
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The references
// ---------------------------------------------------------------------------------------------------------------

fn syrk_scalar(x: &[f32], d: usize, n: usize, ld: usize, c: &mut [f32]) {
    for i in 0..d {
        for j in i..d {
            let (a, b) = (&x[i * ld..i * ld + n], &x[j * ld..j * ld + n]);
            let mut s = 0f32;
            for k in 0..n {
                s += a[k] * b[k];
            }
            c[i * d + j] += s;
        }
    }
}

fn project_scalar(q: &[i8], n: usize, d: usize, w: &[f32], k: usize, bias: &[f32], out: &mut [f32]) {
    for i in 0..n {
        let v = &q[i * d..(i + 1) * d];
        for kk in 0..k {
            let wk = &w[kk * d..(kk + 1) * d];
            let mut s = bias[kk];
            for b in 0..d {
                s += f32::from(v[b]) * wk[b];
            }
            out[i * k + kk] = s;
        }
    }
}

fn dot_dequant_scalar(a: &[i8], b: &[i8], n: usize, d: usize, s: &[f32], o: &[f32], out: &mut [f32]) {
    for i in 0..n {
        let (x, y) = (&a[i * d..(i + 1) * d], &b[i * d..(i + 1) * d]);
        let mut acc = 0f32;
        for k in 0..d {
            acc += (f32::from(x[k]) * s[k] + o[k]) * (f32::from(y[k]) * s[k] + o[k]);
        }
        out[i] = acc;
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Eight lanes: independent accumulators the compiler turns into vector registers
// ---------------------------------------------------------------------------------------------------------------

#[inline(always)]
fn dot8(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for l in 0..8 {
            acc[l] = x[l].mul_add(y[l], acc[l]);
        }
    }
    let mut s: f32 = acc.iter().sum();
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

fn syrk_lanes(x: &[f32], d: usize, n: usize, ld: usize, c: &mut [f32]) {
    for i in 0..d {
        let a = &x[i * ld..i * ld + n];
        for j in i..d {
            c[i * d + j] += dot8(a, &x[j * ld..j * ld + n]);
        }
    }
}

fn project_lanes(q: &[i8], n: usize, d: usize, w: &[f32], k: usize, bias: &[f32], out: &mut [f32]) {
    let mut v = vec![0f32; d];
    for i in 0..n {
        for (f, &x) in v.iter_mut().zip(&q[i * d..(i + 1) * d]) {
            *f = f32::from(x);
        }
        for kk in 0..k {
            out[i * k + kk] = bias[kk] + dot8(&v, &w[kk * d..(kk + 1) * d]);
        }
    }
}

fn dot_dequant_lanes(a: &[i8], b: &[i8], n: usize, d: usize, s: &[f32], o: &[f32], out: &mut [f32]) {
    let (mut fa, mut fb) = (vec![0f32; d], vec![0f32; d]);
    for i in 0..n {
        for k in 0..d {
            fa[k] = f32::from(a[i * d + k]).mul_add(s[k], o[k]);
            fb[k] = f32::from(b[i * d + k]).mul_add(s[k], o[k]);
        }
        out[i] = dot8(&fa, &fb);
    }
}

// ---------------------------------------------------------------------------------------------------------------
// AVX2 + FMA
// ---------------------------------------------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    #[target_feature(enable = "avx2,fma")]
    unsafe fn hsum(v: __m256) -> f32 {
        let s = _mm_add_ps(_mm256_castps256_ps128(v), _mm256_extractf128_ps(v, 1));
        let s = _mm_add_ps(s, _mm_movehl_ps(s, s));
        let s = _mm_add_ss(s, _mm_shuffle_ps(s, s, 1));
        _mm_cvtss_f32(s)
    }

    #[target_feature(enable = "avx2,fma")]
    unsafe fn i8x8(p: *const i8) -> __m256 {
        unsafe { _mm256_cvtepi32_ps(_mm256_cvtepi8_epi32(_mm_loadl_epi64(p.cast()))) }
    }

    /// 2 x 4 tiles: 8 accumulators and 6 loads in the 16 ymm registers.
    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn syrk(x: &[f32], d: usize, n: usize, ld: usize, c: &mut [f32]) {
        assert!(d.is_multiple_of(4) && n.is_multiple_of(8) && x.len() >= (d - 1) * ld + n && c.len() >= d * d);
        let xp = x.as_ptr();
        let mut i = 0;
        while i < d {
            let mut j = i & !3;
            while j < d {
                unsafe {
                    let z = _mm256_setzero_ps();
                    let (mut a00, mut a01, mut a02, mut a03) = (z, z, z, z);
                    let (mut a10, mut a11, mut a12, mut a13) = (z, z, z, z);
                    let p0 = xp.add(i * ld);
                    let p1 = p0.add(ld);
                    let q0 = xp.add(j * ld);
                    let (q1, q2, q3) = (q0.add(ld), q0.add(2 * ld), q0.add(3 * ld));
                    let mut k = 0;
                    while k < n {
                        let (x0, x1) = (_mm256_loadu_ps(p0.add(k)), _mm256_loadu_ps(p1.add(k)));
                        let (y0, y1) = (_mm256_loadu_ps(q0.add(k)), _mm256_loadu_ps(q1.add(k)));
                        let (y2, y3) = (_mm256_loadu_ps(q2.add(k)), _mm256_loadu_ps(q3.add(k)));
                        a00 = _mm256_fmadd_ps(x0, y0, a00);
                        a01 = _mm256_fmadd_ps(x0, y1, a01);
                        a02 = _mm256_fmadd_ps(x0, y2, a02);
                        a03 = _mm256_fmadd_ps(x0, y3, a03);
                        a10 = _mm256_fmadd_ps(x1, y0, a10);
                        a11 = _mm256_fmadd_ps(x1, y1, a11);
                        a12 = _mm256_fmadd_ps(x1, y2, a12);
                        a13 = _mm256_fmadd_ps(x1, y3, a13);
                        k += 8;
                    }
                    let c0 = i * d + j;
                    let c1 = c0 + d;
                    c[c0] += hsum(a00);
                    c[c0 + 1] += hsum(a01);
                    c[c0 + 2] += hsum(a02);
                    c[c0 + 3] += hsum(a03);
                    c[c1] += hsum(a10);
                    c[c1 + 1] += hsum(a11);
                    c[c1 + 2] += hsum(a12);
                    c[c1 + 3] += hsum(a13);
                }
                j += 4;
            }
            i += 2;
        }
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn project(q: &[i8], n: usize, d: usize, w: &[f32], k: usize, bias: &[f32], out: &mut [f32]) {
        assert!(q.len() >= n * d && w.len() >= k * d && out.len() >= n * k && bias.len() >= k);
        unsafe {
            match k {
                1 => project_k::<1>(q, n, d, w, bias, out),
                2 => project_k::<2>(q, n, d, w, bias, out),
                3 => project_k::<3>(q, n, d, w, bias, out),
                _ => project_k::<4>(q, n, d, w, bias, out),
            }
        }
    }

    /// K accumulators, the K weight rows kept in L1 across pixels (unrolled for each K).
    #[target_feature(enable = "avx2,fma")]
    unsafe fn project_k<const K: usize>(q: &[i8], n: usize, d: usize, w: &[f32], bias: &[f32], out: &mut [f32]) {
        let d8 = d & !7;
        let wp = w.as_ptr();
        for i in 0..n {
            unsafe {
                let v = q.as_ptr().add(i * d);
                let mut acc = [_mm256_setzero_ps(); K];
                let mut b = 0;
                while b < d8 {
                    let x = i8x8(v.add(b));
                    for (kk, a) in acc.iter_mut().enumerate() {
                        *a = _mm256_fmadd_ps(x, _mm256_loadu_ps(wp.add(kk * d + b)), *a);
                    }
                    b += 8;
                }
                for (kk, a) in acc.iter().enumerate() {
                    let mut s = bias[kk] + hsum(*a);
                    for b in d8..d {
                        s += f32::from(*v.add(b)) * *wp.add(kk * d + b);
                    }
                    *out.get_unchecked_mut(i * K + kk) = s;
                }
            }
        }
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn dot_dequant(a: &[i8], b: &[i8], n: usize, d: usize, s: &[f32], o: &[f32], out: &mut [f32]) {
        assert!(a.len() >= n * d && b.len() >= n * d && s.len() >= d && o.len() >= d && out.len() >= n);
        let d8 = d & !7;
        for i in 0..n {
            unsafe {
                let (pa, pb) = (a.as_ptr().add(i * d), b.as_ptr().add(i * d));
                let mut acc = _mm256_setzero_ps();
                let mut k = 0;
                while k < d8 {
                    let (sk, ok) = (_mm256_loadu_ps(s.as_ptr().add(k)), _mm256_loadu_ps(o.as_ptr().add(k)));
                    let fa = _mm256_fmadd_ps(i8x8(pa.add(k)), sk, ok);
                    let fb = _mm256_fmadd_ps(i8x8(pb.add(k)), sk, ok);
                    acc = _mm256_fmadd_ps(fa, fb, acc);
                    k += 8;
                }
                let mut r = hsum(acc);
                for k in d8..d {
                    r += (f32::from(*pa.add(k)) * s[k] + o[k]) * (f32::from(*pb.add(k)) * s[k] + o[k]);
                }
                out[i] = r;
            }
        }
    }
}
