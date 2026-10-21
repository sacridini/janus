//! The application: its state, the dock of panels (the C++'s default layout, app.cpp dockDefaultLayout), panels
//! dragged out of the window into windows of their own (the C++'s ImGui viewports), the menu, the shortcuts and
//! the startup measurements.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui_wgpu::RenderState;
use egui::{Id, Ui, ViewportBuilder, ViewportId, WidgetText};
use egui_dock::{DockArea, DockState, NodeIndex, SurfaceIndex, TabViewer};
use janus_gpu::Gpu;

use crate::display::Display;
use crate::map::{MapView, Pin, PixelAt};
use crate::session::{Opening, Session};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    Map,
    Layers,
    Display,
    TimeSeries,
    Statistics,
    Performance,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Map => "Map",
            Tab::Layers => "Layers",
            Tab::Display => "Display",
            Tab::TimeSeries => "Time series",
            Tab::Statistics => "Statistics",
            Tab::Performance => "Performance",
        }
    }
}

/// The C++'s default layout: Layers above Display and Performance on the left (0.21), the map in the centre, the
/// time series and the statistics below it (0.36).
fn default_layout() -> DockState<Tab> {
    let mut d = DockState::new(vec![Tab::Map]);
    let s = d.main_surface_mut();
    let [map, left] = s.split_left(NodeIndex::root(), 0.21, vec![Tab::Layers]);
    let [_, _] = s.split_below(left, 0.40, vec![Tab::Display, Tab::Performance]);
    let [_, bottom] = s.split_below(map, 0.64, vec![Tab::TimeSeries]);
    let [_, _] = s.split_right(bottom, 0.66, vec![Tab::Statistics]);
    d
}

/// Options of the command line.
#[derive(Default, Clone)]
pub struct Options {
    pub inputs: Vec<String>,
    pub budget_mb: i64,
    pub measure_startup: bool,
    /// a PNG of the window once the series is ready, then exit
    pub screenshot: Option<PathBuf>,
    /// the map opened in a window of its own (to check detached panels); with --screenshot, both windows saved
    pub detach_map: bool,
    /// --profile-tour: the C++'s fixed tour of the series, timed stage by stage
    pub profile_tour: bool,
    /// the tour's timings as JSON
    pub tour_json: Option<PathBuf>,
    /// --dump-golden: the folder of the maps and statistics to compare with the C++'s
    pub dump_golden: Option<PathBuf>,
    /// --basemap: a basemap (a preset's id or an XYZ URL) shown once the series opens
    pub basemap: Option<String>,
    /// --selftest-composite: the RGB composite of a series with several bands per date against its files
    pub selftest_composite: bool,
    /// the overview's budget in bytes, for the tests (instead of budget_mb)
    pub budget_bytes: Option<i64>,
}

/// The exit code of a tour or a test run in the window (main returns it once the window closed).
pub static EXIT_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Times since the process started (ms), as the C++'s --measure-startup prints them.
#[derive(Default)]
pub struct Startup {
    pub gdal_ms: f64,
    pub window_ms: f64,
    pub first_frame_ms: Option<f64>,
    /// the map's pipelines, made after the first frame
    pub gpu_ms: Option<f64>,
}

/// The interface's CPU time per frame, the last few hundred frames.
#[derive(Default)]
pub struct Perf {
    ui_ms: VecDeque<f64>,
    pub last_cpu: Option<f32>,
}

impl Perf {
    fn push(&mut self, ms: f64) {
        if self.ui_ms.len() == 300 {
            self.ui_ms.pop_front();
        }
        self.ui_ms.push_back(ms);
    }

    pub fn summary(&self) -> Option<(f64, f64, f64)> {
        if self.ui_ms.is_empty() {
            return None;
        }
        let mut v: Vec<f64> = self.ui_ms.iter().copied().collect();
        v.sort_by(f64::total_cmp);
        let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
        Some((q(0.5), q(0.95), *v.last().unwrap()))
    }
}

