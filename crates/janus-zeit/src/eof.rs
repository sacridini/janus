//! Temporal EOF (Tools > Temporal dynamics > EOF / principal components), as Zeit's `eof`: the maps and the time series
//! (principal components) that explain most of a window's variability. The seasonal cycle is taken out of every pixel
//! (anomalies), the pixels are weighted by their area, and the cube (dates x pixels) is decomposed through zeit-core's
//! `eof` (the T x T Gram matrix, or a randomized range finder with power iterations), which reads it in blocks of
//! pixels, as often as the solver needs, and then once more for the maps.
//!
//! The window is read once, in strips of rows; each strip's anomalies are made (they are pixel-local) and the kept
//! pixels' series are cached as float32, in memory when they take at most a quarter of the machine's RAM (and 4 GB), else
//! in a scratch file of the run's folder, read back block by block and deleted at the end. A pixel is kept when it is
//! valid at the fraction `min_valid` of the dates; a date is part of the fit when half the kept pixels are valid at it
//! (the others get their principal components by Everson and Sirovich's gappy projection). Weights, scaling, signs,
//! North's test and the varimax rotation are `zeit.eof`'s.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use serde_json::{Value, json};
use zeit_core::anomaly::{AnomalySettings, Seasonal, anomalies_cube, baseline};
use zeit_core::eof::{self, BLOCK_PIXELS, Blocks, Decomposition, Settings as CoreSettings, Solver};

use crate::areas::PixelAreas;
use crate::job::{
    ChunkOut, Estimate, Inputs, JobResult, JobSpec, Outputs, active_outputs, date_labels, for_strips, rows_per_chunk,
};
use crate::manifest::Tool;
use crate::pixel::Params;

/// The fraction of the kept pixels valid at a date for it to be part of the fit.
const MIN_DATE_VALID: f64 = 0.5;

/// What a run decides, from the tool's parameters.
#[derive(Clone, Debug)]
pub struct Settings {
    /// modes computed
    pub modes: usize,
    pub anomaly: AnomalySettings,
    pub weights: Weights,
    /// the maps in data units per standard deviation of the PC (unit-variance PCs), else orthonormal EOFs
    pub units: bool,
    pub solver: Solver,
    pub seed: u64,
    /// modes rotated by varimax (0 or at least 2)
    pub rotate: usize,
    pub kaiser: bool,
    /// North's n*: the lag-1 autocorrelation of each PC, else the number of dates
    pub autocorrelation: bool,
    pub min_valid: f64,
    pub corr_maps: bool,
}

/// How the pixels are weighted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weights {
    Area,
    CosLat,
    None,
}

/// The anomalies' settings of the parameters (`anomaly`: none is the pixel's mean, month the monthly climatology,
/// harmonic the fit of `harmonics` sine and cosine pairs).
pub fn anomaly_settings(p: &Params) -> AnomalySettings {
    let seasonal = match p.str("anomaly") {
        "none" | "mean" => Seasonal::Mean,
        "harmonic" => Seasonal::Harmonic { order: p.int("harmonics").max(1) as usize },
        _ => Seasonal::Monthly { min_per_month: 2 },
    };
    AnomalySettings { seasonal, base: None, detrend: false, standardize: false }
}

pub fn settings(p: &Params) -> Result<Settings, String> {
    let modes = p.int("n_modes");
    if modes < 1 {
        return Err(format!("ValueError: n_modes must be at least 1, got {modes}"));
    }
    let solver = match p.str("solver") {
        "exact" => Solver::Exact,
        "randomized" => Solver::Randomized {
            oversampling: p.int("oversampling").max(0) as usize,
            power_iterations: p.int("power_iterations").max(0) as usize,
        },
        _ => Solver::Auto,
    };
    let weights = match p.str("weights") {
        "none" => Weights::None,
        "coslat" => Weights::CosLat,
        _ => Weights::Area,
    };
    let min_valid = p.f64("min_valid");
    if !(0.0..=1.0).contains(&min_valid) {
        return Err(format!("ValueError: min_valid must be in [0, 1], got {min_valid}"));
    }
    let rotate = p.int("rotate").max(0) as usize;
    if rotate == 1 {
        return Err("ValueError: rotate must be 0 or at least 2 modes".into());
    }
    Ok(Settings {
        modes: modes as usize,
        anomaly: anomaly_settings(p),
        weights,
        units: p.str("scaling") != "none",
        solver,
        seed: p.int("seed").max(0) as u64,
        rotate,
        kaiser: p.bool("kaiser"),
        autocorrelation: p.str("north_n") != "dates",
        min_valid,
        corr_maps: p.bool("correlation_maps"),
    })
}

/// A pixel's series with the mode's pattern and component put back on its seasonal cycle: what the chart draws over
/// the pixel's values, `baseline + loading * pc` at every date (the anomalies' baseline is what `anomaly` removed; the
/// pixel's remaining mean over the fitted dates is not added back).
pub fn pixel_overlay(values: &[f64], days: &[i64], settings: &AnomalySettings, loading: f64, pc: &[f64]) -> Vec<f64> {
    let base = baseline(values, days, settings);
    base.iter().zip(pc).map(|(b, z)| b + loading * z).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The cache of the kept pixels' anomalies
// ---------------------------------------------------------------------------------------------------------------

/// Where the kept pixels' anomalies are (float32, `T` values a pixel, the pixels one after the other).
enum Store {
    Memory(Vec<f32>),
    File { path: PathBuf, writer: Option<BufWriter<File>>, reader: Option<File> },
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(f, buf, offset)
}

#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(f, buf, offset)
}

