//! L-TAE, the lightweight temporal attention encoder (Sainte Fare Garnot & Landrieu 2020, "Lightweight Temporal
//! Self-Attention for Classifying Satellite Images Time Series"), written from the paper and from the authors' code
//! (VSainteuf/lightweight-temporal-attention-pytorch, MIT), and a pixel classifier built on it. Where the paper and
//! the code differ the code is followed: sine and cosine positional encoding, one key layer over all the channels,
//! the extra layer norms. Weights trained by other implementations (the R package sits' among them) do not load
//! into this network.
//!
//! The authors' encoder of each date is a pixel-set encoder, which summarizes the pixels of a parcel; a sample here
//! is one pixel, so each date goes through an MLP instead (linear, batch normalization and ReLU to 32 and 64
//! units, then a linear layer of 128 units and a batch normalization, as the pixel-set encoder's two MLPs without
//! the pooling between them), its weights shared by the dates. Then the L-TAE: a layer norm of the channels, a
//! projection to `d_model` (a 1x1 convolution) and a layer norm over channels and dates together (so the number of
//! dates is fixed by the network), the positional encoding of each date's day, 16 heads each with a learned master
//! query against keys of 8 values, the heads' outputs concatenated, an MLP, dropout and a last layer norm; and an
//! MLP decoder of logits (128, 64, 32, classes).
//!
//! The authors' initialization is kept (normal weights and biases of the projection, Xavier-normal linear layers
//! with normal biases, normal batch normalization scales). A batch is [n, dates, bands].

use burn::module::Param;
use burn::nn::{
    BatchNorm, BatchNormConfig, Dropout, DropoutConfig, Initializer, LayerNorm, LayerNormConfig, Linear, LinearConfig,
};
use burn::prelude::*;
use burn::tensor::Distribution;
use burn::tensor::activation::{relu, softmax};
use serde::{Deserialize, Serialize};

pub use crate::tempcnn::VERSION;

/// L-TAE's architecture (the authors' configuration of the paper: 16 heads of 8 keys, a width of 256).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LTaeHyper {
    /// the networks' generation (`tempcnn::VERSION`); a model of an older one is refused when opened (model.rs)
    #[serde(default)]
    pub version: u32,
    pub n_head: usize,
    pub d_k: usize,
    /// the width the channels are projected to (a multiple of the heads) and the L-TAE's output
    pub d_model: usize,
    pub width: usize,
    /// the dropout after the MLP, and the one on the attention weights
    pub dropout: f64,
    pub attn_dropout: f64,
    /// the positional encoding's temperature
    pub temperature: f64,
    /// each date's MLP (its units, the last the L-TAE's input) and the decoder's hidden layers
    pub encoder: Vec<usize>,
    pub decoder: Vec<usize>,
}

impl Default for LTaeHyper {
    fn default() -> Self {
        LTaeHyper {
            version: VERSION,
            n_head: 16,
            d_k: 8,
            d_model: 256,
            width: 128,
            dropout: 0.2,
            attn_dropout: 0.1,
            temperature: 1000.0,
            encoder: vec![32, 64, 128],
            decoder: vec![64, 32],
        }
    }
}

/// The authors' sinusoidal table: [dates][d * repeat] (flat, row by row), `sin(p / temperature^(2 (j / 2) / d))` for
/// even `j` and `cos` of the same for odd `j` (p the date's day since the first, j the column in 0..d), the `d`
/// columns repeated once for each head.
pub fn positional_table(days: &[f64], d: usize, temperature: f64, repeat: usize) -> Vec<f32> {
    let mut table = Vec::with_capacity(days.len() * d * repeat);
    for &p in days {
        let row: Vec<f32> = (0..d)
            .map(|j| {
                let angle = p / temperature.powf(2.0 * (j / 2) as f64 / d as f64);
                (if j % 2 == 0 { angle.sin() } else { angle.cos() }) as f32
            })
            .collect();
        for _ in 0..repeat {
            table.extend_from_slice(&row);
        }
    }
    table
}

/// A linear layer as the authors initialize it: Xavier-normal weights (or `weights`) and normal biases.
fn linear(i: usize, o: usize, weights: Initializer, device: &Device) -> Linear {
    let mut l = LinearConfig::new(i, o).with_initializer(weights).init(device);
    l.bias = Some(Initializer::Normal { mean: 0.0, std: 1.0 }.init([o], device));
    l
}

