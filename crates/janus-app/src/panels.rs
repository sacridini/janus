//! The side panels: Display (date, mode, colormap, stretch, histogram), Layers (the series), Statistics (the
//! cursor's pixel) and Performance (the timings this phase is judged by, RUST_MIGRATION.md).

use egui::{Color32, ComboBox, DragValue, Grid, Rect, Sense, Slider, Ui, vec2};
use janus_cube::multidim::format_g;
use janus_cube::spectral::{COMPOSITES, INDICES, composite_layers, index_bands};
use janus_cube::{BandSelection, QaRule};
use janus_gpu::{Mode, colormaps};

use crate::app::State;
use crate::display::{MODE_ORDER, mode_name_of, range_label};
use crate::theme;

/// A section's title with a line after it (ImGui's SeparatorText).
/// A window of Janus: its title bar thin (egui's has the window's margin above and below the title: 12 points
/// each in the modern interface, a bar about twice the text's height).
///
/// Every such window can leave the main window for an OS window of its own, to take to another screen (see
/// `PanelWindow`); `no_detach` keeps a transient one (a quick prompt) where it is.
pub(crate) fn window<'a>(title: impl Into<egui::WidgetText>, ctx: &egui::Context) -> PanelWindow<'a> {
    let style = ctx.global_style();
    let m = style.spacing.window_margin;
    let bar =
        egui::Frame::window(&style).inner_margin(egui::Margin { left: m.left, right: m.right, top: 3, bottom: 3 });
    let title: egui::WidgetText = title.into();
    let text = title.text().to_string();
    let id = egui::Id::new(text.as_str());
    PanelWindow {
        win: egui::Window::new(title).title_frame(bar).id(id),
        title: text,
        id,
        size: vec2(340.0, 420.0),
        detachable: true,
        own_button: false,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Windows that leave the main window.

/// Where a window of Janus is: in the main window, or in an OS window (an egui viewport) of its own. It lives in the
/// context's memory under the window's id, so a window needs no field for it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Detach {
    /// In an OS window.
    pub detached: bool,
    /// Where (on the screen) and how large the OS window opens, taken from the window as it was drawn.
    at: Option<egui::Pos2>,
    size: Option<egui::Vec2>,
    /// The viewport it was drawn in during the last frame (the main window's is `ViewportId::ROOT`).
    pub viewport: Option<egui::ViewportId>,
    /// The size of the content drawn in the OS window during the last frame.
    pub content: Option<egui::Vec2>,
    /// The last frame the window was asked for.
    seen: u64,
}

impl Detach {
    /// Takes the window out of the main window, to open at `at` with `size`.
    pub fn start(&mut self, at: Option<egui::Pos2>, size: Option<egui::Vec2>) {
        self.detached = true;
        self.at = at;
        self.size = size;
    }

    /// Brings the window back into the main window.
    pub fn back(&mut self) {
        *self = Self { seen: self.seen, ..Self::default() };
    }

    /// Called once in each frame the window is asked for: one that was not asked for in the frame before (it was
    /// closed) comes back into the main window, so that it opens there the next time.
    pub fn seen(&mut self, frame: u64) {
        if self.detached && self.seen.saturating_add(1) < frame {
            self.back();
        }
        self.seen = frame;
    }
}

fn detach_key(id: egui::Id) -> egui::Id {
    egui::Id::new(("detach state", id))
}

fn detach_load(ctx: &egui::Context, id: egui::Id) -> Detach {
    ctx.data(|d| d.get_temp::<Detach>(detach_key(id))).unwrap_or_default()
}

fn detach_store(ctx: &egui::Context, id: egui::Id, st: Detach) {
    ctx.data_mut(|d| d.insert_temp(detach_key(id), st));
}

/// Whether the window with this id is in an OS window of its own.
pub(crate) fn is_detached(ctx: &egui::Context, id: egui::Id) -> bool {
    detach_load(ctx, id).detached
}

/// The viewport the window was drawn in during the last frame and the size of its content (None while it is not
/// drawn in an OS window of its own).
pub(crate) fn detached_viewport(ctx: &egui::Context, id: egui::Id) -> Option<(egui::ViewportId, egui::Vec2)> {
    let st = detach_load(ctx, id);
    st.viewport.zip(st.content)
}

/// Whether the window with this id was asked for in the last frame or the one before (drawn, in the main window or
/// in its own).
pub(crate) fn drawn_last_frame(ctx: &egui::Context, id: egui::Id) -> bool {
    detach_load(ctx, id).seen + 1 >= ctx.cumulative_frame_nr()
}

/// Takes the window with this id out of the main window, or brings it back (what the Detach button does).
pub(crate) fn set_detached(ctx: &egui::Context, id: egui::Id, on: bool) {
    let mut st = detach_load(ctx, id);
    if on && !st.detached {
        let title = ctx.data(|d| d.get_temp::<String>(title_key(id)));
        detach_start(ctx, id, &mut st, title.as_deref());
        st.seen = ctx.cumulative_frame_nr();
    } else if !on {
        st.back();
    }
    detach_store(ctx, id, st);
}

/// The viewport of a window in an OS window of its own.
fn detached_viewport_id(id: egui::Id) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("detached window", id))
}

/// The geometry an OS window of a window of Janus had when it was last seen: its outer position and its inner size
/// in points, `[x, y, width, height]`: what `with_position` and `with_inner_size` take, so that it round-trips.
pub(crate) type Geom = [f32; 4];

/// The geometries kept, by the window's title (stable between runs, unlike its id's hash). They live in the
/// context's memory; `pump_settings` carries them to and from the settings file.
fn geoms_key() -> egui::Id {
    egui::Id::new("detached windows geometry")
}

pub(crate) fn detach_geoms(ctx: &egui::Context) -> Vec<(String, Geom)> {
    ctx.data(|d| d.get_temp::<Vec<(String, Geom)>>(geoms_key())).unwrap_or_default()
}

pub(crate) fn set_detach_geoms(ctx: &egui::Context, geoms: Vec<(String, Geom)>) {
    ctx.data_mut(|d| d.insert_temp(geoms_key(), geoms));
}

/// How many windows' geometries are kept.
const MAX_GEOMS: usize = 64;

/// Keeps the geometry of the OS window being drawn (called inside its viewport), when it changed. A minimized window
/// (the system parks it far away) or one not placed yet is left out.
fn record_geom(ctx: &egui::Context, key: &str) {
    let (outer, inner, minimized) = ctx.input(|i| {
        let v = i.viewport();
        (v.outer_rect, v.inner_rect, v.minimized)
    });
    let (Some(outer), Some(inner)) = (outer, inner) else { return };
    if minimized == Some(true) || inner.width() < 100.0 || inner.height() < 60.0 {
        return;
    }
    // whole points: no jitter from the scale factor's fractions
    let g = [outer.min.x.round(), outer.min.y.round(), inner.width().round(), inner.height().round()];
    let mut all = detach_geoms(ctx);
    match all.iter_mut().find(|(k, _)| k == key) {
        Some(e) if e.1 == g => return,
        Some(e) => e.1 = g,
        None => {
            if all.len() >= MAX_GEOMS {
                all.remove(0);
            }
            all.push((key.to_string(), g));
        }
    }
    set_detach_geoms(ctx, all);
}

/// The title the window with this id was drawn with (its key for the geometries).
fn title_key(id: egui::Id) -> egui::Id {
    egui::Id::new(("detach title", id))
}

