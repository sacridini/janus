//! A series made one composite per period (File > Search STAC catalog's Download a composite, as zeit's
//! build_annual_composites): the best available pixel (zeit_core::bap: each pixel's clear observation of the highest
//! score, by its day of the year near a target day, its distance to the nearest cloud or shadow, its sensor and its
//! scene's cloud cover), the greenest (the highest NDVI), the medoid or the median of the dates of each period.
//!
//! Read block by block. The best available pixel and the greenest know their winners before reading a band: the
//! quality band of every date first (with a margin of the cloud distance around the block, the clouds of the
//! neighbours seen), the winners chosen, then the bands of the dates that won somewhere in the block only, so a
//! catalog's series is mostly not downloaded (the quality band and a few dates of each block). The medoid and the
//! median need every date's bands.
//!
//! Written as cloud-optimized GeoTIFFs, `<stem>_<YYYYMMDD>.tif` a period (its first day), and with `info` the
//! date each pixel took (its year and day of the year), its score and its clear observations in info/.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Metadata;
use gdal::spatial_ref::SpatialRef;
use janus_cube::dates::{civil_from_days, days_from_civil};
use janus_cube::{CubeInfo, QaRule, with_thread_reader};
use zeit_core::bap;

/// The best available pixel's settings (zeit_core::bap's), for callers without zeit-core.
pub type BapOptions = bap::Options;

use crate::regularize::{Composite, Period, Step, composite as reduce, periods, to_cogs, ymd};

/// What a period's dates give.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// each pixel's clear observation of the highest score (zeit_core::bap)
    Bap,
    /// each pixel's clear observation of the highest NDVI
    Greenest,
    /// the observation nearest the bands' medians
    Medoid,
    /// each band's median
    Median,
}

impl Method {
    pub const ALL: [Method; 4] = [Method::Bap, Method::Greenest, Method::Medoid, Method::Median];

    pub fn label(self) -> &'static str {
        match self {
            Method::Bap => "Best available pixel",
            Method::Greenest => "Greenest (highest NDVI)",
            Method::Medoid => "Medoid",
            Method::Median => "Median",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Method::Bap => "bap",
            Method::Greenest => "greenest",
            Method::Medoid => "medoid",
            Method::Median => "median",
        }
    }

    /// Its winners known from the quality (and NDVI) before the bands are read.
    pub fn picks(self) -> bool {
        matches!(self, Method::Bap | Method::Greenest)
    }
}

#[derive(Clone, Debug)]
pub struct CompOptions {
    pub method: Method,
    /// one composite per period of this step (None: one of every date)
    pub step: Option<Step>,
    /// the bands composited (1-based in each date's file), in their order, and their names
    pub bands: Vec<i32>,
    pub names: Vec<String>,
    /// the quality band (1-based, 0: none) and its rule
    pub qa_band: i32,
    pub qa_rule: QaRule,
    /// Greenest: the red and NIR bands (1-based)
    pub red: i32,
    pub nir: i32,
    /// Bap: the score's settings, its cloud distances in metres
    pub bap: bap::Options,
    /// each date's sensor and cover scores (0 to 1; empty: 1)
    pub sensor: Vec<f64>,
    pub cover: Vec<f64>,
    /// write the info files (the date, score and clear observations of each pixel)
    pub info: bool,
    /// the window (x0, y0, x1, y1) in the series' pixels (None: all of it)
    pub window: Option<[usize; 4]>,
    /// the side of a block (pixels)
    pub block: usize,
    pub threads: usize,
    pub stem: String,
}

impl Default for CompOptions {
    fn default() -> Self {
        CompOptions {
            method: Method::Bap,
            step: None,
            bands: Vec::new(),
            names: Vec::new(),
            qa_band: 0,
            qa_rule: QaRule::None,
            red: 0,
            nir: 0,
            bap: bap::Options { cloud_min: 30.0, cloud_max: 1500.0, ..Default::default() },
            sensor: Vec::new(),
            cover: Vec::new(),
            info: true,
            window: None,
            block: 1024,
            threads: 4,
            stem: "composite".into(),
        }
    }
}

