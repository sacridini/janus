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
use crate::map::MapView;
use crate::session::{Opening, Session};
use crate::theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Tab {
    Map,
    /// a map panel ("Map <id>")
    MapView(u32),
    Layers,
    Files,
    Display,
    TimeSeries,
    Statistics,
    Performance,
    /// while there is a transect, next to the time series
    Transect,
    /// while interpreting a vector layer, next to the statistics
    Interpretation,
    /// View > Analysis, next to the time series
    Analysis,
    /// with a layer of embeddings (or a spectrum's analysis), next to Display
    Embeddings,
    /// Validation > Validation panel, next to the statistics
    Validation,
}

impl Tab {
    fn title(self) -> String {
        match self {
            Tab::MapView(id) => format!("Map {id}"),
            _ => self.name().to_string(),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Tab::Map | Tab::MapView(_) => "Map",
            Tab::Layers => "Layers",
            Tab::Files => "Files",
            Tab::Display => "Display",
            Tab::TimeSeries => "Time series",
            Tab::Statistics => "Statistics",
            Tab::Performance => "Performance",
            Tab::Transect => "Transect",
            Tab::Interpretation => "Interpretation",
            Tab::Analysis => "Analysis",
            Tab::Embeddings => "Embeddings",
            Tab::Validation => "Validation",
        }
    }
}

/// The C++'s default layout (app.cpp, dockDefaultLayout): Layers and Files above Display on the left (0.21; the
/// Performance panel, off by default, opens as a tab next to Display), the map in the centre, the time series and
/// the statistics below it (0.36).
fn default_layout() -> DockState<Tab> {
    let mut d = DockState::new(vec![Tab::Map]);
    let s = d.main_surface_mut();
    let [map, left] = s.split_left(NodeIndex::root(), 0.21, vec![Tab::Layers, Tab::Files]);
    let [_, _] = s.split_below(left, 0.42, vec![Tab::Display]);
    let [_, bottom] = s.split_below(map, 0.64, vec![Tab::TimeSeries]);
    let [_, _] = s.split_right(bottom, 0.66, vec![Tab::Statistics]);
    d
}

/// The layout of the panels kept between sessions (the C++ keeps it in ImGui's ini): the main window's dock,
/// without the map panels (a session starts without them, as in the C++).
fn layout_path() -> std::path::PathBuf {
    crate::settings::app_data_dir().join("janus-rs-layout.json")
}

fn load_layout() -> Option<DockState<Tab>> {
    let text = std::fs::read_to_string(layout_path()).ok()?;
    let d: DockState<Tab> = serde_json::from_str(&text).ok()?;
    // every panel the program has, once (a layout from another version may lack one)
    let all = [Tab::Map, Tab::Layers, Tab::Files, Tab::Display, Tab::TimeSeries, Tab::Statistics];
    all.iter().all(|t| d.find_tab(t).is_some()).then_some(d)
}

/// The dock as kept: the map panels left out.
fn layout_text(dock: &DockState<Tab>) -> Option<String> {
    let mut d = dock.clone();
    while let Some(p) = d.find_tab_from(|t| {
        matches!(
            t,
            Tab::MapView(_) | Tab::Transect | Tab::Interpretation | Tab::Analysis | Tab::Embeddings | Tab::Validation
        )
    }) {
        d.remove_tab(p);
    }
    serde_json::to_string(&d).ok()
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
    /// --season: days of the year (start, end) of a season set once the series opens
    pub season: Option<(i32, i32)>,
    /// --layer: series added as layers once the first is open
    pub extra_layers: Vec<String>,
    /// --map-view, --swipe: a map panel, the swipe, once every series is open (screenshots)
    pub map_view: bool,
    /// a validation project opened: its panel on this tab (0 Design ... 4 Report)
    pub validation_tab: Option<usize>,
    pub swipe: bool,
    /// --pin, --roi: pins and an ROI once the series is open (screenshots)
    pub pins: Vec<(usize, usize)>,
    pub roi: Option<[i64; 4]>,
    /// --roi with more than two corners: a polygon's vertices (x, y, ...)
    pub roi_polygon: Vec<f64>,
    /// --transect: its vertices (x, y, ...)
    pub transect: Vec<f64>,
    /// --analysis: the Analysis panel at this tab
    pub analysis: Option<usize>,
    /// --zeit TOOL: that Zeit tool fitted on the chart, its window open
    pub zeit_tool: Option<String>,
    /// --zeit-run: the --zeit tool run on the raster (its ROI if --roi, else the whole image) once the series is
    /// loaded
    pub zeit_run: bool,
    /// --selftest-reproject B F: F reprojected on B's grid
    pub selftest_reproject: bool,
    /// --selftest-multidim H/clim.nc: a NetCDF of two variables, its levels, opened in place
    pub selftest_multidim: bool,
    /// --selftest-ui A B [C D E]: the layers workflow on the test series
    pub selftest_ui: bool,
    /// --measure-vectors IN VEC...: the vector files measured over the series
    pub measure_vectors: bool,
    pub measure_vector_files: Vec<String>,
    /// --selftest-embeddings-ui DIR [OUT]
    pub selftest_embeddings_ui: bool,
    pub selftest_embeddings_out: Option<PathBuf>,
}

