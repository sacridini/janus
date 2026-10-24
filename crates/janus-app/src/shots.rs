//! The screenshots of the website's guides (`--shots TOUR IN OUT`, site/*.html; src/app_shots.cpp): the program
//! walks through a guide in a window of a fixed size, one stage after the other, and saves the window and some
//! panels as PNGs. A tour gives its panels places of their own in the dock (the tools on the right, the tasks with
//! the statistics), where they are drawn while shown, instead of windows. What the tours share, and the tours of
//! the Zeit tools; the validation's is in shots_validation.rs.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::{Context, Event, Order, PointerButton, Rect};
use egui_dock::{DockState, NodeIndex, TabIndex};
use janus_cube::BandSelection;
use janus_zeit::manifest;
use serde_json::{Value, json};

use crate::app::{JanusApp, State, Tab};
use crate::composite::BandComposite;
use crate::session::Opening;
use crate::zeit::JobState;

/// The window's size in pixels (the C++'s kShotsWidth x kShotsHeight).
pub const SIZE: [f32; 2] = [1680.0, 1050.0];

/// What a crop of a screenshot shows.
#[derive(Clone, Copy, Debug)]
pub enum Target {
    /// a panel's place in the dock; fitted: no lower than its content
    Tab(Tab, bool),
    /// a menu open: its title, its items and the tooltip shown
    Menu(Rect),
}

/// A file of a screenshot: the whole window, a rectangle of it (pixels), or a panel that was not there.
#[derive(Clone, Copy, Debug)]
enum Crop {
    Whole,
    At(Rect),
    Missing,
}

/// The left column's share of the window: 0.25 of the C++'s 0.63 left of the right panels.
const LEFT: f32 = 0.25 * 0.63;

/// The layout made: the places of the panels on the right and of those with the statistics, and those panels.
struct Places {
    /// the map's node with the time series and the statistics under it: the panels on the right split it
    mid: NodeIndex,
    right_tabs: Vec<Tab>,
    under_tabs: Vec<Tab>,
}

#[derive(Default)]
pub struct Shots {
    pub tour: String,
    pub input: String,
    pub out: PathBuf,
    pub stage: usize,
    pub frames: usize,
    since: Option<Instant>,
    /// the run started by the tour (in zeit.jobs), the stage that started it, the result maps before it
    pub(crate) job: Option<usize>,
    pub(crate) job_stage: Option<usize>,
    pub(crate) results: usize,
    /// the layout asked for: the panels on the right, and those with the statistics
    pub(crate) layout: Option<(Vec<Tab>, Vec<Tab>)>,
    places: Option<Places>,
    /// panels brought to the front before the dock is drawn
    pub(crate) focus: Vec<Tab>,
    /// a screenshot asked for (a file name, "-" for no file of the whole window, and crops), then sent (the files)
    /// until it arrives
    shoot: Option<(String, Vec<(String, Target)>)>,
    sent: Option<Vec<(PathBuf, Crop)>>,
    /// the panels in the dock, where each is, and how far down its content goes
    pub(crate) docked: Vec<Tab>,
    rects: HashMap<Tab, Rect>,
    content: HashMap<Tab, f32>,
    /// the panels' scroll range at the last frame
    reach: HashMap<Tab, f32>,
    /// the menus' titles, and the items of the one open in this frame and in the last
    menus: HashMap<&'static str, Rect>,
    popup: Option<Rect>,
    last_popup: Option<Rect>,
    /// the pointer's events for the next frame (none: the pointer away)
    events: Vec<Event>,
    /// a panel scrolled to this offset (f32::MAX: its end)
    pub(crate) scroll: Option<(Tab, f32)>,
    pub(crate) val: crate::shots_validation::ValShots,
    /// the stage whose summary was printed (shots_ml.rs)
    pub(crate) printed: usize,
    /// the tour's end: its exit code
    done: Option<i32>,
}

impl Shots {
    pub fn new(tour: &str, args: &[String]) -> Self {
        Shots { tour: tour.into(), input: args[0].clone(), out: args[1].clone().into(), ..Default::default() }
    }

    /// Seconds since the stage started.
    pub(crate) fn time(&self) -> f64 {
        self.since.map_or(0.0, |s| s.elapsed().as_secs_f64())
    }

    pub(crate) fn next(&mut self, done: &str) {
        println!("[stage {:2}] {done}", self.stage);
        self.stage += 1;
        self.since = Some(Instant::now());
        self.frames = 0;
    }

    /// A screenshot of this frame: `name`.png (unless "-") and its crops, name-suffix.png (suffix.png with "-").
    pub(crate) fn shoot(&mut self, name: &str, crops: &[(&str, Target)]) {
        self.shoot = Some((name.into(), crops.iter().map(|(f, t)| (f.to_string(), *t)).collect()));
    }

    /// A tab's place in the dock; fitted to its content.
    pub(crate) fn tab(t: Tab) -> Target {
        Target::Tab(t, false)
    }

    pub(crate) fn fit(t: Tab) -> Target {
        Target::Tab(t, true)
    }
}

/// A Zeit tool's panel.
pub(crate) fn tool_tab(id: &str) -> Tab {
    Tab::Tool(manifest().tools.iter().position(|t| t.id == id).unwrap_or(0) as u16)
}

