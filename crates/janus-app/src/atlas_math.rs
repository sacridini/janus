//! The Atlas' mathematics (atlas.rs): each pixel's series reduced to features (its dates averaged into at most
//! MAX_BINS bins, the gaps interpolated), their principal components, the exact nearest neighbours in that space
//! and UMAP's layout of them in a plane (McInnes, Healy & Melville 2018, as umap-learn computes it: the fuzzy
//! neighbourhood of each pixel, its union with its neighbours', and the layout optimised by stochastic gradient
//! descent with negative sampling; min_dist 0.1, spread 1). An inverted file over the samples finds, for any
//! pixel of the map, the sample nearest to it (its place in the plane). The work runs in the rayon pool the caller
//! installs (its threads: the processing threads).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rayon::prelude::*;

/// The most features a series is reduced to: consecutive dates averaged.
pub const MAX_BINS: usize = 128;
/// The principal components the neighbours are found in.
pub const COMPONENTS: usize = 16;
/// The neighbours of each pixel (UMAP's n_neighbors).
pub const NEIGHBOURS: usize = 15;
/// UMAP's curve for min_dist 0.1 and spread 1 (umap-learn's find_ab_params).
const A: f32 = 1.576_943_5;
const B: f32 = 0.895_060_9;
const NEGATIVES: f32 = 5.0;

/// The first date of each bin, and the number of dates at the end.
pub fn bin_edges(dates: usize) -> Vec<usize> {
    let per = dates.div_ceil(MAX_BINS).max(1);
    (0..dates.div_ceil(per)).map(|b| b * per).chain(std::iter::once(dates)).collect()
}

/// A pixel's features from its value at every date: each bin's mean, the bins without a value interpolated
/// linearly (held past the ends), standardized over the pixel's own bins if `shape` (its form, not its level).
/// False when fewer than half the bins have a value.
pub fn features(values: &[f32], edges: &[usize], shape: bool, out: &mut [f32]) -> bool {
    let nb = edges.len() - 1;
    let mut known = 0;
    for b in 0..nb {
        let (mut s, mut c) = (0f32, 0u32);
        for &v in values[edges[b]..edges[b + 1]].iter().filter(|v| v.is_finite()) {
            s += v;
            c += 1;
        }
        out[b] = if c > 0 { s / c as f32 } else { f32::NAN };
        known += usize::from(c > 0);
    }
    if known * 2 < nb || known == 0 {
        return false;
    }
    // the gaps: between two known bins linearly, held before the first and after the last
    let mut last: Option<usize> = None;
    for b in 0..nb {
        if !out[b].is_finite() {
            continue;
        }
        match last {
            None => {
                let first = out[b];
                out[..b].iter_mut().for_each(|v| *v = first);
            }
            Some(a) if b > a + 1 => {
                let (va, vb) = (out[a], out[b]);
                for (k, v) in out[a + 1..b].iter_mut().enumerate() {
                    *v = va + (vb - va) * (k + 1) as f32 / (b - a) as f32;
                }
            }
            _ => {}
        }
        last = Some(b);
    }
    if let Some(a) = last {
        let va = out[a];
        out[a + 1..nb].iter_mut().for_each(|v| *v = va);
    }
    if shape {
        let m = out[..nb].iter().sum::<f32>() / nb as f32;
        let sd = (out[..nb].iter().map(|v| (v - m) * (v - m)).sum::<f32>() / nb as f32).sqrt();
        out[..nb].iter_mut().for_each(|v| *v = if sd > 0.0 { (*v - m) / sd } else { 0.0 });
    }
    true
}

/// The principal components of rows of `dim` features.
pub struct Pca {
    pub dim: usize,
    pub d: usize,
    /// d rows of dim: the components, by variance
    comps: Vec<f32>,
    /// mean . each component
    offset: Vec<f32>,
    /// the share of the variance the d components explain
    pub explained: f64,
}

