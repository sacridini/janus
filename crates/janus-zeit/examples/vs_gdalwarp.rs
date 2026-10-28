//! Janus's warp (IDEIAS phase 43) timed for tools/bench_warp_vs_gdalwarp.py:
//!
//!     vs_gdalwarp CASE SRC DST THREADS
//!
//! CASE: lonlat (to EPSG:4326, bilinear, COG), mode90 (90 m by mode, COG), clip3857 (a window to EPSG:3857, cubic,
//! GeoTIFF), vrt (to EPSG:4326, bilinear, virtual). Prints {"seconds", "args"}: gdalwarp's arguments, for the
//! command run on the same files.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::warp::{Format, WarpOptions, warp_files};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("vs_gdalwarp CASE SRC DST_DIR THREADS");
        std::process::exit(2);
    }
    let threads = a[4].parse().unwrap_or(8);
    let base = WarpOptions { threads, ..Default::default() };
    let opts = match a[1].as_str() {
        "lonlat" => WarpOptions { t_srs: Some("EPSG:4326".into()), resampling: "bilinear".into(), ..base },
        "mode90" => WarpOptions { pixel: Some((90.0, 90.0)), resampling: "mode".into(), ..base },
        "clip3857" => WarpOptions {
            t_srs: Some("EPSG:3857".into()),
            extent: Some(([300000.0, 7400000.0, 360000.0, 7460000.0], "EPSG:32723".into())),
            resampling: "cubic".into(),
            format: Format::GTiff,
            ..base
        },
        "vrt" => {
            WarpOptions { t_srs: Some("EPSG:4326".into()), resampling: "bilinear".into(), format: Format::Vrt, ..base }
        }
        c => {
            eprintln!("{c}: no such case");
            std::process::exit(2)
        }
    };
    let t0 = Instant::now();
    let out = warp_files(&[a[2].clone()], std::path::Path::new(&a[3]), &opts, &|_, _| {}, &AtomicBool::new(false))
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
    let seconds = t0.elapsed().as_secs_f64();
    println!("{}", serde_json::json!({"seconds": seconds, "args": opts.args().unwrap(), "output": out[0]}));
}
