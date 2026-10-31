//! A Random Forest classifier (Breiman 2001) of the series as the networks read them (each date's values, gaps
//! filled and scaled: the splits do not depend on the scaling): trees grown on bootstrap samples, each split the best
//! by the Gini impurity among `mtry` values drawn at random (more when those cannot split the node, as
//! scikit-learn's), down to leaves of `min_leaf` samples or pure; a pixel's probabilities the mean over the trees of
//! its leaf's share of each class (scikit-learn's `predict_proba`). Its out-of-bag accuracy (each sample by the
//! trees that did not see it) and each value's importance, the mean decrease of the impurity (scikit-learn's
//! `feature_importances_`, Zeit's weights of the area of applicability). Trees grown in parallel, each from its
//! own seed: the same forest whatever the threads.

use std::sync::atomic::{AtomicBool, Ordering};

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::data::Series;
use crate::train::{Epoch, Rng, accuracies};

/// The forest's settings (ranger's and scikit-learn's defaults for classification).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ForestHyper {
    pub trees: usize,
    /// values drawn at each split (0: the square root of their number)
    pub mtry: usize,
    /// samples in a leaf at least
    pub min_leaf: usize,
    /// a tree's depth at most (0: no limit)
    pub max_depth: usize,
    /// rasters read beside the series as further predictors (texture, a DEM...), a value each a sample
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<crate::extra::Extra>,
}

impl Default for ForestHyper {
    fn default() -> Self {
        ForestHyper { trees: 500, mtry: 0, min_leaf: 1, max_depth: 0, extra: vec![] }
    }
}

impl ForestHyper {
    /// The values drawn at each split, of `p`.
    pub fn mtry_of(&self, p: usize) -> usize {
        if self.mtry > 0 { self.mtry.min(p) } else { ((p as f64).sqrt().floor() as usize).clamp(1, p.max(1)) }
    }
}

const LEAF: u32 = u32::MAX;

/// A node: a split (`x[feature] <= threshold` to the left), or a leaf (`feature` LEAF, `left` the offset of its
/// classes' shares in the tree's `probs`).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Node {
    feature: u32,
    threshold: f32,
    left: u32,
    right: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Tree {
    nodes: Vec<Node>,
    probs: Vec<f32>,
}

impl Tree {
    /// The shares of the classes in the leaf of sample `x`.
    fn leaf<'a>(&'a self, x: &[f32], k: usize) -> &'a [f32] {
        let mut i = 0;
        loop {
            let n = self.nodes[i];
            if n.feature == LEAF {
                return &self.probs[n.left as usize..n.left as usize + k];
            }
            // NaN goes right (none in practice: the gaps are filled)
            i = if x[n.feature as usize] <= n.threshold { n.left } else { n.right } as usize;
        }
    }
}

/// A trained forest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Forest {
    /// values a sample has, and classes
    pub features: usize,
    pub classes: usize,
    trees: Vec<Tree>,
}

/// The training's result.
pub struct TrainedForest {
    pub forest: Forest,
    /// every few trees: `epoch` the trees grown, `train_acc` the out-of-bag accuracy, `val_acc` the validation's
    /// (losses: their mean log loss)
    pub history: Vec<Epoch>,
    /// the validation samples' confusion (rows: their classes; columns: the forest's), and the out-of-bag one
    pub confusion: Vec<Vec<u32>>,
    pub oob_confusion: Vec<Vec<u32>>,
    /// each value's mean decrease of the impurity, summing to 1
    pub importance: Vec<f64>,
    pub n_train: usize,
    pub n_val: usize,
    /// the validation split: "places" (a polygon's or a pin's samples on one side) or "samples"
    pub split: String,
}

impl TrainedForest {
    pub fn accuracies(&self) -> (f64, Vec<f64>, Vec<f64>) {
        accuracies(&self.confusion)
    }

