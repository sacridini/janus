//! The whole cube at reduced resolution, [t][y][x] float32 (NaN = nodata), kept in RAM for instant series and
//! statistics and sent to the GPU. Built in parallel, one job per date, following the date the user looks at,
//! and cached on disk date by date (src/overview.cpp). The cache file is the C++'s `.januscube`: the same name
//! (its key), header, index of the dates written and layout, so both programs read what the other wrote.
//!
//! Not yet here (phase 2): the disk cache budget (disk_cache.cpp), one reader on a hard disk, and building the
//! full-resolution cache in the same read.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use janus_cube::{CubeInfo, with_thread_reader};

use crate::disk;
use crate::fullres::FullResCache;
use crate::pool::JobPool;

const MAGIC: [u8; 8] = *b"TSVCUBE1";
/// magic, key, w, h, T, reserved (1: the index of the dates written follows)
const HEADER_BYTES: u64 = 8 + 8 + 4 * 4;
pub const EXTENSION: &str = ".januscube";

fn fnv1a(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h = (h ^ u64::from(b)).wrapping_mul(1099511628211);
    }
    h
}

/// The last modification of a file as the C++ hashes it: `std::filesystem::last_write_time(...)
/// .time_since_epoch().count()`, whose clock differs by standard library.
fn mtime_count(meta: &std::fs::Metadata) -> i64 {
    #[cfg(windows)]
    {
        // MSVC: 100 ns ticks since 1601, the FILETIME itself
        use std::os::windows::fs::MetadataExt;
        meta.last_write_time() as i64
    }
    #[cfg(not(windows))]
    {
        let ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as i64);
        // libstdc++'s file clock counts nanoseconds from 2174-01-01; libc++'s from 1970
        if cfg!(target_os = "linux") { ns - 6_437_664_000 * 1_000_000_000 } else { ns }
    }
}

/// Hash of the files behind a cube (paths, sizes, dates, nodata/scale) seeded with `salt`: the name of its caches
/// (overview.cpp, cubeCacheKey).
pub fn cube_cache_key(info: &CubeInfo, salt: &[u8]) -> u64 {
    let mut k = fnv1a(1469598103934665603, salt);
    for l in &info.layers {
        k = fnv1a(k, l.path.as_bytes());
        k = fnv1a(k, &l.band.to_le_bytes());
        // the file holding the layer (a NetCDF variable's .nc); GDAL's stat for network paths (a HEAD request,
        // cached by GDAL: it was just opened)
        let file = janus_cube::dataset_file(&l.path);
        let (size, mtime) = if janus_cube::is_remote_path(file) || file.starts_with("/vsi") {
            vsi_stat(file)
        } else {
            std::fs::metadata(file).map_or((0u64, 0i64), |m| (m.len(), mtime_count(&m)))
        };
        k = fnv1a(k, &size.to_le_bytes());
        k = fnv1a(k, &mtime.to_le_bytes());
        for v in [l.meta.no_data.unwrap_or(0.0), l.meta.scale, l.meta.offset] {
            k = fnv1a(k, &v.to_le_bytes());
        }
    }
    // the normalized difference and the quality mask change the values (hashed only when in use, so caches of plain
    // series stay valid)
    let sel = &info.sel;
    if sel.nd_band > 0 || sel.qa_band > 0 {
        for v in [sel.band, sel.nd_band, sel.qa_band, sel.qa_rule.code()] {
            k = fnv1a(k, &v.to_le_bytes());
        }
        for l in &info.layers {
            for v in [l.nd_meta.no_data.unwrap_or(0.0), l.nd_meta.scale, l.nd_meta.offset] {
                k = fnv1a(k, &v.to_le_bytes());
            }
        }
    }
    k
}

/// The size and modification time (seconds) of a file in one of GDAL's virtual file systems.
fn vsi_stat(file: &str) -> (u64, i64) {
    janus_gdal::vsi_stat(file).unwrap_or((0, 0))
}

/// The overview's size: as large as `budget_bytes` allows for T floats per pixel plus 8 (statistics over time, of
/// two dates or more), never larger than the source nor than `max_texture` on a side.
pub fn overview_size(width: usize, height: usize, t: usize, budget_bytes: i64, max_texture: usize) -> (usize, usize) {
    let (w, h) = (width as f64, height as f64);
    let stats = if t >= 2 { 8.0 } else { 0.0 };
    let max_px = budget_bytes as f64 / (4.0 * (t as f64 + stats));
    let s = (max_px / (w * h)).sqrt().min(1.0).min(max_texture as f64 / w.max(h));
    (((w * s).round() as usize).max(1), ((h * s).round() as usize).max(1))
}

