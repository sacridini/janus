//! Janus: layers in other CRSs on the map, the web basemap under it and plane geometry (src/reproject.cpp,
//! src/basemap.cpp, src/geom.cpp).

pub mod basemap;
pub mod plane;
pub mod reproject;
pub mod vectors;

pub use basemap::{Basemap, BasemapSource, Placement};
pub use reproject::Reprojection;
