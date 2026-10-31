//! A pixel-interleaved GeoTIFF's blocks (crate::interleaved_blocks) decompressed here rather than by GDAL: GDAL
//! splits a block into its bands whole, band by band, each a pass over the block with a stride of every band (875
//! Float64 bands of a CCDC stack: 3.5 GB of memory read for a block of 459 MB), where an overview or a series needs
//! a few of its pixels. The block's bytes are read from the file at the offset GDAL reports, decompressed by GDAL's
//! own codec (zlib for DEFLATE, zstd), and only the pixels asked for are taken out, every band of each together.
//! Without a predictor, in DEFLATE or ZSTD; anything else is left to GDAL.

use std::fs::File;
use std::io::Read;

use gdal::Metadata;

use crate::{BandMeta, CubeInfo};

#[derive(Clone, Copy)]
enum Sample {
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    F32,
    F64,
}

impl Sample {
    fn bytes(self) -> usize {
        match self {
            Sample::U8 | Sample::I8 => 1,
            Sample::U16 | Sample::I16 => 2,
            Sample::U32 | Sample::I32 | Sample::F32 => 4,
            Sample::F64 => 8,
        }
    }
}

pub(crate) struct RawBlocks {
    file: File,
    codec: janus_gdal::Codec,
    pub bands: usize,
    pub block: (usize, usize),
    blocks_x: usize,
    height: usize,
    sample: Sample,
    little: bool,
    /// each block's offset and size in the file, row by row
    places: Vec<(u64, usize)>,
    /// each band's no data, scale and offset
    pub metas: Vec<BandMeta>,
    /// the compressed block read last
    comp: SysBuf,
}

/// Bytes from the system's allocator rather than the program's (mimalloc kept the blocks of every reader, GBs,
/// after they were freed): a block of hundreds of MB goes back to the system with the reader.
pub(crate) struct SysBuf {
    ptr: *mut u8,
    len: usize,
    cap: usize,
}

// SAFETY: it owns its bytes, as a Vec<u8> does.
unsafe impl Send for SysBuf {}
unsafe impl Sync for SysBuf {}

impl SysBuf {
    pub(crate) const fn new() -> Self {
        SysBuf { ptr: std::ptr::NonNull::dangling().as_ptr(), len: 0, cap: 0 }
    }

    /// `len` bytes, what they hold unspecified (kept when the buffer does not grow).
    fn set_len(&mut self, len: usize) {
        use std::alloc::{GlobalAlloc, Layout, System};
        if len > self.cap {
            self.free();
            // SAFETY: a layout of non-zero size; the pointer is checked.
            let p = unsafe { System.alloc(Layout::from_size_align(len, 16).unwrap()) };
            assert!(!p.is_null(), "out of memory for a block of {len} bytes");
            (self.ptr, self.cap) = (p, len);
        }
        self.len = len;
    }

    fn free(&mut self) {
        use std::alloc::{GlobalAlloc, Layout, System};
        if self.cap > 0 {
            // SAFETY: allocated by set_len with this size and alignment.
            unsafe { System.dealloc(self.ptr, Layout::from_size_align(self.cap, 16).unwrap()) };
            (self.ptr, self.len, self.cap) = (std::ptr::NonNull::dangling().as_ptr(), 0, 0);
        }
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        // SAFETY: len <= cap bytes allocated (or a dangling pointer for none), initialized by their writer.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        // SAFETY: as bytes(), uniquely borrowed.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for SysBuf {
    fn drop(&mut self) {
        self.free();
    }
}

/// Reads exactly `buf.len()` bytes at `offset`.
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            match f.seek_read(&mut buf[done..], offset + done as u64) {
                Ok(0) | Err(_) => return false,
                Ok(n) => done += n,
            }
        }
        true
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, offset).is_ok()
    }
}