impl Store {
    fn push(&mut self, values: &[f32]) -> Result<(), String> {
        match self {
            Store::Memory(v) => v.extend_from_slice(values),
            Store::File { writer, path, .. } => {
                let w = writer.as_mut().ok_or("the cache is already closed")?;
                let mut bytes = Vec::with_capacity(values.len() * 4);
                values.iter().for_each(|v| bytes.extend_from_slice(&v.to_le_bytes()));
                w.write_all(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        Ok(())
    }

    /// Ends the writing: a file is flushed and opened for reading.
    fn finish(&mut self) -> Result<(), String> {
        if let Store::File { path, writer, reader } = self {
            if let Some(mut w) = writer.take() {
                w.flush().map_err(|e| format!("{}: {e}", path.display()))?;
            }
            *reader = Some(File::open(&*path).map_err(|e| format!("{}: {e}", path.display()))?);
        }
        Ok(())
    }

    /// The values of `count` pixels from the `first`-th, `t` each.
    fn with<R>(&self, first: usize, count: usize, t: usize, f: impl FnOnce(&[f32]) -> R) -> Result<R, String> {
        match self {
            Store::Memory(v) => Ok(f(&v[first * t..(first + count) * t])),
            Store::File { path, reader, .. } => {
                let file = reader.as_ref().ok_or("the cache is not open for reading")?;
                let mut bytes = vec![0u8; count * t * 4];
                let mut done = 0;
                while done < bytes.len() {
                    let n = read_at(file, &mut bytes[done..], (first * t * 4 + done) as u64)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if n == 0 {
                        return Err(format!("{}: the cache is shorter than it was written", path.display()));
                    }
                    done += n;
                }
                let values: Vec<f32> = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
                Ok(f(&values))
            }
        }
    }
}

/// The cube of the kept pixels as zeit-core's blocks: each block's pixels' weighted anomalies over the dates of the
/// fit, centred, the gaps 0.
struct Cube {
    /// all the dates
    t: usize,
    /// the dates of the fit
    dates: Vec<usize>,
    /// the window's pixel of each kept pixel
    pixels: Vec<u32>,
    /// the square roots of the weights (mean 1) of the kept pixels
    sqrt_w: Vec<f64>,
    store: Store,
    /// the first error of a read (a block cannot fail in the trait)
    error: Mutex<Option<String>>,
}

impl Cube {
    fn kept(&self) -> usize {
        self.pixels.len()
    }

    fn fail(&self, e: String) {
        let mut slot = self.error.lock().unwrap_or_else(|e| e.into_inner());
        slot.get_or_insert(e);
    }

    fn check(&self) -> Result<(), String> {
        match self.error.lock().unwrap_or_else(|e| e.into_inner()).take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// The mean of a pixel's values over the dates of the fit (0 without any).
    fn mean(&self, col: &[f32]) -> f64 {
        let (mut s, mut n) = (0.0, 0usize);
        for &d in &self.dates {
            let v = col[d];
            if v.is_finite() {
                s += f64::from(v);
                n += 1;
            }
        }
        if n > 0 { s / n as f64 } else { 0.0 }
    }

    /// The weighted, centred values of the `rows` (dates of the cube, a date left out of the fit too) over the kept
    /// pixels, NaN where missing: the input of the gappy projection.
    fn weighted_rows(&self, rows: &[usize]) -> Result<Vec<Vec<f64>>, String> {
        let nk = self.kept();
        let mut out = vec![vec![f64::NAN; nk]; rows.len()];
        for b in 0..nk.div_ceil(BLOCK_PIXELS) {
            let (start, end) = (b * BLOCK_PIXELS, ((b + 1) * BLOCK_PIXELS).min(nk));
            self.store.with(start, end - start, self.t, |raw| {
                for (j, col) in raw.chunks(self.t).enumerate() {
                    let mean = self.mean(col);
                    for (r, &d) in rows.iter().enumerate() {
                        if col[d].is_finite() {
                            out[r][start + j] = (f64::from(col[d]) - mean) * self.sqrt_w[start + j];
                        }
                    }
                }
            })?;
        }
        Ok(out)
    }
}

impl Blocks for Cube {
    fn dates(&self) -> usize {
        self.dates.len()
    }

    fn block_count(&self) -> usize {
        self.kept().div_ceil(BLOCK_PIXELS)
    }

    fn block(&self, b: usize, out: &mut Vec<f64>) -> Range<usize> {
        let (start, end) = (b * BLOCK_PIXELS, ((b + 1) * BLOCK_PIXELS).min(self.kept()));
        let tk = self.dates.len();
        out.clear();
        out.resize(tk * (end - start), 0.0);
        let filled = self.store.with(start, end - start, self.t, |raw| {
            for (j, col) in raw.chunks(self.t).enumerate() {
                let mean = self.mean(col);
                for (i, &d) in self.dates.iter().enumerate() {
                    if col[d].is_finite() {
                        out[j * tk + i] = (f64::from(col[d]) - mean) * self.sqrt_w[start + j];
                    }
                }
            }
        });
        if let Err(e) = filled {
            self.fail(e);
        }
        start..end
    }

    fn sqrt_weights(&self, range: Range<usize>, out: &mut Vec<f64>) {
        out.clear();
        out.extend_from_slice(&self.sqrt_w[range]);
    }
}

/// The machine's physical memory (bytes), where it can be told.
#[cfg(windows)]
fn total_ram() -> Option<u64> {
    #[repr(C)]
    struct MemoryStatusEx {
        length: u32,
        load: u32,
        total_phys: u64,
        avail_phys: u64,
        total_page: u64,
        avail_page: u64,
        total_virtual: u64,
        avail_virtual: u64,
        avail_extended: u64,
    }
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
    }
    let mut m = MemoryStatusEx {
        length: std::mem::size_of::<MemoryStatusEx>() as u32,
        load: 0,
        total_phys: 0,
        avail_phys: 0,
        total_page: 0,
        avail_page: 0,
        total_virtual: 0,
        avail_virtual: 0,
        avail_extended: 0,
    };
    // SAFETY: `m` is a MEMORYSTATUSEX with its length set, alive through the call.
    (unsafe { GlobalMemoryStatusEx(&mut m) } != 0).then_some(m.total_phys)
}

/// The machine's physical memory (bytes), where it can be told.
#[cfg(not(windows))]
fn total_ram() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kb: u64 = text.lines().find_map(|l| l.strip_prefix("MemTotal:"))?.split_whitespace().next()?.parse().ok()?;
    Some(kb * 1024)
}

/// The most the cache may take in memory: a quarter of the RAM, at most 4 GB (2 GB where the RAM is not known).
pub fn memory_limit() -> u64 {
    const GB: u64 = 1 << 30;
    total_ram().map_or(2 * GB, |r| (r / 4).min(4 * GB))
}

// ---------------------------------------------------------------------------------------------------------------
// The weights
// ---------------------------------------------------------------------------------------------------------------

/// The weight of each pixel of the window, row by row (before the normalization to mean 1).
struct PixelWeights {
    kind: Weights,
    areas: Option<PixelAreas>,
    /// the geotransform (the window's rows' latitudes, for cos(latitude))
    gt: [f64; 6],
    x0: usize,
    y0: usize,
    /// the name written down: what was applied
    applied: &'static str,
}

impl PixelWeights {
    fn new(src: &gdal::Dataset, window: [usize; 4], kind: Weights) -> Result<PixelWeights, String> {
        use gdal::spatial_ref::SpatialRef;
        let [x0, y0, x1, y1] = window;
        let wkt = src.projection();
        let gt = src.geo_transform().unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, -1.0]);
        let none = |kind| PixelWeights { kind, areas: None, gt, x0, y0, applied: "none" };
        if kind == Weights::None || wkt.is_empty() {
            // without a CRS the pixels count the same, as Zeit does
            return Ok(none(Weights::None));
        }
        if kind == Weights::CosLat {
            let geographic = SpatialRef::from_wkt(&wkt).map(|s| s.is_geographic()).unwrap_or(false);
            if !geographic {
                return Err("ValueError: weights='coslat' is for a geographic CRS (degrees); use 'area' for a \
                            projected one"
                    .into());
            }
            return Ok(PixelWeights { kind, applied: "coslat", ..none(kind) });
        }
        let areas = PixelAreas::new(&gt, &wkt, x0, y0, x1 - x0, y1 - y0);
        Ok(PixelWeights { kind, areas: Some(areas), applied: "area", ..none(kind) })
    }

