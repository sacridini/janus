//! The field boundary network of Fields of The World (Kerner et al. 2025, fieldsoftheworld/ftw-baselines, MIT) and
//! of WRI's Trazo models (its South American checkpoints, CC-BY-4.0): segmentation_models_pytorch's U-Net on an
//! EfficientNet-B3 encoder (lukemelas' EfficientNet as smp vendors it: TensorFlow's "same" padding fixed for its
//! 300 px images, swish, squeeze-and-excitation), 8 inputs (red, green, blue and NIR of two windows of the season,
//! the early one first) and 3 classes per pixel: background, a field's interior, a field's boundary.
//!
//! Layer for layer with the same names (after the remapping in `load_ftw`), so a checkpoint of either loads as it
//! is; the parity with PyTorch is tests/fieldnet_parity.rs.

use std::path::Path;

use burn::nn::conv::{Conv2d, Conv2dConfig};
use burn::nn::{BatchNorm, BatchNormConfig, PaddingConfig2d};
use burn::prelude::*;
use burn::store::{ModuleSnapshot, PytorchStore};
use burn::tensor::activation::{relu, sigmoid, silu};
use burn::tensor::module::adaptive_avg_pool2d;
use burn::tensor::ops::PadMode;

/// The network's classes, in its order.
pub const BACKGROUND: usize = 0;
pub const INTERIOR: usize = 1;
pub const BOUNDARY: usize = 2;
pub const CLASSES: usize = 3;
/// Its inputs: red, green, blue, NIR of window A, then of window B.
pub const INPUTS: usize = 8;

/// EfficientNet-B3's stages (lukemelas' block strings, scaled: width 1.2, depth 1.4): repeats, kernel, stride,
/// expansion, input and output filters.
const B3_STAGES: [(usize, usize, usize, usize, usize, usize); 7] = [
    (2, 3, 1, 1, 40, 24),
    (3, 3, 2, 6, 24, 32),
    (3, 5, 2, 6, 32, 48),
    (5, 3, 2, 6, 48, 96),
    (5, 5, 1, 6, 96, 136),
    (6, 5, 2, 6, 136, 232),
    (2, 3, 1, 6, 232, 384),
];
/// The blocks whose outputs the decoder takes (smp's out_indexes for B3), after the stem's.
const B3_OUT: [usize; 4] = [4, 7, 17, 25];
/// smp's decoder widths.
const DECODER: [usize; 5] = [256, 128, 64, 32, 16];
/// The image size lukemelas' static padding is computed for (B3's 300 px).
const B3_IMAGE: usize = 300;

/// TensorFlow's "same" padding of a convolution for an input of `size` (lukemelas' Conv2dStaticSamePadding): the
/// pixels before and after.
fn same_pad(size: usize, k: usize, s: usize) -> (usize, usize) {
    let out = size.div_ceil(s);
    let pad = ((out - 1) * s + k).saturating_sub(size);
    (pad / 2, pad - pad / 2)
}

fn bn(c: usize, eps: f64, device: &Device) -> BatchNorm {
    BatchNormConfig::new(c).with_epsilon(eps).init(device)
}

fn conv(i: usize, o: usize, k: usize, s: usize, groups: usize, bias: bool, device: &Device) -> Conv2d {
    Conv2dConfig::new([i, o], [k, k])
        .with_stride([s, s])
        .with_groups(groups)
        .with_bias(bias)
        .with_padding(PaddingConfig2d::Valid)
        .init(device)
}

/// A mobile inverted bottleneck: expansion (1x1), depthwise convolution, squeeze-and-excitation, projection (1x1),
/// the input added when the shape stays.
#[derive(Module, Debug)]
pub struct MbConv {
    expand_conv: Option<Conv2d>,
    bn0: Option<BatchNorm>,
    depthwise_conv: Conv2d,
    bn1: BatchNorm,
    se_reduce: Conv2d,
    se_expand: Conv2d,
    project_conv: Conv2d,
    bn2: BatchNorm,
    pad_lo: usize,
    pad_hi: usize,
    skip: bool,
}

impl MbConv {
    fn forward(&self, input: Tensor<4>) -> Tensor<4> {
        let mut x = input.clone();
        if let (Some(c), Some(b)) = (&self.expand_conv, &self.bn0) {
            x = silu(b.forward(c.forward(x)));
        }
        let (lo, hi) = (self.pad_lo, self.pad_hi);
        if lo + hi > 0 {
            x = x.pad((lo, hi, lo, hi), PadMode::Constant(0.0));
        }
        x = silu(self.bn1.forward(self.depthwise_conv.forward(x)));
        let s = adaptive_avg_pool2d(x.clone(), [1, 1]);
        let s = self.se_expand.forward(silu(self.se_reduce.forward(s)));
        x = sigmoid(s) * x;
        x = self.bn2.forward(self.project_conv.forward(x));
        if self.skip { x + input } else { x }
    }
}

