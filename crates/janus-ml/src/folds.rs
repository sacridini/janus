//! Spatial folds of a cross-validation, as blockCV's (Valavi et al. 2019, doi:10.1111/2041-210X.13107) and CAST's
//! kNNDM (Linnenbrink et al. 2024, doi:10.5194/gmd-17-5897-2024), Zeit's `spatial_folds` in Rust: the samples'
//! places (a polygon, a pin: a point each, in metres) dealt into folds that keep the places tested away from those
//! the model learns from, so that its accuracy is not that of places next to those it saw.
//!
//! - blocks: a grid of hexagons or squares `size` across, its blocks dealt into k folds at random (the most even of
//!   `iterations` draws, by class too), in rows (systematic), or as a checkerboard (squares, k = 2);
//! - buffer: each place tested without the places within `size` of it in the training;
//! - clusters: k clusters of the places by k-means of their values (the predictors, standardised);
//! - kNNDM: the places in q clusters (Ward's, or k-means), merged into k folds of about equal size, for q from k
//!   to the number of places; kept, the folds whose distances from each place to the nearest place of another fold
//!   are distributed most like those from the area to predict to the nearest place (the W statistic).
//!
//! And the range of the spatial autocorrelation of the predictors (blockCV's `cv_spatial_autocor`), the blocks'
//! size. The folds choose a model and calibrate its area of applicability; a probability sample is still what
//! estimates a map's accuracy (Wadoux et al. 2021, doi:10.1016/j.ecolmodel.2021.109692).

use serde::{Deserialize, Serialize};

use crate::cluster;
use crate::train::Rng;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    /// whole places at random (`train::split_by_place`)
    #[default]
    Places,
    Blocks,
    Buffer,
    Clusters,
    Knndm,
}

impl Method {
    pub const ALL: [Method; 5] = [Method::Places, Method::Blocks, Method::Buffer, Method::Clusters, Method::Knndm];

    pub fn name(self) -> &'static str {
        match self {
            Method::Places => "places",
            Method::Blocks => "blocks",
            Method::Buffer => "buffer",
            Method::Clusters => "clusters",
            Method::Knndm => "knndm",
        }
    }

    /// The name of the "split" a model keeps, back.
    pub fn of_name(s: &str) -> Option<Method> {
        Method::ALL.into_iter().find(|m| m.name() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            Method::Places => "Whole places at random",
            Method::Blocks => "Spatial blocks",
            Method::Buffer => "Buffer around the places tested",
            Method::Clusters => "Clusters of the values (k-means)",
            Method::Knndm => "kNNDM (distances as the map's)",
        }
    }

    /// Folds the places are dealt into (not the buffer's, nor whole places at random)
    pub fn has_folds(self) -> bool {
        matches!(self, Method::Blocks | Method::Clusters | Method::Knndm)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    #[default]
    Hexagon,
    Square,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Selection {
    #[default]
    Random,
    Systematic,
    Checkerboard,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Clustering {
    #[default]
    Hierarchical,
    Kmeans,
}

/// How the places are dealt into folds (the training's settings; Zeit's `spatial_folds` names and defaults).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FoldSpec {
    pub method: Method,
    pub k: usize,
    /// the blocks' size or the buffer's radius, in metres; 0: the range of the spatial autocorrelation (blocks)
    pub size: f64,
    pub shape: Shape,
    pub selection: Selection,
    pub iterations: usize,
    pub clustering: Clustering,
    pub maxp: f64,
    pub candidates: usize,
}

impl Default for FoldSpec {
    fn default() -> Self {
        FoldSpec {
            method: Method::Places,
            k: 5,
            size: 0.0,
            shape: Shape::Hexagon,
            selection: Selection::Random,
            iterations: 100,
            clustering: Clustering::Hierarchical,
            maxp: 0.5,
            candidates: 100,
        }
    }
}

/// A block of the grid: its outline (metres), its fold and the places in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub ring: Vec<[f64; 2]>,
    pub fold: usize,
    pub points: usize,
}

/// The places' folds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Folds {
    pub method: Method,
    pub k: usize,
    /// each place's fold (-1: in none)
    pub fold: Vec<i64>,
    /// the buffer's: each place's neighbours, out of the training when it is tested
    pub exclude: Vec<Vec<usize>>,
    pub size: f64,
    pub blocks: Vec<Block>,
    /// kNNDM's: the W statistic, the clusters q, and the distances from the area to predict to the nearest place
    /// (Gij), from each place to the nearest other (Gj) and to the nearest of another fold (Gj*)
    pub w: f64,
    pub q: usize,
    pub gij: Vec<f64>,
    pub gj: Vec<f64>,
    pub gjstar: Vec<f64>,
}

