//! Reading a cube's layers with GDAL. A GDAL dataset may move between threads but not be shared by them, so each
//! thread keeps its own reader with its own datasets (src/cube.cpp, CubeReader and threadReader).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gdal::Dataset;
use gdal::raster::ResampleAlg;

use crate::{BandMeta, CubeInfo, QaRule};

pub struct CubeReader {
    info: Arc<CubeInfo>,
    /// each file opened, whether it is a calculator's VRT (read block by block), and the files that VRT reads when it
    /// may be read whole at a smaller size (vrt::whole_sources)
    datasets: HashMap<String, Option<(Dataset, bool, Option<usize>)>>,
    /// a second handle of such a VRT, opened to be read whole (GDAL reads a VRT as it was opened: block by block
    /// when opened so)
    whole: HashMap<String, Option<Dataset>>,
    tmp: Vec<f32>,
    /// an expression's bands
    bands: Vec<Vec<f32>>,
}

/// The threads reading a calculator's band in stripes (eight, kept for the process), each reading through handles it
/// opened and keeps (stripe_handle): GDAL pools a VRT's sources by the thread that opened them, so a handle read on
/// other threads shared a source's dataset with a thread Windows had given the same id, and crashed; a handle opened
/// for each read cost 0.3 s a read to close (its blocks dropped from GDAL's cache).
fn stripe_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .thread_name(|i| format!("calculator stripes {i}"))
            .build()
            .expect("the calculator's stripe threads")
    })
}

/// A stripe thread's handles: by series (a live result rewrites its VRTs under the same names), file and whether read
/// block by block; the latest 16.
type StripeKey = (u64, String, bool);

thread_local! {
    static STRIPE_HANDLES: RefCell<Vec<(StripeKey, Dataset)>> = const { RefCell::new(Vec::new()) };
}

/// `f` over this thread's handle of `key` (opened the first time), GDAL reading block by block on it or not.
fn with_stripe_handle<R>(key: &StripeKey, f: impl FnOnce(&Dataset) -> R) -> Option<R> {
    let blocks = key.2;
    STRIPE_HANDLES.with_borrow_mut(|hs| {
        let i = match hs.iter().position(|h| &h.0 == key) {
            Some(i) => i,
            None => {
                let ds = crate::calc::vrt::with_block_reads(blocks, || crate::open_raster(&key.1)).ok()?;
                if hs.len() >= 16 {
                    hs.remove(0);
                }
                hs.push((key.clone(), ds));
                hs.len() - 1
            }
        };
        Some(crate::calc::vrt::with_block_reads(blocks, || f(&hs[i].1)))
    })
}

/// Calculator bands being read now, by every reader of the process: their stripes share eight threads (eight workers
/// of an overview each reading a date in eight stripes ran 64 threads).
static CALC_READS: AtomicUsize = AtomicUsize::new(0);

/// A read of a calculator's band under way (counted in CALC_READS while it lives).
struct CalcRead;

impl CalcRead {
    fn enter() -> CalcRead {
        CALC_READS.fetch_add(1, Ordering::Relaxed);
        CalcRead
    }

    /// The threads for this read's stripes: its share of eight (at most the processors), one at least.
    fn threads(&self) -> usize {
        let all = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        (all / CALC_READS.load(Ordering::Relaxed).max(1)).max(1)
    }
}

