//! The whole cube at full resolution on the local disk (src/fullres_cache.cpp): exact series, ROI and detail tiles
//! without touching the source files, which may sit on an HDD. Stored date by date in 64×64 blocks, each
//! compressed on its own and losslessly (float bits as integers, difference with the previous pixel, zigzag,
//! bytes split into 4 planes, zstd through GDAL's compressor registry). A series reads one small block per date;
//! an ROI or a tile reads the blocks it covers.
//!
//! The file is the C++'s `.janusfull`: the same name (key), header, flags, block tables and blocks, compressed by
//! the same zstd (GDAL's), so either program reads and resumes what the other wrote. A date's blocks are written
//! first, then its table, then its flag: a write cut short is never trusted.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

use janus_cube::{CubeInfo, CubeReader};
use janus_gdal::Codec;

use crate::disk;
use crate::overview::cube_cache_key;

pub const BLOCK: usize = 64;
const MAGIC: [u8; 8] = *b"JANUSFR1";
/// magic, key, W, H, T, block, codec, reserved, table offset, data offset
const HEADER_BYTES: u64 = 8 + 8 + 6 * 4 + 8 + 8;
const ZSTD: i32 = 1;
const ZLIB: i32 = 2;

fn codec_name(c: i32) -> &'static str {
    if c == ZSTD { "zstd" } else { "zlib" }
}

/// Reads exactly `buf.len()` bytes at `offset`, without moving a shared file position.
fn read_at(f: &File, buf: &mut [u8], offset: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.read_exact_at(buf, offset)
    }
}

fn write_at(f: &File, buf: &[u8], offset: u64) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = f.seek_write(&buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            done += n;
        }
        Ok(())
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt;
        f.write_all_at(buf, offset)
    }
}

/// Lossless block coding. Neighbouring floats share sign, exponent and high mantissa bits: as integers their
/// differences are small, zigzag makes them positive, and splitting the 4 bytes into planes puts the (nearly
/// constant) high bytes together. Measured on Landsat NDVI: 1.42× with zstd level 1, against 1.31× for zstd on
/// shuffled bytes alone and 1.17× for zstd alone.
pub fn to_planes(v: &[f32], p: &mut [u8]) {
    let n = v.len();
    let mut prev = 0u32;
    for (i, x) in v.iter().enumerate() {
        let u = x.to_bits();
        let d = u.wrapping_sub(prev);
        prev = u;
        let z = (d << 1) ^ 0u32.wrapping_sub(d >> 31);
        p[i] = z as u8;
        p[n + i] = (z >> 8) as u8;
        p[2 * n + i] = (z >> 16) as u8;
        p[3 * n + i] = (z >> 24) as u8;
    }
}

pub fn from_planes(p: &[u8], v: &mut [f32]) {
    let n = v.len();
    let mut prev = 0u32;
    for (i, x) in v.iter_mut().enumerate() {
        let z =
            u32::from(p[i]) | u32::from(p[n + i]) << 8 | u32::from(p[2 * n + i]) << 16 | u32::from(p[3 * n + i]) << 24;
        prev = prev.wrapping_add((z >> 1) ^ 0u32.wrapping_sub(z & 1));
        *x = f32::from_bits(prev);
    }
}

/// Source column of each buffer pixel when reading (off, size) into n pixels: GDAL's nearest neighbour exactly
/// (GDALRasterBand::IRasterIO: columns by accumulated increments, with a 1e-10 epsilon), so a subsampled read from
/// the cache gives what the source gives.
pub fn source_columns(off: usize, size: usize, n: usize, raster: usize) -> Vec<usize> {
    if n == size {
        return (off..off + n).collect();
    }
    let inc = size as f64 / n as f64;
    let mut d = 0.5 * inc + off as f64 + 1e-10;
    (0..n)
        .map(|_| {
            let v = d.max(0.0).min((raster - 1) as f64) as usize;
            d += inc;
            v
        })
        .collect()
}

/// Rows: by multiplication (GDAL's), with the same epsilon.
pub fn source_rows(off: usize, size: usize, n: usize, raster: usize) -> Vec<usize> {
    if n == size {
        return (off..off + n).collect();
    }
    let inc = size as f64 / n as f64;
    (0..n).map(|i| ((i as f64 + 0.5) * inc + off as f64 + 1e-10).max(0.0).min((raster - 1) as f64) as usize).collect()
}