/// What `composite` wrote.
#[derive(Debug)]
pub struct Composited {
    /// the composites, a period each, in order
    pub files: Vec<String>,
    pub info_files: Vec<String>,
    pub periods: Vec<Period>,
    /// periods without a date
    pub empty: usize,
    /// the pixels read (of every band and date, the quality band's margin included), and those reading every date's
    /// bands and quality would have read
    pub reads: usize,
    pub full_reads: usize,
    /// band windows that could not be read (their dates left out there)
    pub failed: usize,
}

/// The info files' bands.
pub const INFO_BANDS: [&str; 4] = ["year", "day of year", "score", "clear observations"];

/// The score of a sensor at a day (days since 1970): 0.5 for Landsat 7 after its scan line corrector failed
/// (2003-05-31), else 1.
pub fn sensor_score(platform: &str, day: i64) -> f64 {
    let p = platform.to_ascii_lowercase().replace('_', "-");
    if (p.contains("landsat-7") || p == "le07" || p.contains("etm")) && day >= days_from_civil(2003, 5, 31) {
        0.5
    } else {
        1.0
    }
}

/// The day of the year (1 to 366) of Unix seconds.
fn day_of_year(t: f64) -> f64 {
    let d = (t / 86400.0).floor() as i64;
    let (y, _, _) = civil_from_days(d);
    (d - days_from_civil(y, 1, 1) + 1) as f64
}

/// A pixel's side in metres: the geotransform's, in degrees of longitude at the middle latitude when the CRS is
/// geographic.
fn pixel_metres(info: &CubeInfo) -> f64 {
    let Some(gt) = info.geo_transform else { return 1.0 };
    let geographic = !info.crs_wkt.is_empty() && SpatialRef::from_wkt(&info.crs_wkt).is_ok_and(|s| s.is_geographic());
    if geographic {
        let lat = gt[3] + 0.5 * info.height as f64 * gt[5];
        gt[1].abs() * 111_320.0 * lat.to_radians().cos()
    } else {
        gt[1].abs()
    }
}

/// Runs `f(i)` for i in 0..n on `threads` threads: the results in order (None for those cancelled).
fn par_map<T: Send>(n: usize, threads: usize, cancel: &AtomicBool, f: impl Fn(usize) -> T + Sync) -> Vec<Option<T>> {
    let out: Vec<std::sync::Mutex<Option<T>>> = (0..n).map(|_| std::sync::Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads.clamp(1, n.max(1)) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let v = f(i);
                    *out[i].lock().unwrap() = Some(v);
                }
            });
        }
    });
    out.into_iter().map(|m| m.into_inner().unwrap()).collect()
}

/// A window of one band of a date (scaled, no data NaN), None if it could not be read.
fn read(info: &Arc<CubeInfo>, t: usize, band: i32, win: (usize, usize, usize, usize)) -> Option<Vec<f32>> {
    let mut buf = vec![0f32; win.2 * win.3];
    with_thread_reader(info, |r| r.read_band_window(t, band, win, &mut buf, (win.2, win.3))).then_some(buf)
}

/// The quality states (zeit_core::bap) of a quality band's codes.
fn states_of(q: &[f32], rule: QaRule, out: &mut [u8]) {
    for (s, &v) in out.iter_mut().zip(q) {
        *s = if v.is_nan() || v < 0.0 {
            bap::NO_DATA
        } else if rule == QaRule::None || rule.usable(v as u32) {
            bap::CLEAR
        } else if rule.is_cloud(v as u32) {
            bap::CLOUD
        } else {
            bap::NO_DATA
        };
    }
}

/// The counters of the pixels read (`done`, `full`) and of the band windows not read (`failed`).
#[derive(Default)]
struct Reads {
    done: AtomicUsize,
    full: AtomicUsize,
    failed: AtomicUsize,
}

/// One block's composite: `out` bands x pixels (NaN none), `inf` the info bands x pixels.
struct Block {
    win: (usize, usize, usize, usize),
    out: Vec<f32>,
    inf: Vec<f32>,
}

