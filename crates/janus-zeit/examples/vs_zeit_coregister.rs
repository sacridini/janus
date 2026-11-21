//! The co-registration of dates against zeit.coregister and AROSICS (tools/bench_coreg_vs_zeit.py runs it): the bands
//! `DIR/b1.tif`..`DIR/bN.tif` (a file a band, a band a date, the last one the NIR), the first date the reference, run
//! through the job runner as Janus runs it (the matching band read, the shifts, the series written as warped VRTs or
//! COGs), the best of `REPEAT` runs; prints the JSON of its time and the run's folder.
//!
//!     cargo run --release -p janus-zeit --example vs_zeit_coregister -- DIR BANDS DATES MODE FORMAT THREADS WINDOW GRID [REPEAT]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::job::{JobSpec, run_job};
use serde_json::json;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&a[0]);
    let bands: usize = a[1].parse().unwrap();
    let dates: usize = a[2].parse().unwrap();
    let (mode, format) = (a[3].clone(), a[4].clone());
    let threads: i64 = a[5].parse().unwrap();
    let (window, grid): (i64, i64) = (a[6].parse().unwrap(), a[7].parse().unwrap());
    let repeat: usize = a.get(8).map_or(3, |r| r.parse().unwrap());
    let mut spec = JobSpec { tool: "coregister".into(), threads: Some(threads), ..Default::default() };
    for b in 1..=bands {
        let path = dir.join(format!("b{b}.tif")).to_string_lossy().into_owned();
        spec.bands.insert(format!("b{b:03}"), path.clone());
        spec.band_names.insert(format!("b{b:03}"), format!("b{b}"));
        if b == bands {
            spec.bands.insert("nir".into(), path.clone());
            spec.input = path;
        }
    }
    let days: Vec<i64> = (0..dates as i64).map(|k| 19000 + 16 * k).collect();
    spec.years = days.iter().map(|&d| janus_zeit::numeric::decimal_year(d)).collect();
    spec.days = Some(days);
    spec.params = [
        ("mode".to_string(), json!(mode)),
        ("ref_date".to_string(), json!(1)),
        ("window_size".to_string(), json!(window)),
        ("grid_res".to_string(), json!(grid)),
        ("format".to_string(), json!(format)),
    ]
    .into();
    let mut best = f64::INFINITY;
    let mut folder = String::new();
    for k in 0..repeat {
        let out = dir.join(format!("janus_out_{mode}_{format}_{k}"));
        let _ = std::fs::remove_dir_all(&out);
        spec.output_dir = out.to_string_lossy().into_owned();
        let t = Instant::now();
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).expect("the run");
        let s = t.elapsed().as_secs_f64();
        if s < best {
            (best, folder) = (s, spec.output_dir.clone());
        }
    }
    println!("{}", json!({ "seconds": best, "folder": folder, "threads": threads }));
}
