//! terrio against GDAL on the reads Janus makes (RUST_MIGRATION.md, decision 6): `terrio-trial <series folder>`.
//!
//! 1. a whole date at full resolution (the full-resolution cache, tiles), bit for bit;
//! 2. a date reduced to the overview's size by nearest neighbour (the overview), GDAL's pixels exactly;
//! 3. a pixel's series over every date (the cursor), files kept open;
//! 4. the overview of every date on 12 threads (opening a series).

use std::time::Instant;

use janus_cache::fullres::{source_columns, source_rows};
use janus_cache::overview::overview_size;
use terrio::{Window, WindowOptions};

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn gdal_window(path: &str, win: (usize, usize, usize, usize), size: (usize, usize)) -> Vec<f32> {
    let ds = janus_cube::open_raster(path).unwrap();
    let band = ds.rasterband(1).unwrap();
    let mut buf = vec![0f32; size.0 * size.1];
    band.read_into_slice::<f32>(
        (win.0 as isize, win.1 as isize),
        (win.2, win.3),
        size,
        &mut buf,
        Some(gdal::raster::ResampleAlg::NearestNeighbour),
    )
    .unwrap();
    buf
}

fn terrio_full(path: &str) -> Vec<f32> {
    let mut r = terrio::open_raster_lazy(path).unwrap();
    let (w, h) = (r.width(), r.height());
    let ds = r.read_window(Window::full(w, h), &WindowOptions { bands: vec![1], overview: None }).unwrap();
    match ds.raster.data.as_f32_slice() {
        Some(s) => s.to_vec(),
        None => ds.raster.data.to_f64_vec().into_iter().map(|v| v as f32).collect(),
    }
}

/// The overview by nearest neighbour as GDAL picks its pixels: only the source rows it needs, one window each.
fn terrio_reduced(path: &str, w: usize, h: usize, ow: usize, oh: usize) -> Vec<f32> {
    let mut r = terrio::open_raster_lazy(path).unwrap();
    let xs = source_columns(0, w, ow, w);
    let ys = source_rows(0, h, oh, h);
    let mut out = vec![0f32; ow * oh];
    let opts = WindowOptions { bands: vec![1], overview: None };
    for (j, &y) in ys.iter().enumerate() {
        let row = r.read_window(Window::new(0, y, w, 1), &opts).unwrap();
        let s = row.raster.data.as_f32_slice().unwrap();
        for (i, &x) in xs.iter().enumerate() {
            out[j * ow + i] = s[x];
        }
    }
    out
}

fn same(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let info = janus_cube::open_cube(&args, &janus_cube::BandSelection::default()).expect("open");
    let (w, h, t) = (info.width, info.height, info.t());
    let (ow, oh) = overview_size(w, h, t, 1024 << 20, 16384);
    let paths: Vec<String> = info.layers.iter().map(|l| l.path.clone()).collect();
    println!("{}: {w} x {h} x {t}, overview {ow} x {oh}", args[0]);
    // the files in the system's cache first, for both
    for p in &paths {
        let _ = std::fs::read(p);
    }

    // 1. a whole date
    let s = Instant::now();
    let g = gdal_window(&paths[0], (0, 0, w, h), (w, h));
    let g_ms = ms(s);
    let s = Instant::now();
    let tr = terrio_full(&paths[0]);
    let t_ms = ms(s);
    println!("1. one date at full resolution: GDAL {g_ms:.0} ms, terrio {t_ms:.0} ms, same pixels: {}", same(&g, &tr));

    // 2. a date reduced to the overview
    let s = Instant::now();
    let g = gdal_window(&paths[1], (0, 0, w, h), (ow, oh));
    let g_ms = ms(s);
    let s = Instant::now();
    let tr = terrio_reduced(&paths[1], w, h, ow, oh);
    let t_ms = ms(s);
    println!("2. one date reduced to the overview: GDAL {g_ms:.0} ms, terrio {t_ms:.0} ms, same pixels: {}", same(&g, &tr));

    // 3. a pixel's series, files kept open (the second pass: blocks cached by each library as it caches them)
    let (x, y) = (w / 3, h / 2);
    let gds: Vec<_> = paths.iter().map(|p| janus_cube::open_raster(p).unwrap()).collect();
    let mut tds: Vec<_> = paths.iter().map(|p| terrio::open_raster_lazy(p).unwrap()).collect();
    let opts = WindowOptions { bands: vec![1], overview: None };
    for pass in 0..2 {
        let s = Instant::now();
        let gv: Vec<f32> = gds
            .iter()
            .map(|d| {
                let mut v = [0f32];
                d.rasterband(1).unwrap().read_into_slice((x as isize, y as isize), (1, 1), (1, 1), &mut v, None).unwrap();
                v[0]
            })
            .collect();
        let g_ms = ms(s);
        let s = Instant::now();
        let tv: Vec<f32> = tds
            .iter_mut()
            .map(|r| r.read_window(Window::new(x, y, 1, 1), &opts).unwrap().raster.data.as_f32_slice().unwrap()[0])
            .collect();
        let t_ms = ms(s);
        println!(
            "3. a pixel's series ({}): GDAL {g_ms:.2} ms, terrio {t_ms:.2} ms, same values: {}",
            if pass == 0 { "first" } else { "again" },
            same(&gv, &tv)
        );
    }

    // 4. the overview of every date on 12 threads
    let threads = 12;
    let run = |f: &(dyn Fn(&str) -> Vec<f32> + Sync)| {
        let s = Instant::now();
        let next = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(|| {
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= paths.len() {
                            break;
                        }
                        std::hint::black_box(f(&paths[i]));
                    }
                });
            }
        });
        ms(s)
    };
    let g_ms = run(&|p| gdal_window(p, (0, 0, w, h), (ow, oh)));
    let t_ms = run(&|p| terrio_reduced(p, w, h, ow, oh));
    let xs = source_columns(0, w, ow, w);
    let ys = source_rows(0, h, oh, h);
    let reduce = |p: &str| -> Vec<f32> {
        let full = terrio_full(p);
        let mut out = vec![0f32; ow * oh];
        for (j, &y) in ys.iter().enumerate() {
            for (i, &x) in xs.iter().enumerate() {
                out[j * ow + i] = full[y * w + x];
            }
        }
        out
    };
    let s = Instant::now();
    let one = reduce(&paths[1]);
    println!("2b. one date reduced from a whole read: terrio {:.0} ms, same pixels: {}", ms(s), same(&one, &gdal_window(&paths[1], (0, 0, w, h), (ow, oh))));
    let f_ms = run(&reduce);
    println!("4. the overview, {t} dates on {threads} threads: GDAL {g_ms:.0} ms, terrio by rows {t_ms:.0} ms, terrio whole dates {f_ms:.0} ms");
}