/// Composites the series `info` into `out_dir` (`progress` from 0 to 1 with a message).
pub fn composite(
    info: &Arc<CubeInfo>,
    opts: &CompOptions,
    out_dir: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Composited, String> {
    if !info.time_is_date {
        return Err("the series has no dates".into());
    }
    if opts.bands.is_empty() {
        return Err("no band to composite".into());
    }
    if opts.bands.len() > 64 {
        return Err(format!("{} bands: 64 at most", opts.bands.len()));
    }
    if opts.method == Method::Greenest && (opts.red < 1 || opts.nir < 1) {
        return Err("the greenest needs the red and NIR bands".into());
    }
    let px = pixel_metres(info);
    let bopts =
        bap::Options { cloud_min: opts.bap.cloud_min / px, cloud_max: opts.bap.cloud_max / px, ..opts.bap.clone() };
    if opts.method == Method::Bap {
        bopts.check()?;
    }
    let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
    let periods = match opts.step {
        Some(s) => periods(&times, s),
        None => {
            let days: Vec<i64> = times.iter().map(|t| (t / 86400.0).floor() as i64).collect();
            let (a, b) = (days.iter().min().copied().unwrap_or(0), days.iter().max().copied().unwrap_or(0));
            vec![Period { start: a, end: b + 1, dates: (0..times.len()).collect() }]
        }
    };
    if periods.is_empty() {
        return Err("no dates".into());
    }
    let win = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    if w == 0 || h == 0 {
        return Err("an empty window".into());
    }
    let nb = opts.bands.len();
    let t_n = info.t();
    let date_of = |t: usize| bap::Date {
        doy: day_of_year(times[t]),
        sensor: opts.sensor.get(t).copied().unwrap_or(1.0),
        cover: opts.cover.get(t).copied().unwrap_or(1.0),
    };
    // the blocks: a side that keeps every date's bands of a block under 512 MB for the medoid and the median
    let side = if opts.method.picks() {
        opts.block.max(16)
    } else {
        let per_px = (t_n * (nb + 1) * 4).max(1);
        (((512usize << 20) / per_px) as f64).sqrt().clamp(32.0, opts.block.max(32) as f64) as usize
    };
    let part = out_dir.join(".part");
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let co = gdal::raster::RasterCreationOptions::from_iter(["TILED=YES", "BIGTIFF=IF_SAFER", "SPARSE_OK=TRUE"]);
    let gt = info.geo_transform.map(|g| {
        let (x0, y0) = (win[0] as f64, win[1] as f64);
        [g[0] + x0 * g[1] + y0 * g[2], g[1], g[2], g[3] + x0 * g[4] + y0 * g[5], g[4], g[5]]
    });
    // the bands' wavelengths and units as the series has them (a STAC series' VRTs do): the composite opens as a
    // spectrum, in true colour
    let wavelengths: Vec<(String, String)> = info
        .layers
        .first()
        .and_then(|l| gdal::Dataset::open(&l.path).ok())
        .map(|ds| {
            opts.bands
                .iter()
                .filter_map(|&b| ds.rasterband(b as usize).ok())
                .filter_map(|rb| {
                    let w = rb.metadata_item("wavelength", "")?;
                    Some((w, rb.metadata_item("wavelength_units", "").unwrap_or_default()))
                })
                .collect()
        })
        .unwrap_or_default();
    let create = |f: &Path, names: &[&str], nm: &[(String, String)]| -> Result<gdal::Dataset, String> {
        let mut ds = drv
            .create_with_band_type_with_options::<f32, _>(f, w, h, names.len(), &co)
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
            if let Some((w, u)) = nm.get(b) {
                rb.set_metadata_item("wavelength", w, "").map_err(|e| e.to_string())?;
                if !u.is_empty() {
                    rb.set_metadata_item("wavelength_units", u, "").map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(ds)
    };
    let names: Vec<&str> = opts.names.iter().map(String::as_str).collect();
    let mut dss = Vec::new();
    let mut temps = Vec::new();
    let mut infos = Vec::new();
    let mut info_temps = Vec::new();
    for p in &periods {
        let f = part.join(format!("{}_{}.tif", opts.stem, ymd(p.start)));
        // the wavelengths when every band has one
        let nm = if wavelengths.len() == names.len() { &wavelengths[..] } else { &[] };
        dss.push(create(&f, &names, nm)?);
        temps.push(f);
        if opts.info {
            let f = part.join("info").join(format!("{}_{}_info.tif", opts.stem, ymd(p.start)));
            std::fs::create_dir_all(part.join("info")).map_err(|e| e.to_string())?;
            infos.push(create(&f, &INFO_BANDS, &[])?);
            info_temps.push(f);
        }
    }
    let reads = Reads::default();
    let blocks: Vec<(usize, usize)> =
        (0..h).step_by(side).flat_map(|y| (0..w).step_by(side).map(move |x| (x, y))).collect();
    let steps = blocks.len() * periods.len();
    let mut k = 0usize;
    for &(bx, by) in &blocks {
        let bwin = (win[0] + bx, win[1] + by, side.min(w - bx), side.min(h - by));
        for (pi, p) in periods.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let what = format!(
                "block {} of {}{}",
                k / periods.len() + 1,
                blocks.len(),
                if periods.len() > 1 { format!(", period {} of {}", pi + 1, periods.len()) } else { String::new() }
            );
            progress(0.9 * k as f64 / steps as f64, &what);
            k += 1;
            if p.dates.is_empty() {
                continue;
            }
            let dates: Vec<bap::Date> = p.dates.iter().map(|&t| date_of(t)).collect();
            let b = match opts.method {
                Method::Bap => bap_block(info, opts, &bopts, p, &dates, bwin, &reads, cancel)?,
                Method::Greenest => greenest_block(info, opts, p, bwin, &reads, cancel),
                Method::Medoid | Method::Median => typical_block(info, opts, p, bwin, &reads, cancel),
            };
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            let (x, y) = (b.win.0 - win[0], b.win.1 - win[1]);
            let n = b.win.2 * b.win.3;
            let put = |ds: &mut gdal::Dataset, data: &[f32], bands: usize| -> Result<(), String> {
                for band in 0..bands {
                    let mut rb = ds.rasterband(band + 1).map_err(|e| e.to_string())?;
                    let mut buf =
                        gdal::raster::Buffer::new((b.win.2, b.win.3), data[band * n..(band + 1) * n].to_vec());
                    rb.write((x as isize, y as isize), (b.win.2, b.win.3), &mut buf).map_err(|e| e.to_string())?;
                }
                Ok(())
            };
            put(&mut dss[pi], &b.out, nb)?;
            if opts.info {
                put(&mut infos[pi], &b.inf, INFO_BANDS.len())?;
            }
        }
    }
    drop(dss);
    drop(infos);
    let files = to_cogs(&temps, out_dir, opts.threads, &|f| progress(0.9 + 0.08 * f, "writing the COGs"), cancel)?;
    let info_files = if opts.info {
        let d = out_dir.join("info");
        std::fs::create_dir_all(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        to_cogs(&info_temps, &d, opts.threads, &|f| progress(0.98 + 0.02 * f, "writing the info"), cancel)?
    } else {
        Vec::new()
    };
    let _ = std::fs::remove_dir_all(&part);
    let empty = periods.iter().filter(|p| p.dates.is_empty()).count();
    Ok(Composited {
        files,
        info_files,
        periods,
        empty,
        reads: reads.done.into_inner(),
        full_reads: reads.full.into_inner(),
        failed: reads.failed.into_inner(),
    })
}

/// The info bands of a block: each pixel's date (index into the period's dates, -1 none), score, clear count.
fn info_of(info: &CubeInfo, p: &Period, date: &[i32], score: &[f32], clear: &[u16]) -> Vec<f32> {
    let n = date.len();
    let mut inf = vec![f32::NAN; INFO_BANDS.len() * n];
    for i in 0..n {
        inf[3 * n + i] = f32::from(clear[i]);
        if date[i] < 0 {
            continue;
        }
        let t = info.layers[p.dates[date[i] as usize]].time;
        let d = (t / 86400.0).floor() as i64;
        inf[i] = civil_from_days(d).0 as f32;
        inf[n + i] = day_of_year(t) as f32;
        inf[2 * n + i] = score[i];
    }
    inf
}

/// The bands of the winners read and put in place: `date` each pixel's (index into the period's dates, -1 none),
/// `todo` the pixels to fill, `have` the bands (1-based) already in `out`, not read again. The pixels whose winner had
/// a band without a value are returned (left NaN).
#[allow(clippy::too_many_arguments)]
fn fill_winners(
    info: &Arc<CubeInfo>,
    opts: &CompOptions,
    p: &Period,
    win: (usize, usize, usize, usize),
    date: &[i32],
    todo: &[bool],
    have: &[i32],
    out: &mut [f32],
    reads: &Reads,
    cancel: &AtomicBool,
) -> Vec<usize> {
    let n = win.2 * win.3;
    let nb = opts.bands.len();
    let to_read: Vec<usize> = (0..nb).filter(|&b| !have.contains(&opts.bands[b])).collect();
    // each date read over the box of its winners in the block only (one read a band: smaller ones would read the
    // source's tiles again, each thread its own)
    let mut boxes = vec![[usize::MAX, usize::MAX, 0, 0]; p.dates.len()];
    for i in 0..n {
        if todo[i] && date[i] >= 0 {
            let (x, y) = (i % win.2, i / win.2);
            let k = &mut boxes[date[i] as usize];
            *k = [k[0].min(x), k[1].min(y), k[2].max(x + 1), k[3].max(y + 1)];
        }
    }
    let winners: Vec<usize> = (0..p.dates.len()).filter(|&d| boxes[d][2] > 0).collect();
    // a few dates at a time: their bands in memory, then put in place
    let chunk = opts.threads.max(1);
    for ws in winners.chunks(chunk) {
        let jobs: Vec<(usize, usize, [usize; 4])> =
            ws.iter().flat_map(|&d| to_read.iter().map(move |&b| (d, b))).map(|(d, b)| (d, b, boxes[d])).collect();
        let got = par_map(jobs.len(), opts.threads, cancel, |j| {
            let (d, b, k) = jobs[j];
            read(info, p.dates[d], opts.bands[b], (win.0 + k[0], win.1 + k[1], k[2] - k[0], k[3] - k[1]))
        });
        for (j, v) in got.into_iter().enumerate() {
            let (d, b, k) = jobs[j];
            let bw = k[2] - k[0];
            reads.done.fetch_add(bw * (k[3] - k[1]), Ordering::Relaxed);
            let Some(Some(v)) = v else {
                reads.failed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            for y in k[1]..k[3] {
                for x in k[0]..k[2] {
                    let i = y * win.2 + x;
                    if todo[i] && date[i] == d as i32 {
                        out[b * n + i] = v[(y - k[1]) * bw + x - k[0]];
                    }
                }
            }
        }
    }
    (0..n).filter(|&i| todo[i] && date[i] >= 0 && (0..nb).any(|b| out[b * n + i].is_nan())).collect()
}

/// The best available pixel of a block: the quality of every date over the block and the margin of the cloud
/// distance, the winners chosen (zeit_core::bap), their bands read. A pixel whose winner has a band without a value
/// (a scene's edge) goes to its next best date (twice at most).
#[allow(clippy::too_many_arguments)]
fn bap_block(
    info: &Arc<CubeInfo>,
    opts: &CompOptions,
    bopts: &bap::Options,
    p: &Period,
    dates: &[bap::Date],
    win: (usize, usize, usize, usize),
    reads: &Reads,
    cancel: &AtomicBool,
) -> Result<Block, String> {
    let m = if bopts.w_cloud > 0.0 && opts.qa_band > 0 { bopts.margin() } else { 0 };
    // the block and its margin, within the series
    let (hx0, hy0) = (win.0.saturating_sub(m), win.1.saturating_sub(m));
    let (hx1, hy1) = ((win.0 + win.2 + m).min(info.width), (win.1 + win.3 + m).min(info.height));
    let hwin = (hx0, hy0, hx1 - hx0, hy1 - hy0);
    let hn = hwin.2 * hwin.3;
    let nd = p.dates.len();
    let n = win.2 * win.3;
    // the quality of each date (without a quality band: the first band's values)
    let (qband, rule) = if opts.qa_band > 0 { (opts.qa_band, opts.qa_rule) } else { (opts.bands[0], QaRule::None) };
    let qs = par_map(nd, opts.threads, cancel, |d| read(info, p.dates[d], qband, hwin));
    reads.done.fetch_add(nd * hn, Ordering::Relaxed);
    reads.full.fetch_add(nd * (opts.bands.len() + usize::from(opts.qa_band > 0)) * n, Ordering::Relaxed);
    let mut states = vec![bap::NO_DATA; nd * hn];
    for (d, q) in qs.into_iter().enumerate() {
        match q {
            Some(Some(q)) => states_of(&q, rule, &mut states[d * hn..(d + 1) * hn]),
            _ => {
                reads.failed.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
    let at = |i: usize| (win.1 - hy0 + i / win.2) * hwin.2 + (win.0 - hx0 + i % win.2);
    let mut out = vec![f32::NAN; opts.bands.len() * n];
    let mut date = vec![-1i32; n];
    let mut score = vec![f32::NAN; n];
    let mut clear = vec![0u16; n];
    let mut todo = vec![true; n];
    for round in 0..3 {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let best = bap::best(&states, hwin.3, hwin.2, dates, bopts, opts.threads as i32)?;
        let mut round_date = vec![-1i32; n];
        for i in 0..n {
            if !todo[i] {
                continue;
            }
            let j = at(i);
            round_date[i] = best.date[j];
            date[i] = best.date[j];
            score[i] = best.score[j];
            if round == 0 {
                clear[i] = best.clear[j];
            }
        }
        let missing = fill_winners(info, opts, p, win, &round_date, &todo, &[], &mut out, reads, cancel);
        if missing.is_empty() || round == 2 {
            break;
        }
        // their winners left out there, the others kept
        todo.fill(false);
        for &i in &missing {
            states[round_date[i] as usize * hn + at(i)] = bap::NO_DATA;
            todo[i] = true;
            for b in 0..opts.bands.len() {
                out[b * n + i] = f32::NAN;
            }
            date[i] = -1;
            score[i] = f32::NAN;
        }
    }
    // a pixel left without its bands has no date
    for i in 0..n {
        if (0..opts.bands.len()).any(|b| out[b * n + i].is_nan()) {
            date[i] = -1;
            score[i] = f32::NAN;
        }
    }
    let inf = if opts.info { info_of(info, p, &date, &score, &clear) } else { Vec::new() };
    Ok(Block { win, out, inf })
}

/// The greenest pixel of a block: the red, NIR and quality bands of every date, each pixel's clear observation of
/// the highest NDVI (the earlier date of equal ones), the winners' bands read.
fn greenest_block(
    info: &Arc<CubeInfo>,
    opts: &CompOptions,
    p: &Period,
    win: (usize, usize, usize, usize),
    reads: &Reads,
    cancel: &AtomicBool,
) -> Block {
    let n = win.2 * win.3;
    let nd = p.dates.len();
    let mut best = vec![f32::NEG_INFINITY; n];
    let mut date = vec![-1i32; n];
    let mut clear = vec![0u16; n];
    // the winners' red and NIR, kept: not read again
    let (mut best_r, mut best_v) = (vec![f32::NAN; n], vec![f32::NAN; n]);
    let with_qa = opts.qa_band > 0 && opts.qa_rule != QaRule::None;
    let per = 2 + usize::from(with_qa);
    reads.full.fetch_add(nd * (opts.bands.len() + usize::from(opts.qa_band > 0)) * n, Ordering::Relaxed);
    // a few dates at a time, in order: the earlier of equal NDVIs kept
    for ds in (0..nd).collect::<Vec<_>>().chunks(opts.threads.max(1)) {
        let got = par_map(ds.len() * per, opts.threads, cancel, |j| {
            let (d, k) = (ds[j / per], j % per);
            let band = [opts.red, opts.nir, opts.qa_band][k];
            read(info, p.dates[d], band, win)
        });
        reads.done.fetch_add(got.len() * n, Ordering::Relaxed);
        for (o, &d) in ds.iter().enumerate() {
            let (Some(Some(r)), Some(Some(v))) = (&got[o * per], &got[o * per + 1]) else {
                reads.failed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let q = if with_qa { got[o * per + 2].as_ref().and_then(|q| q.as_ref()) } else { None };
            if with_qa && q.is_none() {
                reads.failed.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            for i in 0..n {
                if let Some(q) = q
                    && (q[i].is_nan() || q[i] < 0.0 || !opts.qa_rule.usable(q[i] as u32))
                {
                    continue;
                }
                let s = v[i] + r[i];
                if !s.is_finite() || s == 0.0 {
                    continue;
                }
                let ndvi = (v[i] - r[i]) / s;
                clear[i] += 1;
                if ndvi > best[i] {
                    best[i] = ndvi;
                    date[i] = d as i32;
                    (best_r[i], best_v[i]) = (r[i], v[i]);
                }
            }
        }
    }
    let mut out = vec![f32::NAN; opts.bands.len() * n];
    for (b, &band) in opts.bands.iter().enumerate() {
        let kept = if band == opts.red {
            &best_r
        } else if band == opts.nir {
            &best_v
        } else {
            continue;
        };
        out[b * n..(b + 1) * n].copy_from_slice(kept);
    }
    let todo = vec![true; n];
    let missing = fill_winners(info, opts, p, win, &date, &todo, &[opts.red, opts.nir], &mut out, reads, cancel);
    for i in missing {
        date[i] = -1;
    }
    let score: Vec<f32> = (0..n).map(|i| if date[i] >= 0 { best[i] } else { f32::NAN }).collect();
    let inf = if opts.info { info_of(info, p, &date, &score, &clear) } else { Vec::new() };
    Block { win, out, inf }
}

/// The medoid or the median of a block: every date's bands and quality.
fn typical_block(
    info: &Arc<CubeInfo>,
    opts: &CompOptions,
    p: &Period,
    win: (usize, usize, usize, usize),
    reads: &Reads,
    cancel: &AtomicBool,
) -> Block {
    let n = win.2 * win.3;
    let nd = p.dates.len();
    let nb = opts.bands.len();
    let with_qa = opts.qa_band > 0 && opts.qa_rule != QaRule::None;
    let per = nb + usize::from(with_qa);
    let got = par_map(nd * per, opts.threads, cancel, |j| {
        let (d, k) = (j / per, j % per);
        let band = if k < nb { opts.bands[k] } else { opts.qa_band };
        read(info, p.dates[d], band, win)
    });
    reads.done.fetch_add(got.len() * n, Ordering::Relaxed);
    reads.full.fetch_add(nd * (nb + usize::from(opts.qa_band > 0)) * n, Ordering::Relaxed);
    // dates x bands x pixels, NaN where the quality masks or a band failed
    let mut values = vec![f32::NAN; nd * nb * n];
    for d in 0..nd {
        let q = if with_qa { got[d * per + nb].as_ref().and_then(|q| q.as_ref()) } else { None };
        if with_qa && q.is_none() {
            reads.failed.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        for b in 0..nb {
            let Some(Some(v)) = &got[d * per + b] else {
                reads.failed.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let dst = &mut values[(d * nb + b) * n..(d * nb + b + 1) * n];
            for i in 0..n {
                let ok = q.is_none_or(|q| !q[i].is_nan() && q[i] >= 0.0 && opts.qa_rule.usable(q[i] as u32));
                dst[i] = if ok { v[i] } else { f32::NAN };
            }
        }
    }
    let c = if opts.method == Method::Medoid { Composite::Medoid } else { Composite::Median };
    let mut out = vec![f32::NAN; nb * n];
    let mut clear = vec![0u16; n];
    let (mut obs, mut tmp, mut comp) = (Vec::with_capacity(nd * nb), Vec::new(), vec![0f64; nb]);
    for i in 0..n {
        obs.clear();
        for d in 0..nd {
            let mut all = true;
            for b in 0..nb {
                let v = values[(d * nb + b) * n + i];
                all &= !v.is_nan();
                obs.push(f64::from(v));
            }
            clear[i] += u16::from(all);
        }
        reduce(c, &obs, nb, &[], &mut tmp, &mut comp);
        for b in 0..nb {
            out[b * n + i] = comp[b] as f32;
        }
    }
    let inf = if opts.info {
        let date = vec![-1i32; n];
        let score = vec![f32::NAN; n];
        info_of(info, p, &date, &score, &clear)
    } else {
        Vec::new()
    };
    Block { win, out, inf }
}