/// Takes the window out of the main window: it opens where and as large as it was in an OS window before (when that
/// place is plausible: its top-left no farther from the main window than a monitor's size, else the screen it was
/// on is gone), else where it is drawn now.
fn detach_start(ctx: &egui::Context, id: egui::Id, st: &mut Detach, title: Option<&str>) {
    let (outer, monitor) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().monitor_size));
    let origin = outer.map(|r| r.min).unwrap_or_default();
    let area = ctx.memory(|m| m.area_rect(id));
    let mut at = area.map(|r| origin + r.min.to_vec2());
    let mut size = area.map(|r| r.size());
    let saved = title.and_then(|t| detach_geoms(ctx).into_iter().find(|(k, _)| k == t)).map(|e| e.1);
    if let Some([x, y, w, h]) = saved {
        size = Some(vec2(w, h));
        let pos = egui::pos2(x, y);
        if plausible_position(pos, outer, monitor) {
            at = Some(pos);
        }
    }
    st.start(at, size);
}

/// Is a saved position still on a screen: its top-left within a monitor's size of the main window's rectangle (any
/// side)? Without the main window's place or the monitor's size nothing says otherwise.
fn plausible_position(pos: egui::Pos2, main: Option<Rect>, monitor: Option<egui::Vec2>) -> bool {
    let (Some(main), Some(monitor)) = (main, monitor) else { return true };
    main.expand2(monitor).contains(pos)
}

/// The OS window's builder: titled, sized and placed from what the window was in the main window.
fn detached_builder(title: &str, st: &mut Detach, default: egui::Vec2) -> egui::ViewportBuilder {
    let size = st.size.unwrap_or(default);
    let mut builder = egui::ViewportBuilder::default()
        .with_title(format!("Janus - {title}"))
        .with_inner_size([size.x.max(320.0), size.y.max(200.0)])
        .with_min_inner_size([240.0, 160.0]);
    if let Some(at) = st.at.take() {
        builder = builder.with_position(at);
    }
    builder
}

/// The row at the top of a window's content with the button to take it out of the main window (or, in its own
/// window, back into it). `width` is how wide the row is in the main window, so that the button is at its right
/// edge without making the window wider. True when the button was clicked.
fn detach_row(ui: &mut Ui, width: f32, detached: bool) -> bool {
    let mut clicked = false;
    let row = vec2(ui.available_width().min(width), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let (label, tip) = if detached {
            ("Back to main window", "This window in the main window again (closing its window does too)")
        } else {
            ("Detach", "This window in an OS window of its own, to take to another screen")
        };
        clicked = ui.small_button(label).on_hover_text(tip).clicked();
    });
    clicked
}

/// A window of Janus (`window`, `fitted_window`): `egui::Window`'s builder, drawn in the main window or, once it is
/// detached (its Detach button), in an OS window of its own with the same content. Closing the OS window brings it
/// back into the main window; the window's X (and what the caller's `open` says) closes it as before.
#[must_use = "You should call .show()"]
pub(crate) struct PanelWindow<'a> {
    win: egui::Window<'a>,
    title: String,
    id: egui::Id,
    /// The default size, for the OS window when the window was not drawn yet.
    size: egui::Vec2,
    detachable: bool,
    /// The window's content has its own button (the calculator's toolbar): none is added.
    own_button: bool,
}

impl<'a> PanelWindow<'a> {
    pub fn id(mut self, id: egui::Id) -> Self {
        self.win = self.win.id(id);
        self.id = id;
        self
    }
    pub fn open(mut self, open: &'a mut bool) -> Self {
        self.win = self.win.open(open);
        self
    }
    pub fn resizable(mut self, r: impl Into<egui::Vec2b>) -> Self {
        self.win = self.win.resizable(r);
        self
    }
    pub fn collapsible(mut self, c: bool) -> Self {
        self.win = self.win.collapsible(c);
        self
    }
    pub fn default_pos(mut self, p: impl Into<egui::Pos2>) -> Self {
        self.win = self.win.default_pos(p);
        self
    }
    pub fn current_pos(mut self, p: impl Into<egui::Pos2>) -> Self {
        self.win = self.win.current_pos(p);
        self
    }
    pub fn anchor(mut self, a: egui::Align2, offset: impl Into<egui::Vec2>) -> Self {
        self.win = self.win.anchor(a, offset);
        self
    }
    pub fn pivot(mut self, p: egui::Align2) -> Self {
        self.win = self.win.pivot(p);
        self
    }
    pub fn default_size(mut self, s: impl Into<egui::Vec2>) -> Self {
        let s = s.into();
        self.size = s;
        self.win = self.win.default_size(s);
        self
    }
    pub fn default_width(mut self, w: f32) -> Self {
        self.size.x = w;
        self.win = self.win.default_width(w);
        self
    }
    pub fn default_height(mut self, h: f32) -> Self {
        self.size.y = h;
        self.win = self.win.default_height(h);
        self
    }
    pub fn fixed_size(mut self, s: impl Into<egui::Vec2>) -> Self {
        let s = s.into();
        self.size = s;
        self.win = self.win.fixed_size(s);
        self
    }
    pub fn min_size(mut self, s: impl Into<egui::Vec2>) -> Self {
        self.win = self.win.min_size(s);
        self
    }
    pub fn max_width(mut self, w: f32) -> Self {
        self.win = self.win.max_width(w);
        self
    }
    pub fn max_height(mut self, h: f32) -> Self {
        self.win = self.win.max_height(h);
        self
    }

    /// A window that stays in the main window (a transient prompt: nobody takes it to another screen).
    #[allow(dead_code)]
    pub fn no_detach(mut self) -> Self {
        self.detachable = false;
        self
    }

    /// The content brings its own Detach button (the calculator's toolbar, with `set_detached`): none is added.
    pub fn own_detach_button(mut self) -> Self {
        self.own_button = true;
        self
    }

