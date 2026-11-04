//! A file whose blocks hold every band (pixel-interleaved, a band a date) read block by block, its blocks
//! decompressed by Janus: the overview, windows of every date and the full-resolution cache (built block by block)
//! as one read of each date through GDAL gives them (GDAL's nearest neighbour), bit for bit; in tiles of Float64 and
//! in strips of Int16 (the last strip shorter, the cache's bands of 64 rows across strips); of several bands a date
//! (a normalized difference masked by a quality band, each read from the block).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use gdal::DriverManager;
use gdal::raster::{Buffer, GdalType, RasterCreationOptions};
use janus_cache::blocks::BlockSampling;
use janus_cache::{FullResCache, JobPool, Overview, OverviewOptions};
use janus_cube::{BandSelection, CubeInfo, CubeReader, QaRule, interleaved_blocks, open_cube};

const W: usize = 300;
const H: usize = 250;
const BANDS: usize = 20;

fn stack<T: GdalType + Copy>(dir: &Path, opts: &[&str], value: impl Fn(f64) -> T) -> Arc<CubeInfo> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let f = dir.join("stack.tif");
    let opts = RasterCreationOptions::from_iter(opts.iter().copied().chain(["COMPRESS=DEFLATE", "INTERLEAVE=PIXEL"]));
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = drv.create_with_band_type_with_options::<T, _>(&f, W, H, BANDS, &opts).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 8900000.0, 0.0, -30.0]).unwrap();
    for b in 1..=BANDS {
        let v: Vec<T> = (0..W * H)
            .map(|i| value(if i % 997 == b { 0.0 } else { ((i * 7919 + b * 104729) % 10007) as f64 * 0.37 - b as f64 }))
            .collect();
        let mut rb = ds.rasterband(b).unwrap();
        rb.set_no_data_value(Some(0.0)).unwrap();
        rb.write((0, 0), (W, H), &mut Buffer::new((W, H), v)).unwrap();
    }
    drop(ds);
    Arc::new(open_cube(&[f.to_string_lossy().to_string()], &BandSelection::default()).unwrap())
}

