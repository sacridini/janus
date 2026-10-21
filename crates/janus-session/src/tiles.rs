//! Detail tiles (src/tiles.cpp): full resolution or intermediate levels, for when the zoom goes past the
//! overview's resolution. Only the current date (and the next one while playing) is asked for; requests that left
//! the screen are dropped before being read. The textures are the caller's (`T`, made by `upload_ready`), kept in
//! an LRU within a budget of bytes.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use janus_cache::{FullResCache, JobPool};
use janus_cube::{CubeInfo, with_thread_reader};

pub const TILE_SIZE: usize = 256;

/// The visible region in source pixels and its scale (screen pixels per source pixel).
#[derive(Clone, Copy, Debug)]
pub struct ViewRect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
    pub scale: f64,
}

/// A tile read: its texels and where it goes in the source.
pub struct TileData {
    pub key: u64,
    pub w: usize,
    pub h: usize,
    /// its rectangle in source pixels
    pub x: f64,
    pub y: f64,
    pub sw: f64,
    pub sh: f64,
    pub data: Vec<f32>,
    pub ms: f64,
}

/// A tile on the GPU.
pub struct Tile<T> {
    pub tex: T,
    pub w: usize,
    pub h: usize,
    pub x: f64,
    pub y: f64,
    pub sw: f64,
    pub sh: f64,
    last_used: u64,
}

#[derive(Default)]
struct Shared {
    /// key -> the last frame it was visible
    wanted: HashMap<u64, u64>,
    inflight: HashSet<u64>,
    results: Vec<TileData>,
}

pub struct TileManager<T> {
    info: Arc<CubeInfo>,
    full_res: Option<Arc<FullResCache>>,
    overview_factor: f64,
    /// the coarsest level still finer than the overview (None: the overview is enough at every zoom)
    max_level: Option<u32>,
    max_bytes: usize,
    bytes: usize,
    frame: u64,
    resident: HashMap<u64, Tile<T>>,
    shared: Arc<Mutex<Shared>>,
    shared_frame: Arc<AtomicU64>,
    load_ms_sum: f64,
    loads: usize,
}

pub fn make_key(t: usize, level: u32, tx: usize, ty: usize) -> u64 {
    ((t as u64) << 43) | (u64::from(level) << 38) | ((tx as u64) << 19) | ty as u64
}

/// The tiles of a level covering the view.
fn tiles_in_view(v: &ViewRect, level: u32, w: usize, h: usize) -> impl Iterator<Item = (usize, usize)> {
    let span = (TILE_SIZE << level) as f64;
    let (ntx, nty) = ((w as f64 / span).ceil() as i64, (h as f64 / span).ceil() as i64);
    let tx0 = (v.x0 / span).floor().max(0.0) as i64;
    let ty0 = (v.y0 / span).floor().max(0.0) as i64;
    let tx1 = (ntx - 1).min(((v.x1 - 1e-9) / span).floor() as i64);
    let ty1 = (nty - 1).min(((v.y1 - 1e-9) / span).floor() as i64);
    (ty0..=ty1).flat_map(move |ty| (tx0..=tx1).map(move |tx| (tx as usize, ty as usize)))
}

impl<T> TileManager<T> {
    /// Tiles are read from `full_res` when it has the date, else from the source.
    pub fn new(
        info: Arc<CubeInfo>,
        overview_factor: f64,
        max_bytes: usize,
        full_res: Option<Arc<FullResCache>>,
    ) -> Self {
        // the largest level L (2^L source pixels per texel) still finer than the overview
        let max_level = (overview_factor > 1.0001).then(|| (overview_factor.log2().ceil() as i64 - 1).max(0) as u32);
        TileManager {
            info,
            full_res,
            overview_factor,
            max_level,
            max_bytes,
            bytes: 0,
            frame: 0,
            resident: HashMap::new(),
            shared: Arc::default(),
            shared_frame: Arc::default(),
            load_ms_sum: 0.0,
            loads: 0,
        }
    }

    /// Once per frame, whatever the number of map panels calling update(): requests not renewed for 2 frames are
    /// dropped before being read.
    pub fn tick(&mut self) {
        self.frame += 1;
        self.shared_frame.store(self.frame, Ordering::Release);
        if self.frame.is_multiple_of(120) {
            let f = self.frame;
            self.shared.lock().unwrap().wanted.retain(|_, &mut seen| seen + 120 >= f);
        }
    }

    fn level_for(&self, v: &ViewRect) -> Option<u32> {
        let max = self.max_level?;
        if v.scale * self.overview_factor <= 1.0 {
            return None;
        }
        Some(((1.0 / v.scale).log2().floor().max(0.0) as u32).min(max))
    }

    /// For each map panel showing this layer: asks for the tiles of date t the view needs (and of `prefetch_t`,
    /// at a lower priority, while playing). Returns the level wanted (None: the overview is enough).
    pub fn update(&mut self, pool: &JobPool, t: usize, v: &ViewRect, prefetch_t: Option<usize>) -> Option<u32> {
        let level = self.level_for(v)?;
        self.request(pool, t, level, v, 1);
        if let Some(p) = prefetch_t {
            self.request(pool, p, level, v, 3);
        }
        Some(level)
    }

    /// Tiles of date t the view wants and the GPU does not have yet (an exported frame waits for them).
    pub fn missing(&self, t: usize, v: &ViewRect) -> usize {
        let Some(level) = self.level_for(v) else { return 0 };
        tiles_in_view(v, level, self.info.width, self.info.height)
            .filter(|&(tx, ty)| !self.resident.contains_key(&make_key(t, level, tx, ty)))
            .count()
    }

