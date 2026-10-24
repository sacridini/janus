//! Deep learning on the series in Janus's process (IDEIAS.md, "IA", phases 34-38), on Burn: no Python, the GPU
//! through the same wgpu as the map (or the CPU). Samples read from the cube at full resolution and scaled as sits
//! scales them (`data`), sits' networks (`tempcnn`, `ltae`), their training (`train`), a model in a file with what
//! applying it needs (`model`), and its application to a window of a series, as a Zeit run (`apply`).

pub mod apply;
pub mod data;
pub mod dense;
pub mod fetch;
pub mod ltae;
pub mod model;
pub mod post;
pub mod presto;
pub mod tempcnn;
pub mod train;
pub mod utae;

pub use data::{Calendar, InputBand, Norm, Series};
pub use model::{Arch, Class, Header, Model, Net};
pub use train::{Epoch, TrainConfig, Trained};
