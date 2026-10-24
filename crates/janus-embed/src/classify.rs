//! Few-shot classification of embeddings: classes given by a few vectors each (pins, an ROI's pixels), fitted in
//! milliseconds and applied to every pixel of a year with the views' SIMD kernel. Two classifiers, both linear in
//! the vector (a pixel's scores are k dot products):
//! - multinomial logistic regression with an L2 penalty, on standardised dimensions, each class weighing the same
//!   whatever its number of samples, solved by Nesterov's accelerated gradient: its score is the probability;
//! - the nearest centroid by cosine (each class's mean unit vector): its score is the cosine.

use std::time::Instant;

use rayon::ThreadPool;

use crate::simd;
use crate::store::EmbeddingStore;
use crate::views::par_chunks;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Logistic,
    Centroid,
}

/// No class: below the minimum score, or no vector.
pub const NONE: u8 = 255;

#[derive(Clone, Debug)]
pub struct Classifier {
    pub method: Method,
    pub d: usize,
    pub k: usize,
    /// k x d and k: class c's score of a vector v is w[c] . v + b[c] (logistic: a logit; centroid: the unit
    /// centroid, b = 0, the score divided by |v|)
    pub w: Vec<f32>,
    pub b: Vec<f32>,
    /// the samples of each class
    pub n: Vec<usize>,
    /// the share of the samples it gets right; held out (None: a class has a single sample)
    pub train_acc: f64,
    pub cv_acc: Option<f64>,
    /// folds of the held-out estimate (= the samples: leave-one-out)
    pub cv_folds: usize,
    pub iterations: usize,
    pub ms: f64,
}

impl Classifier {
    /// The class and its score (probability, cosine) from the k scores of a vector of norm `norm`.
    pub fn decide(&self, scores: &[f32], norm: f32) -> (usize, f32) {
        let mut best = 0;
        for c in 1..self.k {
            if scores[c] > scores[best] {
                best = c;
            }
        }
        let score = match self.method {
            Method::Logistic => {
                let m = scores[best];
                1.0 / scores.iter().map(|&s| (s - m).exp()).sum::<f32>()
            }
            Method::Centroid if norm > 0.0 => scores[best] / norm,
            Method::Centroid => 0.0,
        };
        (best, score)
    }

    /// A vector's class and score.
    pub fn predict(&self, v: &[f32]) -> (usize, f32) {
        let d = self.d;
        let scores: Vec<f32> = (0..self.k)
            .map(|c| {
                let s: f64 = self.w[c * d..(c + 1) * d].iter().zip(v).map(|(&w, &x)| f64::from(w) * f64::from(x)).sum();
                (s + f64::from(self.b[c])) as f32
            })
            .collect();
        let norm = v.iter().map(|&x| f64::from(x) * f64::from(x)).sum::<f64>().sqrt() as f32;
        self.decide(&scores, norm)
    }
}