    /// Draws the window: in the main window, or in its OS window. The content runs once in either case, but the
    /// result is only given when the window is in the main window.
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add: impl FnOnce(&mut Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>> {
        let Self { win, title, id, size, detachable, own_button } = self;
        if !detachable {
            return win.show(ctx, add);
        }
        ctx.data_mut(|d| d.insert_temp(title_key(id), title.clone()));
        let mut st = detach_load(ctx, id);
        st.seen(ctx.cumulative_frame_nr());
        if st.detached {
            let builder = detached_builder(&title, &mut st, size);
            let mut add = Some(add);
            let mut back = false;
            let vid = detached_viewport_id(id);
            let closed = ctx.show_viewport_immediate(vid, builder, |ui, _class| {
                st.viewport = Some(ui.ctx().viewport_id());
                record_geom(ui.ctx(), &title);
                egui::CentralPanel::default().show(ui, |ui| {
                    if !own_button {
                        back = detach_row(ui, f32::INFINITY, true);
                    }
                    if let Some(add) = add.take() {
                        add(ui);
                    }
                    st.content = Some(ui.min_rect().size());
                });
                ui.input(|i| i.viewport().close_requested())
            });
            // the content's own button (own_button) may have brought it back during the frame
            if closed || back || !is_detached(ctx, id) {
                st.back();
            }
            detach_store(ctx, id, st);
            return None;
        }
        let prev = ctx.memory(|m| m.area_rect(id));
        let m = ctx.global_style().spacing.window_margin;
        let row = prev.map_or(size.x, |r| r.width()) - f32::from(m.left) - f32::from(m.right);
        let mut asked = false;
        let out = win.show(ctx, |ui| {
            if !own_button {
                asked = detach_row(ui, row.max(80.0), false);
            }
            add(ui)
        });
        // the content's own button may have changed the state: read it again
        let mut st = detach_load(ctx, id);
        if asked {
            detach_start(ctx, id, &mut st, Some(&title));
        }
        st.viewport = None;
        st.seen = ctx.cumulative_frame_nr();
        detach_store(ctx, id, st);
        out
    }
}

/// What a dialog's content gave and whether it asks to be closed (`egui::ModalResponse`'s `inner` and
/// `should_close`).
pub(crate) struct ModalOut<T> {
    pub inner: T,
    close: bool,
}

impl<T> ModalOut<T> {
    pub fn should_close(&self) -> bool {
        self.close
    }
}

/// A dialog of Janus (a form with its buttons, not a prompt): an `egui::Modal` in the main window, or, once it is
/// detached (its Detach button: the main window is not blocked then, to look at the map while filling it in), an OS
/// window of its own with the same content. Closing the OS window brings it back; Escape closes it in either place.
#[must_use = "You should call .show()"]
pub(crate) struct PanelModal {
    id: egui::Id,
    title: String,
}

pub(crate) fn modal(id: egui::Id, title: &str) -> PanelModal {
    PanelModal { id, title: title.to_string() }
}

impl PanelModal {
    pub fn show<T: Default>(self, ctx: &egui::Context, add: impl FnOnce(&mut Ui) -> T) -> ModalOut<T> {
        let Self { id, title } = self;
        ctx.data_mut(|d| d.insert_temp(title_key(id), title.clone()));
        let mut st = detach_load(ctx, id);
        st.seen(ctx.cumulative_frame_nr());
        if st.detached {
            let builder = detached_builder(&title, &mut st, vec2(480.0, 360.0));
            let (mut add, mut inner, mut back, mut esc) = (Some(add), None, false, false);
            let vid = detached_viewport_id(id);
            let closed = ctx.show_viewport_immediate(vid, builder, |ui, _class| {
                st.viewport = Some(ui.ctx().viewport_id());
                record_geom(ui.ctx(), &title);
                egui::CentralPanel::default().show(ui, |ui| {
                    back = detach_row(ui, f32::INFINITY, true);
                    if let Some(add) = add.take() {
                        inner = Some(add(ui));
                    }
                    st.content = Some(ui.min_rect().size());
                });
                esc = ui.input(|i| i.key_pressed(egui::Key::Escape)) && !egui::Popup::is_any_open(ui.ctx());
                ui.input(|i| i.viewport().close_requested())
            });
            if closed || back || !is_detached(ctx, id) {
                st.back();
            }
            detach_store(ctx, id, st);
            return ModalOut { inner: inner.unwrap_or_default(), close: esc };
        }
        let prev = ctx.memory(|m| m.area_rect(id));
        let m = ctx.global_style().spacing.window_margin;
        let row = prev.map_or(300.0, |r| r.width()) - f32::from(m.left) - f32::from(m.right);
        let mut asked = false;
        let out = egui::Modal::new(id).show(ctx, |ui| {
            asked = detach_row(ui, row.max(80.0), false);
            add(ui)
        });
        let close = out.should_close();
        let mut st = detach_load(ctx, id);
        if asked {
            detach_start(ctx, id, &mut st, Some(&title));
        }
        st.seen = ctx.cumulative_frame_nr();
        detach_store(ctx, id, st);
        ModalOut { inner: out.inner, close }
    }
}

/// A window whose height follows its content (what a search or a choice adds below grows it, and it shrinks back),
/// up to the screen's; past that its content scrolls. The width stays the user's to change. Its content goes in
/// `fitted_body`.
pub(crate) fn fitted_window<'a>(
    title: impl Into<egui::WidgetText>,
    ctx: &egui::Context,
    width: f32,
) -> PanelWindow<'a> {
    let max_h = fitted_height(ctx);
    window(title, ctx).resizable([true, false]).default_width(width).default_height(max_h).max_height(max_h)
}

/// The tallest a fitted window can be: the screen less the menu bar and a gap at the bottom (the window's own
/// limit takes its title bar off).
fn fitted_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 56.0).max(120.0)
}

/// The content of a `fitted_window`: as tall as it is, up to the screen's height, then scrolled.
pub(crate) fn fitted_body<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    let mut max_h = fitted_height(ui.ctx());
    // in an OS window of its own the content scrolls in what it has (the window is as tall as the user made it)
    if ui.ctx().viewport_id() != egui::ViewportId::ROOT {
        max_h = max_h.min(ui.available_height().max(120.0));
    }
    egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(max_h).show(ui, add).inner
}

pub(crate) fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong(title);
        ui.add(egui::Separator::default().horizontal());
    });
}

/// A colormap's gradient with its name, `width` points wide (ImPlot's ColormapButton).
fn colormap_button(ui: &mut Ui, cmap: usize, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, crate::fit::scaled(ui, 20.0)), Sense::click());
    let cm = &colormaps::all()[cmap];
    let n = if cm.qualitative { cm.keys().count() } else { 48 };
    for i in 0..n {
        let c = if cm.qualitative { cm.keys().nth(i).unwrap() } else { cm.sample((i as f32 + 0.5) / n as f32) };
        let x0 = rect.min.x + rect.width() * i as f32 / n as f32;
        let r =
            Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x0 + rect.width() / n as f32 + 0.5, rect.max.y));
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    }
    // the name over it, legible on any colours
    let font = egui::FontId::proportional(crate::fit::scaled(ui, 13.0));
    ui.painter().text(
        rect.center() + vec2(1.0, 1.0),
        egui::Align2::CENTER_CENTER,
        cm.name,
        font.clone(),
        Color32::BLACK,
    );
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, cm.name, font, Color32::WHITE);
    if resp.hovered() {
        ui.painter().rect_stroke(rect, 0.0, egui::Stroke::new(1.0, Color32::WHITE), egui::StrokeKind::Inside);
    }
    resp
}

impl State {
    /// What the active series allows (C++ modeAvailable): class codes have no means or trends; a single date no
    /// temporal statistics; a stack of variables neither (a mean of different variables means nothing), only its
    /// bands one at a time or three as RGB.
    pub(crate) fn mode_available(&self, m: Mode) -> bool {
        let n = self.session.as_ref().map_or(0, |s| s.info.t());
        let stack = self.session.as_ref().is_some_and(|s| s.info.stack);
        if self.classes.on() {
            m == Mode::Value
        } else if m == Mode::Similarity {
            n >= 3 && !stack
        } else if m == Mode::Phase {
            // a series of dates round the year
            n >= 6 && !stack && self.session.as_ref().is_some_and(|s| s.info.time_is_date && !s.info.spectral)
        } else {
            m == Mode::Value || m == Mode::Rgb || (n >= 2 && !stack)
        }
    }

