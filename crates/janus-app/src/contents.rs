//! The Layers panel as a table of contents: what the map draws, top of the list on top, series, the maps the runs
//! made and vector layers in one order (order.rs). Sections that fold, a row a layer (visibility, a swatch, its name,
//! a short detail, a menu), the details of a layer when it is unfolded (its dates, a map's colours), every action in
//! the row's menu (right click, or its "..." button): make it active, opacity, move, export, close. Any row is
//! dragged onto another to reorder; a filter by name keeps a long list usable.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Shape, Stroke, Ui, vec2};
use janus_geo::vectors::write_vector_table;

use crate::app::State;
use crate::order::Item;
use crate::theme;

/// A layer's row in the selection: a series by its cube's id, a vector layer or a run's map by its id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Sel {
    Series(u64),
    Vector(u32),
    Result(u32),
}

/// The panel's own state: the filter, the rows unfolded, the rows selected.
#[derive(Default)]
pub struct ContentsUi {
    pub filter: String,
    /// rows unfolded (by key: "s" + cube id, "r" + a result's file); `folded`: the ones folded by hand
    expanded: HashSet<String>,
    folded: HashSet<String>,
    /// rows selected (a click; Ctrl + click adds or takes one, Shift + click a run of them): Delete or Ctrl+D closes
    /// them; `anchor`: where a run starts
    pub(crate) selected: HashSet<Sel>,
    anchor: Option<Sel>,
    /// the layers' rows as last drawn, top to bottom (Shift + click's run), and the ones being drawn
    pub(crate) order: Vec<Sel>,
    drawing: Vec<Sel>,
    /// the last click was on the list: Delete and Ctrl+D are its keys
    pub(crate) focused: bool,
    /// the rows being drawn are a group's: indented
    indent: f32,
    /// a layer's style copied (Copy style), pasted on others
    pub(crate) style_clip: Option<crate::style::LayerStyle>,
    /// the name a style is saved under (its menu's field); a group being renamed
    style_name: String,
    group_name: Option<(u32, String)>,
}

impl ContentsUi {
    /// A click on a layer's row, with the keys held: the selection as a file manager's.
    pub(crate) fn click(&mut self, s: Sel, m: egui::Modifiers) {
        self.focused = true;
        if m.shift
            && let Some(a) = self.anchor
            && let (Some(i), Some(j)) =
                (self.order.iter().position(|&x| x == a), self.order.iter().position(|&x| x == s))
        {
            if !m.command {
                self.selected.clear();
            }
            self.selected.extend(self.order[i.min(j)..=i.max(j)].iter().copied());
            return;
        }
        if !m.command {
            self.selected.clear();
            self.selected.insert(s);
        } else if !self.selected.remove(&s) {
            self.selected.insert(s);
        }
        self.anchor = Some(s);
    }

    /// The row's Close item: the selection when the row is in it with others (how many), else the row alone.
    fn closes_selection(&self, s: Sel) -> Option<usize> {
        (self.selected.len() > 1 && self.selected.contains(&s)).then_some(self.selected.len())
    }

    fn is_open(&self, key: &str, default: bool) -> bool {
        if default { !self.folded.contains(key) } else { self.expanded.contains(key) }
    }

    fn toggle(&mut self, key: &str, default: bool) {
        let set = if default { &mut self.folded } else { &mut self.expanded };
        if !set.remove(key) {
            set.insert(key.to_string());
        }
    }

    fn shows(&self, name: &str) -> bool {
        let f = self.filter.trim();
        f.is_empty() || name.to_lowercase().contains(&f.to_lowercase())
    }
}

/// What a row shows at its left: a small picture of the layer.
pub(crate) enum Swatch {
    /// a colormap's ramp
    Ramp(usize),
    /// a series: a stack of frames, the front one in its colormap (none: drawn as red, green and blue)
    Stack(Option<usize>),
    /// a vector layer: its kind of geometry in its colour
    Vector { color: Color32, points: bool, lines: bool, areas: bool },
    /// the basemap: tiles of a map
    Basemap,
    /// a group of layers: a folder
    Group,
}

/// A row's content.
pub(crate) struct Row<'a> {
    pub name: &'a str,
    /// the second line, in the weak colour
    pub detail: String,
    /// on the right of the name (the date shown)
    pub badge: String,
    pub active: bool,
    /// in the selection (outlined)
    pub selected: bool,
    pub warn: Option<String>,
    pub indent: f32,
    pub swatch: Swatch,
    /// Some(unfolded) when it unfolds
    pub fold: Option<bool>,
}

/// What a row's widgets did: the row (click, drag, right click), visibility, folding, the menu's button.
pub(crate) struct RowOut {
    pub row: Response,
    pub visible_changed: bool,
    pub fold_clicked: bool,
    pub more: Response,
}

fn paint_swatch(ui: &Ui, rect: Rect, s: &Swatch, dim: bool) {
    let p = ui.painter();
    let v = ui.visuals();
    let border = Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color);
    let fade = |c: Color32| if dim { c.gamma_multiply(0.4) } else { c };
    let ramp = |rect: Rect, k: usize| {
        let cm = &janus_gpu::colormaps::all()[k];
        let n = 8;
        for i in 0..n {
            let [r, g, b, _] = cm.sample((i as f32 + 0.5) / n as f32);
            let x0 = rect.left() + rect.width() * i as f32 / n as f32;
            let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
            p.rect_filled(Rect::from_x_y_ranges(x0..=x1, rect.y_range()), 0.0, fade(Color32::from_rgb(r, g, b)));
        }
    };
    match s {
        Swatch::Ramp(k) => {
            ramp(rect, *k);
            p.rect_stroke(rect, 0.0, border, egui::StrokeKind::Inside);
        }
        Swatch::Stack(cmap) => {
            // two frames behind the front one: a stack of dates
            let front = Rect::from_min_max(rect.min + vec2(0.0, 4.0), rect.max - vec2(4.0, 0.0));
            for k in [2.0, 1.0] {
                let r = front.translate(vec2(2.0 * k, -2.0 * k));
                p.rect_filled(r, 1.0, v.extreme_bg_color);
                p.rect_stroke(r, 1.0, Stroke::new(1.0, fade(v.weak_text_color())), egui::StrokeKind::Inside);
            }
            match cmap {
                Some(k) => ramp(front, *k),
                None => {
                    let w = front.width() / 3.0;
                    for (i, c) in [(200, 70, 60), (80, 170, 90), (70, 110, 210)].into_iter().enumerate() {
                        let x0 = front.left() + w * i as f32;
                        let r = Rect::from_x_y_ranges(x0..=x0 + w, front.y_range());
                        p.rect_filled(r, 0.0, fade(Color32::from_rgb(c.0, c.1, c.2)));
                    }
                }
            }
            let edge = Stroke::new(1.0, fade(v.text_color().gamma_multiply(0.8)));
            p.rect_stroke(front, 1.0, edge, egui::StrokeKind::Inside);
        }
        Swatch::Vector { color, points, lines, areas } => {
            let c = fade(*color);
            p.rect_filled(rect, 2.0, v.extreme_bg_color);
            p.rect_stroke(rect, 2.0, border, egui::StrokeKind::Inside);
            let r = rect.shrink(4.0);
            let at = |x: f32, y: f32| r.lerp_inside(vec2(x, y));
            if *areas {
                let pts = vec![at(0.1, 0.35), at(0.55, 0.0), at(1.0, 0.45), at(0.7, 1.0), at(0.15, 0.9)];
                p.add(Shape::convex_polygon(pts, c.gamma_multiply(0.45), Stroke::new(1.2, c)));
            } else if *lines {
                let pts = vec![at(0.0, 0.85), at(0.3, 0.3), at(0.6, 0.7), at(1.0, 0.1)];
                p.add(Shape::line(pts, Stroke::new(1.6, c)));
            } else if *points {
                for (x, y) in [(0.15, 0.7), (0.5, 0.25), (0.85, 0.65)] {
                    p.circle_filled(at(x, y), 2.2, c);
                }
            }
        }
        Swatch::Group => {
            let c = fade(v.text_color().gamma_multiply(0.75));
            let body = Rect::from_min_max(rect.min + vec2(1.0, 4.0), rect.max - vec2(1.0, 1.0));
            let tab = Rect::from_min_size(rect.min + vec2(1.0, 1.0), vec2(rect.width() * 0.4, 5.0));
            p.rect_filled(tab, 1.5, c.gamma_multiply(0.5));
            p.rect_filled(body, 2.0, c.gamma_multiply(0.35));
            p.rect_stroke(body, 2.0, Stroke::new(1.0, c), egui::StrokeKind::Inside);
        }
        Swatch::Basemap => {
            // a map's tiles: land, a river, the tiles' edges
            p.rect_filled(rect, 2.0, fade(Color32::from_rgb(86, 120, 92)));
            let at = |x: f32, y: f32| rect.lerp_inside(vec2(x, y));
            let river = vec![at(0.0, 0.3), at(0.35, 0.55), at(0.6, 0.4), at(1.0, 0.75)];
            p.add(Shape::line(river, Stroke::new(2.0, fade(Color32::from_rgb(110, 160, 205)))));
            let grid = Stroke::new(1.0, Color32::from_white_alpha(if dim { 25 } else { 60 }));
            p.vline(rect.center().x, rect.y_range(), grid);
            p.hline(rect.x_range(), rect.center().y, grid);
            p.rect_stroke(rect, 2.0, border, egui::StrokeKind::Inside);
        }
    }
}

