//! Every year at reduced resolution (src/embedding.cpp, EmbeddingStore): [year][pixel][dimension] Int8 with a
//! scale and offset per dimension (value = q * scale + offset), a validity flag and the norm of each vector. Read
//! in the background, a year per thread; the dimensions' ranges come first from a sample of every year.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use gdal::Dataset;
use gdal::raster::GdalDataType;
use janus_cube::CubeInfo;

/// A dimension's reading: nodata, scale and offset; `skip`: without data anywhere, read as 0.
#[derive(Clone, Copy, Default)]
pub(crate) struct BandInfo {
    no_data: Option<f64>,
    scale: f64,
    offset: f64,
    skip: bool,
}

pub(crate) fn band_info(ds: &Dataset, d: usize, base: usize, skip: &[u8]) -> Result<Vec<BandInfo>, String> {
    (0..d)
        .map(|b| {
            let rb = ds.rasterband(base + b + 1).map_err(|e| e.to_string())?;
            Ok(BandInfo {
                no_data: rb.no_data_value(),
                scale: rb.scale().unwrap_or(1.0),
                offset: rb.offset().unwrap_or(0.0),
                skip: skip.get(b).copied().unwrap_or(0) != 0,
            })
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ty {
    U8,
    I8,
    I16,
    U16,
    F32,
}

impl Ty {
    fn bytes(self) -> usize {
        match self {
            Ty::U8 | Ty::I8 => 1,
            Ty::I16 | Ty::U16 => 2,
            Ty::F32 => 4,
        }
    }

    fn gdal(self) -> gdal_sys::GDALDataType::Type {
        use gdal_sys::GDALDataType::*;
        match self {
            Ty::U8 => GDT_Byte,
            Ty::I8 => GDT_Int8,
            Ty::I16 => GDT_Int16,
            Ty::U16 => GDT_UInt16,
            Ty::F32 => GDT_Float32,
        }
    }
}

/// A file read one block at a time at full resolution, in its own type (Int16, Byte...), every band in one call:
/// each block is decompressed once. (Resampled or type-converting reads make GDAL go band by band on
/// pixel-interleaved files, decompressing a block once per band when the cache cannot hold every band's share:
/// 30 s instead of 0.5 s for TESSERA's 128 bands.)
pub(crate) struct BlockReader<'a> {
    ds: &'a Dataset,
    bi: &'a [BandInfo],
    ty: Ty,
    bands: Vec<i32>,
    raw: Vec<u8>,
    x0: usize,
    y0: usize,
    w: usize,
    pub width: usize,
    pub height: usize,
    pub bw: usize,
    pub bh: usize,
}

impl<'a> BlockReader<'a> {
    pub(crate) fn new(ds: &'a Dataset, bi: &'a [BandInfo], base: usize) -> Result<Self, String> {
        let (width, height) = ds.raster_size();
        let b1 = ds.rasterband(base + 1).map_err(|e| e.to_string())?;
        let (mut bw, mut bh) = b1.block_size();
        // strips or odd layouts: windows of a reasonable size
        if !(16..=1024).contains(&bw) {
            bw = width.min(if bw < 16 { 256 } else { 1024 });
        }
        if bh < 16 {
            bh = height.min(64);
        }
        let ty = match b1.band_type() {
            GdalDataType::UInt8 => Ty::U8,
            GdalDataType::Int8 => Ty::I8,
            GdalDataType::Int16 => Ty::I16,
            GdalDataType::UInt16 => Ty::U16,
            _ => Ty::F32,
        };
        let bands = (0..bi.len()).map(|b| (base + b + 1) as i32).collect();
        Ok(BlockReader { ds, bi, ty, bands, raw: Vec::new(), x0: 0, y0: 0, w: 0, width, height, bw, bh })
    }

    /// The window (x0, y0, w, h) at full resolution.
    pub(crate) fn read(&mut self, x0: usize, y0: usize, w: usize, h: usize) -> Result<(), String> {
        let (d, by) = (self.bi.len(), self.ty.bytes());
        (self.x0, self.y0, self.w) = (x0, y0, w);
        self.raw.resize(w * h * d * by, 0);
        let err = unsafe {
            gdal_sys::GDALDatasetRasterIOEx(
                self.ds.c_dataset(),
                gdal_sys::GDALRWFlag::GF_Read,
                x0 as i32,
                y0 as i32,
                w as i32,
                h as i32,
                self.raw.as_mut_ptr().cast(),
                w as i32,
                h as i32,
                self.ty.gdal(),
                d as i32,
                self.bands.as_mut_ptr(),
                (d * by) as i64,
                (w * d * by) as i64,
                by as i64,
                std::ptr::null_mut(),
            )
        };
        if err == gdal_sys::CPLErr::CE_None {
            Ok(())
        } else {
            Err(unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }.to_string_lossy().into_owned())
        }
    }

    /// The window last read, to decode its vectors (on any thread).
    pub(crate) fn window(&self) -> Window<'_> {
        Window { raw: &self.raw, bi: self.bi, ty: self.ty, x0: self.x0, y0: self.y0, w: self.w }
    }

    /// Source pixel (x, y) of the window last read: its D values, nodata -> NaN, scaled.
    pub(crate) fn vector(&self, x: usize, y: usize, out: &mut [f32]) {
        self.window().vector(x, y, out);
    }
}

