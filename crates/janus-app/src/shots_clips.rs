//! The short looping videos of the website's home page (`--shots clip-explore|clip-detect|clip-prove IN OUT`,
//! tools/make_clips.py): the program is driven as the guides' screenshots are, but every frame of a stretch of the
//! tour is saved (OUT/<clip>/frame_00001.png...), one every 1/30 s of the clip's own time: the pointer glides, clicks,
//! the date advances and a parameter changes at that time, whatever the time each frame takes to draw. The pointer
//! is the tour's own (egui events, never the user's mouse) and the app draws an arrow at it, over everything, in the
//! frames only.

use std::path::PathBuf;
use std::time::Instant;

use egui::{Color32, Context, Order, Pos2, Rect, Stroke, pos2, vec2};
use janus_cube::BandSelection;
use serde_json::json;

use crate::app::{State, Tab};
use crate::shots::{Shots, tool_tab};

/// The clips' frame rate: the time of a clip frame is 1/FPS s.
pub const FPS: f64 = 30.0;

/// Frames to wait after a frame's state is applied, for the map, the chart and the panels to catch up.
const SETTLE: u8 = 4;

/// The frames of the clip being recorded.
#[derive(Default)]
pub struct Clip {
    dir: PathBuf,
    /// the frames saved in all, and in the stretch under way
    pub k: usize,
    seg: usize,
    /// the settle frames left after a state was applied (None: nothing applied, waiting for the next)
    settle: Option<u8>,
    /// the stretch's extra wait, in seconds, after a change that takes long (a fit), from its application
    slow: f64,
    applied: Option<Instant>,
    /// what the stretch has applied so far (its own bookkeeping), and the pointer's click times done
    pub state: u32,
    clicks: usize,
}

/// What a frame of the tour does about the clip.
pub enum Tick {
    Wait,
    /// the state of the stretch's frame j to be applied
    Apply(usize),
    /// the stretch is over
    Done,
}

/// A move of the pointer, in screen points, over [t0, t1] seconds of the stretch.
#[derive(Clone, Copy)]
pub struct Move {
    pub t0: f64,
    pub t1: f64,
    pub to: Pos2,
}

fn smoother(u: f64) -> f64 {
    let u = u.clamp(0.0, 1.0);
    u * u * u * (u * (u * 6.0 - 15.0) + 10.0)
}

/// The pointer at time `t`, gliding (eased) from one move's end to the next's.
pub fn pointer_at(start: Pos2, moves: &[Move], t: f64) -> Pos2 {
    let mut at = start;
    for m in moves {
        if t >= m.t1 {
            at = m.to;
        } else {
            if t > m.t0 {
                let e = smoother((t - m.t0) / (m.t1 - m.t0)) as f32;
                at += (m.to - at) * e;
            }
            break;
        }
    }
    at
}

/// The pointer's arrow, white with a dark outline, and a ring spreading from a click, over every panel.
pub fn draw_pointer(ctx: &Context, sh: &Shots) {
    let Some(p) = sh.pointer else { return };
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Debug, egui::Id::new("clip pointer")));
    if let (Some(c), Some(clip)) = (sh.click_at, &sh.clip) {
        let age = clip.k.saturating_sub(c) as f32 / FPS as f32;
        if age < 0.45 {
            let a = (1.0 - age / 0.45).clamp(0.0, 1.0);
            painter.circle_stroke(
                p,
                6.0 + age * 40.0,
                Stroke::new(2.5, Color32::from_rgb(255, 214, 0).gamma_multiply(a)),
            );
        }
    }
    // the arrow's tip at the pointer; a fan of convex pieces, as the shape is not convex
    let s = 1.25;
    let pt = |x: f32, y: f32| p + vec2(x, y) * s;
    let outline =
        [pt(0.0, 0.0), pt(0.0, 16.5), pt(3.9, 12.9), pt(6.6, 19.2), pt(9.6, 17.9), pt(6.9, 11.6), pt(12.0, 11.6)];
    painter.add(egui::Shape::closed_line(outline.to_vec(), Stroke::new(3.2, Color32::from_black_alpha(235))));
    let white = Color32::WHITE;
    for tri in [[0, 1, 2], [0, 2, 6], [2, 3, 4], [2, 4, 5], [2, 5, 6]] {
        painter.add(egui::Shape::convex_polygon(
            tri.iter().map(|&i| outline[i]).collect(),
            white,
            Stroke::new(0.6, white),
        ));
    }
}