/// Two 3x3 convolutions with batch normalization and ReLU after the input upsampled (nearest, x2) and the skip
/// connection appended (smp's UnetDecoderBlock without attention).
#[derive(Module, Debug)]
pub struct DecoderBlock {
    conv1: Conv2d,
    bn1: BatchNorm,
    conv2: Conv2d,
    bn2: BatchNorm,
}

/// Nearest upsampling by 2 (each pixel repeated), as F.interpolate to twice the size.
fn up2(x: Tensor<4>) -> Tensor<4> {
    let [n, c, h, w] = x.dims();
    x.reshape([n, c, h, 1, w, 1]).expand([n, c, h, 2, w, 2]).reshape([n, c, 2 * h, 2 * w])
}

impl DecoderBlock {
    fn forward(&self, x: Tensor<4>, skip: Option<Tensor<4>>) -> Tensor<4> {
        let mut x = up2(x);
        if let Some(s) = skip {
            x = Tensor::cat(vec![x, s], 1);
        }
        let x = relu(self.bn1.forward(self.conv1.forward(x.pad((1, 1, 1, 1), PadMode::Constant(0.0)))));
        relu(self.bn2.forward(self.conv2.forward(x.pad((1, 1, 1, 1), PadMode::Constant(0.0)))))
    }
}

#[derive(Module, Debug)]
pub struct Encoder {
    conv_stem: Conv2d,
    bn0: BatchNorm,
    blocks: Vec<MbConv>,
    stem_lo: usize,
    stem_hi: usize,
}

#[derive(Module, Debug)]
pub struct Decoder {
    blocks: Vec<DecoderBlock>,
}

/// The U-Net: an image [b, 8, h, w] (h, w multiples of 32) -> the classes' scores [b, 3, h, w].
#[derive(Module, Debug)]
pub struct FieldNet {
    encoder: Encoder,
    decoder: Decoder,
    segmentation_head: Conv2d,
}

impl FieldNet {
    /// Every batch normalization frozen (its running statistics used, its scale and shift kept): fine-tuning
    /// with batches of a few chips, whose own statistics would replace those of the network's training.
    pub fn freeze_norms(mut self) -> FieldNet {
        let e = &mut self.encoder;
        e.bn0 = e.bn0.clone().freeze();
        for b in e.blocks.iter_mut() {
            b.bn0 = b.bn0.take().map(|n| n.freeze());
            b.bn1 = b.bn1.clone().freeze();
            b.bn2 = b.bn2.clone().freeze();
        }
        for b in self.decoder.blocks.iter_mut() {
            b.bn1 = b.bn1.clone().freeze();
            b.bn2 = b.bn2.clone().freeze();
        }
        self
    }

    pub fn new(inputs: usize, classes: usize, device: &Device) -> FieldNet {
        let eps = 1e-3;
        let mut size = B3_IMAGE;
        let (stem_lo, stem_hi) = same_pad(size, 3, 2);
        size = size.div_ceil(2);
        let mut blocks = vec![];
        for &(repeats, k, s, e, i, o) in &B3_STAGES {
            for r in 0..repeats {
                let (inp, stride) = if r == 0 { (i, s) } else { (o, 1) };
                let exp = inp * e;
                let (pad_lo, pad_hi) = same_pad(size, k, stride);
                if r == 0 {
                    size = size.div_ceil(stride);
                }
                let sq = (inp / 4).max(1);
                blocks.push(MbConv {
                    expand_conv: (e != 1).then(|| conv(inp, exp, 1, 1, 1, false, device)),
                    bn0: (e != 1).then(|| bn(exp, eps, device)),
                    depthwise_conv: conv(exp, exp, k, stride, exp, false, device),
                    bn1: bn(exp, eps, device),
                    se_reduce: conv(exp, sq, 1, 1, 1, true, device),
                    se_expand: conv(sq, exp, 1, 1, 1, true, device),
                    project_conv: conv(exp, o, 1, 1, 1, false, device),
                    bn2: bn(o, eps, device),
                    pad_lo,
                    pad_hi,
                    skip: stride == 1 && inp == o,
                });
            }
        }
        // the decoder: from the encoder's head (384), each block's skip the next shallower feature
        let skips = [136, 48, 32, 40, 0];
        let mut ins = vec![384];
        ins.extend_from_slice(&DECODER[..4]);
        let dec = (0..5)
            .map(|j| DecoderBlock {
                conv1: conv(ins[j] + skips[j], DECODER[j], 3, 1, 1, false, device),
                bn1: bn(DECODER[j], 1e-5, device),
                conv2: conv(DECODER[j], DECODER[j], 3, 1, 1, false, device),
                bn2: bn(DECODER[j], 1e-5, device),
            })
            .collect();
        FieldNet {
            encoder: Encoder {
                conv_stem: conv(inputs, 40, 3, 2, 1, false, device),
                bn0: bn(40, eps, device),
                blocks,
                stem_lo,
                stem_hi,
            },
            decoder: Decoder { blocks: dec },
            segmentation_head: Conv2dConfig::new([16, classes], [3, 3])
                .with_padding(PaddingConfig2d::Explicit(1, 1, 1, 1))
                .init(device),
        }
    }

