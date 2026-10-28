//! Speckle filters of a radar series (IDEIAS phase 47; Tools > Speckle filter): Lee's, spatial, each date in a
//! window of its own, and Quegan's multitemporal one, each date's window mean times the mean of every date's ratio
//! to its own; on the bands' linear values (σ⁰ or γ⁰, not dB), every band of every date, into a folder of COGs as
//! Regularize writes them.
//!
//! The windows are cut by the series' edges (or the window filtered) and count the pixels with a value only, as
//! scipy's uniform_filter over the values and over the pixels with a value (mode "constant") does.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Metadata;
use janus_cube::CubeInfo;
use janus_cube::dates::civil_from_days;

use crate::regularize::{read_date, to_cogs};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Filter {
    /// Lee (1980): the window's mean m plus w (x - m), w = 1 - m² / (L v), at least 0, v the window's variance,
    /// L the number of looks (1 / L the speckle's squared variation)
    Lee { looks: f64 },
    /// Quegan and Yu (2001): J_k = m_k / N · Σ_i x_i / m_i over the N dates with a value, m_i the window mean of
    /// date i
    Quegan,
}

impl Filter {
    pub fn name(&self) -> &'static str {
        match self {
            Filter::Lee { .. } => "Lee",
            Filter::Quegan => "Quegan multitemporal",
        }
    }
}

#[derive(Clone, Debug)]
pub struct SpeckleOptions {
    pub filter: Filter,
    /// the window's side in pixels (odd; an even one is made odd)
    pub size: usize,
    /// the window (x0, y0, x1, y1) in the series' pixels (None: all of it)
    pub window: Option<[usize; 4]>,
    /// the files' names: `<stem>_<YYYYMMDD>.tif`
    pub stem: String,
    pub threads: usize,
}

/// The window means (and variances) of `rows` [a, b) of `src` (its rows of `w` pixels; NaN: no value): each
/// pixel's window of 2 rad + 1 pixels a side, cut by the edges of `src`, over the pixels with a value; NaN where
/// none has one. `mean` (and `var`) take (b - a) rows.
pub fn box_stats(
    src: &[f32],
    w: usize,
    (a, b): (usize, usize),
    rad: usize,
    mean: &mut [f32],
    mut var: Option<&mut [f32]>,
) {
    let rows = src.len() / w.max(1);
    // each column's sums over the window's rows, updated row after row
    let mut s = vec![0f64; w];
    let mut q = vec![0f64; w];
    let mut c = vec![0u32; w];
    let add = |y: usize, sign: f64, s: &mut [f64], q: &mut [f64], c: &mut [u32]| {
        for (x, &v) in src[y * w..(y + 1) * w].iter().enumerate() {
            if v.is_finite() {
                let v = f64::from(v);
                s[x] += sign * v;
                q[x] += sign * v * v;
                if sign > 0.0 {
                    c[x] += 1;
                } else {
                    c[x] -= 1;
                }
            }
        }
    };
    for y in a.saturating_sub(rad)..(a + rad + 1).min(rows) {
        add(y, 1.0, &mut s, &mut q, &mut c);
    }
    // the row's running sums across the columns
    let (mut ps, mut pq, mut pc) = (vec![0f64; w + 1], vec![0f64; w + 1], vec![0u32; w + 1]);
    for y in a..b {
        if y > a {
            if y + rad < rows {
                add(y + rad, 1.0, &mut s, &mut q, &mut c);
            }
            if y > rad {
                add(y - rad - 1, -1.0, &mut s, &mut q, &mut c);
            }
        }
        for x in 0..w {
            ps[x + 1] = ps[x] + s[x];
            pq[x + 1] = pq[x] + q[x];
            pc[x + 1] = pc[x] + c[x];
        }
        let o = (y - a) * w;
        for x in 0..w {
            let (x0, x1) = (x.saturating_sub(rad), (x + rad + 1).min(w));
            let n = pc[x1] - pc[x0];
            if n == 0 {
                mean[o + x] = f32::NAN;
                if let Some(v) = var.as_deref_mut() {
                    v[o + x] = f32::NAN;
                }
                continue;
            }
            let m = (ps[x1] - ps[x0]) / f64::from(n);
            mean[o + x] = m as f32;
            if let Some(v) = var.as_deref_mut() {
                v[o + x] = ((pq[x1] - pq[x0]) / f64::from(n) - m * m).max(0.0) as f32;
            }
        }
    }
}

