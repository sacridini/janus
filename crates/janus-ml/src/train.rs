//! Training as sits trains its torch models (luz): a validation share split off the samples (stratified by class,
//! with a seed), AdamW, the learning rate decayed by steps, the cross-entropy (with the classes' weights if asked),
//! early stopping on the validation loss (patience, min_delta); the model of the best validation loss is kept.

use std::sync::atomic::{AtomicBool, Ordering};

use burn::nn::loss::CrossEntropyLossConfig;
use burn::optim::{AdamWConfig, GradientsParams};
use burn::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::data::{Calendar, Series};
use crate::model::{Arch, Net};

/// The training's settings (sits' names and defaults: `TrainConfig::tempcnn()`, `TrainConfig::lighttae()`).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TrainConfig {
    pub epochs: usize,
    pub batch_size: usize,
    pub validation_split: f64,
    pub lr: f64,
    pub eps: f64,
    pub weight_decay: f64,
    pub lr_decay_epochs: usize,
    pub lr_decay_rate: f64,
    pub patience: usize,
    pub min_delta: f64,
    pub seed: u64,
    /// each class's loss weighed by the inverse of its share of the samples
    pub class_weights: bool,
}

impl TrainConfig {
    pub fn tempcnn() -> TrainConfig {
        TrainConfig {
            epochs: 150,
            batch_size: 64,
            validation_split: 0.2,
            lr: 5e-4,
            eps: 1e-8,
            weight_decay: 1e-6,
            lr_decay_epochs: 1,
            lr_decay_rate: 0.95,
            patience: 20,
            min_delta: 0.01,
            seed: 42,
            class_weights: false,
        }
    }

    pub fn lighttae() -> TrainConfig {
        TrainConfig {
            epochs: 150,
            batch_size: 128,
            weight_decay: 7e-4,
            lr_decay_epochs: 50,
            lr_decay_rate: 1.0,
            ..TrainConfig::tempcnn()
        }
    }

    /// A head on Presto's encodings (zeit.ai.train's settings: 1e-3, 50 epochs, patience 10).
    pub fn presto() -> TrainConfig {
        TrainConfig {
            epochs: 50,
            batch_size: 64,
            lr: 1e-3,
            weight_decay: 0.0,
            lr_decay_rate: 1.0,
            patience: 10,
            min_delta: 0.0,
            ..TrainConfig::tempcnn()
        }
    }

    pub fn for_arch(arch: &Arch) -> TrainConfig {
        match arch {
            Arch::TempCnn(_) => TrainConfig::tempcnn(),
            Arch::LTae(_) => TrainConfig::lighttae(),
            Arch::Presto(_) => TrainConfig::presto(),
            Arch::Utae(_) => crate::dense::utae_config(),
            // the validation share and the seed only
            Arch::RandomForest(_) => TrainConfig { epochs: 0, ..TrainConfig::tempcnn() },
        }
    }
}

/// One epoch's numbers.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Epoch {
    pub epoch: usize,
    pub train_loss: f64,
    pub train_acc: f64,
    pub val_loss: f64,
    pub val_acc: f64,
    pub lr: f64,
    pub seconds: f64,
}

/// The training's result: the network (of the best validation loss), each epoch's numbers, and the validation
/// samples' confusion (rows: the samples' classes; columns: the model's).
pub struct Trained {
    pub net: Net,
    pub history: Vec<Epoch>,
    pub best_epoch: usize,
    pub confusion: Vec<Vec<u32>>,
    pub n_train: usize,
    pub n_val: usize,
    pub stopped_early: bool,
    /// the batch size used (smaller than asked for few samples: see `train`)
    pub batch_size: usize,
}

impl Trained {
    /// Overall accuracy, and each class's user's and producer's accuracy, of the validation samples.
    pub fn accuracies(&self) -> (f64, Vec<f64>, Vec<f64>) {
        accuracies(&self.confusion)
    }

    /// What the model file keeps of the training.
    pub fn summary(&self, cfg: &TrainConfig, per_class: &[usize]) -> serde_json::Value {
        let (oa, ua, pa) = self.accuracies();
        json!({
            "config": cfg,
            "samples": { "per_class": per_class, "train": self.n_train, "validation": self.n_val },
            "batch_size": self.batch_size,
            "history": self.history,
            "best_epoch": self.best_epoch,
            "stopped_early": self.stopped_early,
            "validation": { "confusion": self.confusion, "overall_accuracy": oa, "users_accuracy": ua, "producers_accuracy": pa },
        })
    }
}