impl RawBlocks {
    /// The series' file read so, if it can be.
    pub(crate) fn open(info: &CubeInfo) -> Option<RawBlocks> {
        let ((bw, bh), _) = crate::interleaved_blocks(info)?;
        let path = &info.layers[0].path;
        let ds = crate::open_raster(path).ok()?;
        let codec = match ds.metadata_item("COMPRESSION", "IMAGE_STRUCTURE")?.to_uppercase().as_str() {
            "DEFLATE" => janus_gdal::Codec::get("zlib")?,
            "ZSTD" => janus_gdal::Codec::get("zstd")?,
            _ => return None,
        };
        if ds.metadata_item("PREDICTOR", "IMAGE_STRUCTURE").is_some_and(|p| p.trim() != "1") {
            return None;
        }
        let rb = ds.rasterband(1).ok()?;
        use gdal_sys::GDALDataType as T;
        let sample = match unsafe { gdal_sys::GDALGetRasterDataType(rb.c_rasterband()) } {
            T::GDT_Byte => Sample::U8,
            T::GDT_Int8 => Sample::I8,
            T::GDT_UInt16 => Sample::U16,
            T::GDT_Int16 => Sample::I16,
            T::GDT_UInt32 => Sample::U32,
            T::GDT_Int32 => Sample::I32,
            T::GDT_Float32 => Sample::F32,
            T::GDT_Float64 => Sample::F64,
            _ => return None,
        };
        let mut file = File::open(path).ok()?;
        let mut head = [0u8; 2];
        file.read_exact(&mut head).ok()?;
        let little = match &head {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let (w, h) = ds.raster_size();
        let (blocks_x, blocks_y) = (w.div_ceil(bw), h.div_ceil(bh));
        let mut places = Vec::with_capacity(blocks_x * blocks_y);
        for by in 0..blocks_y {
            for bx in 0..blocks_x {
                let item = |k: &str| rb.metadata_item(&format!("BLOCK_{k}_{bx}_{by}"), "TIFF")?.parse().ok();
                places.push((item("OFFSET")?, item("SIZE")? as usize));
            }
        }
        let n = ds.raster_count();
        let metas = (1..=n).map(|b| ds.rasterband(b).map(|rb| crate::band_meta(&rb))).collect::<Result<_, _>>().ok()?;
        Some(RawBlocks {
            file,
            codec,
            bands: n,
            block: (bw, bh),
            blocks_x,
            height: h,
            sample,
            little,
            places,
            metas,
            comp: SysBuf::new(),
        })
    }

    /// Decompresses block (bx, by) into `out`: its rows (a last strip has fewer), None if it cannot (no data written
    /// for it: GDAL fills it).
    pub(crate) fn decode(&mut self, (bx, by): (usize, usize), out: &mut SysBuf) -> Option<usize> {
        let (off, size) = *self.places.get(by * self.blocks_x + bx)?;
        if off == 0 || size == 0 {
            return None;
        }
        self.comp.set_len(size);
        if !read_at(&self.file, self.comp.bytes_mut(), off) {
            return None;
        }
        let (bw, bh) = self.block;
        let row_bytes = bw * self.bands * self.sample.bytes();
        // a tile is whole; a strip at the bottom holds the rows left
        for rows in [bh, bh.min(self.height.saturating_sub(by * bh))] {
            out.set_len(rows * row_bytes);
            if rows > 0 && self.codec.decompress(self.comp.bytes(), out.bytes_mut()) {
                return Some(rows);
            }
        }
        None
    }

    /// Every band of the pixels (column, row in the block) of a block decompressed: `out[b * n + p]` band b of pixel
    /// p, as GDAL converts it to Float32.
    pub(crate) fn extract(&self, data: &[u8], pixels: &[(usize, usize)], out: &mut Vec<f32>) {
        let (n, bands, bw) = (pixels.len(), self.bands, self.block.0);
        out.resize(bands * n, 0.0);
        // 32 pixels at a time, band by band: each band's values written side by side
        let take = |out: &mut Vec<f32>, size: usize, value: &dyn Fn(&[u8]) -> f32| {
            for (k, chunk) in pixels.chunks(32).enumerate() {
                let bases: Vec<usize> = chunk.iter().map(|&(c, r)| (r * bw + c) * bands * size).collect();
                for b in 0..bands {
                    let row = &mut out[b * n + k * 32..b * n + k * 32 + chunk.len()];
                    for (v, &base) in row.iter_mut().zip(&bases) {
                        *v = value(&data[base + b * size..base + (b + 1) * size]);
                    }
                }
            }
        };
        let le = self.little;
        macro_rules! num {
            ($t:ty) => {
                |s: &[u8]| {
                    let a = s.try_into().unwrap();
                    (if le { <$t>::from_le_bytes(a) } else { <$t>::from_be_bytes(a) }) as f32
                }
            };
        }
        match self.sample {
            Sample::U8 => take(out, 1, &|s| s[0] as f32),
            Sample::I8 => take(out, 1, &|s| s[0] as i8 as f32),
            Sample::U16 => take(out, 2, &num!(u16)),
            Sample::I16 => take(out, 2, &num!(i16)),
            Sample::U32 => take(out, 4, &num!(u32)),
            Sample::I32 => take(out, 4, &num!(i32)),
            Sample::F32 => take(out, 4, &num!(f32)),
            Sample::F64 => take(out, 8, &num!(f64)),
        }
    }
}