/// The eigenvalues and eigenvectors (columns, row-major n x n) of a symmetric matrix, by cyclic Jacobi rotations.
fn jacobi(a: &mut [f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut v = vec![0.0; n * n];
    (0..n).for_each(|i| v[i * n + i] = 1.0);
    for _sweep in 0..100 {
        let off: f64 = (0..n).flat_map(|p| (p + 1..n).map(move |q| (p, q))).map(|(p, q)| a[p * n + q].powi(2)).sum();
        if off < 1e-22 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let t = if theta == 0.0 { 1.0 } else { t };
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
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
    ((0..n).map(|i| a[i * n + i]).collect(), v)
}

impl Pca {
    /// The first `d` components of `n` rows of `dim` features.
    pub fn fit(x: &[f32], n: usize, dim: usize, d: usize) -> Pca {
        let d = d.min(dim).max(1);
        let mut mean = vec![0f64; dim];
        for row in x.chunks(dim) {
            row.iter().zip(&mut mean).for_each(|(&v, m)| *m += f64::from(v));
        }
        mean.iter_mut().for_each(|m| *m /= n.max(1) as f64);
        // the covariance, rows summed in parallel
        let cov = x
            .par_chunks(dim * 256)
            .map(|block| {
                let mut c = vec![0f64; dim * dim];
                let mut r = vec![0f64; dim];
                for row in block.chunks(dim) {
                    r.iter_mut().zip(row.iter().zip(&mean)).for_each(|(r, (&v, m))| *r = f64::from(v) - m);
                    for i in 0..dim {
                        let ri = r[i];
                        for j in i..dim {
                            c[i * dim + j] += ri * r[j];
                        }
                    }
                }
                c
            })
            .reduce(
                || vec![0f64; dim * dim],
                |mut a, b| {
                    a.iter_mut().zip(b).for_each(|(a, b)| *a += b);
                    a
                },
            );
        let mut c = cov;
        for i in 0..dim {
            for j in 0..i {
                c[i * dim + j] = c[j * dim + i];
            }
        }
        let (vals, vecs) = jacobi(&mut c, dim);
        let mut order: Vec<usize> = (0..dim).collect();
        order.sort_by(|&a, &b| vals[b].total_cmp(&vals[a]));
        let total: f64 = vals.iter().map(|v| v.max(0.0)).sum();
        let explained = order[..d].iter().map(|&k| vals[k].max(0.0)).sum::<f64>() / total.max(1e-300);
        let mut comps = vec![0f32; d * dim];
        for (r, &k) in order[..d].iter().enumerate() {
            for i in 0..dim {
                comps[r * dim + i] = vecs[i * dim + k] as f32;
            }
        }
        let offset = comps
            .chunks(dim)
            .map(|c| c.iter().zip(&mean).map(|(&c, &m)| f64::from(c) * m).sum::<f64>() as f32)
            .collect();
        Pca { dim, d, comps, offset, explained }
    }

    pub fn project(&self, row: &[f32], out: &mut [f32]) {
        // (row - mean) . c as row . c - mean . c: no copy of the row
        for (r, o) in out[..self.d].iter_mut().enumerate() {
            *o = lanes(row, &self.comps[r * self.dim..(r + 1) * self.dim], |x, c| x * c) - self.offset[r];
        }
    }
}

/// The squared distance, summed in 8 lanes (vectorized: a sum in order is not).
fn dist2(a: &[f32], b: &[f32]) -> f32 {
    lanes(a, b, |x, y| (x - y) * (x - y))
}

/// The sum of f over pairs of elements, in 8 independent lanes.
#[inline(always)]
fn lanes(a: &[f32], b: &[f32], f: impl Fn(f32, f32) -> f32) -> f32 {
    let mut acc = [0f32; 8];
    let ((ca, ra), (cb, rb)) = (a.as_chunks::<8>(), b.as_chunks::<8>());
    let tail: f32 = ra.iter().zip(rb).map(|(&x, &y)| f(x, y)).sum();
    for (x, y) in ca.iter().zip(cb) {
        for k in 0..8 {
            acc[k] += f(x[k], y[k]);
        }
    }
    acc.iter().sum::<f32>() + tail
}

/// Each row's `k` nearest other rows (exact): their indices and distances, nearest first.
pub fn knn(x: &[f32], n: usize, d: usize, k: usize, cancel: &AtomicBool) -> Option<(Vec<u32>, Vec<f32>)> {
    let k = k.min(n.saturating_sub(1)).max(1);
    let mut idx = vec![0u32; n * k];
    let mut dist = vec![f32::INFINITY; n * k];
    idx.par_chunks_mut(k).zip(dist.par_chunks_mut(k)).enumerate().for_each(|(i, (bi, bd))| {
        if i % 64 == 0 && cancel.load(Ordering::Relaxed) {
            return;
        }
        let xi = &x[i * d..(i + 1) * d];
        for j in 0..n {
            if j == i {
                continue;
            }
            let dd = dist2(xi, &x[j * d..(j + 1) * d]);
            if dd < bd[k - 1] {
                // insert in order
                let mut p = k - 1;
                while p > 0 && bd[p - 1] > dd {
                    bd[p] = bd[p - 1];
                    bi[p] = bi[p - 1];
                    p -= 1;
                }
                bd[p] = dd;
                bi[p] = j as u32;
            }
        }
        bd.iter_mut().for_each(|v| *v = v.sqrt());
    });
    (!cancel.load(Ordering::Relaxed)).then_some((idx, dist))
}

/// UMAP's graph: the edges (head, tail, weight) of the fuzzy union of each row's neighbourhood, both ways.
fn fuzzy_graph(idx: &[u32], dist: &[f32], n: usize, k: usize) -> Vec<(u32, u32, f32)> {
    let target = (k as f32).log2();
    let mean_all = dist.iter().filter(|d| d.is_finite()).sum::<f32>() / dist.len().max(1) as f32;
    let mut directed: Vec<(u32, u32, f32)> = Vec::with_capacity(n * k);
    for i in 0..n {
        let ds = &dist[i * k..(i + 1) * k];
        let rho = ds.iter().copied().find(|&d| d > 0.0).unwrap_or(0.0);
        let (mut lo, mut hi, mut mid) = (0f32, f32::INFINITY, 1f32);
        for _ in 0..64 {
            let psum: f32 = ds.iter().map(|&d| if d - rho > 0.0 { (-(d - rho) / mid).exp() } else { 1.0 }).sum();
            if (psum - target).abs() < 1e-5 {
                break;
            }
            if psum > target {
                hi = mid;
                mid = 0.5 * (lo + hi);
            } else {
                lo = mid;
                mid = if hi.is_infinite() { mid * 2.0 } else { 0.5 * (lo + hi) };
            }
        }
        let mean_i = ds.iter().sum::<f32>() / k as f32;
        let sigma = if rho > 0.0 { mid.max(1e-3 * mean_i) } else { mid.max(1e-3 * mean_all) };
        for (j, &d) in idx[i * k..(i + 1) * k].iter().zip(ds) {
            let w = if d - rho <= 0.0 { 1.0 } else { (-(d - rho) / sigma).exp() };
            directed.push((i as u32, *j, w));
        }
    }
    // the union: a + b - a b for each pair, both directions kept
    let mut pairs: Vec<(u32, u32, f32, bool)> =
        directed.iter().map(|&(i, j, w)| if i < j { (i, j, w, true) } else { (j, i, w, false) }).collect();
    pairs.sort_unstable_by_key(|p| (p.0, p.1));
    let mut edges = Vec::with_capacity(pairs.len() * 2);
    let mut p = 0;
    while p < pairs.len() {
        let (a, b) = (pairs[p].0, pairs[p].1);
        let (mut wf, mut wb) = (0f32, 0f32);
        while p < pairs.len() && pairs[p].0 == a && pairs[p].1 == b {
            if pairs[p].3 {
                wf = wf.max(pairs[p].2)
            } else {
                wb = wb.max(pairs[p].2)
            }
            p += 1;
        }
        let w = wf + wb - wf * wb;
        edges.push((a, b, w));
        edges.push((b, a, w));
    }
    edges
}

/// A small fast generator (splitmix64).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// The layout in a plane of the rows whose neighbours are `idx` / `dist` (k each), from `init` (2 per row, about
/// +-10): `epochs` of UMAP's descent, `snapshot` called with the layout every few epochs (and the share done).
/// None if cancelled. The edges are processed in parallel (each moves its points as umap-learn's parallel mode
/// does, without locks: the points as atomics).
#[allow(clippy::too_many_arguments)]
pub fn umap(
    idx: &[u32],
    dist: &[f32],
    n: usize,
    k: usize,
    init: &[f32],
    epochs: usize,
    cancel: &AtomicBool,
    snapshot: &mut dyn FnMut(&[f32], f32),
) -> Option<Vec<f32>> {
    let mut edges = fuzzy_graph(idx, dist, n, k);
    let wmax = edges.iter().map(|e| e.2).fold(0f32, f32::max);
    edges.retain(|e| e.2 >= wmax / epochs as f32);
    // each edge: its head, tail, epochs per sample, the next sample, the next negative sample
    let mut state: Vec<(u32, u32, f32, f32, f32)> =
        edges.iter().map(|&(i, j, w)| (i, j, wmax / w, wmax / w, wmax / w / NEGATIVES)).collect();
    let y: Vec<AtomicU32> = init.iter().map(|v| AtomicU32::new(v.to_bits())).collect();
    let ld = |i: usize| f32::from_bits(y[i].load(Ordering::Relaxed));
    let st = |i: usize, v: f32| y[i].store(v.to_bits(), Ordering::Relaxed);
    let clip = |v: f32| v.clamp(-4.0, 4.0);
    let chunk = (state.len() / (rayon::current_num_threads() * 8)).max(1024);
    for e in 0..epochs {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let alpha = 1.0 - e as f32 / epochs as f32;
        let ef = e as f32;
        state.par_chunks_mut(chunk).enumerate().for_each(|(c, part)| {
            let mut rng = Rng((e as u64) << 32 ^ c as u64 ^ 0x5DEE_CE66);
            for s in part.iter_mut() {
                if s.3 > ef {
                    continue;
                }
                let (i, j) = (s.0 as usize, s.1 as usize);
                let (yi, yj) = ([ld(2 * i), ld(2 * i + 1)], [ld(2 * j), ld(2 * j + 1)]);
                let d2 = (yi[0] - yj[0]).powi(2) + (yi[1] - yj[1]).powi(2);
                if d2 > 0.0 {
                    let coeff = -2.0 * A * B * d2.powf(B - 1.0) / (A * d2.powf(B) + 1.0);
                    let g = [clip(coeff * (yi[0] - yj[0])), clip(coeff * (yi[1] - yj[1]))];
                    st(2 * i, yi[0] + g[0] * alpha);
                    st(2 * i + 1, yi[1] + g[1] * alpha);
                    st(2 * j, yj[0] - g[0] * alpha);
                    st(2 * j + 1, yj[1] - g[1] * alpha);
                }
                s.3 += s.2;
                // the negative samples: random points pushed away
                let negs = ((ef - s.4) / (s.2 / NEGATIVES)).floor().max(0.0) as usize;
                for _ in 0..negs {
                    let q = (rng.next() % n as u64) as usize;
                    if q == i {
                        continue;
                    }
                    let yi = [ld(2 * i), ld(2 * i + 1)];
                    let yq = [ld(2 * q), ld(2 * q + 1)];
                    let d2 = (yi[0] - yq[0]).powi(2) + (yi[1] - yq[1]).powi(2);
                    let g = if d2 > 0.0 {
                        let coeff = 2.0 * B / ((0.001 + d2) * (A * d2.powf(B) + 1.0));
                        [clip(coeff * (yi[0] - yq[0])), clip(coeff * (yi[1] - yq[1]))]
                    } else {
                        [4.0, 4.0]
                    };
                    st(2 * i, yi[0] + g[0] * alpha);
                    st(2 * i + 1, yi[1] + g[1] * alpha);
                }
                s.4 += negs as f32 * s.2 / NEGATIVES;
            }
        });
        if e % 10 == 9 || e + 1 == epochs {
            let now: Vec<f32> = (0..2 * n).map(ld).collect();
            snapshot(&now, (e + 1) as f32 / epochs as f32);
        }
    }
    Some((0..2 * n).map(ld).collect())
}

/// The epochs UMAP runs for `n` rows (umap-learn's: 500 for small sets, 200 for large ones).
pub fn epochs_for(n: usize) -> usize {
    if n <= 10_000 { 500 } else { 200 }
}

/// The initial layout: the first two components, scaled to +-10 (umap-learn's init "pca"), a little noise.
pub fn initial_layout(x: &[f32], n: usize, d: usize) -> Vec<f32> {
    let mut y = vec![0f32; 2 * n];
    for i in 0..n {
        y[2 * i] = x[i * d];
        y[2 * i + 1] = if d > 1 { x[i * d + 1] } else { 0.0 };
    }
    let m = y.iter().fold(0f32, |a, v| a.max(v.abs())).max(1e-12);
    let mut rng = Rng(42);
    y.iter_mut().for_each(|v| *v = *v / m * 10.0 + ((rng.next() % 2001) as f32 / 1000.0 - 1.0) * 1e-4);
    y
}

/// Rows in blocks of 8, dimension by dimension (block b, dimension k: its 8 rows' values): 8 distances at once,
/// each lane a row (vectorized without a sum across lanes). The rows short of a block far away.
struct Blocks {
    d: usize,
    data: Vec<f32>,
}

const FAR: f32 = 1e18;

impl Blocks {
    fn new(rows: &[&[f32]], d: usize) -> Blocks {
        let nb = rows.len().div_ceil(8);
        let mut data = vec![FAR; nb * d * 8];
        for (r, row) in rows.iter().enumerate() {
            let (b, l) = (r / 8, r % 8);
            for k in 0..d {
                data[(b * d + k) * 8 + l] = row[k];
            }
        }
        Blocks { d, data }
    }

    /// The squared distances from `q` to the 8 rows of block `b`.
    #[inline(always)]
    fn dists(&self, q: &[f32], b: usize) -> [f32; 8] {
        let mut acc = [0f32; 8];
        let block = &self.data[b * self.d * 8..(b + 1) * self.d * 8];
        for (k, &qk) in q[..self.d].iter().enumerate() {
            let col: &[f32; 8] = block[k * 8..k * 8 + 8].try_into().unwrap();
            for l in 0..8 {
                let x = qk - col[l];
                acc[l] += x * x;
            }
        }
        acc
    }
}

/// An inverted file over rows: the k-means cells of the rows, each with its rows; a query searches the nearest
/// PROBE cells only, in the first SEARCH_DIMS components (most of the variation, in principal components): the
/// nearest row, or one nearly as near (placing every pixel of a map for its colour, 2.7 times faster than 3 cells
/// in 16 components).
const SEARCH_DIMS: usize = 8;
const PROBE: usize = 2;

pub struct Ivf {
    d: usize,
    centroids: Blocks,
    nc: usize,
    /// each cell's blocks in `rows`
    cells: Vec<(usize, usize)>,
    rows: Blocks,
    /// the row in each lane of `rows` (NONE: padding)
    ids: Vec<u32>,
}

impl Ivf {
    pub fn build(x: &[f32], n: usize, full: usize) -> Ivf {
        let d = full.min(SEARCH_DIMS);
        let x: Vec<f32> = x.chunks_exact(full).flat_map(|r| r[..d].iter().copied()).collect();
        let x = &x[..];
        let c = ((PROBE as f64 * n as f64).sqrt() as usize).clamp(1, 512).min(n.max(1));
        let mut centroids: Vec<f32> = (0..c).flat_map(|k| x[(k * n / c) * d..(k * n / c + 1) * d].to_vec()).collect();
        let mut assign = vec![0u32; n];
        for _ in 0..8 {
            assign.par_iter_mut().enumerate().for_each(|(i, a)| {
                let xi = &x[i * d..(i + 1) * d];
                *a = (0..c)
                    .map(|p| (dist2(xi, &centroids[p * d..(p + 1) * d]), p))
                    .min_by(|a, b| a.0.total_cmp(&b.0))
                    .map_or(0, |m| m.1) as u32;
            });
            let mut sums = vec![0f64; c * d];
            let mut counts = vec![0usize; c];
            for (i, &a) in assign.iter().enumerate() {
                let a = a as usize;
                counts[a] += 1;
                for t in 0..d {
                    sums[a * d + t] += f64::from(x[i * d + t]);
                }
            }
            for k in 0..c {
                if counts[k] > 0 {
                    for t in 0..d {
                        centroids[k * d + t] = (sums[k * d + t] / counts[k] as f64) as f32;
                    }
                }
            }
        }
        // the rows cell after cell, each cell padded to whole blocks
        let mut order: Vec<u32> = Vec::with_capacity(n + 8 * c);
        let mut cells = Vec::with_capacity(c);
        let mut members = vec![Vec::new(); c];
        for (i, &a) in assign.iter().enumerate() {
            members[a as usize].push(i as u32);
        }
        for m in &members {
            let start = order.len() / 8;
            order.extend(m);
            while !order.len().is_multiple_of(8) {
                order.push(u32::MAX);
            }
            cells.push((start, order.len() / 8));
        }
        let far = vec![FAR; d];
        let rows: Vec<&[f32]> = order
            .iter()
            .map(|&i| if i == u32::MAX { &far[..] } else { &x[i as usize * d..(i as usize + 1) * d] })
            .collect();
        let cent: Vec<&[f32]> = centroids.chunks_exact(d).collect();
        Ivf { d, centroids: Blocks::new(&cent, d), nc: c, cells, rows: Blocks::new(&rows, d), ids: order }
    }

    /// The row nearest to `q` (in its PROBE nearest cells).
    pub fn nearest(&self, q: &[f32]) -> u32 {
        let q = &q[..self.d];
        // the nearest cells, kept in order
        let mut near = [(f32::INFINITY, usize::MAX); PROBE];
        for b in 0..self.nc.div_ceil(8) {
            let ds = self.centroids.dists(q, b);
            for (l, &dd) in ds.iter().enumerate() {
                if dd < near[PROBE - 1].0 && b * 8 + l < self.nc {
                    near[PROBE - 1] = (dd, b * 8 + l);
                    for p in (1..PROBE).rev() {
                        if near[p].0 < near[p - 1].0 {
                            near.swap(p, p - 1);
                        }
                    }
                }
            }
        }
        let mut best = (f32::INFINITY, 0u32);
        for &(_, k) in near.iter().filter(|c| c.1 != usize::MAX) {
            let (a, b) = self.cells[k];
            for blk in a..b {
                let ds = self.rows.dists(q, blk);
                // the block's least first: most blocks hold nothing better
                let m = ds.iter().copied().fold(f32::INFINITY, f32::min);
                if m < best.0 {
                    let l = ds.iter().position(|&v| v == m).unwrap_or(0);
                    best = (m, self.ids[blk * 8 + l]);
                }
            }
        }
        best.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gauss(rng: &mut Rng) -> f32 {
        // Box-Muller
        let u = ((rng.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
        let v = ((rng.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
        ((-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()) as f32
    }

    #[test]
    fn features_fill_the_gaps() {
        let v = [1.0, f32::NAN, 3.0, f32::NAN, 5.0, 6.0, f32::NAN];
        let edges = bin_edges(7);
        let mut out = vec![0.0; 7];
        assert!(features(&v, &edges, false, &mut out));
        assert_eq!(out, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 6.0]);
        // too few values
        assert!(!features(&[1.0, f32::NAN, f32::NAN, f32::NAN], &bin_edges(4), false, &mut out));
        // long series averaged
        let e = bin_edges(1000);
        assert!(e.len() - 1 <= MAX_BINS && e[1] == 8 && *e.last().unwrap() == 1000);
    }

    #[test]
    fn pca_finds_the_main_direction() {
        let mut rng = Rng(1);
        let n = 2000;
        // along (1, 1, 0) mostly, a little noise
        let x: Vec<f32> = (0..n)
            .flat_map(|_| {
                let t = gauss(&mut rng) * 3.0;
                [t + 0.1 * gauss(&mut rng), t + 0.1 * gauss(&mut rng), 0.1 * gauss(&mut rng)]
            })
            .collect();
        let p = Pca::fit(&x, n, 3, 2);
        let mut out = [0.0; 2];
        p.project(&[1.0, 1.0, 0.0], &mut out);
        assert!(out[0].abs() > 1.3 && out[1].abs() < 0.1, "{out:?}");
        assert!(p.explained > 0.99);
    }

    #[test]
    fn neighbours_are_exact() {
        let mut rng = Rng(2);
        let (n, d) = (300, 4);
        let x: Vec<f32> = (0..n * d).map(|_| gauss(&mut rng)).collect();
        let (idx, dist) = knn(&x, n, d, 5, &AtomicBool::new(false)).unwrap();
        for i in [0, 17, 299] {
            let mut all: Vec<(f32, usize)> = (0..n)
                .filter(|&j| j != i)
                .map(|j| (dist2(&x[i * d..(i + 1) * d], &x[j * d..(j + 1) * d]).sqrt(), j))
                .collect();
            all.sort_by(|a, b| a.0.total_cmp(&b.0));
            for r in 0..5 {
                assert_eq!(idx[i * 5 + r] as usize, all[r].1);
                assert!((dist[i * 5 + r] - all[r].0).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn umap_separates_groups() {
        // three groups in 10 dimensions: apart in the plane, each together
        let mut rng = Rng(3);
        let (per, d) = (300, 10);
        let n = 3 * per;
        let x: Vec<f32> = (0..n)
            .flat_map(|i| (0..d).map(move |t| (i / per, t)).collect::<Vec<_>>())
            .map(|(g, t)| {
                let centre = if t == g { 8.0 } else { 0.0 };
                centre + gauss(&mut rng)
            })
            .collect();
        let cancel = AtomicBool::new(false);
        let (idx, dist) = knn(&x, n, d, NEIGHBOURS, &cancel).unwrap();
        let init = initial_layout(&x, n, d);
        let y = umap(&idx, &dist, n, NEIGHBOURS, &init, 200, &cancel, &mut |_, _| {}).unwrap();
        let centre = |g: usize| {
            let (mut a, mut b) = (0f32, 0f32);
            for i in g * per..(g + 1) * per {
                a += y[2 * i];
                b += y[2 * i + 1];
            }
            [a / per as f32, b / per as f32]
        };
        let spread = |g: usize| {
            let c = centre(g);
            ((g * per..(g + 1) * per).map(|i| (y[2 * i] - c[0]).powi(2) + (y[2 * i + 1] - c[1]).powi(2)).sum::<f32>()
                / per as f32)
                .sqrt()
        };
        let cs: Vec<[f32; 2]> = (0..3).map(centre).collect();
        let gap = |a: usize, b: usize| ((cs[a][0] - cs[b][0]).powi(2) + (cs[a][1] - cs[b][1]).powi(2)).sqrt();
        let widest = (0..3).map(spread).fold(0f32, f32::max);
        assert!(gap(0, 1).min(gap(1, 2)).min(gap(0, 2)) > 3.0 * widest, "gaps {cs:?}, spread {widest}");
    }

    #[test]
    fn the_inverted_file_finds_the_nearest() {
        let mut rng = Rng(4);
        let (n, d) = (2000, 6);
        let x: Vec<f32> = (0..n * d).map(|_| gauss(&mut rng)).collect();
        let ivf = Ivf::build(&x, n, d);
        // the rows themselves
        let hits = (0..200).filter(|&i| ivf.nearest(&x[i * d..(i + 1) * d]) as usize == i).count();
        assert_eq!(hits, 200);
        // principal components (variation falling with the component): what it finds nearly as near as the nearest
        let (n, d) = (5000, 16);
        let x: Vec<f32> = (0..n * d).map(|k| gauss(&mut rng) * 0.7f32.powi((k % d) as i32)).collect();
        let ivf = Ivf::build(&x, n, d);
        let mut close = 0;
        for _ in 0..300 {
            let q: Vec<f32> = (0..d).map(|k| gauss(&mut rng) * 0.7f32.powi(k as i32)).collect();
            let found = dist2(&q, &x[ivf.nearest(&q) as usize * d..][..d]);
            let best = (0..n).map(|j| dist2(&q, &x[j * d..(j + 1) * d])).fold(f32::INFINITY, f32::min);
            close += usize::from(found <= 1.5 * best);
        }
        assert!(close >= 270, "{close} of 300 within 1.5 times the nearest's squared distance");
    }
}
