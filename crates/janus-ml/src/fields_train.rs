//! Training the field networks on labelled chips: the FTW/Trazo U-Net fine-tuned on its two windows (as Trazo
//! fine-tunes its checkpoints: the same weighted cross entropy, background 0.05, interior 0.2, boundary 0.75), and a
//! U-TAE on a year of monthly composites with the same three classes and a fourth output, each field pixel's
//! distance to its field's edge (0 at the edge, 1 at its middle: ResUNet-a's and cultionet's distance transform),
//! which the parcels' watershed then follows.
//!
//! A chip: its values ([steps][values][side][side], NaN without), its classes (0 background, 1 interior, 2
//! boundary, NONE unlabelled) and its distance (NaN where unlabelled). Augmented by the square's eight symmetries.

use std::sync::atomic::{AtomicBool, Ordering};

use burn::module::Module;
use burn::nn::loss::CrossEntropyLossConfig;
use burn::optim::{AdamWConfig, GradientsParams};
use burn::prelude::*;
use burn::tensor::activation::sigmoid;
use serde::{Deserialize, Serialize};

use crate::dense::{NONE, Standard};
use crate::fieldnet::FieldNet;
use crate::train::{Epoch, Rng};
use crate::utae::{Utae, UtaeHyper};

/// Trazo's class weights (background, interior, boundary).
pub const CLASS_WEIGHTS: [f32; 3] = [0.05, 0.2, 0.75];

#[derive(Clone, Debug, Default)]
pub struct Chip {
    pub t: usize,
    pub c: usize,
    pub side: usize,
    pub x: Vec<u16>,
    pub classes: Vec<u8>,
    pub dist: Vec<f32>,
}