impl State {
    /// A point (x, y in the active layer's CRS) on the screen, on the main map as drawn now.
    fn clip_screen(&self, geo: [f64; 2]) -> Option<Pos2> {
        let (px, py) = self.session.as_ref()?.info.geo_to_pixel(geo[0], geo[1])?;
        let g = self.grid()?;
        let m = &self.map;
        let rect = Rect::from_min_size(pos2(m.canvas_min[0], m.canvas_min[1]), vec2(m.canvas[0], m.canvas[1]));
        Some(m.screen_of(rect, (px + 0.5) / g.fx, (py + 0.5) / g.fy))
    }

    /// The main map's rectangle on the screen.
    fn clip_map_rect(&self) -> Rect {
        let m = &self.map;
        Rect::from_min_size(pos2(m.canvas_min[0], m.canvas_min[1]), vec2(m.canvas[0], m.canvas[1]))
    }

    /// One frame of a stretch of `frames` frames: Apply(j) when the state of its frame j is to be applied, Done
    /// after the last one is saved. `quiet` says nothing is loading. A frame is saved once the state applied has
    /// settled (a few frames of the app, the wait of `Clip::slow`, nothing loading).
    fn clip_tick(&mut self, sh: &mut Shots, name: &str, frames: usize, quiet: bool) -> Tick {
        let c = sh.clip.get_or_insert_with(Clip::default);
        if c.dir.as_os_str().is_empty() {
            c.dir = sh.out.join(name);
            let _ = std::fs::remove_dir_all(&c.dir);
            let _ = std::fs::create_dir_all(&c.dir);
        }
        if let Some(n) = c.settle {
            if n > 0 {
                c.settle = Some(n - 1);
                return Tick::Wait;
            }
            if !quiet || c.applied.is_some_and(|t| t.elapsed().as_secs_f64() < c.slow) {
                return Tick::Wait;
            }
            c.k += 1;
            c.seg += 1;
            c.settle = None;
            let file = c.dir.join(format!("frame_{:05}.png", c.k));
            sh.save_frame(file);
            return Tick::Wait;
        }
        if c.seg >= frames {
            c.seg = 0;
            c.state = 0;
            c.clicks = 0;
            return Tick::Done;
        }
        c.settle = Some(SETTLE);
        c.applied = Some(Instant::now());
        c.slow = 0.0;
        Tick::Apply(c.seg)
    }

    /// The pointer at time `t` of a stretch: its position, and a click (pressed now, released the next frame) at
    /// each of `clicks` seconds as it is reached.
    fn clip_point(&mut self, sh: &mut Shots, start: Pos2, moves: &[Move], clicks: &[f64], t: f64) {
        let p = pointer_at(start, moves, t);
        sh.pointer = Some(p);
        let done = sh.clip.as_ref().map_or(0, |c| c.clicks);
        if clicks.get(done).is_some_and(|&ct| t >= ct) {
            sh.press = 2;
            if let Some(c) = &mut sh.clip {
                c.clicks += 1;
                sh.click_at = Some(c.k + 1);
            }
        }
    }