    /// What the model file keeps of the training (as the networks' summary).
    pub fn summary(
        &self,
        hyper: &ForestHyper,
        validation_split: f64,
        seed: u64,
        per_class: &[usize],
    ) -> serde_json::Value {
        let (oa, ua, pa) = self.accuracies();
        let (ooa, oua, opa) = accuracies(&self.oob_confusion);
        serde_json::json!({
            "config": { "forest": hyper, "mtry": hyper.mtry_of(self.forest.features), "validation_split": validation_split, "seed": seed },
            "samples": { "per_class": per_class, "train": self.n_train, "validation": self.n_val, "split": self.split },
            "history": self.history,
            "best_epoch": self.forest.trees.len(),
            "stopped_early": false,
            "validation": { "confusion": self.confusion, "overall_accuracy": oa, "users_accuracy": ua, "producers_accuracy": pa },
            "oob": { "confusion": self.oob_confusion, "overall_accuracy": ooa, "users_accuracy": oua, "producers_accuracy": opa },
            "importance": self.importance,
        })
    }
}

/// A tree's seed: the forest's and the tree's number mixed (SplitMix64's first output).
fn tree_seed(seed: u64, tree: usize) -> u64 {
    Rng::new(seed ^ (tree as u64).wrapping_mul(0xD1B5_4A32_D192_ED03)).next_u64()
}

/// Grows a tree on the samples `bag` (indices into x, with repeats) of `x` (n x p, row-major) and `y`; adds its
/// impurity decreases to `importance` (p).
#[allow(clippy::too_many_arguments)]
fn grow(
    x: &[f32],
    p: usize,
    y: &[u32],
    k: usize,
    mut bag: Vec<u32>,
    h: &ForestHyper,
    rng: &mut Rng,
    importance: &mut [f64],
) -> Tree {
    let mtry = h.mtry_of(p);
    let min_leaf = h.min_leaf.max(1);
    let mut tree = Tree::default();
    let mut feats: Vec<u32> = (0..p as u32).collect();
    let mut vals: Vec<(f32, u32)> = Vec::with_capacity(bag.len());
    let (mut counts, mut left) = (vec![0usize; k], vec![0usize; k]);
    // (node, start, end, depth) of the nodes to make
    tree.nodes.push(Node { feature: LEAF, threshold: 0.0, left: 0, right: 0 });
    let mut stack = vec![(0usize, 0usize, bag.len(), 0usize)];
    while let Some((node, s, e, depth)) = stack.pop() {
        let m = e - s;
        counts.iter_mut().for_each(|c| *c = 0);
        for &i in &bag[s..e] {
            counts[y[i as usize] as usize] += 1;
        }
        let sum2: f64 = counts.iter().map(|&c| (c * c) as f64).sum();
        let pure = counts.iter().filter(|&&c| c > 0).count() <= 1;
        let mut best: Option<(f64, u32, f32)> = None;
        if !pure && m >= 2 * min_leaf && (h.max_depth == 0 || depth < h.max_depth) {
            // the values tried: mtry drawn, then more while none could split (all constant here)
            let mut tried = 0;
            while tried < p && (tried < mtry || best.is_none()) {
                let j = tried + (rng.next_u64() % (p - tried) as u64) as usize;
                feats.swap(tried, j);
                let f = feats[tried] as usize;
                tried += 1;
                vals.clear();
                vals.extend(bag[s..e].iter().map(|&i| (x[i as usize * p + f], y[i as usize])));
                vals.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
                if vals[0].0 == vals[m - 1].0 {
                    continue;
                }
                // the class counts left of each cut; the criterion: sum of squares over the sizes, each side
                left.iter_mut().for_each(|c| *c = 0);
                let (mut sl2, mut sr2) = (0f64, sum2);
                for i in 0..m - 1 {
                    let c = vals[i].1 as usize;
                    let (cl, cr) = (left[c] as f64, (counts[c] - left[c]) as f64);
                    sl2 += 2.0 * cl + 1.0;
                    sr2 -= 2.0 * cr - 1.0;
                    left[c] += 1;
                    let (nl, nr) = (i + 1, m - i - 1);
                    if nl < min_leaf || nr < min_leaf || vals[i].0 == vals[i + 1].0 {
                        continue;
                    }
                    let gain = sl2 / nl as f64 + sr2 / nr as f64 - sum2 / m as f64;
                    if best.is_none_or(|b| gain > b.0) {
                        let (a, b) = (vals[i].0, vals[i + 1].0);
                        let mut t = a + (b - a) / 2.0;
                        if t >= b {
                            t = a;
                        }
                        best = Some((gain, f as u32, t));
                    }
                }
            }
        }
        match best.filter(|b| b.0 > 1e-12) {
            Some((gain, f, t)) => {
                importance[f as usize] += gain;
                // the node's samples split in place: the left ones first
                let (mut a, mut b) = (s, e);
                while a < b {
                    if x[bag[a] as usize * p + f as usize] <= t {
                        a += 1;
                    } else {
                        b -= 1;
                        bag.swap(a, b);
                    }
                }
                let (l, r) = (tree.nodes.len(), tree.nodes.len() + 1);
                let empty = Node { feature: LEAF, threshold: 0.0, left: 0, right: 0 };
                tree.nodes.extend([empty, empty]);
                tree.nodes[node] = Node { feature: f, threshold: t, left: l as u32, right: r as u32 };
                stack.push((r, a, e, depth + 1));
                stack.push((l, s, a, depth + 1));
            }
            None => {
                let at = tree.probs.len() as u32;
                tree.probs.extend(counts.iter().map(|&c| c as f32 / m as f32));
                tree.nodes[node] = Node { feature: LEAF, threshold: 0.0, left: at, right: 0 };
            }
        }
    }
    tree
}

