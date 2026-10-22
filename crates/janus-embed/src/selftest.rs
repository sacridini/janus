//! `--selftest-embeddings` (src/embedding.cpp, embeddingSelfTest): the kernels against the scalar ones, top_eigen
//! and fit_pca against known spectra and directions, timings. Prints a report; Err(what failed).

use std::time::Instant;

use crate::eigen::{Rng, orthonormalize, top_eigen};
use crate::store::EmbeddingStore;
use crate::views::{fit_pca, render_pca_rgb, render_similarity, sample_vectors};
use crate::{simd, thread_pool};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn close(a: f32, b: f32, tol: f32) -> bool {
    (a - b).abs() <= tol * b.abs().max(1.0)
}

/// The kernels against the scalar ones (D = 64, 128, 67).
fn kernels(rng: &mut Rng) -> Result<(), &'static str> {
    for d in [64usize, 128, 67] {
        let (dp, n) = ((d + 3) & !3, 256);
        let x: Vec<f32> = (0..dp * n).map(|_| rng.normal() as f32).collect();
        let (mut c1, mut c2) = (vec![0f32; dp * dp], vec![0f32; dp * dp]);
        simd::syrk(&x, dp, n, n, &mut c1);
        simd::force_scalar(true);
        simd::syrk(&x, dp, n, n, &mut c2);
        simd::force_scalar(false);
        for r in 0..dp {
            for c in r..dp {
                if !close(c1[r * dp + c], c2[r * dp + c], 1e-4) {
                    return Err("syrk kernel differs from the scalar one");
                }
            }
        }
        let q: Vec<i8> = (0..1000 * d).map(|_| ((rng.normal() * 40.0) as i32).clamp(-127, 127) as i8).collect();
        let w: Vec<f32> = (0..3 * d).map(|_| rng.normal() as f32).collect();
        let bias = [0.5f32, -1.0, 2.0];
        let s: Vec<f32> = (0..d).map(|i| 0.01 + 0.001 * i as f32).collect();
        let o: Vec<f32> = (0..d).map(|_| 0.1 * rng.normal() as f32).collect();
        for k in 1..=3 {
            let (mut a, mut b) = (vec![0f32; 1000 * k], vec![0f32; 1000 * k]);
            simd::project(&q, 1000, d, &w, k, &bias, &mut a);
            simd::force_scalar(true);
            simd::project(&q, 1000, d, &w, k, &bias, &mut b);
            simd::force_scalar(false);
            if a.iter().zip(&b).any(|(&x, &y)| !close(x, y, 1e-3)) {
                return Err("project kernel differs");
            }
        }
        let (mut a, mut b) = (vec![0f32; 999], vec![0f32; 999]);
        simd::dot_dequant(&q[..999 * d], &q[d..1000 * d], 999, d, &s, &o, &mut a);
        simd::force_scalar(true);
        simd::dot_dequant(&q[..999 * d], &q[d..1000 * d], 999, d, &s, &o, &mut b);
        simd::force_scalar(false);
        if a.iter().zip(&b).any(|(&x, &y)| !close(x, y, 1e-3)) {
            return Err("dot_dequant kernel differs");
        }
    }
    Ok(())
}

