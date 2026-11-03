//! Janus's profiles of every line and its ROI saved as a vector, timed for tools/bench_profiles_vs_rasterio.py:
//!
//!     vs_profiles profiles VECTOR THREADS OUT.csv MAP.tif...
//!
//! every line of VECTOR sampled on the maps' grid (a series of them, a date each) and read at every date
//! (janus_geo::profiles, as Profiles of every line runs it); the long CSV written to OUT.csv; prints {"seconds" (the
//! sampling and the reads), "write_seconds" (the CSV), "lines", "samples", "pixels", "windows"}.
//!
//!     vs_profiles roi MAP.tif OUT_DIR
//!
//! an ROI as the app keeps it (the active layer's pixels: a freehand-like ring of 600 vertices and a hole) written
//! as OUT_DIR/roi.gpkg and OUT_DIR/roi.geojson (janus_geo::polygon_file, as Save ROI as vector writes it), and its
//! pixels (the centres inside, by the even-odd rule) as OUT_DIR/roi_mask.tif; prints {"gpkg_seconds",
//! "geojson_seconds", "pixels"}.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::{BandSelection, open_cube};
use janus_geo::plane;
use janus_geo::polygon_file::{FieldValue, write_polygon};
use janus_geo::profiles::{ProfileOptions, profiles};
use janus_geo::vectors::load_vector_geometry;

fn usage() -> ! {
    eprintln!("vs_profiles profiles VECTOR THREADS OUT.csv MAP.tif...\nvs_profiles roi MAP.tif OUT_DIR");
    std::process::exit(2)
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a.get(1).map(String::as_str) {
        Some("profiles") if a.len() >= 6 => run_profiles(&a),
        Some("roi") if a.len() == 4 => run_roi(&a),
        _ => usage(),
    }
}

fn run_profiles(a: &[String]) {
    let threads: usize = a[3].parse().unwrap_or(3);
    let info = Arc::new(open_cube(&a[5..], &BandSelection { chosen: true, ..Default::default() }).expect("the maps"));
    let t0 = Instant::now();
    let v = load_vector_geometry(&a[2], usize::MAX).expect("the vector").remove(0);
    let opts = ProfileOptions { threads, ..Default::default() };
    let p = profiles(&v, &info, None, &opts, &|_| {}, &AtomicBool::new(false)).expect("the profiles");
    let seconds = t0.elapsed().as_secs_f64();
    let t1 = Instant::now();
    std::fs::write(&a[4], p.long_csv()).expect("the CSV");
    let write = t1.elapsed().as_secs_f64();
    println!(
        "{{\"seconds\": {seconds}, \"write_seconds\": {write}, \"lines\": {}, \"samples\": {}, \"pixels\": {}, \"windows\": {}}}",
        p.lines.len(),
        p.samples(),
        p.pixels,
        p.windows
    );
}

fn run_roi(a: &[String]) {
    let ds = gdal::Dataset::open(&a[2]).expect("the map");
    let (w, h) = ds.raster_size();
    let gt = ds.geo_transform().expect("a geotransform");
    let wkt = ds.spatial_ref().and_then(|s| s.to_wkt()).expect("a CRS");
    // a freehand outline (a vertex every few pixels, wobbling) with a hole, as R + a drag draws it
    let (cx, cy, r) = (w as f64 * 0.47 + 0.31, h as f64 * 0.52 + 0.17, w.min(h) as f64 * 0.35);
    let outline: Vec<f64> = (0..600)
        .flat_map(|k| {
            let t = k as f64 / 600.0 * std::f64::consts::TAU;
            let rr = r * (1.0 + 0.18 * (3.0 * t).sin() + 0.07 * (11.0 * t).cos() + 0.02 * (37.0 * t).sin());
            [cx + rr * t.cos(), cy + 0.8 * rr * t.sin()]
        })
        .collect();
    let hole: Vec<f64> = (0..40)
        .flat_map(|k| {
            let t = -(k as f64) / 40.0 * std::f64::consts::TAU;
            [cx + 0.2 * r + 0.15 * r * t.cos(), cy - 0.1 * r + 0.12 * r * t.sin()]
        })
        .collect();
    let px = vec![outline, hole];
    let mask = plane::mask(&px, 0.0, 0.0, w as f64, h as f64, w, h);
    let pixels = mask.iter().filter(|&&m| m).count();
    // to the map's coordinates, as the app writes it
    let mut geo = px.clone();
    plane::transform(&mut geo, |x, y| Some((gt[0] + x * gt[1] + y * gt[2], gt[3] + x * gt[4] + y * gt[5])));
    let dir = std::path::Path::new(&a[3]);
    std::fs::create_dir_all(dir).expect("the folder");
    let fields = [("name", FieldValue::Text("ROI".into())), ("pixels", FieldValue::Integer(pixels as i64))];
    let mut seconds = [f64::INFINITY; 2];
    for (k, name) in ["roi.gpkg", "roi.geojson"].iter().enumerate() {
        let path = dir.join(name).to_string_lossy().into_owned();
        for _ in 0..5 {
            let t = Instant::now();
            write_polygon(&path, "roi", &wkt, &geo, &fields).expect("the file");
            seconds[k] = seconds[k].min(t.elapsed().as_secs_f64());
        }
    }
    // its pixels, for the comparison
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut m = drv.create_with_band_type::<u8, _>(dir.join("roi_mask.tif"), w, h, 1).expect("the mask");
    m.set_geo_transform(&gt).unwrap();
    m.set_projection(&wkt).unwrap();
    let mut buf = gdal::raster::Buffer::new((w, h), mask.iter().map(|&b| u8::from(b)).collect());
    m.rasterband(1).unwrap().write((0, 0), (w, h), &mut buf).unwrap();
    println!("{{\"gpkg_seconds\": {}, \"geojson_seconds\": {}, \"pixels\": {pixels}}}", seconds[0], seconds[1]);
}