    /// The encoder's features: the stem's (1/2) and the blocks' of B3_OUT (1/4 .. 1/32).
    pub fn features(&self, x: Tensor<4>) -> Vec<Tensor<4>> {
        let e = &self.encoder;
        let x = x.pad((e.stem_lo, e.stem_hi, e.stem_lo, e.stem_hi), PadMode::Constant(0.0));
        let mut x = silu(e.bn0.forward(e.conv_stem.forward(x)));
        let mut out = vec![x.clone()];
        for (i, b) in e.blocks.iter().enumerate() {
            x = b.forward(x);
            if B3_OUT.contains(&i) {
                out.push(x.clone());
            }
        }
        out
    }

    /// The classes' scores (logits) [b, classes, h, w] of images [b, inputs, h, w].
    pub fn forward(&self, x: Tensor<4>) -> Tensor<4> {
        let f = self.features(x);
        let mut x = f[4].clone();
        for (j, block) in self.decoder.blocks.iter().enumerate() {
            let skip = (j < 4).then(|| f[3 - j].clone());
            x = block.forward(x, skip);
        }
        self.segmentation_head.forward(x)
    }

    /// The weights of an FTW or Trazo checkpoint (a PyTorch Lightning .ckpt: its `state_dict`, the network under
    /// `model.`).
    pub fn load_ftw(mut self, path: &Path) -> Result<FieldNet, String> {
        let mut store = PytorchStore::from_file(path)
            .with_top_level_key("state_dict")
            .with_key_remapping(r"^model\.", "")
            // lukemelas' names: _conv_stem, _blocks.3._bn1 ...
            .with_key_remapping(r"(^|\.)_", "${1}")
            .with_key_remapping(r"^decoder\.blocks\.(\d+)\.conv(\d)\.0\.", "decoder.blocks.${1}.conv${2}.")
            .with_key_remapping(r"^decoder\.blocks\.(\d+)\.conv(\d)\.1\.", "decoder.blocks.${1}.bn${2}.")
            .with_key_remapping(r"^segmentation_head\.0\.", "segmentation_head.");
        let r =
            self.load_from(&mut store).map_err(|e| format!("{}: the field network's weights: {e}", path.display()))?;
        // EfficientNet's classification head (conv_head, bn1) is in the checkpoint but not in a U-Net's encoder
        let unused: Vec<&String> = r
            .unused
            .iter()
            .filter(|k| {
                !k.ends_with("num_batches_tracked")
                    && !k.starts_with("criterion")
                    && !k.starts_with("encoder.conv_head")
                    && !k.starts_with("encoder.bn1.")
            })
            .collect();
        if !r.missing.is_empty() || !unused.is_empty() || !r.errors.is_empty() {
            return Err(format!(
                "{}: the field network's weights do not fit (missing {:?}, unused {unused:?}, errors {:?})",
                path.display(),
                r.missing,
                r.errors
            ));
        }
        Ok(self)
    }
}

/// A published checkpoint of the network.
#[derive(Clone, Copy, Debug)]
pub struct Checkpoint {
    pub id: &'static str,
    pub title: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub mb: u32,
    /// the upsampling it is run with: the Trazo checkpoints none (trained at the chips' resolution: their test
    /// sets' pixel IoU 77 % in Mato Grosso, upsampled twice 55 %), FTW's twice (ftw-tools')
    pub resize: usize,
    /// where it is trained and what it is best at (its model card's words)
    pub about: &'static str,
    pub cite: &'static str,
}

const TRAZO_CITE: &str = "Grupp et al. 2026, Field Boundaries of South America, WRI Technical Note, \
doi:10.46830/writn.25.00033 (CC-BY-4.0)";
const FTW_CITE: &str = "Kerner et al. 2025, Fields of The World, AAAI 39(27):28151 (CC-BY-4.0 checkpoint)";