    /// The Display panel (app.cpp, uiLayer): the series, its bands, what the map shows (mode, colormap, stretch,
    /// histogram) and the detail at full resolution.
    pub fn display_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if self.vectors_only() {
            ui.weak("Only vector layers are open: the map stands on their CRS and extent.");
            ui.weak("Open a series (File > Open) to see its values; the vector layers stay.");
            return;
        }
        // a run's map selected in Layers: its own colours (the active series' again once a series is clicked)
        if let Some(k) = self.selected_result() {
            self.result_display_ui(ui, k);
            return;
        }
        let Some(s) = &self.session else {
            ui.weak("No series open.");
            if ui
                .add_sized([ui.available_width(), crate::fit::button_height(ui)], egui::Button::new("Open file(s)..."))
                .clicked()
                && let Some(files) = rfd::FileDialog::new().pick_files()
            {
                self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), true, &ctx);
            }
            if ui
                .add_sized([ui.available_width(), crate::fit::button_height(ui)], egui::Button::new("Open folder..."))
                .clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.open_inputs(vec![dir.to_string_lossy().into_owned()], true, &ctx);
            }
            return;
        };
        let info = s.info.clone();
        let n = info.t();
        if let Some(l) = self.layers.get(self.active) {
            ui.colored_label(theme::accent(), &l.name);
        }
        section(ui, "Series");
        ui.add(egui::Label::new(&info.description).wrap());
        let what = info.steps_noun();
        ui.label(format!("{n} {what}: {} to {}", info.layers[0].label, info.layers[n - 1].label));
        ui.label(format!("{} x {} px, {}", info.width, info.height, info.data_type));
        let crs = if info.crs_name.is_empty() { "(no CRS)" } else { &info.crs_name };
        ui.add(egui::Label::new(format!("CRS: {crs} {}", info.crs_authority)).wrap());
        if let Some(g) = info.geo_transform {
            ui.label(format!("Pixel: {} x {}", format_g(g[1], 6), format_g(g[5].abs(), 6)));
        }
        // one file of several bands without dates: a stack of variables or a series, as its bands' names say
        // unless set here
        let one_file = info.layers.iter().all(|l| l.path == info.layers[0].path);
        if n > 1 && one_file && !info.spectral && !info.time_is_date && !info.multidim && info.bands_per_date <= 1 {
            let mut stack = info.stack;
            if ui
                .checkbox(&mut stack, "A stack of variables")
                .on_hover_text(
                    "On: each band is a variable of its own (its own stretch, its own classes), shown one at a \
                     time; no temporal statistics, trends or playback.\nOff: the bands are a series (dates in \
                     order).\nSet from the bands' names (each named: a stack); opens the file again.",
                )
                .changed()
            {
                let sel = janus_cube::BandSelection { stack: Some(stack), ..info.sel };
                self.reopen_layer(self.active, sel, &ctx);
            }
        }
        self.bands_ui(ui, &info);
        self.composite_ui(ui);

        section(ui, "Display");
        let Some(s) = &self.session else { return };
        let available: Vec<bool> = MODE_ORDER.iter().map(|&m| self.mode_available(m)).collect();
        let sim_pins: Vec<u32> = self.series.pins.iter().map(|p| p.id).collect();
        let sim_roi = self.series.roi_rect.is_some();
        // the cursor's peak, marked on the months' wheel
        let phase_mark = (self.display.mode == Mode::Phase)
            .then_some(self.series.hover.as_ref())
            .flatten()
            .zip(self.session.as_ref())
            .and_then(|(h, s)| {
                let (times, offset) = self.display.gpu_times(s);
                (h.values.len() == times.len()).then(|| janus_gpu::phase::harmonic(&times, offset, |t| h.values[t]))?
            })
            .map(|h| h[0]);
        let d = &mut self.display;
        let mut dirty = false;
        ComboBox::from_id_salt("mode")
            .width(ui.available_width())
            .height(400.0)
            .selected_text(mode_name_of(d.mode, &info))
            .show_ui(ui, |ui| {
                for (k, m) in MODE_ORDER.into_iter().enumerate() {
                    dirty |= ui
                        .add_enabled_ui(available[k], |ui| ui.selectable_value(&mut d.mode, m, mode_name_of(m, &info)))
                        .inner
                        .changed();
                }
            });
        if d.mode == Mode::Similarity {
            dirty |= crate::similarity::options_ui(ui, &mut d.sim, &sim_pins, sim_roi);
        }
        if d.mode == Mode::Phase {
            dirty |= crate::phase::options_ui(ui, &mut d.phase, phase_mark);
        }
        let (changed, turned_on) = self.classes.ui(ui, s, d.t);
        if changed {
            self.map.dirty = true;
        }
        if turned_on {
            d.mode = Mode::Value;
        }
        if self.classes.on()
            && ui
                .add_sized([ui.available_width(), 0.0], egui::Button::new("Classes over time..."))
                .on_hover_text(
                    "Analysis > Classes: the share of each class at every date, what became what between dates (a \
                     Sankey diagram) and the transitions' table",
                )
                .clicked()
        {
            // as State::open_class_timeline (the display is borrowed)
            (self.analysis.show, self.analysis.tab, self.analysis.focus) = (true, 1, true);
        }
        if d.mode == Mode::Composite && crate::season::temporal_options_ui(ui, d, &info) {
            dirty = true;
        }
        if crate::season::season_ui(ui, d, &info) {
            dirty = true;
        }
        if !self.classes.on() {
            // colormap, range and histogram: continuous data only
            if d.mode.needs_stats() {
                ui.weak(if s.stats.is_some() {
                    "Computed on the GPU for every pixel (overview)."
                } else {
                    "Waiting for the complete overview..."
                });
                if d.mode == Mode::Slope {
                    ui.weak(format!("Unit: value{}", if info.time_is_date { "/yr" } else { "/step" }));
                }
                if matches!(d.mode, Mode::DropDate | Mode::DropMag) {
                    ui.weak("Largest decrease between consecutive valid\nobservations; dated at the lower one.");
                }
            }
            if d.mode == Mode::Diff {
                let label = |r: i32| {
                    if r < 0 {
                        "Previous date (t-1)".to_string()
                    } else {
                        info.layers[(r as usize).min(n - 1)].label.clone()
                    }
                };
                ui.horizontal(|ui| {
                    ComboBox::from_id_salt("reference")
                        .width(ui.available_width() - 70.0)
                        .height(300.0)
                        .selected_text(label(d.diff_ref))
                        .show_ui(ui, |ui| {
                            for r in -1..n as i32 {
                                if ui.selectable_value(&mut d.diff_ref, r, label(r)).changed() {
                                    d.ranges[Mode::Diff as usize].invalidate();
                                    dirty = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text("The map shows value(date) - value(reference): a fixed date,\nor the previous date of the series");
                    ui.label("Reference");
                });
            }
            if d.mode == Mode::Phase {
                // no colormap: the months' wheel above
            } else if d.mode == Mode::Rgb {
                if info.spectral {
                    // composites by wavelength; the sliders below fine-tune them
                    let current = COMPOSITES
                        .iter()
                        .find(|c| composite_layers(&info, c) == Some(d.rgb))
                        .map_or("Composite...", |c| c.name);
                    ComboBox::from_id_salt("spectral composite")
                        .width(ui.available_width())
                        .selected_text(current)
                        .show_ui(ui, |ui| {
                            for c in &COMPOSITES {
                                let covered = composite_layers(&info, c);
                                let r = ui
                                    .add_enabled(covered.is_some(), egui::Button::selectable(current == c.name, c.name))
                                    .on_disabled_hover_text("Outside this spectrum (no band within 40 nm)");
                                if r.clicked()
                                    && let Some(rgb) = covered
                                {
                                    d.rgb = rgb;
                                    dirty = true;
                                }
                            }
                        })
                        .response
                        .on_hover_text("Bands nearest to each wavelength as red, green and blue");
                }
                for (c, name) in ["R", "G", "B"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        let label = info.layers[d.rgb[c]].label.clone();
                        dirty |= ui
                            .add(
                                Slider::new(&mut d.rgb[c], 0..=n - 1)
                                    .custom_formatter(move |_, _| label.clone())
                                    .show_value(true),
                            )
                            .changed();
                        ui.label(*name);
                    });
                }
            } else {
                // the colormap: its gradient, the others in a popup (the qualitative ones left out)
                let cmap = &mut d.cmap[d.mode as usize];
                let r = colormap_button(ui, *cmap, ui.available_width());
                let popup = egui::Popup::menu(&r)
                    .id(egui::Id::new("colormaps"))
                    .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
                popup.show(|ui| {
                    for (i, c) in colormaps::all().iter().enumerate() {
                        if !c.qualitative && colormap_button(ui, i, crate::fit::scaled(ui, 220.0)).clicked() {
                            *cmap = i;
                            dirty = true;
                            ui.close();
                        }
                    }
                });
            }
            let r = &mut d.ranges[d.mode as usize];
            ui.horizontal(|ui| {
                let speed = f64::from(((r.hi - r.lo) / 300.0).max(1e-6));
                let g = |v: f64, _| format_g(v, 4);
                let a = ui.add(DragValue::new(&mut r.lo).speed(speed).custom_formatter(g)).changed();
                let b = ui.add(DragValue::new(&mut r.hi).speed(speed).custom_formatter(g)).changed();
                if a || b {
                    r.hi = r.hi.max(r.lo + 1e-6);
                    r.manual = true;
                    dirty = true;
                }
                if ui
                    .add_sized([ui.available_width(), crate::fit::button_height(ui)], egui::Button::new("Auto"))
                    .on_hover_text("Automatic range: 2-98% percentiles (symmetric for anomaly, difference and trend)")
                    .clicked()
                {
                    r.manual = false;
                    r.invalidate();
                }
            });
            // a stack: each band has its own range
            if crate::display::time_dependent(d.mode)
                && !info.stack
                && ui
                    .checkbox(&mut d.per_date_range, "Per-date range")
                    .on_hover_text("Off: same range for every date (colors comparable through time)")
                    .changed()
            {
                d.ranges[d.mode as usize].invalidate();
            }
            // the histogram of the values shown, in the colormap, with the stretch's ends to drag
            if !d.hist.xs.is_empty() {
                ui.add_space(4.0);
                let cmap = (!matches!(d.mode, Mode::Rgb | Mode::Phase)).then(|| d.cmap[d.mode as usize]);
                let discrete = d.mode == Mode::DropDate && d.hist.xs.len() == info.t();
                let mode = d.mode;
                let label = |v: f32| range_label(mode, v, &info);
                let r = &mut d.ranges[d.mode as usize];
                let mut hl = d.highlight.filter(|h| h.0 == mode).map(|h| (h.1, h.2));
                dirty |= crate::histogram::histogram_ui(ui, &d.hist, r, cmap, discrete, &label, &mut hl);
                d.highlight = hl.map(|(a, b)| (mode, a, b));
                if let Some((a, b)) = hl {
                    // its share of the pixels, from the histogram's bars inside it
                    let share: f64 = d
                        .hist
                        .xs
                        .iter()
                        .zip(&d.hist.ys)
                        .filter(|(x, _)| **x >= f64::from(a) && **x <= f64::from(b))
                        .map(|(_, y)| y)
                        .sum();
                    let what = if discrete {
                        let (t0, t1) = ((a + 0.5).round(), (b - 0.5).round());
                        if t0 >= t1 { label(t0) } else { format!("{} .. {}", label(t0), label(t1)) }
                    } else {
                        format!("{} .. {}", label(a), label(b))
                    };
                    ui.horizontal(|ui| {
                        ui.colored_label(theme::accent(), format!("Highlighted: {what} ({share:.1} % of the pixels)"));
                        if ui.small_button("Clear").on_hover_text("Every pixel in its colours again").clicked() {
                            d.highlight = None;
                            dirty = true;
                        }
                    });
                }
            }
        }
        section(ui, "Detail");
        dirty |= ui
            .checkbox(&mut d.detail, "Full resolution when zoomed in")
            .on_hover_text(
                "Past the overview resolution, reads tiles of the visible area\nin the background ('Value at date' \
                 and 'Difference' modes;\nthe difference reads the tiles of both dates).",
            )
            .changed();
        if dirty {
            self.map.dirty = true;
        }
    }

    /// A spectrum: an index of its bands as a map of its own, added as a layer above it (app.cpp, uiSpectralIndex).
    fn spectral_index_ui(&mut self, ui: &mut Ui, info: &janus_cube::CubeInfo) {
        section(ui, "Index map");
        let covered: Vec<&janus_cube::spectral::SpectralIndex> =
            INDICES.iter().filter(|s| index_bands(&info.band_nm, s).is_some()).collect();
        if covered.is_empty() {
            ui.weak("No index fits these wavelengths.");
            return;
        }
        self.spectral_index = self.spectral_index.min(covered.len() - 1);
        let label = |s: &janus_cube::spectral::SpectralIndex| format!("{}  {}", s.name, s.what);
        ComboBox::from_id_salt("spectral index")
            .width(ui.available_width())
            .selected_text(label(covered[self.spectral_index]))
            .show_ui(ui, |ui| {
                for (i, s) in covered.iter().enumerate() {
                    ui.selectable_value(&mut self.spectral_index, i, label(s));
                }
            });
        let s = covered[self.spectral_index];
        let Some((a, b)) = index_bands(&info.band_nm, s) else { return };
        let busy = self.opening.is_some();
        let r = ui
            .add_enabled(!busy, egui::Button::new("Add as a layer").min_size(vec2(ui.available_width(), 0.0)))
            .on_hover_text(format!(
                "(A - B) / (A + B) with A = {:.1} nm and B = {:.1} nm (the nearest bands),\n\
                 as a map of its own on top of the spectrum",
                info.band_nm[a as usize - 1],
                info.band_nm[b as usize - 1]
            ));
        if r.clicked() {
            self.add_spectral_index_layer(s, ui.ctx());
        }
    }

    /// An index of the spectrum's bands opened as a layer above it (a map of its own); false if the spectrum does
    /// not cover its bands.
    pub(crate) fn add_spectral_index_layer(
        &mut self,
        s: &janus_cube::spectral::SpectralIndex,
        ctx: &egui::Context,
    ) -> bool {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return false };
        let Some((a, b)) = index_bands(&info.band_nm, s) else { return false };
        let sel = BandSelection { band: a, nd_band: b, chosen: true, ..Default::default() };
        let mut o = crate::session::Opening::start_with(vec![info.first_path.clone()], true, sel, ctx);
        o.name = Some(format!("{} - {}", s.name, self.layers.get(self.active).map_or("", |l| l.name.as_str())));
        self.opening = Some(o);
        true
    }

    /// The Display panel's band selection of a series with several bands per date (app.cpp, uiBands): the band
    /// shown, a normalized difference, a quality band; applied by reopening the layer.
    fn bands_ui(&mut self, ui: &mut Ui, info: &janus_cube::CubeInfo) {
        if info.spectral {
            self.spectral_index_ui(ui, info);
            section(ui, "Analysis");
            if ui
                .add(egui::Button::new("Principal components, similarity").min_size(vec2(ui.available_width(), 0.0)))
                .on_hover_text(
                    "Each pixel's spectrum as one vector (Embeddings panel): its principal\n\
                     components as RGB, of the scene or of an ROI, or its similarity to the\n\
                     spectrum of the cursor, a pin or the ROI",
                )
                .clicked()
            {
                self.show_spectral_analysis();
            }
            return;
        }
        if info.multidim {
            self.multidim_ui(ui, info);
            return;
        }
        if info.bands_per_date <= 1 {
            return;
        }
        if self.band_sel.as_ref().is_none_or(|(id, _)| *id != info.id) {
            self.band_sel = Some((info.id, info.sel));
        }
        let Some((_, mut sel_now)) = self.band_sel else { return };
        let sel = &mut sel_now;
        let expr_on = sel.expr.is_some();
        section(ui, "Bands");
        if expr_on {
            ui.weak("Shown: the expression below (bands A and B are not used)");
        }
        ui.add_enabled_ui(!expr_on, |ui| self.bands_ab_ui(ui, info, sel));
        section(ui, "Index or expression");
        self.expression_ui(ui, info, sel);
        self.band_sel = Some((info.id, *sel));
        let name = |b: i32| {
            format!("{b}: {}", info.band_names.get((b - 1).max(0) as usize).filter(|_| b >= 1).map_or("?", |s| s))
        };
        let band_combo = |ui: &mut Ui, id: &str, b: &mut i32, none: Option<&str>| {
            let text = if *b > 0 { name(*b) } else { none.unwrap_or("?").to_string() };
            ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(text).show_ui(ui, |ui| {
                if let Some(none) = none {
                    ui.selectable_value(b, 0, none);
                }
                for k in 1..=info.bands_per_date as i32 {
                    ui.selectable_value(b, k, name(k));
                }
            });
        };
        section(ui, "Quality");
        ui.weak("Quality band");
        band_combo(ui, "qa band", &mut sel.qa_band, Some("None"));
        if sel.qa_band > 0 {
            if sel.qa_rule == QaRule::None {
                sel.qa_rule = QaRule::Fmask;
            }
            ComboBox::from_id_salt("qa rule")
                .width(ui.available_width())
                .selected_text(sel.qa_rule.label())
                .show_ui(ui, |ui| {
                    for r in [QaRule::Fmask, QaRule::LandsatC2, QaRule::Scl, QaRule::HlsFmask, QaRule::NonZero] {
                        ui.selectable_value(&mut sel.qa_rule, r, r.label());
                    }
                })
                .response
                .on_hover_text(
                    "Dates whose quality code is not usable (cloud, shadow, snow, fill)\nbecome no-data in the map, \
                     the charts and the tools.",
                );
        } else {
            sel.qa_rule = QaRule::None;
        }
        self.band_sel = Some((info.id, *sel));
        let same = sel.band == info.sel.band
            && sel.nd_band == info.sel.nd_band
            && sel.qa_band == info.sel.qa_band
            && sel.qa_rule == info.sel.qa_rule
            && sel.expr == info.sel.expr;
        if !same {
            let sel = *sel;
            let busy = self.opening.is_some();
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new("Apply (reopens the layer)").min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .clicked()
            {
                let ctx = ui.ctx().clone();
                self.reopen_layer(self.active, BandSelection { chosen: true, ..sel }, &ctx);
            }
            if ui.small_button("Revert").clicked() {
                self.band_sel = Some((info.id, info.sel));
                self.calc.draft = None;
            }
        }
    }

    /// Band A shown, or the normalized difference of A and B (an index of two bands by wavelength).
    fn bands_ab_ui(&mut self, ui: &mut Ui, info: &janus_cube::CubeInfo, sel: &mut BandSelection) {
        if !info.band_nm.is_empty() {
            // bands with wavelengths: indices by name
            let current = INDICES
                .iter()
                .find(|s| index_bands(&info.band_nm, s) == Some((sel.band, sel.nd_band)))
                .map_or("Index by wavelength...", |s| s.name);
            ComboBox::from_id_salt("index")
                .width(ui.available_width())
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for s in &INDICES {
                        let covered = index_bands(&info.band_nm, s);
                        let label = format!("{}  {}", s.name, s.what);
                        if ui
                            .add_enabled(covered.is_some(), egui::Button::selectable(current == s.name, label))
                            .clicked()
                            && let Some((a, b)) = covered
                        {
                            sel.band = a;
                            sel.nd_band = b;
                        }
                    }
                })
                .response
                .on_hover_text("Picks A and B below: the bands nearest to the index's wavelengths");
        }
        let name = |b: i32| {
            format!("{b}: {}", info.band_names.get((b - 1).max(0) as usize).filter(|_| b >= 1).map_or("?", |s| s))
        };
        let band_combo = |ui: &mut Ui, id: &str, b: &mut i32, none: Option<&str>| {
            let text = if *b > 0 { name(*b) } else { none.unwrap_or("?").to_string() };
            ComboBox::from_id_salt(id).width(ui.available_width()).selected_text(text).show_ui(ui, |ui| {
                if let Some(none) = none {
                    ui.selectable_value(b, 0, none);
                }
                for k in 1..=info.bands_per_date as i32 {
                    ui.selectable_value(b, k, name(k));
                }
            });
        };
        ui.weak("Shown band (A)");
        band_combo(ui, "band", &mut sel.band, None);
        let mut nd = sel.nd_band > 0;
        if ui
            .checkbox(&mut nd, "Normalized difference with B")
            .on_hover_text(
                "Shows (A - B) / (A + B): A = NIR and B = Red gives NDVI,\nA = NIR and B = SWIR1 gives NDMI.",
            )
            .changed()
        {
            sel.nd_band = if nd { if sel.band == 1 { 2 } else { 1 } } else { 0 };
        }
        if nd {
            band_combo(ui, "nd band", &mut sel.nd_band, None);
        }
    }

    /// The Statistics panel (app.cpp, uiStats): a column per series on the chart (the cursor, the pins, the ROI's
    /// mean, and the other layers' with "All visible layers"), a row per statistic; a categorical series' classes.
    pub fn statistics_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        // no series shown: the statistics of the map described in Pixel values (a stack shown keeps its bands')
        let stack_shown =
            s.info.stack && self.layers.get(self.active).is_some_and(|l| l.visible && !self.result_over(l.z));
        if !self.chart_shown() && !stack_shown {
            self.values_statistics_ui(ui);
            return;
        }
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        struct Col<'a> {
            name: String,
            st: &'a janus_cube::stats::SeriesStats,
            color: Color32,
            /// a pin's id, to remove it
            pin: Option<u32>,
            roi: bool,
            /// whose dates arg_min / arg_max refer to (None: the active layer)
            cube: Option<&'a janus_cube::CubeInfo>,
            values: Option<&'a [f32]>,
            classes: Option<&'a crate::classes::LayerClasses>,
            t: usize,
            /// the Zeit tools fitted on it (the chart's tools, in order)
            fits: Vec<crate::zeit::ToolFit<'a>>,
        }
        let sr = &self.series;
        let active_classes = self.classes.on().then_some(&self.classes);
        let mut cols: Vec<Col> = Vec::new();
        let t = self.display.t;
        if let Some(h) = &sr.hover {
            let name = if h.exact { "Cursor" } else { "Cursor*" }.to_string();
            cols.push(Col {
                name,
                st: &h.stats,
                color: h.color,
                pin: None,
                roi: false,
                cube: None,
                values: Some(&h.values),
                classes: active_classes,
                t,
                fits: self.zeit.fits_of(info.id, crate::zeit::Which::Hover, h.x, h.y),
            });
        }
        for p in &sr.pins {
            let name = format!("Pin {}{}", p.id, if p.exact { "" } else { "*" });
            cols.push(Col {
                name,
                st: &p.stats,
                color: p.color,
                pin: Some(p.id),
                roi: false,
                cube: None,
                values: Some(&p.values),
                classes: active_classes,
                t,
                fits: self.zeit.fits_of(info.id, crate::zeit::Which::Pin(p.id), p.x, p.y),
            });
        }
        if !sr.roi.mean.is_empty() {
            let color = crate::series::ROI_COLOR;
            cols.push(Col {
                name: "ROI mean".into(),
                st: &sr.roi.stats,
                color,
                pin: None,
                roi: true,
                cube: None,
                values: None,
                classes: None,
                t,
                fits: self.roi_fits(),
            });
        }
        if sr.chart_layers == 1 {
            for (i, l) in self.layers.iter().enumerate() {
                let Some(ls) = l.session.as_ref().filter(|_| i != self.active && l.visible) else { continue };
                let lc = l.classes.on().then_some(&l.classes);
                if let Some(h) = &l.series.hover {
                    let name = format!("Cursor [{}]{}", l.name, if h.exact { "" } else { "*" });
                    cols.push(Col {
                        name,
                        st: &h.stats,
                        color: h.color,
                        pin: None,
                        roi: false,
                        cube: Some(&ls.info),
                        values: Some(&h.values),
                        classes: lc,
                        t: l.disp.t,
                        fits: self.zeit.fits_of(ls.info.id, crate::zeit::Which::Hover, h.x, h.y),
                    });
                }
                for p in &l.series.pins {
                    let name = format!("Pin {} [{}]{}", p.id, l.name, if p.exact { "" } else { "*" });
                    cols.push(Col {
                        name,
                        st: &p.stats,
                        color: p.color,
                        pin: None,
                        roi: false,
                        cube: Some(&ls.info),
                        values: Some(&p.values),
                        classes: lc,
                        t: l.disp.t,
                        fits: self.zeit.fits_of(ls.info.id, crate::zeit::Which::Pin(p.id), p.x, p.y),
                    });
                }
                if !l.series.roi.mean.is_empty() && lc.is_none() {
                    let name = format!("ROI mean [{}]{}", l.name, if l.series.roi.complete() { "" } else { "*" });
                    let color = crate::series::ROI_COLOR;
                    cols.push(Col {
                        name,
                        st: &l.series.roi.stats,
                        color,
                        pin: None,
                        roi: false,
                        cube: Some(&ls.info),
                        values: Some(&l.series.roi.mean),
                        classes: None,
                        t: l.disp.t,
                        fits: vec![],
                    });
                }
            }
        }
        if cols.is_empty() {
            ui.weak("Hover over the map, click to drop pins\nor Shift+drag for an ROI.");
            return;
        }
        let unit = if info.time_is_date { "/yr" } else { "/step" };
        let num = |v: f64| janus_cube::multidim::format_g(v, 5);
        let date = |cube: &janus_cube::CubeInfo, i: Option<usize>| {
            i.and_then(|i| cube.layers.get(i)).map_or("-".to_string(), |l| l.label.clone())
        };
        type Row = (
            &'static str,
            fn(
                &janus_cube::stats::SeriesStats,
                &dyn Fn(Option<usize>) -> String,
                &dyn Fn(f64) -> String,
                &str,
            ) -> String,
        );
        let rows: [Row; 12] = [
            ("Valid n", |s, _, _, _| s.n.to_string()),
            ("Mean", |s, _, num, _| num(s.mean)),
            ("Median", |s, _, num, _| num(s.median)),
            ("Std. deviation", |s, _, num, _| num(s.std)),
            ("CV", |s, _, num, _| num(s.cv)),
            ("Minimum", |s, date, num, _| format!("{} ({})", num(s.min), date(s.arg_min))),
            ("Maximum", |s, date, num, _| format!("{} ({})", num(s.max), date(s.arg_max))),
            ("Amplitude", |s, _, num, _| num(s.max - s.min)),
            ("OLS trend", |s, _, num, unit| format!("{}{unit}", num(s.ols_slope))),
            ("R\u{b2} (OLS)", |s, _, num, _| num(s.r2)),
            ("Sen's slope", |s, _, num, unit| format!("{}{unit}", num(s.sen_slope))),
            ("Largest drop", |s, date, num, _| match s.arg_drop {
                Some(_) => format!("{} ({})", num(s.drop_mag), date(s.arg_drop)),
                None if s.n >= 2 => "none".into(),
                None => "-".into(),
            }),
        ];
        let (mut remove_pin, mut remove_roi) = (None, false);
        egui::ScrollArea::horizontal().show(ui, |ui| {
            // rows of a line of text, as ImGui's table (egui's grid makes each as tall as a button)
            let row = ui.text_style_height(&egui::TextStyle::Body);
            let gap = egui::vec2(ui.spacing().item_spacing.x, 3.0);
            Grid::new("stats").striped(true).num_columns(1 + cols.len()).min_row_height(row).spacing(gap).show(
                ui,
                |ui| {
                    ui.strong("Metric");
                    for c in &cols {
                        ui.horizontal(|ui| {
                            if (c.pin.is_some() || c.roi)
                                && ui
                                    .small_button("x")
                                    .on_hover_text(if c.roi { "Remove the ROI" } else { "Remove this pin" })
                                    .clicked()
                            {
                                if c.roi {
                                    remove_roi = true;
                                } else {
                                    remove_pin = c.pin;
                                }
                            }
                            ui.colored_label(theme::on_window(c.color), egui::RichText::new(&c.name).strong());
                        });
                    }
                    ui.end_row();
                    if info.stack {
                        // a stack of variables: each band's value (a mean or a trend over different variables
                        // means nothing), the band shown in bold and by its class when it has classes
                        for (k, l) in info.layers.iter().enumerate() {
                            let label = egui::RichText::new(&l.label);
                            if k == t {
                                ui.label(label.strong());
                            } else {
                                ui.weak(label);
                            }
                            for c in &cols {
                                let values = if c.roi { Some(&sr.roi.mean[..]) } else { c.values };
                                let v = values.filter(|_| c.cube.is_none()).and_then(|v| v.get(k)).copied();
                                ui.label(match (v, active_classes) {
                                    (Some(x), Some(ac)) if x.is_finite() && k == t => ac.name_of(x),
                                    (Some(x), _) if x.is_finite() => num(f64::from(x)),
                                    _ => "-".to_string(),
                                });
                            }
                            ui.end_row();
                        }
                    } else if let Some(ac) = active_classes {
                        // categorical: class summaries (majority, changes...) instead of means and trends
                        let summaries: Vec<Vec<(String, String)>> = cols
                            .iter()
                            .map(|c| match (c.classes, c.values) {
                                (Some(cl), Some(v)) => cl.summary(c.cube.unwrap_or(&info), v, c.t),
                                _ => Vec::new(),
                            })
                            .collect();
                        let labels = ac.summary(&info, &[], 0);
                        for (r, (label, _)) in labels.iter().enumerate() {
                            ui.weak(label);
                            for sm in &summaries {
                                ui.label(sm.get(r).map_or("-", |x| x.1.as_str()));
                            }
                            ui.end_row();
                        }
                    } else {
                        for (label, f) in rows {
                            ui.weak(label);
                            for c in &cols {
                                let cube = c.cube.unwrap_or(&info);
                                ui.label(f(c.st, &|i| date(cube, i), &num, unit));
                            }
                            ui.end_row();
                        }
                        // the rows of each Zeit tool fitted on the chart, in the order they first appear
                        for (k, (tool, style)) in self.zeit.chart_tools().into_iter().enumerate() {
                            let fits: Vec<_> = cols.iter().map(|c| c.fits.get(k).and_then(|f| f.result)).collect();
                            let rows: Vec<Vec<(String, String)>> =
                                fits.iter().map(|f| crate::zeit::fit_rows(*f)).collect();
                            let mut names: Vec<&String> = vec![];
                            for r in &rows {
                                for (n, _) in r {
                                    if !names.contains(&n) {
                                        names.push(n);
                                    }
                                }
                            }
                            ui.horizontal(|ui| {
                                crate::zeit::fit_swatch(ui, style);
                                ui.colored_label(
                                    theme::on_window(style.color),
                                    egui::RichText::new(&tool.name).strong(),
                                );
                            });
                            for f in &fits {
                                match f {
                                    Some(Err(e)) => ui.weak("error").on_hover_text(e),
                                    None => ui.weak("..."),
                                    Some(Ok(_)) => ui.label(""),
                                };
                            }
                            ui.end_row();
                            for n in names {
                                ui.weak(n);
                                for r in &rows {
                                    // a long one (a message: "add at least one class pattern...") wrapped, not
                                    // widening its column past the panel
                                    let v = r.iter().find(|(k, _)| k == n).map_or("", |(_, v)| v.as_str());
                                    crate::fit::cell(ui, v, 16.0 * crate::fit::em(ui));
                                }
                                ui.end_row();
                            }
                        }
                        if self.display.mode == janus_gpu::Mode::Diff {
                            // what the map shows, from each series (active layer)
                            let r = self.display.diff_ref_date(t, info.t());
                            ui.weak(format!("Difference vs {}", r.map_or("-", |r| info.layers[r].label.as_str())));
                            for c in &cols {
                                let v = c.values.or(c.roi.then_some(sr.roi.mean.as_slice()));
                                let d = v
                                    .filter(|_| c.cube.is_none())
                                    .and_then(|v| Some(v.get(t)? - v.get(r?)?))
                                    .filter(|d| !d.is_nan());
                                ui.label(d.map_or("-".to_string(), |d| num(f64::from(d))));
                            }
                            ui.end_row();
                        }
                    }
                },
            );
        });
        if let Some(id) = remove_pin {
            self.remove_pin(id);
        }
        if remove_roi {
            self.clear_roi();
        }
    }

    /// The Performance panel (app.cpp, uiPerf): the frame, the startup, the active layer's disk, threads, overview
    /// and on-demand reads (the full-resolution and disk caches and the basemap follow).
    pub fn performance_ui(&mut self, ui: &mut Ui) {
        let f = &self.perf;
        if let Some((p50, p95, max)) = f.summary() {
            ui.label(format!("Frame: {p50:.2} ms of CPU (p95 {p95:.2}, max {max:.2})")).on_hover_text(
                "The interface's CPU time per frame, the last few hundred frames. The app only\n\
                 redraws when something changes (mouse, keyboard, new data); idle, the CPU sleeps.",
            );
        }
        let st = &self.startup;
        let gpu = self.render_state.adapter.get_info();
        ui.label(format!(
            "Startup: window {:.0} ms, first frame {}",
            st.window_ms,
            st.first_frame_ms.map_or("-".into(), |v| format!("{v:.0} ms"))
        ))
        .on_hover_text(format!(
            "Since process start: GDAL {:.0} ms ({}), window + wgpu {:.0} ms, first frame {};\n\
             the map's shaders {} (in the background). GPU: {} ({:?})",
            st.gdal_ms,
            janus_cube::gdal_version(),
            st.window_ms,
            st.first_frame_ms.map_or("-".into(), |v| format!("{v:.0} ms")),
            st.gpu_ms.map_or("-".into(), |v| format!("{v:.0} ms")),
            gpu.name,
            gpu.backend
        ));
        if let Some(s) = &self.session {
            let mb = 1024.0 * 1024.0;
            let disk = if s.info.remote {
                "remote (HTTP range requests)"
            } else if s.s.rotational {
                "spinning HDD"
            } else {
                "SSD / network"
            };
            ui.label(format!("Disk: {disk}"));
            ui.label(format!(
                "Threads: {} background readers + {} interactive",
                s.s.bg().threads(),
                s.s.fg().threads()
            ));
            ui.label(format!("Open (metadata): {:.0} ms", s.open_ms));
            section(ui, "Overview (GPU)");
            let ov = &s.s.overview;
            ui.label(format!("{} x {} x {} dates (1:{:.1})", ov.w, ov.h, ov.t, ov.factor));
            let bytes = s.cube.as_ref().map_or(0, |c| c.bytes());
            ui.label(format!("Memory: {:.0} MB (cube + statistics)", bytes as f64 / mb));
            let secs = ov.build_seconds().unwrap_or(0.0);
            if !ov.complete() {
                ui.label(format!("Loading {}/{}...", ov.layers_done(), ov.t));
            } else if ov.from_cache() {
                ui.label(format!("Read from cache in {secs:.2} s"));
            } else if ov.cached_layers() > 0 {
                ui.label(format!("Built in {secs:.2} s ({} dates resumed from cache)", ov.cached_layers()));
            } else if !ov.cache_error().is_empty() {
                ui.colored_label(
                    theme::warning(),
                    format!("Built in {secs:.2} s, not saved to cache: {}", ov.cache_error()),
                );
            } else {
                ui.label(format!("Built in {secs:.2} s (saved to cache)"));
            }
            if s.s.defer_random_reads() {
                ui.weak("Pins, ROI and detail wait for the build (HDD)");
            }
            if ov.failed() > 0 {
                ui.colored_label(theme::error(), format!("{} dates failed", ov.failed()));
            }
            if let Some(stats) = &s.stats {
                ui.label(format!("Temporal statistics: {:.1} ms", stats.ms)).on_hover_text(format!(
                    "{:.1} ms on the GPU, {:.1} ms reading them back",
                    stats.gpu_ms,
                    stats.ms - stats.gpu_ms
                ));
            }
            section(ui, "On-demand reads");
            let t = &s.tiles;
            ui.label(format!(
                "Tiles: {} on GPU ({:.0} MB), {} reading",
                t.gpu_tiles(),
                t.gpu_bytes() as f64 / mb,
                t.inflight()
            ));
            ui.label(format!("Average tile: {:.1} ms", t.avg_load_ms()));
            let last = self.series.hover.as_ref().and_then(|h| s.exact(h.x, h.y)).map(|r| r.ms);
            ui.label(format!(
                "Exact series ({} dates): {}",
                s.info.t(),
                last.map_or("-".into(), |ms| format!("{ms:.1} ms"))
            ));
            if let Some(ms) = self.series.roi.data.as_ref().and_then(|d| d.ms()) {
                ui.label(format!("ROI: {ms:.0} ms"));
            }
            ui.label(format!("Queue: {} (background), {} (interactive)", s.s.bg().pending(), s.s.fg().pending()));
        }
    }

    /// The Performance panel's end: the processing threads, and the Settings.
    pub fn performance_end_ui(&mut self, ui: &mut Ui) {
        ui.separator();
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let n = self.processing_threads();
        ui.label(format!("Processing threads: {n} of {cores}"));
        if ui
            .add_sized([ui.available_width(), crate::fit::button_height(ui)], egui::Button::new("Settings..."))
            .on_hover_text("Processing threads, overview memory, full-resolution cache (Ctrl+,)")
            .clicked()
        {
            self.settings.show = true;
        }
    }
}

