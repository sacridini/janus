//! The inputs of Zeit's Level 2 correction in Rust (`janus_zeit::level2`) against the Python's (`zeit/_level2.py`), on the real
//! scenes of tile 23LKC (Sentinel-2B, 2023-07-28, processing baseline 05.09, and 2021-07-28, baseline 03.01, the legacy
//! radiometry): every array equal in every value. The data are outside the repositories, so the tests are ignored and run by
//! hand:
//!
//! ```text
//! python tools/level2_inputs_fixtures.py <data>/<scene>.SAFE <data>/cop_glo30_23LKC.tif <data>/janus_ref/<scene>.SAFE
//! cargo test -j 8 --release -p janus-zeit --test level2_inputs -- --ignored --nocapture --test-threads=1
//! ```
//!
//! `<data>` is `JANUS_L2_DATA` (default `C:\Users\eduar\Documents\dev\data_force_l2_real`). The Rust side reads the TIFF copies
//! of the bands in `<data>/tif/<scene>.SAFE` (the GDAL of the build may have no JPEG 2000 driver; the values are the same).

use std::path::{Path, PathBuf};
use std::time::Instant;

use janus_zeit::level2::{self, FmaskDem, Safe};

fn data() -> PathBuf {
    PathBuf::from(std::env::var("JANUS_L2_DATA").unwrap_or_else(|_| r"C:\Users\eduar\Documents\dev\data_force_l2_real".into()))
}

// --- .npy --------------------------------------------------------------------------------------------------------

/// A little-endian C-order array of a .npy file: its dtype (`<u2`), shape and bytes.
struct Npy {
    descr: String,
    shape: Vec<usize>,
    bytes: Vec<u8>,
}

fn npy(path: &Path) -> Npy {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    assert_eq!(&raw[..6], b"\x93NUMPY", "{}", path.display());
    let (len, start) = if raw[6] == 1 { (u16::from_le_bytes([raw[8], raw[9]]) as usize, 10) } else { (u32::from_le_bytes(raw[8..12].try_into().unwrap()) as usize, 12) };
    let header = std::str::from_utf8(&raw[start..start + len]).unwrap();
    assert!(header.contains("'fortran_order': False"), "{header}");
    let descr = header.split("'descr': '").nth(1).unwrap().split('\'').next().unwrap().to_string();
    let shape_text = header.split("'shape': (").nth(1).unwrap().split(')').next().unwrap();
    let shape = shape_text.split(',').filter(|s| !s.trim().is_empty()).map(|s| s.trim().parse().unwrap()).collect();
    Npy { descr, shape, bytes: raw[start + len..].to_vec() }
}

fn load<T, const N: usize>(path: &Path, descr: &str, from: fn([u8; N]) -> T) -> (Vec<T>, Vec<usize>) {
    let a = npy(path);
    assert_eq!(a.descr, descr, "{}", path.display());
    (a.bytes.chunks_exact(N).map(|c| from(c.try_into().unwrap())).collect(), a.shape)
}

fn load_u16(p: &Path) -> (Vec<u16>, Vec<usize>) {
    load(p, "<u2", u16::from_le_bytes)
}
fn load_i16(p: &Path) -> (Vec<i16>, Vec<usize>) {
    load(p, "<i2", i16::from_le_bytes)
}
fn load_f32(p: &Path) -> (Vec<f32>, Vec<usize>) {
    load(p, "<f4", f32::from_le_bytes)
}
fn load_f64(p: &Path) -> (Vec<f64>, Vec<usize>) {
    load(p, "<f8", f64::from_le_bytes)
}

// --- the comparison ------------------------------------------------------------------------------------------------

/// One row of the table: the array, its values, how many differ (bit for bit; a NaN is a NaN) and the largest difference.
struct Row {
    name: String,
    values: usize,
    differ: usize,
    max_diff: f64,
}