pub struct State {
    pub opts: Options,
    t0: Instant,
    pub startup: Startup,
    pub render_state: RenderState,
    pub gpu: Option<Gpu>,
    gpu_rx: Option<std::sync::mpsc::Receiver<(Gpu, f64)>>,
    pub opening: Option<Opening>,
    pub session: Option<Session>,
    pub error: Option<String>,
    pub display: Display,
    /// the active layer's classes, if categorical
    pub classes: crate::classes::LayerClasses,
    pub map: MapView,
    pub hover: Option<PixelAt>,
    pub pins: Vec<Pin>,
    pub perf: Perf,
    pub(crate) frame: u64,
    last_play: Option<Instant>,
    screenshot_asked: bool,
    /// screenshots asked and not yet saved
    shots_pending: usize,
    pub(crate) tour: crate::tour::Tour,
    pub(crate) golden: crate::golden::Golden,
    pub basemap: crate::basemap::BasemapState,
    /// three bands of each date as RGB (series with several bands per date)
    pub composite: Option<crate::composite::BandComposite>,
    pub(crate) composite_test: crate::composite::SelfTest,
}

/// A screenshot of a window, written as a PNG.
fn save_png(img: &egui::ColorImage, path: &std::path::Path) {
    let [w, h] = img.size;
    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
    if let Err(e) = image::save_buffer(path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8) {
        eprintln!("{}: {e}", path.display());
    }
}

/// The screenshot of window `id`, if it arrived this frame.
fn screenshot_event(ui: &Ui, id: ViewportId) -> Option<std::sync::Arc<egui::ColorImage>> {
    ui.input(|i| {
        i.raw.events.iter().find_map(|e| match e {
            egui::Event::Screenshot { image, viewport_id, .. } if *viewport_id == id => Some(image.clone()),
            _ => None,
        })
    })
}

/// Panels dragged out of the main window: a window of their own each, with its own dock.
struct Detached {
    id: ViewportId,
    dock: DockState<Tab>,
    at: Option<egui::Pos2>,
}

pub struct JanusApp {
    dock: DockState<Tab>,
    windows: Vec<Detached>,
    window_count: u64,
    state: State,
}

struct Tabs<'a> {
    s: &'a mut State,
}

impl TabViewer for Tabs<'_> {
    type Tab = Tab;

    fn title(&mut self, tab: &mut Tab) -> WidgetText {
        tab.title().into()
    }

    fn id(&mut self, tab: &mut Tab) -> Id {
        Id::new(*tab)
    }

    fn is_closeable(&self, _tab: &Tab) -> bool {
        false
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Map => self.s.map_ui(ui),
            Tab::TimeSeries => self.s.series_ui(ui),
            _ => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match tab {
                    Tab::Layers => {
                        self.s.layers_ui(ui);
                        self.s.basemap_ui(ui);
                    }
                    Tab::Display => {
                        self.s.display_ui(ui);
                        self.s.composite_ui(ui);
                    }
                    Tab::Statistics => self.s.statistics_ui(ui),
                    Tab::Performance => {
                        self.s.performance_ui(ui);
                        self.s.basemap_perf_ui(ui);
                    }
                    _ => {}
                });
            }
        }
    }
}

impl JanusApp {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: Options, t0: Instant, gdal_ms: f64) -> Self {
        let window_ms = t0.elapsed().as_secs_f64() * 1000.0;
        theme::apply(&cc.egui_ctx);
        let render_state = cc.wgpu_render_state.clone().expect("Janus draws with wgpu");
        let opening = (!opts.inputs.is_empty()).then(|| Opening::start(opts.inputs.clone(), &cc.egui_ctx));
        let detach = opts.detach_map;
        let mut app = JanusApp {
            dock: default_layout(),
            windows: Vec::new(),
            window_count: 0,
            state: State {
                opts,
                t0,
                startup: Startup { gdal_ms, window_ms, ..Default::default() },
                render_state,
                gpu: None,
                gpu_rx: None,
                opening,
                session: None,
                error: None,
                display: Display::default(),
                classes: Default::default(),
                map: MapView::default(),
                hover: None,
                pins: Vec::new(),
                perf: Perf::default(),
                frame: 0,
                last_play: None,
                screenshot_asked: false,
                shots_pending: 0,
                tour: Default::default(),
                golden: Default::default(),
                basemap: Default::default(),
                composite: None,
                composite_test: Default::default(),
            },
        };
        if detach {
            app.detach_map();
        }
        app
    }

    /// The map in a window of its own from the start (--detach-map).
    fn detach_map(&mut self) {
        if let Some(path) = self.dock.find_tab(&Tab::Map) {
            self.dock.remove_tab(path);
            self.window_count += 1;
            let id = ViewportId::from_hash_of(("janus window", self.window_count));
            self.windows.push(Detached { id, dock: DockState::new(vec![Tab::Map]), at: None });
        }
    }
}

