//! Many dates of a file whose blocks hold all its bands (pixel-interleaved, janus_cube::interleaved_blocks), read
//! block by block: every date of a block before the next, so each block is decompressed once (GDAL keeps the block
//! last decompressed; a date read whole would decompress every block of every band).

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

use janus_cube::{CubeInfo, CubeReader};

use crate::fullres::{source_columns, source_rows};

/// A window of the source read into `w` × `h` pixels, split by the source's blocks.
pub struct BlockSampling {
    /// source column of each output column, row of each row: GDAL's nearest neighbour, as one read of the window
    xs: Vec<usize>,
    ys: Vec<usize>,
    pub w: usize,
    pub h: usize,
    /// the source blocks holding output pixels (x, y, w, h)
    pub windows: Vec<(usize, usize, usize, usize)>,
}

/// One block's pixels of one date (its output rows by columns), to put into an output of the window's size.
pub struct Scatter<'a> {
    w: usize,
    cols: Range<usize>,
    rows: Range<usize>,
    values: &'a [f32],
}

impl Scatter<'_> {
    /// The output rows it holds.
    pub fn rows(&self) -> Range<usize> {
        self.rows.clone()
    }

    /// The output columns it holds.
    pub fn cols(&self) -> Range<usize> {
        self.cols.clone()
    }

    /// Its values of output row `oy` (one of rows()), over cols().
    pub fn row(&self, oy: usize) -> &[f32] {
        let (n, a) = (self.cols.len(), oy - self.rows.start);
        &self.values[a * n..(a + 1) * n]
    }

    pub fn put(&self, out: &mut [f32]) {
        let n = self.cols.len();
        for (a, oy) in self.rows.clone().enumerate() {
            out[oy * self.w + self.cols.start..oy * self.w + self.cols.end]
                .copy_from_slice(&self.values[a * n..(a + 1) * n]);
        }
    }
}

/// Usable physical memory (bytes), GDAL's estimate.
pub fn usable_ram() -> u64 {
    u64::try_from(unsafe { gdal_sys::CPLGetUsablePhysicalRAM() }).unwrap_or(0)
}

/// Readers at once on blocks of `block_bytes` decompressed each (GDAL keeps one per reader, and as much again in its
/// block cache): an eighth of the memory
/// at most, one at least.
pub fn readers_for(block_bytes: u64, threads: usize) -> usize {
    threads.min(((usable_ram() / 8) / block_bytes.max(1)).max(1) as usize).max(1)
}

impl BlockSampling {
    /// The window (x, y, w, h) of `info`'s source read into `out` pixels, by source blocks of `block`.
    pub fn new(
        info: &CubeInfo,
        (x, y, w, h): (usize, usize, usize, usize),
        out: (usize, usize),
        block: (usize, usize),
    ) -> Self {
        let (sw, sh) = (info.width, info.height);
        let mut s = BlockSampling {
            xs: source_columns(x, w, out.0, sw),
            ys: source_rows(y, h, out.1, sh),
            w: out.0,
            h: out.1,
            windows: Vec::new(),
        };
        let (kw, kh) = (block.0.max(1), block.1.max(1));
        for y0 in (y / kh * kh..y + h).step_by(kh) {
            for x0 in (x / kw * kw..x + w).step_by(kw) {
                let win = (x0, y0, kw.min(sw - x0), kh.min(sh - y0));
                let (cols, rows) = s.ranges(win);
                if !cols.is_empty() && !rows.is_empty() {
                    s.windows.push(win);
                }
            }
        }
        s
    }

    /// The output columns and rows a source block holds.
    fn ranges(&self, (x0, y0, bw, bh): (usize, usize, usize, usize)) -> (Range<usize>, Range<usize>) {
        let range = |v: &[usize], a: usize, n: usize| v.partition_point(|&s| s < a)..v.partition_point(|&s| s < a + n);
        (range(&self.xs, x0, bw), range(&self.ys, y0, bh))
    }

    /// Block `k` of each date of `dates`, each date's pixels given to `put` (its index in `dates`): the pixels it
    /// samples, every band of them decompressed here once (CubeReader::sample_pixels), else the rows and columns
    /// they span read date by date through GDAL (the block decompressed by the first read, the others served from
    /// it). False if a read failed or `stop` was set.
    pub fn read(
        &self,
        r: &mut CubeReader,
        k: usize,
        dates: &[usize],
        values: &mut Vec<f32>,
        stop: &AtomicBool,
        mut put: impl FnMut(usize, &Scatter),
    ) -> bool {
        let (cols, rows) = self.ranges(self.windows[k]);
        values.resize(cols.len() * rows.len(), 0.0);
        let pixels: Vec<(usize, usize)> =
            rows.clone().flat_map(|oy| cols.clone().map(move |ox| (self.xs[ox], self.ys[oy]))).collect();
        if r.sample_pixels(&pixels) {
            for (j, &i) in dates.iter().enumerate() {
                if stop.load(Ordering::Relaxed) || !r.sampled_values(i, values) {
                    return false;
                }
                put(j, &Scatter { w: self.w, cols: cols.clone(), rows: rows.clone(), values });
            }
            return true;
        }
        let (c0, r0) = (self.xs[cols.start], self.ys[rows.start]);
        let (cw, rh) = (self.xs[cols.end - 1] + 1 - c0, self.ys[rows.end - 1] + 1 - r0);
        let mut tile = vec![0f32; cw * rh];
        for (j, &i) in dates.iter().enumerate() {
            if stop.load(Ordering::Relaxed) || !r.read_window(i, (c0, r0, cw, rh), &mut tile, (cw, rh)) {
                return false;
            }
            for (v, &(x, y)) in values.iter_mut().zip(&pixels) {
                *v = tile[(y - r0) * cw + x - c0];
            }
            put(j, &Scatter { w: self.w, cols: cols.clone(), rows: rows.clone(), values });
        }
        true
    }
}