/// Each field pixel's distance to its field's edge (pixels of another label or none, 8-connected steps; the
/// image's border is no edge), over the field's largest: 0 at the edge, 1 at the middle; 0 outside the fields.
pub fn distance_to_edge(labels: &[u32], w: usize, h: usize) -> Vec<f32> {
    let n = w * h;
    let mut d = vec![u32::MAX; n];
    let mut queue = std::collections::VecDeque::new();
    let nb = |i: usize| -> [Option<usize>; 8] {
        let (x, y) = ((i % w) as i64, (i / w) as i64);
        let mut out = [None; 8];
        let mut k = 0;
        for dy in -1..=1i64 {
            for dx in -1..=1i64 {
                if dx == 0 && dy == 0 {
                    continue;
                }
                let (xx, yy) = (x + dx, y + dy);
                if xx >= 0 && yy >= 0 && (xx as usize) < w && (yy as usize) < h {
                    out[k] = Some(yy as usize * w + xx as usize);
                }
                k += 1;
            }
        }
        out
    };
    for i in 0..n {
        if labels[i] == 0 {
            continue;
        }
        // an edge pixel: a 4-neighbour of another label
        let (x, y) = (i % w, i / w);
        let other = (x > 0 && labels[i - 1] != labels[i])
            || (x + 1 < w && labels[i + 1] != labels[i])
            || (y > 0 && labels[i - w] != labels[i])
            || (y + 1 < h && labels[i + w] != labels[i]);
        if other {
            d[i] = 0;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        for j in nb(i).into_iter().flatten() {
            if labels[j] == labels[i] && d[j] == u32::MAX {
                d[j] = d[i] + 1;
                queue.push_back(j);
            }
        }
    }
    // a field with no edge in the image (it covers it): its distance from the image's border
    let mut most = std::collections::HashMap::<u32, u32>::new();
    for i in 0..n {
        if labels[i] > 0 && d[i] != u32::MAX {
            let m = most.entry(labels[i]).or_insert(0);
            *m = (*m).max(d[i]);
        }
    }
    (0..n)
        .map(|i| {
            if labels[i] == 0 {
                return 0.0;
            }
            match (d[i], most.get(&labels[i])) {
                (u32::MAX, _) | (_, None) => 1.0,
                (v, Some(&m)) => if m == 0 { 0.0 } else { v as f32 / m as f32 },
            }
        })
        .collect()
}

/// The classes of instance labels: a field's pixels with a 4-neighbour outside it (of another field or none) its
/// boundary, the rest its interior; 0 outside.
pub fn classes_of(labels: &[u32], w: usize, h: usize) -> Vec<u8> {
    (0..w * h)
        .map(|i| {
            let l = labels[i];
            if l == 0 {
                return 0;
            }
            let (x, y) = (i % w, i / w);
            let edge = (x > 0 && labels[i - 1] != l)
                || (x + 1 < w && labels[i + 1] != l)
                || (y > 0 && labels[i - w] != l)
                || (y + 1 < h && labels[i + w] != l);
            if edge { 2 } else { 1 }
        })
        .collect()
}

/// A crop (x0, y0, side) of a chip turned by one of the square's eight symmetries (k: bit 0 flips x, bit 1 flips
/// y, bit 2 swaps the axes).
fn crop(c: &Chip, x0: usize, y0: usize, s: usize, k: u8) -> (Vec<f32>, Vec<u8>, Vec<f32>) {
    let side = c.side;
    let at = |u: usize, v: usize| -> usize {
        let (mut a, mut b) = (u, v);
        if k & 4 != 0 {
            (a, b) = (b, a);
        }
        if k & 1 != 0 {
            a = s - 1 - a;
        }
        if k & 2 != 0 {
            b = s - 1 - b;
        }
        (y0 + b) * side + x0 + a
    };
    let planes = c.t * c.c;
    let mut x = vec![0f32; planes * s * s];
    let mut y = vec![NONE; s * s];
    let mut d = vec![f32::NAN; s * s];
    for v in 0..s {
        for u in 0..s {
            let src = at(u, v);
            let dst = v * s + u;
            for p in 0..planes {
                let v = c.x[p * side * side + src];
                x[p * s * s + dst] = if v == 0 { f32::NAN } else { v as f32 / crate::fields::NORMALIZATION };
            }
            y[dst] = c.classes[src];
            d[dst] = c.dist[src];
        }
    }
    (x, y, d)
}

/// The training's settings.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldTrainConfig {
    pub epochs: usize,
    pub batch: usize,
    pub lr: f64,
    pub patience: usize,
    pub validation: f64,
    /// the crops' side (the chips' side when as large)
    pub crop: usize,
    /// crops a chip an epoch
    pub crops: usize,
    /// the distance's weight in the loss (U-TAE)
    pub dist_weight: f64,
    pub seed: u64,
}

impl Default for FieldTrainConfig {
    fn default() -> Self {
        FieldTrainConfig {
            epochs: 60,
            batch: 4,
            lr: 1e-3,
            patience: 10,
            validation: 0.1,
            crop: 128,
            crops: 2,
            dist_weight: 1.0,
            seed: 42,
        }
    }
}

/// What a training gives back besides the network.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FieldTrained {
    pub history: Vec<Epoch>,
    pub best_epoch: usize,
    pub n_train: usize,
    pub n_val: usize,
    pub stopped_early: bool,
}

/// A batch: values [b][planes][s][s], classes and distances [b][s][s].
struct Batch {
    b: usize,
    x: Vec<f32>,
    y: Vec<u8>,
    d: Vec<f32>,
}

fn batch_of(chips: &[Chip], picks: &[(usize, usize, usize, u8)], s: usize) -> Batch {
    let mut out = Batch { b: picks.len(), x: vec![], y: vec![], d: vec![] };
    for &(i, x0, y0, k) in picks {
        let (x, y, d) = crop(&chips[i], x0, y0, s, k);
        out.x.extend(x);
        out.y.extend(y);
        out.d.extend(d);
    }
    out
}

