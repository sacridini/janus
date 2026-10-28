//! The raster calculator against its originals (tools/bench_calc_vs_originals.py runs it):
//!
//! - `indices DIR N`: every index of the Awesome Spectral Indices a six-band layer (Blue..SWIR2, `bands.f32`: 6 × N
//!   values) can compute, compiled as Janus does and evaluated over the N pixels; writes `indices.json` (name,
//!   formula as Janus computes it, milliseconds, its file) and each one's values (`NNN.f32`), for spyndex to compare.
//! - `reduce DIR`: the series of `DIR/series/*.tif` reduced over time by the calculator (max, mean, median, std,
//!   count, percentile 90): the VRT written and read whole, as a COG translation reads it; `reduce.json` (ms) and
//!   each result (`r_<name>.f32`), for xarray to compare.
//! - `time DIR`: the operators over time (by year, windows, lags, anomalies...) on `DIR/irregular/*.tif`, each
//!   result written (the reductions read by several dates computed into GeoTIFFs) and read whole: `time.json` (ms,
//!   dates) and `t_<name>.f32`.
//! - `shown DIR`: the overview read (the series of `DIR/bands/*.tif`, four bands a date) with NDVI as a normalized
//!   difference and as an expression, and EVI: `shown.json` (ms).
//!
//!     cargo run --release -p janus-cube --example vs_originals_calc -- indices DIR N

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::calc::names::BandNames;
use janus_cube::calc::plan::{CalcLayer, Options, plan};
use janus_cube::calc::program::BLOCK;
use janus_cube::calc::{DateVars, ExprId, LayerExpr, indices};
use janus_cube::{BandSelection, open_cube, with_thread_reader};

fn read_f32(path: &Path) -> Vec<f32> {
    std::fs::read(path).unwrap().as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
}

