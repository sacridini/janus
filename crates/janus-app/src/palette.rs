//! The command palette (Ctrl+K, also Ctrl+Shift+P): one search over what Janus does. The menus' items, the tools
//! (Zeit's, deep learning, the calculator), the display modes, the spectral indices the active layer has the bands
//! for, the open layers, the panels: typed words matched anywhere in a command's name, category and description
//! (letters in order for a short query), the commands used last first. The commands call the same methods as the
//! menus; one that cannot run now is listed with the reason.

use egui::{Align, Color32, Key, Layout, RichText, Sense};
use janus_gpu::Mode;

use crate::app::State;
use crate::display::{MODE_ORDER, mode_name_of};

/// The commands kept in the recent list.
const RECENT: usize = 12;

/// What a command does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Cmd {
    OpenFiles,
    OpenFolder,
    OpenUrl,
    Stac,
    OpenSeparate,
    OpenVector,
    Export(usize),
    CloseActive,
    CloseAll,
    NewProject,
    OpenProject,
    SaveProject,
    SaveProjectAs,
    Undo,
    Redo,
    Settings,
    Exit,
    Fit,
    NewMapView,
    CloseMapView,
    Swipe,
    Transect,
    ClearTransect,
    RoiPolygon,
    ClearRoi,
    Analysis,
    Embeddings,
    Interpretation,
    AttributeTable,
    Exports,
    Performance,
    ResetLayout,
    Satellite,
    Play,
    FirstDate,
    LastDate,
    Tool(String),
    Calculator,
    DeepLearning,
    Tasks,
    Log,
    NewDesign,
    OpenValidation,
    CloseValidation,
    ValidationPanel,
    InterpretSample,
    Chips,
    Accuracy,
    Help,
    Updates,
    About,
    Mode(Mode),
    Index(String),
    Layer(usize),
    ZoomLayer(usize),
    Blend(janus_gpu::Blend),
    GoTo,
    Minimap,
    Lens,
    SavedView(String),
    GroupSelected,
    CopyStyle,
    PasteStyle,
}

/// A command as listed: its name, category, keys, a longer description, and why it cannot run now (None: it can).
pub(crate) struct Command {
    pub cmd: Cmd,
    pub label: String,
    pub category: String,
    pub keys: &'static str,
    pub detail: String,
    pub disabled: Option<String>,
}

impl Command {
    /// The key its use is remembered by (a layer's command by its name, not its place).
    fn key(&self) -> String {
        format!("{}/{}", self.category, self.label)
    }
}

/// What a command asks of the window's layout (the app's, not the state's).
#[derive(Default)]
pub(crate) struct LayoutAsk {
    pub reset_layout: bool,
    pub toggle_performance: bool,
}

#[derive(Default)]
pub struct PaletteUi {
    pub open: bool,
    query: String,
    /// the row picked with the arrows
    sel: usize,
    /// just opened: the field takes the keyboard
    focus: bool,
    /// a command chosen (tests): run at the next frame as if picked
    pub(crate) run_next: Option<String>,
}

impl PaletteUi {
    /// The text typed (--palette, tests).
    pub(crate) fn set_query(&mut self, text: &str) {
        self.query = text.to_string();
        self.sel = 0;
    }
}

/// How well `query` matches a command: each word of it in the name (best at a word's start), the category or the
/// description; a short word may match the name's letters in order (`exp png`: Export map as PNG). None: a word
/// matches nowhere.
pub(crate) fn score(query: &str, label: &str, category: &str, detail: &str) -> Option<i32> {
    let (label, category, detail) = (label.to_lowercase(), category.to_lowercase(), detail.to_lowercase());
    let mut total = 0;
    for w in query.to_lowercase().split_whitespace() {
        total += if let Some(at) = label.find(w) {
            let word_start = at == 0 || !label.as_bytes()[at - 1].is_ascii_alphanumeric();
            if at == 0 {
                100
            } else if word_start {
                80
            } else {
                50
            }
        } else if category.contains(w) {
            30
        } else if detail.contains(w) {
            20
        } else if w.chars().count() <= 6 && in_order(w, &label) {
            10
        } else {
            return None;
        };
    }
    // the shorter of two names that match alike first
    Some(total * 1000 - label.len() as i32)
}

