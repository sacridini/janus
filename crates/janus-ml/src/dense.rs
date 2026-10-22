//! What U-TAE learns from and how it classifies an image: patches of the series (dates x bands x side x side)
//! with dense labels (a class per pixel, or none), each band standardized by its mean and deviation over the
//! training patches (a value without observation 0, so a date without any is all 0: U-TAE's "no observation");
//! the training (the loss over the labelled pixels only); and the image classified in overlapping windows, each
//! pixel's probabilities the mean of the windows that cover it.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use burn::module::Module;
use burn::nn::loss::CrossEntropyLossConfig;
use burn::optim::{AdamWConfig, GradientsParams};
use burn::prelude::*;
use burn::tensor::activation::softmax;
use janus_cube::CubeInfo;
use serde::{Deserialize, Serialize};

use crate::data::{InputBand, read_window};
use crate::model::{Arch, Model, Net};
use crate::train::{Epoch, Rng, TrainConfig, Trained, accuracies};
use crate::utae::{Utae, UtaeHyper};

/// No label.
pub const NONE: u8 = 255;

/// Each band's mean and standard deviation (U-TAE's normalization, as PASTIS's).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Standard {
    pub mean: Vec<f32>,
    pub std: Vec<f32>,
}

/// Patches: `x[i][t][c][y][x]` (NaN without observation, before standardizing) and `y[i][y][x]` (a class, or
/// NONE).
#[derive(Clone, Debug, Default)]
pub struct Patches {
    pub n: usize,
    pub t: usize,
    pub c: usize,
    pub side: usize,
    pub x: Vec<f32>,
    pub y: Vec<u8>,
}

impl Patches {
    fn len_x(&self) -> usize {
        self.t * self.c * self.side * self.side
    }

    pub fn select(&self, idx: &[usize]) -> Patches {
        let (lx, ly) = (self.len_x(), self.side * self.side);
        let mut out = Patches { n: idx.len(), t: self.t, c: self.c, side: self.side, x: vec![], y: vec![] };
        for &i in idx {
            out.x.extend_from_slice(&self.x[i * lx..(i + 1) * lx]);
            out.y.extend_from_slice(&self.y[i * ly..(i + 1) * ly]);
        }
        out
    }
}

/// [pixels][t][c] of a window (h rows of w) -> [t][c][h][w].
fn to_frames(series: &crate::data::Series, w: usize, h: usize) -> Vec<f32> {
    let (t, c) = (series.t, series.b);
    let mut out = vec![f32::NAN; t * c * h * w];
    for p in 0..w * h {
        let px = series.pixel(p);
        for d in 0..t {
            for k in 0..c {
                out[((d * c + k) * h) * w + p] = px[d * c + k];
            }
        }
    }
    out
}

/// The patches of `side` pixels over the labelled part of a series: `labels` (the series' w x h pixels, a class
/// or NONE); the patches tile the labels' bounding box overlapping by half (more patches from the same labels),
/// those with a labelled pixel kept (points and pins label one pixel each).
pub fn read_patches(
    info: &Arc<CubeInfo>,
    inputs: &[InputBand],
    labels: &[u8],
    side: usize,
    progress: &dyn Fn(f64),
    cancel: &AtomicBool,
) -> Result<Patches, String> {
    let (w, h) = (info.width, info.height);
    if w < side || h < side {
        return Err(format!("the series ({w} x {h} px) is smaller than a patch ({side} px)"));
    }
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (i, &l) in labels.iter().enumerate() {
        if l != NONE {
            let (x, y) = (i % w, i / w);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
        }
    }
    if x1 <= x0 {
        return Err("no labelled pixel".into());
    }
    // the origins: the box tiled, the last ones inside the image
    let starts = |a: usize, b: usize, n: usize| -> Vec<usize> {
        let mut v = vec![];
        let mut s = a;
        loop {
            v.push(s.min(n - side));
            if s + side >= b {
                break;
            }
            s += (side / 2).max(1);
        }
        v.dedup();
        v
    };
    let (xs, ys) = (starts(x0, x1, w), starts(y0, y1, h));
    let mut p = Patches { n: 0, t: info.t(), c: inputs.len(), side, x: vec![], y: vec![] };
    for (k, &py) in ys.iter().enumerate() {
        for &px in &xs {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let lab: Vec<u8> = (0..side * side).map(|i| labels[(py + i / side) * w + px + i % side]).collect();
            if lab.iter().all(|&l| l == NONE) {
                continue;
            }
            let s = read_window(info, inputs, (px, py, side, side));
            p.x.extend(to_frames(&s, side, side));
            p.y.extend(lab);
            p.n += 1;
        }
        progress((k + 1) as f64 / ys.len() as f64);
    }
    if p.n == 0 {
        return Err("no patch has a labelled pixel".into());
    }
    Ok(p)
}