impl Drop for CalcRead {
    fn drop(&mut self) {
        CALC_READS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The output rows (j0..j1) of a window's stripes, each read on a thread of its own: the rows whose source rows
/// (nearest neighbour, as GDAL samples them) fall in a group of whole block rows, as many groups as there are
/// `threads` and at least two block rows... else one stripe (a small window, a window of one block row).
fn stripes((y, h): (usize, usize), bh: usize, block_rows: usize, threads: usize) -> Vec<(usize, usize)> {
    let (first, last) = (y / block_rows, (y + h - 1) / block_rows);
    let n_blocks = last - first + 1;
    if threads < 2 || n_blocks < 2 || bh < 64 || h * bh < 256 * 256 {
        return vec![(0, bh)];
    }
    let k = threads.min(n_blocks);
    let scale = h as f64 / bh as f64;
    // the first output row sampling source row `r` or below it
    let row_of = |r: usize| -> usize {
        if r <= y {
            return 0;
        }
        (((r - y) as f64 / scale - 0.5).ceil().max(0.0) as usize).min(bh)
    };
    let mut out = Vec::new();
    let mut j0 = 0;
    for g in 1..=k {
        // the end of the g-th group of block rows
        let end_block = first + (n_blocks * g).div_ceil(k);
        let j1 = if g == k { bh } else { row_of(end_block * block_rows) };
        if j1 > j0 {
            out.push((j0, j1));
            j0 = j1;
        }
    }
    out
}

/// Output rows j0..j1 of a window (x, y, w, h) read into bw × bh, as the whole read would sample them: the source
/// window of those rows given to GDAL as a floating-point window.
fn read_rows(
    ds: &Dataset,
    band: usize,
    (x, y, w, h): (usize, usize, usize, usize),
    (bw, bh): (usize, usize),
    (j0, j1): (usize, usize),
    out: &mut [f32],
) -> bool {
    let Ok(rb) = ds.rasterband(band) else { return false };
    let scale = h as f64 / bh as f64;
    let fy = y as f64 + j0 as f64 * scale;
    let fh = (j1 - j0) as f64 * scale;
    let iy = fy.floor() as usize;
    let iy1 = ((fy + fh).ceil() as usize).min(y + h);
    let mut extra = gdal_sys::GDALRasterIOExtraArg {
        nVersion: 1,
        eResampleAlg: gdal_sys::GDALRIOResampleAlg::GRIORA_NearestNeighbour,
        pfnProgress: None,
        pProgressData: std::ptr::null_mut(),
        bFloatingPointWindowValidity: 1,
        dfXOff: x as f64,
        dfYOff: fy,
        dfXSize: w as f64,
        dfYSize: fh,
    };
    // SAFETY: a live band, a buffer of bw × (j1 - j0) floats, the extra arguments alive for the call.
    let err = unsafe {
        gdal_sys::GDALRasterIOEx(
            rb.c_rasterband(),
            gdal_sys::GDALRWFlag::GF_Read,
            x as i32,
            iy as i32,
            w as i32,
            (iy1 - iy) as i32,
            out.as_mut_ptr() as *mut std::ffi::c_void,
            bw as i32,
            (j1 - j0) as i32,
            gdal_sys::GDALDataType::GDT_Float32,
            0,
            0,
            &mut extra,
        )
    };
    err == gdal_sys::CPLErr::CE_None
}

/// Whether output row j may begin a chunk of a window's rows (y, h) read into bh rows (read_rows): its source row
/// not a hair from a whole one, which GDAL takes for the whole one (a VRT of the calculator read under another, `s -
/// s[-1]` by chunks, gave at row 1221 of 3666, its source row 2441.0003, the row before the one sampled read whole).
fn clean_cut((y, h): (usize, usize), bh: usize, j: usize) -> bool {
    let f = y as f64 + j as f64 * (h as f64 / bh as f64);
    (f - f.round()).abs() > 0.02
}

/// The output rows of a window read whole at a smaller size, `k` at a time (its sources within memory), each chunk
/// cut at a clean row (clean_cut); None when none is near enough (then block by block).
fn whole_chunks((y, h): (usize, usize), bh: usize, k: usize) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut j0 = 0;
    while j0 < bh {
        let end = (j0 + k).min(bh);
        let j1 = if end == bh { end } else { (j0 + 1..=end).rev().take(64).find(|&j| clean_cut((y, h), bh, j))? };
        out.push((j0, j1));
        j0 = j1;
    }
    Some(out)
}

/// Nodata and infinities become NaN; the band's scale and offset are applied.
fn post_process(m: &BandMeta, v: &mut [f32]) {
    let scaled = m.scale != 1.0 || m.offset != 0.0;
    let (s, o) = (m.scale as f32, m.offset as f32);
    let nd = m.no_data.map(|n| n as f32);
    for x in v {
        if let Some(nd) = nd
            && (*x == nd || (nd.is_nan() && x.is_nan()))
        {
            *x = f32::NAN;
        } else if x.is_infinite() {
            *x = f32::NAN;
        } else if scaled {
            *x = *x * s + o;
        }
    }
}

impl CubeReader {
    pub fn new(info: Arc<CubeInfo>) -> Self {
        CubeReader { info, datasets: HashMap::new(), whole: HashMap::new(), tmp: Vec::new(), bands: Vec::new() }
    }

    pub fn cube_id(&self) -> u64 {
        self.info.id
    }