fn xavier(i: usize, o: usize, device: &Device) -> Linear {
    linear(i, o, Initializer::XavierNormal { gain: 1.0 }, device)
}

/// A batch normalization with the authors' normal scale.
fn batch_norm(n: usize, device: &Device) -> BatchNorm {
    let mut bn = BatchNormConfig::new(n).init(device);
    bn.gamma = Initializer::Normal { mean: 0.0, std: 1.0 }.init([n], device);
    bn
}

/// A layer norm over the channels and the dates together, with a scale and a shift for each of them (torch's
/// `LayerNorm((channels, dates))`): [n, dates, channels].
#[derive(Module, Debug)]
pub struct SequenceNorm {
    gamma: Param<Tensor<2>>,
    beta: Param<Tensor<2>>,
}

impl SequenceNorm {
    fn new(times: usize, channels: usize, device: &Device) -> SequenceNorm {
        SequenceNorm {
            gamma: Initializer::Ones.init([times, channels], device),
            beta: Initializer::Zeros.init([times, channels], device),
        }
    }

    fn forward(&self, x: Tensor<3>) -> Tensor<3> {
        let mean = x.clone().mean_dim(2).mean_dim(1);
        let centred = x - mean;
        let var = centred.clone().powf_scalar(2.0).mean_dim(2).mean_dim(1);
        let normed = centred / (var + 1e-5).sqrt();
        normed * self.gamma.val().unsqueeze_dim::<3>(0) + self.beta.val().unsqueeze_dim::<3>(0)
    }
}

#[derive(Module, Debug)]
pub struct LTae {
    enc_linear: Vec<Linear>,
    enc_norm: Vec<BatchNorm>,
    in_norm: LayerNorm,
    inconv: Linear,
    seq_norm: SequenceNorm,
    /// the positional encoding of the dates, [dates, d_model] (fixed: not a weight)
    positions: Tensor<2>,
    q: Param<Tensor<2>>,
    fc1_k: Linear,
    attn_drop: Dropout,
    mlp: Linear,
    mlp_norm: BatchNorm,
    drop: Dropout,
    out_norm: LayerNorm,
    dec_linear: Vec<Linear>,
    dec_norm: Vec<BatchNorm>,
    dec_out: Linear,
    n_head: usize,
    d_k: usize,
}

impl LTae {
    /// `days`: each date's day since the first (`Calendar::day_offsets`).
    pub fn new(h: &LTaeHyper, bands: usize, days: &[f64], classes: usize, device: &Device) -> LTae {
        assert!(h.d_model % h.n_head == 0, "the width {} is not a multiple of the {} heads", h.d_model, h.n_head);
        assert!(!h.encoder.is_empty(), "the encoder of each date needs a layer");
        let (e, times) = (h.d_model, days.len());
        let (mut enc_linear, mut enc_norm) = (vec![], vec![]);
        let mut w = bands;
        for &next in &h.encoder {
            enc_linear.push(xavier(w, next, device));
            enc_norm.push(batch_norm(next, device));
            w = next;
        }
        let into_ltae = w;
        let (mut dec_linear, mut dec_norm) = (vec![], vec![]);
        let mut w = h.width;
        for &next in &h.decoder {
            dec_linear.push(xavier(w, next, device));
            dec_norm.push(batch_norm(next, device));
            w = next;
        }
        let table = positional_table(days, e / h.n_head, h.temperature, h.n_head);
        LTae {
            enc_linear,
            enc_norm,
            in_norm: LayerNormConfig::new(into_ltae).init(device),
            inconv: linear(into_ltae, e, Initializer::Normal { mean: 0.0, std: 1.0 }, device),
            seq_norm: SequenceNorm::new(times, e, device),
            positions: Tensor::from_data(TensorData::new(table, [times, e]), device),
            q: Param::from_tensor(Tensor::random(
                [h.n_head, h.d_k],
                Distribution::Normal(0.0, (2.0 / h.d_k as f64).sqrt()),
                device,
            )),
            fc1_k: xavier(e, h.n_head * h.d_k, device),
            attn_drop: DropoutConfig::new(h.attn_dropout).init(),
            mlp: xavier(e, h.width, device),
            mlp_norm: batch_norm(h.width, device),
            drop: DropoutConfig::new(h.dropout).init(),
            out_norm: LayerNormConfig::new(h.width).init(device),
            dec_linear,
            dec_norm,
            dec_out: xavier(w, classes, device),
            n_head: h.n_head,
            d_k: h.d_k,
        }
    }

