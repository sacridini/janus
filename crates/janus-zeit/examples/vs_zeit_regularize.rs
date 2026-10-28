//! Regularization (IDEIAS phase 40) against zeit.regularize_time_series and Zeit's Whittaker
//! (tools/bench_regularize_vs_zeit.py runs it): the series of `DIR/one/*.tif` (one band) and `DIR/multi/*.tif`
//! (three bands) regularized to 16 days by the median, the medoid, the clearest date, and the median with its gaps
//! filled linearly and by a Whittaker curve (lambda 10), on 8 threads, the COGs written; `regularize.json` (ms,
//! dates) and each result read back (`<name>.f32`: dates x bands x pixels). With `DIR/sits` (the series given to
//! sits_regularize), its median too.
//!
//!     cargo run --release -p janus-zeit --example vs_zeit_regularize -- DIR

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::{BandSelection, open_cube};
use janus_zeit::regularize::{Composite, Fill, RegOptions, Step, regularize};

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".tif"))
        .collect();
    v.sort();
    v
}

fn read_all(path: &str) -> Vec<f32> {
    let ds = gdal::Dataset::open(path).unwrap();
    let (w, h) = ds.raster_size();
    (1..=ds.raster_count())
        .flat_map(|b| ds.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec())
        .collect()
}

fn main() {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).expect("DIR"));
    let mut out = serde_json::Map::new();
    for (name, set, composite, fill) in [
        ("median", "one", Composite::Median, Fill::None),
        ("medoid", "multi", Composite::Medoid, Fill::None),
        ("medoid_one", "one", Composite::Medoid, Fill::None),
        ("clearest", "multi", Composite::Clearest, Fill::None),
        ("linear", "one", Composite::Median, Fill::Linear),
        ("whittaker", "one", Composite::Median, Fill::Whittaker(10.0)),
        // the series given to sits (one band of Int16 reflectances), when there
        ("sits_median", "sits", Composite::Median, Fill::None),
    ] {
        if !dir.join(set).exists() {
            continue;
        }
        let info = Arc::new(open_cube(&files(&dir.join(set)), &BandSelection::default()).unwrap());
        let opts = RegOptions {
            step: Step::Days(16),
            composite,
            fill,
            max_gap_days: 0.0,
            all_bands: set == "multi",
            window: None,
            stem: name.into(),
            threads: 8,
        };
        let odir = dir.join("reg").join(name);
        let mut best = f64::INFINITY;
        let mut files = Vec::new();
        for _ in 0..2 {
            let _ = std::fs::remove_dir_all(&odir);
            let t = Instant::now();
            let r = regularize(&info, &opts, &odir, &|_, _| {}, &AtomicBool::new(false)).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            files = r.files;
        }
        let v: Vec<f32> = files.iter().flat_map(|f| read_all(f)).collect();
        std::fs::write(dir.join(format!("{name}.f32")), v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>())
            .unwrap();
        out.insert(name.into(), serde_json::json!({"ms": best, "dates": files.len()}));
        println!("{name}: {best:.0} ms, {} regular dates", files.len());
    }
    std::fs::write(dir.join("regularize.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
}
