//! The Layers panel as a table of contents: what the map draws, top of the list on top, series and vector layers
//! in one order (order.rs). Sections that fold, a row a layer (visibility, a swatch, its name, a short detail, a
//! menu), the details of a layer when it is unfolded (its opacity, its placement, the maps its runs made as rows of
//! their own), every action in the row's menu (right click, or its "..." button): make it active, opacity, move,
//! export, close. Any row is dragged onto another to reorder; a filter by name keeps a long list usable.

use std::collections::HashSet;

use egui::{Align, Color32, Layout, Rect, Response, RichText, Sense, Shape, Stroke, Ui, UiBuilder, vec2};
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

/// What a row shows at its left.
pub(crate) enum Swatch {
    Color(Color32),
    /// a colormap's ramp
    Ramp(usize),
    /// a series: a small stack of frames
    Series,
}

/// A row's content.
pub(crate) struct Row<'a> {
    pub name: &'a str,
    pub detail: String,
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

fn paint_swatch(ui: &Ui, rect: Rect, s: &Swatch) {
    let p = ui.painter();
    match s {
        Swatch::Color(c) => {
            p.rect_filled(rect, 2.0, *c);
            p.rect_stroke(
                rect,
                2.0,
                Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
                egui::StrokeKind::Inside,
            );
        }
        Swatch::Ramp(k) => {
            let cm = &janus_gpu::colormaps::all()[*k];
            let n = 6;
            for i in 0..n {
                let [r, g, b, _] = cm.sample((i as f32 + 0.5) / n as f32);
                let x0 = rect.left() + rect.width() * i as f32 / n as f32;
                let x1 = rect.left() + rect.width() * (i + 1) as f32 / n as f32;
                p.rect_filled(Rect::from_x_y_ranges(x0..=x1, rect.y_range()), 0.0, Color32::from_rgb(r, g, b));
            }
        }
        Swatch::Series => {
            let c = ui.visuals().text_color();
            for k in 0..3 {
                let o = k as f32 * 2.5;
                let r = Rect::from_min_size(rect.min + vec2(o, 4.0 - o), vec2(rect.width() - 5.0, rect.height() - 5.0));
                p.rect_filled(r, 1.0, ui.visuals().extreme_bg_color);
                p.rect_stroke(r, 1.0, Stroke::new(1.0, c.gamma_multiply(0.7)), egui::StrokeKind::Inside);
            }
        }
    }
}