    /// [n, dates, bands] -> each date's encoding [n, dates, input of the L-TAE].
    fn encode_dates(&self, x: Tensor<3>) -> Tensor<3> {
        let [n, t, b] = x.dims();
        let mut x = x.reshape([n * t, b]);
        let last = self.enc_linear.len() - 1;
        for (i, (l, bn)) in self.enc_linear.iter().zip(&self.enc_norm).enumerate() {
            x = bn.forward(l.forward(x));
            if i < last {
                x = relu(x);
            }
        }
        let c = x.dims()[1];
        x.reshape([n, t, c])
    }

    /// The attention of the heads over the dates: [n, dates, d_model] (the encodings with their positions) ->
    /// the heads' outputs side by side [n, d_model], and their weights [n, heads, dates] (each head's sum to 1).
    fn attend(&self, z: Tensor<3>) -> (Tensor<2>, Tensor<3>) {
        let [n, t, e] = z.dims();
        let (heads, d_k) = (self.n_head, self.d_k);
        let keys = self.fc1_k.forward(z.clone()).reshape([n, t, heads, d_k]);
        let query = self.q.val().reshape([1, 1, heads, d_k]);
        let scores = (keys * query).sum_dim(3).reshape([n, t, heads]).swap_dims(1, 2) / (d_k as f64).sqrt();
        let weights = softmax(scores, 2);
        // the values are the channels themselves, head h's the h-th group of d_model / heads
        let values = z.reshape([n, t, heads, e / heads]).permute([0, 2, 1, 3]);
        let used = self.attn_drop.forward(weights.clone()).reshape([n, heads, 1, t]);
        (used.matmul(values).reshape([n, e]), weights)
    }

    /// The L-TAE over each pixel's encoded dates [n, dates, channels] -> [n, width], and the attention weights.
    fn temporal(&self, x: Tensor<3>) -> (Tensor<2>, Tensor<3>) {
        let t = x.dims()[1];
        assert_eq!(t, self.positions.dims()[0], "this L-TAE reads {} dates, not {t}", self.positions.dims()[0]);
        let x = self.inconv.forward(self.in_norm.forward(x));
        let z = self.seq_norm.forward(x) + self.positions.clone().unsqueeze_dim::<3>(0);
        let (o, weights) = self.attend(z);
        (self.out_norm.forward(self.drop.forward(relu(self.mlp_norm.forward(self.mlp.forward(o))))), weights)
    }

    /// The attention weights [n, heads, dates] of pixels' series [n, dates, bands].
    pub fn attention(&self, x: Tensor<3>) -> Tensor<3> {
        self.temporal(self.encode_dates(x)).1
    }

    /// [n, dates, bands] -> the classes' logits [n, classes].
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        let mut o = self.temporal(self.encode_dates(x)).0;
        for (l, bn) in self.dec_linear.iter().zip(&self.dec_norm) {
            o = relu(bn.forward(l.forward(o)));
        }
        self.dec_out.forward(o)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAYS: [f64; 10] = [0.0, 16.0, 32.0, 48.0, 64.0, 80.0, 96.0, 112.0, 128.0, 144.0];

    fn net(bands: usize, classes: usize) -> LTae {
        LTae::new(&LTaeHyper::default(), bands, &DAYS, classes, &Device::flex())
    }

    fn input(n: usize, bands: usize) -> Tensor<3> {
        let data: Vec<f32> = (0..n * DAYS.len() * bands).map(|i| ((i * 37 % 101) as f32) / 50.0 - 1.0).collect();
        Tensor::<3>::from_data(TensorData::new(data, [n, DAYS.len(), bands]), &Device::flex())
    }