    /// Band `b` of layer `layer`'s date (1..=bands_per_date; 0 = the shown band), resampled by GDAL's nearest
    /// neighbour into `buf`, no post-processing.
    fn read_raw(
        &mut self,
        layer: usize,
        b: i32,
        (x, y, w, h): (usize, usize, usize, usize),
        buf: &mut [f32],
        (bw, bh): (usize, usize),
    ) -> Option<BandMeta> {
        let l = &self.info.layers[layer];
        let band = if b <= 0 { l.band } else { b + l.band_base };
        let (ds, calc, whole) = self
            .datasets
            .entry(l.path.clone())
            .or_insert_with(|| {
                let calc = l.path.len() > 4
                    && l.path[l.path.len() - 4..].eq_ignore_ascii_case(".vrt")
                    && crate::calc::vrt::is_calc_vrt(&l.path);
                let whole = if calc { crate::calc::vrt::whole_sources(&l.path) } else { None };
                crate::calc::vrt::with_block_reads(calc, || crate::open_raster(&l.path).ok()).map(|d| (d, calc, whole))
            })
            .as_ref()?;
        let calc = *calc;
        // block by block, but at a smaller size when the band's files can be read whole, each at that size (the rows
        // it shows, as a series' own overview reads them): so many output rows at a time that all its sources fit in
        // 16 MB
        let _reading = calc.then(CalcRead::enter);
        let threads = _reading.as_ref().map_or(1, CalcRead::threads);
        let chunks = match *whole {
            Some(n) if bw < w || bh < h => {
                let k = ((16usize << 20) / (4 * bw.max(1) * n.max(1))).clamp(1, bh.div_ceil(threads).max(1));
                whole_chunks((y, h), bh, k)
            }
            _ => None,
        };
        let blocks = calc && chunks.is_none();
        if band < 1 || band as usize > ds.raster_count() {
            return None;
        }
        let ds = if chunks.is_some() {
            self.whole.entry(l.path.clone()).or_insert_with(|| crate::open_raster(&l.path).ok()).as_ref()?
        } else {
            ds
        };
        let rb = ds.rasterband(band as usize).ok()?;
        let meta = if b <= 0 { l.meta } else { crate::band_meta(&rb) };
        // a calculator's band is computed on the thread that reads it: a large window in stripes on several, the
        // first on this one; each other stripe through a handle opened on its thread for this read alone (GDAL pools
        // a VRT's sources by the thread that opened them: handles kept for later reads on other threads shared a
        // source's dataset with a thread Windows had given the same id, and crashed)
        if calc {
            // the runs of output rows, each on a thread: read whole by chunks (a run of them each), else in stripes
            // of whole block rows
            let runs: Vec<Vec<(usize, usize)>> = match &chunks {
                Some(c) => c.chunks(c.len().div_ceil(threads)).map(<[_]>::to_vec).collect(),
                None => stripes((y, h), bh, rb.block_size().1.max(1), threads).into_iter().map(|g| vec![g]).collect(),
            };
            let read = move |ds: &Dataset, run: &[(usize, usize)], out: &mut [f32]| -> bool {
                let j0 = run[0].0;
                run.iter().all(|&(a, b)| {
                    read_rows(ds, band as usize, (x, y, w, h), (bw, bh), (a, b), &mut out[(a - j0) * bw..(b - j0) * bw])
                })
            };
            if runs.len() == 1 && chunks.is_some() {
                return crate::calc::vrt::with_block_reads(false, || read(ds, &runs[0], &mut buf[..bw * bh]))
                    .then_some(meta);
            }
            if runs.len() > 1 {
                let (key, ok) = ((self.info.id, l.path.clone(), blocks), std::sync::atomic::AtomicBool::new(true));
                stripe_pool().scope(|sc| {
                    let mut rest = &mut buf[..bw * bh];
                    for run in &runs {
                        let (part, tail) =
                            std::mem::take(&mut rest).split_at_mut((run[run.len() - 1].1 - run[0].0) * bw);
                        rest = tail;
                        let (key, ok) = (&key, &ok);
                        sc.spawn(move |_| {
                            if with_stripe_handle(key, |ds| read(ds, run, part)) != Some(true) {
                                ok.store(false, std::sync::atomic::Ordering::Relaxed);
                            }
                        });
                    }
                });
                return ok.into_inner().then_some(meta);
            }
        }
        // a calculator's VRT read block by block (its bands may have many sources, all read for a request)
        crate::calc::vrt::with_block_reads(blocks, || {
            rb.read_into_slice::<f32>(
                (x as isize, y as isize),
                (w, h),
                (bw, bh),
                &mut buf[..bw * bh],
                Some(ResampleAlg::NearestNeighbour),
            )
        })
        .ok()?;
        Some(meta)
    }

