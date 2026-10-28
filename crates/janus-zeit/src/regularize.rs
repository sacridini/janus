//! A series regularized (IDEIAS phase 40): one composite per period of a fixed step (8 or 16 days, a month, a year,
//! any number of days), as zeit.regularize_time_series (the median of each band, or the medoid across the bands)
//! or the clearest date's observation; its empty periods filled (linear between neighbours, or by a Whittaker
//! curve) up to a gap; a COG per regular date in a folder that opens as a series (the file's name gives its date).
//!
//! Done in blocks of rows (every date of a block in memory), its pixels on `threads` threads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Metadata;
use janus_cube::dates::{civil_from_days, days_from_civil};
use janus_cube::{CubeInfo, QaRule, with_thread_reader};

/// The periods' step.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Step {
    /// every n days from the first date's day (pandas' "16D")
    Days(u32),
    /// calendar months ("MS")
    Month,
    /// calendar years ("YS")
    Year,
}

/// What a period's dates give.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Composite {
    /// each band's median of its values
    Median,
    /// the observation (with every band) nearest the bands' medians
    Medoid,
    /// the observation of the clearest date that has one (the dates ranked by the share of their pixels with a value)
    Clearest,
}

/// How the periods without a value are filled.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fill {
    None,
    /// linear in time between the values before and after
    Linear,
    /// by a Whittaker curve of this lambda through the values (zeit_core::whittaker)
    Whittaker(f64),
}

#[derive(Clone, Debug)]
pub struct RegOptions {
    pub step: Step,
    pub composite: Composite,
    pub fill: Fill,
    /// a gap is filled only when its values before and after are at most this many days apart (0: any)
    pub max_gap_days: f64,
    /// every band of the dates (else the quantity shown: the band, index or expression, with the quality mask)
    pub all_bands: bool,
    /// the window (x0, y0, x1, y1) in the series' pixels (None: all of it)
    pub window: Option<[usize; 4]>,
    /// the files' names: `<stem>_<YYYYMMDD>.tif`
    pub stem: String,
    pub threads: usize,
}

/// A regular period: its first day (days since 1970), the day after it, its dates.
#[derive(Clone, Debug, PartialEq)]
pub struct Period {
    pub start: i64,
    pub end: i64,
    pub dates: Vec<usize>,
}

/// Unix seconds as days.
fn day_of(time: f64) -> i64 {
    (time / 86400.0).floor() as i64
}

/// The first day of the period after the one starting at `d`.
fn next_start(d: i64, step: Step) -> i64 {
    match step {
        Step::Days(n) => d + i64::from(n.max(1)),
        Step::Month => {
            let (y, m, _) = civil_from_days(d);
            if m == 12 { days_from_civil(y + 1, 1, 1) } else { days_from_civil(y, m + 1, 1) }
        }
        Step::Year => days_from_civil(civil_from_days(d).0 + 1, 1, 1),
    }
}

/// The regular periods from the first date's to the last's (as pandas' resample: from the first day, the first
/// month or year; the empty ones too), each with its dates.
pub fn periods(times: &[f64], step: Step) -> Vec<Period> {
    let days: Vec<i64> = times.iter().map(|&t| day_of(t)).collect();
    let (Some(&first), Some(&last)) = (days.iter().min(), days.iter().max()) else { return Vec::new() };
    let mut start = match step {
        Step::Days(_) => first,
        Step::Month => {
            let (y, m, _) = civil_from_days(first);
            days_from_civil(y, m, 1)
        }
        Step::Year => days_from_civil(civil_from_days(first).0, 1, 1),
    };
    let mut out = Vec::new();
    while start <= last {
        let end = next_start(start, step);
        let dates = (0..days.len()).filter(|&t| days[t] >= start && days[t] < end).collect();
        out.push(Period { start, end, dates });
        start = end;
    }
    out
}

/// The median of `v` (reordered) as zeit_core's medoid takes it: the upper middle value, or the mean of the two
/// middle ones.
fn median(v: &mut [f64]) -> f64 {
    let (n, even) = (v.len() / 2, v.len().is_multiple_of(2));
    let (lower, &mut hi, _) = v.select_nth_unstable_by(n, f64::total_cmp);
    if even {
        let lo = lower.iter().copied().fold(lower[0], |m, x| if m < x { x } else { m });
        (lo + hi) / 2.0
    } else {
        hi
    }
}

