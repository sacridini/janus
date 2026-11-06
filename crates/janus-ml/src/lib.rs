//! Deep learning on the series in Janus's process (IDEIAS.md, "IA", phases 34-38), on Burn: no Python, the GPU
//! through the same wgpu as the map (or the CPU). Samples read from the cube at full resolution and scaled by their
//! quantiles (`data`), the networks (`tempcnn`, `ltae`: the authors' architectures), their training (`train`), a
//! model in a file with what applying it needs (`model`), and its application to a window of a series, as a Zeit
//! run (`apply`).

pub mod aoa;
pub mod apply;
/// k-means and Ward's clustering, in janus-zeit (its k-means tool's too)
pub use janus_zeit::cluster;
pub mod data;
pub mod dense;
pub mod extra;
pub mod fetch;
pub mod fieldnet;
pub mod fields;
pub mod fields_train;
pub mod folds;
pub mod forest;
pub mod ltae;
pub mod model;
pub mod post;
pub mod presto;
pub mod tempcnn;
pub mod train;
pub mod utae;

/// The device networks run on (re-exported for the app's closures).
pub use burn::prelude::Device;
pub use data::{Calendar, InputBand, Norm, Series};
pub use model::{Arch, Class, Header, Model, Net};
pub use train::{Epoch, TrainConfig, Trained};