/// The rectangle `r` (pixels) of a screenshot as an RGB PNG (src/app_shots.cpp, writeShot).
fn write_shot(path: &Path, img: &egui::ColorImage, crop: Crop) {
    let [w, h] = img.size;
    let r = match crop {
        Crop::Whole => [0, 0, w as i64, h as i64],
        Crop::At(r) => {
            [r.min.x.round() as i64, r.min.y.round() as i64, r.width().round() as i64, r.height().round() as i64]
        }
        Crop::Missing => {
            println!("    COULD NOT WRITE {} (its panel is not shown)", path.display());
            return;
        }
    };
    let x0 = r[0].clamp(0, w as i64 - 1) as usize;
    let y0 = r[1].clamp(0, h as i64 - 1) as usize;
    let cw = r[2].clamp(1, (w - x0) as i64) as usize;
    let ch = r[3].clamp(1, (h - y0) as i64) as usize;
    let mut rgb = Vec::with_capacity(cw * ch * 3);
    for y in y0..y0 + ch {
        for c in &img.pixels[y * w + x0..y * w + x0 + cw] {
            rgb.extend([c.r(), c.g(), c.b()]);
        }
    }
    let ok = (|| -> Result<(), image::ImageError> {
        use image::ImageEncoder as _;
        let f = std::io::BufWriter::new(std::fs::File::create(path)?);
        image::codecs::png::PngEncoder::new_with_quality(
            f,
            image::codecs::png::CompressionType::Best,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(&rgb, cw as u32, ch as u32, image::ExtendedColorType::Rgb8)
    })()
    .is_ok();
    println!("    {} {} ({cw} x {ch})", if ok { "wrote" } else { "COULD NOT WRITE" }, path.display());
}

impl State {
    /// Whether a panel of a tour is shown (its tool open, the tasks shown...).
    pub(crate) fn shown(&self, t: Tab) -> bool {
        match t {
            Tab::Tool(i) => manifest()
                .tools
                .get(i as usize)
                .is_some_and(|tool| self.zeit.tools.get(&tool.id).is_some_and(|u| u.open)),
            Tab::Tasks => self.zeit.show_tasks,
            Tab::Validation => self.val.show,
            Tab::Interpretation => self.labels.show,
            Tab::Chips => self.val.show_chips,
            Tab::DeepLearning => self.ml.open,
            _ => true,
        }
    }

    /// Whether a panel is drawn in the dock (a tour's), not as a window.
    pub(crate) fn docked(&self, t: Tab) -> bool {
        self.shots.as_ref().is_some_and(|s| s.docked.contains(&t))
    }

    /// The offset a tour scrolls a panel to, once.
    pub(crate) fn shots_scroll(&mut self, t: Tab) -> Option<f32> {
        let s = self.shots.as_mut()?;
        let (tab, y) = s.scroll?;
        // egui keeps an offset past the end when the content fits (no scroll bar): the panel drawn empty
        (tab == t).then(|| {
            s.scroll = None;
            y.min(s.reach.get(&t).copied().unwrap_or(0.0))
        })
    }

    /// How far down a panel's content went (fitted crops), and how far it scrolls.
    pub(crate) fn shots_content(&mut self, t: Tab, bottom: f32, reach: f32) {
        if let Some(s) = &mut self.shots {
            s.content.insert(t, bottom);
            s.reach.insert(t, reach);
        }
    }

    /// A menu drawn: its title and, while open, its items.
    pub(crate) fn shots_menu(&mut self, name: &'static str, title: Rect, items: Option<Rect>) {
        if let Some(s) = &mut self.shots {
            s.menus.insert(name, title);
            if items.is_some() {
                s.popup = items;
            }
        }
    }

    /// The tour's frame: None while it goes on, Some(exit code) at its end.
    pub(crate) fn shots_step(&mut self, ctx: &Context) -> Option<i32> {
        ctx.request_repaint();
        let mut sh = self.shots.take()?;
        let r = self.shots_frame(&mut sh, ctx);
        self.shots = Some(sh);
        r
    }

    pub(crate) fn shots_frame(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        if sh.done.is_some() {
            return sh.done; // the window closing
        }
        if let Some(files) = &sh.sent {
            let img = ctx.input(|i| {
                i.raw.events.iter().find_map(|e| match e {
                    Event::Screenshot { image, viewport_id, .. } if *viewport_id == egui::ViewportId::ROOT => {
                        Some(image.clone())
                    }
                    _ => None,
                })
            })?;
            for (path, crop) in files {
                write_shot(path, &img, *crop);
            }
            sh.sent = None;
        }
        if sh.shoot.is_some() {
            return None; // taken at the end of this frame: nothing changes until it is
        }
        if sh.since.is_none() {
            sh.since = Some(Instant::now());
            let _ = std::fs::create_dir_all(&sh.out);
        }
        sh.frames += 1;
        sh.done = match sh.tour.as_str() {
            "validation" => self.shots_validation(sh, ctx),
            "snic" | "ml" | "presto" => self.shots_ml(sh, ctx),
            _ => self.shots_tool(sh, ctx),
        };
        sh.done
    }

    /// Nothing loading: the series, result maps, the composite's image.
    pub(crate) fn shots_quiet(&self) -> bool {
        let Some(s) = &self.session else { return false };
        if self.opening.is_some() || !s.s.overview.complete() || self.zeit.loads_pending() {
            return false;
        }
        self.composite.as_ref().is_none_or(|c| c.ready(self.display.t, s.info.t()))
    }

    /// The tour's input opened as the series, with a band selection.
    pub(crate) fn shots_open(&mut self, input: &str, sel: BandSelection, ctx: &Context) {
        self.error = None;
        self.opening = Some(Opening::start_with(vec![input.to_string()], false, sel, ctx));
    }

    /// Another input added as a layer.
    pub(crate) fn shots_add(&mut self, input: &str, ctx: &Context) {
        self.opening = Some(Opening::start_with(vec![input.to_string()], true, BandSelection::default(), ctx));
    }

    /// A tool's panel shown with its defaults and `params`, fitted on the chart.
    pub(crate) fn shots_open_tool(&mut self, id: &str, params: Value) -> bool {
        let Some(tool) = manifest().tool(id) else { return false };
        let u = self.zeit.tools.entry(id.to_string()).or_default();
        u.open = true;
        u.params = tool.params.iter().map(|p| (p.id.clone(), p.default.clone())).collect();
        if let Some(o) = params.as_object() {
            u.params.extend(o.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        self.zeit.fit_only(tool.modes.iter().any(|m| m == "pixel").then_some(id));
        true
    }

    pub(crate) fn shots_set_params(&mut self, id: &str, params: Value) {
        let u = self.zeit.tools.entry(id.to_string()).or_default();
        if let Some(o) = params.as_object() {
            u.params.extend(o.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        u.version += 1;
    }

    /// The tool's run on the raster (scope: 0 whole image, 1 visible area, 2 ROI).
    pub(crate) fn shots_run(&mut self, sh: &mut Shots, id: &str, scope: usize, ctx: &Context) -> bool {
        let Some(tool) = manifest().tool(id) else { return false };
        self.zeit.tools.entry(id.to_string()).or_default().scope = scope;
        let before = self.zeit.jobs.len();
        sh.results = self.zeit.results.len();
        self.run_tool(tool, ctx);
        if self.zeit.jobs.len() == before {
            return false;
        }
        sh.job = Some(self.zeit.jobs.len() - 1);
        true
    }

    /// The run: -1 running (or its maps loading), 0 done with its maps on the map, 1 failed.
    pub(crate) fn shots_ran(&self, sh: &Shots) -> i32 {
        let Some(j) = sh.job.and_then(|j| self.zeit.jobs.get(j)) else { return 1 };
        let state = j.status.lock().unwrap().state;
        match state {
            JobState::Running => -1,
            JobState::Done if j.loaded && !self.zeit.loads_pending() && self.zeit.results.len() > sh.results => 0,
            JobState::Done => -1,
            _ => 1,
        }
    }

    pub(crate) fn shots_job_error(&self, sh: &Shots) -> String {
        sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(String::new(), |j| j.status.lock().unwrap().error.clone())
    }

    /// Only the result `name` (e.g. "Year of detection") of the last run drawn.
    pub(crate) fn shots_show_result(&mut self, sh: &Shots, name: &str) -> bool {
        let mut found = false;
        for (i, r) in self.zeit.results.iter_mut().enumerate() {
            r.visible = i >= sh.results && r.name.ends_with(name);
            found |= r.visible;
        }
        self.map.dirty = true;
        found
    }

    pub(crate) fn shots_hide_results(&mut self) {
        for r in &mut self.zeit.results {
            r.visible = false;
        }
        self.map.dirty = true;
    }

    pub(crate) fn shots_colormap(&mut self, name: &str) {
        if let Some(k) = janus_gpu::colormaps::by_name(name) {
            self.display.cmap[janus_gpu::Mode::Value as usize] = k;
        }
        self.map.dirty = true;
    }

    /// A pin at (x, y) in the active layer's CRS.
    pub(crate) fn shots_pin_at(&mut self, x: f64, y: f64) {
        if let Some((px, py)) = self.session.as_ref().and_then(|s| s.info.geo_to_pixel(x, y))
            && px >= 0.0
            && py >= 0.0
        {
            self.add_pin(px as usize, py as usize);
        }
    }

    /// The map centred on the active layer's pixel (px, py), `scale` screen points per pixel.
    pub(crate) fn shots_zoom_to(&mut self, px: f64, py: f64, scale: f64) {
        let Some(g) = self.grid() else { return };
        self.map.set_view([(px + 0.5) / g.fx, (py + 0.5) / g.fy], scale * g.fx);
        self.map.touched = true;
    }

    pub(crate) fn shots_zoom_to_geo(&mut self, x: f64, y: f64, scale: f64) {
        if let Some((px, py)) = self.session.as_ref().and_then(|s| s.info.geo_to_pixel(x, y)) {
            self.shots_zoom_to(px, py, scale);
        }
    }

    /// The first date whose label starts with `prefix` (e.g. "2025-01"), else the last.
    pub(crate) fn shots_date_of(&self, prefix: &str) -> usize {
        let Some(s) = &self.session else { return 0 };
        s.info.layers.iter().position(|l| l.label.starts_with(prefix)).unwrap_or(s.info.t() - 1)
    }

    /// The stages every tour of a raster run has: started (and its task shot), -1 running, 0 next, 1 failed.
    pub(crate) fn shots_run_stage(
        &mut self,
        sh: &mut Shots,
        id: &str,
        scope: usize,
        task_shot: Option<&str>,
        ctx: &Context,
    ) -> i32 {
        if sh.job_stage != Some(sh.stage) {
            if !self.shots_run(sh, id, scope, ctx) {
                return 1;
            }
            sh.job_stage = Some(sh.stage);
            self.zeit.show_tasks = true;
            return -1;
        }
        sh.focus.push(Tab::Tasks);
        let progress = sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(0.0, |j| j.status.lock().unwrap().progress);
        if task_shot.is_some() && progress < 0.2 && self.shots_ran(sh) < 0 {
            return -1;
        }
        if let Some(f) = task_shot {
            sh.shoot("-", &[(f, Shots::fit(Tab::Tasks))]);
        }
        0
    }

    /// -1 running, 0 done (next stage), 1 failed.
    pub(crate) fn shots_done_stage(&mut self, sh: &mut Shots) -> i32 {
        let r = self.shots_ran(sh);
        if r != 0 {
            return r;
        }
        let secs = sh.job.and_then(|j| self.zeit.jobs.get(j)).map_or(0.0, |j| j.started.elapsed().as_secs_f64());
        sh.next(&format!("the run done in {secs:.0} s"));
        0
    }

    /// A menu opened by a click on its title, the pointer on its first item or on the title, its picture with the
    /// tooltip shown once it has been open 2 s (src/app_shots.cpp, shotsMenu).
    pub(crate) fn shots_menu_shot(&mut self, sh: &mut Shots, menu: &str, file: &str, hover_first: bool) -> bool {
        let Some(&m) = sh.menus.get(menu) else { return false };
        if sh.frames <= 2 {
            let c = m.center();
            sh.events.push(Event::PointerMoved(c));
            sh.events.push(Event::PointerButton {
                pos: c,
                button: PointerButton::Primary,
                pressed: sh.frames == 1,
                modifiers: Default::default(),
            });
            return false;
        }
        let Some(p) = sh.last_popup else { return false };
        let font = self.settings.s.font_size as f32;
        let at = if hover_first { egui::pos2(p.min.x + 60.0, p.min.y + font * 0.5 + 2.0) } else { m.center() };
        sh.events.push(Event::PointerMoved(at));
        if sh.time() < 2.0 {
            return false;
        }
        sh.shoot("-", &[(file, Target::Menu(m))]);
        true
    }

    /// The Landsat series of the guides: NIR and red as NDVI (the quality band found by its name).
    pub(crate) fn shots_open_landsat(&mut self, sh: &Shots, ctx: &Context) {
        let sel = BandSelection { band: 4, nd_band: 3, ..Default::default() };
        let input = sh.input.clone();
        self.shots_open(&input, sel, ctx);
    }

    /// The map in SWIR1 / NIR / red at the last date.
    pub(crate) fn shots_landsat_view(&mut self) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        self.composite = Some(BandComposite::of(info.id, [5, 4, 3]));
        self.set_date(info.t() - 1);
    }

    /// The tours of the Zeit tools (src/app_shots.cpp, shotsToolStep).
    pub(crate) fn shots_tool(&mut self, sh: &mut Shots, ctx: &Context) -> Option<i32> {
        let tour = sh.tour.clone();
        let fail = |sh: &Shots, what: &str| {
            println!("FAIL ({tour}, stage {}): {what}", sh.stage);
            Some(1)
        };
        let t = sh.time();
        // a run's stages (it starts in one and is waited for in the next) may take long
        let waiting = sh.job.is_some() && sh.job_stage.is_some_and(|s| sh.stage <= s + 1);
        if t > if waiting { 7200.0 } else { 120.0 } {
            return fail(sh, "timeout");
        }
        let tool_win = tool_tab(&sh.tour);
        let ready = |s: &State| s.opening.is_none() && s.session.is_some();
        let n_dates = |s: &State| s.session.as_ref().map_or(0, |s| s.info.t());

        if sh.tour == "landtrendr" {
            // IN: an annual series (site/landtrendr.html: NDVI x 10000 of Rondonia, 1985-2024)
            let cleared = [-63.083966, -10.356003]; // forest until 2003, pasture from 2004
            let regrown = [-63.344298, -10.220986]; // cleared in 1999, forest again since
            let forest = [-63.202544, -10.326628]; // forest throughout
            match sh.stage {
                0 => {
                    let input = sh.input.clone();
                    self.shots_open(&input, BandSelection::default(), ctx);
                    sh.next("the annual series opening");
                }
                1 => {
                    // three pins: forest cleared in 2004, cleared in 1999 and grown back, forest throughout
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![tool_win], vec![Tab::Tasks]));
                    self.shots_colormap("NDVI");
                    self.set_date(n_dates(self) - 1);
                    self.shots_pin_at(cleared[0], cleared[1]);
                    self.shots_pin_at(regrown[0], regrown[1]);
                    self.shots_pin_at(forest[0], forest[1]);
                    sh.next(&format!("{} dates; three pins", n_dates(self)));
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit(); // once the panels have their size
                    }
                    if !self.shots_quiet() || t < 4.0 {
                        return None;
                    }
                    sh.shoot("01-series", &[("chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("the series and three pins");
                }
                3 => {
                    // the Tools menu
                    if !self.shots_menu_shot(sh, "Tools", "02-menu", false) {
                        if t > 10.0 {
                            return fail(sh, "the Tools menu did not open");
                        }
                        return None;
                    }
                    sh.next("the Tools menu");
                }
                4 => {
                    // LandTrendr fitted on the clearing
                    if sh.frames == 1 {
                        egui::Popup::close_all(ctx);
                        self.clear_pins(); // one at a time on the chart
                        self.shots_pin_at(cleared[0], cleared[1]);
                        if !self.shots_open_tool("landtrendr", json!({})) {
                            return fail(sh, "no LandTrendr tool");
                        }
                        return None;
                    }
                    if sh.frames == 3 {
                        sh.focus.push(tool_win);
                    }
                    if !self.shots_quiet() || t < 6.0 {
                        return None;
                    }
                    sh.shoot(
                        "03-fit",
                        &[
                            ("tool", Shots::fit(tool_win)),
                            ("chart", Shots::tab(Tab::TimeSeries)),
                            ("stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("LandTrendr on the clearing");
                }
                5 => {
                    // looking for gains: the regrowth
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(regrown[0], regrown[1]);
                        self.shots_set_params("landtrendr", json!({"event_type": "gain"}));
                    }
                    if t < 5.0 {
                        return None;
                    }
                    sh.shoot(
                        "-",
                        &[
                            ("04-gain-chart", Shots::tab(Tab::TimeSeries)),
                            ("04-gain-stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("gain on the regrowth");
                }
                6 => {
                    // fewer segments
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(cleared[0], cleared[1]);
                        self.shots_set_params("landtrendr", json!({"event_type": "loss", "max_segments": 2}));
                    }
                    if t < 5.0 {
                        return None;
                    }
                    sh.shoot("-", &[("05-two-segments", Shots::tab(Tab::TimeSeries))]);
                    sh.next("two segments at most");
                }
                7 => {
                    // the run on the whole image: losses of 0.25 NDVI or more that start in forest
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
                    if progress < 0.25 && self.shots_ran(sh) < 0 {
                        return None;
                    }
                    sh.shoot("-", &[("06-task", Shots::fit(Tab::Tasks))]);
                    sh.next("the run, under way");
                }
                8 => match self.shots_ran(sh) {
                    r if r < 0 => {}
                    r if r > 0 => return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh))),
                    _ => {
                        self.shots_done_stage(sh);
                    }
                },
                9 => {
                    if sh.frames == 1 {
                        if !self.shots_show_result(sh, "Year of detection") {
                            return fail(sh, "no year of detection");
                        }
                        sh.focus.push(tool_win);
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("07-yod", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("year of detection");
                }
                10 => {
                    if sh.frames == 1 {
                        self.shots_show_result(sh, "Magnitude");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("08-magnitude", &[]);
                    sh.next("magnitude");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        // Landsat surface reflectance, one file per date with its QA_PIXEL (tools/make_guide_data.py landsat:
        // Rondonia, 2020-2026): the map in SWIR1 / NIR / red, the chart in NDVI
        let clearing = [-63.119001, -10.130167]; // forest cleared in May-June 2022
        let thinned = [-63.309533, -10.294558]; // forest opened in 2021, partly back since
        let standing = [-63.202544, -10.326628]; // forest throughout

        if sh.tour == "ccdc" {
            let ccdc = tool_tab("ccdc");
            let synthetic = tool_tab("ccdc_synthetic");
            match sh.stage {
                0 => {
                    self.shots_open_landsat(sh, ctx);
                    sh.next("the Landsat series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![ccdc, synthetic], vec![Tab::Tasks]));
                    self.shots_landsat_view();
                    self.shots_pin_at(clearing[0], clearing[1]);
                    self.shots_pin_at(thinned[0], thinned[1]);
                    self.shots_pin_at(standing[0], standing[1]);
                    sh.next(&format!("{} dates; three pins", n_dates(self)));
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    if !self.shots_quiet() || t < 5.0 {
                        return None;
                    }
                    sh.shoot("01-series", &[("chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("the series and three pins");
                }
                3 => {
                    // CCDC on the clearing
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(clearing[0], clearing[1]);
                        if !self.shots_open_tool("ccdc", json!({})) {
                            return fail(sh, "no CCDC tool");
                        }
                        return None;
                    }
                    if sh.frames == 3 {
                        sh.focus.push(ccdc);
                    }
                    if !self.shots_quiet() || t < 10.0 {
                        return None;
                    }
                    sh.shoot(
                        "02-fit",
                        &[
                            ("tool", Shots::fit(ccdc)),
                            ("chart", Shots::tab(Tab::TimeSeries)),
                            ("stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("CCDC on the clearing");
                }
                4 => {
                    // and on the forest opened in 2021
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(thinned[0], thinned[1]);
                        self.zeit.version += 1;
                    }
                    if t < 8.0 {
                        return None;
                    }
                    sh.shoot(
                        "-",
                        &[
                            ("03-thinned-chart", Shots::tab(Tab::TimeSeries)),
                            ("03-thinned-stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("CCDC on the opened forest");
                }
                5 => {
                    // the run on the area around the clearing
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(clearing[0], clearing[1]);
                        self.shots_zoom_to_geo(clearing[0], clearing[1], 3.0);
                    }
                    if sh.frames < 10 {
                        return None; // the zoom drawn before the visible area is taken
                    }
                    match self.shots_run_stage(sh, "ccdc", 1, Some("04-task"), ctx) {
                        1 => return fail(sh, "the run did not start"),
                        0 => sh.next("CCDC over the visible area"),
                        _ => {}
                    }
                }
                6 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                    }
                }
                7 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Largest break date") {
                        return fail(sh, "no break date");
                    }
                    if sh.frames == 3 {
                        sh.focus.push(ccdc);
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("05-break-date", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the largest break's date");
                }
                8 => {
                    if sh.frames == 1 {
                        self.shots_show_result(sh, "Largest break magnitude");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("06-magnitude", &[]);
                    sh.next("its magnitude");
                }
                9 => {
                    // the synthetic image of a cloudy date
                    if sh.frames == 1 {
                        self.shots_hide_results();
                        self.set_date(self.shots_date_of("2025-01"));
                        if !self.shots_open_tool("ccdc_synthetic", json!({})) {
                            return fail(sh, "no CCDC synthetic image tool");
                        }
                        return None;
                    }
                    if sh.frames == 3 {
                        sh.focus.push(synthetic);
                    }
                    if !self.shots_quiet() || t < 8.0 {
                        return None;
                    }
                    sh.shoot("07-observed", &[("tool", Shots::fit(synthetic))]);
                    let label =
                        self.session.as_ref().map_or(String::new(), |s| s.info.layers[self.display.t].label.clone());
                    sh.next(&format!("the observed image of {label}"));
                }
                10 => match self.shots_run_stage(sh, "ccdc_synthetic", 1, None, ctx) {
                    1 => return fail(sh, "the synthetic run did not start"),
                    0 => sh.next("the synthetic image under way"),
                    _ => {}
                },
                11 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the synthetic run failed: {}", self.shots_job_error(sh)));
                    }
                }
                12 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Synthetic (shown series)") {
                        return fail(sh, "no synthetic image");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("08-synthetic", &[]);
                    sh.next("the synthetic NDVI");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        if sh.tour == "coded" {
            let params = json!({"monitor_start": 2022.0, "train_years": 2.0});
            let coded = tool_tab("coded");
            let ndfi = tool_tab("ndfi");
            match sh.stage {
                0 => {
                    self.shots_open_landsat(sh, ctx);
                    sh.next("the Landsat series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![coded, ndfi], vec![Tab::Tasks]));
                    self.shots_landsat_view();
                    self.shots_pin_at(clearing[0], clearing[1]);
                    sh.next("the clearing pinned");
                }
                2 => {
                    // the NDFI of every date, over the area around the clearing
                    if sh.frames == 30 {
                        self.shots_zoom_to_geo(clearing[0], clearing[1], 3.0);
                    }
                    if sh.frames < 40 || !self.shots_quiet() {
                        return None;
                    }
                    if !self.shown(ndfi) && !self.shots_open_tool("ndfi", json!({})) {
                        return fail(sh, "no NDFI tool");
                    }
                    sh.focus.push(ndfi);
                    if !sh.docked.contains(&ndfi) || t < 6.0 {
                        return None;
                    }
                    sh.shoot("-", &[("01-ndfi-tool", Shots::fit(ndfi))]);
                    sh.next("the NDFI tool");
                }
                3 => match self.shots_run_stage(sh, "ndfi", 1, None, ctx) {
                    1 => return fail(sh, "the NDFI run did not start"),
                    0 => sh.next("the NDFI under way"),
                    _ => {}
                },
                4 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the NDFI run failed: {}", self.shots_job_error(sh)));
                    }
                }
                5 => {
                    // the NDFI as a layer, active: its series on the chart
                    if sh.frames == 1 {
                        if self.layers.len() < 2 {
                            return fail(sh, "the NDFI should open as a layer");
                        }
                        self.shots_hide_results(); // the series itself, not its summary maps
                        self.zeit.tools.entry("ndfi".into()).or_default().open = false;
                        self.set_active(self.layers.len() - 1, ctx);
                        self.shots_colormap("NDVI");
                        self.set_date(self.shots_date_of("2022-07"));
                    }
                    if !self.shots_quiet() || t < 6.0 {
                        return None;
                    }
                    sh.shoot("02-ndfi", &[("chart", Shots::tab(Tab::TimeSeries)), ("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the NDFI layer");
                }
                6 => {
                    // CODED on the clearing (the NDVI layer active again)
                    if sh.frames == 1 {
                        self.set_active(0, ctx);
                        self.set_date(n_dates(self).saturating_sub(1));
                        if !self.shots_open_tool("coded", params) {
                            return fail(sh, "no CODED tool");
                        }
                        return None;
                    }
                    if sh.frames == 3 {
                        sh.focus.push(coded);
                        sh.focus.push(Tab::Statistics);
                    }
                    if !self.shots_quiet() || t < 10.0 {
                        return None;
                    }
                    sh.shoot(
                        "03-fit",
                        &[
                            ("tool", Shots::fit(coded)),
                            ("chart", Shots::tab(Tab::TimeSeries)),
                            ("stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("CODED on the clearing");
                }
                7 => {
                    // and on the forest opened in 2021
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(thinned[0], thinned[1]);
                        self.zeit.version += 1;
                    }
                    if t < 8.0 {
                        return None;
                    }
                    sh.shoot(
                        "-",
                        &[
                            ("04-thinned-chart", Shots::tab(Tab::TimeSeries)),
                            ("04-thinned-stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("CODED on the opened forest");
                }
                8 => {
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.shots_pin_at(clearing[0], clearing[1]);
                        for l in self.layers.iter_mut().skip(1) {
                            l.visible = false; // the NDFI under the results
                        }
                        self.map.dirty = true;
                    }
                    match self.shots_run_stage(sh, "coded", 1, None, ctx) {
                        1 => return fail(sh, "the CODED run did not start"),
                        0 => sh.next("CODED over the visible area"),
                        _ => {}
                    }
                }
                9 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the CODED run failed: {}", self.shots_job_error(sh)));
                    }
                }
                10 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Strata") {
                        return fail(sh, "no strata");
                    }
                    if sh.frames == 3 {
                        sh.focus.push(coded);
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("05-strata", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the strata");
                }
                11 => {
                    if sh.frames == 1 {
                        self.shots_show_result(sh, "First change date");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("06-first-change", &[]);
                    sh.next("the first change's date");
                }
                12 => {
                    // stricter: 5 RMSEs below the training model
                    if sh.frames == 1 {
                        self.shots_set_params("coded", json!({"thresh": 5.0}));
                    }
                    match self.shots_run_stage(sh, "coded", 1, None, ctx) {
                        1 => return fail(sh, "the second CODED run did not start"),
                        0 => sh.next("CODED again, 5 RMSEs"),
                        _ => {}
                    }
                }
                13 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the second CODED run failed: {}", self.shots_job_error(sh)));
                    }
                }
                14 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Strata") {
                        return fail(sh, "no strata");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("07-strata-5", &[]);
                    sh.next("the strata at 5 RMSEs");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        if sh.tour == "bfast" {
            let lasting = (435, 153); // EVI 0.42 a year until 2012, about 0.3 since
            let dropped = (334, 485); // lower from 2009
            let (bfast, lite, monitor) = (tool_tab("bfast"), tool_tab("bfast_lite"), tool_tab("bfast_monitor"));
            match sh.stage {
                0 => {
                    let input = sh.input.clone();
                    self.shots_open(&input, BandSelection::default(), ctx);
                    sh.next("the EVI series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![bfast, lite, monitor], vec![Tab::Tasks]));
                    self.shots_colormap("NDVI");
                    self.set_date(self.shots_date_of("2025-07"));
                    self.add_pin(lasting.0, lasting.1);
                    sh.next(&format!("{} dates; a pin", n_dates(self)));
                }
                2 => {
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    if !self.shots_quiet() || t < 4.0 {
                        return None;
                    }
                    sh.shoot("01-series", &[("chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("the series");
                }
                3..=5 => {
                    // BFAST, BFAST Lite, BFAST Monitor (monitoring from 2010) on the chart
                    let (id, tab, params, wait, file, done) = match sh.stage {
                        3 => ("bfast", bfast, json!({}), 12.0, "02-bfast", "BFAST on the chart"),
                        4 => ("bfast_lite", lite, json!({}), 10.0, "03-lite", "BFAST Lite on the chart"),
                        _ => (
                            "bfast_monitor",
                            monitor,
                            json!({"monitor_start": 2010.0}),
                            10.0,
                            "04-monitor",
                            "BFAST Monitor on the chart",
                        ),
                    };
                    if sh.frames == 1 && !self.shots_open_tool(id, params) {
                        return fail(sh, &format!("no {id} tool"));
                    }
                    if sh.frames == 3 {
                        sh.focus.push(tab);
                    }
                    if !self.shots_quiet() || t < wait {
                        return None;
                    }
                    sh.shoot(
                        file,
                        &[
                            ("tool", Shots::fit(tab)),
                            ("chart", Shots::tab(Tab::TimeSeries)),
                            ("stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next(done);
                }
                6 => {
                    // the other pin
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.add_pin(dropped.0, dropped.1);
                        self.shots_open_tool("bfast_lite", json!({}));
                    }
                    if t < 8.0 {
                        return None;
                    }
                    sh.shoot("-", &[("05-dropped-chart", Shots::tab(Tab::TimeSeries))]);
                    sh.next("BFAST Lite on the other pin");
                }
                7 => {
                    // BFAST Monitor (monitoring from 2010) over the whole image: seconds, where BFAST takes an hour
                    if sh.frames == 1 {
                        self.clear_pins();
                        self.add_pin(lasting.0, lasting.1);
                        self.shots_open_tool("bfast_monitor", json!({"monitor_start": 2010.0}));
                        sh.focus.push(monitor);
                    }
                    if sh.frames < 10 {
                        return None;
                    }
                    match self.shots_run_stage(sh, "bfast_monitor", 0, Some("06-task"), ctx) {
                        1 => return fail(sh, "the run did not start"),
                        0 => sh.next("BFAST Monitor over the image"),
                        _ => {}
                    }
                }
                8 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                    }
                }
                9 => {
                    if sh.frames == 1 && !self.shots_show_result(sh, "Break date") {
                        return fail(sh, "no break date");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("07-break-date", &[("layers", Shots::tab(Tab::Layers))]);
                    sh.next("the break dates");
                }
                10 => {
                    if sh.frames == 1 {
                        self.shots_show_result(sh, "Magnitude (median residual)");
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    sh.shoot("08-magnitude", &[]);
                    sh.next("the magnitudes");
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }

        if sh.tour == "phenology" {
            let crop = (71, 319); // a field: a strong season every year
            let pheno = tool_tab("phenology");
            match sh.stage {
                0 => {
                    let input = sh.input.clone();
                    self.shots_open(&input, BandSelection::default(), ctx);
                    sh.next("the EVI series opening");
                }
                1 => {
                    if !ready(self) {
                        return None;
                    }
                    sh.layout = Some((vec![pheno], vec![Tab::Tasks]));
                    self.shots_colormap("NDVI");
                    self.set_date(self.shots_date_of("2024-06"));
                    self.add_pin(crop.0, crop.1);
                    sh.next("a pin on a field");
                }
                2 => {
                    // Phenology on the chart
                    if sh.frames == 30 {
                        self.map.fit();
                    }
                    if sh.frames < 40 || !self.shots_quiet() {
                        return None;
                    }
                    if !self.shown(pheno) && !self.shots_open_tool("phenology", json!({})) {
                        return fail(sh, "no Phenology tool");
                    }
                    sh.focus.push(pheno);
                    if !sh.docked.contains(&pheno) || t < 12.0 {
                        return None;
                    }
                    sh.shoot(
                        "01-fit",
                        &[
                            ("tool", Shots::fit(pheno)),
                            ("chart", Shots::tab(Tab::TimeSeries)),
                            ("stats", Shots::tab(Tab::Statistics)),
                        ],
                    );
                    sh.next("Phenology on the chart");
                }
                3 => match self.shots_run_stage(sh, "phenology", 0, Some("02-task"), ctx) {
                    1 => return fail(sh, "the run did not start"),
                    0 => sh.next("Phenology over the image"),
                    _ => {}
                },
                4 => {
                    if self.shots_done_stage(sh) > 0 {
                        return fail(sh, &format!("the run failed: {}", self.shots_job_error(sh)));
                    }
                }
                5..=7 => {
                    let (name, file, done) = match sh.stage {
                        5 => ("Start of season (SOS)", "03-sos", "the start of the season"),
                        6 => ("Peak of season (POS)", "04-pos", "the peak"),
                        _ => ("Length of season (EOS - SOS)", "05-los", "the length of the season"),
                    };
                    if sh.frames == 1 && !self.shots_show_result(sh, name) {
                        return fail(sh, &format!("no {name}"));
                    }
                    if !self.shots_quiet() || t < 3.0 {
                        return None;
                    }
                    if sh.stage == 5 {
                        sh.shoot(file, &[("layers", Shots::tab(Tab::Layers))]);
                    } else {
                        sh.shoot(file, &[]);
                    }
                    sh.next(done);
                }
                _ => {
                    println!("OK");
                    return Some(0);
                }
            }
            return None;
        }
        fail(sh, "unknown tour (validation, landtrendr, ccdc, coded, bfast, phenology, snic, ml, presto)")
    }
}

impl JanusApp {
    /// Before the dock is drawn: the tour's layout made, its panels in their places while shown, those asked for
    /// in front.
    pub(crate) fn shots_dock(&mut self) {
        let Some(mut sh) = self.state.shots.take() else { return };
        if let Some((right_tabs, under_tabs)) = sh.layout.take() {
            // src/app_shots.cpp, shotsDock: Layers and Files above Display on the left (0.25), the map above the
            // time series, the statistics (and `under`) at their right; the panels on the right (0.37) while one is
            // shown, as ImGui's empty dock node takes no room
            let mut d = DockState::new(vec![Tab::Map]);
            let s = d.main_surface_mut();
            let [mid, left] = s.split_left(NodeIndex::root(), LEFT, vec![Tab::Layers, Tab::Files]);
            let [_, _] = s.split_below(left, 0.45, vec![Tab::Display]);
            let [_, bottom] = s.split_below(mid, 0.64, vec![Tab::TimeSeries]);
            let [_, _] = s.split_right(bottom, 0.52, vec![Tab::Statistics]);
            self.dock = d;
            sh.places = Some(Places { mid, right_tabs, under_tabs });
        }
        if let Some(p) = &sh.places {
            for (right, tabs) in [(true, &p.right_tabs), (false, &p.under_tabs)] {
                for &t in tabs {
                    let found = self.dock.find_tab(&t);
                    if self.state.shown(t) && found.is_none() {
                        // as a window docked when it appears: in front, with the others of its place (the nodes
                        // found by their tabs: removing a leaf renumbers them)
                        let with = if right {
                            tabs.iter().find_map(|o| self.dock.find_tab(o))
                        } else {
                            self.dock.find_tab(&Tab::Statistics)
                        };
                        let s = self.dock.main_surface_mut();
                        match with {
                            Some(at) => {
                                let node = at.node;
                                s[node].append_tab(t);
                                let n = s[node].tabs_count();
                                let _ = s.set_active_tab(node, TabIndex(n - 1));
                            }
                            None => {
                                // the map keeps 0.63 of the window less the left's; removing the right leaf
                                // gives `mid` its subtree back
                                let _ = s.split_right(p.mid, (0.63 - LEFT) / (1.0 - LEFT), vec![t]);
                            }
                        }
                    } else if !self.state.shown(t)
                        && let Some(at) = found
                    {
                        self.dock.remove_tab(at);
                    }
                }
            }
        }
        for t in sh.focus.drain(..) {
            if let Some(at) = self.dock.find_tab(&t) {
                let _ = self.dock.set_active_tab(at);
            }
        }
        sh.docked = self.dock.iter_all_tabs().map(|(_, t)| *t).collect();
        self.state.shots = Some(sh);
    }

    /// After the frame is drawn: where each panel is, and the screenshot asked for sent with its crops.
    pub(crate) fn shots_frame_end(&mut self, ctx: &Context) {
        let Some(sh) = self.state.shots.as_mut() else { return };
        sh.rects.clear();
        for (path, leaf) in self.dock.iter_leaves() {
            if path.surface.is_main() {
                for t in leaf.tabs() {
                    sh.rects.insert(*t, leaf.rect());
                }
            }
        }
        sh.last_popup = sh.popup;
        let Some((name, crops)) = sh.shoot.take() else {
            sh.popup = None;
            return;
        };
        let ppp = ctx.pixels_per_point();
        let px = |r: Rect| Rect::from_min_max((r.min.to_vec2() * ppp).to_pos2(), (r.max.to_vec2() * ppp).to_pos2());
        let mut files = Vec::new();
        if name != "-" {
            files.push((sh.out.join(format!("{name}.png")), Crop::Whole));
        }
        let tooltips: Vec<Rect> = ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|l| l.order == Order::Tooltip)
                .filter_map(|l| m.area_rect(l.id))
                .collect()
        });
        for (suffix, t) in crops {
            let file = if name == "-" { format!("{suffix}.png") } else { format!("{name}-{suffix}.png") };
            let r = match t {
                Target::Tab(tab, fit) => sh.rects.get(&tab).map(|&r| match sh.content.get(&tab) {
                    // as the C++: down to the content, its padding and a little more
                    Some(&bottom) if fit => Rect::from_min_max(r.min, egui::pos2(r.max.x, r.max.y.min(bottom + 14.0))),
                    _ => r,
                }),
                Target::Menu(title) => sh.popup.map(|p| {
                    let mut r = title.union(p.expand(6.0));
                    for t in &tooltips {
                        r = r.union(*t);
                    }
                    r.expand(12.0)
                }),
            };
            files.push((sh.out.join(file), r.map_or(Crop::Missing, |r| Crop::At(px(r)))));
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        sh.sent = Some(files);
        sh.popup = None;
    }

    /// The pointer of the tour alone: the user's mouse over the window does nothing.
    pub(crate) fn shots_input(&mut self, raw: &mut egui::RawInput) {
        let Some(sh) = self.state.shots.as_mut() else { return };
        raw.events.retain(|e| {
            !matches!(
                e,
                Event::PointerMoved(_)
                    | Event::MouseMoved(_)
                    | Event::PointerButton { .. }
                    | Event::PointerGone
                    | Event::MouseWheel { .. }
            )
        });
        if sh.events.is_empty() {
            raw.events.push(Event::PointerGone);
        } else {
            raw.events.append(&mut sh.events);
        }
    }
}