/// The labelled pixels' scores [m, 3], classes [m] and the selection, of scores [b, k, s, s].
fn labelled(scores: Tensor<4>, y: &[u8], device: &Device) -> Option<(Tensor<2>, Tensor<1, Int>, Tensor<1, Int>)> {
    let [b, k, s, _] = scores.dims();
    let flat = scores.permute([0, 2, 3, 1]).reshape([b * s * s, k]);
    let at: Vec<i64> = (0..b * s * s).filter(|&i| y[i] != NONE).map(|i| i as i64).collect();
    if at.is_empty() {
        return None;
    }
    let m = at.len();
    let ys: Vec<i64> = at.iter().map(|&i| y[i as usize] as i64).collect();
    let sel = Tensor::<1, Int>::from_data(TensorData::new(at, [m]), device);
    Some((flat.select(0, sel.clone()), Tensor::<1, Int>::from_data(TensorData::new(ys, [m]), device), sel))
}

/// The epochs' crops: each training chip `crops` times at random places and symmetries.
fn epoch_picks(chips: &[Chip], idx: &[usize], s: usize, crops: usize, rng: &mut Rng) -> Vec<(usize, usize, usize, u8)> {
    let mut v = vec![];
    for &i in idx {
        let side = chips[i].side;
        for _ in 0..crops.max(1) {
            let r = side.saturating_sub(s);
            let x0 = if r > 0 { (rng.next_u64() % (r as u64 + 1)) as usize } else { 0 };
            let y0 = if r > 0 { (rng.next_u64() % (r as u64 + 1)) as usize } else { 0 };
            v.push((i, x0, y0, (rng.next_u64() % 8) as u8));
        }
    }
    rng.shuffle(&mut v);
    v
}

/// The validation's crops: each chip's top left, unturned.
fn val_picks(idx: &[usize]) -> Vec<(usize, usize, usize, u8)> {
    idx.iter().map(|&i| (i, 0, 0, 0)).collect()
}

/// The chips split into training and validation (seeded).
fn split(n: usize, cfg: &FieldTrainConfig, rng: &mut Rng) -> (Vec<usize>, Vec<usize>) {
    let mut order: Vec<usize> = (0..n).collect();
    rng.shuffle(&mut order);
    let n_val = ((n as f64 * cfg.validation).round() as usize).clamp(1, n.saturating_sub(1).max(1));
    (order[n_val..].to_vec(), order[..n_val].to_vec())
}