/// Byte range of date t's blocks in its file, from the GeoTIFF block offsets; None if unknown (another driver, not
/// a local file).
fn date_byte_range(info: &CubeInfo, t: usize) -> Option<(u64, u64)> {
    use gdal::Metadata;
    let l = &info.layers[t];
    if !Path::new(&l.path).is_file() {
        return None;
    }
    let ds = janus_cube::open_raster(&l.path).ok()?;
    if ds.driver().short_name() != "GTiff" {
        return None;
    }
    let rb = ds.rasterband(l.band as usize).ok()?;
    let (bw, bh) = rb.block_size();
    let (nx, ny) = (info.width.div_ceil(bw), info.height.div_ceil(bh));
    let item = |k: String| rb.metadata_item(&k, "TIFF").and_then(|v| v.parse::<u64>().ok());
    let begin = item("BLOCK_OFFSET_0_0".into())?;
    let end = item(format!("BLOCK_OFFSET_{}_{}", nx - 1, ny - 1))? + item(format!("BLOCK_SIZE_{}_{}", nx - 1, ny - 1))?;
    (end > begin).then_some((begin, end))
}

/// HDD: reads a date's bytes ahead of GDAL in large sequential chunks (at most LEAD ahead of what was decoded), so
/// the disk streams while GDAL decodes from the OS cache in parallel; decoders wait for it (a decoder reading on its
/// own would pull the head away).
struct Prefetcher {
    begin: u64,
    end: u64,
    state: Mutex<PrefetchState>,
    cv: Condvar,
}

#[derive(Default)]
struct PrefetchState {
    used: u64,
    pos: u64,
    stop: bool,
    done: bool,
}

impl Prefetcher {
    const CHUNK: usize = 4 << 20;
    const LEAD: u64 = 128 << 20;

    fn start(path: &str, begin: u64, end: u64) -> Arc<Prefetcher> {
        let p = Arc::new(Prefetcher {
            begin,
            end,
            state: Mutex::new(PrefetchState { used: begin, ..Default::default() }),
            cv: Condvar::new(),
        });
        let (q, path) = (p.clone(), path.to_string());
        std::thread::Builder::new().name("fullres prefetch".into()).spawn(move || q.run(&path)).expect("thread");
        p
    }

    fn run(&self, path: &str) {
        let f = File::open(path).ok();
        let mut buf = vec![0u8; Self::CHUNK];
        let mut pos = self.begin;
        let mut ok = f.is_some();
        while ok && pos < self.end {
            {
                let s =
                    self.cv.wait_while(self.state.lock().unwrap(), |s| !s.stop && pos >= s.used + Self::LEAD).unwrap();
                if s.stop {
                    break;
                }
            }
            let n = (Self::CHUNK as u64).min(self.end - pos) as usize;
            ok = read_at(f.as_ref().unwrap(), &mut buf[..n], pos).is_ok();
            pos += n as u64;
            self.state.lock().unwrap().pos = pos;
            self.cv.notify_all();
        }
        self.state.lock().unwrap().done = true;
        self.cv.notify_all();
    }

    fn at(&self, f: f64) -> u64 {
        self.begin + (f * (self.end - self.begin) as f64) as u64
    }

    /// Blocks until the first fraction f of the range was read (or reading stopped).
    fn wait_for(&self, f: f64) {
        let want = self.at(f);
        let mut s = self.state.lock().unwrap();
        if want > s.used {
            // the lead never keeps it from what a decoder waits for
            s.used = want;
            self.cv.notify_all();
        }
        drop(self.cv.wait_while(s, |s| !s.done && s.pos < want).unwrap());
    }

    /// The decoders have gone through this fraction of the range.
    fn progress(&self, f: f64) {
        let mut s = self.state.lock().unwrap();
        s.used = s.used.max(self.at(f));
        self.cv.notify_all();
    }

    fn stop(&self) {
        self.state.lock().unwrap().stop = true;
        self.cv.notify_all();
    }
}

/// What start() settled: the file, its layout and codec.
struct Setup {
    path: PathBuf,
    codec: Codec,
    codec_name: &'static str,
    table_offset: u64,
    data_offset: u64,
    /// the overview pass reads full dates (HDD, source without overviews)
    integrate: bool,
    reader: File,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DateState {
    Missing,
    Building,
    Done,
    Failed,
}

pub struct FullResCache {
    info: Arc<CubeInfo>,
    cache_dir: PathBuf,
    w: usize,
    h: usize,
    t: usize,
    nbx: usize,
    nby: usize,
    nb: usize,
    setup: OnceLock<Setup>,
    retained: Mutex<Option<PathBuf>>,
    decode_threads: AtomicUsize,
    read_only: AtomicBool,
    done: Box<[AtomicBool]>,
    n_done: AtomicUsize,
    /// [t * nb + b]: block offsets and compressed sizes (raw size = stored as is); a date's are written before its
    /// done flag is set (release) and read after it is seen (acquire)
    off: Box<[AtomicU64]>,
    size: Box<[AtomicU32]>,
    state: Mutex<(Vec<DateState>, String)>,
    /// open while dates are missing
    writer: Mutex<Option<File>>,
    end: AtomicU64,
    started: AtomicBool,
    stop: AtomicBool,
    with_overview: AtomicUsize,
    build_start: Mutex<Option<Instant>>,
    build_seconds: Mutex<f64>,
    series_hits: AtomicUsize,
    last_series_us: AtomicU64,
    window_hits: AtomicUsize,
    window_us_sum: AtomicU64,
}

thread_local! {
    static SCRATCH: std::cell::RefCell<(Vec<u8>, Vec<u8>, Vec<f32>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new(), Vec::new())) };
}

