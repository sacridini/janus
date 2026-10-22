//! U-TAE (Garnot & Landrieu 2021, VSainteuf/utae-paps, MIT): a U-Net whose encoder reads every date of a patch
//! (its weights shared in time), a lightweight temporal attention (L-TAE) over each pixel of the bottleneck, and
//! a decoder whose skip connections are summed over the dates with the same attention (per group of channels):
//! it segments a whole patch, so a pixel's class comes from its neighbourhood too (the edges of the fields).
//! Layer for layer as zeit.ai.utae ports it (with the same names, the authors' weights load as they are: their
//! PASTIS weights, Sentinel-2 crops in France, as a starting point).
//!
//! A patch is [batch, dates, bands, height, width]; a date whose values are all 0 (the pad value) is no
//! observation: the encoder leaves it 0 and the attention skips it.

use std::path::Path;

use burn::module::Param;
use burn::nn::conv::{Conv1d, Conv1dConfig, Conv2d, Conv2dConfig, ConvTranspose2d, ConvTranspose2dConfig};
use burn::nn::{
    BatchNorm, BatchNormConfig, Dropout, DropoutConfig, GroupNorm, GroupNormConfig, Linear, LinearConfig,
    PaddingConfig2d,
};
use burn::prelude::*;
use burn::store::{ModuleSnapshot, PytorchStore};
use burn::tensor::Distribution;
use burn::tensor::activation::{relu, softmax};
use burn::tensor::ops::PadMode;
use serde::{Deserialize, Serialize};

/// U-TAE's architecture (the authors' PASTIS configuration by default).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UtaeHyper {
    pub encoder_widths: Vec<usize>,
    pub decoder_widths: Vec<usize>,
    /// the last convolutions' widths (the last one the classes')
    pub out_conv: Vec<usize>,
    pub n_head: usize,
    pub d_model: usize,
    pub d_k: usize,
    /// the patches' side (a multiple of 2^(stages - 1)), and how much windows overlap when classifying an image
    pub patch: usize,
    pub overlap: usize,
    /// started from the authors' PASTIS weights (Sentinel-2, 10 bands)
    #[serde(default)]
    pub pastis: bool,
}

impl Default for UtaeHyper {
    fn default() -> Self {
        UtaeHyper {
            encoder_widths: vec![64, 64, 64, 128],
            decoder_widths: vec![32, 32, 64, 128],
            out_conv: vec![32, 20],
            n_head: 16,
            d_model: 256,
            d_k: 4,
            patch: 64,
            overlap: 16,
            pastis: false,
        }
    }
}

/// Linear resampling of one dimension (`dim`) of n values to m, as PyTorch's bilinear with align_corners=False:
/// output o reads the input at (o + 0.5) n / m - 0.5 (at least 0), between its two neighbours. Selections and
/// weights, so it has a gradient on every backend.
fn resample(x: Tensor<4>, dim: usize, m: usize) -> Tensor<4> {
    let n = x.dims()[dim];
    let device = x.device();
    let (mut i0, mut i1, mut f) = (vec![], vec![], vec![]);
    for o in 0..m {
        let src = ((o as f64 + 0.5) * n as f64 / m as f64 - 0.5).max(0.0);
        let a = (src.floor() as usize).min(n - 1);
        i0.push(a as i64);
        i1.push((a + 1).min(n - 1) as i64);
        f.push((src - a as f64) as f32);
    }
    let idx = |v: Vec<i64>| Tensor::<1, Int>::from_data(TensorData::new(v, [m]), &device);
    let mut shape = [1usize; 4];
    shape[dim] = m;
    let w1 = Tensor::<1>::from_data(TensorData::new(f, [m]), &device).reshape(shape);
    let w0 = w1.clone().neg() + 1.0;
    x.clone().select(dim, idx(i0)) * w0 + x.select(dim, idx(i1)) * w1
}

/// Bilinear upsampling of [n, c, h, w] to [n, c, hh, ww] (PyTorch's, align_corners=False).
fn upsample(x: Tensor<4>, hh: usize, ww: usize) -> Tensor<4> {
    resample(resample(x, 2, hh), 3, ww)
}

/// Convolutions (3x3 unless said, reflect padding), each with its normalization (group or batch) and a ReLU.
#[derive(Module, Debug)]
pub struct ConvLayer {
    convs: Vec<Conv2d>,
    gns: Vec<GroupNorm>,
    bns: Vec<BatchNorm>,
    pad: usize,
}