/// Fine-tunes the FTW/Trazo U-Net on chips of its two windows (8 values, already divided by 3000), at the chips'
/// resolution (as FTW and Trazo train).
pub fn train_fieldnet(
    chips: &[Chip],
    start: &dyn Fn(&Device) -> Result<FieldNet, String>,
    cfg: &FieldTrainConfig,
    device: &Device,
    on_epoch: &mut dyn FnMut(&Epoch),
    cancel: &AtomicBool,
) -> Result<(FieldNet, FieldTrained), String> {
    if chips.len() < 2 {
        return Err("at least two labelled chips are needed".into());
    }
    let mut rng = Rng::new(cfg.seed);
    let (train_idx, val_idx) = split(chips.len(), cfg, &mut rng);
    let s = cfg.crop.min(chips.iter().map(|c| c.side).min().unwrap_or(0)) / 32 * 32;
    if s < 32 {
        return Err("the chips are smaller than 32 px".into());
    }
    let ad = device.clone().autodiff();
    device.seed(cfg.seed);
    // the norms frozen: batches of a few chips
    let mut net = start(&ad)?.freeze_norms();
    let loss_fn = CrossEntropyLossConfig::new().with_weights(Some(CLASS_WEIGHTS.to_vec())).init(&ad);
    let loss_v = CrossEntropyLossConfig::new().with_weights(Some(CLASS_WEIGHTS.to_vec())).init(device);
    let mut opt = AdamWConfig::new().with_weight_decay(1e-4).init();
    let mut out = FieldTrained { n_train: train_idx.len(), n_val: val_idx.len(), ..Default::default() };
    let (mut best, mut since) = (f64::INFINITY, 0);
    let mut best_net = net.clone();
    for epoch in 1..=cfg.epochs {
        let t0 = std::time::Instant::now();
        let picks = epoch_picks(chips, &train_idx, s, cfg.crops, &mut rng);
        let (mut loss_sum, mut right, mut seen) = (0.0, 0usize, 0usize);
        for chunk in picks.chunks(cfg.batch.max(1)) {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let bt = batch_of(chips, chunk, s);
            let x = Tensor::<4>::from_data(TensorData::new(bt.x.iter().map(|v| if v.is_finite() { *v } else { 0.0 }).collect::<Vec<_>>(), [bt.b, chips[0].t * chips[0].c, s, s]), &ad);
            let Some((logits, targets, _)) = labelled(net.forward(x), &bt.y, &ad) else { continue };
            let m = targets.dims()[0];
            let pred: Vec<i64> = logits.clone().argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            let truth: Vec<i64> = targets.clone().into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            right += pred.iter().zip(&truth).filter(|(a, b)| a == b).count();
            seen += m;
            let loss = loss_fn.forward(logits, targets);
            loss_sum += loss.clone().into_scalar::<f32>() as f64 * m as f64;
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = opt.step(cfg.lr, net, grads);
        }
        // validation
        let v = net.clone().valid();
        let (mut vl, mut vr, mut vn) = (0.0, 0usize, 0usize);
        for chunk in val_picks(&val_idx).chunks(cfg.batch.max(1)) {
            let bt = batch_of(chips, chunk, s);
            let x = Tensor::<4>::from_data(TensorData::new(bt.x.iter().map(|v| if v.is_finite() { *v } else { 0.0 }).collect::<Vec<_>>(), [bt.b, chips[0].t * chips[0].c, s, s]), device);
            let Some((logits, targets, _)) = labelled(v.forward(x), &bt.y, device) else { continue };
            let m = targets.dims()[0];
            let pred: Vec<i64> = logits.clone().argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            let truth: Vec<i64> = targets.clone().into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            vr += pred.iter().zip(&truth).filter(|(a, b)| a == b).count();
            vn += m;
            vl += loss_v.forward(logits, targets).into_scalar::<f32>() as f64 * m as f64;
        }
        let e = Epoch {
            epoch,
            train_loss: loss_sum / seen.max(1) as f64,
            train_acc: right as f64 / seen.max(1) as f64,
            val_loss: vl / vn.max(1) as f64,
            val_acc: vr as f64 / vn.max(1) as f64,
            lr: cfg.lr,
            seconds: t0.elapsed().as_secs_f64(),
        };
        on_epoch(&e);
        let val_loss = e.val_loss;
        out.history.push(e);
        if val_loss < best || epoch == 1 {
            (best, out.best_epoch, since) = (val_loss, epoch, 0);
            best_net = net.clone();
        } else {
            since += 1;
            if since >= cfg.patience {
                out.stopped_early = true;
                break;
            }
        }
    }
    Ok((best_net.valid(), out))
}

/// The U-TAE of fields: three classes and the distance (its fourth output, through a sigmoid).
pub fn field_utae(h: &UtaeHyper, bands: usize, device: &Device) -> Utae {
    Utae::new(h, bands, 4, device)
}