/// One pixel's composite of a period: `obs` its dates' values (dates x bands), `rank` the dates' order for
/// Clearest; into `out` (bands), NaN without a value.
fn composite(c: Composite, obs: &[f64], bands: usize, rank: &[usize], tmp: &mut Vec<f64>, out: &mut [f64]) {
    let n = obs.len() / bands;
    match c {
        Composite::Median => {
            for (b, o) in out.iter_mut().enumerate() {
                tmp.clear();
                tmp.extend((0..n).map(|t| obs[t * bands + b]).filter(|v| !v.is_nan()));
                *o = if tmp.is_empty() { f64::NAN } else { median(tmp) };
            }
        }
        Composite::Medoid => {
            // zeit_core::medoid: each band's median of its values, then the complete observation nearest them (the
            // first of equal ones)
            let mut med = [0f64; 64];
            let med = &mut med[..bands.min(64)];
            let mut any = false;
            for (b, m) in med.iter_mut().enumerate() {
                tmp.clear();
                tmp.extend((0..n).map(|t| obs[t * bands + b]).filter(|v| !v.is_nan()));
                *m = if tmp.is_empty() { f64::NAN } else { median(tmp) };
                any |= !tmp.is_empty();
            }
            out.fill(f64::NAN);
            if !any {
                return;
            }
            let mut best = (f64::MAX, None);
            for (t, o) in obs.chunks_exact(bands).enumerate() {
                if o.iter().any(|v| v.is_nan()) {
                    continue;
                }
                let d: f64 = o.iter().zip(med.iter()).map(|(a, m)| (a - m) * (a - m)).sum();
                if d < best.0 {
                    best = (d, Some(t));
                }
            }
            if let Some(t) = best.1 {
                out.copy_from_slice(&obs[t * bands..(t + 1) * bands]);
            }
        }
        Composite::Clearest => {
            out.fill(f64::NAN);
            if let Some(&t) = rank.iter().find(|&&t| obs[t * bands..(t + 1) * bands].iter().all(|v| !v.is_nan())) {
                out.copy_from_slice(&obs[t * bands..(t + 1) * bands]);
            }
        }
    }
}

/// The gaps of one band's regular series filled (x: the periods' days): linear, or the Whittaker curve, where the
/// values around a gap are at most `max_gap` days apart (0: any). The values kept; the ends not extrapolated.
fn fill(f: Fill, x: &[f64], y: &mut [f64], max_gap: f64, z: &mut Vec<f64>) {
    if f == Fill::None {
        return;
    }
    let valid: Vec<usize> = (0..y.len()).filter(|&i| !y[i].is_nan()).collect();
    if valid.len() < 2 || valid.len() == y.len() {
        return;
    }
    if let Fill::Whittaker(lambda) = f {
        z.resize(y.len(), 0.0);
        let w = vec![1.0; y.len()];
        if zeit_core::whittaker::smooth_into(x, y, &w, lambda, z).is_err() {
            return;
        }
    }
    for k in valid.windows(2) {
        let (a, b) = (k[0], k[1]);
        if b == a + 1 || (max_gap > 0.0 && x[b] - x[a] > max_gap) {
            continue;
        }
        for i in a + 1..b {
            y[i] = match f {
                Fill::Linear => y[a] + (y[b] - y[a]) * (x[i] - x[a]) / (x[b] - x[a]),
                _ => z[i],
            };
        }
    }
}

/// The dates ranked clearest first: the share of their pixels with a value, read at a reduced size.
fn clearest_first(info: &Arc<CubeInfo>, win: [usize; 4]) -> Vec<usize> {
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    let s = (256.0 / w.max(h) as f64).min(1.0);
    let size = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
    let mut buf = vec![0f32; size.0 * size.1];
    let mut share: Vec<(usize, f64)> = (0..info.t())
        .map(|t| {
            let ok = with_thread_reader(info, |r| r.read_window(t, (win[0], win[1], w, h), &mut buf, size));
            let v = if ok { buf.iter().filter(|v| v.is_finite()).count() as f64 / buf.len() as f64 } else { 0.0 };
            (t, v)
        })
        .collect();
    // the clearest first, the earlier of equal ones
    share.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    share.into_iter().map(|s| s.0).collect()
}