/// The cross-entropy and the accuracy of probabilities (n x k) against `y`, and their confusion.
fn score(probs: &[f64], y: &[u32], k: usize) -> (f64, f64, Vec<Vec<u32>>) {
    let mut confusion = vec![vec![0u32; k]; k];
    let (mut loss, mut right, mut n) = (0.0, 0usize, 0usize);
    for (i, &c) in y.iter().enumerate() {
        let p = &probs[i * k..(i + 1) * k];
        let total: f64 = p.iter().sum();
        if total <= 0.0 {
            continue;
        }
        let best = (0..k).fold(0, |b, j| if p[j] > p[b] { j } else { b });
        confusion[c as usize][best] += 1;
        right += usize::from(best == c as usize);
        loss -= (p[c as usize] / total).max(1e-15).ln();
        n += 1;
    }
    if n == 0 {
        return (f64::NAN, f64::NAN, confusion);
    }
    (loss / n as f64, right as f64 / n as f64, confusion)
}

/// Trains a forest on the samples (`train`) of `x` and `y` (0..k), the samples `val` its validation: `progress`
/// (trees grown, the numbers so far) follows it; `cancel` stops it between groups of trees.
#[allow(clippy::too_many_arguments)]
pub fn train(
    x: &Series,
    y: &[u32],
    k: usize,
    h: &ForestHyper,
    train_idx: &[usize],
    val_idx: &[usize],
    seed: u64,
    progress: &mut dyn FnMut(&Epoch),
    cancel: &AtomicBool,
) -> Result<TrainedForest, String> {
    let p = x.t * x.b;
    if train_idx.len() < 2 || p == 0 {
        return Err("too few samples to train on".into());
    }
    if k < 2 {
        return Err("at least two classes are needed".into());
    }
    if x.values.iter().any(|v| !v.is_finite()) {
        return Err("the samples have values missing (their gaps are filled before)".into());
    }
    let n = train_idx.len();
    // the training samples, contiguous
    let xt: Vec<f32> = train_idx.iter().flat_map(|&i| x.pixel(i).iter().copied()).collect();
    let yt: Vec<u32> = train_idx.iter().map(|&i| y[i]).collect();
    let xv: Vec<f32> = val_idx.iter().flat_map(|&i| x.pixel(i).iter().copied()).collect();
    let yv: Vec<u32> = val_idx.iter().map(|&i| y[i]).collect();
    let trees_n = h.trees.max(1);
    // trees a group: the progress and the curves' points
    let group = (trees_n / 40).clamp(8, 32);
    let mut forest = Forest { features: p, classes: k, trees: Vec::with_capacity(trees_n) };
    let mut importance = vec![0f64; p];
    let (mut oob, mut val) = (vec![0f64; n * k], vec![0f64; yv.len() * k]);
    let mut history = vec![];
    let t0 = std::time::Instant::now();
    for g0 in (0..trees_n).step_by(group) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let grown: Vec<(Tree, Vec<f64>, Vec<u32>)> = (g0..(g0 + group).min(trees_n))
            .into_par_iter()
            .map(|t| {
                let mut rng = Rng::new(tree_seed(seed, t));
                let bag: Vec<u32> = (0..n).map(|_| (rng.next_u64() % n as u64) as u32).collect();
                let mut seen = vec![false; n];
                for &i in &bag {
                    seen[i as usize] = true;
                }
                let mut imp = vec![0f64; p];
                let tree = grow(&xt, p, &yt, k, bag, h, &mut rng, &mut imp);
                // a tree's importances sum to 1 (scikit-learn's), then the mean over the trees
                let total: f64 = imp.iter().sum();
                if total > 0.0 {
                    imp.iter_mut().for_each(|v| *v /= total);
                }
                let out: Vec<u32> = (0..n as u32).filter(|&i| !seen[i as usize]).collect();
                (tree, imp, out)
            })
            .collect();
        for (tree, imp, out) in grown {
            for (a, b) in importance.iter_mut().zip(&imp) {
                *a += b;
            }
            for &i in &out {
                let i = i as usize;
                for (o, &q) in oob[i * k..(i + 1) * k].iter_mut().zip(tree.leaf(&xt[i * p..(i + 1) * p], k)) {
                    *o += f64::from(q);
                }
            }
            for i in 0..yv.len() {
                for (o, &q) in val[i * k..(i + 1) * k].iter_mut().zip(tree.leaf(&xv[i * p..(i + 1) * p], k)) {
                    *o += f64::from(q);
                }
            }
            forest.trees.push(tree);
        }
        let (train_loss, train_acc, _) = score(&oob, &yt, k);
        let (val_loss, val_acc, _) = score(&val, &yv, k);
        let e = Epoch {
            epoch: forest.trees.len(),
            train_loss,
            train_acc,
            val_loss,
            val_acc,
            lr: 0.0,
            seconds: t0.elapsed().as_secs_f64(),
        };
        progress(&e);
        history.push(e);
    }
    let total: f64 = importance.iter().sum();
    if total > 0.0 {
        importance.iter_mut().for_each(|v| *v /= total);
    }
    Ok(TrainedForest {
        confusion: score(&val, &yv, k).2,
        oob_confusion: score(&oob, &yt, k).2,
        forest,
        history,
        importance,
        n_train: n,
        n_val: yv.len(),
        split: String::new(),
    })
}