impl ConvLayer {
    fn new(widths: &[usize], group: bool, k: usize, s: usize, p: usize, device: &Device) -> ConvLayer {
        let (mut convs, mut gns, mut bns) = (vec![], vec![], vec![]);
        for w in widths.windows(2) {
            convs.push(
                Conv2dConfig::new([w[0], w[1]], [k, k])
                    .with_stride([s, s])
                    .with_padding(PaddingConfig2d::Valid)
                    .init(device),
            );
            if group {
                gns.push(GroupNormConfig::new(4, w[1]).init(device));
            } else {
                bns.push(BatchNormConfig::new(w[1]).init(device));
            }
        }
        ConvLayer { convs, gns, bns, pad: p }
    }

    fn forward(&self, mut x: Tensor<4>) -> Tensor<4> {
        let p = self.pad;
        for (i, c) in self.convs.iter().enumerate() {
            if p > 0 {
                x = x.pad((p, p, p, p), PadMode::Reflect);
            }
            x = c.forward(x);
            x = if let Some(g) = self.gns.get(i) { g.forward(x) } else { self.bns[i].forward(x) };
            x = relu(x);
        }
        x
    }
}

#[derive(Module, Debug)]
pub struct DownBlock {
    down: ConvLayer,
    conv1: ConvLayer,
    conv2: ConvLayer,
}

#[derive(Module, Debug)]
pub struct UpBlock {
    skip_conv: Conv2d,
    skip_bn: BatchNorm,
    up: ConvTranspose2d,
    up_bn: BatchNorm,
    conv1: ConvLayer,
    conv2: ConvLayer,
}

impl UpBlock {
    fn forward(&self, x: Tensor<4>, skip: Tensor<4>) -> Tensor<4> {
        let up = relu(self.up_bn.forward(self.up.forward(x)));
        let skip = relu(self.skip_bn.forward(self.skip_conv.forward(skip)));
        let out = self.conv1.forward(Tensor::cat(vec![up, skip], 1));
        out.clone() + self.conv2.forward(out)
    }
}

#[derive(Module, Debug)]
pub struct Heads {
    q: Param<Tensor<2>>,
    fc1_k: Linear,
}

/// L-TAE over each pixel of a sequence of feature maps (utae-paps' LTAE2d).
#[derive(Module, Debug)]
pub struct TemporalEncoder {
    inconv: Conv1d,
    attention_heads: Heads,
    in_norm: GroupNorm,
    out_norm: GroupNorm,
    mlp: Linear,
    mlp_bn: BatchNorm,
    attn_drop: Dropout,
    drop: Dropout,
    n_head: usize,
    d_k: usize,
}