/// A window read by a BlockReader: its raw values and how to decode them.
#[derive(Clone, Copy)]
pub(crate) struct Window<'a> {
    raw: &'a [u8],
    bi: &'a [BandInfo],
    ty: Ty,
    x0: usize,
    y0: usize,
    w: usize,
}

impl Window<'_> {
    /// Source pixel (x, y): its D values, nodata -> NaN, scaled.
    pub(crate) fn vector(&self, x: usize, y: usize, out: &mut [f32]) {
        let (d, by) = (self.bi.len(), self.ty.bytes());
        let p = &self.raw[((y - self.y0) * self.w + (x - self.x0)) * d * by..][..d * by];
        for (b, m) in self.bi.iter().enumerate() {
            if m.skip {
                out[b] = 0.0;
                continue;
            }
            let c = &p[b * by..(b + 1) * by];
            let x = match self.ty {
                Ty::U8 => f64::from(c[0]),
                Ty::I8 => f64::from(c[0] as i8),
                Ty::I16 => f64::from(i16::from_ne_bytes([c[0], c[1]])),
                Ty::U16 => f64::from(u16::from_ne_bytes([c[0], c[1]])),
                Ty::F32 => f64::from(f32::from_ne_bytes([c[0], c[1], c[2], c[3]])),
            };
            out[b] = if !x.is_finite() || m.no_data == Some(x) { f32::NAN } else { (x * m.scale + m.offset) as f32 };
        }
    }
}

/// A year of the store.
pub struct Year {
    pub q: Vec<i8>,
    pub valid: Vec<u8>,
    pub norm: Vec<f32>,
}

/// Each dimension's scale and offset; the dimensions without data in the whole sample (a spectrum's water
/// absorption bands: read as 0, so they neither count nor make a vector invalid).
pub struct Ranges {
    pub scale: Vec<f32>,
    pub offset: Vec<f32>,
    pub skip: Vec<u8>,
}

pub struct EmbeddingStore {
    pub info: Option<Arc<CubeInfo>>,
    pub w: usize,
    pub h: usize,
    pub t_n: usize,
    pub d: usize,
    /// source pixels per store pixel (x, y)
    pub fx: f64,
    pub fy: f64,
    ranges: OnceLock<Ranges>,
    years: Vec<OnceLock<Year>>,
    done: AtomicUsize,
    cancel: AtomicBool,
    error: Mutex<String>,
    seconds: Mutex<f64>,
}

impl EmbeddingStore {
    fn empty(info: Option<Arc<CubeInfo>>, w: usize, h: usize, t_n: usize, d: usize) -> Self {
        EmbeddingStore {
            info,
            w,
            h,
            t_n,
            d,
            fx: 1.0,
            fy: 1.0,
            ranges: OnceLock::new(),
            years: (0..t_n).map(|_| OnceLock::new()).collect(),
            done: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
            error: Mutex::new(String::new()),
            seconds: Mutex::new(0.0),
        }
    }

