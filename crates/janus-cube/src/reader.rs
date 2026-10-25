//! Reading a cube's layers with GDAL. A GDAL dataset may move between threads but not be shared by them, so each
//! thread keeps its own reader with its own datasets (src/cube.cpp, CubeReader and threadReader).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gdal::Dataset;
use gdal::raster::ResampleAlg;

use crate::{BandMeta, CubeInfo, QaRule};

pub struct CubeReader {
    info: Arc<CubeInfo>,
    /// each file opened, and whether it is a calculator's VRT (read block by block)
    datasets: HashMap<String, Option<(Dataset, bool)>>,
    tmp: Vec<f32>,
    /// an expression's bands
    bands: Vec<Vec<f32>>,
    /// more handles of each calculator's VRT, one per stripe of a large window read on several threads
    stripes: HashMap<String, Vec<Dataset>>,
}

/// The output rows (j0..j1) of a window's stripes, each read on a thread of its own: the rows whose source rows
/// (nearest neighbour, as GDAL samples them) fall in a group of whole block rows, as many groups as there are
/// threads to spare and at least two block rows... else one stripe (a small window, a window of one block row).
fn stripes((y, h): (usize, usize), bh: usize, block_rows: usize) -> Vec<(usize, usize)> {
    let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
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
        CubeReader { info, datasets: HashMap::new(), tmp: Vec::new(), bands: Vec::new(), stripes: HashMap::new() }
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
        let (ds, calc) = self
            .datasets
            .entry(l.path.clone())
            .or_insert_with(|| {
                let calc = l.path.len() > 4
                    && l.path[l.path.len() - 4..].eq_ignore_ascii_case(".vrt")
                    && crate::calc::vrt::is_calc_vrt(&l.path);
                crate::calc::vrt::with_block_reads(calc, || crate::open_raster(&l.path).ok()).map(|d| (d, calc))
            })
            .as_ref()?;
        let calc = *calc;
        if band < 1 || band as usize > ds.raster_count() {
            return None;
        }
        let rb = ds.rasterband(band as usize).ok()?;
        let meta = if b <= 0 { l.meta } else { crate::band_meta(&rb) };
        // a calculator's band is computed on the thread that reads it: a large window in stripes on several
        if calc {
            let block_rows = rb.block_size().1.max(1);
            let groups = stripes((y, h), bh, block_rows);
            if groups.len() > 1 {
                let path = l.path.clone();
                let handles = self.stripes.entry(path.clone()).or_default();
                while handles.len() < groups.len() {
                    match crate::calc::vrt::with_block_reads(true, || crate::open_raster(&path)) {
                        Ok(d) => handles.push(d),
                        Err(_) => return None,
                    }
                }
                let ok = std::sync::atomic::AtomicBool::new(true);
                std::thread::scope(|sc| {
                    let mut rest = &mut buf[..bw * bh];
                    for (&(j0, j1), ds) in groups.iter().zip(handles.iter_mut()) {
                        let (part, tail) = std::mem::take(&mut rest).split_at_mut((j1 - j0) * bw);
                        rest = tail;
                        let ok = &ok;
                        sc.spawn(move || {
                            let r = crate::calc::vrt::with_block_reads(true, || {
                                read_rows(ds, band as usize, (x, y, w, h), (bw, bh), (j0, j1), part)
                            });
                            if !r {
                                ok.store(false, std::sync::atomic::Ordering::Relaxed);
                            }
                        });
                    }
                });
                return ok.into_inner().then_some(meta);
            }
        }
        // a calculator's VRT read block by block (its bands may have many sources, all read for a request)
        crate::calc::vrt::with_block_reads(calc, || {
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