impl Folds {
    /// Each fold's places.
    pub fn counts(&self) -> Vec<usize> {
        let mut c = vec![0; self.k];
        for &f in &self.fold {
            if f >= 0 && (f as usize) < self.k {
                c[f as usize] += 1;
            }
        }
        c
    }
}

const SQRT3: f64 = 1.732_050_807_568_877_2;

/// Each point's hexagon (axial q, r) in a grid of pointy-top hexagons whose neighbouring centres are `size` apart,
/// from rounding the cube coordinates (numpy's round, half to even).
fn hexagon(x: f64, y: f64, size: f64, origin: [f64; 2]) -> (i64, i64) {
    let radius = size / SQRT3;
    let (px, py) = (x - origin[0], y - origin[1]);
    let q = (SQRT3 / 3.0 * px - py / 3.0) / radius;
    let r = (2.0 / 3.0 * py) / radius;
    let s = -q - r;
    let (mut rq, mut rr, rs) = (q.round_ties_even(), r.round_ties_even(), s.round_ties_even());
    let (dq, dr, ds) = ((rq - q).abs(), (rr - r).abs(), (rs - s).abs());
    let fix_q = dq > dr && dq > ds;
    let fix_r = !fix_q && dr > ds;
    if fix_q {
        rq = -rr - rs;
    }
    if fix_r {
        rr = -rq - rs;
    }
    (rq as i64, rr as i64)
}

fn hexagon_ring(q: i64, r: i64, size: f64, origin: [f64; 2]) -> Vec<[f64; 2]> {
    let radius = size / SQRT3;
    let cx = origin[0] + radius * SQRT3 * (q as f64 + r as f64 / 2.0);
    let cy = origin[1] + radius * 1.5 * r as f64;
    (0..=6)
        .map(|i| {
            let a = ((30 + 60 * (i % 6)) as f64).to_radians();
            [cx + radius * a.cos(), cy + radius * a.sin()]
        })
        .collect()
}

/// How far the folds are from holding a k-th of the points (of each class), a chi-square.
fn imbalance(folds: &[usize], k: usize, classes: Option<&[u32]>) -> f64 {
    let chi = |sel: &mut dyn Iterator<Item = usize>| {
        let mut count = vec![0f64; k];
        let mut n = 0.0;
        for f in sel {
            count[f] += 1.0;
            n += 1.0;
        }
        let e = n / k as f64;
        count.iter().map(|c| (c - e) * (c - e)).sum::<f64>() / e
    };
    match classes {
        None => chi(&mut folds.iter().copied()),
        Some(cl) => {
            let set: std::collections::BTreeSet<u32> = cl.iter().copied().collect();
            set.into_iter().map(|c| chi(&mut (0..folds.len()).filter(|&i| cl[i] == c).map(|i| folds[i]))).sum()
        }
    }
}

/// A permutation of 0..n (numpy's `permutation`: a Fisher-Yates shuffle).
fn permutation(n: usize, rng: &mut Rng) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).collect();
    rng.shuffle(&mut v);
    v
}