/// Trains a field U-TAE (`start`'s, on the training's device: a new one or a model's to fine-tune) on chips of
/// monthly composites ([months][4 values], standardized by `standard`; a month without any value is U-TAE's "no
/// observation"), `days` the months' positions (days from the first).
#[allow(clippy::too_many_arguments)]
pub fn train_field_utae(
    chips: &[Chip],
    start: &dyn Fn(&Device) -> Result<Utae, String>,
    days: &[f64],
    standard: &Standard,
    cfg: &FieldTrainConfig,
    device: &Device,
    on_epoch: &mut dyn FnMut(&Epoch),
    cancel: &AtomicBool,
) -> Result<(Utae, FieldTrained), String> {
    if chips.len() < 2 {
        return Err("at least two labelled chips are needed".into());
    }
    let (t, c) = (chips[0].t, chips[0].c);
    let mut rng = Rng::new(cfg.seed);
    let (train_idx, val_idx) = split(chips.len(), cfg, &mut rng);
    let s = cfg.crop.min(chips.iter().map(|c| c.side).min().unwrap_or(0)) / 8 * 8;
    device.seed(cfg.seed);
    let ad = device.clone().autodiff();
    let mut net = start(&ad)?;
    let pos = |d: &Device| {
        Tensor::<2>::from_data(TensorData::new(days.iter().map(|&v| v as f32).collect::<Vec<_>>(), [1, t]), d)
    };
    let (pos_ad, pos_v) = (pos(&ad), pos(device));
    let loss_fn = CrossEntropyLossConfig::new().with_weights(Some(CLASS_WEIGHTS.to_vec())).init(&ad);
    let loss_v = CrossEntropyLossConfig::new().with_weights(Some(CLASS_WEIGHTS.to_vec())).init(device);
    let mut opt = AdamWConfig::new().init();
    let mut out = FieldTrained { n_train: train_idx.len(), n_val: val_idx.len(), ..Default::default() };
    let (mut best, mut since) = (f64::INFINITY, 0);
    let mut best_net = net.clone();
    // the step's loss: the classes' cross entropy and the distance's squared error over the labelled pixels
    let step_loss = |scores: Tensor<4>, bt: &Batch, dev: &Device, ce: &burn::nn::loss::CrossEntropyLoss| {
        let [b, _, s, _] = scores.dims();
        let classes = scores.clone().slice([0..b, 0..3, 0..s, 0..s]);
        let dist = sigmoid(scores.slice([0..b, 3..4, 0..s, 0..s])).reshape([b * s * s]);
        let (logits, targets, sel) = labelled(classes, &bt.y, dev)?;
        let m = targets.dims()[0];
        let dt: Vec<f32> = (0..b * s * s).filter(|&i| bt.y[i] != NONE).map(|i| if bt.d[i].is_finite() { bt.d[i] } else { 0.0 }).collect();
        let dt = Tensor::<1>::from_data(TensorData::new(dt, [m]), dev);
        let dl = (dist.select(0, sel) - dt).powf_scalar(2.0).mean();
        let pred: Vec<i64> = logits.clone().argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
        let truth: Vec<i64> = targets.clone().into_data().convert::<i64>().try_to_vec().unwrap_or_default();
        let right = pred.iter().zip(&truth).filter(|(a, b)| a == b).count();
        Some((ce.forward(logits, targets) + dl * cfg.dist_weight, m, right))
    };
    for epoch in 1..=cfg.epochs {
        let t0 = std::time::Instant::now();
        let picks = epoch_picks(chips, &train_idx, s, cfg.crops, &mut rng);
        let (mut loss_sum, mut right, mut seen) = (0.0, 0usize, 0usize);
        for chunk in picks.chunks(cfg.batch.max(1)) {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let bt = batch_of(chips, chunk, s);
            let mut xs = bt.x.clone();
            drop_months(&mut xs, bt.b, t, c * s * s, &mut rng);
            standard.apply(&mut xs, c, s * s);
            let x = Tensor::<5>::from_data(TensorData::new(xs, [bt.b, t, c, s, s]), &ad);
            let scores = net.forward(x, pos_ad.clone().expand([bt.b, t]));
            let Some((loss, m, r)) = step_loss(scores, &bt, &ad, &loss_fn) else { continue };
            right += r;
            seen += m;
            loss_sum += loss.clone().into_scalar::<f32>() as f64 * m as f64;
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = opt.step(cfg.lr, net, grads);
        }
        let v = net.clone().valid();
        let (mut vl, mut vr, mut vn) = (0.0, 0usize, 0usize);
        for chunk in val_picks(&val_idx).chunks(cfg.batch.max(1)) {
            let bt = batch_of(chips, chunk, s);
            let mut xs = bt.x.clone();
            standard.apply(&mut xs, c, s * s);
            let x = Tensor::<5>::from_data(TensorData::new(xs, [bt.b, t, c, s, s]), device);
            let scores = v.forward(x, pos_v.clone().expand([bt.b, t]));
            let Some((loss, m, r)) = step_loss(scores, &bt, device, &loss_v) else { continue };
            vl += loss.into_scalar::<f32>() as f64 * m as f64;
            vr += r;
            vn += m;
        }
        let e = Epoch {
            epoch,
            train_loss: loss_sum / seen.max(1) as f64,
            train_acc: right as f64 / seen.max(1) as f64,
            val_loss: vl / vn.max(1) as f64,
            val_acc: vr as f64 / vn.max(1) as f64,
            lr: cfg.lr,
            seconds: t0.elapsed().as_secs_f64(),
        };
        on_epoch(&e);
        let val_loss = e.val_loss;
        out.history.push(e);
        if val_loss < best || epoch == 1 {
            (best, out.best_epoch, since) = (val_loss, epoch, 0);
            best_net = net.clone();
        } else {
            since += 1;
            if since >= cfg.patience {
                out.stopped_early = true;
                break;
            }
        }
    }
    Ok((best_net.valid(), out))
}

