//! One open series and its background work (src/session.cpp): the overview, the full-resolution cache, the two
//! job pools (bg: the overview and cache builds; fg: what the user waits for, series, ROI and tiles), exact
//! series where the cursor's newest request wins, and an ROI's statistics date by date.

pub mod hdd;
pub mod tiles;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use janus_cache::{FullResCache, JobPool, Overview, OverviewOptions};
use janus_cube::stats::{SampleStats, sample_stats};
use janus_cube::{CubeInfo, with_thread_reader};

pub use tiles::{TileManager, ViewRect};

pub fn logical_cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Every logical core but 2 (at least 1).
pub fn default_processing_threads() -> usize {
    logical_cores().saturating_sub(2).max(1)
}

/// Full-resolution cache: built for every series, for series on an HDD only, or never (then on demand:
/// Session::build_full_res).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullResMode {
    Off,
    Hdd,
    All,
}

#[derive(Clone, Debug)]
pub struct SessionSettings {
    pub overview_budget_bytes: i64,
    pub tile_budget_bytes: usize,
    pub cache_dir: PathBuf,
    pub max_texture: usize,
    /// background readers (--threads); 0 = automatic (HDD: 1, SSD: the processing threads, at most 12)
    pub io_threads: usize,
    /// CPU threads Janus may use (Settings); 0 = every logical core but 2
    pub threads: usize,
    pub full_res: FullResMode,
    /// the disk cache budget (bytes, 0 = automatic)
    pub cache_budget: u64,
}

impl Default for SessionSettings {
    fn default() -> Self {
        SessionSettings {
            overview_budget_bytes: 1024 << 20,
            tile_budget_bytes: 384 << 20,
            cache_dir: janus_cache::overview::default_cache_dir(),
            max_texture: 16384,
            io_threads: 0,
            threads: 0,
            full_res: FullResMode::Hdd,
            cache_budget: 0,
        }
    }
}

impl SessionSettings {
    pub fn processing_threads(&self) -> usize {
        if self.threads > 0 { self.threads.min(logical_cores()) } else { default_processing_threads() }
    }

    /// Spinning HDD: 1 sequential reader, whatever the processing threads (measured cold, LZW 1-row strips: 1
    /// thread 1.2-1.4 s per date, close to the physical floor; 2 threads 5.0 s, the head jumping between files).
    fn bg_threads(&self, rotational: bool) -> usize {
        if self.io_threads > 0 {
            self.io_threads
        } else if rotational {
            1
        } else {
            self.processing_threads().min(12)
        }
    }

    fn fg_threads(&self, rotational: bool) -> usize {
        if rotational { 1 } else { self.processing_threads().min(4) }
    }

    /// Full-resolution cache on an HDD: one thread streams the file, these decode and compress.
    fn decode_threads(&self, rotational: bool) -> usize {
        if rotational { (self.processing_threads() / 2).clamp(1, 4) } else { 1 }
    }
}

/// A series' background workers: no more than its dates (a date is a job: a map of one date opened as a layer of
/// its own needs one, not the 12 that idled in each of 41 such layers).
fn bg_workers(s: &SessionSettings, rotational: bool, info: &CubeInfo) -> usize {
    s.bg_threads(rotational).min(info.t().max(1))
}

/// The background readers of every open series together (overviews, full-resolution caches): as many at once as
/// one series has, however many series are open; one gate for the files on HDDs (a sequential reader: two series
/// read at once made the head jump between their files), one for the rest.
fn bg_gate(rotational: bool) -> &'static Arc<janus_cache::pool::Gate> {
    static GATES: OnceLock<[Arc<janus_cache::pool::Gate>; 2]> = OnceLock::new();
    &GATES.get_or_init(|| [janus_cache::pool::Gate::new(1), janus_cache::pool::Gate::new(1)])[usize::from(rotational)]
}