/// Visibility as an eye: open when shown, shut when hidden.
fn paint_eye(ui: &Ui, c: egui::Pos2, open: bool, color: Color32) {
    let p = ui.painter();
    let (w, h) = (6.5, 4.0);
    let arc = |sign: f32| -> Vec<egui::Pos2> {
        (0..=10)
            .map(|k| {
                let t = k as f32 / 5.0 - 1.0;
                egui::pos2(c.x + t * w, c.y + sign * h * (1.0 - t * t))
            })
            .collect()
    };
    let stroke = Stroke::new(1.3, color);
    if open {
        p.add(Shape::line(arc(-1.0), stroke));
        p.add(Shape::line(arc(1.0), stroke));
        p.circle_filled(c, 2.0, color);
    } else {
        // shut: the lower lid and its lashes
        let lid = arc(1.0);
        for k in [2, 5, 8] {
            let a = lid[k];
            p.line_segment([a, a + vec2((k as f32 - 5.0) * 0.4, 2.5)], stroke);
        }
        p.add(Shape::line(lid, stroke));
    }
}

/// One row of the contents, as an image editor's layer: [eye] [fold] [picture] name (badge) [!] [...], its detail
/// under the name. The menu's button shows on the row under the pointer and on the active one.
pub(crate) fn row(ui: &mut Ui, visible: Option<&mut bool>, r: &Row) -> RowOut {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let small = egui::TextStyle::Small.resolve(ui.style());
    let (line, line_small) = (ui.fonts_mut(|f| f.row_height(&body)), ui.fonts_mut(|f| f.row_height(&small)));
    let two = !r.detail.is_empty();
    let h = if two { line + line_small + 12.0 } else { (line + 12.0).max(ui.spacing().interact_size.y + 4.0) };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
    let v = ui.visuals().clone();
    let id = resp.id;
    let shown = visible.as_ref().is_none_or(|v| **v);
    // the background: the active row and the selected ones tinted, a bar of the accent at the active one's left, the
    // selected ones outlined; the row under the pointer lighter
    if r.active || r.selected {
        ui.painter().rect_filled(rect, 3.0, v.selection.bg_fill.gamma_multiply(0.30));
    } else if resp.hovered() || resp.context_menu_opened() {
        ui.painter().rect_filled(rect, 3.0, v.widgets.hovered.weak_bg_fill.gamma_multiply(0.55));
    }
    if r.selected {
        let edge = Stroke::new(1.0, theme::accent().gamma_multiply(0.8));
        ui.painter().rect_stroke(rect, 3.0, edge, egui::StrokeKind::Inside);
    }
    if r.active {
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 1.0, theme::accent());
    }
    let mut out = RowOut { row: resp.clone(), visible_changed: false, fold_clicked: false, more: resp.clone() };
    let mid = rect.center().y;
    let mut x = rect.left() + 6.0 + r.indent;
    // the eye
    if let Some(vis) = visible {
        let eye = Rect::from_center_size(egui::pos2(x + 9.0, mid), vec2(20.0, 20.0));
        let e = ui.interact(eye, id.with("eye"), Sense::click());
        let base = if *vis { v.text_color() } else { v.weak_text_color().gamma_multiply(0.6) };
        let c = if e.hovered() { v.strong_text_color() } else { base };
        paint_eye(ui, eye.center(), *vis, c);
        if e.on_hover_text(if *vis { "Hide" } else { "Show" }).clicked() {
            *vis = !*vis;
            out.visible_changed = true;
        }
    }
    x += 22.0;
    // the fold
    if let Some(open) = r.fold {
        let ar = Rect::from_center_size(egui::pos2(x + 5.0, mid), vec2(12.0, 18.0));
        let a = ui.interact(ar, id.with("fold"), Sense::click());
        let c = if a.hovered() { v.strong_text_color() } else { v.weak_text_color() };
        let m = ar.center();
        let pts = if open {
            vec![m + vec2(-3.5, -1.5), m + vec2(3.5, -1.5), m + vec2(0.0, 2.5)]
        } else {
            vec![m + vec2(-1.5, -3.5), m + vec2(2.5, 0.0), m + vec2(-1.5, 3.5)]
        };
        ui.painter().add(Shape::convex_polygon(pts, c, Stroke::NONE));
        out.fold_clicked = a.on_hover_text(if open { "Fold" } else { "Unfold" }).clicked();
    }
    x += 14.0;
    // the picture
    let ph = (h - 12.0).clamp(16.0, 24.0);
    let pic = Rect::from_min_size(egui::pos2(x, mid - ph * 0.5), vec2(ph * 1.35, ph));
    paint_swatch(ui, pic, &r.swatch, !shown);
    x = pic.right() + 8.0;
    // right: the menu's button, a warning, the badge
    let mut right = rect.right() - 4.0;
    let dots = Rect::from_min_max(egui::pos2(right - 20.0, rect.top() + 2.0), egui::pos2(right, rect.bottom() - 2.0));
    let more = ui.interact(dots, id.with("more"), Sense::click());
    let menu_open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&more));
    if r.active || resp.hovered() || more.hovered() || menu_open || resp.context_menu_opened() {
        let dc = if more.hovered() || menu_open { v.strong_text_color() } else { v.weak_text_color() };
        if more.hovered() {
            ui.painter().rect_filled(dots.shrink2(vec2(0.0, 3.0)), 3.0, v.widgets.hovered.weak_bg_fill);
        }
        for k in -1..=1 {
            ui.painter().circle_filled(dots.center() + vec2(k as f32 * 4.0, 0.0), 1.4, dc);
        }
    }
    out.more = more.on_hover_text("More (or right click the row)");
    right = dots.left() - 2.0;
    let name_y = if two { rect.top() + 6.0 } else { mid - line * 0.5 };
    if let Some(w) = &r.warn {
        let c = egui::pos2(right - 7.0, name_y + line * 0.5);
        let tri = vec![c + vec2(0.0, -6.0), c + vec2(6.0, 5.0), c + vec2(-6.0, 5.0)];
        ui.painter().add(Shape::convex_polygon(tri, theme::warning(), Stroke::NONE));
        ui.painter().text(c + vec2(0.0, 1.0), egui::Align2::CENTER_CENTER, "!", small.clone(), v.panel_fill);
        ui.interact(Rect::from_center_size(c, vec2(14.0, 14.0)), id.with("warn"), Sense::hover()).on_hover_text(w);
        right -= 16.0;
    }
    let text = |s: &str, font: &egui::FontId, color: Color32, width: f32| {
        let mut job = egui::text::LayoutJob::single_section(
            s.to_string(),
            egui::TextFormat { font_id: font.clone(), color, ..Default::default() },
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(8.0));
        ui.fonts_mut(|f| f.layout_job(job))
    };
    if !r.badge.is_empty() {
        let g = text(&r.badge, &small, v.weak_text_color(), 90.0);
        let at = egui::pos2(right - g.size().x, name_y + (line - g.size().y) * 0.5 + 1.0);
        right = at.x - 6.0;
        ui.painter().galley(at, g, v.weak_text_color());
    }
    // the name, strong on the active row, faint on a hidden one; its detail under it
    let name_c = if r.active {
        v.strong_text_color()
    } else if shown {
        v.text_color()
    } else {
        v.weak_text_color()
    };
    let font = if r.active { theme::strong_font(body.size) } else { body.clone() };
    ui.painter().galley(egui::pos2(x, name_y), text(r.name, &font, name_c, right - x), name_c);
    if two {
        let g = text(&r.detail, &small, v.weak_text_color(), rect.right() - 8.0 - x);
        ui.painter().galley(egui::pos2(x, name_y + line + 1.0), g, v.weak_text_color());
    }
    out
}