/// Each value's mean and deviation over the chips' observed values (as the networks take them: the digital
/// numbers over 3000), from every `step`-th chip.
pub fn standard_of(chips: &[Chip], step: usize) -> Standard {
    let c = chips.first().map_or(0, |ch| ch.c);
    let (mut sum, mut sq, mut cnt) = (vec![0f64; c], vec![0f64; c], vec![0u64; c]);
    for ch in chips.iter().step_by(step.max(1)) {
        let plane = ch.side * ch.side;
        for (p, v) in ch.x.chunks(plane).enumerate() {
            let k = p % c;
            for &x in v.iter().filter(|&&x| x > 0) {
                let x = x as f64 / crate::fields::NORMALIZATION as f64;
                sum[k] += x;
                sq[k] += x * x;
                cnt[k] += 1;
            }
        }
    }
    let mean: Vec<f32> = (0..c).map(|k| if cnt[k] > 0 { (sum[k] / cnt[k] as f64) as f32 } else { 0.0 }).collect();
    let std: Vec<f32> = (0..c)
        .map(|k| {
            if cnt[k] < 2 {
                return 1.0;
            }
            let m = sum[k] / cnt[k] as f64;
            ((sq[k] / cnt[k] as f64 - m * m).max(0.0).sqrt() as f32).max(1e-6)
        })
        .collect();
    Standard { mean, std }
}

/// Half the crops lose a run of up to a third of their months at the start or the end (no observation): series
/// shorter than the model's year still delineated.
fn drop_months(x: &mut [f32], b: usize, t: usize, per_month: usize, rng: &mut Rng) {
    for i in 0..b {
        if rng.next_u64() % 2 == 0 || t < 3 {
            continue;
        }
        let k = 1 + (rng.next_u64() % (t as u64 / 3)) as usize;
        let months: Vec<usize> = if rng.next_u64() % 2 == 0 { (0..k).collect() } else { (t - k..t).collect() };
        for m in months {
            x[(i * t + m) * per_month..(i * t + m + 1) * per_month].fill(f32::NAN);
        }
    }
}

/// What a chip's values are: the season's two windows (the FTW/Trazo networks) or months (a temporal model).
#[derive(Clone, Debug)]
pub enum ChipInputs {
    Season(crate::fields::Season),
    Months(Vec<Vec<usize>>),
}