impl Forest {
    pub fn trees(&self) -> usize {
        self.trees.len()
    }

    /// The classes' probabilities (n x k) of n samples (n x features), on several threads.
    pub fn predict(&self, x: &[f32], n: usize) -> Vec<f32> {
        let (p, k) = (self.features, self.classes);
        let mut out = vec![0f32; n * k];
        let scale = 1.0 / self.trees.len().max(1) as f32;
        // a block of samples down each tree in turn: the tree stays in the cache (a sample down every tree evicts
        // them, a forest of large trees being tens of MB)
        out.par_chunks_mut(k * 256).zip(x.par_chunks(p * 256)).for_each(|(o, xs)| {
            let ok: Vec<bool> = xs.chunks(p).map(|xi| xi.iter().all(|v| v.is_finite())).collect();
            for t in &self.trees {
                for ((oi, xi), _) in o.chunks_mut(k).zip(xs.chunks(p)).zip(&ok).filter(|(_, ok)| **ok) {
                    for (a, &b) in oi.iter_mut().zip(t.leaf(xi, k)) {
                        *a += b;
                    }
                }
            }
            for (oi, &ok) in o.chunks_mut(k).zip(&ok) {
                if ok {
                    oi.iter_mut().for_each(|v| *v *= scale);
                } else {
                    oi.fill(f32::NAN);
                }
            }
        });
        out
    }

