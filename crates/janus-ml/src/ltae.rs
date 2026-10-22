//! The Lightweight Temporal Attention Encoder (Garnot & Landrieu 2020) of a pixel's series, as sits_lighttae builds
//! it: an MLP of each date's values (the pixel-set encoder of one pixel), a layer normalization, a 1x1 convolution
//! to d_model with a layer normalization over (d_model, dates), the positional encoding of the days since the first
//! date, attention heads with a learnt master query and keys (the values are the input split among the heads), an
//! MLP, dropout and a layer normalization; then the decoder's MLP to the classes (with batch normalization and ReLU
//! on every layer, the scores' too, as sits).

use burn::module::Param;
use burn::nn::conv::{Conv1d, Conv1dConfig};
use burn::nn::{
    BatchNorm, BatchNormConfig, Dropout, DropoutConfig, LayerNorm, LayerNormConfig, Linear, LinearConfig, Relu,
};
use burn::prelude::*;
use burn::tensor::Distribution;
use burn::tensor::activation::softmax;
use serde::{Deserialize, Serialize};

/// sits_lighttae's architecture (its defaults).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LTaeHyper {
    pub layers_spatial_encoder: Vec<usize>,
    pub n_heads: usize,
    pub n_neurons: Vec<usize>,
    pub dropout_rate: f64,
    pub dim_layers_decoder: Vec<usize>,
}

impl Default for LTaeHyper {
    fn default() -> Self {
        LTaeHyper {
            layers_spatial_encoder: vec![32, 64, 128],
            n_heads: 16,
            n_neurons: vec![256, 128],
            dropout_rate: 0.2,
            dim_layers_decoder: vec![64, 32],
        }
    }
}

/// Linear, batch normalization, ReLU (sits' .torch_linear_batch_norm_relu), layer after layer.
#[derive(Module, Debug)]
pub struct Mlp {
    linear: Vec<Linear>,
    bn: Vec<BatchNorm>,
}

impl Mlp {
    fn new(input: usize, hidden: &[usize], device: &Device) -> Mlp {
        let mut c = input;
        let (mut linear, mut bn) = (vec![], vec![]);
        for &h in hidden {
            linear.push(LinearConfig::new(c, h).init(device));
            bn.push(BatchNormConfig::new(h).init(device));
            c = h;
        }
        Mlp { linear, bn }
    }

    /// [n, c] -> [n, last]
    fn forward(&self, mut x: Tensor<2>) -> Tensor<2> {
        for (l, b) in self.linear.iter().zip(&self.bn) {
            x = Relu::new().forward(b.forward(l.forward(x)));
        }
        x
    }
}

#[derive(Module, Debug)]
pub struct LTae {
    spatial: Mlp,
    in_norm: LayerNorm,
    inconv: Conv1d,
    /// the layer normalization over (d_model, dates) of the convolution's output
    inconv_gamma: Param<Tensor<2>>,
    inconv_beta: Param<Tensor<2>>,
    /// the positional encoding [dates, d_model] (fixed)
    pos: Param<Tensor<2>>,
    query: Param<Tensor<2>>,
    fc_k: Linear,
    attn_drop: Dropout,
    mlp: Mlp,
    drop: Dropout,
    out_norm: LayerNorm,
    decoder: Mlp,
    n_heads: usize,
    d_k: usize,
}

/// sits' .torch_positional_encoding: sin and cos of the days by an exponential scale of the dimensions.
pub fn positional_encoding(days: &[f64], d: usize) -> Vec<f32> {
    let mut p = vec![0f32; days.len() * d];
    for (t, &day) in days.iter().enumerate() {
        for i in (0..d).step_by(2) {
            let div = (i as f64 * (-(1000f64.ln()) / d as f64)).exp();
            p[t * d + i] = (day * div).sin() as f32;
            if i + 1 < d {
                p[t * d + i + 1] = (day * div).cos() as f32;
            }
        }
    }
    p
}

