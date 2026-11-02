//! Clusters of points: k-means (Lloyd's, started by k-means++ as scikit-learn's `KMeans`, the best of several
//! starts) and Ward's hierarchical clustering (scipy's `linkage(method="ward")`, by the nearest-neighbour chain,
//! cut into a number of clusters as `fcluster(criterion="maxclust")`). The spatial folds' (janus-ml's folds.rs)
//! and the k-means tool's (kmeans.rs) clusters.

use rayon::prelude::*;

/// SplitMix64 (janus-ml's train::Rng: the same draws, the same folds as before the move here).
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// A uniform number in [0, 1).
fn uniform(rng: &mut Rng) -> f64 {
    (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64
}

/// The squared distance, in eight running sums (the compiler vectorizes them and their additions overlap; a single
/// sum of floats it may not reorder). The same sums in the same order with any vector width: the same bits on any
/// CPU (no fused multiply-add).
#[inline(always)]
fn d2(a: &[f64], b: &[f64]) -> f64 {
    let mut s = [0.0f64; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    for (x, y) in ca.iter().zip(cb) {
        for j in 0..8 {
            let d = x[j] - y[j];
            s[j] += d * d;
        }
    }
    let mut tail = 0.0;
    for (x, y) in ra.iter().zip(rb) {
        tail += (x - y) * (x - y);
    }
    ((s[0] + s[1]) + (s[2] + s[3])) + ((s[4] + s[5]) + (s[6] + s[7])) + tail
}

/// The distance kernels on a block of points (`xs`: m x p), compiled twice: for any x86-64 (SSE2) and for AVX2,
/// chosen once at run time.
mod kernels {
    use super::d2;
    use std::sync::OnceLock;

    #[inline(always)]
    fn nearest_body(xs: &[f64], p: usize, centres: &[f64], k: usize, out: &mut [(usize, f64)]) {
        for (v, o) in xs.chunks_exact(p).zip(out.iter_mut()) {
            let mut best = (0, f64::INFINITY);
            for c in 0..k {
                let d = d2(v, &centres[c * p..(c + 1) * p]);
                if d < best.1 {
                    best = (c, d);
                }
            }
            *o = best;
        }
    }

    #[inline(always)]
    fn min_body(xs: &[f64], p: usize, cands: &[f64], prev: &[f64], out: &mut [f64]) {
        let m = cands.len() / p;
        for ((v, &d), o) in xs.chunks_exact(p).zip(prev).zip(out.chunks_exact_mut(m)) {
            for (t, c) in cands.chunks_exact(p).enumerate() {
                o[t] = d.min(d2(v, c));
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn nearest_avx2(xs: &[f64], p: usize, centres: &[f64], k: usize, out: &mut [(usize, f64)]) {
        nearest_body(xs, p, centres, k, out)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2")]
    unsafe fn min_avx2(xs: &[f64], p: usize, cands: &[f64], prev: &[f64], out: &mut [f64]) {
        min_body(xs, p, cands, prev, out)
    }

    fn avx2() -> bool {
        static A: OnceLock<bool> = OnceLock::new();
        #[cfg(target_arch = "x86_64")]
        return *A.get_or_init(|| is_x86_feature_detected!("avx2"));
        #[cfg(not(target_arch = "x86_64"))]
        return *A.get_or_init(|| false);
    }

    /// Each point's nearest centre (of the k x p `centres`) and its squared distance.
    pub fn nearest(xs: &[f64], p: usize, centres: &[f64], k: usize, out: &mut [(usize, f64)]) {
        #[cfg(target_arch = "x86_64")]
        if avx2() {
            // SAFETY: the CPU has AVX2.
            return unsafe { nearest_avx2(xs, p, centres, k, out) };
        }
        nearest_body(xs, p, centres, k, out)
    }

    /// Each point's squared distance to each of the m candidates (`cands`: m x p) where below `prev`, else
    /// `prev` (`out`: points x m).
    pub fn min_to(xs: &[f64], p: usize, cands: &[f64], prev: &[f64], out: &mut [f64]) {
        #[cfg(target_arch = "x86_64")]
        if avx2() {
            // SAFETY: the CPU has AVX2.
            return unsafe { min_avx2(xs, p, cands, prev, out) };
        }
        min_body(xs, p, cands, prev, out)
    }
}

/// Points a thread takes at once in the distance kernels.
const KERNEL_BLOCK: usize = 512;

/// Points summed per block when the centres are updated: the blocks on the threads, their sums added in order (the
/// same centres whatever the threads).
const BLOCK: usize = 4096;

/// Each point's nearest centre and its squared distance, and each cluster's sum of its points (k x p) and their
/// count, in one pass over the points (a block's sums while its points are in the cache).
fn assign_sums(x: &[f64], p: usize, centres: &[f64], k: usize) -> (Vec<(usize, f64)>, Vec<f64>, Vec<usize>) {
    let mut near = vec![(0, 0.0); x.len() / p];
    let parts: Vec<(Vec<f64>, Vec<usize>)> = near
        .par_chunks_mut(BLOCK)
        .enumerate()
        .map(|(b, block)| {
            let i0 = b * BLOCK;
            for (j, o) in block.chunks_mut(KERNEL_BLOCK).enumerate() {
                let i = i0 + j * KERNEL_BLOCK;
                kernels::nearest(&x[i * p..(i + o.len()) * p], p, centres, k, o);
            }
            let (mut sums, mut counts) = (vec![0.0; k * p], vec![0usize; k]);
            for (o, &(c, _)) in block.iter().enumerate() {
                let i = i0 + o;
                counts[c] += 1;
                for (s, v) in sums[c * p..(c + 1) * p].iter_mut().zip(&x[i * p..(i + 1) * p]) {
                    *s += v;
                }
            }
            (sums, counts)
        })
        .collect();
    let (mut sums, mut counts) = (vec![0.0; k * p], vec![0usize; k]);
    for (s, c) in parts {
        for (a, b) in sums.iter_mut().zip(&s) {
            *a += b;
        }
        for (a, b) in counts.iter_mut().zip(&c) {
            *a += b;
        }
    }
    (near, sums, counts)
}

/// k-means of the `n` points `x` (n x p, row-major): each point's cluster (0..k), the centres (k x p) and the sum
/// of squared distances to them (the inertia).
pub struct KMeans {
    pub labels: Vec<usize>,
    pub centres: Vec<f64>,
    pub inertia: f64,
    pub iterations: usize,
}

/// k-means++'s start: the first centre a point at random, each next one a point drawn with a probability
/// proportional to its squared distance to the nearest centre so far (scikit-learn's greedy version: of 2 + ln k
/// draws, the one that lowers the inertia most).
fn plus_plus(x: &[f64], n: usize, p: usize, k: usize, rng: &mut Rng) -> Vec<f64> {
    let mut centres = Vec::with_capacity(k * p);
    let first = (rng.next_u64() % n as u64) as usize;
    centres.extend_from_slice(&x[first * p..(first + 1) * p]);
    let mut nearest: Vec<f64> = (0..n).map(|i| d2(&x[i * p..(i + 1) * p], &centres[..p])).collect();
    let trials = 2 + (k as f64).ln() as usize;
    for _ in 1..k {
        let total: f64 = nearest.iter().sum();
        // the candidates drawn first (from the same distances), then their distances in one pass over the points
        let picks: Vec<usize> = (0..trials)
            .map(|_| {
                if total > 0.0 {
                    let r = uniform(rng) * total;
                    let mut acc = 0.0;
                    let mut c = n - 1;
                    for (i, d) in nearest.iter().enumerate() {
                        acc += d;
                        if acc > r {
                            c = i;
                            break;
                        }
                    }
                    c
                } else {
                    (rng.next_u64() % n as u64) as usize
                }
            })
            .collect();
        let cands: Vec<f64> = picks.iter().flat_map(|&i| x[i * p..(i + 1) * p].iter().copied()).collect();
        let mut after = vec![0.0; n * trials];
        after.par_chunks_mut(KERNEL_BLOCK * trials).zip(nearest.par_chunks(KERNEL_BLOCK)).enumerate().for_each(
            |(b, (out, prev))| {
                let i = b * KERNEL_BLOCK;
                kernels::min_to(&x[i * p..(i + prev.len()) * p], p, &cands, prev, out);
            },
        );
        // the candidate that lowers the sum most (the first of equal sums)
        let mut sums = vec![0.0; trials];
        for row in after.chunks_exact(trials) {
            for (s, d) in sums.iter_mut().zip(row) {
                *s += d;
            }
        }
        let mut t_best = 0;
        for t in 1..trials {
            if sums[t] < sums[t_best] {
                t_best = t;
            }
        }
        let (pick, after): (usize, Vec<f64>) =
            (picks[t_best], after.iter().skip(t_best).step_by(trials).copied().collect());
        centres.extend_from_slice(&x[pick * p..(pick + 1) * p]);
        nearest = after;
    }
    centres
}

/// k-means of the points `x` (n x p) into `k` clusters, the best (least inertia) of `starts` runs from k-means++,
/// each of at most `max_iter` iterations, stopped when the centres move less than `tol` times the points' mean
/// variance (scikit-learn's defaults: 300, 1e-4). Fewer distinct points than k: some clusters empty, kept where
/// they were. The rayon pool of the caller's threads.
pub fn kmeans(x: &[f64], n: usize, p: usize, k: usize, starts: usize, seed: u64) -> KMeans {
    assert!(x.len() == n * p && n > 0 && p > 0 && k > 0, "x must be n x p, n, p and k positive");
    let k = k.min(n);
    let (max_iter, tol) = (300, 1e-4);
    let var: f64 = (0..p)
        .map(|j| {
            let m = (0..n).map(|i| x[i * p + j]).sum::<f64>() / n as f64;
            (0..n).map(|i| (x[i * p + j] - m).powi(2)).sum::<f64>() / n as f64
        })
        .sum::<f64>()
        / p.max(1) as f64;
    let tol = tol * var;
    let mut rng = Rng::new(seed);
    let mut best: Option<KMeans> = None;
    for _ in 0..starts.max(1) {
        let mut centres = plus_plus(x, n, p, k, &mut rng);
        let mut iterations = 0;
        let (mut near, mut sums, mut counts) = assign_sums(x, p, &centres, k);
        for it in 1..=max_iter {
            iterations = it;
            let mut shift = 0.0;
            for c in 0..k {
                if counts[c] == 0 {
                    continue;
                }
                for j in 0..p {
                    let v = sums[c * p + j] / counts[c] as f64;
                    shift += (v - centres[c * p + j]).powi(2);
                    centres[c * p + j] = v;
                }
            }
            (near, sums, counts) = assign_sums(x, p, &centres, k);
            if shift <= tol {
                break;
            }
        }
        let inertia: f64 = near.iter().map(|&(_, d)| d).sum();
        if best.as_ref().is_none_or(|b| inertia < b.inertia) {
            best = Some(KMeans { labels: near.iter().map(|&(c, _)| c).collect(), centres, inertia, iterations });
        }
    }
    best.unwrap()
}

/// The merges of Ward's hierarchical clustering of the `n` points `x` (n x p), as scipy's linkage matrix: n - 1
/// rows (a, b, height, size), a and b the clusters merged (points 0..n, the merge of row r is n + r), in the order
/// of their heights. The nearest-neighbour chain on the Lance-Williams update of the squared distances: n²/2
/// distances in memory (the caller keeps n to a few thousand).
pub fn ward(x: &[f64], n: usize, p: usize) -> Vec<(usize, usize, f64, usize)> {
    assert!(x.len() == n * p, "x must be n x p");
    if n < 2 {
        return vec![];
    }
    // the condensed matrix of squared distances: (i, j), i < j
    let at = |i: usize, j: usize| -> usize {
        let (i, j) = if i < j { (i, j) } else { (j, i) };
        n * i - i * (i + 1) / 2 + j - i - 1
    };
    let mut d: Vec<f64> = (0..n)
        .into_par_iter()
        .flat_map_iter(|i| (i + 1..n).map(move |j| (i, j)))
        .map(|(i, j)| d2(&x[i * p..(i + 1) * p], &x[j * p..(j + 1) * p]))
        .collect();
    let mut size = vec![1usize; n];
    let mut alive = vec![true; n];
    // the cluster a slot holds now (its id in the linkage)
    let mut id: Vec<usize> = (0..n).collect();
    let mut merges = Vec::with_capacity(n - 1);
    let mut chain: Vec<usize> = Vec::with_capacity(n);
    for _ in 0..n - 1 {
        if chain.is_empty() {
            chain.push((0..n).find(|&i| alive[i]).unwrap());
        }
        let (a, b) = loop {
            let x0 = *chain.last().unwrap();
            let prev = (chain.len() >= 2).then(|| chain[chain.len() - 2]);
            // the nearest of x0, the one before it in the chain on ties (so that the chain ends)
            let (mut y, mut best) = match prev {
                Some(pv) => (pv, d[at(x0, pv)]),
                None => (usize::MAX, f64::INFINITY),
            };
            for i in 0..n {
                if alive[i] && i != x0 {
                    let v = d[at(x0, i)];
                    if v < best {
                        best = v;
                        y = i;
                    }
                }
            }
            if Some(y) == prev {
                chain.pop();
                chain.pop();
                break (x0, y);
            }
            chain.push(y);
        };
        let (a, b) = if a < b { (a, b) } else { (b, a) };
        let (na, nb) = (size[a] as f64, size[b] as f64);
        let dab = d[at(a, b)];
        // the merged cluster in slot b; a gone
        for i in 0..n {
            if alive[i] && i != a && i != b {
                let ni = size[i] as f64;
                let t = na + nb + ni;
                let v = ((na + ni) * d[at(a, i)] + (nb + ni) * d[at(b, i)] - ni * dab) / t;
                d[at(b, i)] = v;
            }
        }
        merges.push((id[a], id[b], dab.max(0.0).sqrt(), size[a] + size[b]));
        alive[a] = false;
        size[b] += size[a];
        id[b] = n + merges.len() - 1;
    }
    // in the order of their heights, the merges renumbered (scipy: a stable sort, then the labels again)
    let mut order: Vec<usize> = (0..merges.len()).collect();
    order.sort_by(|&i, &j| merges[i].2.total_cmp(&merges[j].2));
    let mut new_of = vec![0usize; merges.len()];
    for (r, &o) in order.iter().enumerate() {
        new_of[o] = r;
    }
    let relabel = |c: usize| if c < n { c } else { n + new_of[c - n] };
    order
        .iter()
        .map(|&o| {
            let (a, b, h, s) = merges[o];
            let (a, b) = (relabel(a), relabel(b));
            (a.min(b), a.max(b), h, s)
        })
        .collect()
}

/// Each point's cluster (0..q, numbered by their first point) when the tree of `merges` (`ward`'s) is cut into `q`
/// clusters: its first n - q merges made.
pub fn cut(merges: &[(usize, usize, f64, usize)], n: usize, q: usize) -> Vec<usize> {
    let mut parent: Vec<usize> = (0..2 * n).collect();
    fn root(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for (r, &(a, b, _, _)) in merges.iter().take(n.saturating_sub(q.max(1))).enumerate() {
        let (ra, rb) = (root(&mut parent, a), root(&mut parent, b));
        parent[ra] = n + r;
        parent[rb] = n + r;
    }
    let mut label = std::collections::HashMap::new();
    (0..n)
        .map(|i| {
            let r = root(&mut parent, i);
            let next = label.len();
            *label.entry(r).or_insert(next)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three tight groups of points in 2-D.
    fn groups() -> (Vec<f64>, usize) {
        let mut rng = Rng::new(3);
        let mut x = vec![];
        for (cx, cy) in [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)] {
            for _ in 0..20 {
                x.push(cx + uniform(&mut rng));
                x.push(cy + uniform(&mut rng));
            }
        }
        (x, 60)
    }

    #[test]
    fn kmeans_finds_the_groups() {
        let (x, n) = groups();
        let k = kmeans(&x, n, 2, 3, 10, 1);
        for g in 0..3 {
            let l = k.labels[g * 20];
            assert!(k.labels[g * 20..(g + 1) * 20].iter().all(|&v| v == l), "group {g} split");
        }
        assert_eq!(k.labels.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
        // each centre about its group's middle (0.5 off the corner)
        assert!(k.inertia < 60.0 * 0.2, "{}", k.inertia);
    }

    /// The same clusters to the bit on any number of threads (the sums by fixed blocks, added in order), with more
    /// points than a block and a dimension the eight sums do not divide.
    #[test]
    fn kmeans_same_on_any_threads() {
        let (n, p) = (9000, 13);
        let mut rng = Rng::new(3);
        let x: Vec<f64> = (0..n * p).map(|i| uniform(&mut rng) + ((i / p) % 4) as f64).collect();
        let on = |threads: usize| {
            let pool = rayon::ThreadPoolBuilder::new().num_threads(threads).build().unwrap();
            pool.install(|| kmeans(&x, n, p, 5, 3, 7))
        };
        let (a, b) = (on(1), on(3));
        assert_eq!(a.labels, b.labels);
        assert_eq!(a.inertia.to_bits(), b.inertia.to_bits());
        assert!(a.centres.iter().zip(&b.centres).all(|(u, v)| u.to_bits() == v.to_bits()));
    }

    #[test]
    fn ward_by_hand() {
        // points at 0, 1, 5 and 6 on a line: {0,1} and {5,6} at 1, then together at Ward's
        // sqrt(2 * |A||B| / (|A| + |B|)) * |mean A - mean B| = sqrt(2) * 5
        let x = [0.0, 1.0, 5.0, 6.0];
        let m = ward(&x, 4, 1);
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].0, m[0].1, m[1].0, m[1].1), (0, 1, 2, 3));
        assert!((m[0].2 - 1.0).abs() < 1e-12 && (m[1].2 - 1.0).abs() < 1e-12);
        assert_eq!((m[2].0, m[2].1, m[2].3), (4, 5, 4));
        assert!((m[2].2 - 50f64.sqrt()).abs() < 1e-9, "{}", m[2].2);
        assert_eq!(cut(&m, 4, 2), vec![0, 0, 1, 1]);
        assert_eq!(cut(&m, 4, 4), vec![0, 1, 2, 3]);
        assert_eq!(cut(&m, 4, 1), vec![0; 4]);
        let (x, n) = groups();
        let l = cut(&ward(&x, n, 2), n, 3);
        assert_eq!(l.iter().collect::<std::collections::BTreeSet<_>>().len(), 3);
        for g in 0..3 {
            assert!(l[g * 20..(g + 1) * 20].iter().all(|&v| v == l[g * 20]));
        }
    }
}
