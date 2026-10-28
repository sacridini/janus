//! Janus's polygonize (IDEIAS phase 41) timed for tools/bench_polygonize_vs_gdal.py:
//!
//!     vs_gdal_polygonize MAP.tif OUT.gpkg CONNECTIVITY(4|8) MIN_AREA_HA [MEAN_OF.tif]
//!
//! prints {"seconds", "polygons", "area_ha", "dropped"} as JSON.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::polygonize::{PolyOptions, polygonize};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("vs_gdal_polygonize MAP.tif OUT.gpkg CONNECTIVITY MIN_AREA_HA [MEAN_OF.tif]");
        std::process::exit(2);
    }
    let opts = PolyOptions {
        band: 1,
        eight: a[3] == "8",
        min_area_ha: a[4].parse().unwrap_or(0.0),
        simplify_m: 0.0,
        classes: vec![],
        years: false,
        mean_of: a.get(5).map(|p| ("other".to_string(), p.clone(), 1)),
        layer: "polygons".into(),
    };
    let t0 = Instant::now();
    let r = polygonize(&a[1], &opts, std::path::Path::new(&a[2]), &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(
        |e| {
            eprintln!("{e}");
            std::process::exit(1)
        },
    );
    println!(
        "{{\"seconds\": {}, \"polygons\": {}, \"area_ha\": {}, \"dropped\": {}}}",
        t0.elapsed().as_secs_f64(),
        r.polygons,
        r.area_ha,
        r.dropped
    );
}