/// The places `xy` (metres) in blocks of a grid, the blocks in `k` folds.
#[allow(clippy::too_many_arguments)]
pub fn blocks(
    xy: &[[f64; 2]],
    k: usize,
    size: f64,
    shape: Shape,
    selection: Selection,
    classes: Option<&[u32]>,
    iterations: usize,
    seed: u64,
) -> Result<Folds, String> {
    let n = xy.len();
    check_k(k, n)?;
    if size.is_nan() || size <= 0.0 {
        return Err(format!("the blocks' size must be positive, not {size}"));
    }
    let origin =
        [xy.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min), xy.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min)];
    // each point's cell: (q, r) of a hexagon, (row, col) of a square; the blocks in the cells' order (np.unique)
    let cells: Vec<(i64, i64)> = xy
        .iter()
        .map(|p| match shape {
            Shape::Hexagon => hexagon(p[0], p[1], size, origin),
            Shape::Square => (((p[1] - origin[1]) / size).floor() as i64, ((p[0] - origin[0]) / size).floor() as i64),
        })
        .collect();
    let ids: Vec<(i64, i64)> = cells.iter().copied().collect::<std::collections::BTreeSet<_>>().into_iter().collect();
    let block: Vec<usize> = cells.iter().map(|c| ids.binary_search(c).unwrap()).collect();
    let nb = ids.len();
    if nb < k {
        return Err(format!(
            "only {nb} blocks of {size:.0} m hold places, fewer than the {k} folds: a smaller size or fewer folds"
        ));
    }
    let mut rng = Rng::new(seed);
    let of_block: Vec<usize> = match selection {
        Selection::Random => {
            let mut best: Option<(f64, Vec<usize>)> = None;
            for _ in 0..iterations.max(1) {
                let perm = permutation(nb, &mut rng);
                let of: Vec<usize> = perm.iter().map(|&i| i % k).collect();
                let at: Vec<usize> = block.iter().map(|&b| of[b]).collect();
                let score = imbalance(&at, k, classes);
                if best.as_ref().is_none_or(|b| score < b.0) {
                    best = Some((score, of));
                }
            }
            best.unwrap().1
        }
        Selection::Systematic => {
            // in rows: of hexagons, by r then q; of squares, by row then column
            let mut rank: Vec<usize> = (0..nb).collect();
            match shape {
                Shape::Hexagon => rank.sort_by_key(|&i| (ids[i].1, ids[i].0)),
                Shape::Square => rank.sort_by_key(|&i| ids[i]),
            }
            let mut of = vec![0; nb];
            for (j, &i) in rank.iter().enumerate() {
                of[i] = j % k;
            }
            of
        }
        Selection::Checkerboard => {
            if shape != Shape::Square {
                return Err("a checkerboard needs square blocks".into());
            }
            if k != 2 {
                return Err("a checkerboard gives 2 folds".into());
            }
            ids.iter().map(|&(r, c)| (r + c).rem_euclid(2) as usize).collect()
        }
    };
    let mut points = vec![0; nb];
    for &b in &block {
        points[b] += 1;
    }
    let blocks = ids
        .iter()
        .enumerate()
        .map(|(i, &(a, b))| Block {
            ring: match shape {
                Shape::Hexagon => hexagon_ring(a, b, size, origin),
                Shape::Square => {
                    let (x0, y0) = (origin[0] + b as f64 * size, origin[1] + a as f64 * size);
                    vec![[x0, y0], [x0 + size, y0], [x0 + size, y0 + size], [x0, y0 + size], [x0, y0]]
                }
            },
            fold: of_block[i],
            points: points[i],
        })
        .collect();
    Ok(Folds {
        method: Method::Blocks,
        k,
        fold: block.iter().map(|&b| of_block[b] as i64).collect(),
        size,
        blocks,
        ..Default::default()
    })
}

fn check_k(k: usize, n: usize) -> Result<(), String> {
    if n < 2 {
        return Err("at least two places are needed".into());
    }
    if !(2..=n).contains(&k) {
        return Err(format!("the folds must be between 2 and the {n} places, not {k}"));
    }
    Ok(())
}

/// Each place its own fold, its neighbours within `size` metres out of the training when it is tested.
pub fn buffer(xy: &[[f64; 2]], size: f64) -> Result<Folds, String> {
    if size.is_nan() || size <= 0.0 {
        return Err("a buffer needs its radius (metres)".into());
    }
    let n = xy.len();
    let r2 = size * size;
    let exclude = (0..n)
        .map(|i| {
            (0..n).filter(|&j| j != i && (xy[i][0] - xy[j][0]).powi(2) + (xy[i][1] - xy[j][1]).powi(2) <= r2).collect()
        })
        .collect();
    Ok(Folds { method: Method::Buffer, k: n, fold: (0..n as i64).collect(), exclude, size, ..Default::default() })
}

/// The values standardised (each column's mean and standard deviation, of `ddof`; a constant column left as it is).
fn standardise(v: &mut [f64], n: usize, p: usize, ddof: f64) {
    for j in 0..p {
        let m = (0..n).map(|i| v[i * p + j]).sum::<f64>() / n as f64;
        let s = ((0..n).map(|i| (v[i * p + j] - m).powi(2)).sum::<f64>() / (n as f64 - ddof).max(1.0)).sqrt();
        let s = if s > 0.0 { s } else { 1.0 };
        for i in 0..n {
            v[i * p + j] = (v[i * p + j] - m) / s;
        }
    }
}

