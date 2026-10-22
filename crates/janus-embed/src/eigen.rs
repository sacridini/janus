//! The leading eigenpairs of a covariance matrix by subspace iteration (src/embedding.cpp, topEigen), and a small
//! seeded generator (the same request gives the same sample).

/// SplitMix64: a seeded stream of u64, uniform doubles and normal ones (Box-Muller).
pub struct Rng {
    state: u64,
    spare: Option<f64>,
}

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng { state: seed, spare: None }
    }

    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// [0, 1)
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn normal(&mut self) -> f64 {
        if let Some(s) = self.spare.take() {
            return s;
        }
        let (u, v) = (1.0 - self.uniform(), self.uniform());
        let r = (-2.0 * u.ln()).sqrt();
        let a = 2.0 * std::f64::consts::PI * v;
        self.spare = Some(r * a.sin());
        r * a.cos()
    }
}

/// Cyclic Jacobi on a small symmetric n x n matrix (row-major, destroyed): the eigenvalues descending, the
/// eigenvectors as the columns of the second (n x n, row-major).
fn jacobi(a: &mut [f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _ in 0..100 {
        let (mut off, mut diag) = (0.0, 0.0);
        for i in 0..n {
            for j in 0..n {
                let x = a[i * n + j] * a[i * n + j];
                if i == j { diag += x } else { off += x }
            }
        }
        if off <= 1e-30 * diag.max(1e-300) {
            break;
        }
        for p in 0..n - 1 {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let (app, aqq) = (a[p * n + p], a[q * n + q]);
                let theta = (aqq - app) / (2.0 * apq);
                let t = (if theta >= 0.0 { 1.0 } else { -1.0 }) / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                // A = J^T A J
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let (vkp, vkq) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&x, &y| a[y * n + y].total_cmp(&a[x * n + x]));
    let vals = order.iter().map(|&o| a[o * n + o]).collect();
    let mut vs = vec![0.0; n * n];
    for (c, &o) in order.iter().enumerate() {
        for r in 0..n {
            vs[r * n + c] = v[r * n + o];
        }
    }
    (vals, vs)
}

/// The columns of q (d x b, column j at q[j*d..]) made orthonormal (modified Gram-Schmidt, twice).
pub fn orthonormalize(q: &mut [f64], d: usize, b: usize) {
    for _ in 0..2 {
        for j in 0..b {
            let (before, rest) = q.split_at_mut(j * d);
            let qj = &mut rest[..d];
            for i in 0..j {
                let qi = &before[i * d..(i + 1) * d];
                let dot: f64 = qi.iter().zip(qj.iter()).map(|(a, b)| a * b).sum();
                for (x, y) in qj.iter_mut().zip(qi) {
                    *x -= dot * y;
                }
            }
            let nn = qj.iter().map(|x| x * x).sum::<f64>().sqrt();
            if nn < 1e-300 {
                // a degenerate direction: any unit vector orthogonal enough
                qj.fill(0.0);
                qj[j % d] = 1.0;
                continue;
            }
            for x in qj.iter_mut() {
                *x /= nn;
            }
        }
    }
}

/// The top k eigenpairs of a symmetric d x d matrix (row-major) by subspace iteration (block power iteration on
/// k + 6 vectors with a Rayleigh-Ritz step, Jacobi on the small matrix), eigenvalues descending: (vectors k x d,
/// values, iterations).
pub fn top_eigen(c: &[f64], d: usize, k: usize) -> (Vec<f64>, Vec<f64>, usize) {
    let k = k.clamp(1, d);
    let b = d.min(k + 6); // guard vectors: convergence goes as (lambda_{b+1} / lambda_k)^iterations
    let mut rng = Rng::new(7);
    let mut q: Vec<f64> = (0..d * b).map(|_| rng.normal()).collect();
    orthonormalize(&mut q, d, b);
    let mut z = vec![0.0; d * b];
    let rayleigh_ritz = |q: &[f64], z: &mut [f64]| -> (Vec<f64>, Vec<f64>) {
        for j in 0..b {
            // Z = C Q
            let qj = &q[j * d..(j + 1) * d];
            for r in 0..d {
                z[j * d + r] = c[r * d..(r + 1) * d].iter().zip(qj).map(|(x, y)| x * y).sum();
            }
        }
        let mut h = vec![0.0; b * b];
        for a in 0..b {
            // H = Q^T Z (symmetric)
            for cc in a..b {
                let mut s = 0.0;
                for r in 0..d {
                    s += q[a * d + r] * z[cc * d + r] + q[cc * d + r] * z[a * d + r];
                }
                h[a * b + cc] = 0.5 * s;
                h[cc * b + a] = 0.5 * s;
            }
        }
        jacobi(&mut h, b)
    };
    let mut it = 0;
    loop {
        it += 1;
        let (_, v) = rayleigh_ritz(&q, &mut z);
        // a power step on the Ritz vectors: Q = orth(Z V) = orth(C Q V)
        let mut next = vec![0.0; d * b];
        for cc in 0..b {
            for a in 0..b {
                let vv = v[a * b + cc];
                for r in 0..d {
                    next[cc * d + r] += vv * z[a * d + r];
                }
            }
        }
        // converged when the k leading directions stop turning
        let mut worst = 0f64;
        if it > 1 {
            for cc in 0..k {
                let (o, qq) = (&next[cc * d..(cc + 1) * d], &q[cc * d..(cc + 1) * d]);
                let dot: f64 = o.iter().zip(qq).map(|(a, b)| a * b).sum();
                let nn: f64 = o.iter().map(|a| a * a).sum();
                worst = worst.max(1.0 - dot.abs() / nn.max(1e-300).sqrt());
            }
        }
        q = next;
        orthonormalize(&mut q, d, b);
        if (it > 1 && worst < 1e-12) || it >= 300 {
            break;
        }
    }
    let (lam, v) = rayleigh_ritz(&q, &mut z); // the final Ritz pairs
    let mut vecs = vec![0.0; k * d];
    for cc in 0..k {
        for a in 0..b {
            let vv = v[a * b + cc];
            for r in 0..d {
                vecs[cc * d + r] += vv * q[a * d + r];
            }
        }
    }
    (vecs, lam[..k].to_vec(), it)
}
