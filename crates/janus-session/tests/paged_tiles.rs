//! A pixel-interleaved file's detail tiles paged: the first date read at a place reads every date there at once,
//! the other dates come from memory; each tile as one read of its date through GDAL gives it, bit for bit, at full
//! resolution and subsampled.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gdal::DriverManager;
use gdal::raster::{Buffer, RasterCreationOptions};
use janus_cache::JobPool;
use janus_cube::{BandSelection, CubeInfo, CubeReader, open_cube};
use janus_session::tiles::TILE_SIZE;
use janus_session::{TileManager, ViewRect};

const W: usize = 300;
const H: usize = 250;
const BANDS: usize = 20;

fn stack(dir: &Path) -> Arc<CubeInfo> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir).unwrap();
    let f = dir.join("stack.tif");
    let opts = RasterCreationOptions::from_iter([
        "TILED=YES",
        "BLOCKXSIZE=64",
        "BLOCKYSIZE=64",
        "COMPRESS=DEFLATE",
        "INTERLEAVE=PIXEL",
    ]);
    let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
    let mut ds = drv.create_with_band_type_with_options::<f64, _>(&f, W, H, BANDS, &opts).unwrap();
    ds.set_geo_transform(&[500000.0, 30.0, 0.0, 8900000.0, 0.0, -30.0]).unwrap();
    for b in 1..=BANDS {
        let v: Vec<f64> = (0..W * H)
            .map(|i| if i % 997 == b { 0.0 } else { ((i * 7919 + b * 104729) % 10007) as f64 * 0.37 - b as f64 })
            .collect();
        let mut rb = ds.rasterband(b).unwrap();
        rb.set_no_data_value(Some(0.0)).unwrap();
        rb.write((0, 0), (W, H), &mut Buffer::new((W, H), v)).unwrap();
    }
    drop(ds);
    Arc::new(open_cube(&[f.to_string_lossy().to_string()], &BandSelection::default()).unwrap())
}

/// Date t's tiles of the view read (the textures: their values), each checked against GDAL's read of its date.
fn check(tiles: &mut TileManager<Vec<f32>>, pool: &JobPool, r: &mut CubeReader, t: usize, v: &ViewRect) -> usize {
    let start = Instant::now();
    loop {
        tiles.tick();
        tiles.update(pool, t, v, None).expect("zoomed past the overview");
        tiles.upload_ready(64, |d| d.data.clone());
        if tiles.missing(t, v) == 0 {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "date {t}'s tiles never read");
        std::thread::sleep(Duration::from_millis(2));
    }
    let shown = tiles.visible(t, v);
    for tile in &shown {
        let win = (tile.x as usize, tile.y as usize, tile.sw as usize, tile.sh as usize);
        let mut want = vec![0f32; tile.w * tile.h];
        assert!(r.read_window(t, win, &mut want, (tile.w, tile.h)));
        let bad = want.iter().zip(&tile.tex).filter(|(a, b)| a.to_bits() != b.to_bits()).count();
        assert_eq!(bad, 0, "date {t}, tile {win:?} in {} x {}: {bad} pixels differ", tile.w, tile.h);
    }
    shown.len()
}

#[test]
fn dates_paged_from_memory() {
    gdal::DriverManager::register_all();
    let d = std::env::temp_dir().join("janus-paged-tiles");
    let info = stack(&d);
    assert_eq!(info.t(), BANDS);
    let mut r = CubeReader::new(info.clone());
    let pool = JobPool::new(2, "test", None);
    // an overview of a quarter a side: tiles of levels 0 and 1
    let mut tiles = TileManager::<Vec<f32>>::new(info.clone(), 4.0, 1 << 30, None);
    assert_eq!(tiles.paged_bytes(), Some(0), "seen as pixel-interleaved");

    // full resolution, the whole image: 2 x 1 tiles of 256 (the last ones cut)
    let full = ViewRect { x0: 0.0, y0: 0.0, x1: W as f64, y1: H as f64, scale: 1.0 };
    assert_eq!(check(&mut tiles, &pool, &mut r, 0, &full), 2);
    let page = |tw: usize, th: usize| BANDS * tw * th * 4;
    let level0 = page(TILE_SIZE, H) + page(W - TILE_SIZE, H);
    assert_eq!(tiles.paged_bytes(), Some(level0), "every date of both places kept");
    // other dates: from memory (nothing more kept)
    for t in [7, 19, 3] {
        assert_eq!(check(&mut tiles, &pool, &mut r, t, &full), 2);
    }
    assert_eq!(tiles.paged_bytes(), Some(level0));

    // subsampled (level 1: 512 source pixels a tile, in 150 x 125)
    let half = ViewRect { scale: 0.4, ..full };
    for t in [5, 6] {
        assert_eq!(check(&mut tiles, &pool, &mut r, t, &half), 1);
    }
    assert_eq!(tiles.paged_bytes(), Some(level0 + page(W.div_ceil(2), H.div_ceil(2))));

    // let go with the tiles
    tiles.clear();
    assert_eq!(tiles.paged_bytes(), Some(0));
    let _ = std::fs::remove_dir_all(&d);
}
