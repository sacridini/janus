//! Janus: layers in other CRSs on the map and the web basemap under it (src/reproject.cpp, src/basemap.cpp).

pub mod basemap;
pub mod reproject;

pub use basemap::{Basemap, BasemapSource, Placement};
pub use reproject::Reprojection;