    /// Starts reading every year: `budget_bytes` for q + valid + norm of every year (the resolution follows).
    pub fn start(
        info: Arc<CubeInfo>,
        budget_bytes: u64,
        threads: usize,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        let (t_n, d) = (info.t(), info.bands_per_date);
        let (ww, hh) = (info.width as f64, info.height as f64);
        let per_pixel = t_n as f64 * (d + 1 + 4) as f64;
        let f = (ww * hh * per_pixel / budget_bytes.max(1 << 20) as f64).sqrt().max(1.0);
        let (w, h) = (((ww / f).ceil() as usize).max(1), ((hh / f).ceil() as usize).max(1));
        let mut s = Self::empty(Some(info), w, h, t_n, d);
        (s.fx, s.fy) = (ww / w as f64, hh / h as f64);
        let s = Arc::new(s);
        let st = s.clone();
        std::thread::Builder::new()
            .name("embeddings".into())
            .spawn(move || {
                let t0 = Instant::now();
                if let Err(e) = st.compute_ranges() {
                    *st.error.lock().unwrap() = e;
                    wake();
                    return;
                }
                wake();
                let next = AtomicUsize::new(0);
                std::thread::scope(|sc| {
                    for _ in 0..threads.clamp(1, st.t_n.max(1)) {
                        sc.spawn(|| {
                            loop {
                                let t = next.fetch_add(1, Ordering::Relaxed);
                                if t >= st.t_n || st.cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                if let Err(e) = st.read_year(t) {
                                    let mut er = st.error.lock().unwrap();
                                    if er.is_empty() {
                                        *er = e;
                                    }
                                } else if st.done.fetch_add(1, Ordering::AcqRel) + 1 == st.t_n {
                                    *st.seconds.lock().unwrap() = t0.elapsed().as_secs_f64();
                                }
                                wake();
                            }
                        });
                    }
                });
            })
            .expect("a thread for the embeddings");
        s
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    pub fn ranges(&self) -> Option<&Ranges> {
        self.ranges.get()
    }

    pub fn ranges_ready(&self) -> bool {
        self.ranges.get().is_some()
    }

    pub fn year(&self, t: usize) -> Option<&Year> {
        self.years.get(t)?.get()
    }

    pub fn year_ready(&self, t: usize) -> bool {
        self.year(t).is_some()
    }

    pub fn years_done(&self) -> usize {
        self.done.load(Ordering::Acquire)
    }

    pub fn complete(&self) -> bool {
        self.t_n > 0 && self.years_done() == self.t_n
    }

    pub fn error(&self) -> String {
        self.error.lock().unwrap().clone()
    }

    pub fn failed(&self) -> bool {
        !self.error.lock().unwrap().is_empty()
    }

    pub fn seconds(&self) -> f64 {
        *self.seconds.lock().unwrap()
    }

    pub fn pixels(&self) -> usize {
        self.w * self.h
    }

    pub fn bytes(&self) -> usize {
        self.pixels() * self.t_n * (self.d + 1 + 4)
    }

    /// The Int8 codes of vector p of year t (zeros where invalid).
    pub fn vec(&self, t: usize, p: usize) -> &[i8] {
        &self.year(t).expect("a year read").q[p * self.d..(p + 1) * self.d]
    }

    pub fn valid(&self, t: usize, p: usize) -> bool {
        self.year(t).is_some_and(|y| y.valid[p] != 0)
    }

    pub fn norm(&self, t: usize, p: usize) -> f32 {
        self.year(t).map_or(0.0, |y| y.norm[p])
    }

    pub fn dequant(&self, t: usize, p: usize, out: &mut [f32]) {
        let r = self.ranges().expect("the ranges");
        for ((o, &q), (s, f)) in out.iter_mut().zip(self.vec(t, p)).zip(r.scale.iter().zip(&r.offset)) {
            *o = f32::from(q) * s + f;
        }
    }

    /// The store pixel of a layer pixel (continuous coordinates); None outside.
    pub fn pixel_of(&self, lx: f64, ly: f64) -> Option<usize> {
        let (sx, sy) = ((lx / self.fx).floor(), (ly / self.fy).floor());
        (sx >= 0.0 && sy >= 0.0 && sx < self.w as f64 && sy < self.h as f64).then(|| sy as usize * self.w + sx as usize)
    }

    /// Each dimension's range from the 0.1 and 99.9 percentiles of a sample of every year (blocks spread over the
    /// image, each year at other places), widened by 5%: the Int8 steps cover every year, not just one. (From one
    /// year alone, 60% of another year's AlphaEarth vectors had a dimension clipped.)
    fn compute_ranges(&self) -> Result<(), String> {
        let info = self.info.as_ref().ok_or("no series")?;
        let (t_n, d) = (self.t_n, self.d);
        let mut buf: Vec<f32> = Vec::new(); // sampled vectors, pixel after pixel
        let mut v = vec![0f32; d];
        let cells = t_n.max(16); // blocks read in all
        let g = (cells as f64).sqrt().ceil() as usize; // on a g x g grid over the image
        let per_cell = 65536.0 / cells as f64; // vectors from each
        for t in 0..t_n {
            let l = &info.layers[t];
            let ds = Dataset::open(&l.path).map_err(|e| format!("{}: {e}", l.path))?;
            if ds.raster_count() < l.band_base as usize + d {
                return Err(format!("{}: fewer bands than the first year", l.path));
            }
            let bi = band_info(&ds, d, l.band_base as usize, &[])?;
            let mut rd = BlockReader::new(&ds, &bi, l.band_base as usize)?;
            let (nbx, nby) = (rd.width.div_ceil(rd.bw), rd.height.div_ceil(rd.bh));
            let mut seen: Vec<(usize, usize)> = Vec::new(); // blocks of this year already read (small images)
            for k in (t..cells).step_by(t_n) {
                // this year's cells: t, t + T...
                let bx = (((k % g) as f64 + 0.5) * nbx as f64 / g as f64) as usize;
                let by = (((k / g) as f64 + 0.5) * nby as f64 / g as f64) as usize;
                let (bx, by) = (bx.min(nbx - 1), by.min(nby - 1));
                if seen.contains(&(bx, by)) {
                    continue;
                }
                seen.push((bx, by));
                let (x0, y0) = (bx * rd.bw, by * rd.bh);
                let (w, h) = (rd.bw.min(rd.width - x0), rd.bh.min(rd.height - y0));
                rd.read(x0, y0, w, h).map_err(|e| format!("{}: {e}", l.path))?;
                let step = (((w * h) as f64 / per_cell).sqrt() as usize).max(1);
                for y in (y0..y0 + h).step_by(step) {
                    for x in (x0..x0 + w).step_by(step) {
                        rd.vector(x, y, &mut v);
                        buf.extend_from_slice(&v);
                    }
                }
            }
        }
        let n = buf.len() / d.max(1);
        let (mut scale, mut offset, mut skip) = (vec![1f32; d], vec![0f32; d], vec![0u8; d]);
        let mut col: Vec<f32> = Vec::new();
        for b in 0..d {
            col.clear();
            col.extend((0..n).map(|p| buf[p * d + b]).filter(|x| x.is_finite()));
            let (mut lo, mut hi) = (-1f32, 1f32);
            skip[b] = u8::from(col.is_empty()); // e.g. a spectrum's water absorption bands, no-data everywhere
            if !col.is_empty() {
                let i0 = (0.001 * (col.len() - 1) as f64) as usize;
                let i1 = (0.999 * (col.len() - 1) as f64) as usize;
                lo = *col.select_nth_unstable_by(i0, f32::total_cmp).1;
                hi = *col.select_nth_unstable_by(i1, f32::total_cmp).1;
            }
            let pad = (0.05 * (hi - lo)).max(1e-6);
            (lo, hi) = (lo - pad, hi + pad);
            offset[b] = 0.5 * (lo + hi);
            scale[b] = (hi - lo) / 254.0;
        }
        if skip.iter().all(|&s| s != 0) {
            skip.fill(0); // no data anywhere (e.g. embeddings of the sea): every vector invalid, not zeros
        }
        let _ = self.ranges.set(Ranges { scale, offset, skip });
        Ok(())
    }

    fn read_year(&self, t: usize) -> Result<(), String> {
        let info = self.info.as_ref().ok_or("no series")?;
        let r = self.ranges().ok_or("no ranges")?;
        let (d, n) = (self.d, self.pixels());
        let l = &info.layers[t];
        let ds = Dataset::open(&l.path).map_err(|e| format!("{}: {e}", l.path))?;
        if ds.raster_count() < l.band_base as usize + d {
            return Err(format!("{}: fewer bands than the first year", l.path));
        }
        let mut q: Vec<i8> = Vec::new();
        q.try_reserve_exact(n * d).map_err(|_| "not enough memory for the embeddings".to_string())?;
        q.resize(n * d, 0);
        let (mut valid, mut norm) = (vec![0u8; n], vec![0f32; n]);
        let bi = band_info(&ds, d, l.band_base as usize, &r.skip)?;
        let mut rd = BlockReader::new(&ds, &bi, l.band_base as usize)?;
        // the source pixel nearest to each store pixel's centre
        let src_x: Vec<usize> =
            (0..self.w).map(|x| (((x as f64 + 0.5) * self.fx) as usize).min(rd.width - 1)).collect();
        let src_y: Vec<usize> =
            (0..self.h).map(|y| (((y as f64 + 0.5) * self.fy) as usize).min(rd.height - 1)).collect();
        let inv: Vec<f32> = r.scale.iter().map(|s| 1.0 / s).collect();
        let mut v = vec![0f32; d];
        for y0 in (0..rd.height).step_by(rd.bh) {
            let y1 = rd.height.min(y0 + rd.bh);
            let (r0, r1) = (src_y.partition_point(|&s| s < y0), src_y.partition_point(|&s| s < y1));
            if r0 >= r1 {
                continue; // no store row samples this block row
            }
            for x0 in (0..rd.width).step_by(rd.bw) {
                if self.cancel.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let x1 = rd.width.min(x0 + rd.bw);
                let (c0, c1) = (src_x.partition_point(|&s| s < x0), src_x.partition_point(|&s| s < x1));
                if c0 >= c1 {
                    continue;
                }
                rd.read(x0, y0, x1 - x0, y1 - y0).map_err(|e| format!("{}: {e}", l.path))?;
                for row in r0..r1 {
                    for c in c0..c1 {
                        rd.vector(src_x[c], src_y[row], &mut v);
                        let p = row * self.w + c;
                        let qq = &mut q[p * d..(p + 1) * d];
                        let mut nn = 0f64;
                        let mut ok = true;
                        for b in 0..d {
                            if !v[b].is_finite() {
                                ok = false;
                                break;
                            }
                            let k = ((v[b] - r.offset[b]) * inv[b]).round_ties_even().clamp(-127.0, 127.0);
                            qq[b] = k as i8;
                            let x = f64::from(k) * f64::from(r.scale[b]) + f64::from(r.offset[b]);
                            nn += x * x;
                        }
                        if !ok {
                            qq.fill(0);
                        }
                        valid[p] = u8::from(ok);
                        norm[p] = if ok { nn.sqrt() as f32 } else { 0.0 };
                    }
                }
            }
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let _ = self.years[t].set(Year { q, valid, norm });
        Ok(())
    }

    /// A store filled in memory (the self-test): T years of w x h vectors from `gen(t, p, v)`, every dimension
    /// over [-range, range].
    pub fn synthetic(
        w: usize,
        h: usize,
        t_n: usize,
        d: usize,
        range: f32,
        mut make: impl FnMut(usize, usize, &mut [f32]),
    ) -> Self {
        let s = Self::empty(None, w, h, t_n, d);
        let scale = vec![range / 127.0; d];
        let mut v = vec![0f32; d];
        for t in 0..t_n {
            let n = w * h;
            let (mut q, valid, mut norm) = (vec![0i8; n * d], vec![1u8; n], vec![0f32; n]);
            for p in 0..n {
                make(t, p, &mut v);
                let mut nn = 0f64;
                for b in 0..d {
                    let k = (v[b] / scale[b]).round_ties_even().clamp(-127.0, 127.0) as i8;
                    q[p * d + b] = k;
                    nn += f64::from(f32::from(k) * scale[b]).powi(2);
                }
                norm[p] = nn.sqrt() as f32;
            }
            let _ = s.years[t].set(Year { q, valid, norm });
            s.done.fetch_add(1, Ordering::Relaxed);
        }
        let _ = s.ranges.set(Ranges { scale, offset: vec![0.0; d], skip: vec![0; d] });
        s
    }
}

impl Drop for EmbeddingStore {
    fn drop(&mut self) {
        self.cancel();
    }
}