/// The checkpoints Janus offers (all CC-BY-4.0, pinned to their revision and SHA-256); the first is the default.
pub const CHECKPOINTS: [Checkpoint; 5] = [
    Checkpoint {
        id: "trazo3",
        title: "Trazo3 (South America, best in Mato Grosso)",
        url: "https://huggingface.co/worldresourcesinstitute/Trazo3/resolve/fca3f0b645edd7407273b6a40b6082b5a67ce60c/Trazo3.ckpt",
        sha256: "c5b7e4ff46215046d0898737b7e988a88602deb8662c64cd790f71367c876c06",
        mb: 204,
        resize: 1,
        about: "Trazo2 plus 400 hard samples of Mato Grosso: the best pixel IoU there (79 %) and the best object recall; \
trained on median composites of the two windows.",
        cite: TRAZO_CITE,
    },
    Checkpoint {
        id: "trazosa",
        title: "TrazoSA (South America only)",
        url: "https://huggingface.co/worldresourcesinstitute/TrazoSA/resolve/79121c08e1a8fb0dc8f62acd8dce1939e91e28c8/TrazoSA.ckpt",
        sha256: "ae5fe1044934a65373ceb84d1746d65112879b249be7f3b262cae54927bc3414",
        mb: 204,
        resize: 1,
        about: "Only the 17 soy ecoregions of South America and partner data: the highest precision (pixels and objects).",
        cite: TRAZO_CITE,
    },
    Checkpoint {
        id: "trazo2",
        title: "Trazo2 (global FTW + South America)",
        url: "https://huggingface.co/worldresourcesinstitute/Trazo2/resolve/d49d73a9ae81c65f54da6fb3688b329e674230e0/Trazo2.ckpt",
        sha256: "65b76f1d39246640ee6d83b7d9cf077e2abdb38c08396a4372bfa947f42955ea",
        mb: 204,
        resize: 1,
        about: "The whole FTW dataset and the 17 South American ecoregions.",
        cite: TRAZO_CITE,
    },
    Checkpoint {
        id: "trazo1",
        title: "Trazo1 (large row crops)",
        url: "https://huggingface.co/worldresourcesinstitute/Trazo1/resolve/4717ff281eae0c479720e25681a2325355e49415/Trazo1.ckpt",
        sha256: "92236cb8e62c89e097b176ae0285c6a94e114c377a641e448f21a953be31136c",
        mb: 204,
        resize: 1,
        about: "The first Trazo, more sensitive to boundaries: large uniform row-crop fields.",
        cite: TRAZO_CITE,
    },
    Checkpoint {
        id: "ftw-b3",
        title: "FTW v3.1 B3 (global, 24 countries)",
        url: "https://github.com/fieldsoftheworld/ftw-baselines/releases/download/v3.1/prue_efnetb3_ccby_checkpoint.ckpt",
        sha256: "cd51f020168e66ddd1eff501e81da347b39632af34bebf19a6a20d6c73c5a6d4",
        mb: 53,
        resize: 2,
        about: "Fields of The World's B3 trained on its CC-BY countries only (Europe, Africa, Asia, South America): \
outside South America.",
        cite: FTW_CITE,
    },
];

pub fn checkpoint(id: &str) -> Option<&'static Checkpoint> {
    CHECKPOINTS.iter().find(|c| c.id == id)
}

/// A checkpoint's file: in a folder JANUS_WEIGHTS names, or Janus's cache (downloaded on first use and checked
/// against its SHA-256).
pub fn weights(
    cache: &Path,
    ck: &Checkpoint,
    progress: &dyn Fn(f64),
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<std::path::PathBuf, String> {
    let name = format!("fields_{}.ckpt", ck.id);
    let ok = |p: &Path| -> bool {
        let stamp = std::path::PathBuf::from(format!("{}.sha256", p.display()));
        if std::fs::read_to_string(&stamp).is_ok_and(|s| s.trim() == ck.sha256) {
            return true;
        }
        let good = std::fs::read(p).is_ok_and(|b| crate::fetch::sha256_hex(&b) == ck.sha256);
        if good {
            let _ = std::fs::write(&stamp, ck.sha256);
        }
        good
    };
    let given = std::env::var_os("JANUS_WEIGHTS").map(|d| std::path::PathBuf::from(d).join(&name));
    for c in given.iter().chain(std::iter::once(&cache.join(&name))) {
        if c.exists() && ok(c) {
            return Ok(c.clone());
        }
    }
    let path = cache.join(&name);
    crate::fetch::fetch(ck.url, &path, progress, cancel)?;
    if !ok(&path) {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{}: not {}'s checkpoint (its SHA-256 differs)", ck.url, ck.title));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_padding_is_lukemelas() {
        // B3 at 300 px: the stem and stride-2 3x3 pad (0, 1), stride-2 5x5 (2, 2), stride 1 (k/2, k/2)
        assert_eq!(same_pad(300, 3, 2), (0, 1));
        assert_eq!(same_pad(150, 3, 1), (1, 1));
        assert_eq!(same_pad(150, 3, 2), (0, 1));
        assert_eq!(same_pad(75, 5, 2), (2, 2));
        assert_eq!(same_pad(38, 3, 2), (0, 1));
        assert_eq!(same_pad(19, 5, 2), (2, 2));
    }
}