/// Overall accuracy and the user's (by the model's class) and producer's (by the reference) accuracies.
pub fn accuracies(c: &[Vec<u32>]) -> (f64, Vec<f64>, Vec<f64>) {
    let k = c.len();
    let total: u32 = c.iter().flatten().sum();
    let diag: u32 = (0..k).map(|i| c[i][i]).sum();
    let ratio = |a: u32, b: u32| if b > 0 { a as f64 / b as f64 } else { f64::NAN };
    let users = (0..k).map(|j| ratio(c[j][j], (0..k).map(|i| c[i][j]).sum())).collect();
    let producers = (0..k).map(|i| ratio(c[i][i], c[i].iter().sum())).collect();
    (ratio(diag, total), users, producers)
}

/// SplitMix64: the shuffles and the split.
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

    pub fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let j = (self.next_u64() % (i as u64 + 1)) as usize;
            v.swap(i, j);
        }
    }
}

/// The samples split: (training, validation) indices, `share` of each class to the validation (at least one when
/// the class has two samples or more).
pub fn split(labels: &[u32], classes: usize, share: f64, seed: u64) -> (Vec<usize>, Vec<usize>) {
    let mut rng = Rng::new(seed);
    let (mut train, mut val) = (vec![], vec![]);
    for c in 0..classes as u32 {
        let mut idx: Vec<usize> = (0..labels.len()).filter(|&i| labels[i] == c).collect();
        rng.shuffle(&mut idx);
        let mut n_val = (idx.len() as f64 * share).round() as usize;
        if n_val == 0 && idx.len() >= 2 && share > 0.0 {
            n_val = 1;
        }
        val.extend_from_slice(&idx[..n_val]);
        train.extend_from_slice(&idx[n_val..]);
    }
    train.sort_unstable();
    val.sort_unstable();
    (train, val)
}

/// The loss and the accuracy of a network on samples (in batches, validation mode), and their confusion.
#[allow(clippy::too_many_arguments)]
fn evaluate(
    net: &Net,
    x: &Tensor<3>,
    y: &[u32],
    idx: &[usize],
    batch: usize,
    classes: usize,
    weights: Option<Vec<f32>>,
    device: &Device,
) -> (f64, f64, Vec<Vec<u32>>) {
    let mut confusion = vec![vec![0u32; classes]; classes];
    if idx.is_empty() {
        return (f64::NAN, f64::NAN, confusion);
    }
    let loss_fn = CrossEntropyLossConfig::new().with_weights(weights).init(device);
    let (mut loss_sum, mut right) = (0.0, 0);
    for chunk in idx.chunks(batch.max(1)) {
        let sel = Tensor::<1, Int>::from_data(
            TensorData::new(chunk.iter().map(|&i| i as i64).collect::<Vec<_>>(), [chunk.len()]),
            device,
        );
        let xb = x.clone().select(0, sel);
        let yt: Vec<i64> = chunk.iter().map(|&i| y[i] as i64).collect();
        let yb = Tensor::<1, Int>::from_data(TensorData::new(yt.clone(), [chunk.len()]), device);
        let scores = net.forward(xb);
        let loss: f32 = loss_fn.forward(scores.clone(), yb).into_scalar();
        loss_sum += loss as f64 * chunk.len() as f64;
        let pred: Vec<i64> = scores.argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
        for (p, t) in pred.iter().zip(&yt) {
            confusion[*t as usize][*p as usize] += 1;
            right += (p == t) as usize;
        }
    }
    (loss_sum / idx.len() as f64, right as f64 / idx.len() as f64, confusion)
}