impl State {
    pub fn open(&mut self, inputs: Vec<String>, ctx: &egui::Context) {
        self.session = None;
        self.error = None;
        self.hover = None;
        self.pins.clear();
        self.display = Display::default();
        self.classes = Default::default();
        self.map.fit();
        self.opening = Some(Opening::start(inputs, ctx));
    }

    pub(crate) fn set_date(&mut self, t: usize) {
        if let Some(s) = &self.session {
            self.display.t = t.min(s.info.t() - 1);
            s.s.overview.set_focus(self.display.t);
        }
    }

    /// The work of a frame before the panels are drawn: the series opened, the dates sent to the GPU, the stretch.
    fn pump(&mut self, ctx: &egui::Context) {
        if self.gpu.is_none() && self.gpu_rx.is_none() {
            // the map's pipelines in a thread of their own: nothing new on the first frame's path (IDEIAS.md,
            // "Abertura rápida"), and no frame waits for their shaders to compile
            let (tx, rx) = std::sync::mpsc::channel();
            let (device, queue, c) = (self.render_state.device.clone(), self.render_state.queue.clone(), ctx.clone());
            std::thread::Builder::new()
                .name("map pipelines".into())
                .spawn(move || {
                    let start = Instant::now();
                    let gpu = Gpu::new(&device, &queue);
                    let _ = tx.send((gpu, start.elapsed().as_secs_f64() * 1000.0));
                    c.request_repaint();
                })
                .expect("thread");
            self.gpu_rx = Some(rx);
        }
        if let Some(rx) = &self.gpu_rx
            && let Ok((gpu, ms)) = rx.try_recv()
        {
            self.gpu = Some(gpu);
            self.startup.gpu_ms = Some(ms);
        }
        if let Some(o) = &self.opening
            && let Some(r) = o.poll()
        {
            let o = self.opening.take().unwrap();
            match r {
                Ok(info) => {
                    let max_tex = self.render_state.device.limits().max_texture_dimension_2d as usize;
                    let settings = janus_session::SessionSettings {
                        overview_budget_bytes: self.opts.budget_bytes.unwrap_or(self.opts.budget_mb << 20),
                        max_texture: max_tex.min(16384),
                        ..Default::default()
                    };
                    let s = Session::new(info, &o, &settings, ctx);
                    self.display.t = 0;
                    self.display.rgb = [0, s.info.t() / 2, s.info.t() - 1];
                    self.session = Some(s);
                    self.map.fit();
                    // each series opens without the basemap: drawn only when asked for
                    self.basemap.ui.on = false;
                    if let Some(b) = &self.opts.basemap {
                        if b.contains("://") {
                            self.basemap.ui.source = "custom".into();
                            self.basemap.ui.url = b.clone();
                        } else {
                            self.basemap.ui.source = b.clone();
                        }
                        self.basemap.ui.on = true;
                    }
                }
                Err(e) => self.error = Some(e),
            }
        }
        let (Some(s), Some(gpu)) = (&mut self.session, &mut self.gpu) else { return };
        if s.pump(gpu, ctx) {
            self.map.dirty = true;
        }
        let (changed, became_on) = self.classes.update(s, gpu);
        if changed {
            self.map.dirty = true;
        }
        if became_on {
            self.display.mode = janus_gpu::Mode::Value;
        }
        if self.display.update(s) {
            self.map.dirty = true;
        }
        // playing: four dates a second
        if self.display.playing {
            let now = Instant::now();
            if self.last_play.is_none_or(|l| now.duration_since(l).as_secs_f64() >= 0.25) {
                self.last_play = Some(now);
                self.display.t = (self.display.t + 1) % s.info.t();
                s.s.overview.set_focus(self.display.t);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let n = self.session.as_ref().map_or(0, |s| s.info.t());
        let (left, right, home, end, fit, play, satellite) = ctx.input(|i| {
            use egui::Key;
            (
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::F),
                i.key_pressed(Key::P),
                i.key_pressed(Key::S),
            )
        });
        if satellite {
            self.toggle_satellite();
        }
        if n == 0 {
            return;
        }
        let t = self.display.t;
        if left && t > 0 {
            self.set_date(t - 1);
        }
        if right && t + 1 < n {
            self.set_date(t + 1);
        }
        if home {
            self.set_date(0);
        }
        if end {
            self.set_date(n - 1);
        }
        if fit {
            self.map.fit();
        }
        if play {
            self.display.playing = !self.display.playing;
        }
    }

    fn menu(&mut self, ui: &mut Ui) -> bool {
        let mut reset_layout = false;
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open folder...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.open(vec![dir.to_string_lossy().into_owned()], ui.ctx());
                }
                if ui.button("Open files...").clicked()
                    && let Some(files) =
                        rfd::FileDialog::new().add_filter("rasters", &["tif", "tiff", "vrt", "img", "jp2"]).pick_files()
                {
                    self.open(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), ui.ctx());
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                if ui.button("Fit the map (F)").clicked() {
                    self.map.fit();
                }
                if ui.button("Reset layout").clicked() {
                    reset_layout = true;
                }
            });
            ui.menu_button("Help", |ui| {
                ui.label(format!("Janus {} in Rust (phase 1 of RUST_MIGRATION.md)", env!("CARGO_PKG_VERSION")));
                ui.label("Wheel: zoom   drag: move   click: pin   right click: remove a pin");
                ui.label("Left / Right: date   P: play   F: fit");
            });
        });
        reset_layout
    }

    /// --measure-startup and --screenshot (of the main window and of the `detached` ones).
    fn measurements(&mut self, ui: &Ui, detached: &[ViewportId]) {
        let ctx = ui.ctx();
        if self.frame == 1 && self.startup.first_frame_ms.is_none() {
            // the second frame starts once the first one is on the screen
            let ms = self.t0.elapsed().as_secs_f64() * 1000.0;
            self.startup.first_frame_ms = Some(ms);
            if self.opts.measure_startup {
                let s = &self.startup;
                println!(
                    "startup (ms since process start): gdal {:.0} | window+wgpu {:.0} | first frame {:.0}",
                    s.gdal_ms, s.window_ms, ms
                );
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        if let Some(path) = self.opts.screenshot.clone().filter(|_| !self.opts.profile_tour) {
            // the series ready, and the basemap's tiles if asked for (none loading, the map placed)
            let basemap_ready = self.opts.basemap.is_none()
                || !self.basemap.why.is_empty()
                || self.basemap.b.as_ref().is_some_and(|b| b.has_map() && b.loads() > 0 && b.inflight() == 0);
            let ready = self.session.as_ref().is_some_and(|s| s.ready_ms.is_some()) && basemap_ready;
            if ready && !self.screenshot_asked && self.frame > 30 {
                self.screenshot_asked = true;
                self.shots_pending += 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                // eframe takes no screenshots of immediate viewports (its windows of their own): the map panel's
                // target, read back from the GPU, shows what the detached map drew
                if !detached.is_empty()
                    && let Some((w, h, rgba)) = self.gpu.as_ref().and_then(|g| g.read_target(0))
                    && let Err(e) = image::save_buffer(
                        path.with_extension("detached.png"),
                        &rgba,
                        w,
                        h,
                        image::ExtendedColorType::Rgba8,
                    )
                {
                    eprintln!("{e}");
                }
            }
            if let Some(img) = screenshot_event(ui, ViewportId::ROOT) {
                save_png(&img, &path);
                self.shots_pending -= 1;
                if let Some(s) = &self.session {
                    println!(
                        "open {:.0} ms, overview read {:.0} ms, ready in {:.0} ms (statistics {:.1} ms, GPU {:.1}); {}",
                        s.open_ms,
                        s.s.overview.build_seconds().unwrap_or(0.0) * 1000.0,
                        s.ready_ms.unwrap_or(0.0),
                        s.stats.as_ref().map_or(0.0, |st| st.ms),
                        s.stats.as_ref().map_or(0.0, |st| st.gpu_ms),
                        self.perf.summary().map_or(String::new(), |(a, b, c)| format!(
                            "frame CPU p50 {a:.2} p95 {b:.2} max {c:.2} ms"
                        ))
                    );
                }
            }
            if self.screenshot_asked && self.shots_pending == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            if self.screenshot_asked {
                ctx.request_repaint();
            }
        }
    }
}

/// The tabs dragged out of the dock into floating windows (egui_dock's): taken out, to become windows of their own.
fn floating_tabs(dock: &mut DockState<Tab>) -> Vec<Vec<Tab>> {
    let surfaces: Vec<SurfaceIndex> =
        dock.iter_surfaces_indexed().filter(|(i, s)| !i.is_main() && !s.is_empty()).map(|(i, _)| i).collect();
    let mut out = Vec::new();
    for i in surfaces.into_iter().rev() {
        let tabs: Vec<Tab> = dock[i].tabs().copied().collect();
        dock.remove_surface(i);
        if !tabs.is_empty() {
            out.push(tabs);
        }
    }
    out
}

impl eframe::App for JanusApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let start = Instant::now();
        let ctx = ui.ctx().clone();
        let s = &mut self.state;
        s.perf.last_cpu = frame.info().cpu_usage;
        let detached: Vec<ViewportId> = self.windows.iter().map(|w| w.id).collect();
        s.measurements(ui, &detached);
        s.pump(&ctx);
        s.pump_basemap(&ctx);
        s.pump_composite(&ctx);
        let rc = if s.opts.dump_golden.is_some() {
            s.golden_step(&ctx)
        } else if s.opts.selftest_composite {
            s.composite_selftest_step(&ctx)
        } else if s.opts.profile_tour {
            s.tour_step(&ctx)
        } else {
            None
        };
        if let Some(rc) = rc {
            EXIT_CODE.store(rc, std::sync::atomic::Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        s.shortcuts(&ctx);
        // files dropped on the window open as the series
        let dropped: Vec<String> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().into_owned()).collect());
        if !dropped.is_empty() {
            s.open(dropped, &ctx);
        }

        let mut reset = false;
        egui::Panel::top("menu").show(ui, |ui| reset = self.state.menu(ui));
        if reset {
            self.dock = default_layout();
            for w in self.windows.drain(..) {
                drop(w);
            }
        }
        let style = theme::dock_style(ui.style());
        egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .id(Id::new("main dock"))
                .style(style.clone())
                .show_leaf_collapse_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_inside(ui, &mut Tabs { s: &mut self.state });
        });

        // tabs dragged out of the window: windows of their own, to take to another screen
        let floating = floating_tabs(&mut self.dock);
        let origin = ctx.input(|i| i.viewport().outer_rect.map(|r| r.min)).unwrap_or_default();
        let pointer = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
        for group in floating {
            self.window_count += 1;
            let id = ViewportId::from_hash_of(("janus window", self.window_count));
            let mut dock = DockState::new(vec![group[0]]);
            for &t in &group[1..] {
                dock.push_to_first_leaf(t);
            }
            self.windows.push(Detached {
                id,
                dock,
                at: Some(origin + (pointer - egui::Pos2::ZERO) - egui::vec2(40.0, 12.0)),
            });
        }
        let mut back = Vec::new();
        let state = &mut self.state;
        self.windows.retain_mut(|w| {
            let title = w.dock.iter_all_tabs().map(|(_, t)| t.title()).collect::<Vec<_>>().join(", ");
            let mut builder =
                ViewportBuilder::default().with_title(format!("Janus - {title}")).with_inner_size([900.0, 640.0]);
            if let Some(at) = w.at.take() {
                builder = builder.with_position(at);
            }
            let style = style.clone();
            let closed = ctx.show_viewport_immediate(w.id, builder, |ui, _class| {
                egui::CentralPanel::default().frame(egui::Frame::new()).show(ui, |ui| {
                    DockArea::new(&mut w.dock)
                        .id(Id::new(("window dock", w.id)))
                        .style(style.clone())
                        .show_leaf_collapse_buttons(false)
                        .show_leaf_close_all_buttons(false)
                        .show_inside(ui, &mut Tabs { s: state });
                });
                ui.input(|i| i.viewport().close_requested())
            });
            // a window closed (or emptied): its panels go back to the main window
            let tabs: Vec<Tab> = w.dock.iter_all_tabs().map(|(_, t)| *t).collect();
            if closed || tabs.is_empty() {
                back.extend(tabs);
                return false;
            }
            true
        });
        for t in back {
            self.dock.push_to_first_leaf(t);
        }

        let s = &mut self.state;
        s.perf.push(start.elapsed().as_secs_f64() * 1000.0);
        s.frame += 1;
        if s.frame <= 2 || s.opts.screenshot.is_some() && !s.screenshot_asked {
            ctx.request_repaint();
        }
    }
}