fn cache_key(info: &CubeInfo, w: usize, h: usize) -> u64 {
    let mut salt = MAGIC.to_vec();
    salt.extend_from_slice(&(w as i32).to_le_bytes());
    salt.extend_from_slice(&(h as i32).to_le_bytes());
    cube_cache_key(info, &salt)
}

/// Where the cache lives by default: the C++'s folder (%LOCALAPPDATA%\Janus\cache, ~/Library/Caches/Janus,
/// $XDG_CACHE_HOME/janus).
pub fn default_cache_dir() -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Janus").join("cache"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME").map(|d| PathBuf::from(d).join("Library/Caches/Janus"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".cache")))
        .map(|d| d.join("janus"));
    base.unwrap_or_else(std::env::temp_dir)
}

pub struct OverviewOptions {
    pub budget_bytes: i64,
    pub max_texture: usize,
    pub cache_dir: PathBuf,
    /// the disk cache budget setting (bytes, 0 = automatic; disk.rs), within which the cache file must fit
    pub cache_budget: u64,
    /// built in the same read of each date, when it integrates with the overview (HDD)
    pub full_res: Option<Arc<FullResCache>>,
}

/// Opens a cache file for writing, closed to other writers meanwhile (Windows; the C++ opens it `_SH_DENYWR`).
pub(crate) fn open_for_writing(path: &Path, create: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true);
    if create {
        o.create(true).truncate(true);
        // the cache folder of a first run (e.g. ~/.cache/janus on a new Linux account)
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(1); // FILE_SHARE_READ
    }
    o.open(path)
}

pub struct Overview {
    pub info: Arc<CubeInfo>,
    pub w: usize,
    pub h: usize,
    pub t: usize,
    /// source pixels per overview pixel
    pub factor: f64,
    layers: Vec<OnceLock<Box<[f32]>>>,
    claimed: Mutex<Vec<bool>>,
    focus: AtomicUsize,
    /// the series closed: no more dates read
    abandoned: AtomicBool,
    ready: Mutex<Vec<usize>>,
    done: AtomicUsize,
    failed: AtomicUsize,
    from_cache: AtomicBool,
    cached_layers: AtomicUsize,
    cache_path: PathBuf,
    /// why there is no cache file (no room in the budget), if so
    cache_error: Mutex<String>,
    /// the cache file while dates are written to it, and the offset of the first date
    cache: Mutex<Option<(File, u64)>>,
    full_res: Option<Arc<FullResCache>>,
    started: Instant,
    build_seconds: Mutex<Option<f64>>,
    on_ready: Box<dyn Fn() + Send + Sync>,
}

