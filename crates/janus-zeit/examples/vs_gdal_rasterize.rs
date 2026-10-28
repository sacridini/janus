//! Janus's rasterize (IDEIAS phase 44) timed for tools/bench_rasterize_vs_gdal.py:
//!
//!     vs_gdal_rasterize VECTOR FIELD GRID.tif OUT.tif ALL_TOUCHED(0|1)
//!
//! burns FIELD (or 1 when FIELD is "-") on GRID.tif's grid; prints {"seconds", "burned"}.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::rasterize::{Burn, RasterizeOptions, rasterize};
use janus_zeit::warp::Grid;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("vs_gdal_rasterize VECTOR FIELD GRID.tif OUT.tif ALL_TOUCHED");
        std::process::exit(2);
    }
    let g = gdal::Dataset::open(&a[3]).expect("the grid");
    let (w, h) = g.raster_size();
    let grid = Grid { wkt: g.projection(), gt: g.geo_transform().expect("a geotransform"), w, h };
    let burn = if a[2] == "-" { Burn::One } else { Burn::Attribute(a[2].clone()) };
    let opts = RasterizeOptions { layer: String::new(), burn, all_touched: a[5] == "1", grid };
    let t0 = Instant::now();
    let read = std::cell::Cell::new(0.0);
    let progress = |f: f64, m: &str| {
        if f == 0.3 && m.starts_with("burning ") {
            read.set(t0.elapsed().as_secs_f64());
        }
    };
    let r =
        rasterize(&a[1], &opts, std::path::Path::new(&a[4]), &progress, &AtomicBool::new(false)).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
    println!(
        "{}",
        serde_json::json!({"seconds": t0.elapsed().as_secs_f64(), "read_s": read.get(), "burned": r.burned})
    );
}
