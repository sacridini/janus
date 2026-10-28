//! Janus's areas (IDEIAS phase 42) timed for tools/bench_areas_vs_terra.py:
//!
//!     vs_terra_areas OUT.json FLOOR(0|1) THREADS MAP.tif...
//!
//! writes {"seconds", "note", "values", "m2": [[...] per map], "pixels": [[...] per map]}.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::areas::{AreaOptions, AreaSource, areas};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() < 5 {
        eprintln!("vs_terra_areas OUT.json FLOOR THREADS MAP.tif...");
        std::process::exit(2);
    }
    let sources: Vec<AreaSource> =
        a[4..].iter().map(|p| AreaSource { path: p.clone(), band: 1, label: p.clone() }).collect();
    let opts = AreaOptions { window: None, floor: a[2] == "1", threads: a[3].parse().unwrap_or(8) };
    let t0 = Instant::now();
    let t = areas(&sources, &opts, &|_, _| {}, &AtomicBool::new(false)).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let seconds = t0.elapsed().as_secs_f64();
    let j = serde_json::json!({
        "seconds": seconds,
        "note": t.note,
        "values": t.values,
        "m2": t.m2,
        "pixels": t.pixels,
    });
    std::fs::write(&a[1], j.to_string()).expect("the JSON");
    println!("{seconds}");
}