/// Reads date `t`'s bands over rows y0..y0+rows of the window into `out` (bands x pixels): the quantity shown, or
/// every band with the quality mask.
fn read_date(info: &Arc<CubeInfo>, t: usize, all: bool, win: (usize, usize, usize, usize), out: &mut [f32]) -> bool {
    let n = win.2 * win.3;
    with_thread_reader(info, |r| {
        if !all {
            return r.read_window(t, win, &mut out[..n], (win.2, win.3));
        }
        for b in 0..info.bands_per_date {
            if !r.read_band_window(t, b as i32 + 1, win, &mut out[b * n..(b + 1) * n], (win.2, win.3)) {
                return false;
            }
        }
        let sel = info.sel;
        if sel.qa_band > 0 && sel.qa_rule != QaRule::None {
            let mut q = vec![0f32; n];
            if !r.read_band_window(t, sel.qa_band, win, &mut q, (win.2, win.3)) {
                return false;
            }
            for (i, &q) in q.iter().enumerate() {
                if q.is_nan() || q < 0.0 || !sel.qa_rule.usable(q as u32) {
                    for b in 0..info.bands_per_date {
                        out[b * n + i] = f32::NAN;
                    }
                }
            }
        }
        true
    })
}

/// The day's text, YYYYMMDD.
fn ymd(d: i64) -> String {
    let (y, m, dd) = civil_from_days(d);
    format!("{y:04}{m:02}{dd:02}")
}

/// What `regularize` wrote.
pub struct Regularized {
    /// the COGs, a regular date each, in order
    pub files: Vec<String>,
    pub periods: Vec<Period>,
    /// periods without any date
    pub empty: usize,
}