impl FullResCache {
    pub fn new(info: Arc<CubeInfo>, cache_dir: PathBuf) -> Arc<Self> {
        let (w, h, t) = (info.width, info.height, info.t());
        let (nbx, nby) = (w.div_ceil(BLOCK), h.div_ceil(BLOCK));
        let nb = nbx * nby;
        Arc::new(FullResCache {
            info,
            cache_dir,
            w,
            h,
            t,
            nbx,
            nby,
            nb,
            setup: OnceLock::new(),
            retained: Mutex::new(None),
            decode_threads: AtomicUsize::new(1),
            read_only: AtomicBool::new(false),
            done: (0..t.max(1)).map(|_| AtomicBool::new(false)).collect(),
            n_done: AtomicUsize::new(0),
            off: (0..t * nb).map(|_| AtomicU64::new(0)).collect(),
            size: (0..t * nb).map(|_| AtomicU32::new(0)).collect(),
            state: Mutex::new((vec![DateState::Missing; t], String::new())),
            writer: Mutex::new(None),
            end: AtomicU64::new(0),
            started: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            with_overview: AtomicUsize::new(0),
            build_start: Mutex::new(None),
            build_seconds: Mutex::new(0.0),
            series_hits: AtomicUsize::new(0),
            last_series_us: AtomicU64::new(0),
            window_hits: AtomicUsize::new(0),
            window_us_sum: AtomicU64::new(0),
        })
    }

    fn block_w(&self, bx: usize) -> usize {
        BLOCK.min(self.w - bx * BLOCK)
    }

    fn block_h(&self, by: usize) -> usize {
        BLOCK.min(self.h - by * BLOCK)
    }

    pub fn has_date(&self, t: usize) -> bool {
        t < self.t && self.done[t].load(Ordering::Acquire)
    }

    pub fn dates_done(&self) -> usize {
        self.n_done.load(Ordering::Acquire)
    }

    pub fn complete(&self) -> bool {
        self.t > 0 && self.dates_done() == self.t
    }

    pub fn started(&self) -> bool {
        self.started.load(Ordering::Acquire)
    }

    pub fn building(&self) -> bool {
        self.started()
            && !self.stop.load(Ordering::Relaxed)
            && !self.read_only.load(Ordering::Relaxed)
            && !self.complete()
    }

    pub fn read_only(&self) -> bool {
        self.read_only.load(Ordering::Relaxed)
    }

    pub fn error(&self) -> String {
        self.state.lock().unwrap().1.clone()
    }

    pub fn path(&self) -> Option<&Path> {
        self.setup.get().map(|s| s.path.as_path())
    }

