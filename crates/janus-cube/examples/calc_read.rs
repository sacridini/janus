//! The calculator's results read as Janus reads them, timed, and their values written for comparison between
//! builds. Each run reads VRTs written anew, GDAL's block cache emptied and the files Janus keeps open closed before
//! it (the files in the system's cache): a result as it is first shown.
//!
//! - `count_full`, `count_half`: `count(nd(s.N, s.R) > 0.5)` over every date, an image read whole at full resolution
//!   (the map's overview of a result of one image) and at half its size;
//! - `pixel`: the series `nd(s.N, s.R)` (a VRT a date) read at one pixel at every date (the chart);
//! - `cog`: `median(nd(s.N, s.R))` written as a cloud-optimized GeoTIFF (Tools > Raster calculator with GeoTIFFs).
//!
//! Writes `calc_read.json` (milliseconds, the best of three runs) and the values read (`*.f32`) into OUT.
//!
//!     cargo run --release -p janus-cube --example calc_read -- SERIES_DIR OUT [THREADS]

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use janus_cube::calc::plan::{CalcLayer, Options, plan};
use janus_cube::{BandSelection, CubeInfo, open_cube, with_thread_reader};

const RUNS: usize = 3;

fn write_f32(path: &Path, v: &[f32]) {
    std::fs::write(path, v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>()).unwrap();
}

fn written(layers: &[CalcLayer], text: &str, dir: &Path) -> Vec<String> {
    let p = plan(text, layers, &Options::default()).unwrap();
    let _ = std::fs::remove_dir_all(dir);
    p.write(text, layers, &Options::default(), dir, &|_, _| {}, &AtomicBool::new(false)).unwrap().files
}

fn opened(files: &[String]) -> Arc<CubeInfo> {
    Arc::new(open_cube(files, &BandSelection { chosen: true, ..Default::default() }).unwrap())
}

/// GDAL's block cache emptied (its limit set to nothing a moment).
fn cold_cache() {
    // SAFETY: GDAL's cache settings, no pointers.
    unsafe {
        let max = gdal_sys::GDALGetCacheMax64();
        gdal_sys::GDALSetCacheMax64(0);
        gdal_sys::GDALSetCacheMax64(max);
    }
}

/// `f` timed over RUNS runs, each on what `setup` makes for it: the best and every time (ms).
fn timed<T>(mut setup: impl FnMut(usize) -> T, mut f: impl FnMut(T)) -> serde_json::Value {
    let mut all = Vec::new();
    for run in 0..RUNS {
        let x = setup(run);
        janus_cube::drop_thread_reader();
        // the files Janus's reads keep open closed too (GDAL's pool of a VRT's sources stays as it is)
        janus_cube::calc::eval::close_files();
        cold_cache();
        let t = Instant::now();
        f(x);
        all.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let best = all.iter().copied().fold(f64::INFINITY, f64::min);
    serde_json::json!({"ms": best, "runs": all})
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (series, out) = (Path::new(&a[1]), Path::new(&a[2]));
    let threads: usize = a.get(3).and_then(|t| t.parse().ok()).unwrap_or(0);
    if threads > 0 {
        janus_cube::calc::eval::set_threads(threads);
    }
    std::fs::create_dir_all(out).unwrap();
    let mut files: Vec<String> = std::fs::read_dir(series)
        .unwrap()
        .flatten()
        .map(|e| e.path().to_string_lossy().into_owned())
        .filter(|p| p.ends_with(".tif"))
        .collect();
    files.sort();
    let info = opened(&files);
    let layers = vec![CalcLayer { name: "s".into(), alias: "s".into(), info }];
    let mut json = serde_json::Map::new();

    // a reduction to one image, read whole as the overview reads it, and at half its size
    for (name, half) in [("count_full", false), ("count_half", true)] {
        let mut buf = Vec::new();
        let r = timed(
            |run| opened(&written(&layers, "count(nd(s.N, s.R) > 0.5)", &out.join(format!("vrt_{name}_{run}")))),
            |c| {
                let (w, h) = (c.width, c.height);
                let (bw, bh) = if half { (w / 2, h / 2) } else { (w, h) };
                buf = vec![0f32; bw * bh];
                with_thread_reader(&c, |r| assert!(r.read_window(0, (0, 0, w, h), &mut buf, (bw, bh))));
            },
        );
        write_f32(&out.join(format!("{name}.f32")), &buf);
        println!("{name}: {r}");
        json.insert(name.into(), r);
    }

    // a pixel's series: a VRT a date
    let pixels = [(400, 300), (10, 20), (876, 885)];
    let mut values = Vec::new();
    let r = timed(
        |run| (opened(&written(&layers, "nd(s.N, s.R)", &out.join(format!("vrt_ndvi_{run}")))), pixels[run]),
        |(c, (x, y))| {
            for t in 0..c.t() {
                values.push(with_thread_reader(&c, |r| r.read_pixel(t, x, y)).unwrap_or(f32::INFINITY));
            }
        },
    );
    write_f32(&out.join("pixel.f32"), &values);
    println!("pixel: {r}");
    json.insert("pixel".into(), r);

    // a reduction written at full resolution as a COG
    let mut tifs = Vec::new();
    let r = timed(
        |run| {
            let d = out.join(format!("cog_{run}"));
            let _ = std::fs::remove_dir_all(&d);
            (written(&layers, "median(nd(s.N, s.R))", &out.join(format!("vrt_median_{run}"))), d)
        },
        |(vrts, d)| tifs = janus_cube::stac_vrt::download(&vrts, &d, &|_, _| {}, &AtomicBool::new(false)).unwrap(),
    );
    let ds = gdal::Dataset::open(&tifs[0]).unwrap();
    let (cw, ch) = ds.raster_size();
    let v = ds.rasterband(1).unwrap().read_as::<f32>((0, 0), (cw, ch), (cw, ch), None).unwrap();
    write_f32(&out.join("cog.f32"), v.data());
    println!("cog: {r}");
    json.insert("cog".into(), r);
    std::fs::write(out.join("calc_read.json"), serde_json::to_string_pretty(&json).unwrap()).unwrap();
}