/// The letters of `w` in `text`, in order.
fn in_order(w: &str, text: &str) -> bool {
    let mut it = text.chars();
    w.chars().all(|c| it.by_ref().any(|t| t == c))
}

impl State {
    /// Every command, as things stand: those that cannot run now with the reason.
    pub(crate) fn palette_commands(&self) -> Vec<Command> {
        let mut out = Vec::new();
        let has = self.session.is_some();
        let no_series = || Some("open a series first".to_string());
        let need = |ok: bool, why: &str| (!ok).then(|| why.to_string());
        let mut add = |cmd: Cmd, cat: &str, label: &str, keys: &'static str, disabled: Option<String>| {
            out.push(Command { cmd, label: label.into(), category: cat.into(), keys, detail: String::new(), disabled });
        };
        let series = if has { None } else { no_series() };
        // File
        add(Cmd::OpenFiles, "File", "Open file(s)...", "Ctrl+O", None);
        add(Cmd::OpenFolder, "File", "Open folder...", "Ctrl+Shift+O", None);
        add(Cmd::OpenUrl, "File", "Open URL (COG, S3, GCS)...", "Ctrl+U", None);
        add(Cmd::Stac, "File", "Search STAC catalog...", "", None);
        add(Cmd::OpenSeparate, "File", "Open files as separate layers...", "", None);
        add(Cmd::OpenVector, "File", "Open vector file...", "", None);
        add(Cmd::Export(1), "File", "Export map as PNG...", "Ctrl+E", series.clone());
        add(Cmd::Export(2), "File", "Export values as GeoTIFF...", "Ctrl+Shift+E", series.clone());
        let geo = self.session.as_ref().is_some_and(|s| s.info.geo_transform.is_some());
        add(
            Cmd::Export(3),
            "File",
            "Export rendered view as GeoTIFF...",
            "Ctrl+Shift+V",
            need(geo, "a georeferenced series"),
        );
        add(Cmd::Export(5), "File", "Export animation (GIF, MP4, WebM)...", "Ctrl+Shift+A", series.clone());
        add(Cmd::NewProject, "File", "New project", "", None);
        add(Cmd::OpenProject, "File", "Open project...", "", None);
        add(Cmd::SaveProject, "File", "Save project", "Ctrl+S", None);
        add(Cmd::SaveProjectAs, "File", "Save project as...", "", None);
        let undo = self.can_undo().map(|w| format!("Undo {w}"));
        let redo = self.can_redo().map(|w| format!("Redo {w}"));
        add(Cmd::Undo, "Edit", undo.as_deref().unwrap_or("Undo"), "Ctrl+Z", need(undo.is_some(), "nothing to undo"));
        add(
            Cmd::Redo,
            "Edit",
            redo.as_deref().unwrap_or("Redo"),
            "Ctrl+Shift+Z",
            need(redo.is_some(), "nothing to redo"),
        );
        add(Cmd::CloseActive, "File", "Close active layer", "", series.clone());
        add(Cmd::CloseAll, "File", "Close all", "", need(!self.layers.is_empty(), "nothing is open"));
        add(Cmd::Settings, "File", "Settings...", "Ctrl+,", None);
        add(Cmd::Exit, "File", "Exit", "Alt+F4", None);
        // View
        add(Cmd::Fit, "View", "Fit map to window", "F", series.clone());
        add(Cmd::NewMapView, "View", "New map view", "Ctrl+T", series.clone());
        add(Cmd::CloseMapView, "View", "Close map view", "Ctrl+W", need(!self.views.is_empty(), "no map view is open"));
        add(Cmd::Swipe, "View", "Swipe on / off", "C", series.clone());
        add(Cmd::Lens, "View", "Lens on / off", "L", series.clone());
        add(Cmd::Minimap, "View", "Minimap on / off", "M", series.clone());
        add(Cmd::GoTo, "View", "Go to coordinate...", "Ctrl+G", series.clone());
        for v in self.saved_views() {
            add(Cmd::SavedView(v.name.clone()), "View", &format!("Saved view: {}", v.name), "", series.clone());
        }
        add(Cmd::Transect, "View", "Draw transect", "T", series.clone());
        add(Cmd::ClearTransect, "View", "Clear transect", "", need(self.transect.on, "there is no transect"));
        add(Cmd::RoiPolygon, "View", "Draw ROI polygon", "R", series.clone());
        add(Cmd::ClearRoi, "View", "Clear ROI", "", need(self.series.roi_rect.is_some(), "there is no ROI"));
        add(Cmd::Analysis, "View", "Analysis panel", "", series.clone());
        add(Cmd::Embeddings, "View", "Embeddings panel", "", None);
        add(Cmd::Interpretation, "View", "Interpretation panel", "", None);
        add(
            Cmd::AttributeTable,
            "View",
            "Attribute table",
            "",
            need(!self.vectors.is_empty(), "no vector layer is open"),
        );
        add(Cmd::Exports, "View", "Exports", "Ctrl+J", None);
        add(Cmd::Performance, "View", "Performance panel", "", None);
        add(Cmd::ResetLayout, "View", "Reset layout", "Ctrl+Shift+R", None);
        add(Cmd::Satellite, "View", "Satellite basemap on / off", "S", series.clone());
        let dates = self.session.as_ref().is_some_and(|s| s.info.t() > 1);
        let no_dates = need(dates, "the active layer has a single date");
        add(Cmd::Play, "Time", "Play / pause", "P", no_dates.clone());
        add(Cmd::FirstDate, "Time", "First date", "Home", no_dates.clone());
        add(Cmd::LastDate, "Time", "Last date", "End", no_dates);
        // Tools
        let info = self.session.as_ref().map(|s| s.info.clone());
        for t in &janus_zeit::manifest().tools {
            let why =
                info.as_ref().map_or(Some("open a series first".to_string()), |i| crate::zeit::applicability(t, i));
            let mut c = Command {
                cmd: Cmd::Tool(t.id.clone()),
                label: format!("{}...", t.name),
                category: format!("Tools > {}", t.category),
                keys: "",
                detail: t.description.clone(),
                disabled: None,
            };
            // its window opens anyway: the reason shows there
            c.detail = match why {
                Some(w) => format!("{} (now: {w})", c.detail),
                None => c.detail,
            };
            out.push(c);
        }
        let mut add = |cmd: Cmd, cat: &str, label: &str, keys: &'static str, disabled: Option<String>| {
            out.push(Command { cmd, label: label.into(), category: cat.into(), keys, detail: String::new(), disabled });
        };
        add(Cmd::Calculator, "Tools", "Raster calculator...", "", None);
        add(Cmd::DeepLearning, "Tools > Classification", "Deep learning (TempCNN, L-TAE, Presto, U-TAE)...", "", None);
        add(Cmd::Tasks, "Tools", "Tasks", "", None);
        add(Cmd::Log, "Tools", "Log", "", need(!self.zeit.jobs.is_empty(), "no task has run yet"));
        // Validation
        let project = self.validation_project_open();
        let no_project = need(project, "no validation project is open");
        add(Cmd::NewDesign, "Validation", "New sampling design...", "", None);
        add(Cmd::OpenValidation, "Validation", "Open validation project...", "", None);
        add(Cmd::CloseValidation, "Validation", "Close validation project", "", no_project.clone());
        add(Cmd::ValidationPanel, "Validation", "Validation panel", "", None);
        add(Cmd::InterpretSample, "Validation", "Interpret the sample", "", no_project.clone());
        add(Cmd::Chips, "Validation", "Chips", "", None);
        add(Cmd::Accuracy, "Validation", "Accuracy and areas", "", no_project);
        // Help
        add(Cmd::Help, "Help", "Shortcuts and usage", "", None);
        add(Cmd::Updates, "Help", "Check for updates...", "", None);
        add(Cmd::About, "Help", "About Janus", "", None);
        // the Layers panel: the active layer's blend mode, its style, the selection grouped
        if let Some(l) = self.layers.get(self.active).filter(|l| has && !l.canvas) {
            for b in janus_gpu::Blend::ALL {
                let why = (l.blend == b).then(|| "it is the active layer's".to_string());
                add(Cmd::Blend(b), "Layers", &format!("Blend: {}", b.name()), "", why);
            }
            add(Cmd::CopyStyle, "Layers", "Copy style of the active layer", "", None);
        }
        let n = self.contents.selected.len();
        let clip = self.contents.style_clip.is_some();
        add(
            Cmd::PasteStyle,
            "Layers",
            "Paste style on the selected layers",
            "",
            need(clip && n > 0, if clip { "select layers on the Layers panel" } else { "copy a layer's style first" }),
        );
        add(
            Cmd::GroupSelected,
            "Layers",
            "Group the selected layers",
            "",
            need(n > 0, "select layers on the Layers panel"),
        );
        // the active layer's display
        if let Some(info) = &info {
            for m in MODE_ORDER {
                let why = need(self.mode_available(m), "not for this layer (classes, a stack or one date)");
                add(Cmd::Mode(m), "Display", mode_name_of(m, info), "", why);
            }
            // the indices it has the bands for
            if info.bands_per_date > 1 || info.spectral {
                let names = janus_cube::calc::names::BandNames::of(info);
                for ix in janus_cube::calc::indices::all().iter().filter(|ix| ix.usable(&names)) {
                    out.push(Command {
                        cmd: Cmd::Index(ix.name.clone()),
                        label: ix.title(),
                        category: "Index".into(),
                        keys: "",
                        detail: format!("{} {}", ix.formula, ix.domain),
                        disabled: None,
                    });
                }
            }
        }
        // the layers
        for (i, l) in self.layers.iter().enumerate().filter(|(_, l)| !l.canvas) {
            out.push(Command {
                cmd: Cmd::Layer(i),
                label: format!("Make active: {}", l.name),
                category: "Layers".into(),
                keys: "",
                detail: String::new(),
                disabled: (i == self.active).then(|| "it is the active layer".to_string()),
            });
            out.push(Command {
                cmd: Cmd::ZoomLayer(i),
                label: format!("Zoom to: {}", l.name),
                category: "Layers".into(),
                keys: "",
                detail: String::new(),
                disabled: None,
            });
        }
        out
    }

    /// The commands matching the query, best first; without a query, the recent ones first, then all of them.
    pub(crate) fn palette_matches(&self, query: &str) -> Vec<Command> {
        let recent = &self.settings.s.palette_recent;
        let rank = |c: &Command| recent.iter().position(|k| *k == c.key()).unwrap_or(usize::MAX);
        let mut all: Vec<(i32, usize, usize, Command)> = self
            .palette_commands()
            .into_iter()
            .enumerate()
            .filter_map(|(i, c)| {
                let s = if query.trim().is_empty() { 0 } else { score(query, &c.label, &c.category, &c.detail)? };
                Some((s, rank(&c), i, c))
            })
            .collect();
        // a match before a weaker one; among equals, the recent first, then the menus' order. Exit and Close all
        // after the others that match as well: Enter on a short query should not close anything
        for x in &mut all {
            if matches!(x.3.cmd, Cmd::Exit | Cmd::CloseAll) {
                x.0 -= 5000;
            }
        }
        all.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        all.into_iter().map(|x| x.3).collect()
    }

    /// The palette, when open: a field, then the matching commands; arrows and Enter, or a click; Escape closes.
    pub(crate) fn palette_ui(&mut self, ctx: &egui::Context) -> LayoutAsk {
        let mut ask = LayoutAsk::default();
        if let Some(key) = self.palette.run_next.take()
            && let Some(c) = self.palette_commands().into_iter().find(|c| c.key() == key && c.disabled.is_none())
        {
            ask = self.run_command(&c, ctx);
        }
        if !self.palette.open {
            return ask;
        }
        let matches = self.palette_matches(&self.palette.query);
        let p = &mut self.palette;
        let (up, down, enter, esc) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if down {
            p.sel = (p.sel + 1).min(matches.len().saturating_sub(1));
        }
        if up {
            p.sel = p.sel.saturating_sub(1);
        }
        p.sel = p.sel.min(matches.len().saturating_sub(1));
        let mut chosen = enter.then_some(p.sel).filter(|&k| k < matches.len());
        let screen = ctx.content_rect();
        let width = (screen.width() - 32.0).clamp(240.0, 640.0);
        let mut close = esc;
        let r = egui::Area::new(egui::Id::new("command palette"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(screen.center().x - width / 2.0, screen.top() + 60.0))
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).inner_margin(8).show(ui, |ui| {
                    ui.set_width(width);
                    let field = ui.add(
                        egui::TextEdit::singleline(&mut p.query)
                            .hint_text("Type a command: open, bfast, evi, swipe, export png...")
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(&mut p.focus) || !field.has_focus() {
                        field.request_focus();
                    }
                    if field.changed() {
                        p.sel = 0;
                    }
                    ui.add_space(4.0);
                    if matches.is_empty() {
                        ui.weak("No command matches.");
                    }
                    let row_h = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
                    egui::ScrollArea::vertical()
                        .max_height(row_h * 12.0)
                        .min_scrolled_height(row_h * 12.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (k, c) in matches.iter().enumerate() {
                                let on = k == p.sel;
                                // the row picked tinted across, a disabled one faint
                                let v = ui.visuals();
                                let fill =
                                    if on { v.selection.bg_fill.gamma_multiply(0.45) } else { Color32::TRANSPARENT };
                                let text = if c.disabled.is_some() { v.weak_text_color() } else { v.text_color() };
                                let row = egui::Frame::new()
                                    .fill(fill)
                                    .corner_radius(3)
                                    .inner_margin(egui::Margin::symmetric(6, 0))
                                    .show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.set_min_height(row_h);
                                            ui.set_width(ui.available_width());
                                            ui.add(egui::Label::new(RichText::new(&c.label).color(text)).truncate());
                                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                                if !c.keys.is_empty() {
                                                    ui.label(RichText::new(c.keys).monospace().weak());
                                                }
                                                ui.label(RichText::new(&c.category).small().weak());
                                            });
                                        });
                                    })
                                    .response;
                                let r = ui.interact(row.rect, ui.id().with(("command", k)), Sense::click());
                                if r.hovered() && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO) {
                                    p.sel = k;
                                }
                                let tip = match (&c.disabled, c.detail.is_empty()) {
                                    (Some(w), _) => format!("Not now: {w}"),
                                    (None, false) => c.detail.clone(),
                                    _ => String::new(),
                                };
                                let r = if tip.is_empty() { r } else { r.on_hover_text(tip) };
                                if on && (up || down) {
                                    r.scroll_to_me(None);
                                }
                                if r.clicked() {
                                    chosen = Some(k);
                                }
                            }
                        });
                });
            });
        // a click outside closes it
        if ctx.input(|i| i.pointer.any_pressed())
            && ctx.input(|i| i.pointer.interact_pos()).is_some_and(|q| !r.response.rect.contains(q))
        {
            close = true;
        }
        if let Some(c) = chosen.and_then(|k| matches.into_iter().nth(k))
            && c.disabled.is_none()
        {
            close = true;
            ask = self.run_command(&c, ctx);
        }
        if close {
            self.palette.open = false;
        }
        ask
    }

    /// Opens (or closes) the palette, its field empty.
    pub(crate) fn toggle_palette(&mut self) {
        let p = &mut self.palette;
        p.open = !p.open;
        p.query.clear();
        p.sel = 0;
        p.focus = p.open;
    }

    /// A command run; kept first in the recent list.
    pub(crate) fn run_command(&mut self, c: &Command, ctx: &egui::Context) -> LayoutAsk {
        let key = c.key();
        let r = &mut self.settings.s.palette_recent;
        r.retain(|k| *k != key);
        r.insert(0, key);
        r.truncate(RECENT);
        let mut ask = LayoutAsk::default();
        match c.cmd.clone() {
            Cmd::OpenFiles => self.open_files_dialog(true, ctx),
            Cmd::OpenFolder => self.open_folder_dialog(true, ctx),
            Cmd::OpenUrl => self.open_url_dialog(true),
            Cmd::Stac => self.stac.show = true,
            Cmd::OpenSeparate => {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    self.pending_opens.extend(files.iter().map(|f| vec![f.to_string_lossy().into_owned()]));
                }
            }
            Cmd::OpenVector => {
                if let Some(files) = rfd::FileDialog::new().pick_files() {
                    self.add_vector_files(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), ctx);
                }
            }
            Cmd::Export(kind) => self.open_export(kind),
            Cmd::CloseActive => self.remove_layer(self.active, ctx),
            Cmd::CloseAll => self.close_all(ctx),
            Cmd::NewProject => self.new_project(ctx),
            Cmd::OpenProject => self.open_project_dialog(ctx),
            Cmd::SaveProject => self.save_project(),
            Cmd::SaveProjectAs => self.save_project_as(),
            Cmd::Undo => self.undo(ctx),
            Cmd::Redo => self.redo(ctx),
            Cmd::Settings => self.settings.show = true,
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Fit => self.map.fit(),
            Cmd::NewMapView => self.new_map_view(),
            Cmd::CloseMapView => {
                if let Some(v) = self.views.last() {
                    self.views_closed.push(v.id);
                }
            }
            Cmd::Swipe => self.toggle_swipe(),
            Cmd::Transect => self.transect_draft.mode = true,
            Cmd::ClearTransect => self.clear_transect(),
            Cmd::RoiPolygon => {
                if !self.series.poly.on {
                    self.toggle_polygon();
                }
            }
            Cmd::ClearRoi => self.clear_roi(),
            Cmd::Analysis => self.analysis.show = !self.analysis.show,
            Cmd::Embeddings => self.show_embeddings = !self.show_embeddings,
            Cmd::Interpretation => self.labels.show = !self.labels.show,
            Cmd::AttributeTable => self.attr_table.show = !self.attr_table.show,
            Cmd::Exports => self.show_exports = !self.show_exports,
            Cmd::Performance => ask.toggle_performance = true,
            Cmd::ResetLayout => ask.reset_layout = true,
            Cmd::Satellite => self.toggle_satellite(),
            Cmd::Play => self.display.playing = !self.display.playing,
            Cmd::FirstDate => self.set_date(0),
            Cmd::LastDate => {
                let n = self.session.as_ref().map_or(1, |s| s.info.t());
                self.set_date(n - 1);
            }
            Cmd::Tool(id) => {
                if let Some(t) = janus_zeit::manifest().tool(&id) {
                    self.set_tool_open(t, true);
                }
            }
            Cmd::Calculator => self.calc.show = true,
            Cmd::DeepLearning => self.ml.open = true,
            Cmd::Tasks => self.zeit.show_tasks = true,
            Cmd::Log => self.zeit.log.show = true,
            Cmd::NewDesign => {
                self.val.show = true;
                self.val.select_tab = Some(0);
            }
            Cmd::OpenValidation => {
                if let Some(f) = rfd::FileDialog::new().add_filter("GeoPackage", &["gpkg"]).pick_file() {
                    self.open_validation_project(&f.to_string_lossy(), ctx);
                }
            }
            Cmd::CloseValidation => self.close_validation_project(),
            Cmd::ValidationPanel => self.val.show = !self.val.show,
            Cmd::InterpretSample => {
                self.start_interpretation(self.val.vector_id);
                self.labels.show = true;
            }
            Cmd::Chips => self.val.show_chips = !self.val.show_chips,
            Cmd::Accuracy => {
                self.val.show = true;
                self.val.select_tab = Some(1);
            }
            Cmd::Help => self.help_popup = true,
            Cmd::Updates => {
                self.update.open = true;
                self.update.check(ctx);
            }
            Cmd::About => self.about_open = true,
            Cmd::Mode(m) => {
                self.display.mode = m;
                self.map.dirty = true;
            }
            Cmd::Index(name) => self.show_index(&name, ctx),
            Cmd::Layer(i) => {
                if i < self.layers.len() {
                    self.set_active(i, ctx);
                }
            }
            Cmd::ZoomLayer(i) => {
                if i < self.layers.len() {
                    if i != self.active {
                        self.set_active(i, ctx);
                    }
                    self.map.fit();
                }
            }
            Cmd::GoTo => self.toggle_goto(),
            Cmd::Minimap => self.toggle_minimap(),
            Cmd::Lens => self.toggle_lens(),
            Cmd::SavedView(name) => {
                if let Some(v) = self.saved_views().into_iter().find(|v| v.name == name)
                    && let Err(e) = self.restore_view(&v)
                {
                    self.notify(&format!("Saved view {name}"), &e, true);
                }
            }
            Cmd::Blend(b) => {
                if let Some(l) = self.layers.get_mut(self.active) {
                    l.blend = b;
                    self.map.dirty = true;
                }
            }
            Cmd::CopyStyle => {
                let id = self.session.as_ref().map(|s| s.info.id);
                self.contents.style_clip = id.and_then(|id| self.style_of(crate::contents::Sel::Series(id)));
            }
            Cmd::PasteStyle => {
                if let (Some(st), Some(&first)) =
                    (self.contents.style_clip.clone(), self.contents.selected.iter().next())
                {
                    self.paste_style(first, &st);
                }
            }
            Cmd::GroupSelected => {
                self.group_selected();
            }
        }
        ask
    }

    /// An index of the library shown on the active layer (as the Display panel's Apply): the layer reopened with it.
    pub(crate) fn show_index(&mut self, name: &str, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        let Some(ix) = janus_cube::calc::indices::find(name) else { return };
        let names = janus_cube::calc::names::BandNames::of(&info);
        let Some(formula) = ix.formula_for(&names, &info.band_nm) else { return };
        let sel = janus_cube::BandSelection {
            expr: janus_cube::calc::ExprId::intern(&ix.name, &formula),
            chosen: true,
            ..info.sel
        };
        // the Display panel's text follows the layer
        self.calc.draft = None;
        self.reopen_layer(self.active, sel, ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_rank_names_first() {
        let png = score("export png", "Export map as PNG...", "File", "").unwrap();
        let tif = score("export", "Export values as GeoTIFF...", "File", "");
        assert!(png > 0 && tif.is_some());
        assert!(score("export png", "Export values as GeoTIFF...", "File", "").is_none());
        // a word's start beats its middle; the name beats the category and the description
        assert!(score("map", "Map as PNG", "", "").unwrap() > score("map", "Fit map", "", "").unwrap());
        assert!(score("map", "Fit map", "", "").unwrap() > score("map", "Swipe", "", "a map").unwrap());
        assert!(score("tools", "BFAST...", "Tools > Trend", "").is_some());
        // letters in order for a short word
        assert!(score("bfm", "BFAST Monitor...", "", "").is_some());
        assert!(score("xyz", "BFAST Monitor...", "", "").is_none());
    }
}
