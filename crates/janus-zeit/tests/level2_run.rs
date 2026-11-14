//! The run of Zeit's Level 2 correction in Rust (`janus_zeit::level2_run::level2`) against Python's (`zeit.level2`) on the real scene
//! of tile 23LKC (Sentinel-2B, 2023-07-28, baseline 05.09) with the Copernicus GLO-30 DEM and the defaults (BRDF, topography, Fmask),
//! for no merge, ImproPhe and the regression: every product (`boa` of 10 bands, `qai`, `dst`, `vzn`, `hot`, `aod`, `wvp`) equal in
//! every pixel, the scalar attributes equal, and the GeoTIFFs of `Level2Output::save` equal to those of Zeit's `ds.zeit.save` (values,
//! type, no data, CRS, transform, band names, tiling, compression). The data are outside the repositories, so the tests are ignored
//! and run by hand, one option per process (the run holds 20 to 30 GiB):
//!
//! ```text
//! python tools/level2_run_fixtures.py <data>/<scene>.SAFE <data>/cop_glo30_23LKC.tif <data>/janus_ref/<scene>.SAFE --merge improphe
//! cargo test -j 8 --release -p janus-zeit --test level2_run improphe -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `<data>` is `JANUS_L2_DATA` (default `C:\Users\eduar\Documents\dev\data_force_l2_real`); `JANUS_L2_JOBS` caps the threads (8). The
//! Rust side reads the TIFF copies of the bands in `<data>/tif/<scene>.SAFE` and writes to `<data>/janus_out/<scene>/level2_<merge>`.

use std::path::{Path, PathBuf};
use std::time::Instant;

use gdal::{Dataset, Metadata};
use janus_zeit::level2::Safe;
use janus_zeit::level2_run::{self, Attr, Level2Options, Level2Output, Merge};

const SCENE: &str = "S2B_MSIL1C_20230728T132239_N0509_R038_T23LKC_20230728T150149.SAFE";

fn data() -> PathBuf {
    PathBuf::from(std::env::var("JANUS_L2_DATA").unwrap_or_else(|_| r"C:\Users\eduar\Documents\dev\data_force_l2_real".into()))
}

fn jobs() -> usize {
    std::env::var("JANUS_L2_JOBS").ok().and_then(|v| v.parse().ok()).unwrap_or(8).clamp(1, 8)
}

/// The peak working set and the peak commit of this process in GiB.
#[cfg(windows)]
fn peak_gib() -> (f64, f64) {
    #[repr(C)]
    struct Counters {
        cb: u32,
        page_faults: u32,
        peak_working_set: usize,
        working_set: usize,
        quota_peak_paged: usize,
        quota_paged: usize,
        quota_peak_nonpaged: usize,
        quota_nonpaged: usize,
        pagefile: usize,
        peak_pagefile: usize,
    }
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
    }
    let mut c = Counters {
        cb: std::mem::size_of::<Counters>() as u32,
        page_faults: 0,
        peak_working_set: 0,
        working_set: 0,
        quota_peak_paged: 0,
        quota_paged: 0,
        quota_peak_nonpaged: 0,
        quota_nonpaged: 0,
        pagefile: 0,
        peak_pagefile: 0,
    };
    // SAFETY: the pseudo handle of this process and a zeroed struct of the size given.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb) };
    assert!(ok != 0, "GetProcessMemoryInfo");
    let g = 1024.0 * 1024.0 * 1024.0;
    (c.peak_working_set as f64 / g, c.peak_pagefile as f64 / g)
}

#[cfg(not(windows))]
fn peak_gib() -> (f64, f64) {
    (f64::NAN, f64::NAN)
}

/// The `i16` of every band of a GeoTIFF, band after band.
fn read_tif(path: &Path) -> Vec<i16> {
    let ds = Dataset::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h) = ds.raster_size();
    let mut v = vec![0i16; ds.raster_count() * w * h];
    for b in 0..ds.raster_count() {
        ds.rasterband(b + 1)
            .unwrap()
            .read_into_slice::<i16>((0, 0), (w, h), (w, h), &mut v[b * w * h..(b + 1) * w * h], None)
            .unwrap();
    }
    v
}