impl TemporalEncoder {
    /// x [b, t, c, h, w], positions [b, t] (days), pad [b, t] (1 = no observation) -> [b, c', h, w] and the
    /// attention [heads, b, t, h, w].
    fn forward(&self, x: Tensor<5>, positions: Tensor<2>, pad: Tensor<2>) -> (Tensor<4>, Tensor<5>) {
        let [b, t, c, h, w] = x.dims();
        let n = b * h * w;
        let (heads, d_k) = (self.n_head, self.d_k);
        let out = x.permute([0, 3, 4, 1, 2]).reshape([n, t, c]);
        let out = self.in_norm.forward(out.swap_dims(1, 2)); // [n, c, t]
        let out = self.inconv.forward(out).swap_dims(1, 2); // [n, t, d_model]
        let d_model = out.dims()[2];
        // positional encoding of each pixel's days (T = 1000, d_model / heads values repeated for every head)
        let d = d_model / heads;
        let device = out.device();
        let denom: Vec<f32> = (0..d).map(|i| 1000f32.powf(2.0 * (i / 2) as f32 / d as f32)).collect();
        let denom = Tensor::<1>::from_data(TensorData::new(denom, [d]), &device).reshape([1, 1, d]);
        let pos = positions.reshape([b, 1, 1, t]).expand([b, h, w, t]).reshape([n, t, 1]);
        let table = pos / denom; // [n, t, d]
        let parity: Vec<f32> = (0..d).map(|i| (i % 2) as f32).collect();
        let odd = Tensor::<1>::from_data(TensorData::new(parity, [d]), &device).reshape([1, 1, d]);
        let enc = table.clone().sin() * (odd.clone().neg() + 1.0) + table.cos() * odd;
        let enc = Tensor::cat(vec![enc; heads], 2);
        let v = out + enc;
        // the master query against the keys, the values the input split among the heads
        let k = self.attention_heads.fc1_k.forward(v.clone()).reshape([n, t, heads, d_k]).permute([2, 0, 1, 3]);
        let q = self.attention_heads.q.val().reshape([heads, 1, 1, d_k]).expand([heads, n, 1, d_k]);
        let mut attn = q.matmul(k.swap_dims(2, 3)) / (d_k as f64).sqrt(); // [heads, n, 1, t]
        let padn = pad.reshape([b, 1, 1, t]).expand([b, h, w, t]).reshape([1, n, 1, t]);
        attn = attn * (padn.clone().neg() + 1.0) + padn * (-1e3);
        let attn = softmax(attn, 3);
        let attn_used = self.attn_drop.forward(attn.clone());
        let split = d_model / heads;
        let values = v.reshape([n, t, heads, split]).permute([2, 0, 1, 3]);
        let o = attn_used.matmul(values).reshape([heads, n, split]).swap_dims(0, 1).reshape([n, heads * split]);
        let o = self.drop.forward(relu(self.mlp_bn.forward(self.mlp.forward(o))));
        let c2 = o.dims()[1];
        let o = self.out_norm.forward(o.reshape([n, c2, 1])).reshape([b, h, w, c2]).permute([0, 3, 1, 2]);
        let attn = attn.reshape([heads, b, h, w, t]).permute([0, 1, 4, 2, 3]);
        (o, attn)
    }
}

#[derive(Module, Debug)]
pub struct Utae {
    in_conv: ConvLayer,
    down_blocks: Vec<DownBlock>,
    up_blocks: Vec<UpBlock>,
    temporal_encoder: TemporalEncoder,
    out_conv: ConvLayer,
}

impl Utae {
    pub fn new(h: &UtaeHyper, bands: usize, classes: usize, device: &Device) -> Utae {
        let (enc, dec) = (&h.encoder_widths, &h.decoder_widths);
        let stages = enc.len();
        let mut out_conv = vec![dec[0]];
        out_conv.extend_from_slice(&h.out_conv);
        *out_conv.last_mut().unwrap() = classes;
        Utae {
            in_conv: ConvLayer::new(&[bands, enc[0], enc[0]], true, 3, 1, 1, device),
            down_blocks: (0..stages - 1)
                .map(|i| DownBlock {
                    down: ConvLayer::new(&[enc[i], enc[i]], true, 4, 2, 1, device),
                    conv1: ConvLayer::new(&[enc[i], enc[i + 1]], true, 3, 1, 1, device),
                    conv2: ConvLayer::new(&[enc[i + 1], enc[i + 1]], true, 3, 1, 1, device),
                })
                .collect(),
            up_blocks: (1..stages)
                .rev()
                .map(|i| {
                    let (d_in, d_out, skip) = (dec[i], dec[i - 1], enc[i - 1]);
                    UpBlock {
                        skip_conv: Conv2dConfig::new([skip, skip], [1, 1]).init(device),
                        skip_bn: BatchNormConfig::new(skip).init(device),
                        up: ConvTranspose2dConfig::new([d_in, d_out], [4, 4])
                            .with_stride([2, 2])
                            .with_padding([1, 1])
                            .init(device),
                        up_bn: BatchNormConfig::new(d_out).init(device),
                        conv1: ConvLayer::new(&[d_out + skip, d_out], false, 3, 1, 1, device),
                        conv2: ConvLayer::new(&[d_out, d_out], false, 3, 1, 1, device),
                    }
                })
                .collect(),
            temporal_encoder: TemporalEncoder {
                inconv: Conv1dConfig::new(enc[stages - 1], h.d_model, 1).init(device),
                attention_heads: Heads {
                    q: Param::from_tensor(Tensor::random(
                        [h.n_head, h.d_k],
                        Distribution::Normal(0.0, (2.0 / h.d_k as f64).sqrt()),
                        device,
                    )),
                    fc1_k: LinearConfig::new(h.d_model, h.n_head * h.d_k).init(device),
                },
                in_norm: GroupNormConfig::new(h.n_head, enc[stages - 1]).init(device),
                out_norm: GroupNormConfig::new(h.n_head, enc[stages - 1]).init(device),
                mlp: LinearConfig::new(h.d_model, enc[stages - 1]).init(device),
                mlp_bn: BatchNormConfig::new(enc[stages - 1]).init(device),
                attn_drop: DropoutConfig::new(0.1).init(),
                drop: DropoutConfig::new(0.2).init(),
                n_head: h.n_head,
                d_k: h.d_k,
            },
            out_conv: ConvLayer::new(&out_conv, false, 3, 1, 1, device),
        }
    }