/// `k` clusters of the places by k-means (10 starts) of their `values` (n x p), standardised.
pub fn clusters(values: &[f64], n: usize, p: usize, k: usize, seed: u64) -> Result<Folds, String> {
    check_k(k, n)?;
    let mut v = values.to_vec();
    standardise(&mut v, n, p, 0.0);
    let km = cluster::kmeans(&v, n, p, k, 10, seed);
    Ok(Folds { method: Method::Clusters, k, fold: km.labels.iter().map(|&l| l as i64).collect(), ..Default::default() })
}

/// Clusters merged into `k` folds of about equal size: the largest first (at random among equal sizes), each to the
/// fold with the fewest points.
fn merge(labels: &[usize], k: usize, rng: &mut Rng) -> Vec<usize> {
    let nc = labels.iter().max().map_or(0, |m| m + 1);
    let mut sizes = vec![0usize; nc];
    for &l in labels {
        sizes[l] += 1;
    }
    let mut order = permutation(nc, rng);
    order.sort_by_key(|&c| std::cmp::Reverse(sizes[c])); // stable
    let mut fold_of = vec![0; nc];
    let mut counts = vec![0usize; k];
    for c in order {
        let f = (0..k).min_by_key(|&f| counts[f]).unwrap();
        fold_of[c] = f;
        counts[f] += sizes[c];
    }
    labels.iter().map(|&l| fold_of[l]).collect()
}

/// The most places Ward's clustering takes (its distances in memory: n²/2); beyond, k-means.
pub const WARD_MAX: usize = 4000;

/// kNNDM's folds of the places `train` (n x p: metres, or standardised values) for the points of the area to
/// predict `pred` (m x p). `threads` for the distances.
#[allow(clippy::too_many_arguments)]
pub fn knndm(
    train: &[f64],
    n: usize,
    p: usize,
    pred: &[f64],
    m: usize,
    k: usize,
    clustering: Clustering,
    maxp: f64,
    candidates: usize,
    seed: u64,
    threads: usize,
) -> Result<Folds, String> {
    check_k(k, n)?;
    if m == 0 {
        return Err("the area to predict has no point".into());
    }
    let jobs = threads.max(1) as i32;
    let gij = zeit_core::knndm::nearest(train, n, p, pred, m, jobs);
    let all: Vec<i64> = (0..n as i64).collect();
    let gj = zeit_core::knndm::nearest_other(train, n, p, &all, jobs);
    // q from k to n, about evenly on a log scale (np.geomspace, rounded, distinct)
    let c = candidates.max(2);
    let (lk, ln) = ((k as f64).ln(), (n as f64).ln());
    let mut qs: Vec<usize> =
        (0..c).map(|i| (lk + (ln - lk) * i as f64 / (c - 1) as f64).exp().round_ties_even() as usize).collect();
    qs.sort_unstable();
    qs.dedup();
    let ward_tree = (clustering == Clustering::Hierarchical && n <= WARD_MAX).then(|| cluster::ward(train, n, p));
    let mut rng = Rng::new(seed);
    let mut best: Option<(Vec<usize>, f64, usize, Vec<f64>)> = None;
    for q in qs {
        let labels: Vec<usize> = if q >= n {
            (0..n).collect()
        } else if let Some(tree) = &ward_tree {
            cluster::cut(tree, n, q)
        } else {
            cluster::kmeans(train, n, p, q, 1, seed).labels
        };
        // the labels as 0..distinct (np.unique's inverse)
        let distinct: std::collections::BTreeSet<usize> = labels.iter().copied().collect();
        let distinct: Vec<usize> = distinct.into_iter().collect();
        let labels: Vec<usize> = labels.iter().map(|l| distinct.binary_search(l).unwrap()).collect();
        let folds = merge(&labels, k, &mut rng);
        let mut count = vec![0usize; k];
        for &f in &folds {
            count[f] += 1;
        }
        if *count.iter().max().unwrap() as f64 > maxp * n as f64 {
            continue;
        }
        let f64s: Vec<i64> = folds.iter().map(|&f| f as i64).collect();
        let gjstar = zeit_core::knndm::nearest_other(train, n, p, &f64s, jobs);
        let w = zeit_core::knndm::wasserstein(&gjstar, &gij);
        if best.as_ref().is_none_or(|b| w < b.1) {
            best = Some((folds, w, q, gjstar));
        }
    }
    let Some((folds, w, q, gjstar)) = best else {
        return Err(format!(
            "every way of dealing the places puts more than {:.0} % of them in one fold: the places are too \
             clustered for {k} folds (fewer folds)",
            maxp * 100.0
        ));
    };
    Ok(Folds {
        method: Method::Knndm,
        k,
        fold: folds.iter().map(|&f| f as i64).collect(),
        w,
        q,
        gij,
        gj,
        gjstar,
        ..Default::default()
    })
}