    /// The forest as bytes (little-endian): its sizes, then each tree's nodes and leaves' shares.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"JRF1");
        for v in [self.features, self.classes, self.trees.len()] {
            b.extend_from_slice(&(v as u32).to_le_bytes());
        }
        for t in &self.trees {
            b.extend_from_slice(&(t.nodes.len() as u32).to_le_bytes());
            for n in &t.nodes {
                b.extend_from_slice(&n.feature.to_le_bytes());
                b.extend_from_slice(&n.threshold.to_le_bytes());
                b.extend_from_slice(&n.left.to_le_bytes());
                b.extend_from_slice(&n.right.to_le_bytes());
            }
            b.extend_from_slice(&(t.probs.len() as u32).to_le_bytes());
            for v in &t.probs {
                b.extend_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    pub fn from_bytes(b: &[u8]) -> Result<Forest, String> {
        let bad = || "the forest is damaged".to_string();
        let mut at = 4;
        if b.get(..4) != Some(b"JRF1".as_slice()) {
            return Err(bad());
        }
        let u32_at = |at: &mut usize| -> Result<u32, String> {
            let v = b.get(*at..*at + 4).ok_or_else(bad)?;
            *at += 4;
            Ok(u32::from_le_bytes(v.try_into().map_err(|_| bad())?))
        };
        let (features, classes, n) = (u32_at(&mut at)? as usize, u32_at(&mut at)? as usize, u32_at(&mut at)?);
        let mut trees = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let nn = u32_at(&mut at)? as usize;
            let mut nodes = Vec::with_capacity(nn);
            for _ in 0..nn {
                let feature = u32_at(&mut at)?;
                let threshold = f32::from_bits(u32_at(&mut at)?);
                let (left, right) = (u32_at(&mut at)?, u32_at(&mut at)?);
                nodes.push(Node { feature, threshold, left, right });
            }
            let np = u32_at(&mut at)? as usize;
            let mut probs = Vec::with_capacity(np);
            for _ in 0..np {
                probs.push(f32::from_bits(u32_at(&mut at)?));
            }
            // every child and leaf inside the tree, every split's value inside a sample
            let ok = nodes.iter().all(|n| {
                if n.feature == LEAF {
                    n.left as usize + classes <= np
                } else {
                    (n.feature as usize) < features && (n.left as usize) < nn && (n.right as usize) < nn
                }
            });
            if !ok || nodes.is_empty() {
                return Err(bad());
            }
            trees.push(Tree { nodes, probs });
        }
        Ok(Forest { features, classes, trees })
    }
}

/// Each sample's classes' probabilities by a forest that never saw its place (scikit-learn's `cross_val_predict`
/// with a `GroupKFold`): the places dealt into `folds` (each class's places spread over them), a forest grown on
/// the other folds for each. A sample whose class has no place in the other folds (a class of one place) is not
/// predicted (NaN). And each value's importance, the folds' mean.
pub struct CrossPredicted {
    pub probs: Vec<f32>,
    pub importance: Vec<f64>,
    pub folds: usize,
}

/// Each sample's fold for a cross-validation by place: the places dealt class by class (shuffled) so that each fold
/// has some of each class, a place's samples all in one fold. The folds and their number (at most the places').
pub fn place_folds(y: &[u32], k: usize, places: &[usize], folds: usize, seed: u64) -> (Vec<usize>, usize) {
    let mut rng = Rng::new(seed);
    let mut fold_of = std::collections::HashMap::new();
    let mut next = 0usize;
    for c in 0..k as u32 {
        let mut of: Vec<usize> = (0..y.len())
            .filter(|&i| y[i] == c)
            .map(|i| places[i])
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        rng.shuffle(&mut of);
        for pl in of {
            fold_of.entry(pl).or_insert_with(|| {
                next += 1;
                next % folds.max(2)
            });
        }
    }
    let folds = folds.max(2).min(fold_of.len().max(2));
    ((0..y.len()).map(|i| fold_of[&places[i]] % folds).collect(), folds)
}

#[allow(clippy::too_many_arguments)]
pub fn cross_predict(
    x: &Series,
    y: &[u32],
    k: usize,
    places: &[usize],
    folds: usize,
    h: &ForestHyper,
    seed: u64,
    cancel: &AtomicBool,
) -> Result<CrossPredicted, String> {
    let (n, p) = (x.n, x.t * x.b);
    let mut probs = vec![f32::NAN; n * k];
    let mut importance = vec![0f64; p];
    let (fold_of, folds) = place_folds(y, k, places, folds, seed);
    let fold = |i: usize| fold_of[i];
    let mut grown = 0;
    for f in 0..folds {
        let (test, train_idx): (Vec<usize>, Vec<usize>) = (0..n).partition(|&i| fold(i) == f);
        let present: std::collections::BTreeSet<u32> = train_idx.iter().map(|&i| y[i]).collect();
        if test.is_empty() || present.len() < 2 {
            continue;
        }
        let t = train(x, y, k, h, &train_idx, &[], seed.wrapping_add(f as u64), &mut |_| {}, cancel)?;
        let xt: Vec<f32> = test.iter().flat_map(|&i| x.pixel(i).iter().copied()).collect();
        let pr = t.forest.predict(&xt, test.len());
        for (j, &i) in test.iter().enumerate() {
            if present.contains(&y[i]) {
                probs[i * k..(i + 1) * k].copy_from_slice(&pr[j * k..(j + 1) * k]);
            }
        }
        for (a, b) in importance.iter_mut().zip(&t.importance) {
            *a += b;
        }
        grown += 1;
    }
    if grown > 0 {
        importance.iter_mut().for_each(|v| *v /= grown as f64);
    }
    Ok(CrossPredicted { probs, importance, folds })
}

/// The samples split for the validation, by place: a place's samples (a polygon's pixels, a pin) all on one side, so
/// the validation meets places the forest never saw (`share` of each class's samples, about, in whole places; a
/// class of one place only for the training). Without places (or one place a sample), by sample. Returns the
/// training and validation indices, and how ("places" or "samples").
pub fn split_by_place(
    y: &[u32],
    places: Option<&[usize]>,
    k: usize,
    share: f64,
    seed: u64,
) -> (Vec<usize>, Vec<usize>, &'static str) {
    let Some(pl) = places.filter(|pl| {
        let distinct: std::collections::BTreeSet<_> = pl.iter().collect();
        distinct.len() < pl.len()
    }) else {
        let (a, b) = crate::train::split(y, k, share, seed);
        return (a, b, "samples");
    };
    let mut rng = Rng::new(seed);
    let (mut train, mut val) = (vec![], vec![]);
    for c in 0..k as u32 {
        let idx: Vec<usize> = (0..y.len()).filter(|&i| y[i] == c).collect();
        let mut of: Vec<usize> =
            idx.iter().map(|&i| pl[i]).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        rng.shuffle(&mut of);
        let want = (idx.len() as f64 * share).round() as usize;
        let mut chosen = std::collections::BTreeSet::new();
        let mut n_val = 0;
        for &place in of.iter().take(of.len().saturating_sub(1)) {
            if n_val >= want.max(1) {
                break;
            }
            chosen.insert(place);
            n_val += idx.iter().filter(|&&i| pl[i] == place).count();
        }
        for i in idx {
            if chosen.contains(&pl[i]) { val.push(i) } else { train.push(i) }
        }
    }
    train.sort_unstable();
    val.sort_unstable();
    (train, val, "places")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three classes of series (2 dates x 2 values): by the first value's level, the second value noise.
    fn data(n: usize, seed: u64) -> (Series, Vec<u32>) {
        let mut rng = Rng::new(seed);
        let mut u = || (rng.next_u64() >> 11) as f32 / (1u64 << 53) as f32;
        let (mut values, mut y) = (vec![], vec![]);
        for i in 0..n {
            let c = (i % 3) as u32;
            let level = c as f32 + 0.6 * u();
            values.extend_from_slice(&[level, u(), level + 0.1 * u(), u()]);
            y.push(c);
        }
        (Series { n, t: 2, b: 2, values }, y)
    }

    #[test]
    fn learns_and_keeps() {
        let (x, y) = data(600, 3);
        let all: Vec<usize> = (0..x.n).collect();
        let (tr, va) = (all[..450].to_vec(), all[450..].to_vec());
        let h = ForestHyper { trees: 60, ..Default::default() };
        let mut last = Epoch::default();
        let f = train(&x, &y, 3, &h, &tr, &va, 7, &mut |e| last = e.clone(), &AtomicBool::new(false)).unwrap();
        assert_eq!((f.forest.trees(), last.epoch), (60, 60));
        let (oa, _, _) = f.accuracies();
        assert!(oa > 0.97 && last.train_acc > 0.97, "validation {oa}, out of bag {}", last.train_acc);
        // the level (values 0 and 2) matters, the noise (1 and 3) little
        assert!(f.importance[0] + f.importance[2] > 0.8, "{:?}", f.importance);
        assert!((f.importance.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // probabilities sum to 1; the same after a round trip through bytes
        let p = f.forest.predict(&x.values, x.n);
        assert!(p.chunks(3).all(|q| (q.iter().sum::<f32>() - 1.0).abs() < 1e-5));
        let back = Forest::from_bytes(&f.forest.to_bytes()).unwrap();
        assert_eq!(back, f.forest);
        assert!(Forest::from_bytes(&f.forest.to_bytes()[..100]).is_err());
        // the same forest whatever the threads
        let pool = rayon::ThreadPoolBuilder::new().num_threads(1).build().unwrap();
        let one = pool.install(|| train(&x, &y, 3, &h, &tr, &va, 7, &mut |_| {}, &AtomicBool::new(false)).unwrap());
        assert_eq!(one.forest, f.forest);
    }

    #[test]
    fn predicts_each_place_unseen() {
        // 3 classes, places of 5 samples; one place of class 0 labelled 1: it is the one taken for another class
        let (x, mut y) = data(300, 5);
        let places: Vec<usize> = (0..300).map(|i| (i / 3) / 5 * 3 + i % 3).collect();
        let wrong: Vec<usize> = (0..300).filter(|&i| places[i] == 0).collect();
        for &i in &wrong {
            y[i] = 1;
        }
        let h = ForestHyper { trees: 40, ..Default::default() };
        let c = cross_predict(&x, &y, 3, &places, 5, &h, 1, &AtomicBool::new(false)).unwrap();
        let best = |i: usize| (0..3).fold(0, |b, j| if c.probs[i * 3 + j] > c.probs[i * 3 + b] { j } else { b });
        assert!(wrong.iter().all(|&i| best(i) == 0), "the mislabelled place taken for its own class");
        let right = (0..300).filter(|i| !wrong.contains(i) && best(*i) == y[*i] as usize).count();
        assert!(right as f64 / (300 - wrong.len()) as f64 > 0.95);
        assert!((c.importance.iter().sum::<f64>() - 1.0).abs() < 1e-9 && c.folds == 5);
        // a class of a single place: not predicted
        let mut y2 = y.clone();
        for i in 0..300 {
            if places[i] == 3 {
                y2[i] = 2;
            } else if y2[i] == 2 {
                y2[i] = 0;
            }
        }
        let c = cross_predict(&x, &y2, 3, &places, 5, &h, 1, &AtomicBool::new(false)).unwrap();
        assert!((0..300).all(|i| (y2[i] == 2) == c.probs[i * 3].is_nan()));
    }

    #[test]
    fn splits_whole_places() {
        // 3 classes of 4 places of 5 samples each
        let y: Vec<u32> = (0..60).map(|i| (i / 20) as u32).collect();
        let places: Vec<usize> = (0..60).map(|i| i / 5).collect();
        let (tr, va, how) = split_by_place(&y, Some(&places), 3, 0.25, 1);
        assert_eq!((how, tr.len() + va.len()), ("places", 60));
        let side = |i: usize| va.contains(&i);
        assert!((0..60).all(|i| side(i) == side(places[i] * 5)), "a place on both sides");
        for c in 0..3 {
            let v = va.iter().filter(|&&i| y[i] == c).count();
            assert_eq!(v, 5, "class {c}: one place of four to the validation");
        }
        // one sample a place: by sample
        let own: Vec<usize> = (0..60).collect();
        assert_eq!(split_by_place(&y, Some(&own), 3, 0.25, 1).2, "samples");
    }
}