impl Options {
    /// A tour or a self-test drives the map: the pointer over the window is not the cursor.
    pub fn scripted(&self) -> bool {
        self.profile_tour
            || self.measure_vectors
            || self.selftest_embeddings_ui
            || self.dump_golden.is_some()
            || self.selftest_composite
            || self.selftest_reproject
            || self.selftest_ui
            || self.selftest_multidim
    }
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
    /// the active layer's session (its entry in `layers` holds None)
    pub session: Option<Session>,
    /// every open series, bottom to top; the active one's state is in the members here
    pub layers: Vec<crate::layers::SeriesLayer>,
    pub active: usize,
    pub error: Option<String>,
    pub display: Display,
    /// the active layer's classes, if categorical
    pub classes: crate::classes::LayerClasses,
    pub map: MapView,
    /// the cursor's series, the pins, the ROI
    pub series: crate::series::SeriesState,
    /// the space-time transect and the one being drawn
    pub transect: crate::transect::Transect,
    pub transect_draft: crate::transect::TransectDraft,
    /// vector layers (drawn over every raster layer), the files being read, the feature under the cursor
    pub vectors: Vec<crate::vectors::VectorLayer>,
    pub vector_loads: crate::vectors::VectorLoads,
    pub next_vector_id: u32,
    pub hover_vector: Option<(usize, usize)>,
    pub extract: crate::vectors::ExtractUi,
    /// the interpretation of a vector layer's features
    pub labels: crate::labels::Interpretation,
    /// files written in the background (Exports window)
    pub exports: Vec<std::sync::Arc<crate::exports::ExportJob>>,
    pub show_exports: bool,
    pub(crate) measure_vectors: crate::measure_vectors::MeasureVectors,
    /// the Analysis panel's state
    pub analysis: crate::analysis::AnalysisUi,
    /// the Embeddings panel is shown
    pub show_embeddings: bool,
    pub(crate) embeddings_test: crate::embeddings_test::EmbeddingsTest,
    /// the validation: its design, its project, the Validation panel and the Chips window
    pub val: crate::validation::ValidationUi,
    /// the embeddings download's choices, and the downloads under way
    pub emb_download: crate::emb_download::DownloadUi,
    pub emb_downloads: Vec<crate::emb_download::EmbDownload>,
    pub chart: crate::chart::ChartOptions,
    /// File > Open URL, the variable picker, the Display panel's variables
    pub open_ui: crate::open::OpenUi,
    /// the Files panel
    pub files: crate::files::FileBrowser,
    /// a message in a popup (an open that failed)
    pub error_popup: Option<String>,
    pub help_popup: bool,
    /// Settings, kept between sessions
    pub settings: crate::settings::SettingsState,
    /// the map targets' egui textures (by slot)
    pub map_textures: std::collections::HashMap<i32, egui::TextureId>,
    /// the map panels, the next one's number, those to dock (and to close) this frame
    pub views: Vec<crate::views::ViewSetup>,
    pub next_view: u32,
    pub views_added: Vec<u32>,
    pub views_closed: Vec<u32>,
    /// the swipe (C), and its comparison when it was last turned off
    pub swipe: Option<crate::views::Swipe>,
    pub last_swipe: Option<crate::views::ViewSetup>,
    /// the Display panel's spectral index (among those the spectrum covers)
    pub spectral_index: usize,
    /// the Display panel's band selection being edited (for the cube id)
    pub band_sel: Option<(u64, janus_cube::BandSelection)>,
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
    pub(crate) reproject_test: crate::layers::ReprojectTest,
    /// Zeit's tools: their windows, the fits on the chart
    pub zeit: crate::zeit::ZeitUi,
    pub(crate) ui_test: crate::ui_test::UiTest,
    pub(crate) multidim_test: crate::open::MultidimTest,
}

/// What the menu or a key asks of the window's layout.
#[derive(Default)]
struct MenuAction {
    reset_layout: bool,
    toggle_performance: bool,
}

/// Help > Shortcuts and usage (app.cpp, uiPopups), for what the Rust version has so far.
const HELP: &str = "Open: Ctrl+O (files), Ctrl+Shift+O (folder), Ctrl+U (URL: COGs on the web, S3, GCS)
      or drop onto the window (with Shift: as a layer); NetCDF / Zarr: a variable is
      picked if there are several
Command line: jn <folder | file.tif | pattern_*.tif | https://.../cog.tif ...>
Add a layer: Ctrl+L (files), File > Add layer
Settings: Ctrl+, (theme, font size, processing threads, memory and caches)
Reset layout: Ctrl+Shift+R

Map
  drag ................ pan
  mouse wheel ......... zoom
  hover ............... pixel series (exact once the mouse rests)
  click ............... drop a pin (compare pixels)
  right click on pin .. remove it (Delete removes the last one)
  Shift + drag ........ rectangular ROI (mean and p10-p90 per date; on every
                        visible layer with 'All visible layers' on the chart)
  S ................... satellite basemap on / off (Esri World Imagery, or the
                        imagery source picked in the Layers panel)
  space ............... show / hide the active layer
  F ................... fit to window