/// One row of the table: the product, its values, how many differ and the largest difference.
struct Row {
    name: String,
    values: usize,
    differ: usize,
    max_diff: i64,
}

fn compare(name: &str, got: &[i16], want: &[i16]) -> Row {
    assert_eq!(got.len(), want.len(), "{name}: {} values, Python has {}", got.len(), want.len());
    let (mut differ, mut max_diff) = (0usize, 0i64);
    for (&g, &w) in got.iter().zip(want) {
        if g != w {
            differ += 1;
            max_diff = max_diff.max((i64::from(g) - i64::from(w)).abs());
        }
    }
    Row { name: name.into(), values: got.len(), differ, max_diff }
}

fn print_table(title: &str, rows: &[Row]) {
    println!("\n{title}");
    println!("{:<18}{:>14}{:>12}{:>10}", "product", "values", "differ", "max");
    for r in rows {
        println!("{:<18}{:>14}{:>12}{:>10}", r.name, r.values, r.differ, r.max_diff);
    }
}

fn attr_eq(a: &Attr, v: &serde_json::Value) -> bool {
    match (a, v) {
        (Attr::Bool(a), serde_json::Value::Bool(b)) => a == b,
        (Attr::Int(a), serde_json::Value::Number(n)) => n.as_i64() == Some(*a),
        (Attr::Float(a), serde_json::Value::Number(n)) => n.as_f64().is_some_and(|b| a.to_bits() == b.to_bits()),
        (Attr::Float(a), serde_json::Value::String(s)) => a.is_nan() && s == "NaN",
        (Attr::Str(a), serde_json::Value::String(b)) => a == b,
        (Attr::List(a), serde_json::Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| attr_eq(x, y)),
        _ => false,
    }
}

/// The attributes against Python's `attrs.json`: the same keys and equal values (`core_seconds` is a time).
fn compare_attrs(out: &Level2Output, refs: &Path) -> Vec<String> {
    let want: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(refs.join("attrs.json")).unwrap()).unwrap();
    let want = want.as_object().unwrap();
    let got = out.attrs.pairs();
    let mut bad = vec![];
    // (the order is not compared: Python's attrs.json lists them alphabetically)
    let (mut gk, mut wk): (Vec<&str>, Vec<&str>) = (got.iter().map(|(k, _)| k.as_str()).collect(), want.keys().map(String::as_str).collect());
    gk.sort_unstable();
    wk.sort_unstable();
    if gk != wk {
        bad.push(format!("keys differ: Rust {gk:?}, Python {wk:?}"));
    }
    for (k, a) in &got {
        let Some(v) = want.get(k) else { continue };
        if k == "core_seconds" {
            println!("  core_seconds: Rust {a:?}, Python {v}");
        } else if !attr_eq(a, v) {
            bad.push(format!("{k}: Rust {a:?}, Python {v}"));
        } else {
            println!("  {k}: {a:?}");
        }
    }
    bad
}

/// The description of a GeoTIFF as GDAL reads it, for the comparison of two files.
fn describe_tif(path: &Path) -> Vec<(String, String)> {
    let ds = Dataset::open(path).unwrap();
    let mut d = vec![];
    let mut put = |k: &str, v: String| d.push((k.to_string(), v));
    put("size", format!("{:?}", ds.raster_size()));
    put("bands", ds.raster_count().to_string());
    put("geotransform", format!("{:?}", ds.geo_transform().unwrap()));
    let srs = ds.spatial_ref().unwrap();
    put("crs wkt", srs.to_wkt().unwrap());
    put("crs auth", format!("{:?}:{:?}", srs.auth_name(), srs.auth_code().ok()));
    put("metadata", format!("{:?}", ds.metadata_domain("")));
    put("image structure", format!("{:?}", ds.metadata_domain("IMAGE_STRUCTURE")));
    for b in 1..=ds.raster_count() {
        let band = ds.rasterband(b).unwrap();
        put(&format!("band {b} type"), format!("{:?}", band.band_type()));
        put(&format!("band {b} nodata"), format!("{:?}", band.no_data_value()));
        put(&format!("band {b} description"), format!("{:?}", band.description().unwrap_or_default()));
        put(&format!("band {b} block"), format!("{:?}", band.block_size()));
        put(&format!("band {b} scale/offset"), format!("{:?} {:?}", band.scale(), band.offset()));
        put(&format!("band {b} unit"), format!("{:?}", band.unit()));
        put(&format!("band {b} metadata"), format!("{:?}", band.metadata_domain("")));
        put(&format!("band {b} colour"), format!("{:?}", band.color_interpretation()));
    }
    d
}

