//! One open series (src/session.cpp): its cube, the overview being read, what the GPU has of it, its statistics,
//! and the exact series of the cursor and the pins read from the files in the background, the newest request of
//! the cursor first.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use janus_cache::{Overview, OverviewOptions};
use janus_cube::{CubeInfo, with_thread_reader};
use janus_gpu::{Gpu, Stats, StatsJob};

/// A series being opened: the files' metadata are read off the interface's thread (the first frame never waits
/// for them).
pub struct Opening {
    pub inputs: Vec<String>,
    pub started: Instant,
    rx: Receiver<Result<CubeInfo, String>>,
}

impl Opening {
    pub fn start(inputs: Vec<String>, ctx: &egui::Context) -> Self {
        let (tx, rx) = channel();
        let (i, ctx) = (inputs.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("open".into())
            .spawn(move || {
                let _ = tx.send(janus_cube::open_cube(&i));
                ctx.request_repaint();
            })
            .expect("thread");
        Opening { inputs, started: Instant::now(), rx }
    }

    pub fn poll(&self) -> Option<Result<CubeInfo, String>> {
        self.rx.try_recv().ok()
    }
}

/// The exact series of a source pixel, read from the files.
#[derive(Clone, Debug)]
pub struct ExactSeries {
    pub x: usize,
    pub y: usize,
    pub values: Vec<f32>,
    pub ms: f64,
}

#[derive(Default)]
struct Requests {
    /// the cursor's: only the newest is read
    hover: Option<(usize, usize)>,
    /// the pins': read in order
    pins: VecDeque<(usize, usize)>,
    stop: bool,
}

struct Reader {
    requests: Mutex<Requests>,
    wake: Condvar,
    hover: Mutex<Option<ExactSeries>>,
    pins: Mutex<Vec<ExactSeries>>,
}

pub struct Session {
    pub info: Arc<CubeInfo>,
    pub overview: Arc<Overview>,
    /// the dates the GPU has
    pub uploaded: Vec<bool>,
    pub stats: Option<Stats>,
    stats_job: Option<StatsJob>,
    pub opened: Instant,
    /// from opening the files to the overview complete and its statistics computed (ms)
    pub ready_ms: Option<f64>,
    pub open_ms: f64,
    reader: Arc<Reader>,
}

/// Every logical core but 2 (session.cpp, defaultProcessingThreads).
pub fn processing_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get()).saturating_sub(2).max(1)
}

impl Session {
    pub fn new(info: CubeInfo, opening: &Opening, budget_mb: i64, max_texture: usize, ctx: &egui::Context) -> Self {
        let info = Arc::new(info);
        let opts = OverviewOptions {
            budget_bytes: budget_mb << 20,
            max_texture: max_texture.min(16384),
            cache_dir: janus_cache::overview::default_cache_dir(),
            threads: processing_threads().min(12),
        };
        let c = ctx.clone();
        let overview = Overview::start(info.clone(), &opts, move || c.request_repaint());
        let reader = Arc::new(Reader {
            requests: Mutex::new(Requests::default()),
            wake: Condvar::new(),
            hover: Mutex::new(None),
            pins: Mutex::new(Vec::new()),
        });
        let (r, i, c) = (reader.clone(), info.clone(), ctx.clone());
        std::thread::Builder::new().name("series".into()).spawn(move || series_loop(&r, &i, &c)).expect("thread");
        Session {
            uploaded: vec![false; info.t()],
            info,
            overview,
            stats: None,
            stats_job: None,
            opened: opening.started,
            ready_ms: None,
            open_ms: opening.started.elapsed().as_secs_f64() * 1000.0,
            reader,
        }
    }

    /// Sends the dates read since the last frame to the GPU, and computes the statistics once all are there.
    /// Returns whether the map changed.
    pub fn pump(&mut self, gpu: &mut Gpu, ctx: &egui::Context) -> bool {
        let ov = &self.overview;
        if !gpu.has_cube() {
            let times: Vec<f32> = (0..self.info.t()).map(|t| self.info.years_from_start(t) as f32).collect();
            gpu.create_cube(ov.w as u32, ov.h as u32, ov.t as u32, &times);
        }
        let ready = ov.take_ready();
        for &t in &ready {
            if let Some(layer) = ov.layer(t) {
                gpu.upload_layer(t as u32, layer);
                self.uploaded[t] = true;
            }
        }
        let mut changed = !ready.is_empty();
        if self.stats.is_none() && self.stats_job.is_none() && ov.complete() {
            let c = ctx.clone();
            self.stats_job = Some(gpu.compute_stats_async(move || c.request_repaint()));
        }
        if let Some(job) = &self.stats_job
            && let Some(stats) = job.poll()
        {
            self.stats = stats;
            self.stats_job = None;
            self.ready_ms = Some(self.opened.elapsed().as_secs_f64() * 1000.0);
            changed = true;
        }
        changed
    }

    /// Asks for the exact series of the cursor's source pixel (an older request not yet read is dropped).
    pub fn request_hover(&self, x: usize, y: usize) {
        let mut r = self.reader.requests.lock().unwrap();
        if r.hover != Some((x, y)) {
            r.hover = Some((x, y));
            self.reader.wake.notify_one();
        }
    }

    pub fn request_pin(&self, x: usize, y: usize) {
        self.reader.requests.lock().unwrap().pins.push_back((x, y));
        self.reader.wake.notify_one();
    }

    /// The cursor's exact series, if it is of (x, y).
    pub fn hover_exact(&self, x: usize, y: usize) -> Option<ExactSeries> {
        self.reader.hover.lock().unwrap().clone().filter(|s| s.x == x && s.y == y)
    }

    pub fn pin_exact(&self, x: usize, y: usize) -> Option<ExactSeries> {
        self.reader.pins.lock().unwrap().iter().find(|s| s.x == x && s.y == y).cloned()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.reader.requests.lock().unwrap().stop = true;
        self.reader.wake.notify_all();
    }
}

fn series_loop(r: &Reader, info: &Arc<CubeInfo>, ctx: &egui::Context) {
    loop {
        let (req, is_pin) = {
            let mut q = r.requests.lock().unwrap();
            loop {
                if q.stop {
                    return;
                }
                if let Some(h) = q.hover.take() {
                    break (h, false);
                }
                if let Some(p) = q.pins.pop_front() {
                    break (p, true);
                }
                q = r.wake.wait(q).unwrap();
            }
        };
        let start = Instant::now();
        let (x, y) = req;
        let mut values = Vec::with_capacity(info.t());
        let mut abandoned = false;
        with_thread_reader(info, |reader| {
            for t in 0..info.t() {
                // a newer cursor position: this one is not wanted any more
                if !is_pin && r.requests.lock().unwrap().hover.is_some() {
                    abandoned = true;
                    return;
                }
                values.push(reader.read_pixel(t, x, y).unwrap_or(f32::NAN));
            }
        });
        if abandoned {
            continue;
        }
        let s = ExactSeries { x, y, values, ms: start.elapsed().as_secs_f64() * 1000.0 };
        if is_pin {
            r.pins.lock().unwrap().push(s);
        } else {
            *r.hover.lock().unwrap() = Some(s);
        }
        ctx.request_repaint();
    }
}