/// Lee's filter of one pixel: its value, its window's mean and variance.
pub fn lee(x: f32, m: f32, v: f32, looks: f64) -> f32 {
    if !x.is_finite() {
        return f32::NAN;
    }
    let (x, m, v) = (f64::from(x), f64::from(m), f64::from(v));
    let k = if v > 0.0 { (1.0 - m * m / (looks * v)).max(0.0) } else { 0.0 };
    (m + k * (x - m)) as f32
}

/// The day's text, YYYYMMDD.
fn ymd(time: f64) -> String {
    let (y, m, d) = civil_from_days((time / 86400.0).floor() as i64);
    format!("{y:04}{m:02}{d:02}")
}

/// A pointer written by several threads at places none shares.
struct SendPtr(*mut f32);
// SAFETY: the threads write disjoint pixels.
unsafe impl Sync for SendPtr {}

/// Filters the series `info` into `out_dir`: a COG a date with every band (`progress` from 0 to 1 with a message).
pub fn speckle(
    info: &Arc<CubeInfo>,
    opts: &SpeckleOptions,
    out_dir: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    if !info.time_is_date {
        return Err("the series has no dates".into());
    }
    let t_n = info.t();
    if t_n == 0 {
        return Err("no dates".into());
    }
    if opts.filter == Filter::Quegan && t_n < 2 {
        return Err("Quegan's filter needs 2 dates or more".into());
    }
    let win = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    if w == 0 || h == 0 {
        return Err("an empty window".into());
    }
    let rad = opts.size.max(1) / 2;
    let bands = info.bands_per_date.max(1);
    let names: Vec<String> =
        (0..bands).map(|b| info.band_names.get(b).cloned().unwrap_or_else(|| format!("band {}", b + 1))).collect();
    let part = out_dir.join(".part");
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    // uncompressed: read once more by the COGs' writing, which compresses
    let co = gdal::raster::RasterCreationOptions::from_iter(["TILED=YES", "BIGTIFF=IF_SAFER", "SPARSE_OK=TRUE"]);
    let gt = info.geo_transform.map(|g| {
        let (x0, y0) = (win[0] as f64, win[1] as f64);
        [g[0] + x0 * g[1] + y0 * g[2], g[1], g[2], g[3] + x0 * g[4] + y0 * g[5], g[4], g[5]]
    });
    let mut dss = Vec::with_capacity(t_n);
    let mut temps: Vec<PathBuf> = Vec::with_capacity(t_n);
    for t in 0..t_n {
        // two dates of one day keep both
        let day = ymd(info.layers[t].time);
        let mut f = part.join(format!("{}_{day}.tif", opts.stem));
        let mut k = 2;
        while temps.contains(&f) {
            f = part.join(format!("{}_{day}_{k}.tif", opts.stem));
            k += 1;
        }
        let mut ds = drv
            .create_with_band_type_with_options::<f32, _>(&f, w, h, bands, &co)
            .map_err(|e| format!("{}: {e}", f.display()))?;
        if let Some(gt) = gt {
            ds.set_geo_transform(&gt).map_err(|e| e.to_string())?;
        }
        if !info.crs_wkt.is_empty() {
            ds.set_projection(&info.crs_wkt).map_err(|e| e.to_string())?;
        }
        for (b, name) in names.iter().enumerate() {
            let mut rb = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
            rb.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
            rb.set_description(name).map_err(|e| e.to_string())?;
        }
        dss.push(ds);
        temps.push(f);
    }
    // rows a block: every date's bands of the block (and its margins) read, filtered and (Quegan) their means
    let planes = t_n * bands;
    let per_row = planes * w * 4 * 3;
    let rows = ((768usize << 20) / per_row.max(1)).clamp(rad.max(16), 512).min(h);
    let threads = opts.threads.max(1);
    let blocks = h.div_ceil(rows);
    for (k, y0) in (0..h).step_by(rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let r = rows.min(h - y0);
        let n = w * r;
        // the block's rows and the window's margins above and below it
        let (ya, yb) = (y0.saturating_sub(rad), (y0 + r + rad).min(h));
        let m = w * (yb - ya);
        progress(0.85 * k as f64 / blocks as f64, &format!("rows {} to {} of {h}: reading", y0 + 1, y0 + r));
        let mut values = vec![f32::NAN; planes * m];
        {
            let next = AtomicUsize::new(0);
            let chunks: Vec<std::sync::Mutex<&mut [f32]>> =
                values.chunks_mut(bands * m).map(std::sync::Mutex::new).collect();
            std::thread::scope(|s| {
                for _ in 0..threads.min(t_n) {
                    s.spawn(|| {
                        loop {
                            let t = next.fetch_add(1, Ordering::Relaxed);
                            if t >= t_n || cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let mut c = chunks[t].lock().unwrap();
                            if !read_date(info, t, true, (win[0], win[1] + ya, w, yb - ya), &mut c) {
                                c.fill(f32::NAN);
                            }
                        }
                    });
                }
            });
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        progress(0.85 * (k as f64 + 0.5) / blocks as f64, &format!("rows {} to {} of {h}: filtering", y0 + 1, y0 + r));
        // each date's band (a plane) on a thread: Lee's values, or Quegan's window means
        let mut out = vec![f32::NAN; planes * n];
        let (a, b) = (y0 - ya, y0 - ya + r);
        {
            let next = AtomicUsize::new(0);
            let chunks: Vec<std::sync::Mutex<&mut [f32]>> = out.chunks_mut(n).map(std::sync::Mutex::new).collect();
            let values = &values;
            std::thread::scope(|s| {
                for _ in 0..threads.min(planes) {
                    s.spawn(|| {
                        let mut var = vec![0f32; n];
                        loop {
                            let p = next.fetch_add(1, Ordering::Relaxed);
                            if p >= planes {
                                break;
                            }
                            let src = &values[p * m..(p + 1) * m];
                            let mut o = chunks[p].lock().unwrap();
                            match opts.filter {
                                Filter::Lee { looks } => {
                                    box_stats(src, w, (a, b), rad, &mut o, Some(&mut var));
                                    for i in 0..n {
                                        o[i] = lee(src[a * w + i], o[i], var[i], looks);
                                    }
                                }
                                Filter::Quegan => box_stats(src, w, (a, b), rad, &mut o, None),
                            }
                        }
                    });
                }
            });
        }
        if opts.filter == Filter::Quegan {
            // each pixel: every date's ratio to its window mean, summed over the dates with a value
            let step = n.div_ceil(threads * 4).max(256);
            let next = AtomicUsize::new(0);
            let out_ptr = SendPtr(out.as_mut_ptr());
            let values = &values;
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let out_ptr = &out_ptr;
                        loop {
                            let i0 = next.fetch_add(step, Ordering::Relaxed);
                            if i0 >= n {
                                break;
                            }
                            for i in i0..(i0 + step).min(n) {
                                for bd in 0..bands {
                                    let (mut sum, mut cnt) = (0f64, 0usize);
                                    for t in 0..t_n {
                                        let p = t * bands + bd;
                                        let x = values[p * m + a * w + i];
                                        // SAFETY: pixel i of every plane is this thread's alone
                                        let mi = unsafe { *out_ptr.0.add(p * n + i) };
                                        if x.is_finite() && mi > 0.0 {
                                            sum += f64::from(x) / f64::from(mi);
                                            cnt += 1;
                                        }
                                    }
                                    for t in 0..t_n {
                                        let p = t * bands + bd;
                                        let x = values[p * m + a * w + i];
                                        // SAFETY: as above
                                        unsafe {
                                            let o = out_ptr.0.add(p * n + i);
                                            *o = if x.is_finite() && cnt > 0 {
                                                (f64::from(*o) * sum / cnt as f64) as f32
                                            } else {
                                                f32::NAN
                                            };
                                        }
                                    }
                                }
                            }
                        }
                    });
                }
            });
        }
        progress(0.85 * (k as f64 + 0.8) / blocks as f64, &format!("rows {} to {} of {h}: writing", y0 + 1, y0 + r));
        for (t, ds) in dss.iter_mut().enumerate() {
            for bd in 0..bands {
                let mut rb = ds.rasterband(bd + 1).map_err(|e| e.to_string())?;
                let p = t * bands + bd;
                let mut buf = gdal::raster::Buffer::new((w, r), out[p * n..(p + 1) * n].to_vec());
                rb.write((0, y0 as isize), (w, r), &mut buf).map_err(|e| e.to_string())?;
            }
        }
    }
    drop(dss);
    let files = to_cogs(&temps, out_dir, opts.threads, &|f| progress(0.85 + 0.15 * f, "writing the COGs"), cancel)?;
    let _ = std::fs::remove_dir_all(&part);
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The windows' means and variances, each pixel's window summed whole.
    fn brute(src: &[f32], w: usize, rad: usize) -> (Vec<f32>, Vec<f32>) {
        let h = src.len() / w;
        let (mut m, mut v) = (vec![f32::NAN; src.len()], vec![f32::NAN; src.len()]);
        for y in 0..h {
            for x in 0..w {
                let (mut s, mut q, mut c) = (0f64, 0f64, 0usize);
                for yy in y.saturating_sub(rad)..(y + rad + 1).min(h) {
                    for xx in x.saturating_sub(rad)..(x + rad + 1).min(w) {
                        let a = src[yy * w + xx];
                        if a.is_finite() {
                            s += f64::from(a);
                            q += f64::from(a) * f64::from(a);
                            c += 1;
                        }
                    }
                }
                if c > 0 {
                    let mm = s / c as f64;
                    m[y * w + x] = mm as f32;
                    v[y * w + x] = (q / c as f64 - mm * mm).max(0.0) as f32;
                }
            }
        }
        (m, v)
    }

    #[test]
    fn windows_as_summed_whole() {
        let (w, h) = (23, 17);
        let mut rng = crate::rng::Mt19937::new(47);
        let src: Vec<f32> =
            (0..w * h).map(|i| if i % 11 == 3 { f32::NAN } else { (rng.next_double() * 0.3) as f32 }).collect();
        for rad in [1, 2, 3, 9] {
            let (bm, bv) = brute(&src, w, rad);
            // the whole, then rows 5 to 12 alone (the rest of the strip their margins)
            for (a, b) in [(0, h), (5, 12)] {
                let mut m = vec![0f32; (b - a) * w];
                let mut v = vec![0f32; (b - a) * w];
                box_stats(&src, w, (a, b), rad, &mut m, Some(&mut v));
                for i in 0..m.len() {
                    let j = a * w + i;
                    assert!((m[i] - bm[j]).abs() < 1e-6 && (v[i] - bv[j]).abs() < 1e-6, "{rad} {a} {i}");
                }
            }
        }
        // a pixel alone with NaN all round: its own value, no variance; NaN far from any value
        let mut s = vec![f32::NAN; 25];
        s[12] = 2.0;
        let (mut m, mut v) = (vec![0f32; 25], vec![0f32; 25]);
        box_stats(&s, 5, (0, 5), 1, &mut m, Some(&mut v));
        assert_eq!((m[12], v[12]), (2.0, 0.0));
        assert!(m[0].is_nan());
    }

    #[test]
    fn lee_weights() {
        // no variation: the mean; much more than the speckle's: the pixel itself, nearly
        assert_eq!(lee(0.5, 0.2, 0.0, 4.4), 0.2);
        assert!((lee(0.5, 0.2, 100.0, 4.4) - 0.5).abs() < 1e-3);
        // the speckle's own variation (v = m² / L): the mean
        assert!((lee(0.5, 0.2, 0.04 / 4.4, 4.4) - 0.2).abs() < 1e-6);
        assert!(lee(f32::NAN, 0.2, 0.1, 4.4).is_nan());
    }

    /// A speckled constant through Lee and Quegan: less spread, the same mean; dates all alike come back as they
    /// were from Quegan's.
    #[test]
    fn filters_series() {
        let dir = std::env::temp_dir().join(format!("janus_speckle_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32720).unwrap().to_wkt().unwrap();
        let (w, h) = (64usize, 48usize);
        let mut rng = crate::rng::Mt19937::new(1);
        // exponential speckle (one look) times 0.1 (VV) and 0.02 (VH)
        let mut speck =
            |base: f64| -> Vec<f32> { (0..w * h).map(|_| (base * -(1.0 - rng.next_double()).ln()) as f32).collect() };
        let mk = |name: &str, bands: &[Vec<f32>]| -> String {
            let p = dir.join(name);
            let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut d = drv.create_with_band_type::<f32, _>(&p, w, h, bands.len()).unwrap();
            d.set_geo_transform(&[500000.0, 10.0, 0.0, 9000000.0, 0.0, -10.0]).unwrap();
            d.set_projection(&wkt).unwrap();
            for (b, v) in bands.iter().enumerate() {
                let mut rb = d.rasterband(b + 1).unwrap();
                rb.set_description(["vv", "vh"][b]).unwrap();
                let mut buf = gdal::raster::Buffer::new((w, h), v.clone());
                rb.write((0, 0), (w, h), &mut buf).unwrap();
            }
            p.to_string_lossy().into_owned()
        };
        let files: Vec<String> =
            (0..6).map(|t| mk(&format!("s1_2023{:02}01.tif", t + 1), &[speck(0.1), speck(0.02)])).collect();
        let info = Arc::new(janus_cube::open_cube(&files, &Default::default()).unwrap());
        assert_eq!((info.t(), info.bands_per_date), (6, 2));
        let none = AtomicBool::new(false);
        let spread = |path: &str, b: usize| -> (f64, f64) {
            let d = gdal::Dataset::open(path).unwrap();
            let v: Vec<f32> =
                d.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec();
            let n = v.len() as f64;
            let m = v.iter().map(|&x| f64::from(x)).sum::<f64>() / n;
            let s = (v.iter().map(|&x| (f64::from(x) - m).powi(2)).sum::<f64>() / n).sqrt();
            (m, s / m)
        };
        let (m0, cv0) = spread(&files[2], 1);
        assert!((cv0 - 1.0).abs() < 0.1, "one look: a variation of 1 ({cv0})");
        for (name, filter) in [("lee", Filter::Lee { looks: 1.0 }), ("quegan", Filter::Quegan)] {
            let opts = SpeckleOptions { filter, size: 7, window: None, stem: "f".into(), threads: 4 };
            let out = speckle(&info, &opts, &dir.join(name), &|_, _| {}, &none).unwrap();
            assert_eq!(out.len(), 6);
            assert!(out[0].ends_with("f_20230101.tif"), "{}", out[0]);
            let (m, cv) = spread(&out[2], 1);
            assert!((m - m0).abs() / m0 < 0.05, "{name}: the mean kept ({m} {m0})");
            assert!(cv < 0.6, "{name}: less speckle ({cv})");
            let d = gdal::Dataset::open(&out[2]).unwrap();
            assert_eq!(d.rasterband(2).unwrap().description().unwrap(), "vh");
        }
        // Quegan on dates all alike: each comes back as it was
        let same = speck(0.1);
        let files: Vec<String> =
            (0..3).map(|t| mk(&format!("same_2023{:02}01.tif", t + 1), &[same.clone(), same.clone()])).collect();
        let info = Arc::new(janus_cube::open_cube(&files, &Default::default()).unwrap());
        let opts = SpeckleOptions { filter: Filter::Quegan, size: 5, window: None, stem: "q".into(), threads: 2 };
        let out = speckle(&info, &opts, &dir.join("same"), &|_, _| {}, &none).unwrap();
        let d = gdal::Dataset::open(&out[1]).unwrap();
        let v = d.rasterband(1).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
        let diff = v.data().iter().zip(&same).map(|(a, b)| (a - b).abs() / b.max(1e-6)).fold(0f32, f32::max);
        assert!(diff < 1e-5, "{diff}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