/// One row of the contents: [fold] [visible] [swatch] name ...... detail [!] [...].
pub(crate) fn row(ui: &mut Ui, visible: Option<&mut bool>, r: &Row) -> RowOut {
    let h = ui.spacing().interact_size.y + 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click_and_drag());
    let v = ui.visuals().clone();
    if r.active {
        ui.painter().rect_filled(rect, 3.0, v.selection.bg_fill.gamma_multiply(0.35));
        ui.painter().rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 1.0, theme::accent());
    } else if resp.hovered() || resp.context_menu_opened() {
        ui.painter().rect_filled(rect, 3.0, v.widgets.hovered.weak_bg_fill.gamma_multiply(0.6));
    }
    let mut out = RowOut { row: resp.clone(), visible_changed: false, fold_clicked: false, more: resp.clone() };
    let mut child = ui.new_child(
        UiBuilder::new().max_rect(rect.shrink2(vec2(4.0, 0.0))).layout(Layout::right_to_left(Align::Center)),
    );
    // right: the menu's button, a warning, the detail
    let (dots, more) = child.allocate_exact_size(vec2(18.0, h - 4.0), Sense::click());
    let dc = if more.hovered() { v.strong_text_color() } else { v.weak_text_color() };
    for k in -1..=1 {
        child.painter().circle_filled(dots.center() + vec2(k as f32 * 4.0, 0.0), 1.4, dc);
    }
    out.more = more.on_hover_text("More (or right click the row)");
    if let Some(w) = &r.warn {
        child.label(RichText::new("!").strong().color(theme::warning())).on_hover_text(w);
    }
    if !r.detail.is_empty() {
        child.label(RichText::new(&r.detail).small().weak());
    }
    // left: fold, visibility, swatch, the name truncated to what is left
    child.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add_space(r.indent);
        if let Some(open) = r.fold {
            let (ar, a) = ui.allocate_exact_size(vec2(12.0, h - 4.0), Sense::click());
            let c = if a.hovered() { v.strong_text_color() } else { v.text_color() };
            let m = ar.center();
            let pts = if open {
                vec![m + vec2(-4.0, -2.0), m + vec2(4.0, -2.0), m + vec2(0.0, 3.0)]
            } else {
                vec![m + vec2(-2.0, -4.0), m + vec2(3.0, 0.0), m + vec2(-2.0, 4.0)]
            };
            ui.painter().add(Shape::convex_polygon(pts, c, Stroke::NONE));
            out.fold_clicked = a.on_hover_text(if open { "Fold" } else { "Unfold: details and maps" }).clicked();
        } else {
            ui.add_space(12.0 + ui.spacing().item_spacing.x);
        }
        if let Some(vis) = visible {
            out.visible_changed = ui.checkbox(vis, "").on_hover_text(if *vis { "Hide" } else { "Show" }).changed();
        }
        let (sr, _) = ui.allocate_exact_size(vec2(16.0, 12.0), Sense::hover());
        paint_swatch(ui, sr, &r.swatch);
        let mut text = RichText::new(r.name);
        if r.active {
            text = text.color(theme::accent()).strong();
        }
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
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
        let total = self.layers.iter().filter(|l| !l.canvas).count() + self.vectors.len();
        if total == 0 && self.vector_loads.is_empty() && self.opening.is_none() {
            ui.add_space(6.0);
            ui.weak("No layers. Open a series or a vector file (+ Add, File, drag and drop or the command line).");
        }

        // series and vector layers in one drawing order
        if total > 0 {
            egui::CollapsingHeader::new(RichText::new(format!("Layers ({total})")).strong())
                .id_salt("contents layers")
                .default_open(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    self.layer_rows(ui, &mut acts)
                })
                .header_response
                .on_hover_text(
                    "Top of the list = drawn on top, series and vector layers alike: drag a row onto another to \
                     reorder.\nClick a series to make it active. Alt + click a polygon on the map: it becomes the \
                     ROI; a line: the transect.",
                );
        }

        egui::CollapsingHeader::new(RichText::new("Embeddings").strong())
            .id_salt("emb dl layers")
            .show(ui, |ui| self.embedding_download_ui(ui))
            .header_response
            .on_hover_text("Foundation-model embeddings (AlphaEarth, TESSERA) of the visible area,\ndownloaded and added as a layer");
        let base = match self.basemap.ui.source.as_str() {
            "none" => "Basemap".to_string(),
            _ if self.basemap.ui.on => "Basemap (on)".to_string(),
            _ => "Basemap (hidden)".to_string(),
        };
        egui::CollapsingHeader::new(RichText::new(base).strong())
            .id_salt("contents basemap")
            .default_open(true)
            .show(ui, |ui| self.basemap_ui(ui))
            .header_response
            .on_hover_text("Satellite imagery or a map under every layer");

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

    /// Every layer's row, top to bottom (the canvas of vector layers alone is not listed).
    fn layer_rows(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        let order = self.draw_order();
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
                detail: d.kind_text(),
                active: interpreting,
                warn,
                indent: 0.0,
                swatch: Swatch::Color(v.color),
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
        let l = &mut self.layers[i];
        ui.push_id(("series", i), |ui| {
            let r = Row {
                name: &name,
                detail: loading.clone().unwrap_or_else(|| date.clone()),
                active: is_active,
                warn: (!l.place.note.is_empty()).then(|| l.place.note.clone()),
                indent: 0.0,
                swatch: Swatch::Series,
                fold: Some(open),
            };
            let out = row(ui, Some(&mut l.visible), &r);
            if out.visible_changed {
                self.map.dirty = true;
            }
            if out.fold_clicked {
                self.contents.toggle(&key, !results.is_empty());
            }
            drag_to_reorder(ui, &out.row, pos, acts);
            let tip = format!(
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
        // unfolded: its details, the maps its runs made
        let l = &mut self.layers[i];
        ui.push_id(("series details", i), |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.indent("details", |ui| {
                ui.horizontal(|ui| {
                    ui.weak("opacity");
                    if ui.add(egui::Slider::new(&mut l.opacity, 0.0..=1.0).fixed_decimals(2)).changed() {
                        self.map.dirty = true;
                    }
                });
                ui.weak(format!("{} dates, {} x {} px", info.t(), info.width, info.height));
                if let Some((r, _)) = l.place.reproj.as_ref().filter(|_| !is_active) {
                    let text = if r.same_crs {
                        "rotated grid, placed on the map's".to_string()
                    } else {
                        format!("reprojected from {}", r.crs_text)
                    };
                    ui.weak(text).on_hover_text(format!(
                        "Drawn through a grid of {} x {} points (interpolation error up to {} of\nits pixels); \
                         cursor, pins and ROI use the exact transformation (PROJ).",
                        r.grid_w,
                        r.grid_h,
                        janus_cube::multidim::format_g(r.grid_error, 2),
                    ));
                }
            });
        });
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
                active: false,
                warn: None,
                indent: 16.0,
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