/// An exact series (full resolution) of a source pixel.
#[derive(Clone, Debug)]
pub struct SeriesResult {
    pub id: u64,
    pub x: usize,
    pub y: usize,
    pub values: Vec<f32>,
    /// dates read from the full-resolution cache
    pub cached: usize,
    pub ms: f64,
}

/// The pixels an ROI keeps: (x0, y0, w, h) of the rectangle in source pixels and (bw, bh) read give a bw × bh
/// mask (a polygon's pixel centres inside).
pub type MaskOf<'a> = &'a dyn Fn(usize, usize, usize, usize, usize, usize) -> Vec<bool>;

/// Statistics of an ROI, date by date (computed in parallel): a rectangle, or the pixels of its bounding box a
/// mask keeps (a polygon's).
pub struct RoiData {
    /// source pixels, [x0, x1)
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
    /// the size read (subsampled above 4 Mpx per date)
    pub bw: usize,
    pub bh: usize,
    pub sampled: bool,
    /// bw × bh, false = left out; empty: the whole rectangle
    pub mask: Vec<bool>,
    pub per_t: Vec<OnceLock<SampleStats>>,
    pub done: AtomicUsize,
    pub cancel: AtomicBool,
    started: Instant,
    ms: Mutex<Option<f64>>,
    pub cached_dates: AtomicUsize,
}

impl RoiData {
    pub fn complete(&self) -> bool {
        self.done.load(Ordering::Acquire) == self.per_t.len()
    }

    /// The time from the request to the last date (ms), once complete.
    pub fn ms(&self) -> Option<f64> {
        *self.ms.lock().unwrap()
    }
}

#[derive(Default)]
struct SeriesState {
    results: Mutex<Vec<SeriesResult>>,
    next_id: AtomicU64,
    latest_hover: AtomicU64,
}

pub struct Session {
    // the pools first: dropped first, their workers joined before the rest goes (fg, then bg, as the C++)
    fg: JobPool,
    bg: JobPool,
    pub info: Arc<CubeInfo>,
    pub overview: Arc<Overview>,
    pub full_res: Arc<FullResCache>,
    /// the dates in decimal years from the first (the statistics' x)
    pub times_years: Vec<f32>,
    pub rotational: bool,
    pub opened: Instant,
    settings: Mutex<SessionSettings>,
    series: Arc<SeriesState>,
}

impl Session {
    /// Opens a series: the full-resolution cache first (on an HDD the overview pass builds both in the same read
    /// of each date), then the overview, then the dates the overview pass does not read. `wake` is called when
    /// background work produced something.
    pub fn new(info: Arc<CubeInfo>, s: &SessionSettings, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        // network reads gain from parallel requests: never an HDD
        let rotational = hdd::is_on_rotational_disk(std::path::Path::new(&info.layers[0].path));
        let w = wake.clone();
        let gate = bg_gate(rotational);
        gate.set_limit(s.bg_threads(rotational));
        let bg =
            JobPool::gated(bg_workers(s, rotational, &info), "bg", Some(Box::new(move || w())), Some(gate.clone()));
        let w = wake.clone();
        let fg = JobPool::new(s.fg_threads(rotational), "fg", Some(Box::new(move || w())));
        let full_res = FullResCache::new(info.clone(), s.cache_dir.clone());
        let want_full = s.full_res == FullResMode::All || (s.full_res == FullResMode::Hdd && rotational);
        if want_full {
            full_res.start(s.decode_threads(rotational), s.cache_budget);
        }
        let opts = OverviewOptions {
            budget_bytes: s.overview_budget_bytes,
            max_texture: s.max_texture,
            cache_dir: s.cache_dir.clone(),
            cache_budget: s.cache_budget,
            full_res: Some(full_res.clone()),
        };
        let w = wake.clone();
        let overview = Overview::start(info.clone(), &opts, &bg, move || w());
        if want_full {
            submit_full_res_build(&full_res, &bg);
        }
        Session {
            fg,
            bg,
            times_years: (0..info.t()).map(|t| info.years_from_start(t) as f32).collect(),
            info,
            overview,
            full_res,
            rotational,
            opened: Instant::now(),
            settings: Mutex::new(s.clone()),
            series: Arc::new(SeriesState::default()),
        }
    }