impl Standard {
    /// Over the patches' observed values, band by band.
    pub fn fit(p: &Patches) -> Standard {
        let plane = p.side * p.side;
        let (mut sum, mut sq, mut cnt) = (vec![0f64; p.c], vec![0f64; p.c], vec![0u64; p.c]);
        for i in 0..p.n {
            for d in 0..p.t {
                for k in 0..p.c {
                    let o = ((i * p.t + d) * p.c + k) * plane;
                    for &v in &p.x[o..o + plane] {
                        if v.is_finite() {
                            sum[k] += v as f64;
                            sq[k] += (v as f64) * (v as f64);
                            cnt[k] += 1;
                        }
                    }
                }
            }
        }
        let mean: Vec<f32> = (0..p.c).map(|k| if cnt[k] > 0 { (sum[k] / cnt[k] as f64) as f32 } else { 0.0 }).collect();
        let std = (0..p.c)
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

    /// [..][c][plane] values standardized in place; no observation 0.
    pub fn apply(&self, x: &mut [f32], c: usize, plane: usize) {
        for (j, v) in x.iter_mut().enumerate() {
            let k = (j / plane) % c;
            *v = if v.is_finite() { (*v - self.mean[k]) / self.std[k] } else { 0.0 };
        }
    }
}

/// U-TAE's training settings (the authors': Adam 1e-3, batches of 4 patches).
pub fn utae_config() -> TrainConfig {
    TrainConfig {
        epochs: 80,
        batch_size: 4,
        lr: 1e-3,
        weight_decay: 0.0,
        lr_decay_rate: 1.0,
        patience: 10,
        min_delta: 0.0,
        ..TrainConfig::tempcnn()
    }
}

/// The network a U-TAE training starts from: its own, or the authors' PASTIS weights with a new last layer.
pub fn utae_net(
    h: &UtaeHyper,
    bands: usize,
    classes: usize,
    pastis: Option<&PathBuf>,
    device: &Device,
) -> Result<Utae, String> {
    match pastis {
        Some(w) => {
            Ok(Utae::new(h, bands, crate::utae::PASTIS_CLASSES, device).load_authors(w)?.with_classes(classes, device))
        }
        None => Ok(Utae::new(h, bands, classes, device)),
    }
}

/// The loss, accuracy and confusion of a network on patches (validation mode), over their labelled pixels.
fn evaluate(
    net: &Utae,
    p: &Patches,
    idx: &[usize],
    positions: &Tensor<2>,
    classes: usize,
    device: &Device,
) -> (f64, f64, Vec<Vec<u32>>) {
    let mut confusion = vec![vec![0u32; classes]; classes];
    let loss_fn = CrossEntropyLossConfig::new().init(device);
    let (mut loss_sum, mut n) = (0.0, 0usize);
    for chunk in idx.chunks(4) {
        let sub = p.select(chunk);
        let b = chunk.len();
        let (logits, targets, at) = batch_scores(net, &sub, positions, classes, device);
        if at.is_empty() {
            continue;
        }
        let loss: f32 = loss_fn.forward(logits.clone(), targets.clone()).into_scalar();
        loss_sum += loss as f64 * at.len() as f64;
        n += at.len();
        let pred: Vec<i64> = logits.argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
        let truth: Vec<i64> = targets.into_data().convert::<i64>().try_to_vec().unwrap_or_default();
        for (pp, tt) in pred.iter().zip(&truth) {
            confusion[*tt as usize][*pp as usize] += 1;
        }
        let _ = b;
    }
    let right: u32 = (0..classes).map(|i| confusion[i][i]).sum();
    (
        if n > 0 { loss_sum / n as f64 } else { f64::NAN },
        if n > 0 { right as f64 / n as f64 } else { f64::NAN },
        confusion,
    )
}

/// A batch's scores at its labelled pixels [m, classes], their classes [m], and where they are.
fn batch_scores(
    net: &Utae,
    p: &Patches,
    positions: &Tensor<2>,
    classes: usize,
    device: &Device,
) -> (Tensor<2>, Tensor<1, Int>, Vec<i64>) {
    let (b, t, c, s) = (p.n, p.t, p.c, p.side);
    let x = Tensor::<5>::from_data(TensorData::new(p.x.clone(), [b, t, c, s, s]), device);
    let pos = positions.clone().slice([0..1, 0..t]).expand([b, t]);
    let scores = net.forward(x, pos); // [b, k, s, s]
    let flat = scores.permute([0, 2, 3, 1]).reshape([b * s * s, classes]);
    let at: Vec<i64> = (0..b * s * s).filter(|&i| p.y[i] != NONE).map(|i| i as i64).collect();
    let ys: Vec<i64> = at.iter().map(|&i| p.y[i as usize] as i64).collect();
    if at.is_empty() {
        return (flat, Tensor::<1, Int>::zeros([0], device), at);
    }
    let sel = Tensor::<1, Int>::from_data(TensorData::new(at.clone(), [at.len()]), device);
    let m = at.len();
    (flat.select(0, sel), Tensor::<1, Int>::from_data(TensorData::new(ys, [m]), device), at)
}

/// Trains a U-TAE on standardized patches (their classes 0..classes, NONE without label), the patches split
/// into training and validation (`cfg.validation_split`, seeded); early stopping on the validation loss, the
/// network of the best kept.
#[allow(clippy::too_many_arguments)]
pub fn train_utae(
    p: &Patches,
    classes: usize,
    h: &UtaeHyper,
    days: &[f64],
    cfg: &TrainConfig,
    pastis: Option<&PathBuf>,
    device: &Device,
    on_epoch: &mut dyn FnMut(&Epoch),
    cancel: &AtomicBool,
) -> Result<Trained, String> {
    if p.n < 2 {
        return Err("at least two patches are needed".into());
    }
    let mut rng = Rng::new(cfg.seed);
    let mut order: Vec<usize> = (0..p.n).collect();
    rng.shuffle(&mut order);
    let n_val = ((p.n as f64 * cfg.validation_split).round() as usize).clamp(1, p.n - 1);
    let (val_idx, train_idx) = (order[..n_val].to_vec(), order[n_val..].to_vec());
    device.seed(cfg.seed);
    let ad = device.clone().autodiff();
    let mut net = utae_net(h, p.c, classes, pastis, &ad)?;
    let positions = Tensor::<2>::from_data(
        TensorData::new(days.iter().map(|&d| d as f32).collect::<Vec<_>>(), [1, days.len()]),
        &ad,
    );
    let positions_v = Tensor::<2>::from_data(
        TensorData::new(days.iter().map(|&d| d as f32).collect::<Vec<_>>(), [1, days.len()]),
        device,
    );
    let loss_fn = CrossEntropyLossConfig::new().init(&ad);
    let mut opt = AdamWConfig::new().with_epsilon(cfg.eps as f32).with_weight_decay(cfg.weight_decay as f32).init();
    let (mut history, mut best, mut best_epoch, mut since) = (vec![], f64::INFINITY, 0, 0);
    let mut best_net = net.clone();
    let mut stopped_early = false;
    let batch = cfg.batch_size.max(1);
    for epoch in 1..=cfg.epochs {
        let t0 = std::time::Instant::now();
        let lr = cfg.lr * cfg.lr_decay_rate.powi(((epoch - 1) / cfg.lr_decay_epochs.max(1)) as i32);
        let mut ord = train_idx.clone();
        rng.shuffle(&mut ord);
        let (mut loss_sum, mut right, mut seen) = (0.0, 0usize, 0usize);
        for chunk in ord.chunks(batch) {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let sub = p.select(chunk);
            let (logits, targets, at) = batch_scores(&net, &sub, &positions, classes, &ad);
            if at.is_empty() {
                continue;
            }
            let pred: Vec<i64> = logits.clone().argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            let truth: Vec<i64> = targets.clone().into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            right += pred.iter().zip(&truth).filter(|(a, b)| a == b).count();
            seen += at.len();
            let loss = loss_fn.forward(logits, targets);
            loss_sum += loss.clone().into_scalar::<f32>() as f64 * at.len() as f64;
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = opt.step(lr, net, grads);
        }
        let (val_loss, val_acc, _) = evaluate(&net.valid(), p, &val_idx, &positions_v, classes, device);
        let e = Epoch {
            epoch,
            train_loss: loss_sum / seen.max(1) as f64,
            train_acc: right as f64 / seen.max(1) as f64,
            val_loss,
            val_acc,
            lr,
            seconds: t0.elapsed().as_secs_f64(),
        };
        on_epoch(&e);
        history.push(e);
        if val_loss < best - cfg.min_delta || epoch == 1 {
            (best, best_epoch, since) = (val_loss, epoch, 0);
            best_net = net.clone();
        } else {
            since += 1;
            if since >= cfg.patience {
                stopped_early = true;
                break;
            }
        }
    }
    let net = best_net.valid();
    let (_, _, confusion) = evaluate(&net, p, &val_idx, &positions_v, classes, device);
    let _ = accuracies(&confusion);
    Ok(Trained {
        net: Net::Utae(net),
        history,
        best_epoch,
        confusion,
        n_train: train_idx.len(),
        n_val: val_idx.len(),
        stopped_early,
        batch_size: batch,
    })
}

/// Takes the rows a classification has done: (first row, rows, probabilities [rows][w][classes]).
pub type RowsDone<'a> = dyn FnMut(usize, usize, &[f32]) -> Result<(), String> + 'a;

/// Classifies a window of a series with a U-TAE model: windows of its patch's side, overlapping by its overlap,
/// each pixel the mean of the windows' probabilities. `out(row, rows, probabilities [rows][w][classes])` takes
/// the rows as they are done, top down.
#[allow(clippy::too_many_arguments)]
pub fn classify_windows(
    model: &Model,
    info: &Arc<CubeInfo>,
    read: &[InputBand],
    window: [usize; 4],
    standard: &Standard,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
    out: &mut RowsDone,
) -> Result<(), String> {
    let Arch::Utae(h) = &model.header.arch else { return Err("not a U-TAE model".into()) };
    let Net::Utae(net) = &model.net else { return Err("not a U-TAE model".into()) };
    let [x0, y0, x1, y1] = window;
    let (w, hh) = (x1 - x0, y1 - y0);
    let side = h.patch;
    let k = model.header.classes.len();
    let stride = side.saturating_sub(h.overlap).max(1);
    let starts = |n: usize| -> Vec<usize> {
        if n <= side {
            return vec![0];
        }
        let mut v: Vec<usize> = (0..=n - side).step_by(stride).collect();
        if *v.last().unwrap() != n - side {
            v.push(n - side);
        }
        v
    };
    let (xs, ys) = (starts(w), starts(hh));
    // the dates' positions are this series' own (U-TAE takes any dates)
    let days = crate::data::Calendar::of(info).day_offsets();
    let t = days.len();
    let c = read.len();
    let device = &model.device;
    let positions =
        Tensor::<2>::from_data(TensorData::new(days.iter().map(|&d| d as f32).collect::<Vec<_>>(), [1, t]), device);
    // rows of probabilities being summed: from `base` on, each [w][k], with the windows' count
    let mut acc: VecDeque<(Vec<f32>, Vec<f32>)> = VecDeque::new();
    let mut base = 0usize;
    let t0 = std::time::Instant::now();
    for (j, &ry) in ys.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let rows = side.min(hh);
        let strip = read_window(info, read, (x0, y0 + ry, w, rows));
        let mut frames = to_frames(&strip, w, rows); // [t][c][rows][w]
        standard.apply(&mut frames, c, rows * w);
        // the strip's windows, in batches
        let mut probs_all = vec![];
        for chunk in xs.chunks(4) {
            let b = chunk.len();
            let mut x = vec![0f32; b * t * c * side * side];
            for (bi, &cx) in chunk.iter().enumerate() {
                for tc in 0..t * c {
                    for yy in 0..rows {
                        for xx in 0..side.min(w) {
                            x[((bi * t * c + tc) * side + yy) * side + xx] = frames[(tc * rows + yy) * w + cx + xx];
                        }
                    }
                }
            }
            let input = Tensor::<5>::from_data(TensorData::new(x, [b, t, c, side, side]), device);
            let p = softmax(net.forward(input, positions.clone().expand([b, t])), 1);
            probs_all.push(p.into_data().try_to_vec::<f32>().unwrap_or_default());
        }
        while acc.len() < ry + rows - base {
            acc.push_back((vec![0f32; w * k], vec![0f32; w]));
        }
        for (ci, chunk) in xs.chunks(4).enumerate() {
            let p = &probs_all[ci];
            for (bi, &cx) in chunk.iter().enumerate() {
                for yy in 0..rows {
                    let (sum, cnt) = &mut acc[ry + yy - base];
                    for xx in 0..side.min(w) {
                        for kk in 0..k {
                            sum[(cx + xx) * k + kk] += p[((bi * k + kk) * side + yy) * side + xx];
                        }
                        cnt[cx + xx] += 1.0;
                    }
                }
            }
        }
        // the rows no later window covers
        let done_to = ys.get(j + 1).copied().unwrap_or(hh);
        let n_done = done_to.min(hh) - base;
        if n_done > 0 {
            let mut buf = Vec::with_capacity(n_done * w * k);
            for _ in 0..n_done {
                let (sum, cnt) = acc.pop_front().unwrap();
                for x in 0..w {
                    for kk in 0..k {
                        buf.push(if cnt[x] > 0.0 { sum[x * k + kk] / cnt[x] } else { f32::NAN });
                    }
                }
            }
            out(base, n_done, &buf)?;
            base += n_done;
        }
        let el = t0.elapsed().as_secs_f64();
        progress(
            base as f64 / hh as f64,
            &format!(
                "rows {base}/{hh}, {el:.0} s elapsed, ~{:.0} s left",
                el / base.max(1) as f64 * (hh - base) as f64
            ),
        );
    }
    Ok(())
}
