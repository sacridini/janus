//! `janus --measure-cache on|off IN...` (src/app_fullres.cpp, measureCacheStep), without a window: the exact series
//! of a few pixels and a 256×256 ROI read from the source (cold: before anything else reads the files), the
//! overview and full-resolution builds, then the same reads from the cache, checked bit for bit against the
//! source. The pixels are the C++'s (the same std::mt19937 draws), so both programs' numbers compare.

use std::sync::Arc;
use std::time::{Duration, Instant};

use janus_cube::CubeReader;
use janus_session::{FullResMode, Session, SessionSettings};

/// std::mt19937: the C++'s generator, for the same pixels.
struct Mt19937 {
    mt: [u32; 624],
    i: usize,
}

impl Mt19937 {
    fn new(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for i in 1..624 {
            mt[i] = 1812433253u32.wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30)).wrapping_add(i as u32);
        }
        Mt19937 { mt, i: 624 }
    }

    fn next(&mut self) -> u32 {
        if self.i >= 624 {
            for k in 0..624 {
                let y = (self.mt[k] & 0x8000_0000) | (self.mt[(k + 1) % 624] & 0x7fff_ffff);
                self.mt[k] = self.mt[(k + 397) % 624] ^ (y >> 1) ^ if y & 1 != 0 { 0x9908_b0df } else { 0 };
            }
            self.i = 0;
        }
        let mut y = self.mt[self.i];
        self.i += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^ (y >> 18)
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

pub fn measure_cache(args: &[String]) -> i32 {
    if args.len() < 2 || (args[0] != "on" && args[0] != "off") {
        println!("usage: --measure-cache on|off INPUT...");
        return 2;
    }
    let on = args[0] == "on";
    let inputs = &args[1..];
    let info = match janus_cube::open_cube(inputs, &janus_cube::BandSelection::default()) {
        Ok(i) => Arc::new(i),
        Err(e) => {
            println!("{e}");
            return 1;
        }
    };
    let (w, h, t) = (info.width, info.height, info.t());
    println!(
        "{}: {w} x {h} x {t} dates ({}), cache {}\n  {}",
        inputs[0],
        info.data_type,
        if on { "on" } else { "off" },
        info.description
    );
    let mut rng = Mt19937::new(7);
    let mut px: Vec<(usize, usize)> =
        (0..5).map(|_| ((rng.next() % w as u32) as usize, (rng.next() % h as u32) as usize)).collect();
    // the C++'s 256 x 256 at the centre, inside the image when it is smaller
    let roi = ((w / 2).saturating_sub(128), (h / 2).saturating_sub(128), 256.min(w), 256.min(h));
    let mut reader = CubeReader::new(info.clone());
    let series =
        |r: &mut CubeReader, x, y| -> Vec<f32> { (0..t).map(|i| r.read_pixel(i, x, y).unwrap_or(f32::NAN)).collect() };
    for &(x, y) in &px {
        let s = Instant::now();
        series(&mut reader, x, y);
        println!("  source series ({x}, {y}): {:.0} ms (cold)", ms(s));
    }
    let mut buf = vec![0f32; roi.2 * roi.3];
    let s = Instant::now();
    for i in 0..t {
        reader.read_window(i, roi, &mut buf, (roi.2, roi.3));
    }
    println!("  source ROI 256x256 x {t} dates: {:.0} ms (cold)", ms(s));

    let settings =
        SessionSettings { full_res: if on { FullResMode::All } else { FullResMode::Off }, ..Default::default() };
    let opened = Instant::now();
    let session = Session::new(info.clone(), &settings, Arc::new(|| {}));
    while !session.overview.complete() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let ov = &session.overview;
    println!(
        "  overview: {:.1} s ({}), {} x {}",
        ov.build_seconds().unwrap_or(0.0),
        if ov.from_cache() { "from its cache" } else { "built" },
        ov.w,
        ov.h
    );
    let c = &session.full_res;
    if !on {
        return 0;
    }
    while !c.complete() && (c.building() || session.bg().pending() > 0) {
        std::thread::sleep(Duration::from_millis(20));
    }
    if !c.complete() {
        println!("  full-resolution cache not built: {}", c.error());
        return 1;
    }
    let raw = (w * h * t * 4) as f64;
    println!(
        "  full-resolution cache: {:.1} s since open ({:.1} s building, {} dates with the overview), {:.2} GB (raw {:.2} GB, {:.2}x)",
        opened.elapsed().as_secs_f64(),
        c.build_seconds(),
        c.dates_with_overview(),
        c.bytes() as f64 / 1e9,
        raw / 1e9,
        raw / c.bytes() as f64
    );
    // a new reader, as the C++'s: the source is read again through the system's cache, not GDAL's blocks
    let mut reader = CubeReader::new(info.clone());
    let mut rng = Mt19937::new(11);
    px.extend((0..5).map(|_| ((rng.next() % w as u32) as usize, (rng.next() % h as u32) as usize)));
    let mut bad = 0;
    for &(x, y) in &px {
        let (mut v, mut got) = (vec![0f32; t], vec![false; t]);
        let s = Instant::now();
        let n = c.read_series(x, y, &mut v, &mut got);
        let cache_ms = ms(s);
        let s = Instant::now();
        let src = series(&mut reader, x, y);
        let src_ms = ms(s);
        bad += usize::from(n != t || v.iter().zip(&src).any(|(a, b)| a.to_bits() != b.to_bits()));
        println!("  series ({x}, {y}): cache {cache_ms:.2} ms, source {src_ms:.0} ms (files read just now)");
    }
    let (mut a, mut b) = (vec![0f32; roi.2 * roi.3], vec![0f32; roi.2 * roi.3]);
    let (mut cache_ms, mut src_ms) = (0.0, 0.0);
    for i in 0..t {
        let s = Instant::now();
        c.read_window(i, roi, &mut a, (roi.2, roi.3));
        cache_ms += ms(s);
        let s = Instant::now();
        reader.read_window(i, roi, &mut b, (roi.2, roi.3));
        src_ms += ms(s);
        bad += usize::from(a.iter().zip(&b).any(|(p, q)| p.to_bits() != q.to_bits()));
    }
    println!("  ROI 256x256 x {t} dates: cache {cache_ms:.1} ms, source {src_ms:.0} ms (1 thread each)");
    if bad > 0 {
        println!("FAIL: {bad} reads differ from the source");
        1
    } else {
        println!("OK: cache reads equal the source");
        0
    }
}

/// `janus --describe IN...` (src/selftest.cpp, describeInputs): how the inputs open (the variables of a NetCDF /
/// Zarr, dates, size, CRS, the series of the centre pixel), without a window; the C++'s output, line for line.
pub fn describe(inputs: &[String]) -> i32 {
    use janus_cube::multidim::{format_g, list_variables};
    let t0 = Instant::now();
    if inputs.len() == 1 {
        let vars = list_variables(&inputs[0]);
        if vars.len() > 1 {
            println!("{} variables ({:.0} ms):", vars.len(), ms(t0));
            for v in &vars {
                println!(
                    "  {:<12} {}
    {}",
                    v.name, v.desc, v.dataset
                );
            }
            return 0;
        }
    }
    let info = match janus_cube::open_cube(inputs, &janus_cube::BandSelection::default()) {
        Ok(i) => Arc::new(i),
        Err(e) => {
            println!("FAIL: {e}");
            return 1;
        }
    };
    println!(
        "{}
{} x {}, {}, {} layer(s), opened in {:.0} ms{}",
        info.description,
        info.width,
        info.height,
        info.data_type,
        info.t(),
        ms(t0),
        if info.remote { " (remote)" } else { "" }
    );
    println!(
        "CRS: {}{}{}",
        if info.crs_name.is_empty() { "none" } else { &info.crs_name },
        if info.crs_authority.is_empty() { "" } else { "  " },
        info.crs_authority
    );
    if let Some(g) = info.geo_transform {
        println!(
            "origin {}, {}; pixel {} x {}",
            format_g(g[0], 10),
            format_g(g[3], 10),
            format_g(g[1], 6),
            format_g(g[5], 6)
        );
    }
    if !info.md_levels.is_empty() {
        println!("levels: {}, shown: {}", info.md_levels.len(), info.md_levels[info.sel.band as usize - 1]);
    }
    let t = info.t();
    let mut reader = CubeReader::new(info.clone());
    let (px, py) = (info.width / 2, info.height / 2);
    let tr = Instant::now();
    let mut i = 0;
    while i < t {
        if t > 14 && i == 6 {
            println!("  ...");
            i = t - 6;
        }
        let v = reader.read_pixel(i, px, py);
        // std::to_string of a float: %f
        let text = v.map_or("(read failed)".to_string(), |v| {
            if v.is_nan() { "nan".into() } else { format!("{:.6}", f64::from(v)) }
        });
        println!("  {:<20} {text}", info.layers[i].label);
        i += 1;
    }
    println!("centre pixel ({px}, {py}) read in {:.0} ms", ms(tr));
    0
}

#[cfg(test)]
mod tests {
    use super::Mt19937;

    #[test]
    fn as_std_mt19937() {
        // the C++ standard's check: the 10000th draw of a default-seeded mt19937 is 4123659995
        let mut g = Mt19937::new(5489);
        let v = (0..10000).map(|_| g.next()).last();
        assert_eq!(v, Some(4123659995));
    }
}
