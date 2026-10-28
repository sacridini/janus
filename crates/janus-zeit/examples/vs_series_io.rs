//! Janus's build and save of a series (IDEIAS phase 46) timed for tools/bench_series_io.py:
//!
//!     vs_series_io build OUT_DIR STEM vrt|cog THREADS FILE...
//!     vs_series_io save OUT_PATH cog|nc|zarr shown|all THREADS FILE...
//!
//! build: the files planned and built (the plan timed with it); save: the files opened as a series (the band shown
//! the first) and saved. Prints {"seconds", ...}.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::series_io::{BuildFormat, SaveFormat, SaveOptions, build, plan, save_series};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 7 {
        eprintln!(
            "vs_series_io build OUT_DIR STEM vrt|cog THREADS FILE... | save OUT_PATH cog|nc|zarr shown|all THREADS FILE..."
        );
        std::process::exit(2);
    }
    let fail = |e: String| -> ! {
        eprintln!("{e}");
        std::process::exit(1)
    };
    let none = AtomicBool::new(false);
    let threads: usize = a[5].parse().unwrap_or(8);
    let files: Vec<String> = a[6..].to_vec();
    let t0 = Instant::now();
    if a[1] == "build" {
        let p = plan(&files).unwrap_or_else(|e| fail(e));
        let planned = t0.elapsed().as_secs_f64();
        let format = if a[4] == "cog" { BuildFormat::Cog } else { BuildFormat::Vrt };
        let made = build(&p, std::path::Path::new(&a[2]), &a[3], format, threads, &|_, _| {}, &none)
            .unwrap_or_else(|e| fail(e));
        println!(
            "{}",
            serde_json::json!({"seconds": t0.elapsed().as_secs_f64(), "plan_s": planned, "dates": made.len(),
                "bands": p.bands, "skipped": p.skipped.len()})
        );
    } else {
        let info = Arc::new(janus_cube::open_cube(&files, &Default::default()).unwrap_or_else(|e| fail(e)));
        let opened = t0.elapsed().as_secs_f64();
        let format = match a[3].as_str() {
            "nc" => SaveFormat::NetCdf,
            "zarr" => SaveFormat::Zarr,
            _ => SaveFormat::Cog,
        };
        let opts =
            SaveOptions { format, all_bands: a[4] == "all", name: "value".into(), threads, ..Default::default() };
        let t1 = Instant::now();
        let s = save_series(&info, &opts, std::path::Path::new(&a[2]), &|_, _| {}, &none).unwrap_or_else(|e| fail(e));
        println!(
            "{}",
            serde_json::json!({"seconds": t1.elapsed().as_secs_f64(), "open_s": opened, "dates": s.dates,
                "variables": s.variables})
        );
    }
}
