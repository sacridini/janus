//! Paging through dates zoomed in on a pixel-interleaved stack: the tiles of a view at full resolution, date after
//! date, read one date at a time through GDAL (as before paging) and through the tile manager's pages.
//!
//!     cargo run --release -p janus-session --example paged_tiles_bench -- STACK.tif [DATES]

use std::sync::Arc;
use std::time::{Duration, Instant};

use janus_cache::JobPool;
use janus_cube::{BandSelection, CubeReader, open_cube};
use janus_session::tiles::TILE_SIZE;
use janus_session::{TileManager, ViewRect};

fn main() {
    gdal::DriverManager::register_all();
    let a: Vec<String> = std::env::args().collect();
    let info = Arc::new(open_cube(&[a[1].clone()], &BandSelection::default()).unwrap());
    let n: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let t = info.t();
    let dates: Vec<usize> = (0..n.min(t)).map(|i| i * t / n.min(t)).collect();
    // a 1600 x 950 map at 2 screen pixels a source pixel, at the centre
    let (cx, cy) = (info.width as f64 / 2.0, info.height as f64 / 2.0);
    let v = ViewRect { x0: cx - 400.0, y0: cy - 237.5, x1: cx + 400.0, y1: cy + 237.5, scale: 2.0 };
    println!("{} x {} x {} dates; view 800 x 475 source pixels at full resolution", info.width, info.height, t);

    // before: each tile of each date read through GDAL (4 threads)
    let tiles: Vec<(usize, usize, usize, usize)> = {
        let (x0, y0) = ((v.x0 / TILE_SIZE as f64) as usize, (v.y0 / TILE_SIZE as f64) as usize);
        let (x1, y1) = ((v.x1 / TILE_SIZE as f64) as usize, (v.y1 / TILE_SIZE as f64) as usize);
        (y0..=y1)
            .flat_map(|ty| (x0..=x1).map(move |tx| (tx * TILE_SIZE, ty * TILE_SIZE)))
            .map(|(x, y)| (x, y, TILE_SIZE.min(info.width - x), TILE_SIZE.min(info.height - y)))
            .collect()
    };
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build().unwrap();
    let old: Vec<f64> = dates
        .iter()
        .take(3)
        .map(|&d| {
            let s = Instant::now();
            pool.install(|| {
                use rayon::prelude::*;
                tiles.par_iter().for_each(|&w| {
                    let mut r = CubeReader::new(info.clone());
                    let mut buf = vec![0f32; w.2 * w.3];
                    assert!(r.read_window(d, w, &mut buf, (w.2, w.3)));
                });
            });
            s.elapsed().as_secs_f64()
        })
        .collect();
    println!("one date at a time through GDAL, {} tiles: {:?} s a date", tiles.len(), old);
    drop(pool);

    // paged
    let jobs = JobPool::new(4, "bench", None);
    let mut tm = TileManager::<()>::new(info.clone(), 4.0, 1 << 32, None);
    for &d in &dates {
        let s = Instant::now();
        loop {
            tm.tick();
            tm.update(&jobs, d, &v, None);
            tm.upload_ready(64, |_| ());
            if tm.missing(d, &v) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        println!(
            "paged, date {d:>3}: {:>7.3} s ({} MB kept)",
            s.elapsed().as_secs_f64(),
            tm.paged_bytes().unwrap_or(0) >> 20
        );
    }
}