/// W of the places (n x p) dealt into k folds at random: kNNDM's yardstick (a random k-fold's distances).
pub fn random_w(train: &[f64], n: usize, p: usize, k: usize, gij: &[f64], seed: u64) -> f64 {
    let order = permutation(n, &mut Rng::new(seed));
    let mut fold = vec![0i64; n];
    for (j, &i) in order.iter().enumerate() {
        fold[i] = (j % k.max(1)) as i64;
    }
    zeit_core::knndm::wasserstein(&zeit_core::knndm::nearest_other(train, n, p, &fold, 1), gij)
}

/// The places' values for kNNDM in the space of the predictors: the places' and the area's, both standardised by
/// the places' (their standard deviation of n - 1).
pub fn standardise_pair(train: &mut [f64], n: usize, pred: &mut [f64], m: usize, p: usize) {
    for j in 0..p {
        let mean = (0..n).map(|i| train[i * p + j]).sum::<f64>() / n as f64;
        let sd = ((0..n).map(|i| (train[i * p + j] - mean).powi(2)).sum::<f64>() / (n as f64 - 1.0).max(1.0)).sqrt();
        let sd = if sd > 0.0 { sd } else { 1.0 };
        for i in 0..n {
            train[i * p + j] = (train[i * p + j] - mean) / sd;
        }
        for i in 0..m {
            pred[i * p + j] = (pred[i * p + j] - mean) / sd;
        }
    }
}

/// The range of a map's spatial autocorrelation: of each column of `values` (at the points `x`, `y`, metres; NaN
/// left out), the semivariogram up to `cutoff` in automap's bands and the model that fits it best (zeit-core's
/// variogram); the median of the ranges, and each column's (NaN: no model).
pub fn autocorrelation_range(
    x: &[f64],
    y: &[f64],
    columns: &[Vec<f64>],
    cutoff: f64,
    threads: usize,
) -> (f64, Vec<f64>) {
    let bands = zeit_core::variogram::automap_boundaries(cutoff);
    let ranges: Vec<f64> = columns
        .iter()
        .map(|v| {
            let keep: Vec<usize> = (0..v.len()).filter(|&i| v[i].is_finite()).collect();
            let (xs, ys, vs): (Vec<f64>, Vec<f64>, Vec<f64>) = (
                keep.iter().map(|&i| x[i]).collect(),
                keep.iter().map(|&i| y[i]).collect(),
                keep.iter().map(|&i| v[i]).collect(),
            );
            let e = zeit_core::variogram::empirical(&xs, &ys, &vs, &bands, threads.max(1) as i32);
            zeit_core::variogram::fit_best(&e).map_or(f64::NAN, |m| m.range)
        })
        .collect();
    let mut ok: Vec<f64> = ranges.iter().copied().filter(|r| r.is_finite()).collect();
    ok.sort_by(f64::total_cmp);
    let median = match ok.len() {
        0 => f64::NAN,
        l if l % 2 == 1 => ok[l / 2],
        l => 0.5 * (ok[l / 2 - 1] + ok[l / 2]),
    };
    (median, ranges)
}