/// A progress bar with buttons after it on one row: the buttons laid from the row's right end (`buttons` adds them
/// right to left, the last one first), the bar taking exactly the room they leave. A row wider than its window
/// makes a window that fits its content grow, and the next frame's row with it: measured instead (the labels'
/// widths, the paddings), the row came out a fraction of a point too wide in the modern interface's font, and the
/// Tasks window grew frame after frame.
pub(crate) fn progress_row<R>(ui: &mut Ui, bar: egui::ProgressBar, buttons: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = buttons(ui);
            ui.add(bar.desired_width(ui.available_width().max(0.0)));
            r
        })
        .inner
    })
    .inner
}

/// A long text without spaces (a path) on one line, cut with "..." to the room it has (its whole on hover):
/// it would not wrap, and would widen its window to its own length.
pub(crate) fn path_label(ui: &mut Ui, text: &str) -> egui::Response {
    // whole on hover, cut or not (egui's own tooltip of a cut label left out: it would show it twice)
    crate::layout_audit::cut_on_purpose(text);
    ui.add(egui::Label::new(egui::RichText::new(text).weak()).truncate().show_tooltip_when_elided(false))
        .on_hover_text(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_detached_window_comes_back_when_closed_or_not_asked_for() {
        let mut d = Detach::default();
        d.seen(10);
        d.start(Some(egui::pos2(5.0, 6.0)), Some(vec2(300.0, 200.0)));
        assert!(d.detached);
        // asked for every frame: stays where it is
        d.seen(11);
        d.seen(12);
        assert!(d.detached);
        d.viewport = Some(egui::ViewportId::from_hash_of("w"));
        // not asked for in frame 13 (closed): back in the main window when asked for again, with nothing kept
        d.seen(14);
        assert!(!d.detached);
        assert_eq!((d.viewport, d.at, d.size), (None, None, None));
        // the Detach button and the OS window's close
        d.start(None, None);
        d.back();
        assert!(!d.detached);
    }

    #[test]
    fn the_state_lives_in_the_context_under_the_windows_id() {
        let ctx = egui::Context::default();
        let id = egui::Id::new("a window");
        assert!(!is_detached(&ctx, id));
        set_detached(&ctx, id, true);
        assert!(is_detached(&ctx, id));
        assert!(!is_detached(&ctx, egui::Id::new("another")));
        set_detached(&ctx, id, false);
        assert!(!is_detached(&ctx, id));
    }
}