/// The chips of a labelled window of a series (x0, y0, x1, y1): `labels` the fields drawn (a parcel's number per
/// pixel of the window, 0 none), `known` where every field is drawn (elsewhere unlabelled); squares of `side` (or
/// the window's smaller side, a multiple of 32) overlapping by a quarter, those with a tenth of their pixels known.
#[allow(clippy::too_many_arguments)]
pub fn chips_of_window(
    info: &std::sync::Arc<janus_cube::CubeInfo>,
    bands: &crate::fields::FieldBands,
    inputs: &ChipInputs,
    window: [usize; 4],
    labels: &[u32],
    known: &[bool],
    side: usize,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<Chip>, String> {
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let s = side.min(w).min(h) / 32 * 32;
    if s < 64 {
        return Err(format!("the labelled area is {w} x {h} px: at least 64 x 64 are needed"));
    }
    let (xs, ys) = (crate::fields::origins(w, s, s / 8), crate::fields::origins(h, s, s / 8));
    let squares: Vec<(usize, usize)> = ys.iter().flat_map(|&y| xs.iter().map(move |&x| (x, y))).collect();
    let mut chips = vec![];
    for (k, &(tx, ty)) in squares.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(k as f64 / squares.len() as f64, &format!("the training's chips: {}/{}", k + 1, squares.len()));
        let at = |u: usize, v: usize| (ty + v) * w + tx + u;
        let n_known = (0..s * s).filter(|&i| known[at(i % s, i / s)]).count();
        if n_known * 10 < s * s {
            continue;
        }
        let inst: Vec<u32> = (0..s * s).map(|i| labels[at(i % s, i / s)]).collect();
        let mut classes = classes_of(&inst, s, s);
        let mut dist = distance_to_edge(&inst, s, s);
        for i in 0..s * s {
            if !known[at(i % s, i / s)] {
                (classes[i], dist[i]) = (NONE, f32::NAN);
            }
        }
        let win = (x0 + tx, y0 + ty, s, s);
        let groups: Vec<&Vec<usize>> = match inputs {
            // window B first, as the FTW and Trazo datasets stack them
            ChipInputs::Season(se) => vec![&se.b, &se.a],
            ChipInputs::Months(m) => m.iter().collect(),
        };
        let mut x = vec![0u16; groups.len() * 4 * s * s];
        for (g, dates) in groups.iter().enumerate() {
            if dates.is_empty() {
                continue;
            }
            let v = crate::fields::composite(info, &bands.bands, dates, win);
            for (j, &a) in v.iter().enumerate() {
                x[g * 4 * s * s + j] = bands.to_dn(a);
            }
        }
        let (t, c) = match inputs {
            ChipInputs::Season(_) => (1, 8),
            ChipInputs::Months(m) => (m.len(), 4),
        };
        chips.push(Chip { t, c, side: s, x, classes, dist });
    }
    Ok(chips)
}

/// A field model's file: "JANUSFIELDS" and a version byte, the header's length (u64, little end), the header
/// (JSON) and the weights (safetensors).
const FIELD_MAGIC: &[u8] = b"JANUSFIELDS\x01";

/// What a field model is.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum FieldArch {
    /// the FTW/Trazo U-Net fine-tuned (`base` the checkpoint it started from): two windows' composites
    FieldNet { base: String },
    /// a U-TAE on `months` monthly composites from the month `first_month` (1-12) of the year before the labels'
    /// year; three classes and the distance to the field's edge
    Utae { hyper: UtaeHyper, months: usize, first_month: u32, standard: Standard },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FieldHeader {
    pub arch: FieldArch,
    /// what it was trained on, and how well it did
    pub about: String,
    pub training: serde_json::Value,
}

pub enum FieldNetwork {
    FieldNet(FieldNet),
    Utae(Utae),
}

pub struct FieldModel {
    pub header: FieldHeader,
    pub net: FieldNetwork,
}