fn check(d: &Path, info: Arc<CubeInfo>, block: (usize, usize)) {
    let dates = info.t();
    let (b, _) = interleaved_blocks(&info).expect("seen as pixel-interleaved");
    assert_eq!(b, block);
    let mut r = CubeReader::new(info.clone());
    assert!(r.sample_pixels(&[(0, 0), (1, 0)]), "its blocks decompressed by Janus");

    // the overview, about a third a side
    let opts = OverviewOptions {
        budget_bytes: (W * H / 9 * 4 * dates) as i64,
        max_texture: 16384,
        cache_dir: d.join("cache"),
        cache_budget: 0,
        full_res: None,
    };
    let pool = JobPool::new(3, "test", None);
    let ov = Overview::start(info.clone(), &opts, &pool, || {});
    let start = Instant::now();
    while !ov.complete() {
        assert!(start.elapsed() < Duration::from_secs(60), "the overview never done");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(ov.w < W / 2 && ov.h < H / 2, "a reduced overview: {} x {}", ov.w, ov.h);
    assert_eq!(ov.failed(), 0);
    let mut want = vec![0f32; ov.w * ov.h];
    for t in 0..dates {
        assert!(r.read_window(t, (0, 0, W, H), &mut want, (ov.w, ov.h)));
        let got = ov.layer(t).unwrap();
        let bad = want.iter().zip(got).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
        assert_eq!(bad, 0, "date {t}: {bad} pixels of the overview differ from the file's");
    }
    assert!(ov.layer(0).unwrap().iter().any(|v| v.is_nan()), "no data as NaN");

    // a window across blocks, subsampled (an ROI's), one at full resolution, one at the bottom edge
    for (win, size) in [((37, 21, 200, 190), (61, 55)), ((50, 60, 100, 90), (100, 90)), ((10, 200, 280, 50), (280, 50))]
    {
        let s = BlockSampling::new(&info, win, size, block);
        let all: Vec<usize> = (0..dates).collect();
        let mut got = vec![vec![f32::NAN; size.0 * size.1]; dates];
        let (mut values, stop) = (Vec::new(), AtomicBool::new(false));
        for k in 0..s.windows.len() {
            assert!(s.read(&mut r, k, &all, &mut values, &stop, |j, sc| sc.put(&mut got[j])));
        }
        let mut want = vec![0f32; size.0 * size.1];
        for (t, got) in got.iter().enumerate() {
            assert!(r.read_window(t, win, &mut want, size));
            let bad = want.iter().zip(got).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
            assert_eq!(bad, 0, "window {win:?}, date {t}: {bad} pixels differ");
        }
    }

    // the full-resolution cache, every date built at once block by block; read again from its file
    let cache = d.join("fullres");
    let full = FullResCache::new(info.clone(), cache.clone());
    assert!(full.start(1, 0));
    assert!(full.submit_by_blocks(&pool, 20), "built block by block");
    let start = Instant::now();
    while !full.complete() {
        assert!(start.elapsed() < Duration::from_secs(60), "the cache never done: {}", full.error());
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(full);
    let full = FullResCache::new(info.clone(), cache);
    assert!(full.start(1, 0) && full.complete(), "the dates read back from the file");
    let mut got = vec![0f32; W * H];
    for (win, size) in [((0, 0, W, H), (W, H)), ((37, 21, 200, 190), (61, 55))] {
        let mut want = vec![0f32; size.0 * size.1];
        for t in 0..dates {
            assert!(r.read_window(t, win, &mut want, size));
            assert!(full.read_window(t, win, &mut got[..size.0 * size.1], size));
            let bad = want.iter().zip(&got).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
            assert_eq!(bad, 0, "cache, window {win:?}, date {t}: {bad} pixels differ");
        }
    }
    let (mut v, mut seen) = (vec![0f32; dates], vec![false; dates]);
    assert_eq!(full.read_series(W - 1, H - 1, &mut v, &mut seen), dates);
    for (t, v) in v.iter().enumerate() {
        assert_eq!(v.to_bits(), r.read_pixel(t, W - 1, H - 1).unwrap().to_bits(), "series, date {t}");
    }
    drop((ov, full, pool));
}

#[test]
fn tiles_of_float64() {
    let d = std::env::temp_dir().join("janus_cache_interleaved_tiles");
    let info = stack(&d.join("in"), &["TILED=YES", "BLOCKXSIZE=64", "BLOCKYSIZE=64"], |v| v);
    check(&d, info, (64, 64));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn strips_of_int16() {
    let d = std::env::temp_dir().join("janus_cache_interleaved_strips");
    let info = stack(&d.join("in"), &["BLOCKYSIZE=7"], |v| v as i16);
    check(&d, info, (W, 7));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn dates_of_four_bands() {
    let d = std::env::temp_dir().join("janus_cache_interleaved_dated");
    let info = stack(&d.join("in"), &["TILED=YES", "BLOCKXSIZE=64", "BLOCKYSIZE=64"], |v| v as f32);
    assert_eq!(info.t(), BANDS);
    let dates: String = (0..BANDS / 4)
        .map(|k| {
            format!(
                "20{:02}-06-15
",
                10 + k
            )
        })
        .collect();
    std::fs::write(
        d.join("in").join("stack_dates.csv"),
        format!(
            "Date
{dates}"
        ),
    )
    .unwrap();
    // (band 3 - band 1) / (band 3 + band 1) where band 4 is not 0
    let sel =
        BandSelection { band: 3, nd_band: 1, qa_band: 4, qa_rule: QaRule::NonZero, chosen: true, ..Default::default() };
    let info = Arc::new(open_cube(&[info.layers[0].path.clone()], &sel).unwrap());
    assert_eq!((info.t(), info.bands_per_date, info.layers[2].band), (BANDS / 4, 4, 11));
    check(&d, info, (64, 64));
    let _ = std::fs::remove_dir_all(&d);
}
