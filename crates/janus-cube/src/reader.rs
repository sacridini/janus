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
    datasets: HashMap<String, Option<Dataset>>,
    tmp: Vec<f32>,
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
        CubeReader { info, datasets: HashMap::new(), tmp: Vec::new() }
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
        let ds = self.datasets.entry(l.path.clone()).or_insert_with(|| crate::open_raster(&l.path).ok()).as_ref()?;
        if band < 1 || band as usize > ds.raster_count() {
            return None;
        }
        let rb = ds.rasterband(band as usize).ok()?;
        rb.read_into_slice::<f32>(
            (x as isize, y as isize),
            (w, h),
            (bw, bh),
            &mut buf[..bw * bh],
            Some(ResampleAlg::NearestNeighbour),
        )
        .ok()?;
        Some(if b <= 0 { l.meta } else { crate::band_meta(&rb) })
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
        let Some(meta) = self.read_raw(layer, 0, win, buf, size) else { return false };
        post_process(&meta, &mut buf[..n]);
        let sel = self.info.sel;
        let mut tmp = std::mem::take(&mut self.tmp);
        tmp.resize(n, 0.0);
        let mut ok = true;
        if sel.nd_band > 0 {
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
