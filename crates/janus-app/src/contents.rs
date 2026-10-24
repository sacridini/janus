//! The Layers panel as a table of contents: what the map draws, top of the list on top, series and vector layers
//! in one order (order.rs). Sections that fold, a row a layer (visibility, a swatch, its name, a short detail, a
//! menu), the details of a layer when it is unfolded (its opacity, its placement, the maps its runs made as rows of
//! their own), every action in the row's menu (right click, or its "..." button): make it active, opacity, move,
//! export, close. Any row is dragged onto another to reorder; a filter by name keeps a long list usable.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Shape, Stroke, Ui, vec2};
use janus_geo::vectors::write_vector_table;

use crate::app::State;
use crate::order::Item;
use crate::theme;

/// The panel's own state: the filter, the rows unfolded.
#[derive(Default)]
pub struct ContentsUi {
    pub filter: String,
    /// rows unfolded (by key: "s" + cube id, "r" + a result's file); `folded`: the ones folded by hand
    expanded: HashSet<String>,
    folded: HashSet<String>,
}

impl ContentsUi {
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
}

/// A row's content.
pub(crate) struct Row<'a> {
    pub name: &'a str,
    /// the second line, in the weak colour
    pub detail: String,
    /// on the right of the name (the date shown)
    pub badge: String,
    pub active: bool,
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
    // the background: the active row tinted, a bar of the accent at its left; the row under the pointer lighter
    if r.active {
        ui.painter().rect_filled(rect, 3.0, v.selection.bg_fill.gamma_multiply(0.30));
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 1.0, theme::accent());
    } else if resp.hovered() || resp.context_menu_opened() {
        ui.painter().rect_filled(rect, 3.0, v.widgets.hovered.weak_bg_fill.gamma_multiply(0.55));
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
    /// from, to: places in the drawing order (bottom to top)
    Move(usize, usize),
    Export(usize, usize),
    Fit(usize),
    SaveResult(usize),
    SaveRun(usize),
    RemoveResult(usize),
    RemoveVector(usize),
    ExtractVector(u32),
    Interpret(u32),
    ZoomVector(usize),
    ExportVector(usize),
    AttributeTable(u32),
}

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
        let total = self.layers.iter().filter(|l| !l.canvas).count() + self.vectors.len();
        // the list: series and vector layers in one drawing order, the basemap under them all
        list_header(ui, "Layers", &total.to_string()).on_hover_text(
            "Top of the list = drawn on top, series and vector layers alike: drag a row onto another to \
             reorder.\nClick a series to make it active; the eye shows or hides a layer. Alt + click a polygon on \
             the map: it becomes the ROI; a line: the transect.",
        );
        let v = ui.visuals();
        egui::Frame::new()
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
            });
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
                Act::Move(from, to) => self.move_in_order(from, to),
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
                Act::SaveRun(k) => self.open_save_run(k),
                Act::RemoveResult(k) => {
                    if k < self.zeit.results.len() {
                        self.zeit.results.remove(k);
                        self.map.dirty = true;
                    }
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
        if order.is_empty() {
            return;
        }
        // the canvas stays at the bottom
        let lo = usize::from(matches!(order.first(), Some(Item::Series(i)) if self.layers[*i].canvas));
        let top = order.len() - 1;
        for pos in (0..order.len()).rev() {
            match order[pos] {
                Item::Series(i) => self.series_row(ui, i, (pos, lo, top), acts),
                Item::Vector(li) => self.vector_row(ui, li, (pos, lo, top), acts),
            }
        }
    }

    fn vector_row(&mut self, ui: &mut Ui, li: usize, place: (usize, usize, usize), acts: &mut Vec<Act>) {
        let has = self.session.is_some();
        let pos = place.0;
        let name = self.vectors[li].name.clone();
        if !self.contents.shows(&name) {
            return;
        }
        let interpreting = self.labels.vector == Some(self.vectors[li].id);
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
        ui.push_id(("vector", id), |ui| {
            let r = Row {
                name: &name,
                detail: if v.placing.is_some() { "placing...".into() } else { d.kind_text() },
                badge: if interpreting { "interpreting".into() } else { String::new() },
                active: interpreting,
                warn,
                indent: 0.0,
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
            };
            let out_row = out.row.on_hover_ui(tip);
            drag_to_reorder(ui, &out_row, pos, acts);
            let mut menu = |ui: &mut Ui| {
                let mut c = v.color;
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    if ui.color_edit_button_srgba(&mut c).changed() {
                        v.color = c;
                        self.map.dirty = true;
                    }
                });
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
                ui.separator();
                if ui.button("Export as...").on_hover_text("GeoPackage, GeoJSON, Shapefile or CSV").clicked() {
                    acts.push(Act::ExportVector(li));
                    ui.close();
                }
                ui.separator();
                if ui.button("Remove").clicked() {
                    acts.push(Act::RemoveVector(li));
                    ui.close();
                }
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
        let results: Vec<usize> =
            (0..self.zeit.results.len()).filter(|&k| self.zeit.results[k].cube == info.id).collect();
        let child_match = results.iter().any(|&k| self.contents.shows(&self.zeit.results[k].name));
        if !self.contents.shows(&name) && !child_match {
            return;
        }
        let key = format!("s{}", info.id);
        let open = self.contents.is_open(&key, !results.is_empty());
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
        let what = if info.spectral { "bands" } else { "dates" };
        let detail = loading.clone().unwrap_or_else(|| {
            let n = info.t();
            if n > 1 {
                format!("{n} {what}, {} .. {}", info.layers[0].label, info.layers[n - 1].label)
            } else {
                format!("{} x {} px", info.width, info.height)
            }
        });
        let l = &mut self.layers[i];
        ui.push_id(("series", i), |ui| {
            let r = Row {
                name: &name,
                detail,
                badge: if loading.is_some() || info.t() < 2 { String::new() } else { date.clone() },
                active: is_active,
                warn: (!l.place.note.is_empty()).then(|| l.place.note.clone()),
                indent: 0.0,
                swatch: Swatch::Stack(cmap),
                fold: (!results.is_empty()).then_some(open),
            };
            let out = row(ui, Some(&mut l.visible), &r);
            if out.visible_changed {
                self.map.dirty = true;
            }
            if out.fold_clicked {
                self.contents.toggle(&key, !results.is_empty());
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
            tip += "\n\nClick: make it active. Double click: zoom to it. Drag: reorder.";
            let out_row = out.row.on_hover_text(tip);
            if out_row.clicked() && !is_active {
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
                ui.separator();
                move_menu(ui, place, acts);
                ui.menu_button("Export", |ui| {
                    for (label, kind) in EXPORTS {
                        if ui.button(label).clicked() {
                            acts.push(Act::Export(i, kind));
                            ui.close();
                        }
                    }
                });
                ui.separator();
                if ui.button("Close").clicked() {
                    acts.push(Act::Remove(i));
                    ui.close();
                }
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
        });
        if !open {
            return;
        }
        // unfolded: the maps its runs made
        for &k in results.iter().rev() {
            if !self.contents.shows(&self.zeit.results[k].name) && !self.contents.shows(&name) {
                continue;
            }
            self.result_row(ui, k, acts);
        }
    }

    /// A map a run made, under its series.
    fn result_row(&mut self, ui: &mut Ui, k: usize, acts: &mut Vec<Act>) {
        let key = format!("r{}", self.zeit.results[k].path);
        let open = self.contents.is_open(&key, false);
        let r = &mut self.zeit.results[k];
        let mut fold_clicked = false;
        ui.push_id(("result", k), |ui| {
            let swatch = Swatch::Ramp(r.cmap);
            let row_def = Row {
                name: &r.name,
                detail: if r.classes.is_empty() { r.unit.clone() } else { format!("{} classes", r.classes.len()) },
                badge: String::new(),
                active: false,
                warn: None,
                indent: 22.0,
                swatch,
                fold: Some(open),
            };
            let out = row(ui, Some(&mut r.visible), &row_def);
            if out.visible_changed {
                self.map.dirty = true;
            }
            fold_clicked = out.fold_clicked;
            let out_row = out.row.on_hover_text(format!("{}\n(the file stays on disk when removed)", r.path));
            let mut menu = |ui: &mut Ui| {
                if ui.add(egui::Slider::new(&mut r.opacity, 0.0..=1.0).text("opacity").fixed_decimals(2)).changed() {
                    self.map.dirty = true;
                }
                ui.separator();
                if ui.button("Save as GeoTIFF...").clicked() {
                    acts.push(Act::SaveResult(k));
                    ui.close();
                }
                if ui.button("Save all outputs of its run...").clicked() {
                    acts.push(Act::SaveRun(k));
                    ui.close();
                }
                ui.separator();
                if ui.button("Remove from the map").clicked() {
                    acts.push(Act::RemoveResult(k));
                    ui.close();
                }
            };
            out_row.context_menu(&mut menu);
            egui::Popup::menu(&out.more).show(&mut menu);
            if !open {
                return;
            }
            ui.indent("result details", |ui| {
                ui.add_space(0.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
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
