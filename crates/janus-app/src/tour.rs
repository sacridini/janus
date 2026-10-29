//! `janus --profile-tour IN` (src/app_profile.cpp): a fixed tour of a series in a 1600 × 950 window, timed stage by
//! stage as the C++'s, to compare the two: open, the overview and its statistics, the cursor's series over 40
//! places, every date, the display modes, a zoom to full resolution, panning and dates there (detail tiles), an
//! ROI, and back.

use std::sync::Arc;
use std::time::Instant;

use janus_gpu::Mode;
use janus_session::RoiData;

use crate::app::State;

/// Stage names, as the C++'s: index = stage.
const STAGES: [&str; 11] = [
    "",
    "open",
    "overview",
    "hover series",
    "every date",
    "display modes",
    "zoom in",
    "pan (full res)",
    "dates (full res)",
    "ROI 512x512",
    "zoom out",
];

#[derive(Default)]
pub struct Tour {
    stage: usize,
    since: Option<Instant>,
    t0: Option<Instant>,
    frames: u32,
    step: usize,
    step_frame: u32,
    roi: Option<Arc<RoiData>>,
    /// printed lines, also written to --tour-json if asked
    pub lines: Vec<(String, f64, u32)>,
}

impl Tour {
    fn next(&mut self) {
        if self.stage > 0 {
            let ms = self.since.map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0);
            let per = if self.frames > 0 { ms / f64::from(self.frames) } else { 0.0 };
            println!("  {:<18} {ms:8.0} ms  {:5} frames  {per:6.1} ms/frame", STAGES[self.stage], self.frames);
            self.lines.push((STAGES[self.stage].to_string(), ms, self.frames));
        }
        self.stage += 1;
        self.since = Some(Instant::now());
        self.frames = 0;
        self.step = 0;
        self.step_frame = 0;
    }
}

impl State {
    /// The map centred on source pixel (x, y) at `scale` screen pixels per source pixel.
    pub(crate) fn view_at(&mut self, x: f64, y: f64, scale: f64) {
        let Some(s) = &self.session else { return };
        let ov = &s.s.overview;
        let (fx, fy) = (s.info.width as f64 / ov.w as f64, s.info.height as f64 / ov.h as f64);
        self.map.set_view([x / fx, y / fy], scale * fx);
    }

    /// One frame of the tour; Some(exit code) when it is over.
    pub fn tour_step(&mut self, ctx: &egui::Context) -> Option<i32> {
        ctx.request_repaint();
        let mut p = std::mem::take(&mut self.tour);
        let r = self.tour_stage(&mut p);
        self.tour = p;
        r
    }