fn row<T: Copy>(name: &str, got: &[T], want: &[T], diff: impl Fn(T, T) -> Option<f64>) -> Row {
    assert_eq!(got.len(), want.len(), "{name}: {} values, Python has {}", got.len(), want.len());
    let (mut differ, mut max_diff) = (0usize, 0.0f64);
    for (&g, &w) in got.iter().zip(want) {
        if let Some(d) = diff(g, w) {
            differ += 1;
            max_diff = max_diff.max(d);
        }
    }
    Row { name: name.into(), values: got.len(), differ, max_diff }
}

fn ints<T: Copy + Into<i64>>(name: &str, got: &[T], want: &[T]) -> Row {
    row(name, got, want, |g, w| {
        let (g, w): (i64, i64) = (g.into(), w.into());
        (g != w).then(|| (g - w).abs() as f64)
    })
}

fn floats32(name: &str, got: &[f32], want: &[f32]) -> Row {
    row(name, got, want, |g, w| {
        if g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()) {
            None
        } else if g.is_nan() || w.is_nan() {
            Some(f64::INFINITY)
        } else {
            Some((f64::from(g) - f64::from(w)).abs())
        }
    })
}

fn floats64(name: &str, got: &[f64], want: &[f64]) -> Row {
    row(name, got, want, |g, w| {
        if g.to_bits() == w.to_bits() || (g.is_nan() && w.is_nan()) { None } else { Some((g - w).abs()) }
    })
}

fn report(scene: &str, rows: &[Row], times: &[(String, f64)]) {
    println!("\n{scene}");
    println!("{:<14}{:>14}{:>12}{:>14}", "array", "values", "differ", "max |diff|");
    for r in rows {
        println!("{:<14}{:>14}{:>12}{:>14}", r.name, r.values, r.differ, r.max_diff);
    }
    println!("Rust seconds: {}", times.iter().map(|(n, s)| format!("{n} {s:.2}")).collect::<Vec<_>>().join(", "));
    let bad: Vec<&str> = rows.iter().filter(|r| r.differ > 0).map(|r| r.name.as_str()).collect();
    assert!(bad.is_empty(), "{scene}: not equal to Python's: {bad:?}");
}