    /// The authors' weights (a utae-paps checkpoint: its `state_dict`), into a U-TAE of their configuration
    /// (their number of classes: see `with_classes`).
    pub fn load_authors(mut self, path: &Path) -> Result<Utae, String> {
        let mut store = PytorchStore::from_file(path)
            .with_top_level_key("state_dict")
            // a ConvBlock's ConvLayer's Sequential: conv, norm, ReLU, conv, norm, ReLU
            .with_key_remapping(r"^(in_conv|out_conv)\.conv\.conv\.", "${1}.conv.")
            .with_key_remapping(r"\.conv\.0\.", ".convs.0.")
            .with_key_remapping(r"\.conv\.3\.", ".convs.1.")
            .with_key_remapping(r"^(in_conv|down_blocks\.\d+\.\w+)\.conv\.1\.", "${1}.gns.0.")
            .with_key_remapping(r"^(in_conv|down_blocks\.\d+\.\w+)\.conv\.4\.", "${1}.gns.1.")
            .with_key_remapping(r"\.conv\.1\.", ".bns.0.")
            .with_key_remapping(r"\.conv\.4\.", ".bns.1.")
            .with_key_remapping(r"\.skip_conv\.0\.", ".skip_conv.")
            .with_key_remapping(r"\.skip_conv\.1\.", ".skip_bn.")
            .with_key_remapping(r"\.up\.0\.", ".up.")
            .with_key_remapping(r"\.up\.1\.", ".up_bn.")
            .with_key_remapping(r"attention_heads\.Q$", "attention_heads.q")
            .with_key_remapping(r"\.mlp\.0\.", ".mlp.")
            .with_key_remapping(r"\.mlp\.1\.", ".mlp_bn.");
        let r = self.load_from(&mut store).map_err(|e| format!("{}: U-TAE's weights: {e}", path.display()))?;
        let unused: Vec<&String> = r.unused.iter().filter(|k| !k.ends_with("num_batches_tracked")).collect();
        if !r.missing.is_empty() || !unused.is_empty() || !r.errors.is_empty() {
            return Err(format!(
                "{}: U-TAE's weights do not fit (missing {:?}, unused {unused:?}, errors {:?})",
                path.display(),
                r.missing,
                r.errors
            ));
        }
        Ok(self)
    }

    /// The same network with a new last convolution for `classes` classes (a pretrained one adapted to others).
    pub fn with_classes(mut self, classes: usize, device: &Device) -> Utae {
        let last = self.out_conv.convs.len() - 1;
        let c_in = self.out_conv.convs[last].weight.dims()[1];
        self.out_conv.convs[last] =
            Conv2dConfig::new([c_in, classes], [3, 3]).with_padding(PaddingConfig2d::Valid).init(device);
        if let Some(bn) = self.out_conv.bns.get_mut(last) {
            *bn = BatchNormConfig::new(classes).init(device);
        }
        self
    }

    /// Sums the dates of skip connections [b, t, c, H, W] weighted by the attention [heads, b, t, h, w] (resampled
    /// to H x W), each head its group of channels (utae-paps' Temporal_Aggregator, "att_group").
    fn aggregate(x: Tensor<5>, attn: Tensor<5>, pad: Tensor<2>) -> Tensor<4> {
        let [b, t, c, hh, ww] = x.dims();
        let [heads, _, _, h, w] = attn.dims();
        let mut a = attn.reshape([heads * b, t, h, w]);
        if hh > h {
            a = upsample(a, hh, ww);
        }
        let keep = pad.neg().add_scalar(1.0).reshape([1, b, t, 1, 1]);
        let a = a.reshape([heads, b, t, hh, ww]) * keep;
        let g = c / heads;
        let xs = x.reshape([b, t, heads, g, hh, ww]).permute([2, 0, 1, 3, 4, 5]); // [heads, b, t, g, H, W]
        let out = (a.reshape([heads, b, t, 1, hh, ww]) * xs).sum_dim(2).reshape([heads, b, g, hh, ww]);
        out.swap_dims(0, 1).reshape([b, heads * g, hh, ww])
    }

