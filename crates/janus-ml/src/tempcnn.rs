//! TempCNN (Pelletier et al. 2019), as sits_tempcnn builds it: three 1D convolutions over time (each with batch
//! normalization, ReLU and dropout), a dense layer (batch normalization, ReLU, dropout) and the classes' scores.
//! A series of fixed length, its values per date as channels.

use burn::nn::conv::{Conv1d, Conv1dConfig};
use burn::nn::{BatchNorm, BatchNormConfig, Dropout, DropoutConfig, Linear, LinearConfig, PaddingConfig1d, Relu};
use burn::prelude::*;
use serde::{Deserialize, Serialize};

/// sits_tempcnn's hyperparameters (its defaults).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct TempCnnHyper {
    pub cnn_layers: Vec<usize>,
    pub cnn_kernels: Vec<usize>,
    pub cnn_dropout_rates: Vec<f64>,
    pub dense_layer_nodes: usize,
    pub dense_layer_dropout_rate: f64,
}

impl Default for TempCnnHyper {
    fn default() -> Self {
        TempCnnHyper {
            cnn_layers: vec![64, 64, 64],
            cnn_kernels: vec![3, 3, 3],
            cnn_dropout_rates: vec![0.2, 0.2, 0.2],
            dense_layer_nodes: 256,
            dense_layer_dropout_rate: 0.5,
        }
    }
}

#[derive(Module, Debug)]
pub struct ConvBlock {
    conv: Conv1d,
    bn: BatchNorm,
    drop: Dropout,
}

#[derive(Module, Debug)]
pub struct TempCnn {
    blocks: Vec<ConvBlock>,
    dense: Linear,
    dense_bn: BatchNorm,
    dense_drop: Dropout,
    out: Linear,
}

impl TempCnn {
    pub fn new(h: &TempCnnHyper, bands: usize, times: usize, classes: usize, device: &Device) -> TempCnn {
        let mut blocks = vec![];
        let mut c_in = bands;
        for (k, &c) in h.cnn_layers.iter().enumerate() {
            let kernel = h.cnn_kernels.get(k).copied().unwrap_or(3);
            blocks.push(ConvBlock {
                conv: Conv1dConfig::new(c_in, c, kernel)
                    .with_padding(PaddingConfig1d::Explicit(kernel / 2, kernel / 2))
                    .init(device),
                bn: BatchNormConfig::new(c).init(device),
                drop: DropoutConfig::new(h.cnn_dropout_rates.get(k).copied().unwrap_or(0.2)).init(),
            });
            c_in = c;
        }
        TempCnn {
            blocks,
            dense: LinearConfig::new(c_in * times, h.dense_layer_nodes).init(device),
            dense_bn: BatchNormConfig::new(h.dense_layer_nodes).init(device),
            dense_drop: DropoutConfig::new(h.dense_layer_dropout_rate).init(),
            out: LinearConfig::new(h.dense_layer_nodes, classes).init(device),
        }
    }

    /// [n, times, bands] -> the classes' scores [n, classes] (before the softmax).
    pub fn forward(&self, x: Tensor<3>) -> Tensor<2> {
        let mut x = x.swap_dims(1, 2);
        for b in &self.blocks {
            x = b.drop.forward(Relu::new().forward(b.bn.forward(b.conv.forward(x))));
        }
        let [n, c, t] = x.dims();
        let x = self.dense.forward(x.reshape([n, c * t]));
        let x = self.dense_drop.forward(Relu::new().forward(self.dense_bn.forward(x)));
        self.out.forward(x)
    }
}