/// The weights of samples `xs` (d values each) of classes `ys` (0..k): k x d and k, and the iterations.
#[allow(clippy::too_many_arguments)]
fn fit_raw(
    xs: &[&[f32]],
    ys: &[usize],
    k: usize,
    d: usize,
    method: Method,
    lambda: f64,
    mu: &[f64],
    sd: &[f64],
) -> (Vec<f32>, Vec<f32>, usize) {
    let mut count = vec![0usize; k];
    for &y in ys {
        count[y] += 1;
    }
    if method == Method::Centroid {
        // each class's mean unit vector, made unit
        let mut w = vec![0f64; k * d];
        for (x, &y) in xs.iter().zip(ys) {
            let n = x.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
            if n > 0.0 {
                for i in 0..d {
                    w[y * d + i] += f64::from(x[i]) / n;
                }
            }
        }
        for c in 0..k {
            let n = w[c * d..(c + 1) * d].iter().map(|v| v * v).sum::<f64>().sqrt();
            if n > 0.0 {
                w[c * d..(c + 1) * d].iter_mut().for_each(|v| *v /= n);
            }
        }
        return (w.iter().map(|&v| v as f32).collect(), vec![0.0; k], 0);
    }
    // standardised samples, each class weighing 1 / (classes present)
    let present = count.iter().filter(|&&c| c > 0).count().max(1) as f64;
    let z: Vec<f64> = xs.iter().flat_map(|x| (0..d).map(move |i| (f64::from(x[i]) - mu[i]) / sd[i])).collect();
    let om: Vec<f64> = ys.iter().map(|&y| 1.0 / (present * count[y] as f64)).collect();
    // the gradient is Lipschitz with L <= 1/2 sum om |(z, 1)|^2 + lambda (softmax's Hessian is below 1/2 I)
    let lip = 0.5
        * (0..xs.len()).map(|i| om[i] * (z[i * d..(i + 1) * d].iter().map(|v| v * v).sum::<f64>() + 1.0)).sum::<f64>()
        + lambda;
    let kappa = lip / lambda.max(1e-12);
    let beta = (kappa.sqrt() - 1.0) / (kappa.sqrt() + 1.0);
    let np = k * (d + 1); // W row by row, then b
    let (mut th, mut v, mut g) = (vec![0f64; np], vec![0f64; np], vec![0f64; np]);
    let mut s = vec![0f64; k];
    let mut it = 0;
    while it < 3000 {
        it += 1;
        // the gradient at v
        g.fill(0.0);
        for i in 0..xs.len() {
            let zi = &z[i * d..(i + 1) * d];
            for c in 0..k {
                s[c] = v[k * d + c] + v[c * d..(c + 1) * d].iter().zip(zi).map(|(a, b)| a * b).sum::<f64>();
            }
            let m = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let sum: f64 = s.iter().map(|&x| (x - m).exp()).sum();
            for c in 0..k {
                let r = om[i] * ((s[c] - m).exp() / sum - if c == ys[i] { 1.0 } else { 0.0 });
                for (gw, &zz) in g[c * d..(c + 1) * d].iter_mut().zip(zi) {
                    *gw += r * zz;
                }
                g[k * d + c] += r;
            }
        }
        for j in 0..k * d {
            g[j] += lambda * v[j];
        }
        let big = g.iter().fold(0f64, |a, &x| a.max(x.abs()));
        // a step from v, then the momentum
        for j in 0..np {
            let new = v[j] - g[j] / lip;
            v[j] = new + beta * (new - th[j]);
            th[j] = new;
        }
        if big < 1e-7 {
            break;
        }
    }
    // back to the layer's values: w = W / sd, b = b - sum W mu / sd
    let mut w = vec![0f32; k * d];
    let mut b = vec![0f32; k];
    for c in 0..k {
        let mut bc = th[k * d + c];
        for i in 0..d {
            let wi = th[c * d + i] / sd[i];
            w[c * d + i] = wi as f32;
            bc -= wi * mu[i];
        }
        b[c] = bc as f32;
    }
    (w, b, it)
}

/// Fits a classifier to `classes` (each a list of vectors of d values; at least two classes, each with a sample).
/// `lambda`: the L2 penalty of the logistic regression; `norm`: each dimension's mean and spread to standardise by
/// (e.g. of the whole layer; None: of the samples).
pub fn fit(
    classes: &[Vec<Vec<f32>>],
    method: Method,
    lambda: f64,
    norm: Option<(&[f32], &[f32])>,
) -> Result<Classifier, String> {
    let t0 = Instant::now();
    let k = classes.len();
    if k < 2 {
        return Err("needs at least two classes".into());
    }
    if let Some(c) = classes.iter().position(|c| c.is_empty()) {
        return Err(format!("class {} has no sample", c + 1));
    }
    let d = classes[0][0].len();
    if d == 0 || classes.iter().flatten().any(|v| v.len() != d || v.iter().any(|x| !x.is_finite())) {
        return Err("the samples are not vectors of the same layer".into());
    }
    let xs: Vec<&[f32]> = classes.iter().flatten().map(Vec::as_slice).collect();
    let ys: Vec<usize> = classes.iter().enumerate().flat_map(|(c, v)| std::iter::repeat_n(c, v.len())).collect();
    let n = xs.len();
    let (mu, sd): (Vec<f64>, Vec<f64>) = match norm {
        Some((m, s)) if m.len() == d && s.len() == d => {
            (m.iter().map(|&x| f64::from(x)).collect(), s.iter().map(|&x| f64::from(x)).collect())
        }
        _ => {
            let mu: Vec<f64> = (0..d).map(|i| xs.iter().map(|x| f64::from(x[i])).sum::<f64>() / n as f64).collect();
            let sd = (0..d)
                .map(|i| (xs.iter().map(|x| (f64::from(x[i]) - mu[i]).powi(2)).sum::<f64>() / n as f64).sqrt())
                .collect();
            (mu, sd)
        }
    };
    // a dimension without spread (no data anywhere) is left as it is
    let sd: Vec<f64> = sd.into_iter().map(|s| if s > 1e-9 { s } else { 1.0 }).collect();
    let (w, b, iterations) = fit_raw(&xs, &ys, k, d, method, lambda, &mu, &sd);
    let mut c = Classifier {
        method,
        d,
        k,
        w,
        b,
        n: classes.iter().map(Vec::len).collect(),
        train_acc: 0.0,
        cv_acc: None,
        cv_folds: 0,
        iterations,
        ms: 0.0,
    };
    c.train_acc = xs.iter().zip(&ys).filter(|(x, y)| c.predict(x).0 == **y).count() as f64 / n as f64;
    // held out: leave-one-out up to 60 samples, else 5 folds, each class spread over them
    if c.n.iter().all(|&m| m >= 2) {
        let folds = if n <= 60 { n } else { 5 };
        let mut fold = vec![0usize; n];
        let mut seen = vec![0usize; k];
        for (i, &y) in ys.iter().enumerate() {
            fold[i] = if folds == n { i } else { seen[y] % folds };
            seen[y] += 1;
        }
        let mut right = 0;
        for f in 0..folds {
            let train: Vec<usize> = (0..n).filter(|&i| fold[i] != f).collect();
            let txs: Vec<&[f32]> = train.iter().map(|&i| xs[i]).collect();
            let tys: Vec<usize> = train.iter().map(|&i| ys[i]).collect();
            let (w, b, _) = fit_raw(&txs, &tys, k, d, method, lambda, &mu, &sd);
            let held = Classifier { w, b, ..c.clone() };
            right += (0..n).filter(|&i| fold[i] == f && held.predict(xs[i]).0 == ys[i]).count();
        }
        (c.cv_acc, c.cv_folds) = (Some(right as f64 / n as f64), folds);
    }
    c.ms = t0.elapsed().as_secs_f64() * 1000.0;
    Ok(c)
}

