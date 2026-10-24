//! Zeit's tools in Janus's process (RUST_MIGRATION.md, "Zeit nativo"): what the Python bridge (zeit_bridge/)
//! does around Zeit's API, on zeit-core directly. Each tool has its descriptor (the manifest the bridge sent,
//! tools.json) and runs on a pixel's series; tests/bridge.rs compares every run with the bridge's answer on
//! Zeit 2.0 (tools/zeit_fixtures.py).
//!
//! The algorithms are zeit-core's. What Zeit keeps in Python and Janus needs (LandTrendr's events, ...) is in
//! `upstream`, written to move into zeit-core as it is.

mod agreement;
mod ar_trend;
pub mod bands;
mod bfast;
mod ccdc;
mod coded;
pub mod ewmacd;
pub mod job;
mod landtrendr;
pub mod manifest;
mod mann_kendall;
mod ndfi;
pub mod numeric;
mod phenology;
pub mod pixel;
pub mod postproc;
pub mod python;
pub mod recovery;
pub mod rng;
mod smooth;
pub mod snic;
mod som;
mod tmask;
pub mod trend_test;
mod twdtw;
mod unmix;
pub mod upstream;

/// The Zeit whose zeit-core is linked (Cargo.toml's revision: 2.0.0 with remotePARTS's methods).
pub const ZEIT_VERSION: &str = "2.0.0";

pub use manifest::{Manifest, Output, Param, Tool, manifest};
pub use pixel::{Overlay, Params, PixelInput, PixelResult, run_pixel};