impl FieldModel {
    pub fn save(&self, path: &std::path::Path) -> Result<(), String> {
        use burn::store::{ModuleSnapshot, SafetensorsStore};
        let mut store = SafetensorsStore::from_bytes(None);
        match &self.net {
            FieldNetwork::FieldNet(n) => n.clone().save_into(&mut store),
            FieldNetwork::Utae(n) => n.clone().save_into(&mut store),
        }
        .map_err(|e| format!("cannot keep the weights: {e}"))?;
        let weights = store.get_bytes().map_err(|e| format!("cannot keep the weights: {e}"))?;
        let header = serde_json::to_vec(&self.header).map_err(|e| e.to_string())?;
        let mut out = Vec::with_capacity(FIELD_MAGIC.len() + 8 + header.len() + weights.len());
        out.extend_from_slice(FIELD_MAGIC);
        out.extend_from_slice(&(header.len() as u64).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&weights);
        let tmp = path.with_extension("part");
        std::fs::write(&tmp, out).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn split<'a>(bytes: &'a [u8], path: &std::path::Path) -> Result<(FieldHeader, &'a [u8]), String> {
        let bad = || format!("{} is not a Janus field model", path.display());
        if bytes.len() < FIELD_MAGIC.len() + 8 || &bytes[..FIELD_MAGIC.len()] != FIELD_MAGIC {
            return Err(bad());
        }
        let len = u64::from_le_bytes(bytes[FIELD_MAGIC.len()..FIELD_MAGIC.len() + 8].try_into().map_err(|_| bad())?) as usize;
        let start = FIELD_MAGIC.len() + 8;
        let json = bytes.get(start..start + len).ok_or_else(bad)?;
        let header = serde_json::from_slice(json).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok((header, &bytes[start + len..]))
    }

    pub fn read_header(path: &std::path::Path) -> Result<FieldHeader, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self::split(&bytes, path)?.0)
    }

    /// A field model's network on a device (the training's, autodiff, to fine-tune it further).
    pub fn load(path: &std::path::Path, device: &Device) -> Result<FieldModel, String> {
        use burn::store::{ModuleSnapshot, SafetensorsStore};
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let (header, weights) = Self::split(&bytes, path)?;
        let mut store = SafetensorsStore::from_bytes(Some(weights.to_vec()));
        let bad = |e: String| format!("{}: its weights do not fit its network: {e}", path.display());
        let net = match &header.arch {
            FieldArch::FieldNet { .. } => {
                let mut n = FieldNet::new(crate::fieldnet::INPUTS, crate::fieldnet::CLASSES, device);
                n.load_from(&mut store).map_err(|e| bad(e.to_string()))?;
                FieldNetwork::FieldNet(n)
            }
            FieldArch::Utae { hyper, .. } => {
                let mut n = field_utae(hyper, 4, device);
                n.load_from(&mut store).map_err(|e| bad(e.to_string()))?;
                FieldNetwork::Utae(n)
            }
        };
        Ok(FieldModel { header, net })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_zero_at_edges_one_in_the_middle() {
        // a 5x5 field in a 7x7 image
        let (w, h) = (7, 7);
        let labels: Vec<u32> = (0..49).map(|i| if (1..6).contains(&(i % 7)) && (1..6).contains(&(i / 7)) { 1 } else { 0 }).collect();
        let d = distance_to_edge(&labels, w, h);
        assert_eq!(d[1 * 7 + 1], 0.0);
        assert_eq!(d[3 * 7 + 3], 1.0);
        assert_eq!(d[2 * 7 + 2], 0.5);
        assert_eq!(d[0], 0.0);
        let c = classes_of(&labels, w, h);
        assert_eq!((c[1 * 7 + 1], c[3 * 7 + 3], c[0]), (2, 1, 0));
    }

    #[test]
    fn crops_turn_consistently() {
        let side = 4;
        let ch = Chip {
            t: 1,
            c: 1,
            side,
            x: (1..17).map(|v| v as u16).collect(),
            classes: (0..16).map(|v| v as u8).collect(),
            dist: (0..16).map(|v| v as f32).collect(),
        };
        for k in 0..8 {
            let (x, y, d) = crop(&ch, 0, 0, 4, k);
            for i in 0..16 {
                let v = (x[i] * crate::fields::NORMALIZATION).round() as u8 - 1;
                assert_eq!(v, y[i]);
                assert_eq!(d[i] as u8, v);
            }
            let mut s = y.clone();
            s.sort();
            assert_eq!(s, (0..16).collect::<Vec<u8>>());
        }
    }
}