/// The samples split for a validation by the places' folds (`place` of each sample): the fold held out the one
/// whose classes the training keeps best (each class of the validation also in the training; then as many classes
/// as can be; then the fold nearest a k-th of the samples). The buffer: the places of `share` of each class's
/// samples held out (`train::split_by_place`), their neighbours out of the training. Places in no fold train.
pub fn holdout(
    folds: &Folds,
    place: &[usize],
    y: &[u32],
    classes: usize,
    share: f64,
    seed: u64,
) -> (Vec<usize>, Vec<usize>) {
    let n = y.len();
    if folds.method == Method::Buffer {
        let (_, val, _) = crate::train::split_by_place(y, Some(place), classes, share, seed);
        let tested: std::collections::BTreeSet<usize> = val.iter().map(|&i| place[i]).collect();
        let mut out: std::collections::BTreeSet<usize> = tested.clone();
        for &pl in &tested {
            out.extend(folds.exclude.get(pl).into_iter().flatten().copied());
        }
        let train = (0..n).filter(|&i| !out.contains(&place[i])).collect();
        return (train, val);
    }
    let fold_of = |i: usize| folds.fold.get(place[i]).copied().unwrap_or(-1);
    let mut best: Option<((usize, usize, f64), i64)> = None;
    for f in 0..folds.k as i64 {
        let mut in_val = vec![0usize; classes];
        let mut in_train = vec![0usize; classes];
        for i in 0..n {
            if fold_of(i) == f { in_val[y[i] as usize] += 1 } else { in_train[y[i] as usize] += 1 }
        }
        let total: usize = in_val.iter().sum();
        if total == 0 {
            continue;
        }
        let lost = (0..classes).filter(|&c| in_val[c] > 0 && in_train[c] == 0).count();
        let present = (0..classes).filter(|&c| in_val[c] > 0).count();
        let off = (total as f64 - n as f64 / folds.k as f64).abs();
        let key = (lost, classes - present, off);
        let better = |b: &(usize, usize, f64)| (key.0, key.1).cmp(&(b.0, b.1)).then(key.2.total_cmp(&b.2)).is_lt();
        if best.as_ref().is_none_or(|b| better(&b.0)) {
            best = Some((key, f));
        }
    }
    let Some((_, f)) = best else { return ((0..n).collect(), vec![]) };
    (0..n).partition(|&i| fold_of(i) != f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(side: usize, step: f64) -> Vec<[f64; 2]> {
        (0..side * side).map(|i| [(i % side) as f64 * step, (i / side) as f64 * step]).collect()
    }

    #[test]
    fn hexagons_hold_their_points() {
        // a point is in its hexagon: nearer its centre than any other's
        let origin = [0.0, 0.0];
        let size = 10.0;
        let mut rng = Rng::new(1);
        for _ in 0..2000 {
            let x = (rng.next_u64() % 10_000) as f64 / 100.0;
            let y = (rng.next_u64() % 10_000) as f64 / 100.0;
            let (q, r) = hexagon(x, y, size, origin);
            let centre = |q: i64, r: i64| {
                let radius = size / SQRT3;
                [radius * SQRT3 * (q as f64 + r as f64 / 2.0), radius * 1.5 * r as f64]
            };
            let c = centre(q, r);
            let d = (x - c[0]).hypot(y - c[1]);
            for (dq, dr) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, -1), (-1, 1)] {
                let o = centre(q + dq, r + dr);
                assert!(d <= (x - o[0]).hypot(y - o[1]) + 1e-9, "({x}, {y}) nearer another hexagon");
            }
        }
    }

    #[test]
    fn blocks_keep_neighbours_together() {
        let xy = grid(20, 10.0);
        for shape in [Shape::Hexagon, Shape::Square] {
            for selection in [Selection::Random, Selection::Systematic] {
                let f = blocks(&xy, 5, 50.0, shape, selection, None, 50, 3).unwrap();
                assert_eq!(f.fold.len(), 400);
                assert!(f.counts().iter().all(|&c| c > 0), "{shape:?} {selection:?}: an empty fold");
                assert_eq!(f.blocks.iter().map(|b| b.points).sum::<usize>(), 400);
                // two points 10 m apart are mostly in the same block: the same fold
                let same = (0..400).filter(|&i| i % 20 < 19 && f.fold[i] == f.fold[i + 1]).count();
                assert!(same as f64 / 380.0 > 0.7, "{shape:?} {selection:?}: {same}");
            }
        }
        let c = blocks(&xy, 2, 50.0, Shape::Square, Selection::Checkerboard, None, 1, 0).unwrap();
        // squares of 5 x 5 points: (0, 0) and (50, 0) in different folds, (0, 0) and (100, 0) in the same
        assert_ne!(c.fold[0], c.fold[5]);
        assert_eq!(c.fold[0], c.fold[10]);
        assert_ne!(c.fold[0], c.fold[100]);
        assert!(blocks(&xy, 5, 1000.0, Shape::Square, Selection::Random, None, 1, 0).is_err());
        assert!(blocks(&xy, 3, 50.0, Shape::Hexagon, Selection::Checkerboard, None, 1, 0).is_err());
    }

    #[test]
    fn random_blocks_balance_the_classes() {
        let xy = grid(20, 10.0);
        let classes: Vec<u32> = (0..400).map(|i| ((i % 20) / 10) as u32).collect();
        let f = blocks(&xy, 4, 40.0, Shape::Square, Selection::Random, Some(&classes), 200, 1).unwrap();
        for c in 0..2 {
            let mut count = [0usize; 4];
            for i in (0..400).filter(|&i| classes[i] == c) {
                count[f.fold[i] as usize] += 1;
            }
            assert!(count.iter().all(|&v| v > 20), "class {c}: {count:?}");
        }
    }

    #[test]
    fn buffer_and_holdout() {
        let xy = grid(10, 10.0);
        let f = buffer(&xy, 15.0).unwrap();
        // inside: the 8 around (diagonals at 14.1 m)
        assert_eq!(f.exclude[11].len(), 8);
        assert_eq!(f.exclude[0].len(), 3);
        let y: Vec<u32> = (0..100).map(|i| (i % 2) as u32).collect();
        let place: Vec<usize> = (0..100).collect();
        let (tr, va) = holdout(&f, &place, &y, 2, 0.2, 1);
        assert_eq!(va.len(), 20);
        for &v in &va {
            assert!(!tr.contains(&v) && f.exclude[v].iter().all(|j| !tr.contains(j)));
        }
    }

    #[test]
    fn knndm_matches_the_map_distances() {
        // places in tight clumps, the map to predict far around them: the folds keep a clump together (W small
        // against a random k-fold's, which tests each place next to its clump)
        let mut rng = Rng::new(9);
        let mut u = || (rng.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        let mut train = vec![];
        for c in 0..20 {
            let (cx, cy) = ((c % 5) as f64 * 200.0, (c / 5) as f64 * 200.0);
            for _ in 0..10 {
                train.extend_from_slice(&[cx + 5.0 * u(), cy + 5.0 * u()]);
            }
        }
        let pred: Vec<f64> =
            (0..400).flat_map(|i| [(i % 20) as f64 * 200.0 - 1200.0, (i / 20) as f64 * 200.0 - 1200.0]).collect();
        let f = knndm(&train, 200, 2, &pred, 400, 5, Clustering::Hierarchical, 0.5, 30, 1, 2).unwrap();
        assert_eq!(f.fold.len(), 200);
        assert!(f.counts().iter().all(|&c| c > 0));
        for c in 0..20 {
            assert!(f.fold[c * 10..(c + 1) * 10].iter().all(|&v| v == f.fold[c * 10]), "clump {c} split");
        }
        let random: Vec<i64> = (0..200).map(|i| (i % 5) as i64).collect();
        let w_random =
            zeit_core::knndm::wasserstein(&zeit_core::knndm::nearest_other(&train, 200, 2, &random, 1), &f.gij);
        assert!(f.w < w_random, "{} vs a random k-fold's {w_random}", f.w);
        let km = knndm(&train, 200, 2, &pred, 400, 5, Clustering::Kmeans, 0.5, 30, 1, 2).unwrap();
        assert!(km.w < w_random);
    }

    #[test]
    fn the_range_of_a_smooth_map() {
        // a field smooth over hundreds of metres, and one rough over tens: a longer range
        let mut rng = Rng::new(4);
        let (mut x, mut y) = (vec![], vec![]);
        for _ in 0..800 {
            x.push((rng.next_u64() % 2000) as f64);
            y.push((rng.next_u64() % 2000) as f64);
        }
        let field = |s: f64| -> Vec<f64> { x.iter().zip(&y).map(|(a, b)| (a / s).sin() + (b / s).cos()).collect() };
        let cutoff = 2000.0 * 2f64.sqrt() * 0.35;
        let (smooth, each) = autocorrelation_range(&x, &y, &[field(150.0), field(150.0)], cutoff, 2);
        assert_eq!(each.len(), 2);
        assert_eq!(each[0], each[1]);
        let (rough, _) = autocorrelation_range(&x, &y, &[field(20.0)], cutoff, 2);
        assert!(smooth.is_finite() && rough.is_finite() && smooth > 2.0 * rough, "{smooth} {rough}");
    }
}