fn write_f32(path: &Path, v: &[f32]) {
    std::fs::write(path, v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
}

fn best_ms(runs: usize, mut f: impl FnMut()) -> f64 {
    f();
    (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .fold(f64::INFINITY, f64::min)
}

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

fn indices_bench(dir: &Path, n: usize) {
    let bands = read_f32(&dir.join("bands.f32"));
    assert_eq!(bands.len(), 6 * n);
    let nm = [482.0, 561.0, 655.0, 865.0, 1609.0, 2201.0];
    let names = BandNames::new(
        ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2"].iter().map(|s| s.to_string()).collect(),
        &nm,
    );
    let mut out = Vec::new();
    let mut v = vec![0f32; n];
    for ix in indices::all().iter().filter(|i| i.usable(&names)) {
        let f = ix.formula_for(&names, &nm).unwrap();
        let e = LayerExpr::compile(&f, &names).unwrap();
        let ins: Vec<&[f32]> = e.bands.iter().map(|&b| &bands[(b as usize - 1) * n..b as usize * n]).collect();
        let vars = DateVars::default();
        // in blocks, as the reader evaluates a window
        let ms = best_ms(3, || {
            for k in (0..n).step_by(BLOCK * 64) {
                let end = (k + BLOCK * 64).min(n);
                let part: Vec<&[f32]> = ins.iter().map(|s| &s[k..end]).collect();
                e.program.eval(&part, &vars, &mut v[k..end]);
            }
        });
        // numbered: names differ by case only (BAI, BaI), file names may not
        let file = format!("{:03}.f32", out.len());
        write_f32(&dir.join(&file), &v);
        out.push(serde_json::json!({"name": ix.name, "formula": f, "janus_ms": ms, "bands": e.bands, "file": file}));
    }
    std::fs::write(dir.join("indices.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
    println!("{} indices over {n} pixels", out.len());
}

fn read_all(path: &str) -> Vec<f32> {
    janus_cube::calc::vrt::with_block_reads(true, || {
        let ds = gdal::Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        let b = ds.rasterband(1).unwrap();
        b.read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec()
    })
}

fn reduce_bench(dir: &Path) {
    let fs = files(&dir.join("series"));
    let info = open_cube(&fs, &BandSelection::default()).unwrap();
    let layers = vec![CalcLayer { name: "s".into(), alias: "s".into(), info: Arc::new(info) }];
    let opts = Options::default();
    let mut out = serde_json::Map::new();
    for (name, text) in [
        ("max", "max(s)"),
        ("mean", "mean(s)"),
        ("median", "median(s)"),
        ("std", "std(s)"),
        ("count", "count(s > 0.5)"),
        ("p90", "percentile(s, 90)"),
        ("anomaly_last", "s - mean(s)"),
    ] {
        let p = plan(text, &layers, &opts).unwrap();
        let odir = dir.join("calc").join(name);
        let _ = std::fs::remove_dir_all(&odir);
        let w = p.write(text, &layers, &opts, &odir, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        // the last date of a series (the anomaly), the image of a reduction
        let file = w.files.last().unwrap().clone();
        let mut v = Vec::new();
        let ms = best_ms(2, || v = read_all(&file));
        write_f32(&dir.join(format!("r_{name}.f32")), &v);
        out.insert(name.into(), serde_json::json!(ms));
        println!("{name}: {ms:.0} ms");
    }
    std::fs::write(dir.join("reduce.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
}

/// The calculator's operators over time (39b) on an irregular series: each result's dates read whole, one after
/// the other into one file.
fn time_bench(dir: &Path) {
    let fs = files(&dir.join("irregular"));
    let info = open_cube(&fs, &BandSelection::default()).unwrap();
    let layers = vec![CalcLayer { name: "s".into(), alias: "s".into(), info: Arc::new(info) }];
    let opts = Options { materialize: true, ..Default::default() };
    let mut out = serde_json::Map::new();
    for (name, text) in [
        ("by_year_max", "max(s) by year"),
        ("by_month_mean", "mean(s) by month"),
        ("rolling_90d", "mean(s, 90d)"),
        ("argmax", "argmax(s)"),
        ("anomaly_month", "anomaly(s, base=2020..2021, by=month)"),
        ("slice_median", "median(s[2021-06 .. 2021-09])"),
        ("sub_mean", "s - mean(s)"),
    ] {
        let p = plan(text, &layers, &opts).unwrap();
        let odir = dir.join("calc").join(name);
        let (mut v, mut n) = (Vec::new(), 0);
        // written (the reductions read by several dates computed then) and read
        let ms = best_ms(2, || {
            let _ = std::fs::remove_dir_all(&odir);
            let w = p.write(text, &layers, &opts, &odir, &|_, _| {}, &AtomicBool::new(false)).unwrap();
            v = w.files.iter().flat_map(|f| read_all(f)).collect();
            n = w.files.len();
        });
        write_f32(&dir.join(format!("t_{name}.f32")), &v);
        out.insert(name.into(), serde_json::json!({"ms": ms, "dates": n}));
        println!("{name}: {ms:.0} ms, {n} dates");
    }
    std::fs::write(dir.join("time.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
}

fn shown_bench(dir: &Path) {
    let fs = files(&dir.join("bands"));
    let mut out = serde_json::Map::new();
    let nd = BandSelection { band: 4, nd_band: 3, chosen: true, ..Default::default() };
    let ndvi = BandSelection {
        band: 1,
        expr: ExprId::intern("NDVI", "(N - R) / (N + R)"),
        chosen: true,
        ..Default::default()
    };
    let evi = BandSelection {
        band: 1,
        expr: ExprId::intern("EVI", "2.5 * (N - R) / (N + 6 * R - 7.5 * B + 1)"),
        chosen: true,
        ..Default::default()
    };
    for (name, sel) in [
        ("band", BandSelection { band: 4, chosen: true, ..Default::default() }),
        ("nd", nd),
        ("ndvi_expr", ndvi),
        ("evi_expr", evi),
    ] {
        let info = Arc::new(open_cube(&fs, &sel).unwrap());
        let (w, h) = (info.width, info.height);
        // the overview's size (1024 on its long side) at every date, as the map's first view reads it
        let (bw, bh) = if w >= h { (1024, 1024 * h / w) } else { (1024 * w / h, 1024) };
        let mut buf = vec![0f32; bw * bh];
        let ms = best_ms(2, || {
            for t in 0..info.t() {
                with_thread_reader(&info, |r| r.read_window(t, (0, 0, w, h), &mut buf, (bw, bh)));
            }
        });
        out.insert(name.into(), serde_json::json!(ms));
        println!("{name}: {ms:.0} ms for {} dates of {bw} x {bh}", info.t());
    }
    std::fs::write(dir.join("shown.json"), serde_json::to_string_pretty(&out).unwrap()).unwrap();
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dir = Path::new(&a[2]);
    match a[1].as_str() {
        "indices" => indices_bench(dir, a[3].parse().unwrap()),
        "reduce" => reduce_bench(dir),
        "shown" => shown_bench(dir),
        "time" => time_bench(dir),
        x => panic!("unknown {x}"),
    }
}
