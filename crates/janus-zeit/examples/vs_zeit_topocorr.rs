//! The topographic correction against zeit.topographic_correction (tools/bench_topocorr_vs_zeit.py runs it): one date
//! of the bands `DIR/b1.tif`..`DIR/bN.tif` (a band a file) over the DEM `DIR/dem.tif`, the sun given, SCS+C or C, run through
//! the job runner as Janus runs it (the bands read, the DEM warped onto the grid, the corrected file written), the best
//! of `REPEAT` runs; prints the JSON of its time and the run's folder.
//!
//!     cargo run --release -p janus-zeit --example vs_zeit_topocorr -- DIR BANDS ZENITH AZIMUTH METHOD THREADS [REPEAT]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_zeit::job::{JobSpec, run_job};
use janus_zeit::sun::SunHint;
use serde_json::json;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let dir = PathBuf::from(&a[0]);
    let bands: usize = a[1].parse().unwrap();
    let (zenith, azimuth): (f64, f64) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let method = a[4].clone();
    let threads: i64 = a[5].parse().unwrap();
    let repeat: usize = a.get(6).map_or(3, |r| r.parse().unwrap());
    let mut spec = JobSpec { tool: "topocorr".into(), threads: Some(threads), ..Default::default() };
    for b in 1..=bands {
        let path = dir.join(format!("b{b}.tif")).to_string_lossy().into_owned();
        spec.bands.insert(format!("b{b:03}"), path.clone());
        spec.band_names.insert(format!("b{b:03}"), format!("b{b}"));
        if b == 1 {
            spec.input = path;
        }
    }
    spec.days = Some(vec![19000]);
    spec.years = vec![janus_zeit::numeric::decimal_year(19000)];
    spec.suns = vec![SunHint { stac: Some([zenith, azimuth]), ..Default::default() }];
    spec.params = [
        ("dem_source".to_string(), json!("file")),
        ("dem".to_string(), json!(dir.join("dem.tif").to_string_lossy())),
        ("method".to_string(), json!(method)),
        ("min_slope".to_string(), json!(0.0)),
    ]
    .into();
    let mut best = f64::INFINITY;
    let mut folder = String::new();
    for k in 0..repeat {
        let out = dir.join(format!("janus_out_{k}"));
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