/// Each dimension's mean and standard deviation over up to 8192 vectors of every year: what the logistic
/// regression standardises by.
pub fn layer_norm(s: &EmbeddingStore) -> Option<(Vec<f32>, Vec<f32>)> {
    let sample = crate::views::sample_vectors(s, None, 8192, true, 0, None);
    if sample.len() < 2 {
        return None;
    }
    let d = s.d;
    let (mut sum, mut sq, mut v) = (vec![0f64; d], vec![0f64; d], vec![0f32; d]);
    for &(t, p) in &sample {
        s.dequant(t, p as usize, &mut v);
        for i in 0..d {
            sum[i] += f64::from(v[i]);
            sq[i] += f64::from(v[i]) * f64::from(v[i]);
        }
    }
    let n = sample.len() as f64;
    let mean: Vec<f32> = sum.iter().map(|&x| (x / n) as f32).collect();
    let sd = (0..d).map(|i| ((sq[i] / n - (sum[i] / n).powi(2)).max(0.0)).sqrt() as f32).collect();
    Some((mean, sd))
}

/// Year t classified: each pixel's class (NONE: no vector, or a score below `min_score`) and score (NaN: no
/// vector).
pub fn classify_year(
    s: &EmbeddingStore,
    t: usize,
    c: &Classifier,
    min_score: f32,
    pool: &ThreadPool,
) -> (Vec<u8>, Vec<f32>) {
    let (d, n, k) = (s.d, s.pixels(), c.k);
    let mut out = vec![(NONE, f32::NAN); n];
    let (Some(r), Some(year)) = (s.ranges(), s.year(t)) else { return out.into_iter().unzip() };
    if c.d != d || k == 0 {
        return out.into_iter().unzip();
    }
    // the store's scale and offset folded into the weights
    let mut w = vec![0f32; k * d];
    let mut bias = c.b.clone();
    for cc in 0..k {
        for i in 0..d {
            w[cc * d + i] = c.w[cc * d + i] * r.scale[i];
            bias[cc] += c.w[cc * d + i] * r.offset[i];
        }
    }
    par_chunks(pool, &mut out, 1, 8192, &|i0, o| {
        const M: usize = 1024;
        let (mut part, mut scores) = (vec![0f32; M * 4], vec![0f32; M * k]);
        let mut j = 0;
        while j < o.len() {
            let m = M.min(o.len() - j);
            let q = &year.q[(i0 + j) * d..(i0 + j + m) * d];
            // the kernel takes up to four outputs: the classes four at a time
            for g in (0..k).step_by(4) {
                let kg = 4.min(k - g);
                simd::project(q, m, d, &w[g * d..(g + kg) * d], kg, &bias[g..g + kg], &mut part);
                for i in 0..m {
                    scores[i * k + g..i * k + g + kg].copy_from_slice(&part[i * kg..(i + 1) * kg]);
                }
            }
            for i in 0..m {
                let p = i0 + j + i;
                if year.valid[p] == 0 {
                    continue;
                }
                let (cl, sc) = c.decide(&scores[i * k..(i + 1) * k], year.norm[p]);
                o[j + i] = (if sc >= min_score { cl as u8 } else { NONE }, sc);
            }
            j += m;
        }
    });
    out.into_iter().unzip()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eigen::Rng;

    /// k classes around random centres in d dimensions, `per` samples each, spread `noise`.
    fn blobs(k: usize, d: usize, per: usize, noise: f64, seed: u64) -> (Vec<Vec<f32>>, Vec<Vec<Vec<f32>>>) {
        let mut rng = Rng::new(seed);
        let centres: Vec<Vec<f32>> = (0..k).map(|_| (0..d).map(|_| rng.normal() as f32).collect()).collect();
        let classes = centres
            .iter()
            .map(|c| (0..per).map(|_| c.iter().map(|&x| x + (noise * rng.normal()) as f32).collect()).collect())
            .collect();
        (centres, classes)
    }

    #[test]
    fn few_shots_separate_classes() {
        for method in [Method::Logistic, Method::Centroid] {
            let (centres, classes) = blobs(4, 64, 3, 0.3, 7);
            let c = fit(&classes, method, 0.1, None).unwrap();
            assert_eq!(c.train_acc, 1.0, "{method:?}");
            assert_eq!(c.cv_acc, Some(1.0), "{method:?}");
            assert_eq!(c.cv_folds, 12);
            // new vectors near each centre
            let (_, test) = blobs(4, 64, 20, 0.3, 7);
            for (y, vs) in test.iter().enumerate() {
                for v in vs {
                    assert_eq!(c.predict(v).0, y, "{method:?}");
                }
            }
            assert_eq!(c.predict(&centres[2]).0, 2);
        }
    }

    #[test]
    fn logistic_solves_its_problem() {
        // at the optimum the gradient vanishes: in the standardised space, sum_i om_i (p_i - y_i) z_i + lambda W = 0
        let (_, classes) = blobs(3, 8, 5, 1.0, 3);
        let lambda = 0.05;
        let c = fit(&classes, Method::Logistic, lambda, None).unwrap();
        let xs: Vec<&Vec<f32>> = classes.iter().flatten().collect();
        let n = xs.len() as f64;
        let mu: Vec<f64> = (0..8).map(|i| xs.iter().map(|x| f64::from(x[i])).sum::<f64>() / n).collect();
        let sd: Vec<f64> =
            (0..8).map(|i| (xs.iter().map(|x| (f64::from(x[i]) - mu[i]).powi(2)).sum::<f64>() / n).sqrt()).collect();
        let mut g = [0f64; 3 * 8];
        for (y, vs) in classes.iter().enumerate() {
            for v in vs {
                let s: Vec<f64> = (0..3)
                    .map(|cc| {
                        f64::from(c.b[cc]) + (0..8).map(|i| f64::from(c.w[cc * 8 + i]) * f64::from(v[i])).sum::<f64>()
                    })
                    .collect();
                let m = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                let sum: f64 = s.iter().map(|x| (x - m).exp()).sum();
                for cc in 0..3 {
                    let r = ((s[cc] - m).exp() / sum - f64::from(u8::from(cc == y))) / (3.0 * 5.0);
                    for i in 0..8 {
                        g[cc * 8 + i] += r * (f64::from(v[i]) - mu[i]) / sd[i];
                    }
                }
            }
        }
        for cc in 0..3 {
            for i in 0..8 {
                g[cc * 8 + i] += lambda * f64::from(c.w[cc * 8 + i]) * sd[i];
            }
        }
        let big = g.iter().fold(0f64, |a, x| a.max(x.abs()));
        assert!(big < 1e-4, "gradient {big} after {} iterations", c.iterations);
    }

    #[test]
    fn classifies_a_store_as_the_vectors() {
        // a synthetic store: the left half near class 0's centre, the right half near class 1's
        let (centres, classes) = blobs(2, 16, 4, 0.2, 11);
        let s = EmbeddingStore::synthetic(20, 10, 2, 16, 4.0, |t, p, v| {
            let c = &centres[usize::from(p % 20 >= 10)];
            for (i, x) in v.iter_mut().enumerate() {
                *x = c[i] * if t == 1 { 0.9 } else { 1.0 };
            }
        });
        let pool = crate::thread_pool(2);
        for method in [Method::Logistic, Method::Centroid] {
            let c = fit(&classes, method, 0.1, None).unwrap();
            for t in 0..2 {
                let (cl, sc) = classify_year(&s, t, &c, 0.0, &pool);
                let mut v = vec![0f32; 16];
                for p in 0..200 {
                    assert_eq!(cl[p], u8::from(p % 20 >= 10), "{method:?} {p}");
                    s.dequant(t, p, &mut v);
                    let (want, score) = c.predict(&v);
                    assert_eq!(usize::from(cl[p]), want);
                    assert!((sc[p] - score).abs() < 1e-4, "{} vs {score}", sc[p]);
                }
                // a minimum score above every one: no class
                let (none, _) = classify_year(&s, t, &c, 1.01, &pool);
                assert!(none.iter().all(|&x| x == NONE));
            }
        }
    }
}