    /// The shown quantity over the window (x, y, w, h) resampled (nearest neighbour) to bw × bh, as GDAL
    /// resamples it: the band, or the normalized difference of two bands, nodata as NaN, scale and offset applied,
    /// unusable observations (quality band) as NaN.
    pub fn read_window(
        &mut self,
        layer: usize,
        win: (usize, usize, usize, usize),
        buf: &mut [f32],
        size: (usize, usize),
    ) -> bool {
        let n = size.0 * size.1;
        let sel = self.info.sel;
        if let Some(e) = self.info.expr.clone() {
            // the expression over its bands, each scaled and with its no data as NaN
            let mut bands = std::mem::take(&mut self.bands);
            bands.resize_with(e.bands.len(), Vec::new);
            for (k, &b) in e.bands.iter().enumerate() {
                bands[k].resize(n, 0.0);
                let Some(meta) = self.read_raw(layer, b, win, &mut bands[k], size) else {
                    self.bands = bands;
                    return false;
                };
                post_process(&meta, &mut bands[k][..n]);
            }
            let inputs: Vec<&[f32]> = bands.iter().map(|b| &b[..n]).collect();
            let l = &self.info.layers[layer];
            let vars = crate::calc::DateVars::new(self.info.time_is_date.then_some(l.time), layer);
            e.program.eval(&inputs, &vars, &mut buf[..n]);
            // a large window's buffers are not kept by the thread
            if n > 1 << 22 {
                bands.clear();
            }
            self.bands = bands;
        } else {
            let Some(meta) = self.read_raw(layer, 0, win, buf, size) else { return false };
            post_process(&meta, &mut buf[..n]);
        }
        let mut tmp = std::mem::take(&mut self.tmp);
        tmp.resize(n, 0.0);
        let mut ok = true;
        if sel.nd_band > 0 && self.info.expr.is_none() {
            match self.read_raw(layer, sel.nd_band, win, &mut tmp, size) {
                Some(_) => {
                    post_process(&self.info.layers[layer].nd_meta, &mut tmp);
                    for (a, &b) in buf[..n].iter_mut().zip(&tmp) {
                        let s = *a + b;
                        // NaN in either input stays NaN
                        *a = if s != 0.0 { (*a - b) / s } else { f32::NAN };
                    }
                }
                None => ok = false,
            }
        }
        if ok && sel.qa_band > 0 && sel.qa_rule != QaRule::None {
            match self.read_raw(layer, sel.qa_band, win, &mut tmp, size) {
                Some(_) => {
                    for (v, &q) in buf[..n].iter_mut().zip(&tmp) {
                        if q.is_nan() || q < 0.0 || !sel.qa_rule.usable(q as u32) {
                            *v = f32::NAN;
                        }
                    }
                }
                None => ok = false,
            }
        }
        self.tmp = tmp;
        ok
    }

    pub fn read_pixel(&mut self, layer: usize, x: usize, y: usize) -> Option<f32> {
        let mut v = [0.0f32];
        self.read_window(layer, (x, y, 1, 1), &mut v, (1, 1)).then_some(v[0])
    }

    /// One source band of a date at a pixel (1..=bands_per_date), scaled, nodata as NaN unless `raw` (quality codes).
    pub fn read_band_pixel(&mut self, layer: usize, b: i32, x: usize, y: usize, raw: bool) -> Option<f32> {
        let mut v = [0.0f32];
        let meta = self.read_raw(layer, b.max(1), (x, y, 1, 1), &mut v, (1, 1))?;
        if !raw {
            post_process(&meta, &mut v);
        }
        Some(v[0])
    }

    /// One source band of a date over a window, resampled like read_window, scaled, nodata as NaN (e.g. the
    /// bands of an RGB composite).
    pub fn read_band_window(
        &mut self,
        layer: usize,
        b: i32,
        win: (usize, usize, usize, usize),
        buf: &mut [f32],
        size: (usize, usize),
    ) -> bool {
        let Some(meta) = self.read_raw(layer, b.max(1), win, buf, size) else { return false };
        post_process(&meta, &mut buf[..size.0 * size.1]);
        true
    }
}

thread_local! {
    static READER: RefCell<Option<CubeReader>> = const { RefCell::new(None) };
}

/// Runs `f` with the current thread's reader for this cube (made on demand).
pub fn with_thread_reader<R>(info: &Arc<CubeInfo>, f: impl FnOnce(&mut CubeReader) -> R) -> R {
    READER.with_borrow_mut(|r| {
        if r.as_ref().is_none_or(|r| r.cube_id() != info.id) {
            *r = Some(CubeReader::new(info.clone()));
        }
        f(r.as_mut().unwrap())
    })
}