    fn request(&mut self, pool: &JobPool, t: usize, level: u32, v: &ViewRect, priority: i32) {
        let (w, h) = (self.info.width, self.info.height);
        for (tx, ty) in tiles_in_view(v, level, w, h) {
            let key = make_key(t, level, tx, ty);
            if let Some(tile) = self.resident.get_mut(&key) {
                tile.last_used = self.frame;
                continue;
            }
            {
                let mut sh = self.shared.lock().unwrap();
                sh.wanted.insert(key, self.frame);
                if !sh.inflight.insert(key) {
                    continue;
                }
            }
            let (shared, frame, info, full) =
                (self.shared.clone(), self.shared_frame.clone(), self.info.clone(), self.full_res.clone());
            pool.submit(priority, move || {
                {
                    let mut sh = shared.lock().unwrap();
                    let now = frame.load(Ordering::Acquire);
                    if sh.wanted.get(&key).is_none_or(|&seen| seen + 2 < now) {
                        sh.inflight.remove(&key); // left the screen before being read
                        return;
                    }
                }
                let start = Instant::now();
                let span = TILE_SIZE << level;
                let (x, y) = (tx * span, ty * span);
                let (sw, sh) = (span.min(info.width - x), span.min(info.height - y));
                let tw = sw.div_ceil(1 << level).max(1);
                let th = sh.div_ceil(1 << level).max(1);
                let mut data = vec![0f32; tw * th];
                let win = (x, y, sw, sh);
                let ok = full.as_ref().is_some_and(|c| c.read_window(t, win, &mut data, (tw, th)))
                    || with_thread_reader(&info, |r| r.read_window(t, win, &mut data, (tw, th)));
                if !ok {
                    data.fill(f32::NAN);
                }
                let ms = start.elapsed().as_secs_f64() * 1000.0;
                shared.lock().unwrap().results.push(TileData {
                    key,
                    w: tw,
                    h: th,
                    x: x as f64,
                    y: y as f64,
                    sw: sw as f64,
                    sh: sh as f64,
                    data,
                    ms,
                });
            });
        }
    }

    /// Turns up to `max` tiles read into textures (`make`). True if anything changed.
    pub fn upload_ready(&mut self, max: usize, mut make: impl FnMut(&TileData) -> T) -> bool {
        let batch: Vec<TileData> = {
            let mut sh = self.shared.lock().unwrap();
            let n = max.min(sh.results.len());
            let at = sh.results.len() - n;
            let batch: Vec<TileData> = sh.results.drain(at..).collect();
            for r in &batch {
                sh.inflight.remove(&r.key);
            }
            batch
        };
        for r in &batch {
            if self.resident.contains_key(&r.key) {
                continue;
            }
            let tile = Tile { tex: make(r), w: r.w, h: r.h, x: r.x, y: r.y, sw: r.sw, sh: r.sh, last_used: self.frame };
            self.resident.insert(r.key, tile);
            self.bytes += r.w * r.h * 4;
            self.load_ms_sum += r.ms;
            self.loads += 1;
        }
        if self.bytes > self.max_bytes {
            self.evict();
        }
        !batch.is_empty()
    }

    /// The least recently used tiles go, down to 90 % of the budget; never one used this frame.
    fn evict(&mut self) {
        let mut order: Vec<(u64, u64)> = self.resident.iter().map(|(&k, t)| (t.last_used, k)).collect();
        order.sort_unstable();
        for (used, key) in order {
            if self.bytes <= self.max_bytes * 9 / 10 || used >= self.frame {
                break;
            }
            if let Some(t) = self.resident.remove(&key) {
                self.bytes -= t.w * t.h * 4;
            }
        }
    }

    /// The cached tiles of date t covering the view, from the coarsest to the finest (drawn in that order).
    pub fn visible(&mut self, t: usize, v: &ViewRect) -> Vec<&Tile<T>> {
        let Some(max) = self.max_level else { return Vec::new() };
        let frame = self.frame;
        let mut keys = Vec::new();
        for level in (0..=max).rev() {
            for (tx, ty) in tiles_in_view(v, level, self.info.width, self.info.height) {
                let key = make_key(t, level, tx, ty);
                if let Some(tile) = self.resident.get_mut(&key) {
                    tile.last_used = frame;
                    keys.push(key);
                }
            }
        }
        keys.iter().map(|k| &self.resident[k]).collect()
    }

    pub fn gpu_bytes(&self) -> usize {
        self.bytes
    }

    pub fn gpu_tiles(&self) -> usize {
        self.resident.len()
    }

    pub fn inflight(&self) -> usize {
        self.shared.lock().unwrap().inflight.len()
    }

    pub fn avg_load_ms(&self) -> f64 {
        if self.loads == 0 { 0.0 } else { self.load_ms_sum / self.loads as f64 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_of_a_view() {
        let v = ViewRect { x0: 100.0, y0: 0.0, x1: 600.0, y1: 256.0, scale: 1.0 };
        let t: Vec<_> = tiles_in_view(&v, 0, 1000, 1000).collect();
        assert_eq!(t, [(0, 0), (1, 0), (2, 0)]);
        // level 1: 512 source pixels per tile
        assert_eq!(tiles_in_view(&v, 1, 1000, 1000).count(), 2);
        assert_ne!(make_key(1, 0, 0, 0), make_key(0, 1, 0, 0));
    }
}
