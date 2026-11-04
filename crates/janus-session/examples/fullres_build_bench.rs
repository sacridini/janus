//! The full-resolution cache of a series built (every series' setting) in a cache folder of its own, then a zoomed
//! view's tiles of each date read from it: the time of the build (at most LIMIT seconds; the dates done by then)
//! and of each date's tiles.
//!
//!     cargo run --release -p janus-session --example fullres_build_bench -- STACK.tif CACHE_DIR [THREADS] [LIMIT]

use std::sync::Arc;
use std::time::{Duration, Instant};

use janus_cube::{BandSelection, open_cube};
use janus_session::{FullResMode, Session, SessionSettings, TileManager, ViewRect};

fn main() {
    gdal::DriverManager::register_all();
    let a: Vec<String> = std::env::args().collect();
    let info = Arc::new(open_cube(&[a[1].clone()], &BandSelection::default()).unwrap());
    let threads: usize = a.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let limit: f64 = a.get(4).and_then(|s| s.parse().ok()).unwrap_or(600.0);
    let _ = std::fs::remove_dir_all(&a[2]);
    std::fs::create_dir_all(&a[2]).unwrap();
    let settings = SessionSettings {
        full_res: FullResMode::All,
        threads,
        io_threads: threads,
        cache_dir: a[2].clone().into(),
        ..Default::default()
    };
    let t = info.t();
    println!("{} x {} x {t} dates, {threads} threads", info.width, info.height);
    let opened = Instant::now();
    let s = Session::new(info.clone(), &settings, Arc::new(|| {}));
    while !s.overview.complete() {
        std::thread::sleep(Duration::from_millis(5));
    }
    println!("overview: {:.1} s", opened.elapsed().as_secs_f64());
    let c = &s.full_res;
    let start = Instant::now();
    let mut said = 0;
    while !c.complete() && start.elapsed().as_secs_f64() < limit && (c.building() || s.bg().pending() > 0) {
        std::thread::sleep(Duration::from_millis(20));
        if c.dates_done() != said {
            said = c.dates_done();
            println!("  {said} dates after {:.1} s", start.elapsed().as_secs_f64());
        }
    }
    println!(
        "full-resolution cache: {}/{t} dates in {:.1} s, {:.2} GB{}",
        c.dates_done(),
        start.elapsed().as_secs_f64(),
        c.bytes() as f64 / 1e9,
        if c.error().is_empty() { String::new() } else { format!(" ({})", c.error()) }
    );
    if !c.complete() {
        c.stop();
        return;
    }
    // a 1600 x 950 map at 2 screen pixels a source pixel, at the centre: each date's tiles from the cache
    let (cx, cy) = (info.width as f64 / 2.0, info.height as f64 / 2.0);
    let v = ViewRect { x0: cx - 400.0, y0: cy - 237.5, x1: cx + 400.0, y1: cy + 237.5, scale: 2.0 };
    let mut tm = TileManager::<()>::new(info.clone(), s.overview.factor, 1 << 32, Some(c.clone()));
    let mut times = Vec::new();
    for d in 0..t {
        let st = Instant::now();
        loop {
            tm.tick();
            tm.update(s.fg(), d, &v, None);
            tm.upload_ready(64, |_| ());
            if tm.missing(d, &v) == 0 {
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        times.push(st.elapsed().as_secs_f64() * 1000.0);
    }
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    let max = times.iter().cloned().fold(0.0, f64::max);
    println!("tiles of each date from the cache: first {:.1} ms, mean {mean:.1} ms, max {max:.1} ms", times[0]);
    // a window across blocks of a few dates, as GDAL reads them
    let win = ((info.width / 2).saturating_sub(150), (info.height / 2).saturating_sub(150), 300, 300);
    let (mut a, mut b) = (vec![0f32; 300 * 300], vec![0f32; 300 * 300]);
    let mut r = janus_cube::CubeReader::new(info.clone());
    for d in [0, t / 2, t - 1] {
        assert!(c.read_window(d, win, &mut a, (300, 300)) && r.read_window(d, win, &mut b, (300, 300)));
        let bad = a.iter().zip(&b).filter(|(x, y)| x.to_bits() != y.to_bits()).count();
        println!("date {d}: {bad} of {} pixels differ from GDAL's", a.len());
    }
}