/// A row being dragged: its place in the drawing order.
struct Dragged(usize);

/// What the panel asks for, done after it is drawn.
enum Act {
    Activate(usize),
    Remove(usize),
    /// the rows selected
    RemoveSelected,
    /// from, to: places in the drawing order (bottom to top); Drop: dragged onto the row at `to` (its group joined)
    Move(usize, usize),
    Drop(usize, usize),
    CopyStyle(Sel),
    PasteStyle(Sel),
    /// a saved style given to the row (and the others selected), saved from it, deleted
    ApplySaved(Sel, String),
    SaveStyle(Sel, String),
    DeleteSaved(String),
    GroupSelected,
    LeaveGroup(Sel),
    Ungroup(u32),
    /// to the top (true) or the bottom
    MoveGroup(u32, bool),
    GroupVisible(u32, bool),
    SelectGroup(u32),
    CloseGroup(u32),
    Export(usize, usize),
    Fit(usize),
    SaveResult(usize),
    SaveRun(usize),
    RemoveResult(usize),
    /// a run's map shown, every other one hidden
    SoloResult(usize),
    Polygonize(Sel),
    RemoveVector(usize),
    ExtractVector(u32),
    Interpret(u32),
    ZoomVector(usize),
    ExportVector(usize),
    AttributeTable(u32),
    /// a series' date, a stack's band: shown on the map (the layer made active and visible)
    ShowStep(usize, usize),
    HideLayer(usize),
    /// shown, then the export dialog of that kind
    ExportStep(usize, usize, usize),
    OpenStep(usize, usize),
    /// a run's provenance (its folder, or a file in it): the window, run again, its Python, the folder
    Provenance(String),
    RunAgain(String),
    CopyRunPython(String),
    OpenFolder(String),
}

/// A date's or a band's export menu: label, the export dialog's kind (what the map shows at that date).
const STEP_EXPORTS: [(&str, usize); 3] =
    [("Values as GeoTIFF...", 2), ("Map as PNG...", 1), ("Rendered view as GeoTIFF...", 3)];

/// The export menu's items: label, the export dialog's kind.
const EXPORTS: [(&str, usize); 4] = [
    ("Map as PNG...", 1),
    ("Values as GeoTIFF...", 2),
    ("Rendered view as GeoTIFF...", 3),
    ("Animation (GIF, MP4, WebM)...", 5),
];

