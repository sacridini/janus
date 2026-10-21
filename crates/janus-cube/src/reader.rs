//! Reading a cube's layers with GDAL. A GDAL dataset may move between threads but not be shared by them, so each
//! thread keeps its own reader with its own datasets (src/cube.cpp, CubeReader and threadReader).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use gdal::Dataset;
use gdal::raster::ResampleAlg;

use crate::{BandMeta, CubeInfo};

pub struct CubeReader {
    info: Arc<CubeInfo>,
    datasets: HashMap<String, Option<Dataset>>,
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
        CubeReader { info, datasets: HashMap::new() }
    }

    pub fn cube_id(&self) -> u64 {
        self.info.id
    }

    /// Reads the window (x, y, w, h) of a layer resampled (nearest neighbour) to bw × bh, as GDAL resamples it.
    pub fn read_window(
        &mut self,
        layer: usize,
        (x, y, w, h): (usize, usize, usize, usize),
        buf: &mut [f32],
        (bw, bh): (usize, usize),
    ) -> bool {
        let l = &self.info.layers[layer];
        let ds = self.datasets.entry(l.path.clone()).or_insert_with(|| crate::open_raster(&l.path).ok());
        let Some(ds) = ds else { return false };
        let Ok(band) = ds.rasterband(l.band as usize) else { return false };
        let out = &mut buf[..bw * bh];
        let ok = band
            .read_into_slice::<f32>(
                (x as isize, y as isize),
                (w, h),
                (bw, bh),
                out,
                Some(ResampleAlg::NearestNeighbour),
            )
            .is_ok();
        if ok {
            post_process(&l.meta, out);
        }
        ok
    }

    pub fn read_pixel(&mut self, layer: usize, x: usize, y: usize) -> Option<f32> {
        let mut v = [0.0f32];
        self.read_window(layer, (x, y, 1, 1), &mut v, (1, 1)).then_some(v[0])
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