/// Every input of the correction of a scene, from the TIFF copies, against the arrays Python dumped.
fn scene(name: &str) {
    let root = data();
    let (tif, refs) = (root.join("tif").join(name), root.join("janus_ref").join(name));
    let dem = root.join("cop_glo30_23LKC.tif");
    let jobs = 8;
    let (mut rows, mut times) = (vec![], vec![]);
    let mut clock = |what: &str, t0: Instant| times.push((what.to_string(), t0.elapsed().as_secs_f64()));

    let safe = Safe::open(&tif).unwrap();
    let t0 = Instant::now();
    let desc = safe.describe().unwrap();
    clock("describe", t0);
    let want: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(refs.join("desc.json")).unwrap()).unwrap();
    let (nr, nc) = (desc.nrows, desc.ncols);
    assert_eq!((want["nrows"].as_u64().unwrap() as usize, want["ncols"].as_u64().unwrap() as usize), (nr, nc));
    assert_eq!(want["epsg"].as_u64().map(|e| e as u32), desc.l1.tile.epsg);
    assert_eq!(want["ulx"].as_f64().unwrap(), desc.l1.tile.ulx);
    assert_eq!(want["uly"].as_f64().unwrap(), desc.l1.tile.uly);
    assert_eq!(want["baseline"].as_str().map(str::to_string), desc.l1.product.baseline);
    assert_eq!(want["dn_scale"].as_f64().unwrap(), f64::from(desc.l1.product.dn_scale));
    for (k, v) in want["radd"].as_array().unwrap().iter().enumerate() {
        assert_eq!(v.as_f64().unwrap(), f64::from(desc.l1.product.radd[k]), "radd {k}");
    }
    let g = &want["geo_grid"];
    assert_eq!(
        (g[0].as_f64().unwrap(), g[1].as_f64().unwrap(), g[2].as_f64().unwrap(), g[3].as_u64().unwrap() as usize, g[4].as_u64().unwrap() as usize),
        (desc.geo_grid.ulx, desc.geo_grid.uly, desc.geo_grid.res, desc.geo_grid.nx, desc.geo_grid.ny)
    );
    for (k, w) in want["windows"].as_array().unwrap().iter().enumerate() {
        let win = &w[1];
        let got = desc.windows[k].1;
        assert_eq!(
            [win[0].as_i64().unwrap(), win[1].as_i64().unwrap(), win[2].as_i64().unwrap(), win[3].as_i64().unwrap()],
            [i64::from(got.0), i64::from(got.1), i64::from(got.2), i64::from(got.3)]
        );
    }

    let t0 = Instant::now();
    let dn = level2::read_bands(&safe, &desc, jobs).unwrap();
    clock("read_bands", t0);
    rows.push(ints("dn", &dn, &load_u16(&refs.join("dn.npy")).0));

    let t0 = Instant::now();
    let (lon, lat) = level2::geo_tables(&safe, &desc).unwrap();
    clock("geo_tables", t0);
    rows.push(floats64("geo lon", &lon, &load_f64(&refs.join("geo_lon.npy")).0));
    rows.push(floats64("geo lat", &lat, &load_f64(&refs.join("geo_lat.npy")).0));

    let t0 = Instant::now();
    let (qai, toa) = level2::prepare(&desc, &dn, jobs).unwrap();
    clock("prepare", t0);
    rows.push(ints("qai0", &qai, &load_i16(&refs.join("qai0.npy")).0));
    rows.push(ints("toa", &toa.concat(), &load_i16(&refs.join("toa.npy")).0));
    drop((qai, toa));

    let t0 = Instant::now();
    let d10 = level2::dem_on_grid(&dem, &safe, None, None, jobs).unwrap();
    clock("dem 10 m", t0);
    rows.push(floats32("dem 10 m", &d10, &load_f32(&refs.join("dem10.npy")).0));
    drop(d10);

    let t0 = Instant::now();
    let d20 = level2::dem_on_grid(&dem, &safe, Some(20.0), Some(&desc), jobs).unwrap();
    clock("dem 20 m", t0);
    rows.push(floats32("dem 20 m", &d20, &load_f32(&refs.join("dem20.npy")).0));

    let t0 = Instant::now();
    let (vza, vaa, missing) = level2::view_angles_20m(&safe, &desc).unwrap();
    clock("view angles", t0);
    rows.push(floats32("vza 20 m", &vza, &load_f32(&refs.join("vza20.npy")).0));
    rows.push(floats32("vaa 20 m", &vaa, &load_f32(&refs.join("vaa20.npy")).0));
    println!("view angles missing: {missing}");
    drop((vza, vaa));

    let t0 = Instant::now();
    let (flags, info) = level2::fmask_flags(&safe, &desc, &dn, Some(FmaskDem::Array(&d20)), None, false, jobs).unwrap();
    clock("fmask_flags", t0);
    rows.push(ints("flags", &flags, &load_i16(&refs.join("flags.npy")).0));
    println!("{info:?}");
    let hist = |f: &[i16]| {
        let mut h = std::collections::BTreeMap::new();
        for &v in f {
            *h.entry(v).or_insert(0usize) += 1;
        }
        h
    };
    println!("flag values: {:?}", hist(&flags));

    // the whole path of the flags, from the DEM's file (its warp inside)
    let t0 = Instant::now();
    let (again, _) = level2::fmask_flags(&safe, &desc, &dn, Some(FmaskDem::File(&dem)), None, false, jobs).unwrap();
    clock("fmask_flags (dem file)", t0);
    rows.push(ints("flags (file)", &again, &load_i16(&refs.join("flags.npy")).0));
    report(name, &rows, &times);
}

#[test]
#[ignore = "needs the real scene and Python's arrays (see the module's doc)"]
fn scene_2023_baseline_05() {
    scene("S2B_MSIL1C_20230728T132239_N0509_R038_T23LKC_20230728T150149.SAFE");
}

#[test]
#[ignore = "needs the real scene and Python's arrays (see the module's doc)"]
fn scene_2021_legacy_radiometry() {
    scene("S2B_MSIL1C_20210728T132239_N0301_R038_T23LKC_20210728T151918.SAFE");
}