    pub fn bg(&self) -> &JobPool {
        &self.bg
    }

    pub fn fg(&self) -> &JobPool {
        &self.fg
    }

    pub fn busy(&self) -> bool {
        !self.overview.complete() || self.bg.pending() > 0 || self.fg.pending() > 0
    }

    /// HDD with the overview still building: random reads (pins, tiles, ROI) wait, so the read head is not
    /// pulled away from the sequential pass.
    pub fn defer_random_reads(&self) -> bool {
        self.rotational && !self.overview.complete()
    }

    /// The exact series of a source pixel: the dates in the full-resolution cache first, the rest from the
    /// files. `cancellable` (the cursor): dropped if a newer cancellable request arrives before it is read.
    pub fn request_series(&self, x: usize, y: usize, cancellable: bool) -> u64 {
        let st = &self.series;
        let id = st.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        if cancellable {
            st.latest_hover.store(id, Ordering::Release);
        }
        let (st, info, full) = (st.clone(), self.info.clone(), self.full_res.clone());
        self.fg.submit(0, move || {
            let stale = || cancellable && st.latest_hover.load(Ordering::Acquire) != id;
            if stale() {
                return; // the mouse has already moved on
            }
            let start = Instant::now();
            let t = info.t();
            let mut values = vec![f32::NAN; t];
            let mut got = vec![false; t];
            let cached = full.read_series(x, y, &mut values, &mut got);
            if cached < t {
                let todo: Vec<usize> = (0..t).filter(|&i| !got[i]).collect();
                let gone = if info.network_sources && todo.len() > 1 {
                    // over the network a date is a few round trips: 12 dates at once (a STAC series' pixel of 107
                    // dates, 4.6 s a date one at a time)
                    let (next, gone) = (AtomicUsize::new(0), AtomicBool::new(false));
                    let read: Vec<AtomicU32> = todo.iter().map(|_| AtomicU32::new(f32::NAN.to_bits())).collect();
                    std::thread::scope(|sc| {
                        for _ in 0..todo.len().min(12) {
                            sc.spawn(|| {
                                loop {
                                    let k = next.fetch_add(1, Ordering::Relaxed);
                                    if k >= todo.len() || gone.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    if stale() {
                                        gone.store(true, Ordering::Relaxed);
                                        break;
                                    }
                                    let v = with_thread_reader(&info, |r| r.read_pixel(todo[k], x, y));
                                    read[k].store(v.unwrap_or(f32::NAN).to_bits(), Ordering::Relaxed);
                                }
                            });
                        }
                    });
                    for (k, &i) in todo.iter().enumerate() {
                        values[i] = f32::from_bits(read[k].load(Ordering::Relaxed));
                    }
                    gone.into_inner()
                } else {
                    with_thread_reader(&info, |r| {
                        for &i in &todo {
                            if stale() {
                                return true;
                            }
                            values[i] = r.read_pixel(i, x, y).unwrap_or(f32::NAN);
                        }
                        false
                    })
                };
                if gone {
                    return;
                }
            }
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            st.results.lock().unwrap().push(SeriesResult { id, x, y, values, cached, ms });
        });
        id
    }

    /// The series read since the last call.
    pub fn take_series(&self) -> Vec<SeriesResult> {
        std::mem::take(&mut *self.series.results.lock().unwrap())
    }

