//! Reading maps in bands of rows, on their own grid or warped onto the strata's; the rows of a polygon; areas
//! of classes along a row (src/validation.cpp: MapReader, Spans, RowAcc, bandRows).

use std::collections::HashMap;
use std::ffi::{CString, c_char};
use std::sync::atomic::{AtomicUsize, Ordering};

use gdal::{Dataset, DatasetOptions, GdalOpenFlags};
use janus_geo::plane::Polygon;

use crate::NONE;
use crate::rules::{Grid, grid_of, same_grid};

/// Rows per band when > 0 (the self-test: bands with halos).
pub static TEST_BAND_ROWS: AtomicUsize = AtomicUsize::new(0);

/// GDAL's last error message, after `what`.
pub(crate) fn last_error(what: &str) -> String {
    // SAFETY: GDAL returns its own (thread-local) message buffer.
    let m = unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }.to_string_lossy().into_owned();
    if m.is_empty() { what.to_string() } else { format!("{what}: {m}") }
}

/// A raster opened read-only; `threads` > 1: GDAL decodes a band's blocks with several threads (GeoTIFF).
pub(crate) fn open_raster(path: &str, threads: usize) -> Result<Dataset, String> {
    let nt = format!("NUM_THREADS={}", threads.max(1));
    let oo = [nt.as_str()];
    Dataset::open_ex(
        path,
        DatasetOptions {
            open_flags: GdalOpenFlags::GDAL_OF_RASTER | GdalOpenFlags::GDAL_OF_READONLY,
            open_options: if threads > 1 { Some(&oo) } else { None },
            ..Default::default()
        },
    )
    .map_err(|e| format!("cannot open {path}: {e}"))
}

/// A NULL-terminated list of C strings for GDAL's utilities.
struct Args {
    _s: Vec<CString>,
    p: Vec<*mut c_char>,
}

impl Args {
    fn new(a: &[String]) -> Args {
        let s: Vec<CString> = a.iter().map(|x| CString::new(x.as_str()).unwrap()).collect();
        let mut p: Vec<*mut c_char> = s.iter().map(|c| c.as_ptr() as *mut c_char).collect();
        p.push(std::ptr::null_mut());
        Args { _s: s, p }
    }
}

/// One band of a raster, read in rows as float (no data -> NaN), on its own grid or warped (nearest neighbour)
/// onto another one.
pub(crate) struct MapReader {
    // dropped in this order: the warp before the datasets it reads
    used: Dataset,
    _one: Option<Dataset>,
    _src: Option<Dataset>,
    band: usize,
    pub w: usize,
    pub h: usize,
    nodata: Option<f32>,
    own: Grid,
}

impl MapReader {
    pub fn open(path: &str, band: i32, target: Option<&Grid>, threads: usize) -> Result<MapReader, String> {
        let ds = open_raster(path, threads)?;
        if band < 1 || band as usize > ds.raster_count() {
            return Err(format!("{path}: no band {band}"));
        }
        let own = grid_of(&ds);
        let (used, one, src, b) = match target {
            Some(t) if !same_grid(&own, t) => {
                if !t.has_gt || t.gt[2] != 0.0 || t.gt[4] != 0.0 || t.gt[5] >= 0.0 || !own.has_gt {
                    return Err(format!(
                        "{path}: not on the grid of the strata, and it cannot be warped onto it (rotated or no transform)"
                    ));
                }
                // the band alone (a VRT), then warped onto the target grid: float, NaN where it has no data
                let bs = band.to_string();
                let targs = Args::new(&["-of".into(), "VRT".into(), "-b".into(), bs.clone()]);
                let empty = CString::new("").unwrap();
                // SAFETY: GDAL's C utilities on live handles; the results are owned by Datasets below.
                let one = unsafe {
                    gdal_sys::CPLErrorReset();
                    let to =
                        gdal_sys::GDALTranslateOptionsNew(targs.p.as_ptr() as *mut *mut c_char, std::ptr::null_mut());
                    let h = gdal_sys::GDALTranslate(empty.as_ptr(), ds.c_dataset(), to, std::ptr::null_mut());
                    gdal_sys::GDALTranslateOptionsFree(to);
                    if h.is_null() {
                        return Err(last_error(&format!("{path}: cannot read band {bs}")));
                    }
                    Dataset::from_c_dataset(h)
                };
                let mut a: Vec<String> =
                    ["-of", "VRT", "-r", "near", "-ot", "Float32", "-dstnodata", "nan"].map(String::from).to_vec();
                if !t.wkt.is_empty() && !own.wkt.is_empty() {
                    a.extend(["-t_srs".to_string(), t.wkt.clone()]);
                }
                a.push("-te".into());
                for v in [t.gt[0], t.gt[3] + t.h as f64 * t.gt[5], t.gt[0] + t.w as f64 * t.gt[1], t.gt[3]] {
                    a.push(crate::rules::fmt_g(v, 17));
                }
                a.extend(["-ts".to_string(), t.w.to_string(), t.h.to_string()]);
                let wargs = Args::new(&a);
                // SAFETY: as above.
                let warped = unsafe {
                    let wo =
                        gdal_sys::GDALWarpAppOptionsNew(wargs.p.as_ptr() as *mut *mut c_char, std::ptr::null_mut());
                    let mut srcs = [one.c_dataset()];
                    let mut usage = 0;
                    let h =
                        gdal_sys::GDALWarp(empty.as_ptr(), std::ptr::null_mut(), 1, srcs.as_mut_ptr(), wo, &mut usage);
                    gdal_sys::GDALWarpAppOptionsFree(wo);
                    if h.is_null() {
                        return Err(last_error(&format!("{path}: cannot warp it onto the grid of the strata")));
                    }
                    Dataset::from_c_dataset(h)
                };
                (warped, Some(one), Some(ds), 1)
            }
            _ => (ds, None, None, band as usize),
        };
        let (w, h) = used.raster_size();
        let nodata = used
            .rasterband(b)
            .map_err(|e| format!("{path}: {e}"))?
            .no_data_value()
            .filter(|v| !v.is_nan())
            .map(|v| v as f32);
        Ok(MapReader { used, _one: one, _src: src, band: b, w, h, nodata, own })
    }

