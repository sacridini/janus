//! The overview and the full-resolution cache built from GeoTIFFs made here, as a series opens: the overview's
//! first date read row by row by every worker of the pool (an SSD), and on an HDD both caches in the same read of
//! each date, the next date read ahead; every value as the files give it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gdal::DriverManager;
use gdal::raster::{Buffer, RasterCreationOptions};
use janus_cache::{FullResCache, JobPool, Overview, OverviewOptions};
use janus_cube::{BandSelection, CubeInfo, CubeReader, open_cube};

const W: usize = 700;
const H: usize = 650;
const DATES: usize = 5;

/// A series of Float32 GeoTIFFs in strips of one row, LZW (the Landsat medoids' layout), a no-data pixel in each.
fn series(dir: &Path) -> Arc<CubeInfo> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let opts = RasterCreationOptions::from_iter(["BLOCKYSIZE=1", "COMPRESS=LZW", "PREDICTOR=3"]);
    let mut files = Vec::new();
    for d in 0..DATES {
        let f = dir.join(format!("s_{}0101.tif", 2000 + d));
        let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
        let mut ds = drv.create_with_band_type_with_options::<f32, _>(&f, W, H, 1, &opts).unwrap();
        ds.set_geo_transform(&[500000.0, 30.0, 0.0, 8900000.0, 0.0, -30.0]).unwrap();
        let v: Vec<f32> = (0..W * H)
            .map(|i| if i == 1000 + d { -9999.0 } else { ((i * 7919 + d * 104729) % 10007) as f32 * 0.37 - d as f32 })
            .collect();
        let mut b = ds.rasterband(1).unwrap();
        b.set_no_data_value(Some(-9999.0)).unwrap();
        b.write((0, 0), (W, H), &mut Buffer::new((W, H), v)).unwrap();
        files.push(f.to_string_lossy().to_string());
    }
    Arc::new(open_cube(&files, &BandSelection::default()).unwrap())
}

fn options(dir: &Path, full_res: Option<Arc<FullResCache>>) -> OverviewOptions {
    // about a third of the source a side, as the 41 Landsat dates in 1 GB
    let budget = (W * H / 9 * 4 * (DATES + 8)) as i64;
    OverviewOptions { budget_bytes: budget, max_texture: 16384, cache_dir: dir.into(), cache_budget: 0, full_res }
}

fn wait(what: &str, done: impl Fn() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(60), "{what} never done");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Every date of the overview as one read of the whole date gives it (GDAL's nearest neighbour), bit for bit.
fn check_overview(info: &Arc<CubeInfo>, ov: &Overview) {
    assert!(ov.w < W / 2 && ov.h < H / 2, "a reduced overview: {} x {}", ov.w, ov.h);
    assert_eq!(ov.failed(), 0);
    let mut r = CubeReader::new(info.clone());
    let mut want = vec![0f32; ov.w * ov.h];
    for t in 0..DATES {
        assert!(r.read_window(t, (0, 0, W, H), &mut want, (ov.w, ov.h)));
        let got = ov.layer(t).unwrap();
        let bad = want.iter().zip(got).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
        assert_eq!(bad, 0, "date {t}: {bad} pixels of the overview differ from the file's");
    }
}

fn dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(name)
}

#[test]
fn overview_first_date_by_every_worker() {
    let d = dir("janus_cache_builds_ssd");
    let info = series(&d.join("in"));
    let pool = JobPool::new(3, "test", None);
    let ov = Overview::start(info.clone(), &options(&d.join("cache"), None), &pool, || {});
    wait("the overview", || ov.complete());
    check_overview(&info, &ov);
    // its cache file, read back by a new session
    drop(ov);
    let ov = Overview::start(info.clone(), &options(&d.join("cache"), None), &pool, || {});
    wait("the overview from its cache", || ov.complete());
    assert!(ov.from_cache());
    check_overview(&info, &ov);
}

#[test]
fn both_caches_in_one_read_the_next_date_ahead() {
    let d = dir("janus_cache_builds_hdd");
    let info = series(&d.join("in"));
    let cache = d.join("cache");
    let full = FullResCache::new(info.clone(), cache.clone());
    // an HDD's: one reader in the pool, the date decoded by 2 threads
    assert!(full.start(2, 0));
    assert!(full.integrates());
    let pool = JobPool::new(1, "test", None);
    let ov = Overview::start(info.clone(), &options(&cache, Some(full.clone())), &pool, || {});
    wait("the overview", || ov.complete());
    wait("the full-resolution cache", || full.complete());
    assert_eq!(full.dates_with_overview(), DATES);
    check_overview(&info, &ov);
    drop((ov, pool, full));

    // the blocks (zstd) read back by a new cache, the same values as the files
    let full = FullResCache::new(info.clone(), cache);
    assert!(full.start(2, 0));
    assert!(full.complete(), "{}", full.error());
    let mut r = CubeReader::new(info.clone());
    let (mut a, mut b) = (vec![0f32; W * H], vec![0f32; W * H]);
    for t in 0..DATES {
        assert!(full.read_window(t, (0, 0, W, H), &mut a, (W, H)));
        assert!(r.read_window(t, (0, 0, W, H), &mut b, (W, H)));
        assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()), "date {t} differs from the file");
    }
    assert!(a.iter().any(|v| v.is_nan()), "no data as NaN");
}
