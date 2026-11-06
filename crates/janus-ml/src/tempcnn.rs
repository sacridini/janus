//! TempCNN (Pelletier, Webb & Petitjean 2019, "Temporal Convolutional Neural Network for the Classification of
//! Satellite Image Time Series", Remote Sensing 11(5):523), written from the paper and from the authors' code
//! (charlotte-pel/temporalCNN, GPL-3), in their reference architecture: three convolutions of 64 filters along time
//! (kernel 5, 'same' padding), each followed by batch normalization, ReLU and dropout (0.5, element by element), a
//! dense layer of 256 units with the same three, and a linear layer of logits. No pooling.
//!
//! Keras' defaults the authors' code relies on are kept: batch normalization with epsilon 1e-3 and a momentum of
//! 0.99 (Burn's `momentum` weighs the new batch, so 0.01), He-normal kernels in the hidden layers, a Glorot-uniform
//! kernel in the output layer, zero biases. The authors' L2 penalty (1e-6 on every kernel) is not in the network
//! nor in the training (`train::TrainConfig::weight_decay` is 0: a decoupled decay of 1e-6 at a rate of 1e-3 would do
//! next to nothing, and it was not measured). Weights trained by other implementations
//! (the R package sits' among them) do not load into this network.
//!
//! A sample is [dates, bands]: a batch is [n, dates, bands], laid out as [n, bands, dates] for the convolutions.

use burn::module::Param;
use burn::nn::conv::{Conv1d, Conv1dConfig};
use burn::nn::{
    BatchNorm, BatchNormConfig, Dropout, DropoutConfig, Initializer, Linear, LinearConfig, PaddingConfig1d,
};
use burn::prelude::*;
use burn::tensor::activation::relu;
use serde::{Deserialize, Serialize};

/// Which generation of the pixel networks a model file holds: files without it (or with a lesser number) were
/// trained with the networks of Janus before this one (TempCNN and L-TAE as sits builds them) and do not fit.
pub const VERSION: u32 = 2;

/// TempCNN's architecture (the authors' reference, "3 convolutions of 64 filters, 1 dense layer of 256 units").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TempCnnHyper {
    /// the networks' generation (`VERSION`); a model of an older one is refused when opened (model.rs)
    #[serde(default)]
    pub version: u32,
    /// the convolutions: how many, their filters and kernel (odd)
    pub layers: usize,
    pub filters: usize,
    pub kernel: usize,
    /// the dense layer's units
    pub dense: usize,
    /// the dropout after every block
    pub dropout: f64,
}

impl Default for TempCnnHyper {
    fn default() -> Self {
        TempCnnHyper { version: VERSION, layers: 3, filters: 64, kernel: 5, dense: 256, dropout: 0.5 }
    }
}

/// Keras' batch normalization: epsilon 1e-3, momentum 0.99.
fn batch_norm(n: usize, device: &Device) -> BatchNorm {
    BatchNormConfig::new(n).with_epsilon(1e-3).with_momentum(0.01).init(device)
}

fn zero_bias(n: usize, device: &Device) -> Option<Param<Tensor<1>>> {
    Some(Param::from_tensor(Tensor::zeros([n], device)))
}

#[derive(Module, Debug)]
pub struct TempCnn {
    convs: Vec<Conv1d>,
    conv_bns: Vec<BatchNorm>,
    dense: Linear,
    dense_bn: BatchNorm,
    out: Linear,
    drop: Dropout,
}

impl TempCnn {
    pub fn new(h: &TempCnnHyper, bands: usize, times: usize, classes: usize, device: &Device) -> TempCnn {
        let he = Initializer::KaimingNormal { gain: 2f64.sqrt(), fan_out_only: false };
        let (mut convs, mut conv_bns) = (vec![], vec![]);
        let mut c = bands;
        for _ in 0..h.layers {
            let mut conv = Conv1dConfig::new(c, h.filters, h.kernel)
                .with_padding(PaddingConfig1d::Same)
                .with_initializer(he.clone())
                .init(device);
            conv.bias = zero_bias(h.filters, device);
            convs.push(conv);
            conv_bns.push(batch_norm(h.filters, device));
            c = h.filters;
        }
        let mut dense = LinearConfig::new(h.filters * times, h.dense).with_initializer(he).init(device);
        dense.bias = zero_bias(h.dense, device);
        let mut out =
            LinearConfig::new(h.dense, classes).with_initializer(Initializer::XavierUniform { gain: 1.0 }).init(device);
        out.bias = zero_bias(classes, device);
        TempCnn {
            convs,
            conv_bns,
            dense,
            dense_bn: batch_norm(h.dense, device),
            out,
            drop: DropoutConfig::new(h.dropout).init(),
        }
    }

    /// [n, dates, bands] -> the classes' logits [n, classes].
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        let mut x = x.swap_dims(1, 2);
        for (conv, bn) in self.convs.iter().zip(&self.conv_bns) {
            x = self.drop.forward(relu(bn.forward(conv.forward(x))));
        }
        let [n, f, t] = x.dims();
        let x = self.drop.forward(relu(self.dense_bn.forward(self.dense.forward(x.reshape([n, f * t])))));
        self.out.forward(x)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn net(bands: usize, times: usize, classes: usize) -> TempCnn {
        TempCnn::new(&TempCnnHyper::default(), bands, times, classes, &Device::flex())
    }

    #[test]
    fn parameters_are_the_papers() {
        // conv 1: 5 * 3 * 64 + 64; convs 2 and 3: 5 * 64 * 64 + 64 each; 3 batch norms of 64 (scale and shift);
        // dense: 64 * 10 * 256 + 256 and its batch norm; output: 256 * 4 + 4. Zeit's torch TempCNN has the same.
        // (Burn's count also holds the batch norms' running mean and variance: 2 * (3 * 64 + 256) values)
        assert_eq!(net(3, 10, 4).num_params() - 2 * (3 * 64 + 256), 208_132);
        assert_eq!(
            net(1, 23, 6).num_params() - 2 * (3 * 64 + 256),
            5 * 64 + 64 + 2 * 20_544 + 3 * 128 + 64 * 23 * 256 + 256 + 512 + 256 * 6 + 6
        );
    }

    #[test]
    fn logits_have_a_class_each_and_prediction_is_deterministic() {
        let m = net(2, 12, 5);
        let data: Vec<f32> = (0..7 * 12 * 2).map(|i| ((i * 37 % 101) as f32) / 50.0 - 1.0).collect();
        let x = Tensor::<3>::from_data(TensorData::new(data, [7, 12, 2]), &Device::flex());
        let (a, b) = (m.forward(x.clone()), m.forward(x));
        assert_eq!(a.dims(), [7, 5]);
        assert_eq!(a.into_data().try_to_vec::<f32>().unwrap(), b.into_data().try_to_vec::<f32>().unwrap());
    }

    #[test]
    fn initialization_is_the_authors() {
        let m = net(3, 10, 4);
        // He normal: the deviation is sqrt(2 / fan_in); the second convolution's fan-in is 5 * 64
        let w = m.convs[1].weight.val();
        let rms = w.powf_scalar(2.0).mean().into_scalar::<f32>().sqrt();
        assert!((rms - (2.0f32 / 320.0).sqrt()).abs() < 0.01, "{rms}");
        // zero biases, Keras' batch normalization
        assert!(m.convs[1].bias.as_ref().unwrap().val().abs().sum().into_scalar::<f32>() == 0.0);
        assert_eq!((m.conv_bns[0].epsilon, m.conv_bns[0].momentum), (1e-3, 0.01));
    }
}