    pub fn codec(&self) -> &'static str {
        self.setup.get().map_or("", |s| s.codec_name)
    }

    /// File size (the data written).
    pub fn bytes(&self) -> u64 {
        self.end.load(Ordering::Relaxed)
    }

    pub fn build_seconds(&self) -> f64 {
        *self.build_seconds.lock().unwrap()
    }

    pub fn dates_with_overview(&self) -> usize {
        self.with_overview.load(Ordering::Relaxed)
    }

    pub fn series_hits(&self) -> usize {
        self.series_hits.load(Ordering::Relaxed)
    }

    pub fn last_series_ms(&self) -> f64 {
        self.last_series_us.load(Ordering::Relaxed) as f64 / 1000.0
    }

    pub fn avg_window_ms(&self) -> f64 {
        let n = self.window_hits.load(Ordering::Relaxed);
        if n == 0 { 0.0 } else { self.window_us_sum.load(Ordering::Relaxed) as f64 / 1000.0 / n as f64 }
    }

    /// Stops building (the date being read is dropped); what is done stays readable.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Stops, with a reason shown until resumed.
    pub fn stop_because(&self, why: &str) {
        self.state.lock().unwrap().1 = why.to_string();
        self.stop();
    }

    /// From the dates written, else the lossless ratio measured on Landsat NDVI (1.4).
    fn bytes_for(&self, dates: usize) -> u64 {
        let done = self.dates_done();
        let data = self.setup.get().map_or(0, |s| s.data_offset);
        let end = self.end.load(Ordering::Relaxed);
        let per =
            if done > 0 && end > data { (end - data) as f64 / done as f64 } else { (self.w * self.h * 4) as f64 / 1.4 };
        (per * dates as f64) as u64
    }

    pub fn estimated_bytes(&self) -> u64 {
        self.setup.get().map_or(0, |s| s.data_offset) + self.bytes_for(self.t)
    }

    /// Opens (or creates) the cache file; `submit_build` then builds the missing dates. `decode_threads` > 1 (HDD):
    /// each date is streamed by one reader and decoded by that many threads. Also resumes a stopped build.
    /// `budget`: the disk cache budget setting (bytes, 0 = automatic), within which the missing dates must fit.
    /// False if it cannot be used: see error().
    pub fn start(&self, decode_threads: usize, budget: u64) -> bool {
        if self.started() {
            // resume after stop(), if the missing dates still fit
            if self.read_only() || self.writer.lock().unwrap().is_none() {
                return self.complete();
            }
            let room = disk::make_room(&self.cache_dir, budget, self.bytes_for(self.t - self.dates_done()));
            let mut st = self.state.lock().unwrap();
            st.1 = room.clone().err().unwrap_or_default();
            if room.is_err() {
                return false;
            }
            for s in &mut st.0 {
                if *s == DateState::Failed {
                    *s = DateState::Missing;
                }
            }
            self.stop.store(false, Ordering::Relaxed);
            return true;
        }
        if self.t == 0 {
            return false;
        }
        self.decode_threads.store(decode_threads.max(1), Ordering::Relaxed);
        let (codec_id, codec) = match Codec::get("zstd") {
            Some(c) => (ZSTD, c),
            None => match Codec::get("zlib") {
                Some(c) => (ZLIB, c),
                None => {
                    self.state.lock().unwrap().1 = "GDAL has neither zstd nor zlib".into();
                    return false;
                }
            },
        };
        let mut salt = MAGIC.to_vec();
        for v in [BLOCK as i32, codec_id, 0, 0] {
            salt.extend_from_slice(&v.to_le_bytes());
        }
        let key = cube_cache_key(&self.info, &salt);
        let path = self.cache_dir.join(format!("{key:016x}.{}", disk::FULLRES_EXT));
        {
            let mut r = self.retained.lock().unwrap();
            if r.is_none() {
                disk::retain(&path);
                *r = Some(path.clone());
            }
        }
        let (t, nb) = (self.t, self.nb);
        let table_offset = HEADER_BYTES + t as u64;
        let data_offset = table_offset + (t * nb * 12) as u64;

        // HDD, source without internal overviews: the overview pass reads full dates and builds both caches at
        // once (on an SSD the overview comes first: it is CPU-bound there, and full dates cost 3x more to decode).
        let integrate = decode_threads > 1
            && janus_cube::open_raster(&self.info.layers[0].path).ok().is_some_and(|ds| {
                let b = (self.info.layers[0].band.max(1) as usize).min(ds.raster_count());
                ds.rasterband(b).ok().is_some_and(|rb| rb.overview_count().unwrap_or(0) == 0)
            });

        // Dates already in the file.
        let mut valid = false;
        let mut valid_end = data_offset;
        if let Ok(f) = File::open(&path) {
            let file_size = f.metadata().map_or(0, |m| m.len());
            let mut hd = [0u8; HEADER_BYTES as usize];
            if read_at(&f, &mut hd, 0).is_ok() {
                let i32_at = |o: usize| i32::from_le_bytes(hd[o..o + 4].try_into().unwrap());
                let u64_at = |o: usize| u64::from_le_bytes(hd[o..o + 8].try_into().unwrap());
                if hd[..8] == MAGIC
                    && u64_at(8) == key
                    && i32_at(16) == self.w as i32
                    && i32_at(20) == self.h as i32
                    && i32_at(24) == t as i32
                    && i32_at(28) == BLOCK as i32
                    && i32_at(32) == codec_id
                    && u64_at(40) == table_offset
                    && u64_at(48) == data_offset
                {
                    let mut flags = vec![0u8; t];
                    valid = read_at(&f, &mut flags, HEADER_BYTES).is_ok();
                    let mut table = vec![0u8; nb * 12];
                    for (d, &flag) in flags.iter().enumerate() {
                        if !valid || flag == 0 || read_at(&f, &mut table, table_offset + (d * nb * 12) as u64).is_err()
                        {
                            continue;
                        }
                        let off = |b: usize| u64::from_le_bytes(table[b * 8..b * 8 + 8].try_into().unwrap());
                        let size = |b: usize| {
                            u32::from_le_bytes(table[nb * 8 + b * 4..nb * 8 + b * 4 + 4].try_into().unwrap())
                        };
                        // an entry past the end of the file is not trusted
                        let ok = (0..nb)
                            .all(|b| off(b) >= data_offset && off(b) + u64::from(size(b)) <= file_size && size(b) > 0);
                        if !ok {
                            continue;
                        }
                        for b in 0..nb {
                            self.off[d * nb + b].store(off(b), Ordering::Relaxed);
                            self.size[d * nb + b].store(size(b), Ordering::Relaxed);
                            valid_end = valid_end.max(off(b) + u64::from(size(b)));
                        }
                        self.state.lock().unwrap().0[d] = DateState::Done;
                        self.done[d].store(true, Ordering::Release);
                        self.n_done.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }
        }
        self.end.store(valid_end, Ordering::Relaxed);

        let open_reader = || File::open(&path).ok();
        if self.complete() {
            disk::touch(&path); // the least recently used goes first
            let Some(reader) = open_reader() else { return false };
            let _ = self.setup.set(Setup {
                path,
                codec,
                codec_name: codec_name(codec_id),
                table_offset,
                data_offset,
                integrate,
                reader,
            });
            self.started.store(true, Ordering::Release);
            return true;
        }

        let fail = |why: String, reader: Option<File>, setup: &OnceLock<Setup>| {
            self.state.lock().unwrap().1 = why;
            self.read_only.store(true, Ordering::Relaxed);
            // what is there stays readable
            if self.dates_done() > 0
                && let Some(reader) = reader
            {
                let _ = setup.set(Setup {
                    path: path.clone(),
                    codec,
                    codec_name: codec_name(codec_id),
                    table_offset,
                    data_offset,
                    integrate: false,
                    reader,
                });
                self.started.store(true, Ordering::Release);
            }
            self.started()
        };
        // Room for the missing dates in the disk cache budget (older caches pruned to make it).
        let need = if valid { 0 } else { data_offset } + self.bytes_for(t - self.dates_done());
        if let Err(why) = disk::make_room(&self.cache_dir, budget, need) {
            return fail(why, open_reader(), &self.setup);
        }

        // Kept open, and closed to other writers, while dates are missing.
        let writer = if valid {
            crate::overview::open_for_writing(&path, false).ok()
        } else {
            crate::overview::open_for_writing(&path, true).ok().and_then(|f| {
                let mut hd = Vec::with_capacity(data_offset as usize);
                hd.extend_from_slice(&MAGIC);
                hd.extend_from_slice(&key.to_le_bytes());
                for v in [self.w as i32, self.h as i32, t as i32, BLOCK as i32, codec_id, 0] {
                    hd.extend_from_slice(&v.to_le_bytes());
                }
                hd.extend_from_slice(&table_offset.to_le_bytes());
                hd.extend_from_slice(&data_offset.to_le_bytes());
                hd.resize(data_offset as usize, 0);
                write_at(&f, &hd, 0).ok().map(|_| f)
            })
        };
        let Some(writer) = writer else {
            let why = if valid {
                "in use by another Janus window (read only)".to_string()
            } else {
                format!("could not write {}", path.display())
            };
            return fail(why, open_reader(), &self.setup);
        };
        if !valid {
            self.end.store(data_offset, Ordering::Relaxed);
        }
        let Some(reader) = open_reader() else { return false };
        *self.writer.lock().unwrap() = Some(writer);
        disk::touch(&path);
        let _ = self.setup.set(Setup {
            path,
            codec,
            codec_name: codec_name(codec_id),
            table_offset,
            data_offset,
            integrate,
            reader,
        });
        *self.build_start.lock().unwrap() = Some(Instant::now());
        self.started.store(true, Ordering::Release);
        true
    }

    /// The missing dates to build after the overview's (the C++ submits them at priority 20 to the background
    /// pool): call `build_next` from that many jobs.
    pub fn missing_dates(&self) -> usize {
        if !self.started() || self.read_only() || self.writer.lock().unwrap().is_none() {
            return 0;
        }
        self.state.lock().unwrap().0.iter().filter(|&&s| s == DateState::Missing).count()
    }

    /// Builds the next missing date (a job of the build).
    pub fn build_next(&self) {
        if self.stop.load(Ordering::Relaxed) {
            return;
        }
        let t = {
            let mut st = self.state.lock().unwrap();
            let Some(t) = st.0.iter().position(|&s| s == DateState::Missing) else { return };
            st.0[t] = DateState::Building;
            t
        };
        self.build_date(t, None);
    }

    fn claim(&self, t: usize) -> bool {
        let mut st = self.state.lock().unwrap();
        if st.0[t] != DateState::Missing {
            return false;
        }
        st.0[t] = DateState::Building;
        true
    }

    /// Overview pass: reads date t at full resolution once for both caches and fills `ov` (ow × oh, GDAL's nearest
    /// neighbour). None: not handled here (date already cached or being built, or the cache is off); Some(ok).
    pub fn build_with_overview(&self, t: usize, ov: &mut [f32], ow: usize, oh: usize) -> Option<bool> {
        let integrate = self.setup.get().is_some_and(|s| s.integrate);
        if !self.started() || self.stop.load(Ordering::Relaxed) || self.read_only() || !integrate || !self.claim(t) {
            return None;
        }
        let ok = self.build_date(t, Some((ov, ow, oh)));
        if ok {
            self.with_overview.fetch_add(1, Ordering::Relaxed);
        }
        Some(ok)
    }

    fn release(&self, t: usize, ok: bool) {
        {
            let mut st = self.state.lock().unwrap();
            st.0[t] = if ok {
                DateState::Done
            } else if self.stop.load(Ordering::Relaxed) {
                DateState::Missing
            } else {
                DateState::Failed
            };
        }
        if !ok {
            return;
        }
        self.done[t].store(true, Ordering::Release);
        if self.n_done.fetch_add(1, Ordering::AcqRel) + 1 == self.t {
            self.finish();
        }
    }

    fn finish(&self) {
        if let Some(s) = *self.build_start.lock().unwrap() {
            *self.build_seconds.lock().unwrap() = s.elapsed().as_secs_f64();
        }
        let mut w = self.writer.lock().unwrap();
        if let Some(f) = w.take() {
            // drop what an interrupted build left past the data
            let end = self.end.load(Ordering::Relaxed);
            if f.metadata().is_ok_and(|m| m.len() > end) {
                let _ = f.set_len(end);
            }
        }
    }

    /// Reads date t in bands of BLOCK rows and stores each band's blocks. On an HDD a Prefetcher streams the file
    /// in order while several threads, each with its own GDAL handles, decode and compress the bands from the OS
    /// cache (GDAL's own multi-threaded decoding was measured at 0.6-1.0 s per date against 0.25 s this way).
    /// Returns whether the date was read (for the overview), stored or not.
    fn build_date(&self, t: usize, ov: Option<(&mut [f32], usize, usize)>) -> bool {
        let Some(setup) = self.setup.get() else {
            self.release(t, false);
            return false;
        };
        let threads = self.decode_threads.load(Ordering::Relaxed);
        let prefetch = if threads > 1 {
            date_byte_range(&self.info, t).map(|(a, b)| Prefetcher::start(&self.info.layers[t].path, a, b))
        } else {
            None
        };
        let with_ov = ov.is_some();
        let (xs, ys, ow) = match &ov {
            Some((_, ow, oh)) => (source_columns(0, self.w, *ow, self.w), source_rows(0, self.h, *oh, self.h), *ow),
            None => (Vec::new(), Vec::new(), 0),
        };
        let ov = Mutex::new(ov.map(|o| o.0));
        let next = AtomicUsize::new(0);
        let bands_done = AtomicUsize::new(0);
        let read_failed = AtomicBool::new(false);
        let write_failed = AtomicBool::new(false);
        let work = || {
            let mut reader = CubeReader::new(self.info.clone());
            let mut band = Vec::new();
            loop {
                let by = next.fetch_add(1, Ordering::Relaxed);
                if by >= self.nby
                    || read_failed.load(Ordering::Relaxed)
                    || (self.stop.load(Ordering::Relaxed) && !with_ov)
                {
                    break;
                }
                let (y0, bh) = (by * BLOCK, self.block_h(by));
                band.resize(self.w * bh, 0.0);
                if let Some(p) = &prefetch {
                    p.wait_for((by + 1) as f64 / self.nby as f64);
                }
                if !reader.read_window(t, (0, y0, self.w, bh), &mut band, (self.w, bh)) {
                    read_failed.store(true, Ordering::Relaxed);
                    break;
                }
                // the overview rows taken from this band (GDAL's nearest neighbour)
                if with_ov {
                    let mut o = ov.lock().unwrap();
                    let o = o.as_mut().unwrap();
                    let first = ys.partition_point(|&y| y < y0);
                    for (j, &y) in ys.iter().enumerate().skip(first).take_while(|(_, y)| **y < y0 + bh) {
                        let src = &band[(y - y0) * self.w..];
                        for (i, &x) in xs.iter().enumerate() {
                            o[j * ow + i] = src[x];
                        }
                    }
                }
                // when stopped the overview still needs the rest of the date
                if !self.stop.load(Ordering::Relaxed)
                    && !write_failed.load(Ordering::Relaxed)
                    && !self.store_band(setup, t, by, &band)
                {
                    write_failed.store(true, Ordering::Relaxed);
                }
                if let Some(p) = &prefetch {
                    p.progress((bands_done.fetch_add(1, Ordering::Relaxed) + 1) as f64 / self.nby as f64);
                }
            }
        };
        std::thread::scope(|s| {
            for _ in 1..threads {
                s.spawn(work);
            }
            work();
        });
        if let Some(p) = &prefetch {
            p.stop();
        }
        let read_ok = !read_failed.load(Ordering::Relaxed);
        let mut ok = read_ok && !self.stop.load(Ordering::Relaxed) && !write_failed.load(Ordering::Relaxed);
        if ok {
            let mut w = self.writer.lock().unwrap();
            let nb = self.nb;
            let mut table = Vec::with_capacity(nb * 12);
            for b in 0..nb {
                table.extend_from_slice(&self.off[t * nb + b].load(Ordering::Relaxed).to_le_bytes());
            }
            for b in 0..nb {
                table.extend_from_slice(&self.size[t * nb + b].load(Ordering::Relaxed).to_le_bytes());
            }
            // the blocks are with the system already (written without a buffer), then the table, then the flag: a
            // build cut short never leaves a flag before its data (the C++'s fflush, not an fsync, which made
            // the build ~6x slower holding the writer's lock while the disk caught up)
            ok = w.as_ref().is_some_and(|f| {
                write_at(f, &table, setup.table_offset + (t * nb * 12) as u64).is_ok()
                    && write_at(f, &[1], HEADER_BYTES + t as u64).is_ok()
            });
            if !ok {
                *w = None;
            }
        }
        if read_ok && !ok && !self.stop.load(Ordering::Relaxed) {
            let mut st = self.state.lock().unwrap();
            if st.1.is_empty() {
                st.1 = "could not write the cache (disk full?)".into();
            }
        }
        self.release(t, ok);
        read_ok
    }

    /// Compresses the blocks of band `by` (W × block height floats) and appends them.
    fn store_band(&self, setup: &Setup, t: usize, by: usize, band: &[f32]) -> bool {
        let bh = self.block_h(by);
        let mut out = Vec::new();
        let mut sizes = vec![0u32; self.nbx];
        SCRATCH.with_borrow_mut(|(a, packed, f)| {
            for (bx, size) in sizes.iter_mut().enumerate() {
                let bw = self.block_w(bx);
                let n = bw * bh;
                let raw = n * 4;
                f.clear();
                for j in 0..bh {
                    let s = j * self.w + bx * BLOCK;
                    f.extend_from_slice(&band[s..s + bw]);
                }
                a.resize(raw, 0);
                to_planes(f, a);
                // stored as is when compression does not pay (the size tells: raw size)
                let packed_ok =
                    setup.codec.compress(a, packed, raw + raw / 8 + 1024, &["LEVEL=1"]) && packed.len() < raw;
                out.extend_from_slice(if packed_ok { packed } else { a });
                *size = if packed_ok { packed.len() } else { raw } as u32;
            }
        });
        let w = self.writer.lock().unwrap();
        let Some(f) = w.as_ref() else { return false };
        let base = self.end.load(Ordering::Relaxed);
        if write_at(f, &out, base).is_err() {
            return false;
        }
        let mut o = base;
        for (bx, &size) in sizes.iter().enumerate() {
            let i = t * self.nb + by * self.nbx + bx;
            self.off[i].store(o, Ordering::Relaxed);
            self.size[i].store(size, Ordering::Relaxed);
            o += u64::from(size);
        }
        self.end.store(o, Ordering::Relaxed);
        true
    }

    fn load_block(&self, setup: &Setup, t: usize, b: usize, out: &mut [f32]) -> bool {
        let i = t * self.nb + b;
        let (offset, size) = (self.off[i].load(Ordering::Relaxed), self.size[i].load(Ordering::Relaxed) as usize);
        let (bx, by) = (b % self.nbx, b / self.nbx);
        let n = self.block_w(bx) * self.block_h(by);
        let raw = n * 4;
        SCRATCH.with_borrow_mut(|(a, packed, _)| {
            packed.resize(size, 0);
            if read_at(&setup.reader, packed, offset).is_err() {
                return false;
            }
            let planes: &[u8] = if size != raw {
                a.resize(raw, 0);
                if !setup.codec.decompress(packed, a) {
                    return false;
                }
                a
            } else {
                packed
            };
            from_planes(planes, &mut out[..n]);
            true
        })
    }

    /// Pixel (x, y) of every cached date into out[t]; got[t] where read. Returns the number of dates read.
    pub fn read_series(&self, x: usize, y: usize, out: &mut [f32], got: &mut [bool]) -> usize {
        let Some(setup) = self.setup.get() else { return 0 };
        if self.dates_done() == 0 || x >= self.w || y >= self.h {
            return 0;
        }
        let start = Instant::now();
        let (bx, by) = (x / BLOCK, y / BLOCK);
        let idx = (y - by * BLOCK) * self.block_w(bx) + (x - bx * BLOCK);
        let mut block = vec![0f32; BLOCK * BLOCK];
        let mut n = 0;
        for t in 0..self.t {
            if !self.has_date(t) || !self.load_block(setup, t, by * self.nbx + bx, &mut block) {
                continue;
            }
            out[t] = block[idx];
            got[t] = true;
            n += 1;
        }
        if n > 0 {
            self.series_hits.fetch_add(1, Ordering::Relaxed);
            self.last_series_us.store(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        }
        n
    }

    /// As CubeReader::read_window (the same pixels, also when subsampled); false if date t is not cached.
    pub fn read_window(
        &self,
        t: usize,
        (x, y, w, h): (usize, usize, usize, usize),
        buf: &mut [f32],
        (bw, bh): (usize, usize),
    ) -> bool {
        let Some(setup) = self.setup.get() else { return false };
        if !self.has_date(t) || w == 0 || h == 0 || bw == 0 || bh == 0 {
            return false;
        }
        let start = Instant::now();
        let xs = source_columns(x, w, bw, self.w);
        let ys = source_rows(y, h, bh, self.h);
        let mut block = vec![0f32; BLOCK * BLOCK];
        // source rows and columns grow with the buffer's: walk them block by block
        let mut j0 = 0;
        while j0 < bh {
            let by = ys[j0] / BLOCK;
            let j1 = j0 + ys[j0..].iter().take_while(|&&v| v / BLOCK == by).count();
            let mut i0 = 0;
            while i0 < bw {
                let bx = xs[i0] / BLOCK;
                let i1 = i0 + xs[i0..].iter().take_while(|&&v| v / BLOCK == bx).count();
                if !self.load_block(setup, t, by * self.nbx + bx, &mut block) {
                    return false;
                }
                let (stride, x0) = (self.block_w(bx), bx * BLOCK);
                for j in j0..j1 {
                    let src = &block[(ys[j] - by * BLOCK) * stride..];
                    let dst = &mut buf[j * bw..];
                    for i in i0..i1 {
                        dst[i] = src[xs[i] - x0];
                    }
                }
                i0 = i1;
            }
            j0 = j1;
        }
        self.window_hits.fetch_add(1, Ordering::Relaxed);
        self.window_us_sum.fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        true
    }

    /// Removes every full-resolution cache not in use; returns the bytes freed.
    pub fn clear(cache_dir: &Path) -> u64 {
        let mut freed = 0;
        for e in std::fs::read_dir(cache_dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != disk::FULLRES_EXT) || disk::in_use(&p) {
                continue;
            }
            let size = e.metadata().map_or(0, |m| m.len());
            if std::fs::remove_file(&p).is_ok() {
                freed += size;
            }
        }
        freed
    }
}

impl Drop for FullResCache {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(p) = self.retained.lock().unwrap().take() {
            disk::release(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planes_round_trip() {
        let v: Vec<f32> =
            (0..4096).map(|i| if i % 97 == 0 { f32::NAN } else { (i as f32 * 0.37).sin() * 1e3 }).collect();
        let mut p = vec![0u8; v.len() * 4];
        to_planes(&v, &mut p);
        let mut back = vec![0f32; v.len()];
        from_planes(&p, &mut back);
        assert!(v.iter().zip(&back).all(|(a, b)| a.to_bits() == b.to_bits()));
    }

    #[test]
    fn nearest_as_gdal() {
        // 1:1 is the window itself; 3 into 2 takes columns 0 and 2 (0.75 + 1e-10 and 2.25 + 1e-10)
        assert_eq!(source_columns(5, 4, 4, 100), vec![5, 6, 7, 8]);
        assert_eq!(source_columns(0, 3, 2, 3), vec![0, 2]);
        assert_eq!(source_rows(0, 3, 2, 3), vec![0, 2]);
        // never past the raster
        assert_eq!(source_columns(0, 10, 3, 10).last(), Some(&8));
    }
}