/// Trains a network of `arch` on scaled series `x` (gaps filled) and their classes `y` (0..classes).
/// `on_epoch` follows the training; `cancel` stops it between batches (an error "cancelled").
#[allow(clippy::too_many_arguments)]
pub fn train(
    x: &Series,
    y: &[u32],
    classes: usize,
    arch: &Arch,
    calendar: &Calendar,
    cfg: &TrainConfig,
    device: &Device,
    on_epoch: &mut dyn FnMut(&Epoch),
    cancel: &AtomicBool,
) -> Result<Trained, String> {
    if x.n != y.len() || x.n == 0 {
        return Err("no samples to train on".into());
    }
    if classes < 2 {
        return Err("at least two classes are needed".into());
    }
    if matches!(arch, Arch::Utae(_)) {
        return Err("U-TAE trains on patches: dense::train_utae".into());
    }
    let (train_idx, val_idx) = split(y, classes, cfg.validation_split, cfg.seed);
    if train_idx.len() < 2 {
        return Err("too few samples to train on".into());
    }
    // at least 8 batches an epoch: with fewer, the batch normalizations' running statistics (what the model uses
    // once trained) are made of too few updates and the model classifies everything alike
    let batch_size = cfg.batch_size.min(train_idx.len() / 8).max(8);
    device.seed(cfg.seed);
    let ad = device.clone().autodiff();
    let mut net = match Net::new(arch, x.b, calendar, classes, &ad)? {
        // Presto's encoder kept as pretrained: only the head learns
        Net::Presto(p) => Net::Presto(p.freeze_encoder()),
        n => n,
    };
    let xs = Tensor::<3>::from_data(TensorData::new(x.values.clone(), [x.n, x.t, x.b]), &ad);
    let weights = cfg.class_weights.then(|| {
        let mut count = vec![0usize; classes];
        for &i in &train_idx {
            count[y[i] as usize] += 1;
        }
        let total = train_idx.len() as f32;
        count.iter().map(|&c| if c > 0 { total / (classes as f32 * c as f32) } else { 0.0 }).collect::<Vec<f32>>()
    });
    let loss_fn = CrossEntropyLossConfig::new().with_weights(weights.clone()).init(&ad);
    let mut opt = AdamWConfig::new().with_epsilon(cfg.eps as f32).with_weight_decay(cfg.weight_decay as f32).init();
    let mut rng = Rng::new(cfg.seed ^ 0x5EED);
    let (mut history, mut best, mut best_epoch, mut since_best) = (vec![], f64::INFINITY, 0, 0);
    let mut best_net = net.clone();
    let mut stopped_early = false;
    for epoch in 1..=cfg.epochs {
        let t0 = std::time::Instant::now();
        let lr = cfg.lr * cfg.lr_decay_rate.powi(((epoch - 1) / cfg.lr_decay_epochs.max(1)) as i32);
        let mut order = train_idx.clone();
        rng.shuffle(&mut order);
        let mut batches: Vec<&[usize]> = order.chunks(batch_size).collect();
        // a last batch of one sample has no batch statistics: joined to the one before
        if batches.len() > 1 && batches.last().is_some_and(|b| b.len() == 1) {
            let k = batches.len();
            let joined = &order[(k - 2) * batch_size..];
            batches.truncate(k - 2);
            batches.push(joined);
        }
        let (mut loss_sum, mut right) = (0.0, 0usize);
        for b in batches {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let sel = Tensor::<1, Int>::from_data(
                TensorData::new(b.iter().map(|&i| i as i64).collect::<Vec<_>>(), [b.len()]),
                &ad,
            );
            let xb = xs.clone().select(0, sel);
            let yt: Vec<i64> = b.iter().map(|&i| y[i] as i64).collect();
            let yb = Tensor::<1, Int>::from_data(TensorData::new(yt.clone(), [b.len()]), &ad);
            let scores = net.forward(xb);
            let pred: Vec<i64> = scores.clone().argmax(1).into_data().convert::<i64>().try_to_vec().unwrap_or_default();
            right += pred.iter().zip(&yt).filter(|(p, t)| p == t).count();
            let loss = loss_fn.forward(scores, yb);
            loss_sum += loss.clone().into_scalar::<f32>() as f64 * b.len() as f64;
            let grads = GradientsParams::from_grads(loss.backward(), &net);
            net = opt.step(lr, net, grads);
        }
        let valid = net.valid();
        let xv = xs.clone().inner();
        let (val_loss, val_acc, _) =
            evaluate(&valid, &xv, y, &val_idx, cfg.batch_size.max(256), classes, weights.clone(), device);
        let e = Epoch {
            epoch,
            train_loss: loss_sum / train_idx.len() as f64,
            train_acc: right as f64 / train_idx.len() as f64,
            val_loss,
            val_acc,
            lr,
            seconds: t0.elapsed().as_secs_f64(),
        };
        on_epoch(&e);
        history.push(e);
        // early stopping on the validation loss (the training loss without validation samples)
        let monitored = if val_idx.is_empty() { history.last().map_or(f64::NAN, |e| e.train_loss) } else { val_loss };
        if monitored < best - cfg.min_delta || epoch == 1 {
            best = monitored;
            best_epoch = epoch;
            best_net = net.clone();
            since_best = 0;
        } else {
            since_best += 1;
            if monitored < best {
                // better, by less than min_delta: kept, but it does not reset the patience
                best = monitored;
                best_epoch = epoch;
                best_net = net.clone();
            }
            if since_best >= cfg.patience {
                stopped_early = true;
                break;
            }
        }
    }
    let net = best_net.valid();
    let xv = xs.inner();
    let (_, _, confusion) = evaluate(&net, &xv, y, &val_idx, cfg.batch_size.max(256), classes, None, device);
    Ok(Trained {
        net,
        history,
        best_epoch,
        confusion,
        n_train: train_idx.len(),
        n_val: val_idx.len(),
        stopped_early,
        batch_size,
    })
}