Time
  left/right arrows ... previous / next date (in the season)
  Home / End .......... first / last date
  P ................... play / pause
  click on the chart .. go to that date";

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
    /// the layout as kept, and when it was last compared with the dock
    layout_saved: String,
    layout_checked: Instant,
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

    fn is_closeable(&self, tab: &Tab) -> bool {
        matches!(
            tab,
            Tab::MapView(_) | Tab::Transect | Tab::Interpretation | Tab::Analysis | Tab::Embeddings | Tab::Validation
        )
    }

    fn on_close(&mut self, tab: &mut Tab) -> egui_dock::tab_viewer::OnCloseResponse {
        if let Tab::MapView(id) = tab {
            self.s.close_map_view(*id);
        }
        if *tab == Tab::Transect {
            self.s.clear_transect();
        }
        if *tab == Tab::Interpretation {
            self.s.labels.show = false;
        }
        if *tab == Tab::Analysis {
            self.s.analysis.show = false;
        }
        if *tab == Tab::Embeddings {
            self.s.show_embeddings = false;
        }
        if *tab == Tab::Validation {
            self.s.val.show = false;
        }
        egui_dock::tab_viewer::OnCloseResponse::Close
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Map => self.s.map_ui(ui),
            Tab::MapView(id) => self.s.map_view_ui(ui, *id),
            Tab::TimeSeries => self.s.series_ui(ui),
            Tab::Transect => self.s.transect_ui(ui),
            Tab::Analysis => self.s.analysis_ui(ui),
            Tab::Embeddings => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.s.embeddings_ui(ui));
            }
            Tab::Interpretation => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.s.interpretation_ui(ui));
            }
            Tab::Validation => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| self.s.validation_ui(ui));
            }
            _ => {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match tab {
                    Tab::Layers => {
                        self.s.layers_ui(ui);
                        // embeddings of the visible area, added on top
                        egui::CollapsingHeader::new("Embeddings")
                            .id_salt("emb dl layers")
                            .show(ui, |ui| self.s.embedding_download_ui(ui))
                            .header_response
                            .on_hover_text(
                                "Foundation-model embeddings (AlphaEarth, TESSERA) of the visible area,\n\
                                 downloaded and added as a layer",
                            );
                        self.s.basemap_ui(ui);
                    }
                    Tab::Display => self.s.display_ui(ui),
                    Tab::Files => self.s.files_ui(ui),
                    Tab::Statistics => self.s.statistics_ui(ui),
                    Tab::Performance => {
                        self.s.performance_ui(ui);
                        self.s.cache_perf_ui(ui);
                        self.s.basemap_perf_ui(ui);
                        self.s.performance_end_ui(ui);
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
        // the settings kept (the C++'s the first time): the theme and the font before the first frame
        let (settings, bm) = crate::settings::load();
        theme::load_fonts(&cc.egui_ctx);
        theme::apply(&cc.egui_ctx, settings.theme, settings.panel_opacity, settings.font_size);
        let no_save = opts.scripted() || opts.screenshot.is_some() || opts.measure_startup;
        let favorites = settings.favorites.clone();
        let render_state = cc.wgpu_render_state.clone().expect("Janus draws with wgpu");
        // vector files (and validation projects) among the inputs become layers once the app is made
        let (vector_inputs, rasters): (Vec<String>, Vec<String>) = opts
            .inputs
            .iter()
            .cloned()
            .partition(|p| janus_geo::vectors::is_vector_path(p) && !std::path::Path::new(p).is_dir());
        let opening = (!rasters.is_empty()).then(|| Opening::start(rasters, false, &cc.egui_ctx));
        let detach = opts.detach_map;
        let scripted = opts.scripted() || opts.screenshot.is_some() || opts.measure_startup;
        let mut app = JanusApp {
            // the tests and screenshots in the C++'s default layout
            dock: if scripted { default_layout() } else { load_layout().unwrap_or_else(default_layout) },
            layout_saved: String::new(),
            layout_checked: Instant::now(),
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
                layers: Vec::new(),
                active: 0,
                error: None,
                display: Display::default(),
                classes: Default::default(),
                map: MapView::default(),
                series: Default::default(),
                transect: Default::default(),
                transect_draft: Default::default(),
                vectors: Vec::new(),
                vector_loads: Default::default(),
                next_vector_id: 1,
                hover_vector: None,
                extract: Default::default(),
                labels: Default::default(),
                exports: Vec::new(),
                show_exports: false,
                measure_vectors: Default::default(),
                analysis: Default::default(),
                show_embeddings: false,
                embeddings_test: Default::default(),
                emb_download: Default::default(),
                emb_downloads: Vec::new(),
                val: Default::default(),
                chart: Default::default(),
                band_sel: None,
                spectral_index: 0,
                map_textures: Default::default(),
                views: Vec::new(),
                next_view: 2,
                views_added: Vec::new(),
                views_closed: Vec::new(),
                swipe: None,
                last_swipe: None,
                open_ui: Default::default(),
                files: crate::files::FileBrowser::new(favorites),
                error_popup: None,
                help_popup: false,
                settings: crate::settings::SettingsState::new(settings, no_save),
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
                reproject_test: Default::default(),
                zeit: Default::default(),
                ui_test: Default::default(),
                multidim_test: Default::default(),
            },
        };
        if !vector_inputs.is_empty() {
            app.state.add_vector_files(vector_inputs, &cc.egui_ctx);
        }
        // the basemap's setting (not whether it is shown: each session starts without it)
        let ui = &mut app.state.basemap.ui;
        ui.source = bm.source.unwrap_or(ui.source.clone());
        ui.opacity = bm.opacity.unwrap_or(ui.opacity);
        ui.url = bm.url.unwrap_or_default();
        ui.max_zoom = bm.max_zoom.unwrap_or(ui.max_zoom);
        ui.tile_size = bm.tile_size.unwrap_or(ui.tile_size);
        ui.attribution = bm.attribution.unwrap_or_default();
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
    /// Opens a series in the background; `add` keeps the open ones and adds it as a new layer. One open at a time.
    pub fn open_inputs(&mut self, inputs: Vec<String>, add: bool, ctx: &egui::Context) {
        // vector files become vector layers
        let inputs = self.take_vector_inputs(inputs, ctx);
        if inputs.is_empty() || self.opening.is_some() {
            return;
        }
        self.error = None;
        self.opening = Some(Opening::start(inputs, add && !self.layers.is_empty(), ctx));
    }

    /// Layer i's files again with another band selection, in place (finish_open replaces its session).
    pub fn reopen_layer(&mut self, i: usize, sel: janus_cube::BandSelection, ctx: &egui::Context) {
        let Some(inputs) = self.layers.get(i).map(|l| l.inputs.clone()) else { return };
        self.reopen_layer_inputs(i, inputs, sel, ctx);
    }

    /// Layer i opened again from `inputs` (another variable of its file) with `sel`, in place.
    pub fn reopen_layer_inputs(
        &mut self,
        i: usize,
        inputs: Vec<String>,
        sel: janus_cube::BandSelection,
        ctx: &egui::Context,
    ) {
        if self.opening.is_some() || i >= self.layers.len() {
            return;
        }
        let mut o = Opening::start_with(inputs, true, sel, ctx);
        o.replace = Some(i);
        self.opening = Some(o);
    }

    /// The settings of a series opened now: the overview's memory (Settings, or --budget for this run), the
    /// processing threads, the full-resolution cache, the disk cache budget.
    pub(crate) fn session_settings(&self) -> janus_session::SessionSettings {
        let max_tex = self.render_state.device.limits().max_texture_dimension_2d as usize;
        let s = &self.settings.s;
        let mb = if self.opts.budget_mb > 0 { self.opts.budget_mb } else { i64::from(s.overview_budget_mb) };
        janus_session::SessionSettings {
            overview_budget_bytes: self.opts.budget_bytes.unwrap_or(mb << 20),
            max_texture: max_tex.min(16384),
            threads: s.threads,
            full_res: s.full_res,
            cache_budget: s.cache_budget_gb << 30,
            ..Default::default()
        }
    }

    /// The series opened: a new layer, made active (the others closed unless it is added).
    fn finish_open(&mut self, info: janus_cube::CubeInfo, o: Opening, ctx: &egui::Context) {
        if let Some(i) = o.replace.filter(|&i| i < self.layers.len()) {
            if o.inputs != self.layers[i].inputs {
                // another variable of the same file: named after it
                self.layers[i].name.clear();
                let name = self.layer_name(&info, &o.inputs);
                let l = &mut self.layers[i];
                l.inputs = o.inputs.clone();
                l.name = name;
            }
            self.replace_layer_session(i, info, &o, ctx);
            return;
        }
        if !o.add {
            self.close_all(ctx);
            // a new series starts without the basemap until ticked
            self.basemap.ui.on = false;
        }
        let s = Session::new(info, &o, &self.session_settings(), ctx);
        let n = s.info.t();
        let mut disp = Display { rgb: [0, n / 2, n - 1], ..Default::default() };
        // a spectrum opens in true colour (if it covers the visible), at the near infrared
        if s.info.spectral {
            if let Some(rgb) = janus_cube::spectral::composite_layers(&s.info, &janus_cube::spectral::COMPOSITES[0]) {
                disp.rgb = rgb;
                disp.mode = janus_gpu::Mode::Rgb;
            }
            disp.t = s.info.nearest_layer(860.0);
        }
        // a new layer starts at the date (or wavelength) nearest to the one shown
        if let Some(a) = &self.session
            && crate::layers::matchable_axis(&a.info, &s.info)
        {
            disp.t = s.info.nearest_layer(a.info.layers[self.display.t].time);
        }
        let name = o.name.clone().unwrap_or_else(|| self.layer_name(&s.info, &o.inputs));
        let emb = crate::embeddings::EmbeddingLayer::new(&s.info);
        self.layers.push(crate::layers::SeriesLayer {
            session: Some(s),
            inputs: o.inputs,
            name,
            visible: true,
            opacity: 1.0,
            disp,
            classes: Default::default(),
            composite: None,
            place: Default::default(),
            series: Default::default(),
            emb,
        });
        // the panel comes up with the first layer of embeddings
        if self.layers.last().and_then(|l| l.emb.as_ref()).is_some_and(|e| !e.spectral) {
            self.show_embeddings = true;
        }
        // the folder of the files (of a NetCDF variable's file) in the Files panel
        if !self.layers.last().is_some_and(|l| l.session.as_ref().is_some_and(|s| s.info.remote)) {
            let l = self.layers.last().unwrap();
            let one = if l.inputs.len() == 1 {
                l.inputs[0].clone()
            } else {
                l.session.as_ref().unwrap().info.first_path.clone()
            };
            let where_ = janus_cube::dataset_file(&one);
            self.files.add_recent(where_);
            if self.settings.s.reveal_opened {
                self.files.reveal(where_);
            }
        }
        let first = self.layers.len() == 1;
        if first {
            self.map.fit();
            self.map.touched = false;
        }
        self.display.playing = false;
        self.set_active(self.layers.len() - 1, ctx);
        // a series a Zeit run wrote joins under the one studied, which stays active
        if let Some(k) = self.zeit.keep_active.take().filter(|&k| k < self.layers.len()) {
            self.set_active(k, ctx);
        }
        if first {
            // test runs: a season, a basemap
            if let Some((a, b)) = self.opts.season {
                self.display.season = crate::season::Season { on: true, start: a, end: b };
            }
            for (x, y) in self.opts.pins.clone() {
                self.add_pin(x, y);
            }
            if let Some(r) = self.opts.roi {
                self.set_roi_rect(r);
            }
            if !self.opts.roi_polygon.is_empty() {
                self.set_roi_polygon(vec![self.opts.roi_polygon.clone()]);
            }
            if let Some(tab) = self.opts.analysis {
                (self.analysis.show, self.analysis.tab) = (true, tab);
            }
            if self.opts.transect.len() >= 4 {
                let pts = self.opts.transect.as_chunks::<2>().0.to_vec();
                self.set_transect(pts);
            }
            if let Some(id) = self.opts.zeit_tool.clone() {
                self.zeit.tools.entry(id.clone()).or_default().open = true;
                self.zeit.pixel_tool = Some(id);
                self.zeit.version += 1;
            }
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
                crate::open::OpenResult::Opened(info, inputs) => {
                    let mut o = o;
                    o.inputs = inputs;
                    self.finish_open(*info, o, ctx);
                }
                // a container of several variables: which one, asked
                crate::open::OpenResult::Variables(vars) => self.ask_variable(vars, o.inputs[0].clone(), o.add),
                crate::open::OpenResult::Failed(e) => {
                    self.error = Some(e.clone());
                    self.error_popup = Some(e);
                }
            }
        }
        // --layer: the next series added once the first is open (the self-tests add theirs themselves)
        if self.opening.is_none()
            && !self.layers.is_empty()
            && !self.opts.selftest_reproject
            && !self.opts.selftest_ui
            && !self.opts.extra_layers.is_empty()
        {
            let next = self.opts.extra_layers.remove(0);
            self.open_inputs(vec![next], true, ctx);
        }
        // --map-view, --swipe: once every series asked for is open
        if (self.opts.map_view || self.opts.swipe)
            && self.opening.is_none()
            && self.opts.extra_layers.is_empty()
            && self.session.as_ref().is_some_and(|s| s.s.overview.complete())
        {
            if std::mem::take(&mut self.opts.map_view) {
                self.new_map_view();
            }
            if std::mem::take(&mut self.opts.swipe) {
                self.toggle_swipe();
            }
        }
        self.pump_season();
        self.pump_layers(ctx);
        self.sync_layer_times();
        self.pump_series();
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
        // playing: the dates per second set on the time bar
        if self.display.playing {
            let now = Instant::now();
            let every = 1.0 / f64::from(self.display.fps.max(0.1));
            if self.last_play.is_none_or(|l| now.duration_since(l).as_secs_f64() >= every) {
                self.last_play = Some(now);
                // the next date in the season (year by year for a composite of the date's year)
                let n = s.info.t() as i64;
                let t = self.display.step_date(&s.info, self.display.t, 1);
                self.display.t = ((t % n + n) % n) as usize;
                s.s.overview.set_focus(self.display.t);
            }
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(every));
        }
    }

    /// The keys of the whole window (app.cpp, handleShortcuts); the map's own (left, right, P, space, F, S) only
    /// when no text field has them.
    fn shortcuts(&mut self, ctx: &egui::Context) -> MenuAction {
        use egui::{Key, Modifiers};
        let mut act = MenuAction::default();
        let chord = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
        let shift_cmd = Modifiers::COMMAND | Modifiers::SHIFT;
        if chord(shift_cmd, Key::O) {
            self.open_folder_dialog(false, ctx);
        } else if chord(Modifiers::COMMAND, Key::O) {
            self.open_files_dialog(false, ctx);
        }
        if chord(Modifiers::COMMAND, Key::U) {
            self.open_url_dialog(false);
        }
        if self.session.is_some() && chord(Modifiers::COMMAND, Key::L) {
            self.open_files_dialog(true, ctx);
        }
        if chord(Modifiers::COMMAND, Key::Comma) {
            self.settings.show = true;
        }
        if self.session.is_some() && chord(Modifiers::COMMAND, Key::T) {
            self.new_map_view();
        }
        // closes the panel under the pointer, else the last one opened (the main map stays)
        if !self.views.is_empty() && chord(Modifiers::COMMAND, Key::W) {
            let id = self.map.prev_pointer_panel.filter(|&p| p >= 2).unwrap_or(self.views.last().unwrap().id);
            self.views_closed.push(id);
        }
        if chord(shift_cmd, Key::R) {
            act.reset_layout = true;
        }
        if ctx.egui_wants_keyboard_input() || self.session.is_none() {
            return act;
        }
        self.interp_shortcuts(ctx);
        let n = self.session.as_ref().map_or(0, |s| s.info.t());
        let (left, right, home, end, fit, play, satellite, space, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::F) && i.modifiers.is_none(),
                i.key_pressed(Key::P) && i.modifiers.is_none(),
                i.key_pressed(Key::S) && i.modifiers.is_none(),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Escape),
            )
        });
        if satellite {
            self.toggle_satellite();
        }
        if left {
            self.step_date(-1);
        }
        if right {
            self.step_date(1);
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
        if ctx.input(|i| i.key_pressed(Key::C) && i.modifiers.is_none()) {
            self.toggle_swipe();
        }
        if space && let Some(l) = self.layers.get_mut(self.active) {
            // shows / hides the active layer
            l.visible = !l.visible;
            self.map.dirty = true;
        }
        if esc {
            self.series.roi_drag = None;
        }
        self.roi_shortcuts(ctx);
        self.transect_shortcuts(ctx);
        act
    }

    pub(crate) fn open_files_dialog(&mut self, add: bool, ctx: &egui::Context) {
        if let Some(files) = rfd::FileDialog::new().pick_files() {
            self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), add, ctx);
        }
    }

    pub(crate) fn open_folder_dialog(&mut self, add: bool, ctx: &egui::Context) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.open_inputs(vec![dir.to_string_lossy().into_owned()], add, ctx);
        }
    }

    /// The menu bar (app.cpp, uiMenu).
    fn menu(&mut self, ui: &mut Ui, perf_shown: bool) -> MenuAction {
        let mut act = MenuAction::default();
        let ctx = ui.ctx().clone();
        let has = self.session.is_some();
        let item = |ui: &mut Ui, label: &str, keys: &str, enabled: bool| {
            ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(keys)).clicked()
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if item(ui, "Open file(s)...", "Ctrl+O", true) {
                    self.open_files_dialog(false, &ctx);
                }
                if item(ui, "Open folder...", "Ctrl+Shift+O", true) {
                    self.open_folder_dialog(false, &ctx);
                }
                let r = ui.add(egui::Button::new("Open URL...").shortcut_text("Ctrl+U")).on_hover_text(
                    "Cloud-optimized GeoTIFFs (and other rasters) straight from the web:\nhttps://, s3://, gs://, \
                     az://. Only the blocks and overview levels\nshown are downloaded; one URL per line makes a series.",
                );
                if r.clicked() {
                    self.open_url_dialog(false);
                }
                if item(ui, "Add layer: file(s)...", "Ctrl+L", has) {
                    self.open_files_dialog(true, &ctx);
                }
                if item(ui, "Add layer: folder...", "", has) {
                    self.open_folder_dialog(true, &ctx);
                }
                if item(ui, "Add layer: URL...", "", has) {
                    self.open_url_dialog(true);
                }
                ui.separator();
                if item(ui, "Close active layer", "", has) {
                    self.remove_layer(self.active, &ctx);
                }
                if item(ui, "Close all", "", !self.layers.is_empty()) {
                    self.close_all(&ctx);
                }
                ui.separator();
                if item(ui, "Settings...", "Ctrl+,", true) {
                    self.settings.show = true;
                }
                ui.separator();
                if item(ui, "Exit", "Alt+F4", true) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                if item(ui, "Fit map to window", "F", has) {
                    self.map.fit();
                }
                if item(ui, "New map view", "Ctrl+T", has) {
                    self.new_map_view();
                }
                if item(ui, "Close map view", "Ctrl+W", !self.views.is_empty()) {
                    let id = self.views.last().unwrap().id;
                    self.views_closed.push(id);
                }
                ui.separator();
                let mut swipe = self.swipe.is_some();
                if ui.add_enabled(has, egui::Checkbox::new(&mut swipe, "Swipe (C)")).clicked() {
                    self.toggle_swipe();
                }
                let mut transect = self.transect_draft.mode;
                if ui
                    .add_enabled(has, egui::Checkbox::new(&mut transect, "Draw transect (T or Ctrl+drag)"))
                    .on_hover_text(
                        "Then drag: a straight line; or click its vertices: a polyline (double click\n\
                         or Enter ends it).",
                    )
                    .clicked()
                {
                    self.transect_draft.mode = transect;
                }
                if item(ui, "Clear transect", "", self.transect.on) {
                    self.clear_transect();
                }
                let mut poly = self.series.poly.on;
                if ui.add_enabled(has, egui::Checkbox::new(&mut poly, "Draw ROI polygon (R)")).clicked() {
                    self.toggle_polygon();
                }
                if item(ui, "Clear ROI", "", self.series.roi_rect.is_some()) {
                    self.clear_roi();
                }
                ui.separator();
                let mut analysis = self.analysis.show;
                if ui
                    .add_enabled(has, egui::Checkbox::new(&mut analysis, "Analysis"))
                    .on_hover_text("Seasonal views, class shares and transitions, scatter of two layers or dates")
                    .clicked()
                {
                    self.analysis.show = analysis;
                }
                let mut emb = self.show_embeddings;
                if ui.checkbox(&mut emb, "Embeddings").clicked() {
                    self.show_embeddings = emb;
                }
                let mut interp = self.labels.show;
                if ui.checkbox(&mut interp, "Interpretation").clicked() {
                    self.labels.show = interp;
                }
                let mut exports = self.show_exports;
                if ui.checkbox(&mut exports, "Exports").clicked() {
                    self.show_exports = exports;
                }
                let mut perf = perf_shown;
                if ui.checkbox(&mut perf, "Performance").clicked() {
                    act.toggle_performance = true;
                }
                ui.separator();
                if ui
                    .add(egui::Button::new("Reset layout").shortcut_text("Ctrl+Shift+R"))
                    .on_hover_text("Every panel back where it was at the first start")
                    .clicked()
                {
                    act.reset_layout = true;
                }
            });
            ui.menu_button("Tools", |ui| self.tools_menu(ui));
            ui.menu_button("Validation", |ui| self.validation_menu(ui));
            ui.menu_button("Help", |ui| {
                if ui.button("Shortcuts and usage").clicked() {
                    self.help_popup = true;
                }
            });
            if let Some(s) = &self.session {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(8.0);
                    ui.weak(&s.info.description);
                });
            }
        });
        act
    }

    /// The popups: an error, the shortcuts, Open URL, the variable picker.
    fn popups(&mut self, ctx: &egui::Context) {
        self.open_dialogs(ctx);
        if let Some(e) = self.error_popup.clone() {
            let m = egui::Modal::new(egui::Id::new("error")).show(ctx, |ui| {
                ui.heading("Error");
                ui.set_max_width(560.0);
                ui.add(egui::Label::new(&e).wrap());
                ui.button("OK").clicked()
            });
            if m.inner || m.should_close() {
                self.error_popup = None;
            }
        }
        if self.help_popup {
            let m = egui::Modal::new(egui::Id::new("help")).show(ctx, |ui| {
                ui.heading("Shortcuts and usage");
                ui.monospace(HELP);
                ui.button("Close").clicked()
            });
            if m.inner || m.should_close() {
                self.help_popup = false;
            }
        }
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
            // a Zeit run asked for: done, its maps loaded
            let zeit_ready = !self.opts.zeit_run || self.zeit.runs_settled();
            // layers drawn as embeddings: the images of their dates made
            let emb_ready = self.state_embeddings_settled();
            let ready =
                self.session.as_ref().is_some_and(|s| s.ready_ms.is_some()) && basemap_ready && zeit_ready && emb_ready;
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
    /// Translucent panels: nothing under them (the theme's clear colour, alpha 0).
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::clear_color()
    }

    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let start = Instant::now();
        let ctx = ui.ctx().clone();
        let s = &mut self.state;
        s.perf.last_cpu = frame.info().cpu_usage;
        let detached: Vec<ViewportId> = self.windows.iter().map(|w| w.id).collect();
        s.measurements(ui, &detached);
        s.map.prev_pointer_panel = s.map.pointer_panel.take();
        s.pump_settings(&ctx);
        s.pump(&ctx);
        s.pump_basemap(&ctx);
        s.pump_composite(&ctx);
        s.pump_zeit(&ctx);
        s.pump_transect(&ctx);
        s.pump_vectors();
        s.pump_interpretation(&ctx);
        s.pump_embeddings(&ctx);
        s.pump_embedding_downloads(&ctx);
        s.pump_validation(&ctx);
        s.pump_chips(&ctx);
        s.pump_zeit_jobs(&ctx);
        s.scripted_zeit_run(&ctx);
        let rc = if s.opts.dump_golden.is_some() {
            s.golden_step(&ctx)
        } else if s.opts.selftest_composite {
            s.composite_selftest_step(&ctx)
        } else if s.opts.selftest_reproject {
            s.reproject_selftest_step(&ctx)
        } else if s.opts.selftest_ui {
            s.ui_selftest_step(&ctx)
        } else if s.opts.selftest_multidim {
            s.multidim_selftest_step(&ctx)
        } else if s.opts.selftest_embeddings_ui {
            s.embeddings_test_step(&ctx)
        } else if s.opts.measure_vectors {
            s.measure_vectors_step(&ctx)
        } else if s.opts.profile_tour {
            s.tour_step(&ctx)
        } else {
            None
        };
        if let Some(rc) = rc {
            EXIT_CODE.store(rc, std::sync::atomic::Ordering::Relaxed);
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let keys = s.shortcuts(&ctx);
        // files dropped on the window open as the series; with Shift held, as new layers
        let (dropped, shift): (Vec<String>, bool) = ctx.input(|i| {
            let f = i.raw.dropped_files.iter().map(|f| f.path().to_string_lossy().into_owned());
            (f.collect(), i.modifiers.shift)
        });
        if !dropped.is_empty() {
            s.open_inputs(dropped, shift, &ctx);
        }

        let mut act = MenuAction::default();
        let perf_shown = self.dock.find_tab(&Tab::Performance).is_some();
        egui::Panel::top("menu").show(ui, |ui| act = self.state.menu(ui, perf_shown));
        if act.toggle_performance {
            // the Performance panel, off by default, as a tab next to Display
            match self.dock.find_tab(&Tab::Performance) {
                Some(p) => {
                    self.dock.remove_tab(p);
                }
                None => {
                    if let Some(p) = self.dock.find_tab(&Tab::Display) {
                        self.dock.set_focused_node_and_surface(p.node_path());
                    }
                    self.dock.push_to_focused_leaf(Tab::Performance);
                }
            }
        }
        if act.reset_layout || keys.reset_layout {
            self.dock = default_layout();
            for w in self.windows.drain(..) {
                drop(w);
            }
        }
        // the Embeddings panel while it is shown, as a tab next to Display
        let emb_tab = self.dock.find_tab(&Tab::Embeddings);
        if self.state.show_embeddings && emb_tab.is_none() {
            if let Some(p) = self.dock.find_tab(&Tab::Display) {
                self.dock.set_focused_node_and_surface(p.node_path());
            }
            self.dock.push_to_focused_leaf(Tab::Embeddings);
        } else if !self.state.show_embeddings
            && let Some(p) = emb_tab
        {
            self.dock.remove_tab(p);
        }
        // the Analysis panel while it is shown, as a tab next to the time series
        let analysis_tab = self.dock.find_tab(&Tab::Analysis);
        if self.state.analysis.show && analysis_tab.is_none() {
            if let Some(p) = self.dock.find_tab(&Tab::TimeSeries) {
                self.dock.set_focused_node_and_surface(p.node_path());
            }
            self.dock.push_to_focused_leaf(Tab::Analysis);
        } else if !self.state.analysis.show
            && let Some(p) = analysis_tab
        {
            self.dock.remove_tab(p);
        }
        // the Validation panel while it is shown, as a tab next to the statistics
        let val_tab = self.dock.find_tab(&Tab::Validation);
        if self.state.val.show && val_tab.is_none() {
            if let Some(p) = self.dock.find_tab(&Tab::Statistics) {
                self.dock.set_focused_node_and_surface(p.node_path());
            }
            self.dock.push_to_focused_leaf(Tab::Validation);
        } else if !self.state.val.show
            && let Some(p) = val_tab
        {
            self.dock.remove_tab(p);
        }
        // the Interpretation panel while it is shown, as a tab next to the statistics
        let interp_tab = self.dock.find_tab(&Tab::Interpretation);
        if self.state.labels.show && interp_tab.is_none() {
            if let Some(p) = self.dock.find_tab(&Tab::Statistics) {
                self.dock.set_focused_node_and_surface(p.node_path());
            }
            self.dock.push_to_focused_leaf(Tab::Interpretation);
        } else if !self.state.labels.show
            && let Some(p) = interp_tab
        {
            self.dock.remove_tab(p);
        }
        // a tab of the Validation panel asked for (the menu): the panel in front
        if self.state.val.select_tab.is_some()
            && let Some(p) = self.dock.find_tab(&Tab::Validation)
        {
            let _ = self.dock.set_active_tab(p);
        }
        // the Transect panel while there is one, as a tab next to the time series
        let transect_tab = self.dock.find_tab(&Tab::Transect);
        if self.state.transect.on && transect_tab.is_none() {
            if let Some(p) = self.dock.find_tab(&Tab::TimeSeries) {
                self.dock.set_focused_node_and_surface(p.node_path());
            }
            self.dock.push_to_focused_leaf(Tab::Transect);
        } else if !self.state.transect.on
            && let Some(p) = transect_tab
        {
            self.dock.remove_tab(p);
        }
        // new map panels to the right of the main map (splitting its area); those closed by a key or the menu
        for id in std::mem::take(&mut self.state.views_added) {
            match self.dock.find_tab(&Tab::Map) {
                Some(p) => {
                    let surface = &mut self.dock[p.surface];
                    surface.split_right(p.node, 0.5, vec![Tab::MapView(id)]);
                }
                None => self.dock.push_to_focused_leaf(Tab::MapView(id)),
            }
        }
        for id in std::mem::take(&mut self.state.views_closed) {
            if let Some(p) = self.dock.find_tab(&Tab::MapView(id)) {
                self.dock.remove_tab(p);
            }
            for w in &mut self.windows {
                if let Some(p) = w.dock.find_tab(&Tab::MapView(id)) {
                    w.dock.remove_tab(p);
                }
            }
            self.state.close_map_view(id);
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

        self.state.settings_ui(&ctx);
        self.state.popups(&ctx);
        self.state.tool_windows(&ctx);
        self.state.tasks_window(&ctx);
        self.state.exports_window(&ctx);
        self.state.extract_window(&ctx);
        self.state.chips_window(&ctx);
        self.state.log_window(&ctx);
        // the layout kept, every few seconds when it changed (not for the tests)
        if self.layout_checked.elapsed().as_secs_f64() > 3.0 && !self.state.settings.no_save {
            self.layout_checked = Instant::now();
            if let Some(text) = layout_text(&self.dock)
                && text != self.layout_saved
            {
                let _ = std::fs::create_dir_all(crate::settings::app_data_dir());
                if std::fs::write(layout_path(), &text).is_ok() {
                    self.layout_saved = text;
                }
            }
        }
        let s = &mut self.state;
        s.perf.push(start.elapsed().as_secs_f64() * 1000.0);
        s.frame += 1;
        if s.frame <= 2 || s.opts.screenshot.is_some() && !s.screenshot_asked {
            ctx.request_repaint();
        }
    }
}
