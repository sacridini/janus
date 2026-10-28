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
mod coded;
pub mod ewmacd;
pub mod hotspots;
pub mod job;
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
mod smooth;
pub mod snic;
mod som;
mod tmask;
pub mod trend_test;
mod twdtw;
mod unmix;
pub mod upstream;
pub mod warp;
pub mod zonal;

/// The Zeit whose zeit-core is linked (Cargo.toml's revision: 2.0.0 with remotePARTS's methods).
pub const ZEIT_VERSION: &str = "2.0.0";

/// zeit-core's git revision (Cargo.toml's `rev`), as a run's provenance records it.
pub const ZEIT_CORE_REV: &str = "8cfbdbf5ecb2ab740332584749d26c18b724b24a";

pub use manifest::{Manifest, Output, Param, Tool, manifest};
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
