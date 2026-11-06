//! Zeit's tools in Janus's process (RUST_MIGRATION.md, "Zeit nativo"): what the Python bridge (zeit_bridge/)
//! does around Zeit's API, on zeit-core directly. Each tool has its descriptor (the manifest the bridge sent,
//! tools.json) and runs on a pixel's series; tests/bridge.rs compares every run with the bridge's answer on
//! Zeit 2.0 (tools/zeit_fixtures.py).
//!
//! The algorithms are zeit-core's. What Zeit keeps in Python and Janus needs (LandTrendr's events, ...) is in
//! `upstream`, written to move into zeit-core as it is.

mod agreement;
mod ar_trend;
pub mod areas;
pub mod bands;
mod beast;
mod bfast;
mod ccdc;
pub mod cluster;
mod coded;
pub mod composite;
pub mod ewmacd;
pub mod harmonize;
pub mod hotspots;
pub mod job;
mod kmeans;
mod landtrendr;
pub mod manifest;
mod mann_kendall;
mod ndfi;
pub mod numeric;
mod phenology;
pub mod pixel;
pub mod polygonize;
pub mod postproc;
pub mod python;
pub mod rasterize;
pub mod recovery;
pub mod regularize;
pub mod rng;
mod sccd;
pub mod series_io;
mod smooth;
pub mod snic;
mod som;
pub mod speckle;
pub mod texture;
mod tmask;
pub mod trend_test;
pub mod twdtw;
mod unmix;
pub mod upstream;
pub mod warp;
pub mod zonal;

/// The Zeit whose zeit-core is linked (Cargo.toml's revision: 3.0.0, GPL-3.0-or-later, the parts from GPL-2-only code rewritten).
pub const ZEIT_VERSION: &str = "3.0.0";

/// zeit-core's git revision (Cargo.toml's `rev`), as a run's provenance records it.
pub const ZEIT_CORE_REV: &str = "7c019786e755f5027c50051cd7b43ee8c2446edb";

pub use manifest::{CATEGORIES, CLASSIFICATION, DATA_PREPARATION, Manifest, Output, Param, Tool, manifest};
pub use pixel::{Overlay, Params, PixelInput, PixelResult, run_pixel};

#[cfg(test)]
mod rev_test {
    /// The revision recorded is the one linked.
    #[test]
    fn zeit_core_rev_is_cargo_toml_s() {
        let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        let line = toml.lines().find(|l| l.starts_with("zeit-core")).unwrap();
        assert!(line.contains(&format!("rev = \"{}\"", super::ZEIT_CORE_REV)), "{line}");
    }
}