pub fn run(threads: usize) -> Result<String, (String, &'static str)> {
    let mut report = String::new();
    let pool = thread_pool(threads);
    let mut rng = Rng::new(1);
    report += &format!("SIMD kernels: {}, {} threads\n", simd::name(), pool.current_num_threads());
    if let Err(e) = kernels(&mut rng) {
        return Err((report, e));
    }
    report += "kernels: syrk, project, dot_dequant equal the scalar ones (D = 64, 128, 67)\n";

    // top_eigen on a known spectrum: C = Q diag(l) Q^T
    {
        let d = 128;
        let mut r64 = Rng::new(3);
        let mut q: Vec<f64> = (0..d * d).map(|_| r64.normal()).collect();
        orthonormalize(&mut q, d, d); // columns: an orthonormal basis
        let lam: Vec<f64> =
            (0..d).map(|i| if i < 3 { 10.0 / (i + 1) as f64 } else { 2.0 * 0.95f64.powi(i as i32) }).collect();
        let mut c = vec![0f64; d * d];
        for r in 0..d {
            for cc in 0..d {
                c[r * d + cc] = (0..d).map(|i| q[i * d + r] * lam[i] * q[i * d + cc]).sum();
            }
        }
        let t0 = Instant::now();
        let (vecs, vals, it) = top_eigen(&c, d, 3);
        let took = ms(t0);
        let (mut err_v, mut err_l) = (0f64, 0f64);
        for cc in 0..3 {
            let dot: f64 = (0..d).map(|r| vecs[cc * d + r] * q[cc * d + r]).sum();
            err_v = err_v.max(1.0 - dot.abs());
            err_l = err_l.max((vals[cc] - lam[cc]).abs() / lam[cc]);
        }
        report += &format!(
            "top_eigen (D = 128, top 3): {it} iterations, {took:.2} ms; eigenvalue error {err_l:.1e}, vector error {err_v:.1e}\n"
        );
        if err_l > 1e-9 || err_v > 1e-9 {
            return Err((report, "top_eigen did not find the known eigenpairs"));
        }
    }

    // fit_pca on vectors with known principal directions, and the timings of every step on a store of 1000 x 1000
    // pixels
    for d in [64usize, 128] {
        let (w, h) = (1000, 1000);
        let mut dirs: Vec<f32> = (0..3 * d).map(|_| rng.normal() as f32).collect();
        for c in 0..3 {
            // orthonormal directions
            for i in 0..c {
                let dot: f32 = (0..d).map(|k| dirs[c * d + k] * dirs[i * d + k]).sum();
                for k in 0..d {
                    dirs[c * d + k] -= dot * dirs[i * d + k];
                }
            }
            let nn = (0..d).map(|k| dirs[c * d + k].powi(2)).sum::<f32>().sqrt();
            for k in 0..d {
                dirs[c * d + k] /= nn;
            }
        }
        let sd = [3.0f32, 2.0, 1.2];
        let mut g = Rng::new(5);
        let st = EmbeddingStore::synthetic(w, h, 1, d, 6.0, |_, _, v| {
            let (a, b, c) = (g.normal() as f32 * sd[0], g.normal() as f32 * sd[1], g.normal() as f32 * sd[2]);
            for k in 0..d {
                v[k] = a * dirs[k] + b * dirs[d + k] + c * dirs[2 * d + k] + 0.15 * g.normal() as f32;
            }
        });
        let ts = Instant::now();
        let sample = sample_vectors(&st, None, 65536, true, 0, None);
        let sample_ms = ms(ts);
        let b = fit_pca(&st, &sample, 3, [0, 1, 2], 2.0, &pool);
        let mut worst = 0f64;
        for c in 0..3 {
            let dot: f64 = (0..d).map(|k| f64::from(b.comps[c * d + k]) * f64::from(dirs[c * d + k])).sum();
            worst = worst.max(1.0 - dot.abs());
        }
        let t1 = Instant::now();
        let _rgba = render_pca_rgb(&st, 0, &b, &pool);
        let rgb_ms = ms(t1);
        let mut reference = vec![0f32; d];
        st.dequant(0, 12345, &mut reference);
        let t1 = Instant::now();
        let out = render_similarity(&st, 0, &reference, &pool);
        let sim_ms = ms(t1);
        if (out[12345] - 1.0).abs() > 1e-4 {
            return Err((report, "similarity of a vector to itself is not 1"));
        }
        // scalar timings for comparison
        simd::force_scalar(true);
        let bs = fit_pca(&st, &sample, 3, [0, 1, 2], 2.0, &pool);
        let t1 = Instant::now();
        let _ = render_pca_rgb(&st, 0, &bs, &pool);
        let rgb_scalar_ms = ms(t1);
        simd::force_scalar(false);
        report += &format!(
            "D = {d:3}: PCA of {} vectors in {:.1} ms (sample {sample_ms:.1}, covariance {:.1} [scalar {:.1}], eigen {:.2}, {} it.); \
             direction error {worst:.1e}\n",
            b.samples, b.ms, b.cov_ms, bs.cov_ms, b.eig_ms, b.iterations
        );
        report +=
            &format!("        1 Mpx year: RGB {rgb_ms:.1} ms [scalar {rgb_scalar_ms:.1}], similarity {sim_ms:.1} ms\n");
        if worst > 1e-3 {
            return Err((report, "fit_pca did not find the known directions"));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    #[test]
    fn selftest() {
        match super::run(4) {
            Ok(r) => println!("{r}"),
            Err((r, e)) => panic!("{r}{e}"),
        }
    }
}
