//! The open series in the interface: janus-session's Session, plus what the GPU has of it (the dates sent, the
//! statistics) and the exact series read for the cursor and the pins.

use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use janus_cube::CubeInfo;
use janus_gpu::{Gpu, GpuCube, GpuTexture, Stats, StatsJob};
use janus_session::TileManager;
use janus_session::{SeriesResult, SessionSettings};

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
                let _ = tx.send(janus_cube::open_cube(&i, &janus_cube::BandSelection::default()));
                ctx.request_repaint();
            })
            .expect("thread");
        Opening { inputs, started: Instant::now(), rx }
    }

    pub fn poll(&self) -> Option<Result<CubeInfo, String>> {
        self.rx.try_recv().ok()
    }
}

pub struct Session {
    pub s: janus_session::Session,
    pub info: Arc<CubeInfo>,
    /// the overview on the GPU (made at the first frame with the GPU)
    pub cube: Option<GpuCube>,
    /// detail tiles at full resolution or intermediate levels, when the zoom goes past the overview's
    pub tiles: TileManager<GpuTexture>,
    /// the dates the GPU has
    pub uploaded: Vec<bool>,
    pub stats: Option<Stats>,
    stats_job: Option<StatsJob>,
    pub opened: Instant,
    /// from opening the files to the overview complete and its statistics computed (ms)
    pub ready_ms: Option<f64>,
    pub open_ms: f64,
    /// the exact series read, newest last (the cursor's and the pins')
    series: Mutex<Vec<SeriesResult>>,
}

impl Session {
    pub fn new(info: CubeInfo, opening: &Opening, settings: &SessionSettings, ctx: &egui::Context) -> Self {
        let info = Arc::new(info);
        let c = ctx.clone();
        let s = janus_session::Session::new(info.clone(), settings, Arc::new(move || c.request_repaint()));
        let tiles =
            TileManager::new(info.clone(), s.overview.factor, settings.tile_budget_bytes, Some(s.full_res.clone()));
        Session {
            cube: None,
            tiles,
            uploaded: vec![false; info.t()],
            info,
            s,
            stats: None,
            stats_job: None,
            opened: opening.started,
            ready_ms: None,
            open_ms: opening.started.elapsed().as_secs_f64() * 1000.0,
            series: Mutex::new(Vec::new()),
        }
    }

    /// Sends the dates read since the last frame to the GPU, computes the statistics once all are there, and
    /// collects the exact series read. Returns whether the map changed.
    pub fn pump(&mut self, gpu: &mut Gpu, ctx: &egui::Context) -> bool {
        let ov = self.s.overview.clone();
        let cube = self
            .cube
            .get_or_insert_with(|| gpu.create_cube(ov.w as u32, ov.h as u32, ov.t as u32, &self.s.times_years));
        let ready = ov.take_ready();
        for &t in &ready {
            if let Some(layer) = ov.layer(t) {
                cube.upload_layer(t as u32, layer);
                self.uploaded[t] = true;
            }
        }
        let mut changed = !ready.is_empty();
        if self.stats.is_none() && self.stats_job.is_none() && ov.complete() {
            let c = ctx.clone();
            self.stats_job = Some(gpu.compute_stats_async(cube, move || c.request_repaint()));
        }
        // the tiles read since the last frame, to the GPU (16 at most per frame, as the C++)
        self.tiles.tick();
        if self.tiles.upload_ready(16, |d| gpu.create_tile(d.w as u32, d.h as u32, &d.data)) {
            changed = true;
        }
        if let Some(job) = &self.stats_job
            && let Some(stats) = job.poll()
        {
            self.stats = stats;
            self.stats_job = None;
            self.ready_ms = Some(self.opened.elapsed().as_secs_f64() * 1000.0);
            changed = true;
        }
        let new = self.s.take_series();
        if !new.is_empty() {
            let mut all = self.series.lock().unwrap();
            all.extend(new);
            // the pins' and the last few cursor positions'
            let excess = all.len().saturating_sub(256);
            all.drain(..excess);
        }
        changed
    }

    /// Asks for the exact series of the cursor's source pixel (an older request not yet read is dropped).
    pub fn request_hover(&self, x: usize, y: usize) {
        self.s.request_series(x, y, true);
    }

    pub fn request_pin(&self, x: usize, y: usize) {
        self.s.request_series(x, y, false);
    }

    /// The exact series of a source pixel, if read.
    pub fn exact(&self, x: usize, y: usize) -> Option<SeriesResult> {
        self.series.lock().unwrap().iter().rev().find(|r| r.x == x && r.y == y).cloned()
    }
}