/// Regularizes the series `info` into `out_dir` (`progress` from 0 to 1 with a message).
pub fn regularize(
    info: &Arc<CubeInfo>,
    opts: &RegOptions,
    out_dir: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Regularized, String> {
    if !info.time_is_date {
        return Err("the series has no dates".into());
    }
    let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
    let periods = periods(&times, opts.step);
    if periods.is_empty() {
        return Err("no dates".into());
    }
    let win = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    if w == 0 || h == 0 {
        return Err("an empty window".into());
    }
    let bands = if opts.all_bands { info.bands_per_date.max(1) } else { 1 };
    if bands > 64 {
        return Err(format!("{bands} bands: 64 at most"));
    }
    let names: Vec<String> = if opts.all_bands {
        (0..bands).map(|b| info.band_names.get(b).cloned().unwrap_or_else(|| format!("band {}", b + 1))).collect()
    } else {
        vec![info.selection_text()]
    };
    let rank = if opts.composite == Composite::Clearest {
        progress(0.0, "ranking the dates by their clear pixels");
        clearest_first(info, win)
    } else {
        Vec::new()
    };
    let part = out_dir.join(".part");
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    // the temporary GeoTIFFs, a period each, written block by block
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    // uncompressed: read once more by the COGs' writing, which compresses
    let co = gdal::raster::RasterCreationOptions::from_iter(["TILED=YES", "BIGTIFF=IF_SAFER", "SPARSE_OK=TRUE"]);
    let gt = info.geo_transform.map(|g| {
        let (x0, y0) = (win[0] as f64, win[1] as f64);
        [g[0] + x0 * g[1] + y0 * g[2], g[1], g[2], g[3] + x0 * g[4] + y0 * g[5], g[4], g[5]]
    });
    let mut dss = Vec::with_capacity(periods.len());
    let mut temps = Vec::with_capacity(periods.len());
    for p in &periods {
        let f = part.join(format!("{}_{}.tif", opts.stem, ymd(p.start)));
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
    // rows a block: every date's bands of the block in memory, a few hundred MB at most
    let t_n = info.t();
    let per_row = w * t_n * bands * 4 + w * periods.len() * bands * 8;
    let rows = ((512usize << 20) / per_row.max(1)).clamp(1, 256).min(h);
    let x: Vec<f64> = periods.iter().map(|p| p.start as f64).collect();
    let threads = opts.threads.max(1);
    let blocks = h.div_ceil(rows);
    for (k, y0) in (0..h).step_by(rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let r = rows.min(h - y0);
        let n = w * r;
        progress(0.85 * k as f64 / blocks as f64, &format!("rows {} to {} of {h}: reading", y0 + 1, y0 + r));
        // every date's bands of the block, the dates read on the threads
        let mut values = vec![f32::NAN; t_n * bands * n];
        {
            let next = AtomicUsize::new(0);
            let failed = AtomicBool::new(false);
            let chunks: Vec<std::sync::Mutex<&mut [f32]>> =
                values.chunks_mut(bands * n).map(std::sync::Mutex::new).collect();
            std::thread::scope(|s| {
                for _ in 0..threads.min(t_n) {
                    s.spawn(|| {
                        loop {
                            let t = next.fetch_add(1, Ordering::Relaxed);
                            if t >= t_n || cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            let mut c = chunks[t].lock().unwrap();
                            if !read_date(info, t, opts.all_bands, (win[0], win[1] + y0, w, r), &mut c) {
                                // a date not read: no value at it
                                c.fill(f32::NAN);
                                failed.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                }
            });
        }
        progress(0.85 * (k as f64 + 0.5) / blocks as f64, &format!("rows {} to {} of {h}: composites", y0 + 1, y0 + r));
        // each pixel's composites and gaps, the pixels on the threads; out: period x band x pixel
        let np = periods.len();
        let mut out = vec![f32::NAN; np * bands * n];
        {
            let step = n.div_ceil(threads * 4).max(256);
            let next = AtomicUsize::new(0);
            let out_ptr = SendPtr(out.as_mut_ptr());
            std::thread::scope(|s| {
                for _ in 0..threads {
                    s.spawn(|| {
                        let out_ptr = &out_ptr;
                        let mut obs = Vec::new();
                        let mut tmp = Vec::new();
                        let mut prank = Vec::new();
                        let mut comp = vec![0f64; bands];
                        let mut series = vec![vec![f64::NAN; np]; bands];
                        let mut z = Vec::new();
                        loop {
                            let a = next.fetch_add(step, Ordering::Relaxed);
                            if a >= n {
                                break;
                            }
                            for i in a..(a + step).min(n) {
                                for (pi, p) in periods.iter().enumerate() {
                                    obs.clear();
                                    for &t in &p.dates {
                                        for b in 0..bands {
                                            obs.push(f64::from(values[(t * bands + b) * n + i]));
                                        }
                                    }
                                    if opts.composite == Composite::Clearest {
                                        // the period's dates (their place in it) in the order of the ranking
                                        prank.clear();
                                        prank.extend(rank.iter().filter_map(|t| p.dates.iter().position(|d| d == t)));
                                    }
                                    if p.dates.is_empty() {
                                        comp.fill(f64::NAN);
                                    } else {
                                        composite(opts.composite, &obs, bands, &prank, &mut tmp, &mut comp);
                                    }
                                    for b in 0..bands {
                                        series[b][pi] = comp[b];
                                    }
                                }
                                for (b, s) in series.iter_mut().enumerate() {
                                    fill(opts.fill, &x, s, opts.max_gap_days, &mut z);
                                    for (pi, &v) in s.iter().enumerate() {
                                        // SAFETY: each pixel i is written by the one thread that took its chunk
                                        unsafe { *out_ptr.0.add((pi * bands + b) * n + i) = v as f32 };
                                    }
                                }
                            }
                        }
                    });
                }
            });
        }
        progress(0.85 * (k as f64 + 0.8) / blocks as f64, &format!("rows {} to {} of {h}: writing", y0 + 1, y0 + r));
        for (pi, ds) in dss.iter_mut().enumerate() {
            for b in 0..bands {
                let mut rb = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
                let s = &out[(pi * bands + b) * n..(pi * bands + b + 1) * n];
                let mut buf = gdal::raster::Buffer::new((w, r), s.to_vec());
                rb.write((0, y0 as isize), (w, r), &mut buf).map_err(|e| e.to_string())?;
            }
        }
    }
    drop(dss);
    // the temporary files as cloud-optimized GeoTIFFs, a few at once
    let files = to_cogs(&temps, out_dir, &|f| progress(0.85 + 0.15 * f, "writing the COGs"), cancel)?;
    let _ = std::fs::remove_dir_all(&part);
    let empty = periods.iter().filter(|p| p.dates.is_empty()).count();
    Ok(Regularized { files, periods, empty })
}

/// A pointer written by several threads at places none shares.
struct SendPtr(*mut f32);
// SAFETY: the threads write disjoint pixels.
unsafe impl Sync for SendPtr {}

/// Files made cloud-optimized GeoTIFFs in `out` (4 at once, 2 threads each: 8 at most).
fn to_cogs(
    files: &[PathBuf],
    out: &Path,
    progress: &(dyn Fn(f64) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let results: Vec<std::sync::Mutex<Option<Result<String, String>>>> =
        files.iter().map(|_| std::sync::Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..4.min(files.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let src = &files[i];
                    let dst = out.join(src.file_name().unwrap_or_default());
                    let r = (|| {
                        let ds = gdal::Dataset::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
                        let drv = gdal::DriverManager::get_driver_by_name("COG").map_err(|e| e.to_string())?;
                        let co = gdal::raster::RasterCreationOptions::from_iter([
                            "COMPRESS=DEFLATE",
                            "PREDICTOR=YES",
                            "BIGTIFF=IF_SAFER",
                            "NUM_THREADS=2",
                            "OVERVIEW_RESAMPLING=NEAREST",
                        ]);
                        ds.create_copy(&drv, &dst, &co).map_err(|e| format!("{}: {e}", dst.display()))?;
                        Ok(dst.to_string_lossy().into_owned())
                    })();
                    *results[i].lock().unwrap() = Some(r);
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(d as f64 / files.len() as f64);
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    results.into_iter().map(|r| r.into_inner().unwrap().unwrap_or_else(|| Err("not written".into()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> f64 {
        days_from_civil(y, m, d) as f64 * 86400.0
    }

    #[test]
    fn periods_as_pandas() {
        let t = [day(2020, 1, 3), day(2020, 1, 20), day(2020, 2, 25), day(2020, 4, 2)];
        // 16 days from 2020-01-03: [3, 19), [19, 35)... the last holding 2020-04-02
        let p = periods(&t, Step::Days(16));
        assert_eq!(p[0].start, days_from_civil(2020, 1, 3));
        assert_eq!(p[0].dates, vec![0]);
        assert_eq!(p[1].dates, vec![1]);
        assert!(p.last().unwrap().dates == vec![3]);
        assert_eq!(p.len(), 6);
        // months: January to April, March empty
        let m = periods(&t, Step::Month);
        assert_eq!(m.iter().map(|p| p.dates.len()).collect::<Vec<_>>(), vec![2, 1, 0, 1]);
        assert_eq!(m[2].start, days_from_civil(2020, 3, 1));
        assert_eq!(periods(&t, Step::Year).len(), 1);
    }

    #[test]
    fn composites() {
        let mut tmp = Vec::new();
        let mut out = [0.0; 2];
        // 3 dates of 2 bands, a value missing at date 0
        let obs = [f64::NAN, 0.0, 1.0, 1.1, 5.0, 5.0];
        composite(Composite::Median, &obs, 2, &[], &mut tmp, &mut out);
        assert_eq!(out, [3.0, 1.1]);
        composite(Composite::Medoid, &obs, 2, &[], &mut tmp, &mut out);
        // medians (3, 1.1): date 1 at 4 + 0, date 2 at 4 + 15.2
        assert_eq!(out, [1.0, 1.1]);
        composite(Composite::Clearest, &obs, 2, &[0, 2, 1], &mut tmp, &mut out);
        assert_eq!(out, [5.0, 5.0], "date 0 incomplete, date 2 next");
    }

    #[test]
    fn gaps() {
        let x = [0.0, 10.0, 20.0, 30.0, 40.0, 50.0];
        let mut z = Vec::new();
        let mut y = [f64::NAN, 1.0, f64::NAN, f64::NAN, 4.0, f64::NAN];
        fill(Fill::Linear, &x, &mut y, 0.0, &mut z);
        assert_eq!((y[1], y[2], y[3], y[4]), (1.0, 2.0, 3.0, 4.0));
        assert!(y[0].is_nan() && y[5].is_nan(), "the ends not extrapolated");
        // a gap of 30 days left over a limit of 20
        let mut y = [1.0, f64::NAN, f64::NAN, 4.0, 5.0, 6.0];
        fill(Fill::Linear, &x, &mut y, 20.0, &mut z);
        assert!(y[1].is_nan() && y[2].is_nan());
        // Whittaker: the values kept, the gap from the curve
        let mut y = [1.0, 2.0, f64::NAN, 4.0, 5.0, 6.0];
        fill(Fill::Whittaker(1.0), &x, &mut y, 0.0, &mut z);
        assert_eq!((y[0], y[1], y[3]), (1.0, 2.0, 4.0));
        assert!((y[2] - 3.0).abs() < 0.2, "{}", y[2]);
    }
}
