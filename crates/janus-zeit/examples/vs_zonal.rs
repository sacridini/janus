//! Janus's zonal statistics (IDEIAS phase 45) timed for tools/bench_zonal_vs_rasterstats.py:
//!
//!     vs_zonal VECTOR values|classes THREADS OUT.csv MAP.tif...
//!
//! each feature of VECTOR in each map (on the first map's grid); the long CSV written to OUT.csv (not timed);
//! prints {"seconds", "zones", "shared"}.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::areas::AreaSource;
use janus_zeit::warp::Grid;
use janus_zeit::zonal::{ZonalKind, ZonalOptions, zonal};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 6 {
        eprintln!("vs_zonal VECTOR values|classes THREADS OUT.csv MAP.tif...");
        std::process::exit(2);
    }
    let g = gdal::Dataset::open(&a[5]).expect("the first map");
    let (w, h) = g.raster_size();
    let grid = Grid { wkt: g.projection(), gt: g.geo_transform().expect("a geotransform"), w, h };
    let sources: Vec<AreaSource> = a[5..]
        .iter()
        .map(|p| AreaSource {
            path: p.clone(),
            band: 1,
            label: std::path::Path::new(p).file_stem().map_or(String::new(), |s| s.to_string_lossy().into_owned()),
        })
        .collect();
    let kind = if a[2] == "classes" { ZonalKind::Classes } else { ZonalKind::Continuous };
    let opts = ZonalOptions { kind, threads: a[3].parse().unwrap_or(8), ..Default::default() };
    let t0 = Instant::now();
    let t = zonal(&a[1], &grid, &sources, &opts, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let seconds = t0.elapsed().as_secs_f64();
    std::fs::write(&a[4], t.long_csv(&|v| v.to_string())).expect("the CSV");
    println!("{}", serde_json::json!({"seconds": seconds, "zones": t.fids.len(), "shared": t.shared}));
}