    fn tour_stage(&mut self, p: &mut Tour) -> Option<i32> {
        if p.stage > 0 {
            p.frames += 1;
        }
        let tiles_settled = |s: &State, p: &Tour| {
            p.frames - p.step_frame >= 3 && s.session.as_ref().is_some_and(|s| s.tiles.inflight() == 0)
        };
        match p.stage {
            0 => {
                if self.opts.inputs.is_empty() {
                    println!("usage: --profile-tour INPUT...");
                    return Some(2);
                }
                println!("profile tour: {}", self.opts.inputs[0]);
                p.t0 = Some(Instant::now());
                p.next();
            }
            1 => {
                let Some(s) = &self.session else {
                    if self.opening.is_none() {
                        println!("could not open {}: {}", self.opts.inputs[0], self.error.clone().unwrap_or_default());
                        return Some(1);
                    }
                    return None;
                };
                let ov = &s.s.overview;
                println!(
                    "  {} x {} x {} {}, overview {} x {} ({})",
                    s.info.width,
                    s.info.height,
                    s.info.t(),
                    s.info.data_type,
                    ov.w,
                    ov.h,
                    if ov.from_cache() { "from its cache" } else { "built" }
                );
                p.next();
            }
            2 => {
                let s = self.session.as_ref()?;
                if s.loaded() {
                    p.next();
                }
            }
            3 => {
                // the cursor over 40 places, each until its exact series came (or 120 frames)
                let s = self.session.as_ref()?;
                let (w, h) = (s.info.width, s.info.height);
                let exact = self.series.hover.as_ref().is_some_and(|h| h.exact);
                if p.step == 0 || exact || p.frames - p.step_frame > 120 {
                    if p.step == 40 {
                        p.next();
                        return None;
                    }
                    // a fixed scatter (golden ratio steps): the same places on every run
                    let u = (0.5 + p.step as f64 * 0.6180339887) % 1.0;
                    let v = (0.5 + p.step as f64 * 0.7548776662) % 1.0;
                    let (x, y) = ((u * (w - 1) as f64) as usize, (v * (h - 1) as f64) as usize);
                    // as the map puts it: the overview's series at once, the exact one asked for at the next
                    // frame (as if the mouse had rested)
                    self.set_hover(Some((x, y)));
                    self.rest_hover();
                    p.step_frame = p.frames;
                    p.step += 1;
                }
            }
            4 => {
                // every date, one per frame (an animation over the whole image)
                if p.step == 0 {
                    self.series.hover = None;
                    self.map.fit();
                }
                let t = self.session.as_ref()?.info.t();
                if p.step >= t {
                    self.set_date(0);
                    p.next();
                    return None;
                }
                self.set_date(p.step);
                p.step += 1;
            }
            5 => {
                // the display modes that need no options, 10 frames each
                const MODES: [Mode; 10] = [
                    Mode::Anomaly,
                    Mode::Mean,
                    Mode::Std,
                    Mode::Slope,
                    Mode::Min,
                    Mode::Max,
                    Mode::Amplitude,
                    Mode::DropDate,
                    Mode::DropMag,
                    Mode::Value,
                ];
                if p.frames % 10 != 1 {
                    return None;
                }
                if p.step == MODES.len() {
                    p.next();
                    return None;
                }
                self.display.mode = MODES[p.step];
                p.step += 1;
            }
            6 => {
                // from the whole image to 2 screen pixels per source pixel around the centre, in 60 frames
                let s = self.session.as_ref()?;
                let (w, h) = (s.info.width as f64, s.info.height as f64);
                let [cw, ch] = self.map.canvas;
                let fit = 0.98 * (f64::from(cw) / w).min(f64::from(ch) / h);
                let k = (f64::from(p.frames) / 60.0).min(1.0);
                self.view_at(w * 0.5, h * 0.5, fit * (2.0 / fit).powf(k));
                if k >= 1.0 && p.step_frame == 0 {
                    p.step_frame = p.frames;
                }
                if k >= 1.0 && tiles_settled(self, p) {
                    p.next();
                }
            }
            7 => {
                // 12 steps of 300 screen pixels to the east, each once its tiles are on the GPU
                if p.step == 0 || tiles_settled(self, p) {
                    if p.step == 12 {
                        p.next();
                        return None;
                    }
                    p.step += 1;
                    p.step_frame = p.frames;
                    let s = self.session.as_ref()?;
                    let (w, h) = (s.info.width as f64, s.info.height as f64);
                    self.view_at(w * 0.5 + p.step as f64 * 150.0, h * 0.5, 2.0);
                }
            }
            8 => {
                // with --screenshot: the map at full resolution, its detail tiles drawn (the panning's last view)
                if p.step == 0
                    && let Some(path) = self.opts.screenshot.clone()
                    && let Some((w, h, rgba)) = self.gpu.as_ref().and_then(|g| g.read_target(0))
                    && let Err(e) = image::save_buffer(&path, &rgba, w, h, image::ExtendedColorType::Rgba8)
                {
                    eprintln!("{}: {e}", path.display());
                }
                // 10 dates at full resolution, each once its tiles are on the GPU
                let t = self.session.as_ref()?.info.t();
                let n = t.min(10);
                if p.step == 0 || tiles_settled(self, p) {
                    if p.step == n {
                        self.set_date(0);
                        p.next();
                        return None;
                    }
                    self.set_date(p.step * t / n);
                    p.step += 1;
                    p.step_frame = p.frames;
                }
            }
            9 => {
                // an ROI of 512 x 512 source pixels at the centre, until its series is complete
                let s = self.session.as_ref()?;
                if p.step == 0 {
                    let (cx, cy) = (s.info.width / 2, s.info.height / 2);
                    p.roi = Some(s.s.start_roi((cx - 256, cy - 256, cx + 256, cy + 256), None));
                    p.step = 1;
                }
                if let Some(roi) = &p.roi
                    && roi.complete()
                {
                    println!(
                        "  (ROI read in {:.0} ms, {} dates from the full-resolution cache)",
                        roi.ms().unwrap_or(0.0),
                        roi.cached_dates.load(std::sync::atomic::Ordering::Relaxed)
                    );
                    p.next();
                }
            }
            10 => {
                // back to the whole image
                if p.frames == 1 {
                    self.map.fit();
                }
                if p.frames < 30 {
                    return None;
                }
                p.next();
                println!("  total {:.1} s", p.t0.map_or(0.0, |t| t.elapsed().as_secs_f64()));
                if let Some(path) = &self.opts.tour_json {
                    let stages: Vec<String> = p
                        .lines
                        .iter()
                        .map(|(n, ms, f)| format!("    {{\"stage\": \"{n}\", \"ms\": {ms:.1}, \"frames\": {f}}}"))
                        .collect();
                    let json = format!(
                        "{{\n  \"program\": \"janus (Rust) {}\",\n  \"input\": {:?},\n  \"stages\": [\n{}\n  ]\n}}\n",
                        env!("CARGO_PKG_VERSION"),
                        self.opts.inputs[0],
                        stages.join(",\n")
                    );
                    if let Err(e) = std::fs::write(path, json) {
                        eprintln!("{}: {e}", path.display());
                    }
                }
                return Some(0);
            }
            _ => {}
        }
        None
    }
}
