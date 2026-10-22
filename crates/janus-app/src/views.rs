//! Map panels and the swipe (src/app.cpp: newMapView, uiMapViews, uiViewBar, uiMapView, renderView; app_swipe.cpp).
//! A map panel shows one layer next to the main map, at its own date and in its own mode if wanted, always on the
//! same area (they share the main map's view, centred alike). The swipe draws such a view right of a draggable
//! divider over the main map.

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, Ui, vec2};
use janus_gpu::{Gpu, MapFrame, Mode};
use janus_session::ViewRect;

use crate::app::State;
use crate::display::{MODE_ORDER, Range, mode_name, range_label, time_dependent};
use crate::map::{MapView, body_font};
use crate::theme;

/// The GPU target of the swipe ("Map" is 0, the panels 2, 3...).
pub const SWIPE_SLOT: i32 = 1;

/// A view of one layer: a map panel's, or the swipe's comparison.
#[derive(Clone)]
pub struct ViewSetup {
    /// "Map <id>" and its GPU target
    pub id: u32,
    /// the layer shown (its cube's id)
    pub cube: u64,
    /// a date of its own, else the layer's
    pub own_date: bool,
    pub t: usize,
    /// a mode of its own, else the layer's mode, colormap and range
    pub own_mode: bool,
    pub mode: Mode,
    /// the automatic range of its own mode
    pub range: Range,
    range_key: u64,
    /// the basemap alone (no layer), e.g. as the swipe's comparison
    pub basemap_only: bool,
    pub dirty: bool,
    pub detail_level: Option<u32>,
    drawn_size: [u32; 2],
}

impl ViewSetup {
    pub(crate) fn new(id: u32, cube: u64) -> Self {
        ViewSetup {
            id,
            cube,
            own_date: false,
            t: 0,
            own_mode: false,
            mode: Mode::Value,
            range: Range::default(),
            range_key: u64::MAX,
            basemap_only: false,
            dirty: true,
            detail_level: None,
            drawn_size: [0, 0],
        }
    }
}

/// A colour bar: the colormap and its range's ends, as shown.
type ColourBar = (usize, String, String);

/// The swipe: its comparison and the divider (a fraction of the canvas' width).
pub struct Swipe {
    pub view: ViewSetup,
    pub x: f32,
    dragging: bool,
}

/// The parts of a layer a view draws: the active one's live state, or another's own.
struct LayerRef<'a> {
    session: &'a mut crate::session::Session,
    disp: &'a crate::display::Display,
    classes: &'a crate::classes::LayerClasses,
    composite: Option<&'a mut crate::composite::BandComposite>,
    place: &'a crate::layers::Placement,
    name: &'a str,
}

impl State {
    /// The index of the layer showing cube `id`.
    fn layer_of(&self, cube: u64) -> Option<usize> {
        (0..self.layers.len()).find(|&i| self.layer_session(i).is_some_and(|s| s.info.id == cube))
    }

    /// A new view: another layer than the active one if there is one (e.g. NBR next to NDVI), else the same series
    /// at its own date (e.g. 2005 next to 2020).
    fn new_view(&self, id: u32) -> Option<ViewSetup> {
        let s = self.session.as_ref()?;
        let other = (0..self.layers.len()).find(|&i| i != self.active).and_then(|i| self.layer_session(i));
        Some(match other {
            Some(o) => ViewSetup::new(id, o.info.id),
            None => ViewSetup { own_date: true, t: self.display.t.saturating_sub(1), ..ViewSetup::new(id, s.info.id) },
        })
    }

    /// View > New map view (Ctrl+T): docked next to the main map.
    pub fn new_map_view(&mut self) {
        let id = self.next_view;
        if let Some(v) = self.new_view(id) {
            self.next_view += 1;
            self.views.push(v);
            self.views_added.push(id);
        }
    }

    /// A panel closed: its target goes.
    pub fn close_map_view(&mut self, id: u32) {
        self.views.retain(|v| v.id != id);
        if let Some(g) = self.gpu.as_mut() {
            g.release_target(id as i32);
        }
        self.map_textures.remove(&(id as i32));
    }