    /// The weights of the window's row `y` (0 is the window's first row).
    fn row(&self, y: usize, out: &mut [f64]) {
        match (self.kind, &self.areas) {
            (Weights::Area, Some(a)) => a.row(y, out),
            (Weights::CosLat, _) => {
                for (x, o) in out.iter_mut().enumerate() {
                    let lat = self.gt[3]
                        + (self.x0 + x) as f64 * self.gt[4]
                        + (self.y0 + y) as f64 * self.gt[5]
                        + 0.5 * (self.gt[4] + self.gt[5]);
                    *o = lat.to_radians().cos().max(0.0);
                }
            }
            _ => out.fill(1.0),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------------------------------------------

/// The principal components of a run: every date's (NaN where there is none), what filled them and the rotated ones.
struct Components {
    /// modes x dates, in the scaling asked
    pcs: Vec<f64>,
    /// modes x dates, unit variance
    unit: Vec<f64>,
    filled: Vec<bool>,
}

/// The sign rule of `eof::fix_signs` for one unit-variance PC over the dates of the fit.
fn flip_needed(pc: &[f64], weighted_sum: &[f64], total: f64) -> bool {
    let norm_m = weighted_sum.iter().map(|v| v * v).sum::<f64>().sqrt();
    let noise = norm_m <= 1e-10 * total.max(0.0).sqrt();
    let norm_pc = pc.iter().map(|v| v * v).sum::<f64>().sqrt();
    let dot: f64 = pc.iter().zip(weighted_sum).map(|(a, b)| a * b).sum();
    if noise || dot.abs() < 1e-6 * norm_m * norm_pc {
        pc.iter().fold(0.0f64, |m, &v| if v.abs() > m.abs() { v } else { m }) < 0.0
    } else {
        dot < 0.0
    }
}

/// One output of the run: a mode's pattern, its correlation map or a rotated pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Eof(usize),
    Corr(usize),
    Rotated(usize),
}

fn kind_of(id: &str) -> Option<Kind> {
    let num = |prefix: &str| id.strip_prefix(prefix).and_then(|k| k.parse::<usize>().ok()).filter(|&k| k >= 1);
    num("eof_rotated_")
        .map(Kind::Rotated)
        .or_else(|| num("corrmap_").map(Kind::Corr))
        .or_else(|| num("eof_").map(Kind::Eof))
}

/// The dates as days since 1970-01-01: the series' real dates, or (for anomalies that need none) from its decimal
/// years.
fn days_of(spec: &JobSpec, s: &Settings) -> Result<Vec<i64>, String> {
    let t = spec.years.len();
    let days: Vec<i64> = match &spec.days {
        Some(d) if d.len() == t => d.clone(),
        _ if matches!(s.anomaly.seasonal, Seasonal::Mean) => {
            spec.years.iter().map(|&y| crate::numeric::decimal_year_days(y) as i64).collect()
        }
        _ => {
            return Err("ValueError: the monthly and harmonic anomalies need a series with real dates (choose the \
                        anomaly 'none' for one without)"
                .into());
        }
    };
    if days.windows(2).any(|w| w[1] < w[0]) {
        return Err("ValueError: the dates of the series must be in ascending order".into());
    }
    Ok(days)
}

pub(crate) fn run(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<JobResult, String> {
    run_with_limit(spec, tool, p, progress, cancel, memory_limit())
}

/// `run` with the most bytes the cache may take in memory (above it, the kept pixels' anomalies go to a scratch file in
/// the run's folder).
pub fn run_with_limit(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
    limit: u64,
) -> Result<JobResult, String> {
    let s = settings(p)?;
    let days = days_of(spec, &s)?;
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let window = spec.window.unwrap_or([0, 0, iw, ih]);
    let [x0, y0, x1, y1] = window;
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    if t < 4 {
        return Err(format!("ValueError: an EOF needs at least 4 dates, the series has {t}"));
    }
    if n == 0 || n > u32::MAX as usize {
        return Err(format!("ValueError: the window ({w} x {h} pixels) has no pixel or too many for an EOF"));
    }
    let dir = PathBuf::from(&spec.output_dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let weights = PixelWeights::new(inputs.input.ds(), window, s.weights)?;
    let in_memory = 4 * t as u64 * n as u64 <= limit;
    let scratch = dir.join("eof_cache.bin");
    let mut store = if in_memory {
        Store::Memory(Vec::with_capacity(t * n))
    } else {
        let f = File::create(&scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
        Store::File { path: scratch.clone(), writer: Some(BufWriter::with_capacity(1 << 20, f)), reader: None }
    };
    let result = run_cached(spec, tool, p, progress, cancel, &s, &days, &mut inputs, &weights, &mut store, in_memory);
    drop(store);
    if !in_memory {
        let _ = std::fs::remove_file(&scratch);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_cached(
    spec: &JobSpec,
    tool: &Tool,
    p: &Params,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
    s: &Settings,
    days: &[i64],
    inputs: &mut Inputs,
    weights: &PixelWeights,
    store: &mut Store,
    in_memory: bool,
) -> Result<JobResult, String> {
    let [x0, y0, x1, y1] = spec.window.unwrap_or_else(|| {
        let (iw, ih) = inputs.input.ds().raster_size();
        [0, 0, iw, ih]
    });
    let window = [x0, y0, x1, y1];
    let (w, h) = (x1 - x0, y1 - y0);
    let (t, n) = (spec.years.len(), w * h);
    let n_jobs = spec.n_jobs();
    let rows = rows_per_chunk(tool, w);

    // ---- the window read once: anomalies, the pixels that are kept, their weights and the valid pixels of each date
    let mut pixels: Vec<u32> = Vec::new();
    let mut raw_w: Vec<f64> = Vec::new();
    let mut date_valid = vec![0usize; t];
    let (mut sum_w_valid, mut n_valid, mut sum_w_all) = (0.0, 0usize, 0.0);
    let need = s.min_valid * t as f64;
    for_strips(inputs, window, rows, cancel, |r, hh, chunk| {
        let np = chunk.pixels();
        let mut vals = chunk.stack;
        anomalies_cube(&mut vals, t, np, days, &s.anomaly, n_jobs);
        let mut count = vec![0usize; np];
        for row in vals.chunks(np) {
            for (c, v) in count.iter_mut().zip(row) {
                *c += usize::from(v.is_finite());
            }
        }
        let mut wrow = vec![0.0; w];
        let mut column = Vec::with_capacity(t);
        for y in 0..hh {
            weights.row(r + y, &mut wrow);
            for (x, &wp) in wrow.iter().enumerate() {
                let j = y * w + x;
                sum_w_all += wp;
                if count[j] > 0 {
                    sum_w_valid += wp;
                    n_valid += 1;
                }
                if count[j] == 0 || (count[j] as f64) < need {
                    continue;
                }
                column.clear();
                for (i, row) in vals.chunks(np).enumerate() {
                    let v = row[j];
                    date_valid[i] += usize::from(v.is_finite());
                    column.push(v as f32);
                }
                store.push(&column)?;
                pixels.push(((r + y) * w + x) as u32);
                raw_w.push(wp);
            }
        }
        progress(0.3 * (r + hh) as f64 / h as f64, &format!("reading rows {}/{h}", r + hh));
        Ok(())
    })?;
    store.finish()?;
    let nk = pixels.len();
    if nk == 0 {
        return Err("ValueError: no pixel has enough valid dates (lower the minimum valid fraction)".into());
    }
    let mean_w = if n_valid > 0 && sum_w_valid > 0.0 { sum_w_valid / n_valid as f64 } else { sum_w_all / n as f64 };
    let sqrt_w: Vec<f64> = raw_w.iter().map(|&v| (v / mean_w).sqrt()).collect();
    drop(raw_w);
    if sqrt_w.iter().any(|v| !v.is_finite()) {
        return Err("ValueError: the weights must be finite and not negative".into());
    }
    let dates: Vec<usize> =
        (0..t).filter(|&i| date_valid[i] > 0 && date_valid[i] as f64 >= MIN_DATE_VALID * nk as f64).collect();
    if dates.len() < 3 {
        return Err("ValueError: fewer than 3 dates are valid at enough of the pixels".into());
    }
    let tk = dates.len();
    let cube = Cube {
        t,
        dates,
        pixels,
        sqrt_w,
        store: std::mem::replace(store, Store::Memory(Vec::new())),
        error: Mutex::new(None),
    };
    progress(0.3, &format!("{nk} pixels and {tk} dates in the fit"));

    // ---- the decomposition
    let core = CoreSettings { modes: s.modes, solver: s.solver, seed: s.seed };
    let mut d = eof::decompose(&cube, &core, n_jobs, &|f| progress(0.3 + 0.3 * f, "the decomposition"))?;
    cube.check()?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    eof::fix_signs(&mut d);
    let m = d.modes();
    if s.rotate > m {
        return Err(format!("ValueError: rotate must be 0 or between 2 and the {m} modes computed, got {}", s.rotate));
    }
    let f = ((tk.max(2) - 1) as f64).sqrt();

    // ---- the maps: V of every kept pixel (and the correlation of the first modes' pixels with their PCs)
    let nmaps = if s.corr_maps { (p.int("maps").max(0) as usize).min(m) } else { 0 };
    let v_all = Mutex::new(vec![0.0f64; nk * m]);
    let corr_all = Mutex::new(vec![f32::NAN; nk * nmaps]);
    let next = AtomicUsize::new(0);
    let nb = cube.block_count();
    let threads = zeit_core::parallel::threads(n_jobs).max(1).min(nb);
    let work = || {
        let (mut buf, mut v) = (Vec::new(), Vec::new());
        loop {
            let b = next.fetch_add(1, Ordering::Relaxed);
            if b >= nb || cancel.load(Ordering::Relaxed) {
                break;
            }
            let range = cube.block(b, &mut buf);
            let cols = range.len();
            v.clear();
            v.resize(cols * m, 0.0);
            eof::loadings(&d, &buf, cols, &mut v);
            let mut vg = v_all.lock().unwrap_or_else(|e| e.into_inner());
            for k in 0..m {
                vg[k * nk + range.start..k * nk + range.end].copy_from_slice(&v[k * cols..(k + 1) * cols]);
            }
            drop(vg);
            if nmaps > 0 {
                let norm: Vec<f64> = buf.chunks(tk).map(|c| c.iter().map(|x| x * x).sum::<f64>().sqrt()).collect();
                let mut cg = corr_all.lock().unwrap_or_else(|e| e.into_inner());
                for k in 0..nmaps {
                    for j in 0..cols {
                        if norm[j] > 0.0 {
                            cg[k * nk + range.start + j] = (v[k * cols + j] * d.singular[k] / norm[j]) as f32;
                        }
                    }
                }
            }
            progress(0.6 + 0.15 * (b + 1) as f64 / nb as f64, "the maps");
        }
    };
    if threads <= 1 {
        work();
    } else {
        std::thread::scope(|sc| {
            for _ in 0..threads {
                sc.spawn(work);
            }
        });
    }
    cube.check()?;
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let v = v_all.into_inner().unwrap_or_else(|e| e.into_inner());
    let corr = corr_all.into_inner().unwrap_or_else(|e| e.into_inner());

    // ---- the components of every date: the fit's, and the gappy projection for the dates left out
    progress(0.75, "the components of every date");
    let comps = components(&cube, &d, &v, s.units, f, tk)?;
    let unit_all = &comps.unit;
    let eigen = d.eigenvalues();
    let unit_kept = d.pcs(true);
    let n_star: Vec<f64> = (0..m)
        .map(|k| {
            if s.autocorrelation {
                eof::effective_dates(tk, eof::lag1(&unit_kept[k * tk..(k + 1) * tk])).max(1e-9)
            } else {
                tk as f64
            }
        })
        .collect();
    let next_eigen = d.next_singular * d.next_singular / (tk.max(2) - 1) as f64;
    let north = eof::north(&eigen, &n_star, next_eigen);
    let total_var = d.total / (tk.max(2) - 1) as f64;

    // ---- the varimax rotation
    let rotated = if s.rotate >= 2 { Some(rotation(&cube, &d, &v, unit_all, s, f, total_var)) } else { None };

    // ---- the maps written, strip by strip
    let outs_wanted: Vec<_> = active_outputs(tool, p)
        .into_iter()
        .filter(|o| match kind_of(&o.id) {
            Some(Kind::Eof(k)) | Some(Kind::Corr(k)) => k <= m,
            Some(Kind::Rotated(k)) => rotated.is_some() && k <= s.rotate,
            None => false,
        })
        .collect();
    let mut outs = Outputs::new(tool, &outs_wanted, &dir_of(spec), inputs.input.ds(), window, spec)?;
    let scale: Vec<f64> = d.singular.iter().map(|s| s / f).collect();
    let mut r = 0;
    while r < h {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let hh = rows.min(h - r);
        let lo = cube.pixels.partition_point(|&q| (q as usize) < r * w);
        let hi = cube.pixels.partition_point(|&q| (q as usize) < (r + hh) * w);
        let mut res = ChunkOut::new();
        for o in &outs_wanted {
            let mut map = vec![f64::NAN; w * hh];
            let kind = kind_of(&o.id).ok_or_else(|| format!("{}: no such output", o.id))?;
            for pi in lo..hi {
                let at = cube.pixels[pi] as usize - r * w;
                map[at] = match kind {
                    Kind::Eof(k) => {
                        let val = v[(k - 1) * nk + pi];
                        if !s.units {
                            val
                        } else if cube.sqrt_w[pi] > 0.0 {
                            val * scale[k - 1] / cube.sqrt_w[pi]
                        } else {
                            f64::NAN
                        }
                    }
                    Kind::Corr(k) => f64::from(corr[(k - 1) * nk + pi]),
                    Kind::Rotated(k) => rotated.as_ref().map_or(f64::NAN, |q| q.maps[(k - 1) * nk + pi]),
                };
            }
            res.insert(o.id.clone(), map);
        }
        outs.write(&res, r, hh)?;
        r += hh;
        progress(0.8 + 0.2 * r as f64 / h as f64, &format!("writing rows {r}/{h}"));
    }

    // ---- what the app shows: the amplitude of each mode, its variance and North's test; the files of the run
    let explained = d.explained();
    let labels = date_labels(spec);
    let dir = dir_of(spec);
    let amplitude = |series: &[f64]| json!({ "x": spec.years, "y": series });
    let mut extra = serde_json::Map::new();
    for o in &outs_wanted {
        let Some(kind) = kind_of(&o.id) else { continue };
        let info = match kind {
            Kind::Eof(k) => {
                let i = k - 1;
                json!({
                    "mode": k,
                    "name": format!("EOF {k}: {:.1} % of the variance", 100.0 * explained[i]),
                    "variance": explained[i],
                    "eigenvalue": eigen[i],
                    "north_error": north[i].0 / total_var,
                    "separable": north[i].1,
                    "amplitude": amplitude(&comps.pcs[i * t..(i + 1) * t]),
                })
            }
            Kind::Corr(k) => json!({
                "mode": k,
                "name": format!("Correlation with PC {k}: {:.1} % of the variance", 100.0 * explained[k - 1]),
                "variance": explained[k - 1],
                "amplitude": amplitude(&comps.pcs[(k - 1) * t..k * t]),
            }),
            Kind::Rotated(k) => {
                let q = rotated.as_ref().ok_or("no rotation")?;
                json!({
                    "mode": k,
                    "name": format!("Rotated EOF {k}: {:.1} % of the variance", 100.0 * q.variance[k - 1]),
                    "variance": q.variance[k - 1],
                    "amplitude": amplitude(&q.pcs[(k - 1) * t..k * t]),
                })
            }
        };
        extra.insert(o.id.clone(), info);
    }
    write_files(
        &dir,
        &Summary {
            d: &d,
            eigen: &eigen,
            north: &north,
            total_var,
            comps: &comps,
            rotated: rotated.as_ref(),
            labels: &labels,
            years: &spec.years,
            s,
            applied: weights.applied,
            n_pixels: nk,
            n_dates: tk,
            cache: if in_memory { "memory" } else { "file" },
        },
    )?;
    let model = json!({ "outputs": Value::Object(extra) });
    let outputs = outs.describe(p, Some(&model));
    drop(outs);
    progress(1.0, "done");
    Ok(JobResult { outputs, window })
}

fn dir_of(spec: &JobSpec) -> PathBuf {
    PathBuf::from(&spec.output_dir)
}

/// The PCs of every date: the fit's, and for the dates left out of it the gappy least squares onto their valid pixels
/// (Everson and Sirovich 1995); the dates with too few valid pixels stay NaN.
fn components(cube: &Cube, d: &Decomposition, v: &[f64], units: bool, f: f64, tk: usize) -> Result<Components, String> {
    let (t, nk, m) = (cube.t, cube.kept(), d.modes());
    let unit_kept = d.pcs(true);
    let mut unit_all = vec![f64::NAN; m * t];
    let mut is_kept = vec![false; t];
    for (i, &ti) in cube.dates.iter().enumerate() {
        is_kept[ti] = true;
        for k in 0..m {
            unit_all[k * t + ti] = unit_kept[k * tk + i];
        }
    }
    let mut filled = vec![false; t];
    let dropped: Vec<usize> = (0..t).filter(|&ti| !is_kept[ti]).collect();
    // a few dates at a time: each one's row over the kept pixels is eight bytes a pixel
    let group = ((256usize << 20) / (8 * nk).max(1)).clamp(1, 64);
    for part in dropped.chunks(group) {
        let rows = cube.weighted_rows(part)?;
        for (&ti, row) in part.iter().zip(&rows) {
            let coef = eof::project_gappy(v, m, row);
            for k in 0..m {
                if d.singular[k] > 0.0 {
                    unit_all[k * t + ti] = coef[k] / d.singular[k] * f;
                }
            }
            filled[ti] = coef.iter().all(|c| c.is_finite());
        }
    }
    let mut pcs = unit_all.clone();
    if !units {
        for (k, col) in pcs.chunks_mut(t).enumerate() {
            let c = d.singular[k] / f;
            col.iter_mut().for_each(|x| *x *= c);
        }
    }
    Ok(Components { pcs, unit: unit_all, filled })
}

/// The first modes rotated by varimax, ordered by their variance, each with its sign rule applied.
struct Rotation {
    /// modes x kept pixels (the patterns in data units per standard deviation of the PC)
    maps: Vec<f64>,
    /// modes x dates, unit variance
    pcs: Vec<f64>,
    variance: Vec<f64>,
    iterations: usize,
}

#[allow(clippy::too_many_arguments)]
fn rotation(
    cube: &Cube,
    d: &Decomposition,
    v: &[f64],
    unit_all: &[f64],
    s: &Settings,
    f: f64,
    total_var: f64,
) -> Rotation {
    let (t, nk, m) = (cube.t, cube.kept(), s.rotate);
    let mut l = vec![0.0; nk * m];
    for k in 0..m {
        for p in 0..nk {
            l[k * nk + p] = v[k * nk + p] * d.singular[k] / f;
        }
    }
    let (r, iterations) = eof::varimax(&mut l, nk, m, s.kaiser, 1e-8, 1000);
    let var: Vec<f64> =
        (0..m).map(|k| l[k * nk..(k + 1) * nk].iter().map(|a| a * a).sum::<f64>() / total_var).collect();
    let mut order: Vec<usize> = (0..m).collect();
    order.sort_by(|&a, &b| var[b].total_cmp(&var[a]));
    let z = eof::rotate(&unit_all[..m * t], t, m, &r);
    let (mut pcs, mut maps, mut variance) = (vec![0.0; m * t], vec![f64::NAN; m * nk], vec![0.0; m]);
    for (new, &old) in order.iter().enumerate() {
        let mut col = z[old * t..(old + 1) * t].to_vec();
        let u: Vec<f64> = cube.dates.iter().map(|&ti| col[ti] / f).collect();
        let sign = if flip_needed(&u, &d.weighted_sum, d.total) { -1.0 } else { 1.0 };
        col.iter_mut().for_each(|c| *c *= sign);
        pcs[new * t..(new + 1) * t].copy_from_slice(&col);
        for p in 0..nk {
            maps[new * nk + p] = if cube.sqrt_w[p] > 0.0 { sign * l[old * nk + p] / cube.sqrt_w[p] } else { f64::NAN };
        }
        variance[new] = var[old];
    }
    Rotation { maps, pcs, variance, iterations }
}

/// What eof.json and pcs.csv say.
struct Summary<'a> {
    d: &'a Decomposition,
    eigen: &'a [f64],
    north: &'a [(f64, bool)],
    total_var: f64,
    comps: &'a Components,
    rotated: Option<&'a Rotation>,
    labels: &'a [String],
    years: &'a [f64],
    s: &'a Settings,
    applied: &'static str,
    n_pixels: usize,
    n_dates: usize,
    cache: &'static str,
}

/// eof.json (the decomposition's numbers) and pcs.csv (the components of every date) in the run's folder.
fn write_files(dir: &std::path::Path, z: &Summary) -> Result<(), String> {
    let (m, t) = (z.d.modes(), z.years.len());
    let solver = match z.d.solver_used {
        Solver::Randomized { oversampling, power_iterations } => {
            json!({ "name": "randomized", "oversampling": oversampling, "power_iterations": power_iterations })
        }
        _ => json!({ "name": "exact" }),
    };
    let explained = z.d.explained();
    let doc = json!({
        "modes": m,
        "dates": t,
        "n_dates": z.n_dates,
        "n_pixels": z.n_pixels,
        "total_variance": z.total_var,
        "solver": solver,
        "weights": z.applied,
        "scaling": if z.s.units { "units" } else { "none" },
        "north_n": if z.s.autocorrelation { "autocorrelation" } else { "dates" },
        "cache": z.cache,
        "variance": explained,
        "eigenvalue": z.eigen,
        "north_error": z.north.iter().map(|x| x.0).collect::<Vec<_>>(),
        "separable": z.north.iter().map(|x| x.1).collect::<Vec<_>>(),
        "variance_rotated": z.rotated.map(|r| r.variance.clone()),
        "rotation_iterations": z.rotated.map(|r| r.iterations),
    });
    let path = dir.join("eof.json");
    std::fs::write(&path, serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let mut csv = String::from("date,year,filled");
    (1..=m).for_each(|k| csv += &format!(",pc{k}"));
    if let Some(r) = z.rotated {
        (1..=r.variance.len()).for_each(|k| csv += &format!(",rotated{k}"));
    }
    csv.push('\n');
    let num = |v: f64| if v.is_finite() { format!("{v}") } else { String::new() };
    for i in 0..t {
        csv += &format!("{},{:.4},{}", z.labels[i].replace(',', " "), z.years[i], u8::from(z.comps.filled[i]));
        for k in 0..m {
            csv += &format!(",{}", num(z.comps.pcs[k * t + i]));
        }
        if let Some(r) = z.rotated {
            for k in 0..r.variance.len() {
                csv += &format!(",{}", num(r.pcs[k * t + i]));
            }
        }
        csv.push('\n');
    }
    let path = dir.join("pcs.csv");
    std::fs::write(&path, csv).map_err(|e| format!("{}: {e}", path.display()))
}

/// The run's time: the window read and its anomalies made once (measured on a sample of it), the cache read back for
/// every pass of the solver and the maps (float32 from memory), and the products of the passes (flops).
pub(crate) fn estimate(spec: &JobSpec, p: &Params, cells: f64) -> Result<Estimate, String> {
    let s = settings(p)?;
    let days = days_of(spec, &s)?;
    let mut inputs = Inputs::new(spec)?;
    let (iw, ih) = inputs.input.ds().raster_size();
    let [x0, y0, x1, y1] = spec.window.unwrap_or([0, 0, iw, ih]);
    let (w, h) = (x1 - x0, y1 - y0);
    let t = spec.years.len();
    let (sw, sh) = (32.min(w), 32.min(h));
    let (sx, sy) = (x0 + (w - sw) / 2, y0 + (h - sh) / 2);
    inputs.read(sx, sy, sw.min(4), sh.min(4))?; // warm-up
    let began = std::time::Instant::now();
    let chunk = inputs.read(sx, sy, sw, sh)?;
    let read = began.elapsed().as_secs_f64();
    let began = std::time::Instant::now();
    let mut vals = chunk.stack;
    anomalies_cube(&mut vals, t, sw * sh, &days, &s.anomaly, spec.n_jobs());
    let anomalies = began.elapsed().as_secs_f64();
    let per_px = (read + anomalies) / (sw * sh).max(1) as f64;
    // the passes of the solver over the cache and the flops of each pixel (multiply-adds, two flops each)
    let tk = t as f64;
    let modes = (s.modes.min(t.saturating_sub(1))) as f64;
    let (passes, flops) = match s.solver {
        Solver::Randomized { oversampling, power_iterations } => {
            let l = modes + oversampling as f64;
            let q = power_iterations as f64;
            (q + 2.0, 2.0 * tk * l * (2.0 + 2.0 * q) + 2.0 * l * l * (q + 1.0))
        }
        _ => (1.0, 2.0 * tk * tk),
    };
    let maps_flops = 2.0 * tk * modes;
    let threads = match spec.n_jobs() {
        n if n > 0 => n as f64,
        _ => std::thread::available_parallelism().map_or(4.0, |n| n.get() as f64),
    };
    // a core multiplies about 4e9 a second in blocks of matrices; the cache streams at about 2e9 bytes a second
    let compute = (flops + maps_flops) / (4e9 * threads.min(8.0));
    let stream = (passes + 1.0) * 4.0 * tk / 2e9;
    Ok(Estimate { sec_per_px: per_px + compute + stream, sec_per_chunk: 0.0, chunk_cells: cells, sec_fixed: 0.0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(v: Value) -> Params {
        let given = v.as_object().unwrap().iter().map(|(k, x)| (k.clone(), x.clone())).collect();
        Params::with_defaults("eof", &given).unwrap()
    }

    #[test]
    fn the_anomaly_parameter_is_zeits() {
        let a = anomaly_settings(&params(json!({ "anomaly": "none" })));
        assert_eq!(a.seasonal, Seasonal::Mean);
        let a = anomaly_settings(&params(json!({ "anomaly": "harmonic", "harmonics": 3 })));
        assert_eq!(a.seasonal, Seasonal::Harmonic { order: 3 });
        let a = anomaly_settings(&params(json!({})));
        assert_eq!(a.seasonal, Seasonal::Monthly { min_per_month: 2 });
    }

    #[test]
    fn a_rotation_of_one_mode_and_a_bad_fraction_are_refused() {
        assert!(settings(&params(json!({ "rotate": 1 }))).is_err());
        assert!(settings(&params(json!({ "min_valid": 1.5 }))).is_err());
        assert!(settings(&params(json!({ "n_modes": 0 }))).is_err());
        let s = settings(&params(json!({ "rotate": 3, "north_n": "dates", "scaling": "none" }))).unwrap();
        assert_eq!((s.rotate, s.autocorrelation, s.units), (3, false, false));
    }

    #[test]
    fn the_outputs_ids_name_their_mode() {
        assert_eq!(kind_of("eof_3"), Some(Kind::Eof(3)));
        assert_eq!(kind_of("eof_rotated_2"), Some(Kind::Rotated(2)));
        assert_eq!(kind_of("corrmap_1"), Some(Kind::Corr(1)));
        assert_eq!(kind_of("eof_0"), None);
        assert_eq!(kind_of("pc_1"), None);
    }

    #[test]
    fn the_overlay_puts_the_pattern_on_the_seasonal_cycle() {
        // a pure seasonal cycle: its baseline is the cycle, so the overlay of a zero pattern is the cycle
        let days: Vec<i64> = (0..48).map(|i| 18262 + 30 * i).collect();
        let values: Vec<f64> = days.iter().map(|&d| (d as f64 / 58.0).sin()).collect();
        let a = AnomalySettings::default();
        let zero = pixel_overlay(&values, &days, &a, 0.0, &vec![1.0; 48]);
        let one = pixel_overlay(&values, &days, &a, 2.0, &vec![1.0; 48]);
        for (z, o) in zero.iter().zip(&one) {
            if z.is_finite() {
                assert!((o - z - 2.0).abs() < 1e-12);
            }
        }
    }
}