fn run_merge(merge: Option<Merge>) {
    let label = merge.map_or("none", Merge::name);
    let root = data();
    let (tif, refs) = (root.join("tif").join(SCENE), root.join("janus_ref").join(SCENE).join(format!("level2_{label}")));
    let dem = root.join("cop_glo30_23LKC.tif");
    let safe = Safe::open(&tif).unwrap();
    let opts = Level2Options { dem: Some(dem), n_jobs: jobs(), merge, ..Level2Options::default() };

    let t0 = Instant::now();
    let out = level2_run::level2(&safe, &opts).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    let (peak_ws, peak_commit) = peak_gib();
    println!(
        "\nRust level2 (merge {label}, {} threads): {secs:.1} s in all, {:.1} s in the core; peak working set {peak_ws:.1} GiB, peak commit {peak_commit:.1} GiB",
        opts.n_jobs, out.attrs.core_seconds
    );

    let mut rows = vec![compare("boa (10 bands)", &out.boa, &read_tif(&refs.join("boa.tif")))];
    for (k, p) in out.planes() {
        rows.push(compare(k, p, &read_tif(&refs.join(format!("{k}.tif")))));
    }
    print_table(&format!("{SCENE}, merge {label}: Rust against Python's saved products"), &rows);
    println!("attributes:");
    let bad_attrs = compare_attrs(&out, &refs);

    // the files of the save against Zeit's
    let dst = root.join("janus_out").join(SCENE).join(format!("level2_{label}"));
    let t0 = Instant::now();
    let files = out.save(&dst).unwrap();
    println!("save: {:.1} s", t0.elapsed().as_secs_f64());
    let names: Vec<String> = files.iter().map(|f| f.file_name().unwrap().to_string_lossy().into_owned()).collect();
    drop(out);
    let mut frows = vec![];
    let mut meta_bad = vec![];
    for name in &names {
        let (a, b) = (dst.join(name), refs.join(name));
        let (da, db) = (describe_tif(&a), describe_tif(&b));
        for ((k, va), (_, vb)) in da.iter().zip(&db) {
            if va != vb {
                meta_bad.push(format!("{name}: {k}: Rust {va}, Python {vb}"));
            }
        }
        if da.len() != db.len() {
            meta_bad.push(format!("{name}: {} properties in Rust's file, {} in Python's", da.len(), db.len()));
        }
        frows.push(compare(name, &read_tif(&a), &read_tif(&b)));
    }
    print_table("the written GeoTIFFs: Rust's against Zeit's save", &frows);
    for f in std::fs::read_dir(&refs).unwrap().flatten() {
        let n = f.file_name().to_string_lossy().into_owned();
        if n.ends_with(".tif") && !names.contains(&n) {
            meta_bad.push(format!("{n}: written by Zeit's save, not by Rust"));
        }
    }
    println!("file properties that differ: {}", if meta_bad.is_empty() { "none".to_string() } else { format!("\n  {}", meta_bad.join("\n  ")) });

    let bad: Vec<&str> = rows.iter().chain(&frows).filter(|r| r.differ > 0).map(|r| r.name.as_str()).collect();
    assert!(bad.is_empty() && bad_attrs.is_empty() && meta_bad.is_empty(), "differs from Python: products {bad:?}, attributes {bad_attrs:?}, files {meta_bad:?}");
}

#[test]
#[ignore = "needs the real scene and Python's products (see the module's doc)"]
fn level2_without_merge() {
    run_merge(None);
}

#[test]
#[ignore = "needs the real scene and Python's products (see the module's doc)"]
fn level2_improphe() {
    run_merge(Some(Merge::Improphe));
}

#[test]
#[ignore = "needs the real scene and Python's products (see the module's doc)"]
fn level2_regression() {
    run_merge(Some(Merge::Regression));
}