impl State {
    /// The Layers tab.
    pub(crate) fn contents_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut acts = vec![];
        self.contents_toolbar(ui);
        self.opacity_strip(ui);
        let total = self.layers.iter().filter(|l| !l.canvas).count() + self.vectors.len() + self.zeit.results.len();
        // the list: series and vector layers in one drawing order, the basemap under them all
        list_header(ui, "Layers", &total.to_string()).on_hover_text(
            "Top of the list = drawn on top, series and vector layers alike: drag a row onto another to \
             reorder.\nClick a series to make it active; the eye shows or hides a layer. Ctrl + click, Shift + \
             click: select several; Delete or Ctrl+D closes the ones selected.\nAlt + click a polygon on the map: it \
             becomes the ROI; a line: the transect.",
        );
        let v = ui.visuals();
        let list = egui::Frame::new()
            .fill(v.extreme_bg_color)
            .stroke(v.widgets.noninteractive.bg_stroke)
            .corner_radius(4)
            .inner_margin(2)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 1.0;
                if total == 0 && self.vector_loads.is_empty() && self.opening.is_none() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new("No layers yet: + Add, File > Open, or drop files on the window.").weak(),
                            )
                            .wrap(),
                        );
                    });
                    ui.add_space(8.0);
                }
                self.layer_rows(ui, &mut acts);
                self.basemap_row(ui);
            })
            .response
            .rect;
        // a click elsewhere: Delete and Ctrl+D go back to the map (its pins) and the other panels
        if ui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !list.contains(p))) {
            self.contents.focused = false;
        }
        ui.add_space(6.0);
        egui::CollapsingHeader::new(RichText::new("Add embeddings").strong())
            .id_salt("emb dl layers")
            .show(ui, |ui| self.embedding_download_ui(ui))
            .header_response
            .on_hover_text("Foundation-model embeddings (AlphaEarth, TESSERA) of the visible area,\ndownloaded and added as a layer");

        for a in acts {
            match a {
                Act::Activate(i) => self.set_active(i, &ctx),
                Act::Remove(i) => self.remove_layer(i, &ctx),
                Act::RemoveSelected => self.remove_selected(&ctx),
                Act::Move(from, to) | Act::Drop(from, to) => {
                    let o = self.draw_order();
                    let moved = o.get(from).and_then(|&it| self.sel_of(it));
                    let target = o.get(to).and_then(|&it| self.sel_of(it));
                    self.move_in_order(from, to);
                    if let Some(m) = moved {
                        self.regroup_moved(m, target, matches!(a, Act::Drop(..)));
                    }
                }
                Act::CopyStyle(s) => self.contents.style_clip = self.style_of(s),
                Act::PasteStyle(s) => {
                    if let Some(st) = self.contents.style_clip.clone() {
                        self.paste_style(s, &st);
                    }
                }
                Act::ApplySaved(s, name) => {
                    if let Some((_, st)) = self.saved_styles_for(s).into_iter().find(|x| x.0 == name) {
                        self.paste_style(s, &st);
                    }
                }
                Act::SaveStyle(s, name) => {
                    if let Some(st) = self.style_of(s) {
                        self.save_style(&name, &st);
                    }
                }
                Act::DeleteSaved(name) => self.delete_saved_style(&name),
                Act::GroupSelected => {
                    self.group_selected();
                }
                Act::LeaveGroup(s) => self.set_group(s, None),
                Act::Ungroup(g) => self.ungroup(g),
                Act::MoveGroup(g, top) => self.move_group(g, top),
                Act::GroupVisible(g, v) => self.set_group_visible(g, v),
                Act::SelectGroup(g) => self.select_group(g),
                Act::CloseGroup(g) => {
                    self.select_group(g);
                    self.remove_selected(&ctx);
                }
                Act::Export(i, kind) => {
                    if i != self.active {
                        self.set_active(i, &ctx);
                    }
                    self.open_export(kind);
                }
                Act::Fit(i) => {
                    if i != self.active {
                        self.set_active(i, &ctx);
                    }
                    self.map.fit();
                }
                Act::SaveResult(k) => self.save_result(k, &ctx),
                Act::Polygonize(sel) => self.open_polygonize(sel),
                Act::SaveRun(k) => self.open_save_run(k),
                Act::RemoveResult(k) => {
                    if k < self.zeit.results.len() {
                        self.zeit.results.remove(k);
                        self.map.dirty = true;
                    }
                }
                Act::SoloResult(k) => {
                    for (i, r) in self.zeit.results.iter_mut().enumerate() {
                        r.visible = i == k;
                    }
                    self.map.dirty = true;
                }
                Act::RemoveVector(i) => {
                    self.remove_vector(i);
                    self.map.dirty = true;
                }
                Act::ExtractVector(id) => self.open_extract(id),
                Act::Interpret(id) => self.start_interpretation(id),
                Act::ZoomVector(i) => self.zoom_to_vector(i),
                Act::ExportVector(i) => self.export_vector(i),
                Act::AttributeTable(id) => self.open_attribute_table(id),
                Act::ShowStep(i, t) => self.show_step(i, t, &ctx),
                Act::HideLayer(i) => {
                    self.layers[i].visible = false;
                    self.map.dirty = true;
                }
                Act::ExportStep(i, t, kind) => {
                    self.show_step(i, t, &ctx);
                    self.open_export(kind);
                }
                Act::OpenStep(i, t) => self.open_step_as_layer(i, t, &ctx),
                Act::Provenance(p) => self.show_provenance(&p),
                Act::RunAgain(p) => {
                    match crate::provenance::find(&p).map(|d| crate::provenance::Provenance::read(&d)) {
                        Some(Ok(prov)) => self.run_again(&prov),
                        Some(Err(e)) => self.error_popup = Some(e),
                        None => {}
                    }
                }
                Act::CopyRunPython(p) => {
                    if let Err(e) = self.copy_run_python(&ctx, &p) {
                        self.error_popup = Some(e);
                    }
                }
                Act::OpenFolder(p) => crate::platform::open_in_explorer(&p),
            }
        }
    }

    /// + Add, and the filter once the list is long.
    fn contents_toolbar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let has = !self.layers.is_empty();
        ui.horizontal(|ui| {
            ui.menu_button("+ Add", |ui| {
                ui.weak("A series (as a new layer)");
                if ui.button("Files...").clicked()
                    && let Some(files) = rfd::FileDialog::new().pick_files()
                {
                    self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), has, &ctx);
                }
                if ui.button("Folder (a file per date)...").clicked()
                    && let Some(dir) = rfd::FileDialog::new().pick_folder()
                {
                    self.open_inputs(vec![dir.to_string_lossy().into_owned()], has, &ctx);
                }
                if ui.button("URL (COG, S3, GCS)...").clicked() {
                    self.open_url_dialog(has);
                    ui.close();
                }
                ui.separator();
                if ui
                    .button("Vector file...")
                    .on_hover_text("GeoJSON, Shapefile, GeoPackage, KML, CSV of points (x/lon, y/lat columns)...")
                    .clicked()
                    && let Some(files) = rfd::FileDialog::new().pick_files()
                {
                    self.add_vector_files(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), &ctx);
                }
            });
            let n = self.layers.len() + self.vectors.len() + self.zeit.results.len();
            if n >= 5 || !self.contents.filter.is_empty() {
                ui.add(
                    egui::TextEdit::singleline(&mut self.contents.filter)
                        .hint_text("Filter by name")
                        .desired_width(f32::INFINITY),
                );
            }
        });
        if self.opening.is_some() {
            ui.weak("opening...");
        }
        if !self.vector_loads.is_empty() {
            ui.weak(format!("reading {} vector file(s)...", self.vector_loads.len()));
        }
        ui.add_space(2.0);
    }

    /// The active layer's opacity above the list (an image editor's Opacity).
    fn opacity_strip(&mut self, ui: &mut Ui) {
        if self.session.is_none() {
            return;
        }
        let Some(l) = self.layers.get_mut(self.active).filter(|l| !l.canvas) else { return };
        ui.horizontal(|ui| {
            let blend = l.blend;
            egui::ComboBox::from_id_salt("blend strip")
                .width(76.0)
                .selected_text(blend.name())
                .show_ui(ui, |ui| {
                    for b in janus_gpu::Blend::ALL {
                        if ui.selectable_value(&mut l.blend, b, b.name()).changed() {
                            self.map.dirty = true;
                        }
                    }
                })
                .response
                .on_hover_text(format!("{}: how it combines with the layers under it", l.name));
            ui.label(RichText::new("Opacity").weak());
            ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(40.0);
            let slider = egui::Slider::new(&mut l.opacity, 0.0..=1.0)
                .custom_formatter(|v, _| format!("{:.0} %", v * 100.0))
                .custom_parser(|s| s.trim().trim_end_matches('%').trim().parse::<f64>().ok().map(|v| v / 100.0));
            if ui.add(slider).on_hover_text(format!("{}: its opacity on the map", l.name)).changed() {
                self.map.dirty = true;
            }
        });
        ui.add_space(2.0);
    }

    /// The basemap as the list's last row (it is under every layer): the eye shows or hides it, its source and
    /// opacity unfold under it (a click on the row unfolds it too).
    fn basemap_row(&mut self, ui: &mut Ui) {
        let key = "basemap";
        let open = self.contents.is_open(key, false);
        let presets = janus_geo::basemap::presets();
        let bm = &mut self.basemap.ui;
        let none = bm.source == "none";
        let source = match bm.source.as_str() {
            "none" => "None: nothing under the layers".to_string(),
            "custom" => "Custom XYZ URL".to_string(),
            id => presets.iter().find(|p| p.id == id).map_or("None".into(), |p| p.name.clone()),
        };
        let r = Row {
            name: "Basemap",
            detail: source,
            badge: if none || bm.on { String::new() } else { "hidden".into() },
            active: false,
            selected: false,
            warn: None,
            indent: 0.0,
            swatch: Swatch::Basemap,
            fold: Some(open),
        };
        let mut on = bm.on;
        let out = ui.push_id("basemap row", |ui| row(ui, (!none).then_some(&mut on), &r)).inner;
        if out.visible_changed {
            self.basemap.ui.on = on;
            self.map.dirty = true;
        }
        let row_r = out.row.on_hover_text("Satellite imagery or a map under every layer");
        let mut menu = |ui: &mut Ui| {
            let bm = &mut self.basemap.ui;
            if ui
                .add_enabled(!none, egui::Slider::new(&mut bm.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2))
                .changed()
            {
                self.map.dirty = true;
            }
            if ui.button(if open { "Hide its settings" } else { "Source and settings..." }).clicked() {
                self.contents.toggle(key, false);
                ui.close();
            }
        };
        row_r.context_menu(&mut menu);
        egui::Popup::menu(&out.more).show(&mut menu);
        if out.fold_clicked || row_r.clicked() {
            self.contents.toggle(key, false);
        }
        if self.contents.is_open(key, false) {
            ui.push_id("basemap settings", |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left: 42, right: 6, top: 2, bottom: 6 }).show(
                    ui,
                    |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        self.basemap_ui(ui);
                    },
                );
            });
        }
    }

    /// Every layer's row, top to bottom (the canvas of vector layers alone is not listed).
    fn layer_rows(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let order = self.draw_order();
        self.contents.drawing.clear();
        if !order.is_empty() {
            // the canvas stays at the bottom
            let lo = usize::from(matches!(order.first(), Some(Item::Series(i)) if self.layers[*i].canvas));
            let top = order.len() - 1;
            let sels: Vec<Option<Sel>> = order.iter().map(|&it| self.sel_of(it)).collect();
            self.tidy_groups(&sels.iter().flatten().copied().collect::<Vec<_>>());
            let groups: Vec<Option<u32>> = sels.iter().map(|s| s.and_then(|s| self.group_of(s))).collect();
            // a group's row over each run of its layers, its layers indented under it (none when it is folded)
            let mut run: Option<(u32, bool)> = None;
            for pos in (0..order.len()).rev() {
                let g = groups[pos];
                if g != run.map(|r| r.0) {
                    run = g.map(|g| {
                        let n = (0..=pos).rev().take_while(|&k| groups[k] == Some(g)).count();
                        (g, self.group_row(ui, g, n, pos, acts))
                    });
                }
                if run.is_some_and(|r| !r.1) {
                    continue;
                }
                self.contents.indent = if run.is_some() { 14.0 } else { 0.0 };
                match order[pos] {
                    Item::Series(i) => self.series_row(ui, i, (pos, lo, top), acts),
                    Item::Vector(li) => self.vector_row(ui, li, (pos, lo, top), acts),
                    Item::Result(k) => self.result_row(ui, k, (pos, lo, top), acts),
                }
            }
            self.contents.indent = 0.0;
        }
        // the selection: the rows on the list (a layer closed, or filtered out, leaves it)
        let c = &mut self.contents;
        c.order = std::mem::take(&mut c.drawing);
        c.selected.retain(|s| c.order.contains(s));
        if c.anchor.is_some_and(|a| !c.order.contains(&a)) {
            c.anchor = None;
        }
    }

    /// The layers selected on the list closed, series and vector layers alike (Delete, Ctrl+D, a row's menu).
    pub(crate) fn remove_selected(&mut self, ctx: &egui::Context) {
        let sel = std::mem::take(&mut self.contents.selected);
        self.contents.anchor = None;
        if sel.is_empty() {
            return;
        }
        let series: Vec<usize> = (0..self.layers.len())
            .filter(|&i| {
                !self.layers[i].canvas && self.layer_session(i).is_some_and(|s| sel.contains(&Sel::Series(s.info.id)))
            })
            .collect();
        let vectors: Vec<usize> =
            (0..self.vectors.len()).filter(|&i| sel.contains(&Sel::Vector(self.vectors[i].id))).collect();
        self.zeit.results.retain(|r| !sel.contains(&Sel::Result(r.id)));
        // from the last, the places of the others kept
        for &i in vectors.iter().rev() {
            self.remove_vector(i);
        }
        for &i in series.iter().rev() {
            self.remove_layer(i, ctx);
        }
        self.map.dirty = true;
    }

    fn vector_row(&mut self, ui: &mut Ui, li: usize, place: (usize, usize, usize), acts: &mut Vec<Act>) {
        let has = self.session.is_some();
        let pos = place.0;
        let name = self.vectors[li].name.clone();
        if !self.contents.shows(&name) {
            return;
        }
        let interpreting = self.labels.vector == Some(self.vectors[li].id);
        let extra = self.row_extras(Sel::Vector(self.vectors[li].id));
        let v = &mut self.vectors[li];
        let d = v.data.clone();
        let warn = if !v.why.is_empty() {
            Some(v.why.clone())
        } else if v.off_map {
            Some("nothing of it over the active layer".to_string())
        } else {
            None
        };
        let id = v.id;
        let me = Sel::Vector(id);
        self.contents.drawing.push(me);
        let selected = self.contents.selected.contains(&me);
        let closes = self.contents.closes_selection(me);
        let contents = &mut self.contents;
        let indent = contents.indent;
        ui.push_id(("vector", id), |ui| {
            let r = Row {
                name: &name,
                detail: if v.placing.is_some() { "placing...".into() } else { d.kind_text() },
                badge: if interpreting { "interpreting".into() } else { String::new() },
                active: interpreting,
                selected,
                warn,
                indent,
                swatch: Swatch::Vector { color: v.color, points: d.points > 0, lines: d.lines > 0, areas: d.areas > 0 },
                fold: None,
            };
            let out = row(ui, Some(&mut v.visible), &r);
            if out.visible_changed {
                self.map.dirty = true;
            }
            let tip = |ui: &mut Ui| {
                ui.label(&d.path);
                ui.label(format!("{}, {} vertices, read in {:.0} ms", d.kind_text(), d.vertices, d.load_ms));
                ui.label(format!(
                    "CRS: {}{}",
                    if d.crs_name.is_empty() { "none (read as the raster's)" } else { &d.crs_name },
                    if d.lon_lat_assumed { " (none in the file: longitudes and latitudes)" } else { "" }
                ));
                if !d.fields.is_empty() {
                    let mut f = d.fields.iter().take(12).cloned().collect::<Vec<_>>().join(", ");
                    if d.fields.len() > 12 {
                        f += ", ...";
                    }
                    ui.label(format!("Fields: {f}"));
                }
                if d.truncated {
                    ui.colored_label(theme::warning(), "Read up to the vertex limit: the rest is left out.");
                }
                ui.weak("Click: select it (Ctrl, Shift: several). Delete or Ctrl+D: close the ones selected.");
            };
            let out_row = out.row.on_hover_ui(tip);
            select_on_click(ui, &out_row, &out.more, me, contents);
            drag_to_reorder(ui, &out_row, pos, acts);
            let style_name = &mut contents.style_name;
            let mut menu = |ui: &mut Ui| {
                let mut c = v.color;
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        v.color = c;
                        self.map.dirty = true;
                    }
                });
                style_menu(ui, me, &extra, style_name, acts);
                ui.separator();
                if ui
                    .button("Attribute table")
                    .on_hover_text("Its features' fields, read from the file now (a filter, sorting, zoom to a row)")
                    .clicked()
                {
                    acts.push(Act::AttributeTable(id));
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Interpret (label) its features...")).clicked() {
                    acts.push(Act::Interpret(id));
                    ui.close();
                }
                if ui
                    .add_enabled(has && v.why.is_empty(), egui::Button::new("Extract series to CSV..."))
                    .on_hover_text("The series of every feature (a point's pixel, a polygon's mean)")
                    .clicked()
                {
                    acts.push(Act::ExtractVector(id));
                    ui.close();
                }
                if ui.add_enabled(!v.px.is_empty(), egui::Button::new("Zoom to it")).clicked() {
                    acts.push(Act::ZoomVector(li));
                    ui.close();
                }
                move_menu(ui, place, acts);
                group_items(ui, me, &extra, acts);
                ui.separator();
                if ui.button("Export as...").on_hover_text("GeoPackage, GeoJSON, Shapefile or CSV").clicked() {
                    acts.push(Act::ExportVector(li));
                    ui.close();
                }
                ui.separator();
                close_item(ui, "Remove", closes, Act::RemoveVector(li), acts);
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
            if out_row.double_clicked() && !v.px.is_empty() {
                acts.push(Act::ZoomVector(li));
            }
        });
    }

    fn series_row(&mut self, ui: &mut Ui, i: usize, place: (usize, usize, usize), acts: &mut Vec<Act>) {
        let pos = place.0;
        let is_active = i == self.active;
        let info = if is_active {
            self.session.as_ref().map(|s| s.info.clone())
        } else {
            self.layers[i].session.as_ref().map(|s| s.info.clone())
        };
        let Some(info) = info else { return };
        if self.layers[i].canvas {
            return;
        }
        let name = self.layers[i].name.clone();
        if !self.contents.shows(&name) {
            return;
        }
        let key = format!("s{}", info.id);
        // unfolded from the start: a stack's bands (what it holds)
        let unfolded = info.stack && info.t() <= 16;
        let open = self.contents.is_open(&key, unfolded);
        let t = if is_active { self.display.t } else { self.layers[i].disp.t };
        let date = info.layers.get(t.min(info.t().saturating_sub(1))).map_or(String::new(), |l| l.label.clone());
        let loading = if is_active {
            self.session.as_ref().map(|s| &s.s.overview)
        } else {
            self.layers[i].session.as_ref().map(|s| &s.s.overview)
        }
        .filter(|o| !o.complete())
        .map(|o| format!("loading {}/{}", o.layers_done(), info.t()));
        // its picture in the colours the map draws it with (red, green and blue for a composite of three dates)
        let d = if is_active { &self.display } else { &self.layers[i].disp };
        let cmap = (d.mode != janus_gpu::Mode::Rgb).then(|| d.cmap[d.mode as usize]);
        let what = info.steps_noun();
        let detail = loading.clone().unwrap_or_else(|| {
            let n = info.t();
            if n > 1 {
                format!("{n} {what}, {} .. {}", info.layers[0].label, info.layers[n - 1].label)
            } else {
                format!("{} x {} px", info.width, info.height)
            }
        });
        let me = Sel::Series(info.id);
        self.contents.drawing.push(me);
        let selected = self.contents.selected.contains(&me);
        let closes = self.contents.closes_selection(me);
        let extra = self.row_extras(me);
        let indent = self.contents.indent;
        let l = &mut self.layers[i];
        ui.push_id(("series", i), |ui| {
            let r = Row {
                name: &name,
                detail,
                badge: if loading.is_some() || info.t() < 2 { String::new() } else { date.clone() },
                active: is_active,
                selected,
                warn: (!l.place.note.is_empty()).then(|| l.place.note.clone()),
                indent,
                swatch: Swatch::Stack(cmap),
                fold: (info.t() > 1).then_some(open),
            };
            let out = row(ui, Some(&mut l.visible), &r);
            if out.visible_changed {
                self.map.dirty = true;
            }
            if out.fold_clicked {
                self.contents.toggle(&key, unfolded);
            }
            drag_to_reorder(ui, &out.row, pos, acts);
            let mut tip = format!(
                "{}\n{}, {} x {} px, {} dates ({} .. {})\nCRS: {}",
                if l.inputs.len() == 1 { &l.inputs[0] } else { &info.first_path },
                info.description,
                info.width,
                info.height,
                info.t(),
                info.layers[0].label,
                info.layers[info.t() - 1].label,
                if info.crs_authority.is_empty() { &info.crs_name } else { &info.crs_authority }
            );
            if let Some((r, _)) = l.place.reproj.as_ref().filter(|_| !is_active) {
                tip += &if r.same_crs {
                    "\nA rotated grid, placed on the map's".to_string()
                } else {
                    format!("\nReprojected from {}", r.crs_text)
                };
                tip += &format!(
                    " through a grid of {} x {} points (interpolation error up to {} of\nits pixels); cursor, \
                     pins and ROI use the exact transformation (PROJ).",
                    r.grid_w,
                    r.grid_h,
                    janus_cube::multidim::format_g(r.grid_error, 2),
                );
            }
            tip += "\n\nClick: make it active. Double click: zoom to it. Drag: reorder.\nCtrl + click, Shift + click: \
                    select several. Delete or Ctrl+D: close the ones selected.";
            let out_row = out.row.on_hover_text(tip);
            select_on_click(ui, &out_row, &out.more, me, &mut self.contents);
            // Ctrl or Shift: the selection alone
            if out_row.clicked() && !is_active && !ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                acts.push(Act::Activate(i));
            }
            if out_row.double_clicked() {
                acts.push(Act::Fit(i));
            }
            let mut menu = |ui: &mut Ui| {
                if ui.add_enabled(!is_active, egui::Button::new("Make active")).clicked() {
                    acts.push(Act::Activate(i));
                    ui.close();
                }
                if ui.button("Zoom to it").clicked() {
                    acts.push(Act::Fit(i));
                    ui.close();
                }
                if ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2)).changed() {
                    self.map.dirty = true;
                }
                if blend_menu(ui, &mut l.blend) {
                    self.map.dirty = true;
                }
                style_menu(ui, me, &extra, &mut self.contents.style_name, acts);
                ui.separator();
                move_menu(ui, place, acts);
                group_items(ui, me, &extra, acts);
                if ui
                    .add_enabled(!info.multidim && !info.remote, egui::Button::new("Polygonize..."))
                    .on_hover_text("The date shown's patches (a class or change map) as polygons in a GeoPackage")
                    .clicked()
                {
                    acts.push(Act::Polygonize(me));
                    ui.close();
                }
                ui.menu_button("Export", |ui| {
                    for (label, kind) in EXPORTS {
                        if ui.button(label).clicked() {
                            acts.push(Act::Export(i, kind));
                            ui.close();
                        }
                    }
                });
                if let Some(dir) = l.inputs.first().and_then(|p| crate::provenance::find(p)) {
                    ui.separator();
                    provenance_items(ui, &dir.to_string_lossy(), acts);
                }
                ui.separator();
                close_item(ui, "Close", closes, Act::Remove(i), acts);
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
        });
        // unfolded: its dates (a stack's bands)
        if open && info.t() > 1 {
            self.step_rows(ui, i, &info, is_active, acts);
        }
    }

    /// An unfolded series' dates, or a stack's bands, a row each: the map shows one at a time, the one whose eye is
    /// open (a click on the row or its eye shows it, the eye of the one shown hides the layer); its menu exports it
    /// alone or opens it as a layer of its own.
    fn step_rows(&mut self, ui: &mut Ui, i: usize, info: &janus_cube::CubeInfo, is_active: bool, acts: &mut Vec<Act>) {
        let d = if is_active { &self.display } else { &self.layers[i].disp };
        let (t_now, cmap) = (d.t, d.cmap[janus_gpu::Mode::Value as usize]);
        let layer_visible = self.layers[i].visible;
        let noun = if info.stack || info.spectral { "band" } else { "date" };
        let all = self.contents.shows(&self.layers[i].name);
        let row_h = ui.text_style_height(&egui::TextStyle::Body) + 14.0;
        egui::ScrollArea::vertical()
            .id_salt(("steps", info.id))
            .max_height(row_h * 10.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for (t, l) in info.layers.iter().enumerate() {
                    if !all && !self.contents.shows(&l.label) {
                        continue;
                    }
                    let can_open = step_input(info, t).is_some();
                    let mut shown = layer_visible && t == t_now;
                    let r = Row {
                        name: &l.label,
                        detail: String::new(),
                        badge: if info.stack { format!("{}", t + 1) } else { String::new() },
                        active: is_active && t == t_now,
                        selected: false,
                        warn: None,
                        indent: 18.0,
                        swatch: Swatch::Ramp(cmap),
                        fold: None,
                    };
                    ui.push_id(("step", i, t), |ui| {
                        let out = row(ui, Some(&mut shown), &r);
                        if out.visible_changed {
                            acts.push(if shown { Act::ShowStep(i, t) } else { Act::HideLayer(i) });
                        }
                        let resp = out.row.on_hover_text(format!(
                            "{} {} of {}: {}\nClick, or its eye: shown on the map (one {noun} at a time).\nIts menu: \
                             export it alone, or open it as a layer of its own.",
                            if info.stack || info.spectral { "Band" } else { "Date" },
                            t + 1,
                            info.t(),
                            l.label
                        ));
                        if resp.clicked() && !(is_active && t == t_now && layer_visible) {
                            acts.push(Act::ShowStep(i, t));
                        }
                        let mut menu = |ui: &mut Ui| {
                            if ui.button("Show on the map").clicked() {
                                acts.push(Act::ShowStep(i, t));
                                ui.close();
                            }
                            ui.menu_button(format!("Export this {noun}"), |ui| {
                                for (label, kind) in STEP_EXPORTS {
                                    if ui.button(label).clicked() {
                                        acts.push(Act::ExportStep(i, t, kind));
                                        ui.close();
                                    }
                                }
                            });
                            if ui
                                .add_enabled(can_open, egui::Button::new("Open as a layer of its own"))
                                .on_hover_text(format!("This {noun} alone, as a new layer (an image of one {noun})"))
                                .clicked()
                            {
                                acts.push(Act::OpenStep(i, t));
                                ui.close();
                            }
                        };
                        resp.context_menu(&mut menu);
                        egui::Popup::menu(&out.more).show(&mut menu);
                    });
                }
            });
    }

    /// Layer i shown at date (band) t: made active and visible.
    fn show_step(&mut self, i: usize, t: usize, ctx: &egui::Context) {
        if i != self.active {
            self.set_active(i, ctx);
        }
        self.set_date(t);
        self.layers[i].visible = true;
        self.map.dirty = true;
    }

    /// Date (band) t of layer i opened as a layer of its own, named after both.
    fn open_step_as_layer(&mut self, i: usize, t: usize, ctx: &egui::Context) {
        let info = if i == self.active {
            self.session.as_ref().map(|s| s.info.clone())
        } else {
            self.layers[i].session.as_ref().map(|s| s.info.clone())
        };
        let Some((input, sel)) = info.as_ref().and_then(|info| step_input(info, t)) else { return };
        if self.opening.is_some() {
            return;
        }
        let mut o = crate::session::Opening::start_with(vec![input], true, sel, ctx);
        o.name = info.map(|info| format!("{} [{}]", self.layers[i].name, info.layers[t].label));
        self.opening = Some(o);
    }

    /// A map a run made: a layer of its own (its eye, its place in the order, its group), the series it was computed
    /// on only named; unfolded, its colours. A click selects it, and the Pixel values panel describes it.
    fn result_row(&mut self, ui: &mut Ui, k: usize, place: (usize, usize, usize), acts: &mut Vec<Act>) {
        let pos = place.0;
        if !self.contents.shows(&self.zeit.results[k].name) && !self.contents.shows(&self.zeit.results[k].source) {
            return;
        }
        let me = Sel::Result(self.zeit.results[k].id);
        self.contents.drawing.push(me);
        let selected = self.contents.selected.contains(&me);
        let closes = self.contents.closes_selection(me);
        let extra = self.row_extras(me);
        let key = format!("r{}", self.zeit.results[k].path);
        let open = self.contents.is_open(&key, false);
        let indent = self.contents.indent;
        let many = self.zeit.results.len() > 1;
        let r = &mut self.zeit.results[k];
        let mut fold_clicked = false;
        ui.push_id(("result", r.id), |ui| {
            let what = if r.classes.is_empty() { r.unit.clone() } else { format!("{} classes", r.classes.len()) };
            let row_def = Row {
                name: &r.name,
                detail: match (what.is_empty(), r.source.is_empty()) {
                    (_, true) => what,
                    (true, false) => format!("of {}", r.source),
                    (false, false) => format!("{what}, of {}", r.source),
                },
                badge: if r.preview { "preview".into() } else { String::new() },
                active: false,
                selected,
                warn: (!r.place.note.is_empty()).then(|| r.place.note.clone()),
                indent,
                swatch: Swatch::Ramp(r.cmap),
                fold: Some(open),
            };
            let out = row(ui, Some(&mut r.visible), &row_def);
            if out.visible_changed {
                self.map.dirty = true;
            }
            fold_clicked = out.fold_clicked;
            let out_row = out.row.on_hover_text(format!(
                "{}\nComputed on {}: a layer of its own (hiding or closing that series leaves it).\nClick: select it \
                 (its values and histogram in Pixel values). Drag: reorder. Delete or Ctrl+D: remove the ones \
                 selected (the files stay on disk).",
                r.path,
                if r.source.is_empty() { "a series" } else { &r.source }
            ));
            select_on_click(ui, &out_row, &out.more, me, &mut self.contents);
            drag_to_reorder(ui, &out_row, pos, acts);
            let mut menu = |ui: &mut Ui| {
                if ui.add(egui::Slider::new(&mut r.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2)).changed() {
                    self.map.dirty = true;
                }
                if ui
                    .add_enabled(many, egui::Button::new("Show only this map"))
                    .on_hover_text("Every other map of the runs hidden")
                    .clicked()
                {
                    acts.push(Act::SoloResult(k));
                    ui.close();
                }
                ui.separator();
                move_menu(ui, place, acts);
                group_items(ui, me, &extra, acts);
                ui.separator();
                if ui.button("Save as GeoTIFF...").clicked() {
                    acts.push(Act::SaveResult(k));
                    ui.close();
                }
                if ui.button("Save all outputs of its run...").clicked() {
                    acts.push(Act::SaveRun(k));
                    ui.close();
                }
                if ui
                    .button("Polygonize...")
                    .on_hover_text("Its patches as polygons (value, class, area in ha...) in a GeoPackage")
                    .clicked()
                {
                    acts.push(Act::Polygonize(me));
                    ui.close();
                }
                ui.separator();
                provenance_items(ui, &r.run, acts);
                ui.separator();
                close_item(ui, "Remove from the map", closes, Act::RemoveResult(k), acts);
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
            if !open {
                return;
            }
            ui.indent("result details", |ui| {
                ui.add_space(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0 + indent);
                    ui.vertical(|ui| crate::zeit::result_details_ui(ui, r, &mut self.map.dirty));
                });
            });
        });
        if fold_clicked {
            self.contents.toggle(&key, false);
        }
    }

    /// A vector layer written as GeoPackage, GeoJSON, Shapefile or CSV (by the extension chosen).
    fn export_vector(&mut self, li: usize) {
        let Some(v) = self.vectors.get(li) else { return };
        let stem =
            std::path::Path::new(&v.data.path).file_stem().map_or("layer".into(), |s| s.to_string_lossy().into_owned());
        let Some(to) = rfd::FileDialog::new()
            .add_filter("GeoPackage", &["gpkg"])
            .add_filter("GeoJSON", &["geojson"])
            .add_filter("Shapefile", &["shp"])
            .add_filter("CSV (lon, lat and the fields)", &["csv"])
            .set_file_name(format!("{stem}.gpkg"))
            .save_file()
        else {
            return;
        };
        let path = to.to_string_lossy().into_owned();
        if let Err(e) = write_vector_table(&path, &v.data, &[], &|_, _| String::new()) {
            self.error_popup = Some(e);
        }
    }
}