    /// The tours of the home page's clips.
    pub(crate) fn shots_clips(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let tour = sh.tour.clone();
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL ({tour}, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        let waiting = sh.job.is_some() && sh.job_stage.is_some_and(|s| sh.stage <= s + 1);
        if t > if waiting { 7200.0 } else { 240.0 } {
            return fail(sh, "timeout");
        }
        let tool_win = tool_tab("landtrendr");
        let ready = |s: &State| s.opening.is_none() && s.session.is_some();
        // the annual NDVI stack of Rondonia, 1985-2024 (the landtrendr tour's)
        let cleared = [-63.083966, -10.356003]; // forest until 2003, pasture from 2004
        let forest = [-63.202544, -10.326628]; // forest throughout
        let n_dates = |s: &State| s.session.as_ref().map_or(0, |s| s.info.t());
        let quiet = self.shots_quiet();
        match (sh.tour.as_str(), sh.stage) {
            (_, 0) => {
                let input = sh.input.clone();
                self.shots_open(&input, BandSelection::default(), ctx);
                sh.next("the annual series opening");
            }
            (tour, 1) => {
                if !ready(self) {
                    return None;
                }
                let detect = tour == "clip-detect";
                sh.layout = Some((if detect { vec![tool_win] } else { vec![] }, vec![Tab::Tasks]));
                self.shots_colormap("NDVI");
                self.set_date(if detect { n_dates(self) - 1 } else { 0 });
                if detect {
                    self.shots_pin_at(cleared[0], cleared[1]);
                }
                sh.next(&format!("{} dates", n_dates(self)));
            }
            (_, 2) => {
                if sh.frames == 30 {
                    self.map.fit(); // once the panels have their size
                }
                if !quiet || t < 4.0 {
                    return None;
                }
                if sh.tour == "clip-detect" {
                    // LandTrendr fitted on the clearing
                    if !self.shots_open_tool("landtrendr", json!({})) {
                        return fail(sh, "no LandTrendr tool");
                    }
                }
                // closer, on the two places the clips are about
                self.shots_zoom_to_geo((forest[0] + cleared[0]) / 2.0, (forest[1] + cleared[1]) / 2.0, 0.8);
                sh.next("the view settled");
            }
            ("clip-explore", 3) => {
                // 9.6 s: the pointer over forest and a clearing, two pins, then the years going by
                if t < 2.0 {
                    return None; // the zoom settled
                }
                let total = 288;
                let n = n_dates(self);
                let (Some(f), Some(c)) = (self.clip_screen(forest), self.clip_screen(cleared)) else {
                    return fail(sh, "the places are off the map");
                };
                let start = f + vec2(-150.0, 80.0);
                let moves = [
                    Move { t0: 0.4, t1: 1.7, to: f },
                    Move { t0: 2.5, t1: 3.8, to: c },
                    Move { t0: 4.6, t1: 5.7, to: f },
                    Move { t0: 6.6, t1: 7.6, to: f + vec2(120.0, 90.0) },
                ];
                let clicks = [4.1, 5.9];
                match self.clip_tick(sh, "explore", total, quiet) {
                    Tick::Apply(j) => {
                        let tj = j as f64 / FPS;
                        if j == 0
                            && let Some(c) = &mut sh.clip
                        {
                            c.slow = 1.5; // the cursor's series read in full
                        }
                        self.clip_point(sh, start, &moves, &clicks, tj);
                        // the years going by, 1985 to 2024, from 6.2 s on
                        let u = ((tj - 6.2) / 3.2).clamp(0.0, 1.0);
                        self.set_date((u * (n - 1) as f64).round() as usize);
                    }
                    Tick::Done => {
                        sh.pointer = None;
                        sh.next("the explore clip");
                    }
                    Tick::Wait => {}
                }
            }
            ("clip-detect", 3) => {
                // 6.4 s: the fit with the default parameters, at most 2 segments, at most 6 again
                if sh.frames == 3 {
                    sh.focus.push(tool_win);
                }
                if sh.frames < 5 || t < 3.0 || !quiet {
                    return None;
                }
                match self.clip_tick(sh, "detect", 192, quiet) {
                    Tick::Apply(j) => {
                        let tj = j as f64 / FPS;
                        let want = if tj < 1.8 {
                            1
                        } else if tj < 4.0 {
                            2
                        } else {
                            3
                        };
                        let c = sh.clip.as_mut()?;
                        if c.state != want {
                            c.state = want;
                            c.slow = 0.8;
                            let segments = [0, 6, 2, 6][want as usize];
                            if want > 1 {
                                self.shots_set_params("landtrendr", json!({"max_segments": segments}));
                            }
                        }
                    }
                    Tick::Done => sh.next("the fits"),
                    Tick::Wait => {}
                }
            }
            ("clip-detect", 4) => {
                // the run on the whole image: losses of 0.25 NDVI or more that start in forest; one second of it
                if sh.frames == 1 {
                    self.shots_set_params(
                        "landtrendr",
                        json!({"max_segments": 6, "min_magnitude": 2500, "pre_val_threshold": 7500}),
                    );
                    if !self.shots_run(sh, "landtrendr", 0, ctx) {
                        return fail(sh, "the run did not start");
                    }
                    self.zeit.show_tasks = true;
                    return None;
                }
                if sh.frames == 3 {
                    sh.focus.push(Tab::Tasks);
                }
                let progress =
                    sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(0.0, |j| j.status.lock().unwrap().progress);
                if sh.frames < 5 || (progress < 0.25 && self.shots_ran(sh) < 0) {
                    return None;
                }
                if let Tick::Done = self.clip_tick(sh, "detect", 30, true) {
                    sh.next("the run, under way");
                }
            }
            ("clip-detect", 5) => match self.shots_ran(sh) {
                r if r < 0 => {}
                r if r > 0 => return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh))),
                _ => {
                    self.shots_done_stage(sh);
                }
            },
            ("clip-detect", 6) => {
                // the year of detection on the map, held (the run's notice dismissed)
                self.clear_notices();
                if sh.frames == 1 {
                    if !self.shots_show_result(sh, "Year of detection") {
                        return fail(sh, "no year of detection");
                    }
                    self.zeit.show_tasks = false;
                    sh.focus.push(tool_win);
                    return None;
                }
                if sh.frames < 5 || !quiet {
                    return None;
                }
                if let Tick::Done = self.clip_tick(sh, "detect", 78, quiet) {
                    sh.next("the year of detection");
                }
            }
            _ => {
                sh.pointer = None;
                println!("OK");
                return Some(0);
            }
        }
        None
    }

    /// The validation tour's stretch kept as a clip (`clip-prove`): the tour's own stage has made the state and
    /// asked for its screenshot; here the state is held for `hold.frames` frames while the pointer glides over the
    /// map or the panel scrolls.
    pub(crate) fn shots_hold(&mut self, sh: &mut Shots, ctx: &Context) {
        let Some(h) = sh.hold.clone() else { return };
        let quiet = self.shots_val_quiet();
        match self.clip_tick(sh, "prove", h.frames, quiet) {
            Tick::Apply(j) => {
                let u = j as f64 / h.frames as f64;
                if j == 0
                    && let Some(c) = &mut sh.clip
                {
                    c.slow = 1.0; // the series of the cursor read in full
                }
                // the pointer along its points (fractions of the map), each leg eased
                sh.pointer = None;
                if h.path.len() >= 2 {
                    let legs = h.path.len() - 1;
                    let x = (u * legs as f64).min(legs as f64 - 1e-9);
                    let (i, f) = (x.floor() as usize, x.fract());
                    let e = smoother(f) as f32;
                    let r = if h.window { ctx.content_rect() } else { self.clip_map_rect() };
                    let at = |p: (f32, f32)| pos2(r.min.x + r.width() * p.0, r.min.y + r.height() * p.1);
                    sh.pointer = Some(at(h.path[i]) + (at(h.path[i + 1]) - at(h.path[i])) * e);
                }
            }
            Tick::Done => {
                sh.pointer = None;
                sh.hold = None;
            }
            Tick::Wait => {}
        }
    }
}

/// A stretch of the validation tour held for a clip.
#[derive(Clone)]
pub struct Hold {
    pub frames: usize,
    /// the pointer's points, as fractions of the map (or of the window)
    pub path: Vec<(f32, f32)>,
    pub window: bool,
}

/// The stretch a validation screenshot of this name becomes in the clip, if any.
pub fn hold_of(name: &str) -> Option<Hold> {
    let hold = |frames, window, path: &[(f32, f32)]| Some(Hold { frames, path: path.to_vec(), window });
    match name {
        // the sample over the map, the pointer reading the series of the points it passes
        "05-sample" => hold(84, false, &[(0.25, 0.35), (0.6, 0.5), (0.45, 0.75), (0.7, 0.3)]),
        // a point interpreted: its chips, its label, its confidence
        "06-interpretation" => hold(78, true, &[(0.46, 0.80), (0.5, 0.82), (0.79, 0.27), (0.77, 0.4)]),
        // the accuracy and the areas, with their intervals
        "07-accuracy" => hold(108, true, &[(0.62, 0.25), (0.78, 0.36), (0.83, 0.52), (0.86, 0.72)]),
        _ => None,
    }
}