    /// The swipe on or off (C): the previous comparison, if its layer is still open.
    pub fn toggle_swipe(&mut self) {
        if self.swipe.is_some() || self.session.is_none() {
            self.swipe = None;
            if let Some(g) = self.gpu.as_mut() {
                g.release_target(SWIPE_SLOT);
            }
            self.map_textures.remove(&SWIPE_SLOT);
            return;
        }
        let keep = self.last_swipe.take().filter(|v| self.layer_of(v.cube).is_some());
        let Some(view) = keep.or_else(|| self.new_view(SWIPE_SLOT as u32)) else { return };
        self.swipe = Some(Swipe { view: ViewSetup { dirty: true, ..view }, x: 0.5, dragging: false });
    }

    /// The bar of a map panel (and of the swipe): its layer (or the basemap alone), own date, own mode; and the
    /// automatic range of an own mode.
    fn view_bar(&mut self, ui: &mut Ui, v: &mut ViewSetup) {
        let i = match self.layer_of(v.cube) {
            Some(i) => i,
            None => {
                // its layer was closed: the active one
                v.cube = self.session.as_ref().map_or(0, |s| s.info.id);
                v.dirty = true;
                self.active
            }
        };
        let names: Vec<(u64, String)> = (0..self.layers.len())
            .filter_map(|k| Some((self.layer_session(k)?.info.id, self.layers[k].name.clone())))
            .collect();
        let basemap = self.basemap.ui.on && self.basemap.ui.source != "none";
        if v.basemap_only && !basemap {
            v.basemap_only = false;
            v.dirty = true;
        }
        let Some(s) = self.layer_session(i) else { return };
        let info = s.info.clone();
        let n = info.t();
        let name = self.layers[i].name.clone();
        let classes_on = if i == self.active { self.classes.on() } else { self.layers[i].classes.on() };
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt(("view layer", v.id))
                .width(170.0)
                .selected_text(if v.basemap_only { "Basemap only".to_string() } else { name.clone() })
                .show_ui(ui, |ui| {
                    for (cube, nm) in &names {
                        if ui.selectable_label(*cube == v.cube && !v.basemap_only, nm).clicked() {
                            v.cube = *cube;
                            v.basemap_only = false;
                            v.range_key = u64::MAX;
                            v.dirty = true;
                        }
                    }
                    if basemap && ui.selectable_label(v.basemap_only, "Basemap only").clicked() {
                        v.basemap_only = true;
                        v.dirty = true;
                    }
                })
                .response
                .on_hover_text("Layer shown in this panel (or the basemap alone)");
            if v.basemap_only {
                return;
            }
            v.dirty |= ui
                .checkbox(&mut v.own_date, "Own date")
                .on_hover_text("Off: the layer's date (the main time bar)")
                .changed();
            v.t = v.t.min(n - 1);
            if v.own_date {
                let label = info.layers[v.t].label.clone();
                ui.spacing_mut().slider_width = (ui.available_width() * 0.35).max(90.0);
                v.dirty |= ui.add(egui::Slider::new(&mut v.t, 0..=n - 1).show_value(false)).changed();
                ui.label(label);
            }
            if ui
                .checkbox(&mut v.own_mode, "Own mode")
                .on_hover_text("Off: the layer's display mode, colormap and range")
                .changed()
            {
                v.range_key = u64::MAX;
                v.dirty = true;
            }
            let available =
                |m: Mode| if classes_on { m == Mode::Value } else { m == Mode::Value || m == Mode::Rgb || n >= 2 };
            if v.own_mode {
                if !available(v.mode) {
                    v.mode = Mode::Value;
                }
                egui::ComboBox::from_id_salt(("view mode", v.id))
                    .width(ui.available_width().max(90.0))
                    .selected_text(mode_name(v.mode))
                    .show_ui(ui, |ui| {
                        for m in MODE_ORDER {
                            if m != Mode::Rgb
                                && available(m)
                                && ui.selectable_label(m == v.mode, mode_name(m)).clicked()
                            {
                                v.mode = m;
                                v.range_key = u64::MAX;
                                v.dirty = true;
                            }
                        }
                    });
            }
        });
        // the automatic range of an own mode (the layer's ranges are its own business)
        if v.own_mode && !v.basemap_only && !(v.mode.needs_stats() && s.stats.is_none()) {
            let disp = if i == self.active { &self.display } else { &self.layers[i].disp };
            let key = ((s.s.overview.layers_done() as u64) * 2 + u64::from(s.stats.is_some())) * 65536
                + (disp.diff_ref + 2) as u64
                + s.stats_gen * 7919;
            if v.range_key != key
                && let Some((lo, hi)) = disp.auto_range_of(v.mode, s)
            {
                v.range.lo = lo;
                v.range.hi = hi;
                v.range_key = key;
                v.dirty = true;
            }
        }
    }

    /// A view drawn into its target (slot `v.id`, the main map's view centred on a canvas of `size` pixels).
    pub(crate) fn render_view(&mut self, v: &mut ViewSetup, size: [u32; 2], ppp: f32, slot: i32) {
        let Some(g) = self.grid() else { return };
        let (fx, fy) = (g.fx, g.fy);
        let i = self.layer_of(v.cube);
        let State { session, display, classes, composite, layers, gpu, basemap, map, active, .. } = self;
        let Some(gpu) = gpu.as_mut() else { return };
        let m: &MapView = map;
        // the active layer's source pixels -> this view's target pixels
        let half = [size[0] as f64 / 2.0 / f64::from(ppp), size[1] as f64 / 2.0 / f64::from(ppp)];
        let to_map = |q: [f64; 4]| -> [f32; 4] {
            let p = |x: f64, y: f64| {
                [
                    ((half[0] + (x / fx - m.center[0]) * m.scale) * f64::from(ppp)) as f32,
                    ((half[1] + (y / fy - m.center[1]) * m.scale) * f64::from(ppp)) as f32,
                ]
            };
            let (a, b) = (p(q[0], q[1]), p(q[2], q[3]));
            [a[0], a[1], b[0], b[1]]
        };
        // the view in map space (the active layer's pixels), at target pixels per map pixel
        let view = ViewRect {
            x0: (m.center[0] - half[0] / m.scale) * fx,
            y0: (m.center[1] - half[1] / m.scale) * fy,
            x1: (m.center[0] + half[0] / m.scale) * fx,
            y1: (m.center[1] + half[1] / m.scale) * fy,
            scale: m.scale * f64::from(ppp) / fx,
        };
        let bg = theme::map_background();
        let mut f = gpu.begin_map(slot, size, bg);
        basemap.draw(gpu, &mut f, &view, bg, |q| to_map(*q));
        let layer = match i {
            Some(i) if i == *active => session.as_mut().map(|s| LayerRef {
                session: s,
                disp: display,
                classes,
                composite: composite.as_mut(),
                place: &layers[i].place,
                name: &layers[i].name,
            }),
            Some(i) => {
                let l = &mut layers[i];
                l.session.as_mut().map(|s| LayerRef {
                    session: s,
                    disp: &l.disp,
                    classes: &l.classes,
                    composite: l.composite.as_mut(),
                    place: &l.place,
                    name: &l.name,
                })
            }
            None => None,
        };
        v.detail_level = None;
        if let Some(l) = layer.filter(|l| l.place.aligned && !v.basemap_only) {
            draw_view_layer(gpu, &mut f, v, l, &view, &to_map);
        }
        gpu.end_map(f);
        v.drawn_size = size;
        v.dirty = false;
    }

    /// A map panel ("Map <id>"): its bar, then the main map's view of its layer.
    pub(crate) fn map_view_ui(&mut self, ui: &mut Ui, id: u32) {
        let Some(k) = self.views.iter().position(|v| v.id == id) else { return };
        if self.session.is_none() {
            ui.weak("No series open.");
            return;
        }
        let mut v = self.views[k].clone();
        self.view_bar(ui, &mut v);
        let (rect, resp) = ui.allocate_exact_size(ui.available_size().max(vec2(64.0, 64.0)), Sense::click_and_drag());
        if resp.hovered() {
            self.map.pointer_panel = Some(id);
        }
        let inside = self.canvas_input(ui, rect, &resp, false);
        let ppp = ui.ctx().pixels_per_point();
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        let Some((texture, remade)) = self.target_texture(id as i32, size) else { return };
        if remade || v.drawn_size != size || self.map.redrawn {
            v.dirty = true;
        }
        if v.dirty || self.view_changed(&v) {
            self.render_view(&mut v, size, ppp, id as i32);
        }
        let painter = ui.painter_at(rect);
        painter.image(texture, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        self.map_marks(&painter, rect, inside, id);
        self.basemap_note(&painter, rect);
        let (title, bar, wait) = self.view_title(&v);
        State::canvas_title(&painter, rect, &title);
        if let Some(w) = wait {
            painter.text(rect.center(), Align2::CENTER_CENTER, w, body_font(&painter), Color32::from_gray(220));
        }
        if let Some((cmap, lo, hi)) = bar {
            State::colour_bar(&painter, rect, cmap, lo, hi);
        }
        if let Some(slot) = self.views.iter_mut().find(|x| x.id == id) {
            *slot = v;
        }
    }

    /// Whether what a view draws changed since it was drawn (its layer's tiles arrived, its date moved).
    fn view_changed(&self, v: &ViewSetup) -> bool {
        // the layer's tiles and the main map's redraws reach the view through `dirty`; a playing date moves it
        !v.own_date && self.display.playing
    }

    /// A view's title, its colour bar (colormap, the range's ends) and what it waits for.
    fn view_title(&self, v: &ViewSetup) -> (String, Option<ColourBar>, Option<&'static str>) {
        if v.basemap_only {
            return (format!("Basemap: {}", self.basemap.ui.source().name), None, None);
        }
        let Some(i) = self.layer_of(v.cube) else { return (String::new(), None, None) };
        let Some(s) = self.layer_session(i) else { return (String::new(), None, None) };
        let info = &s.info;
        let n = info.t();
        let disp = if i == self.active { &self.display } else { &self.layers[i].disp };
        let classes_on = if i == self.active { self.classes.on() } else { self.layers[i].classes.on() };
        let name = &self.layers[i].name;
        let mode = if v.own_mode { v.mode } else { disp.mode };
        let t = if v.own_date { v.t.min(n - 1) } else { disp.t.min(n - 1) };
        let label = |t: usize| info.layers[t].label.as_str();
        let title = match mode {
            Mode::Rgb => format!("{name}  |  RGB"),
            Mode::Diff => {
                format!("{name}  |  {} - {}  |  Difference", label(t), disp.diff_ref_date(t, n).map_or("(none)", label))
            }
            Mode::Composite => format!("{name}  |  {}", disp.temporal_text(info, t)),
            m if time_dependent(m) => format!("{name}  |  {}  |  {}", label(t), mode_name(m)),
            m => format!("{name}  |  {}", mode_name(m)),
        };
        let wait = (mode.needs_stats() && s.stats.is_none())
            .then_some("Computing statistics: waiting for the complete overview...");
        let range = if v.own_mode { v.range } else { disp.ranges[mode as usize] };
        let bar = (mode != Mode::Rgb && !classes_on)
            .then(|| (disp.cmap[mode as usize], range_label(mode, range.lo, info), range_label(mode, range.hi, info)));
        (title, bar, wait)
    }

    /// The swipe's bar, a row between the time bar and the map.
    pub(crate) fn swipe_bar(&mut self, ui: &mut Ui) {
        let Some(mut sw) = self.swipe.take() else { return };
        let mut off = false;
        ui.horizontal(|ui| {
            off = ui
                .button("Swipe off")
                .on_hover_text("Swipe (C): the map left of the divider, this comparison right of it")
                .clicked();
            if !off {
                self.view_bar(ui, &mut sw.view);
            }
        });
        if off {
            self.last_swipe = Some(sw.view);
            if let Some(g) = self.gpu.as_mut() {
                g.release_target(SWIPE_SLOT);
            }
            self.map_textures.remove(&SWIPE_SLOT);
        } else {
            self.swipe = Some(sw);
        }
    }

    /// Dragging the divider (main map only); true while it has the pointer.
    pub(crate) fn swipe_input(&mut self, ui: &Ui, rect: Rect, resp: &egui::Response) -> bool {
        let Some(sw) = self.swipe.as_mut() else { return false };
        let x = rect.min.x + rect.width() * sw.x;
        let near = resp.hover_pos().is_some_and(|p| (p.x - x).abs() <= 6.0);
        if near || sw.dragging {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
        if resp.drag_started() && near && !shift && !ctrl {
            sw.dragging = true;
        }
        if !sw.dragging {
            return false;
        }
        if resp.dragged()
            && let Some(p) = resp.interact_pointer_pos()
        {
            sw.x = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
        } else {
            sw.dragging = false; // released
        }
        true
    }

    /// The swipe over the main map: the comparison right of the divider (the same view, so the same texture
    /// coordinates), the divider and its handle, what it shows at the top right.
    pub(crate) fn draw_swipe(&mut self, ui: &Ui, painter: &egui::Painter, rect: Rect, ppp: f32) {
        let Some(mut sw) = self.swipe.take() else { return };
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        if let Some((texture, remade)) = self.target_texture(SWIPE_SLOT, size) {
            if remade || sw.view.drawn_size != size || self.map.redrawn {
                sw.view.dirty = true;
            }
            if sw.view.dirty || self.view_changed(&sw.view) {
                self.render_view(&mut sw.view, size, ppp, SWIPE_SLOT);
            }
            let x = rect.min.x + rect.width() * sw.x;
            painter.image(
                texture,
                Rect::from_min_max(Pos2::new(x, rect.min.y), rect.max),
                Rect::from_min_max(Pos2::new(sw.x, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            painter.line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                Stroke::new(3.0, Color32::from_black_alpha(200)),
            );
            painter.line_segment(
                [Pos2::new(x, rect.min.y), Pos2::new(x, rect.max.y)],
                Stroke::new(1.0, Color32::from_white_alpha(230)),
            );
            let c = Pos2::new(x, rect.center().y);
            painter.circle_filled(c, 12.0, Color32::from_black_alpha(180));
            painter.circle_stroke(c, 12.0, Stroke::new(1.5, Color32::from_white_alpha(230)));
            let white = Color32::from_white_alpha(230);
            painter.add(egui::Shape::convex_polygon(
                vec![c + vec2(-3.0, -5.0), c + vec2(-3.0, 5.0), c + vec2(-8.0, 0.0)],
                white,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![c + vec2(3.0, -5.0), c + vec2(8.0, 0.0), c + vec2(3.0, 5.0)],
                white,
                Stroke::NONE,
            ));
            // what the right side shows, at the top right (below the overview's progress bar)
            let (title, _, _) = self.view_title(&sw.view);
            let font = body_font(painter);
            let galley = painter.layout_no_wrap(title, font, Color32::WHITE);
            let complete = self.session.as_ref().is_some_and(|s| s.s.overview.complete());
            let tp = rect.min
                + vec2(
                    (x - rect.min.x + 10.0).max(rect.width() - galley.size().x - 10.0),
                    if complete { 8.0 } else { 34.0 },
                );
            painter.galley(tp + vec2(1.0, 1.0), galley.clone(), Color32::from_black_alpha(200));
            painter.galley_with_override_text_color(tp, galley, Color32::WHITE);
        }
        let _ = ui;
        self.swipe = Some(sw);
    }
}

/// One layer of a view: its own date and mode, or the layer's; three bands as RGB if so drawn; the detail tiles of
/// the view in the layer's own pixels.
fn draw_view_layer(
    gpu: &Gpu,
    f: &mut MapFrame,
    v: &mut ViewSetup,
    l: LayerRef,
    view: &ViewRect,
    to_map: &dyn Fn([f64; 4]) -> [f32; 4],
) {
    let s = l.session;
    let info = s.info.clone();
    let n = info.t();
    let d = l.disp;
    let mode = if v.own_mode { v.mode } else { d.mode };
    let t = if v.own_date { v.t.min(n - 1) } else { d.t.min(n - 1) };
    if let Some(c) = l.composite.filter(|c| c.on && !v.own_mode) {
        let place = l.place;
        c.draw(t, n, |tex, w| {
            if let Some((q, warp)) = place.quad(&info, w[0], w[1], w[2], w[3]) {
                gpu.draw_image(f, tex, to_map(q), 1.0, warp.as_ref(), true);
            }
        });
        let _ = l.name;
        return;
    }
    // the layer's parameters with this view's date and mode
    let mut params = d.layer_params(s);
    params.mode = mode;
    match mode {
        Mode::Diff => params.layers = [t as i32, d.diff_ref_date(t, n).map_or(t as i32, |r| r as i32), 0],
        Mode::Rgb => {}
        Mode::Composite => {
            let (a, b) = d.temporal_window(&info, t);
            params.layers = [a as i32, b as i32, d.reducer];
        }
        _ => params.layers = [t as i32, 0, 0],
    }
    let range = if v.own_mode { v.range } else { d.ranges[mode as usize] };
    params.range = [range.lo, range.hi];
    if l.classes.on() && mode == Mode::Value {
        params.class_lut = l.classes.lut.clone();
    }
    let cmap = d.cmap[mode as usize];
    let up = |k: i32| s.uploaded.get(k as usize).copied().unwrap_or(false);
    let ready = match mode {
        Mode::Rgb => params.layers.iter().all(|&k| up(k)),
        Mode::Diff => up(t as i32) && d.diff_ref_date(t, n).is_some_and(|r| up(r as i32)),
        Mode::Composite => (params.layers[0]..=params.layers[1]).all(up),
        m if m.needs_stats() => s.stats.is_some(),
        _ => up(t as i32),
    };
    let Some(cube) = &s.cube else { return };
    if ready && let Some((q, warp)) = l.place.quad(&info, 0.0, 0.0, info.width as f64, info.height as f64) {
        params.warp = warp;
        gpu.draw_cube(f, cube, to_map(q), &params, cmap, 1.0);
    }
    // this view's area in the layer's own pixels: its detail tiles
    let detail = d.detail
        && matches!(mode, Mode::Value | Mode::Diff)
        && (mode != Mode::Diff || ready)
        && !s.s.defer_random_reads();
    if detail {
        let lv = l.place.view(view.x0, view.y0, view.x1, view.y1, view.scale);
        v.detail_level = s.tiles.update(s.s.fg(), t, &lv, None);
        if mode == Mode::Diff
            && let Some(r) = d.diff_ref_date(t, n)
        {
            s.tiles.update(s.s.fg(), r, &lv, None);
        }
        let tiles: Vec<_> = if mode == Mode::Diff {
            let r = d.diff_ref_date(t, n).unwrap_or(t);
            s.tiles.visible_pairs(t, r, &lv).into_iter().map(|(a, b)| (a, Some(b))).collect()
        } else {
            s.tiles.visible(t, &lv).into_iter().map(|a| (a, None)).collect()
        };
        for (tile, reference) in tiles {
            if let Some((q, warp)) = l.place.quad(&info, tile.x, tile.y, tile.x + tile.sw, tile.y + tile.sh) {
                params.tile2 = reference.map(|r| r.tex.clone());
                params.warp = warp;
                gpu.draw_tile(f, cube, &tile.tex, to_map(q), &params, cmap, 1.0);
            }
        }
    }
}