/// A result's provenance items (provenance.rs), for the run in folder `run`: shown only when it has one.
fn provenance_items(ui: &mut Ui, run: &str, acts: &mut Vec<Act>) {
    let has = !run.is_empty() && std::path::Path::new(run).join(crate::provenance::FILE).is_file();
    let tip = "Made by a Janus before 1.6, or not by a run: no provenance.json in its folder";
    let mut item = |label: &str, hover: &str, act: Act| {
        let b = ui.add_enabled(has, egui::Button::new(label));
        if if has { b.on_hover_text(hover) } else { b.on_disabled_hover_text(tip) }.clicked() {
            acts.push(act);
            ui.close();
        }
    };
    item(
        "Provenance...",
        "The tool, versions, when and how long, the series read, the scope, the parameters, the files written",
        Act::Provenance(run.to_string()),
    );
    item(
        "Run again with these parameters...",
        "The tool's window with the run's parameters and scope",
        Act::RunAgain(run.to_string()),
    );
    item(
        "Copy Python of the run",
        "A script that redoes the run with Zeit outside Janus and writes its maps",
        Act::CopyRunPython(run.to_string()),
    );
    if ui.button("Open folder").on_hover_text(run).clicked() {
        acts.push(Act::OpenFolder(run.to_string()));
        ui.close();
    }
}