    /// Patches [b, t, bands, h, w] (dates of only 0 are no observation) and their dates' positions (days) [b, t]
    /// -> the classes' scores [b, classes, h, w].
    pub fn forward(&self, x: Tensor<5>, positions: Tensor<2>) -> Tensor<4> {
        let [b, t, c, h, w] = x.dims();
        let pad = x.clone().abs().reshape([b, t, c * h * w]).max_dim(2).reshape([b, t]).equal_elem(0.0).float();
        let keep = pad.clone().neg().add_scalar(1.0).reshape([b * t, 1, 1, 1]);
        // the encoder on every date (an absent date stays 0)
        let frames = x.reshape([b * t, c, h, w]);
        let mut maps = vec![self.in_conv.forward(frames) * keep.clone()];
        for d in &self.down_blocks {
            let prev = maps.last().unwrap().clone();
            let out = d.conv1.forward(d.down.forward(prev));
            maps.push((out.clone() + d.conv2.forward(out)) * keep.clone());
        }
        let five = |m: Tensor<4>| {
            let [_, cc, hh, ww] = m.dims();
            m.reshape([b, t, cc, hh, ww])
        };
        let (mut out, attn) = self.temporal_encoder.forward(five(maps.last().unwrap().clone()), positions, pad.clone());
        let n = maps.len();
        for (i, up) in self.up_blocks.iter().enumerate() {
            let skip = Self::aggregate(five(maps[n - 2 - i].clone()), attn.clone(), pad.clone());
            out = up.forward(out, skip);
        }
        self.out_conv.forward(out)
    }
}

/// The authors' U-TAE trained on PASTIS for semantic segmentation (fold 1; Zenodo 5172293, CC-BY-4.0): 10
/// Sentinel-2 bands (B2, B3, B4, B5, B6, B7, B8, B8A, B11, B12), 20 classes, dates as days since 2018-09-01.
pub const PASTIS_ZIP: &str = "https://zenodo.org/api/records/5172293/files/UTAE_SemanticSeg_weights.zip/content";
pub const PASTIS_MEMBER: &str = "UATE_zenodo/Fold_1/model.pth.tar";
pub const PASTIS_SHA256: &str = "23e8de5a9e35d3db36cfc83612f467d533bf15e05709c8cfd6f18e3e1fb6f926";
pub const PASTIS_CLASSES: usize = 20;
/// PASTIS's inputs, in its order (names as Presto's aliases find them)
pub const PASTIS_BANDS: [&str; 10] = ["B2", "B3", "B4", "B5", "B6", "B7", "B8", "B8A", "B11", "B12"];

/// The PASTIS weights' file: in a folder JANUS_WEIGHTS names, or Janus's cache (taken from the authors' zip on
/// first use, only its member, and checked against its SHA-256).
pub fn pastis_weights(
    cache: &Path,
    progress: &dyn Fn(f64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::path::PathBuf, String> {
    let name = "utae_pastis_fold1.pth.tar";
    let ok = |p: &Path| -> bool {
        let stamp = std::path::PathBuf::from(format!("{}.sha256", p.display()));
        if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == PASTIS_SHA256) {
            return true;
        }
        let good = std::fs::read(p).is_ok_and(|b| crate::fetch::sha256_hex(&b) == PASTIS_SHA256);
        if good {
            let _ = std::fs::write(&stamp, PASTIS_SHA256);
        }
        good
    };
    let given = std::env::var_os("JANUS_WEIGHTS").map(|d| std::path::PathBuf::from(d).join(name));
    for c in given.iter().chain(std::iter::once(&cache.join(name))) {
        if c.exists() && ok(c) {
            return Ok(c.clone());
        }
    }
    let path = cache.join(name);
    let vsi = format!("/vsizip/{{/vsicurl/{PASTIS_ZIP}}}/{PASTIS_MEMBER}");
    crate::fetch::fetch_vsi(&vsi, PASTIS_ZIP, &path, progress, cancel)?;
    if !ok(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{PASTIS_ZIP}: {PASTIS_MEMBER} is not U-TAE's PASTIS weights (its SHA-256 differs)"));
    }
    Ok(path)
}