    #[test]
    fn parameters_are_the_architectures() {
        // 3 bands, 10 dates, 4 classes. Each date's MLP: 3-32-64-128 with its three batch norms
        let encoder = (3 * 32 + 32) + 2 * 32 + (32 * 64 + 64) + 2 * 64 + (64 * 128 + 128) + 2 * 128;
        // layer norm of 128; projection to 256; layer norm over (10, 256); queries 16 x 8; keys 256 -> 128;
        // MLP 256 -> 128 and its batch norm; last layer norm
        let ltae = 2 * 128
            + (128 * 256 + 256)
            + 2 * 10 * 256
            + 16 * 8
            + (256 * 128 + 128)
            + (256 * 128 + 128)
            + 2 * 128
            + 2 * 128;
        // decoder 128-64-32-4
        let decoder = (128 * 64 + 64) + 2 * 64 + (64 * 32 + 32) + 2 * 32 + (32 * 4 + 4);
        assert_eq!(encoder, 11_008);
        assert_eq!(ltae, 104_832);
        assert_eq!(decoder, 10_660);
        // (Burn's count also holds the batch norms' running mean and variance: 2 * (32 + 64 + 128 + 128 + 64 + 32))
        assert_eq!(net(3, 4).num_params() - 2 * (32 + 64 + 128 + 128 + 64 + 32), 126_500);
    }

    #[test]
    fn positional_table_follows_the_authors() {
        let t = positional_table(&[0.0, 1.0, 30.0], 16, 1000.0, 3);
        assert_eq!(t.len(), 3 * 48);
        // the first date: sin 0 and cos 0, alternating
        for j in 0..48 {
            assert_eq!(t[j], (j % 2) as f32);
        }
        // day 1: column 2 is sin(1 / 1000^(2/16)), column 3 its cosine, column 5 cos(1 / 1000^(2/16))
        let a = 1.0f64 / 1000f64.powf(2.0 / 16.0);
        assert!((t[48 + 2] as f64 - a.sin()).abs() < 1e-6);
        assert!((t[48 + 3] as f64 - a.cos()).abs() < 1e-6);
        // columns 0 and 1 share the angle p (the exponent is 0)
        assert!((t[96] as f64 - 30f64.sin()).abs() < 1e-5 && (t[97] as f64 - 30f64.cos()).abs() < 1e-5);
        // every head has the same 16 columns
        for date in 0..3 {
            for j in 0..16 {
                assert_eq!(t[date * 48 + j], t[date * 48 + 16 + j]);
                assert_eq!(t[date * 48 + j], t[date * 48 + 32 + j]);
            }
        }
    }

    #[test]
    fn the_network_holds_the_table_of_its_days() {
        let m = net(3, 4);
        let d = m.positions.clone().into_data().try_to_vec::<f32>().unwrap();
        assert_eq!(d, positional_table(&DAYS, 16, 1000.0, 16));
    }

    #[test]
    fn attention_weights_sum_to_one() {
        let m = net(2, 5);
        let w = m.attention(input(6, 2));
        assert_eq!(w.dims(), [6, 16, 10]);
        let sums = w.sum_dim(2).into_data().try_to_vec::<f32>().unwrap();
        assert!(sums.iter().all(|s| (s - 1.0).abs() < 1e-5), "{sums:?}");
    }

    #[test]
    fn a_head_averages_its_group_of_channels() {
        let m = net(2, 5);
        let z = m.seq_norm.forward(m.inconv.forward(m.in_norm.forward(m.encode_dates(input(3, 2))))) + m.positions.clone().unsqueeze_dim::<3>(0);
        let (o, w) = m.attend(z.clone());
        let (z, o, w) = (
            z.into_data().try_to_vec::<f32>().unwrap(),
            o.into_data().try_to_vec::<f32>().unwrap(),
            w.into_data().try_to_vec::<f32>().unwrap(),
        );
        // pixel 1, head 3: the weighted mean over the dates of channels 48..64
        let (n, h, eh) = (1usize, 3usize, 16usize);
        for c in 0..eh {
            let mean: f32 = (0..10).map(|t| w[(n * 16 + h) * 10 + t] * z[(n * 10 + t) * 256 + h * eh + c]).sum();
            assert!((o[n * 256 + h * eh + c] - mean).abs() < 1e-4, "channel {c}");
        }
    }

    #[test]
    fn logits_have_a_class_each_and_prediction_is_deterministic() {
        let m = net(2, 5);
        let (a, b) = (m.forward(input(7, 2)), m.forward(input(7, 2)));
        assert_eq!(a.dims(), [7, 5]);
        assert_eq!(a.into_data().try_to_vec::<f32>().unwrap(), b.into_data().try_to_vec::<f32>().unwrap());
    }
}