/// A list's title, small and in capitals, with `right` (a count) at its end.
fn list_header(ui: &mut Ui, title: &str, right: &str) -> Response {
    ui.horizontal(|ui| {
        ui.add_space(2.0);
        ui.label(RichText::new(title.to_uppercase()).small().strong().color(ui.visuals().weak_text_color()));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(4.0);
            ui.label(RichText::new(right).small().weak());
        });
    })
    .response
}

/// A click on a layer's row selects it (Ctrl: adds or takes it, Shift: the run from the last one clicked); a right
/// click, or its menu's button, on a row out of the selection selects it alone.
fn select_on_click(ui: &Ui, row: &Response, more: &Response, me: Sel, contents: &mut ContentsUi) {
    if row.clicked() {
        contents.click(me, ui.input(|i| i.modifiers));
    } else if (row.secondary_clicked() || more.clicked()) && !contents.selected.contains(&me) {
        contents.click(me, egui::Modifiers::NONE);
    }
}

/// A row's Close (Remove) item, Delete its key: the selection when the row is in it with others.
fn close_item(ui: &mut Ui, label: &str, closes: Option<usize>, alone: Act, acts: &mut Vec<Act>) {
    let text = closes.map_or(label.to_string(), |n| format!("{label} the {n} selected"));
    if ui.add(egui::Button::new(text).shortcut_text("Del")).clicked() {
        acts.push(if closes.is_some() { Act::RemoveSelected } else { alone });
        ui.close();
    }
}