    /// The statistics of a rectangle of source pixels date by date, oldest first; `mask_of(bw, bh)` gives the
    /// pixels kept (a polygon's; None: the whole rectangle). Above 4 Mpx per date it is read subsampled.
    pub fn start_roi(
        &self,
        (x0, y0, x1, y1): (usize, usize, usize, usize),
        mask_of: Option<MaskOf<'_>>,
    ) -> Arc<RoiData> {
        let (w_all, h_all) = (self.info.width, self.info.height);
        let rx0 = x0.min(x1).min(w_all - 1);
        let ry0 = y0.min(y1).min(h_all - 1);
        let rx1 = x0.max(x1).clamp(rx0 + 1, w_all);
        let ry1 = y0.max(y1).clamp(ry0 + 1, h_all);
        let (w, h) = (rx1 - rx0, ry1 - ry0);
        let s = (4.0e6 / (w as f64 * h as f64)).sqrt().min(1.0);
        let (bw, bh) = (((w as f64 * s) as usize).max(1), ((h as f64 * s) as usize).max(1));
        let mask = mask_of.map(|f| f(rx0, ry0, w, h, bw, bh)).unwrap_or_default();
        let t = self.info.t();
        let roi = Arc::new(RoiData {
            x0: rx0,
            y0: ry0,
            x1: rx1,
            y1: ry1,
            bw,
            bh,
            sampled: s < 1.0,
            mask,
            per_t: (0..t).map(|_| OnceLock::new()).collect(),
            done: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
            started: Instant::now(),
            ms: Mutex::new(None),
            cached_dates: AtomicUsize::new(0),
        });
        for i in 0..t {
            let (roi, info, full) = (roi.clone(), self.info.clone(), self.full_res.clone());
            // chronological: the oldest date first
            self.fg.submit(5 + i as i32, move || {
                if !roi.cancel.load(Ordering::Relaxed) {
                    let mut buf = vec![0f32; roi.bw * roi.bh];
                    let win = (roi.x0, roi.y0, roi.x1 - roi.x0, roi.y1 - roi.y0);
                    let cached = full.read_window(i, win, &mut buf, (roi.bw, roi.bh));
                    if cached {
                        roi.cached_dates.fetch_add(1, Ordering::Relaxed);
                    }
                    if cached || with_thread_reader(&info, |r| r.read_window(i, win, &mut buf, (roi.bw, roi.bh))) {
                        for (v, &keep) in buf.iter_mut().zip(&roi.mask) {
                            if !keep {
                                *v = f32::NAN;
                            }
                        }
                        let _ = roi.per_t[i].set(sample_stats(&mut buf));
                    }
                }
                if roi.done.fetch_add(1, Ordering::AcqRel) + 1 == roi.per_t.len() {
                    *roi.ms.lock().unwrap() = Some(roi.started.elapsed().as_secs_f64() * 1000.0);
                }
            });
        }
        roi
    }

    /// Starts (or resumes) building the full-resolution cache now, whatever the setting. False if it cannot be
    /// built: see full_res.error().
    pub fn build_full_res(&self) -> bool {
        let s = self.settings.lock().unwrap().clone();
        if !self.full_res.start(s.decode_threads(self.rotational), s.cache_budget) {
            return false;
        }
        submit_full_res_build(&self.full_res, &self.bg);
        true
    }

    /// New processing threads or disk cache budget settings: the pools grow or shrink now (jobs running keep
    /// going); a full-resolution build started later uses both.
    pub fn apply_settings(&self, s: &SessionSettings) {
        bg_gate(self.rotational).set_limit(s.bg_threads(self.rotational));
        self.bg.set_threads(bg_workers(s, self.rotational, &self.info));
        self.fg.set_threads(s.fg_threads(self.rotational));
        *self.settings.lock().unwrap() = s.clone();
    }
}

/// The missing dates of the full-resolution cache, after the overview's jobs (priorities 0 and 10): on an HDD the
/// single reader finishes the overview first, then sweeps the files again for the dates whose overview came from
/// its cache.
fn submit_full_res_build(full: &Arc<FullResCache>, pool: &JobPool) {
    for _ in 0..full.missing_dates() {
        let f = full.clone();
        pool.submit(20, move || f.build_next());
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // the date being built is dropped instead of waited for, even when the overview reads it
        self.full_res.abandon();
        self.overview.abandon();
        self.series.latest_hover.store(u64::MAX, Ordering::Release);
    }
}