impl LTae {
    pub fn new(h: &LTaeHyper, bands: usize, days: &[f64], classes: usize, device: &Device) -> LTae {
        let t = days.len();
        let enc = *h.layers_spatial_encoder.last().unwrap_or(&128);
        let d_model = h.n_neurons[0];
        let d_k = enc / h.n_heads;
        let pos = Tensor::<2>::from_data(TensorData::new(positional_encoding(days, d_model), [t, d_model]), device);
        let mut decoder = h.dim_layers_decoder.clone();
        decoder.push(classes);
        let last = *h.n_neurons.last().unwrap_or(&128);
        LTae {
            spatial: Mlp::new(bands, &h.layers_spatial_encoder, device),
            in_norm: LayerNormConfig::new(enc).init(device),
            inconv: Conv1dConfig::new(enc, d_model, 1).init(device),
            inconv_gamma: Param::from_tensor(Tensor::ones([d_model, t], device)),
            inconv_beta: Param::from_tensor(Tensor::zeros([d_model, t], device)),
            pos: Param::from_tensor(pos).set_require_grad(false),
            query: Param::from_tensor(Tensor::random(
                [h.n_heads, d_k],
                Distribution::Normal(0.0, (2.0 / d_k as f64).sqrt()),
                device,
            )),
            fc_k: LinearConfig::new(d_model, h.n_heads * d_k).init(device),
            attn_drop: DropoutConfig::new(0.1).init(),
            mlp: Mlp::new(d_model, &h.n_neurons[1..], device),
            drop: DropoutConfig::new(h.dropout_rate).init(),
            out_norm: LayerNormConfig::new(last).init(device),
            decoder: Mlp::new(last, &decoder, device),
            n_heads: h.n_heads,
            d_k,
        }
    }

    /// [n, dates, bands] -> the classes' scores [n, classes].
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        let [n, t, b] = x.dims();
        let (heads, d_k) = (self.n_heads, self.d_k);
        // each date's values through the MLP
        let enc = self.spatial.forward(x.reshape([n * t, b]));
        let c = enc.dims()[1];
        let v = self.in_norm.forward(enc.reshape([n, t, c]));
        // 1x1 convolution, then the layer normalization over (d_model, dates)
        let v = self.inconv.forward(v.swap_dims(1, 2)); // [n, d_model, t]
        let d_model = v.dims()[1];
        let mean = v.clone().reshape([n, d_model * t]).mean_dim(1).reshape([n, 1, 1]);
        let centred = v - mean;
        let var = centred.clone().powf_scalar(2.0).reshape([n, d_model * t]).mean_dim(1).reshape([n, 1, 1]);
        let v = centred / (var + 1e-5).sqrt();
        let v = v * self.inconv_gamma.val().unsqueeze_dim::<3>(0) + self.inconv_beta.val().unsqueeze_dim::<3>(0);
        let v = v.swap_dims(1, 2) + self.pos.val().unsqueeze_dim::<3>(0); // [n, t, d_model]
        // keys [heads, n, t, d_k]; the master query [heads, n, 1, d_k]
        let keys = self.fc_k.forward(v.clone()).reshape([n, t, heads, d_k]).permute([2, 0, 1, 3]);
        let query = self.query.val().reshape([heads, 1, 1, d_k]).expand([heads, n, 1, d_k]);
        let attn = query.matmul(keys.swap_dims(2, 3)) / (d_k as f64).sqrt(); // [heads, n, 1, t]
        let attn = self.attn_drop.forward(softmax(attn, 3));
        // the values: the input split among the heads [heads, n, t, d_model / heads]
        let split = d_model / heads;
        let values = v.reshape([n, t, heads, split]).permute([2, 0, 1, 3]);
        let out = attn.matmul(values).reshape([heads, n, split]).swap_dims(0, 1).reshape([n, heads * split]);
        let out = self.out_norm.forward(self.drop.forward(self.mlp.forward(out)));
        self.decoder.forward(out)
    }
}