/// A row dragged onto another: moved to its place in the drawing order (a line shows where it goes).
fn drag_to_reorder(ui: &Ui, row: &Response, pos: usize, acts: &mut Vec<Act>) {
    if row.drag_started() {
        row.dnd_set_drag_payload(Dragged(pos));
    }
    if let Some(from) = row.dnd_hover_payload::<Dragged>()
        && from.0 != pos
    {
        let y = if from.0 < pos { row.rect.top() } else { row.rect.bottom() };
        ui.painter().hline(row.rect.x_range(), y, Stroke::new(2.0, theme::accent()));
    }
    if let Some(from) = row.dnd_release_payload::<Dragged>() {
        acts.push(Act::Move(from.0, pos));
    }
}

/// What a row's menu needs of the panel beyond its layer: the style copied (if this kind of layer takes it), the
/// saved styles it takes, how many layers a paste or a group takes, its group.
struct RowExtras {
    paste: bool,
    saved: Vec<String>,
    targets: usize,
    group: Option<u32>,
}

impl State {
    fn row_extras(&self, me: Sel) -> RowExtras {
        use crate::style::LayerStyle;
        let c = &self.contents;
        let fits = |st: &LayerStyle| {
            matches!((me, st), (Sel::Series(_), LayerStyle::Series(_)) | (Sel::Vector(_), LayerStyle::Vector(_)))
        };
        RowExtras {
            paste: c.style_clip.as_ref().is_some_and(fits),
            saved: self.saved_styles_for(me).into_iter().map(|s| s.0).collect(),
            targets: if c.selected.contains(&me) { c.selected.len().max(1) } else { 1 },
            group: self.group_of(me),
        }
    }

