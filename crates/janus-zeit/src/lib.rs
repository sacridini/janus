//! Zeit's tools in Janus's process (RUST_MIGRATION.md, "Zeit nativo"): what the Python bridge (zeit_bridge/)
//! does around Zeit's API, on zeit-core directly. Each tool has its descriptor (the manifest the bridge sent,
//! tools.json) and runs on a pixel's series; tests/bridge.rs compares every run with the bridge's answer on
//! Zeit 2.0 (tools/zeit_fixtures.py).
//!
//! The algorithms are zeit-core's. What Zeit keeps in Python and Janus needs (LandTrendr's events, ...) is in
//! `upstream`, written to move into zeit-core as it is.

pub mod bands;
mod bfast;
mod ccdc;
mod coded;
mod landtrendr;
pub mod manifest;
mod mann_kendall;
mod ndfi;
pub mod numeric;
mod phenology;
pub mod pixel;
mod smooth;
mod tmask;
mod twdtw;
pub mod upstream;

pub use manifest::{Manifest, Output, Param, Tool, manifest};
pub use pixel::{Overlay, Params, PixelInput, PixelResult, run_pixel};