    /// Rows y0..y0 + n into `out` (w * n floats), no data -> NaN.
    pub fn rows(&self, y0: usize, n: usize, out: &mut [f32]) -> Result<(), String> {
        let band = self.used.rasterband(self.band).map_err(|e| e.to_string())?;
        band.read_into_slice::<f32>((0, y0 as isize), (self.w, n), (self.w, n), &mut out[..self.w * n], None)
            .map_err(|e| e.to_string())?;
        if let Some(nd) = self.nodata {
            for v in &mut out[..self.w * n] {
                if *v == nd {
                    *v = f32::NAN;
                }
            }
        }
        Ok(())
    }

    pub fn own(&self) -> &Grid {
        &self.own
    }
}

/// The crossings of a polygon's rings with the centre line of each row, rows asked in increasing order (an
/// active edge list).
pub(crate) struct Spans {
    /// x0, y0 (lower), x1, y1
    edges: Vec<[f64; 4]>,
    active: Vec<usize>,
    xs: Vec<f64>,
    next: usize,
}

impl Spans {
    pub fn new(p: &Polygon) -> Spans {
        let mut edges = Vec::new();
        for r in p {
            let n = r.len() / 2;
            for i in 0..n {
                let (mut x0, mut y0) = (r[2 * i], r[2 * i + 1]);
                let (mut x1, mut y1) = (r[2 * ((i + 1) % n)], r[2 * ((i + 1) % n) + 1]);
                if y0 == y1 {
                    continue;
                }
                if y0 > y1 {
                    std::mem::swap(&mut x0, &mut x1);
                    std::mem::swap(&mut y0, &mut y1);
                }
                edges.push([x0, y0, x1, y1]);
            }
        }
        edges.sort_by(|a, b| a[1].total_cmp(&b[1]));
        Spans { edges, active: Vec::new(), xs: Vec::new(), next: 0 }
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Pixels whose centres are inside on row y: inside[x] = true.
    pub fn row(&mut self, y: usize, w: usize, inside: &mut Vec<bool>) {
        let yc = y as f64 + 0.5;
        while self.next < self.edges.len() && self.edges[self.next][1] <= yc {
            self.active.push(self.next);
            self.next += 1;
        }
        self.xs.clear();
        let mut k = 0;
        while k < self.active.len() {
            let e = self.edges[self.active[k]];
            if e[3] <= yc {
                self.active.swap_remove(k);
                continue;
            }
            self.xs.push(e[0] + (yc - e[1]) * (e[2] - e[0]) / (e[3] - e[1]));
            k += 1;
        }
        self.xs.sort_by(f64::total_cmp);
        inside.clear();
        inside.resize(w, false);
        for k in (0..self.xs.len().saturating_sub(1)).step_by(2) {
            // centres x + 0.5 in [a, b)
            let a = ((self.xs[k] - 0.5).ceil() as i64).max(0) as usize;
            let b = ((self.xs[k + 1] - 0.5).ceil() as i64).clamp(0, w as i64) as usize;
            for v in inside.iter_mut().take(b).skip(a) {
                *v = true;
            }
        }
    }
}

/// Areas of classes on one row: runs of a class added at once.
pub(crate) struct RowAcc {
    count: HashMap<i32, i64>,
    last: i32,
    run: i64,
}

impl Default for RowAcc {
    fn default() -> Self {
        RowAcc { count: HashMap::new(), last: NONE, run: 0 }
    }
}

impl RowAcc {
    #[inline]
    pub fn add(&mut self, c: i32) {
        if c == self.last {
            self.run += 1;
            return;
        }
        self.flush();
        self.last = c;
        self.run = 1;
    }

    fn flush(&mut self) {
        if self.run != 0 {
            *self.count.entry(self.last).or_insert(0) += self.run;
        }
        self.run = 0;
        self.last = NONE;
    }

    /// The row's pixels of each class, the accumulator emptied for the next row.
    pub fn take(&mut self) -> Vec<(i32, i64)> {
        self.flush();
        self.count.drain().collect()
    }
}

/// Rows per band: about 256 MB of floats, a multiple of 256 (the strata raster's tiles) when large.
pub(crate) fn band_rows(w: usize, maps: usize, halo: usize) -> usize {
    let t = TEST_BAND_ROWS.load(Ordering::Relaxed);
    if t > 0 {
        return t;
    }
    let budget: i64 = 256 << 20;
    let mut b = budget / (w.max(1) as i64 * (4 * maps as i64 + 8)) - 2 * halo as i64;
    b = b.clamp(1, 1024);
    if b >= 256 {
        b -= b % 256;
    }
    b as usize
}