impl Overview {
    /// Starts loading or building the overview in `pool` (the C++'s priorities: the cache 0, building 10, the
    /// focused date first); `on_ready` is called whenever dates are ready (to wake the interface).
    pub fn start(
        info: Arc<CubeInfo>,
        opts: &OverviewOptions,
        pool: &JobPool,
        on_ready: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Self> {
        let t = info.t();
        let (w, h) = overview_size(info.width, info.height, t, opts.budget_bytes, opts.max_texture);
        let key = cache_key(&info, w, h);
        let cache_path = opts.cache_dir.join(format!("{key:016x}{EXTENSION}"));
        disk::retain(&cache_path);
        let n = w * h;

        // Dates already in the cache: all of them in a complete file without an index (the first format), the
        // indexed ones in a file written date by date (a build that was interrupted resumes where it stopped).
        let mut have = vec![false; t];
        let mut data_offset = HEADER_BYTES + t as u64;
        let mut indexed = false;
        if let Ok(mut f) = File::open(&cache_path) {
            let size = f.metadata().map_or(0, |m| m.len());
            let mut hd = [0u8; HEADER_BYTES as usize];
            if f.read_exact(&mut hd).is_ok() {
                let int = |o: usize| i32::from_le_bytes(hd[o..o + 4].try_into().unwrap());
                let matches = hd[..8] == MAGIC
                    && u64::from_le_bytes(hd[8..16].try_into().unwrap()) == key
                    && int(16) == w as i32
                    && int(20) == h as i32
                    && int(24) == t as i32;
                let reserved = int(28);
                if matches && reserved == 0 && size == HEADER_BYTES + (n * t * 4) as u64 {
                    have.fill(true);
                    data_offset = HEADER_BYTES;
                } else if matches && reserved == 1 {
                    let mut idx = vec![0u8; t];
                    if f.read_exact(&mut idx).is_ok() {
                        indexed = true;
                        for (i, &b) in idx.iter().enumerate() {
                            // an entry past the end of the file is not trusted
                            have[i] = b == 1 && data_offset + (n * 4 * (i + 1)) as u64 <= size;
                        }
                    }
                }
            }
        }
        let cached = have.iter().filter(|&&x| x).count();
        let ov = Arc::new(Overview {
            factor: info.width as f64 / w as f64,
            info,
            w,
            h,
            t,
            layers: (0..t).map(|_| OnceLock::new()).collect(),
            claimed: Mutex::new(have.clone()),
            focus: AtomicUsize::new(0),
            abandoned: AtomicBool::new(false),
            ready: Mutex::new(Vec::new()),
            done: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            from_cache: AtomicBool::new(cached == t),
            cached_layers: AtomicUsize::new(cached),
            cache_path,
            cache_error: Mutex::new(String::new()),
            cache: Mutex::new(None),
            full_res: opts.full_res.clone(),
            started: Instant::now(),
            build_seconds: Mutex::new(None),
            on_ready: Box::new(on_ready),
        });

        if cached == t {
            // the least recently opened goes first (disk.rs)
            disk::touch(&ov.cache_path);
            // read date by date so that the GPU receives dates while the rest loads
            let o = ov.clone();
            pool.submit(0, move || {
                let mut f = File::open(&o.cache_path).ok();
                let ok = f.as_mut().is_some_and(|f| f.seek(SeekFrom::Start(data_offset)).is_ok());
                let mut ok = ok;
                for i in 0..o.t {
                    if ok {
                        let mut v = vec![0f32; n];
                        ok = f.as_mut().unwrap().read_exact(bytemuck::cast_slice_mut(&mut v)).is_ok();
                        if ok {
                            let _ = o.layers[i].set(v.into_boxed_slice());
                        }
                    }
                    if !ok {
                        o.failed.fetch_add(1, Ordering::Relaxed);
                    }
                    o.mark_done(i);
                }
            });
            return ov;
        }

        // Room for the missing dates in the disk cache budget (older caches pruned to make it); without it the
        // overview is built in memory only.
        let full = HEADER_BYTES + t as u64 + (n * 4 * t) as u64;
        let on_disk = if indexed { std::fs::metadata(&ov.cache_path).map_or(0, |m| m.len()) } else { 0 };
        let room = disk::make_room(&opts.cache_dir, opts.cache_budget, full.saturating_sub(on_disk));
        if let Err(why) = &room {
            *ov.cache_error.lock().unwrap() = why.clone();
        }
        // The cache file, made or reopened for the missing dates.
        let file = if room.is_err() {
            None
        } else if indexed {
            open_for_writing(&ov.cache_path, false).ok()
        } else {
            have.fill(false);
            open_for_writing(&ov.cache_path, true).ok().and_then(|mut f| {
                let mut hd = Vec::with_capacity(HEADER_BYTES as usize + t);
                hd.extend_from_slice(&MAGIC);
                hd.extend_from_slice(&key.to_le_bytes());
                for v in [w as i32, h as i32, t as i32, 1] {
                    hd.extend_from_slice(&v.to_le_bytes());
                }
                hd.resize(HEADER_BYTES as usize + t, 0);
                (f.write_all(&hd).is_ok() && f.flush().is_ok()).then_some(f)
            })
        };
        if file.is_none() {
            have.fill(false); // another session writes it, or the disk refused: no cache
        }
        *ov.claimed.lock().unwrap() = have.clone();
        ov.cached_layers.store(have.iter().filter(|&&x| x).count(), Ordering::Relaxed);
        *ov.cache.lock().unwrap() = file.map(|f| (f, data_offset));

        if have.iter().any(|&x| x) {
            let o = ov.clone();
            pool.submit(0, move || {
                for i in (0..o.t).filter(|&i| have[i]) {
                    if o.load_layer(i) {
                        o.mark_done(i);
                    } else {
                        o.decode(i); // unreadable in the cache: from the source
                    }
                }
            });
        }
        // Build what is missing. The order follows the focused date: if the user jumps to 2010 while loading,
        // 2010 and the following dates come first.
        for _ in ov.cached_layers()..t {
            let o = ov.clone();
            pool.submit(10, move || {
                if let Some(i) = o.claim_next() {
                    o.decode(i);
                }
            });
        }
        ov
    }

    /// The series closed: the dates not begun are not read (the one being read is, unless the full-resolution cache
    /// reads it: FullResCache::abandon).
    pub fn abandon(&self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }

    /// The next date to build: the focused one, then the ones after it.
    fn claim_next(&self) -> Option<usize> {
        if self.abandoned.load(Ordering::Relaxed) {
            return None;
        }
        let f = self.focus.load(Ordering::Relaxed);
        let mut claimed = self.claimed.lock().unwrap();
        let i = (0..self.t).map(|d| (f + d) % self.t).find(|&i| !claimed[i])?;
        claimed[i] = true;
        Some(i)
    }

    fn decode(&self, i: usize) {
        let mut v = vec![0f32; self.w * self.h];
        let (w, h) = (self.info.width, self.info.height);
        // on an HDD the full-resolution cache reads the whole date once for both
        let full = self.full_res.as_ref().and_then(|c| c.build_with_overview(i, &mut v, self.w, self.h));
        let ok = full.unwrap_or_else(|| {
            with_thread_reader(&self.info, |r| r.read_window(i, (0, 0, w, h), &mut v, (self.w, self.h)))
        });
        if ok {
            self.save_layer(i, &v);
            let _ = self.layers[i].set(v.into_boxed_slice());
        } else {
            self.failed.fetch_add(1, Ordering::Relaxed); // left out of the cache: retried on the next open
        }
        self.mark_done(i);
    }

    fn load_layer(&self, i: usize) -> bool {
        let n = self.w * self.h;
        let mut cache = self.cache.lock().unwrap();
        let Some((f, off)) = cache.as_mut() else { return false };
        let mut v = vec![0f32; n];
        let ok = f.seek(SeekFrom::Start(*off + (n * 4 * i) as u64)).is_ok()
            && f.read_exact(bytemuck::cast_slice_mut(&mut v)).is_ok();
        if ok {
            let _ = self.layers[i].set(v.into_boxed_slice());
        }
        ok
    }

    /// The data first, then its index entry: a write cut short is never trusted.
    fn save_layer(&self, i: usize, v: &[f32]) {
        let n = self.w * self.h;
        let mut cache = self.cache.lock().unwrap();
        let Some((f, off)) = cache.as_mut() else { return };
        let ok = f.seek(SeekFrom::Start(*off + (n * 4 * i) as u64)).is_ok()
            && f.write_all(bytemuck::cast_slice(v)).is_ok()
            && f.flush().is_ok()
            && f.seek(SeekFrom::Start(HEADER_BYTES + i as u64)).is_ok()
            && f.write_all(&[1]).is_ok()
            && f.flush().is_ok();
        if !ok {
            *cache = None;
        }
    }

    fn mark_done(&self, i: usize) {
        self.ready.lock().unwrap().push(i);
        if self.done.fetch_add(1, Ordering::AcqRel) + 1 == self.t {
            *self.build_seconds.lock().unwrap() = Some(self.started.elapsed().as_secs_f64());
            *self.cache.lock().unwrap() = None;
        }
        (self.on_ready)();
    }

    /// Why the overview has no cache file, if so.
    pub fn cache_error(&self) -> String {
        self.cache_error.lock().unwrap().clone()
    }

    /// The dates ready since the last call (to send to the GPU).
    pub fn take_ready(&self) -> Vec<usize> {
        std::mem::take(&mut *self.ready.lock().unwrap())
    }

    /// Date `i`, once read.
    pub fn layer(&self, i: usize) -> Option<&[f32]> {
        self.layers[i].get().map(|b| &b[..])
    }

    /// The date the user is looking at: the next ones built are it and the ones after it.
    pub fn set_focus(&self, i: usize) {
        self.focus.store(i, Ordering::Relaxed);
    }

    pub fn layers_done(&self) -> usize {
        self.done.load(Ordering::Acquire)
    }

    pub fn complete(&self) -> bool {
        self.t > 0 && self.layers_done() == self.t
    }

    pub fn failed(&self) -> usize {
        self.failed.load(Ordering::Relaxed)
    }

    pub fn from_cache(&self) -> bool {
        self.from_cache.load(Ordering::Relaxed)
    }

    /// Dates read from the cache rather than the source.
    pub fn cached_layers(&self) -> usize {
        self.cached_layers.load(Ordering::Relaxed)
    }

    pub fn build_seconds(&self) -> Option<f64> {
        *self.build_seconds.lock().unwrap()
    }

    pub fn cache_path(&self) -> &Path {
        &self.cache_path
    }

    /// The approximate series of an overview pixel (NaN for dates not read yet).
    pub fn series(&self, x: usize, y: usize) -> Vec<f32> {
        let p = y * self.w + x;
        (0..self.t).map(|i| self.layer(i).map_or(f32::NAN, |l| l[p])).collect()
    }
}

impl Drop for Overview {
    fn drop(&mut self) {
        disk::release(&self.cache_path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_as_the_cpp() {
        // 215_067: 41 dates of 7478 x 7319 in 1 GB
        assert_eq!(overview_size(7478, 7319, 41, 1024 << 20, 16384), (2366, 2316));
        assert_eq!(overview_size(100, 50, 3, 1 << 30, 16384), (100, 50));
        assert_eq!(overview_size(40000, 100, 3, 1 << 30, 16384), (16384, 41));
    }
}