    /// Group `g`'s row over its `n` layers (the top one at `pos` in the drawing order): its eye shows or hides
    /// them all, its fold hides their rows, a click selects them, a row dropped on it joins it; whether it is
    /// unfolded.
    fn group_row(&mut self, ui: &mut Ui, g: u32, n: usize, pos: usize, acts: &mut Vec<Act>) -> bool {
        let Some(name) = self.groups.iter().find(|x| x.id == g).map(|x| x.name.clone()) else { return true };
        let key = format!("g{g}");
        let open = self.contents.is_open(&key, true);
        let mut visible = self.group_visible(g);
        let layers = format!("{n} layer{}", if n == 1 { "" } else { "s" });
        ui.push_id(("group", g), |ui| {
            let r = Row {
                name: &name,
                detail: String::new(),
                badge: layers.clone(),
                active: false,
                selected: false,
                warn: None,
                indent: 0.0,
                swatch: Swatch::Group,
                fold: Some(open),
            };
            let out = row(ui, Some(&mut visible), &r);
            if out.visible_changed {
                acts.push(Act::GroupVisible(g, visible));
            }
            if out.fold_clicked {
                self.contents.toggle(&key, true);
            }
            let out_row = out.row.on_hover_text(
                "A group: its eye shows or hides its layers, its arrow folds them.\nClick: select its layers. Drop a \
                 row on it: the layer joins the group.",
            );
            if out_row.clicked() {
                acts.push(Act::SelectGroup(g));
            }
            if out_row.double_clicked() {
                self.contents.toggle(&key, true);
            }
            if let Some(from) = out_row.dnd_hover_payload::<Dragged>()
                && from.0 != pos
            {
                ui.painter().hline(out_row.rect.x_range(), out_row.rect.bottom(), Stroke::new(2.0, theme::accent()));
            }
            if let Some(from) = out_row.dnd_release_payload::<Dragged>() {
                acts.push(Act::Drop(from.0, pos));
            }
            let groups = &mut self.groups;
            let renaming = &mut self.contents.group_name;
            let mut menu = |ui: &mut Ui| {
                let text = renaming.get_or_insert_with(|| (g, name.clone()));
                if text.0 != g {
                    *text = (g, name.clone());
                }
                ui.horizontal(|ui| {
                    ui.label("Name");
                    let r = ui.add(egui::TextEdit::singleline(&mut text.1).desired_width(140.0));
                    if r.changed()
                        && !text.1.trim().is_empty()
                        && let Some(gr) = groups.iter_mut().find(|x| x.id == g)
                    {
                        gr.name = text.1.trim().to_string();
                    }
                });
                if ui.button("Select its layers").clicked() {
                    acts.push(Act::SelectGroup(g));
                    ui.close();
                }
                ui.menu_button("Move", |ui| {
                    for (label, top) in [("To the top", true), ("To the bottom", false)] {
                        if ui.button(label).clicked() {
                            acts.push(Act::MoveGroup(g, top));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button("Ungroup").on_hover_text("The layers stay where they are").clicked() {
                    acts.push(Act::Ungroup(g));
                    ui.close();
                }
                if ui.button(format!("Close its {layers}")).clicked() {
                    acts.push(Act::CloseGroup(g));
                    ui.close();
                }
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
        });
        open
    }
}

/// A series' Blend submenu; whether the mode changed.
fn blend_menu(ui: &mut Ui, blend: &mut janus_gpu::Blend) -> bool {
    let mut changed = false;
    ui.menu_button(format!("Blend: {}", blend.name()), |ui| {
        for b in janus_gpu::Blend::ALL {
            changed |= ui.radio_value(blend, b, b.name()).changed();
        }
    })
    .response
    .on_hover_text(
        "How its colours combine with the layers under it: Multiply darkens by them, Screen lightens,\nAdd sums, \
         Darken and Lighten keep the darker or the lighter (with its opacity)",
    );
    changed
}

/// A row's Style submenu: copy its style, paste the one copied (on the layers selected), the saved styles, save
/// its own under a name.
fn style_menu(ui: &mut Ui, me: Sel, x: &RowExtras, name: &mut String, acts: &mut Vec<Act>) {
    ui.menu_button("Style", |ui| {
        if ui.button("Copy style").on_hover_text("Its colours, range, mode, opacity, blend and classes").clicked() {
            acts.push(Act::CopyStyle(me));
            ui.close();
        }
        let paste =
            if x.targets > 1 { format!("Paste style on the {} selected", x.targets) } else { "Paste style".into() };
        if ui.add_enabled(x.paste, egui::Button::new(paste)).clicked() {
            acts.push(Act::PasteStyle(me));
            ui.close();
        }
        ui.separator();
        if x.saved.is_empty() {
            ui.weak("No saved styles");
        }
        for s in &x.saved {
            ui.horizontal(|ui| {
                if ui.button(s).on_hover_text("Give it this saved style").clicked() {
                    acts.push(Act::ApplySaved(me, s.clone()));
                    ui.close();
                }
                if ui.small_button("x").on_hover_text("Delete this saved style").clicked() {
                    acts.push(Act::DeleteSaved(s.clone()));
                }
            });
        }
        ui.separator();
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(name).hint_text("name").desired_width(110.0));
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if (ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Save style")).clicked() || enter)
                && !name.trim().is_empty()
            {
                acts.push(Act::SaveStyle(me, std::mem::take(name).trim().to_string()));
                ui.close();
            }
        });
    });
}

/// A row's group items: the layers selected made a group, or the row out of its group.
fn group_items(ui: &mut Ui, me: Sel, x: &RowExtras, acts: &mut Vec<Act>) {
    let n = x.targets;
    let label = if n > 1 { format!("Group the {n} selected") } else { "Make a group of it".to_string() };
    if ui.button(label).on_hover_text("Next to each other in the drawing order, under a row of their own").clicked() {
        acts.push(Act::GroupSelected);
        ui.close();
    }
    if x.group.is_some() && ui.button("Out of its group").clicked() {
        acts.push(Act::LeaveGroup(me));
        ui.close();
    }
}

/// A row's Move menu: `place` is its place in the drawing order, the lowest a layer can go and the top.
fn move_menu(ui: &mut Ui, place: (usize, usize, usize), acts: &mut Vec<Act>) {
    let (pos, lo, top) = place;
    ui.menu_button("Move", |ui| {
        for (label, ok, to) in [
            ("To the top", pos < top, top),
            ("Up", pos < top, pos + 1),
            ("Down", pos > lo, pos.saturating_sub(1)),
            ("To the bottom", pos > lo, lo),
        ] {
            if ui.add_enabled(ok, egui::Button::new(label)).clicked() {
                acts.push(Act::Move(pos, to));
                ui.close();
            }
        }
    });
}

/// What opens date (band) t of a series alone: its file as the series reads it, or a band of a file of several
/// dates through GDAL's vrt:// connection (None: a NetCDF variable's level, or embeddings' dimensions).
fn step_input(info: &janus_cube::CubeInfo, t: usize) -> Option<(String, janus_cube::BandSelection)> {
    let l = info.layers.get(t)?;
    let one_file = info.layers.iter().all(|x| x.path == l.path);
    if info.multidim || l.band_base != 0 || (one_file && info.bands_per_date > 1) {
        return None;
    }
    if one_file {
        let path = l.path.replace('\\', "/");
        Some((format!("vrt://{path}?bands={}", l.band), janus_cube::BandSelection::default()))
    } else {
        Some((l.path.clone(), info.sel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers;

    /// The list's selection as a file manager's: a click, Ctrl + click, Shift + click (a run from the last click).
    #[test]
    fn selection_by_clicks() {
        let rows = [Sel::Series(1), Sel::Vector(7), Sel::Series(2), Sel::Series(3)];
        let mut c = ContentsUi { order: rows.to_vec(), ..Default::default() };
        let sel = |c: &ContentsUi| rows.iter().filter(|r| c.selected.contains(r)).copied().collect::<Vec<_>>();
        c.click(rows[1], Modifiers::NONE);
        assert!(c.focused);
        assert_eq!(sel(&c), [rows[1]]);
        c.click(rows[3], Modifiers::COMMAND);
        assert_eq!(sel(&c), [rows[1], rows[3]]);
        c.click(rows[1], Modifiers::COMMAND);
        assert_eq!(sel(&c), [rows[3]]);
        // the run from the last one clicked (rows[1], taken off) to this one
        c.click(rows[3], Modifiers::SHIFT);
        assert_eq!(sel(&c), [rows[1], rows[2], rows[3]]);
        c.click(rows[0], Modifiers::NONE);
        c.click(rows[2], Modifiers::SHIFT);
        assert_eq!(sel(&c), [rows[0], rows[1], rows[2]]);
        assert_eq!(c.closes_selection(rows[1]), Some(3));
        assert_eq!(c.closes_selection(rows[3]), None);
    }
}
