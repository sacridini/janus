//! The side panels: Display (date, mode, colormap, stretch, histogram), Layers (the series), Statistics (the
//! cursor's pixel) and Performance (the timings this phase is judged by, RUST_MIGRATION.md).

use egui::{Color32, ComboBox, DragValue, Grid, Rect, Sense, Slider, Ui, vec2};
use egui_plot::{Bar, BarChart, Plot, VLine};
use janus_cube::multidim::format_g;
use janus_cube::spectral::{COMPOSITES, INDICES, composite_layers, index_bands};
use janus_cube::{BandSelection, QaRule};
use janus_gpu::{Mode, colormaps};

use crate::app::State;
use crate::display::{MODE_ORDER, mode_name_of, range_label};
use crate::theme;

/// A section's title with a line after it (ImGui's SeparatorText).
fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.strong(title);
        ui.add(egui::Separator::default().horizontal());
    });
}

/// A colormap's gradient with its name, `width` points wide (ImPlot's ColormapButton).
fn colormap_button(ui: &mut Ui, cmap: usize, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 20.0), Sense::click());
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
    let font = egui::FontId::proportional(13.0);
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
    /// The Display panel (app.cpp, uiLayer): the series, its bands, what the map shows (mode, colormap, stretch,
    /// histogram) and the detail at full resolution.
    pub fn display_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let Some(s) = &self.session else {
            ui.weak("No series open.");
            if ui.add_sized([ui.available_width(), 20.0], egui::Button::new("Open file(s)...")).clicked()
                && let Some(files) = rfd::FileDialog::new().pick_files()
            {
                self.open_inputs(files.iter().map(|f| f.to_string_lossy().into_owned()).collect(), false, &ctx);
            }
            if ui.add_sized([ui.available_width(), 20.0], egui::Button::new("Open folder...")).clicked()
                && let Some(dir) = rfd::FileDialog::new().pick_folder()
            {
                self.open_inputs(vec![dir.to_string_lossy().into_owned()], false, &ctx);
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
        let what = if info.spectral { "bands" } else { "dates" };
        ui.label(format!("{n} {what}: {} to {}", info.layers[0].label, info.layers[n - 1].label));
        ui.label(format!("{} x {} px, {}", info.width, info.height, info.data_type));
        let crs = if info.crs_name.is_empty() { "(no CRS)" } else { &info.crs_name };
        ui.add(egui::Label::new(format!("CRS: {crs} {}", info.crs_authority)).wrap());
        if let Some(g) = info.geo_transform {
            ui.label(format!("Pixel: {} x {}", format_g(g[1], 6), format_g(g[5].abs(), 6)));
        }
        self.bands_ui(ui, &info);
        self.composite_ui(ui);

        section(ui, "Display");
        let Some(s) = &self.session else { return };
        let classes_on = self.classes.on();
        let d = &mut self.display;
        // what the series allows: class codes have no means or trends; a single date no temporal statistics
        let available =
            |m: Mode| if classes_on { m == Mode::Value } else { m == Mode::Value || m == Mode::Rgb || n >= 2 };
        let mut dirty = false;
        ComboBox::from_id_salt("mode")
            .width(ui.available_width())
            .height(400.0)
            .selected_text(mode_name_of(d.mode, &info))
            .show_ui(ui, |ui| {
                for m in MODE_ORDER {
                    dirty |= ui
                        .add_enabled_ui(available(m), |ui| ui.selectable_value(&mut d.mode, m, mode_name_of(m, &info)))
                        .inner
                        .changed();
                }
            });
        let (changed, turned_on) = self.classes.ui(ui, s);
        if changed {
            self.map.dirty = true;
        }
        if turned_on {
            d.mode = Mode::Value;
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
            if d.mode == Mode::Rgb {
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
                        if !c.qualitative && colormap_button(ui, i, 220.0).clicked() {
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
                    .add_sized([ui.available_width(), 18.0], egui::Button::new("Auto"))
                    .on_hover_text("Automatic range: 2-98% percentiles (symmetric for anomaly, difference and trend)")
                    .clicked()
                {
                    r.manual = false;
                    r.invalidate();
                }
            });
            if crate::display::time_dependent(d.mode)
                && ui
                    .checkbox(&mut d.per_date_range, "Per-date range")
                    .on_hover_text("Off: same range for every date (colors comparable through time)")
                    .changed()
            {
                d.ranges[d.mode as usize].invalidate();
            }
            // the histogram of the values shown, with the stretch
            let r = d.ranges[d.mode as usize];
            let h = &d.hist;
            if !h.xs.is_empty() {
                let bars: Vec<Bar> = h.xs.iter().zip(&h.ys).map(|(&x, &y)| Bar::new(x, y).width(h.bar_w)).collect();
                let drop_dates = d.mode == Mode::DropDate;
                let info2 = info.clone();
                let mut plot = Plot::new("hist")
                    .height(130.0)
                    .show_axes([true, false])
                    .show_grid(false)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .show_x(false)
                    .show_y(false);
                if drop_dates {
                    // date indices: labelled with the dates
                    plot = plot.x_axis_formatter(move |m, _| range_label(Mode::DropDate, m.value as f32, &info2));
                }
                plot.show(ui, |p| {
                    p.bar_chart(BarChart::new("values", bars).color(Color32::from_rgb(115, 158, 230)));
                    for v in [r.lo, r.hi] {
                        p.vline(VLine::new("range", f64::from(v)).color(Color32::from_rgb(255, 153, 51)).width(1.5));
                    }
                });
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

    /// The Display panel's band selection of a series with several bands per date (app.cpp, uiBands): the band
    /// shown, a normalized difference, a quality band; applied by reopening the layer.
    fn bands_ui(&mut self, ui: &mut Ui, info: &janus_cube::CubeInfo) {
        if info.spectral || info.multidim || info.bands_per_date <= 1 {
            return;
        }
        if self.band_sel.as_ref().is_none_or(|(id, _)| *id != info.id) {
            self.band_sel = Some((info.id, info.sel));
        }
        let Some((_, sel)) = self.band_sel.as_mut() else { return };
        section(ui, "Bands");
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
                    for r in [QaRule::Fmask, QaRule::LandsatC2, QaRule::NonZero] {
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
        let same = sel.band == info.sel.band
            && sel.nd_band == info.sel.nd_band
            && sel.qa_band == info.sel.qa_band
            && sel.qa_rule == info.sel.qa_rule;
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
            }
        }
    }

    /// The Statistics panel (app.cpp, uiStats): a column per series on the chart (the cursor, the pins, the ROI's
    /// mean, and the other layers' with "All visible layers"), a row per statistic; a categorical series' classes.
    pub fn statistics_ui(&mut self, ui: &mut Ui) {
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
            Grid::new("stats").striped(true).num_columns(1 + cols.len()).show(ui, |ui| {
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
                if let Some(ac) = active_classes {
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
            });
        });
        if let Some(id) = remove_pin {
            self.remove_pin(id);
        }
        if remove_roi {
            self.clear_roi();
        }
    }

    pub fn performance_ui(&mut self, ui: &mut Ui) {
        let st = &self.startup;
        Grid::new("perf").num_columns(2).striped(true).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.weak(k);
                ui.label(v);
                ui.end_row();
            };
            row("GDAL", format!("{} ms ({})", st.gdal_ms.round(), janus_cube::gdal_version()));
            row("window + wgpu", format!("{} ms", st.window_ms.round()));
            row("first frame", st.first_frame_ms.map_or("-".into(), |v| format!("{} ms", v.round())));
            row("map shaders", st.gpu_ms.map_or("-".into(), |v| format!("{v:.0} ms")));
            let info = self.render_state.adapter.get_info();
            row("GPU", format!("{} ({:?})", info.name, info.backend));
            if let Some(s) = &self.session {
                row("open (metadata)", format!("{:.0} ms", s.open_ms));
                let ov = &s.s.overview;
                row(
                    "overview",
                    format!(
                        "{}/{} dates{}",
                        ov.layers_done(),
                        ov.t,
                        if ov.from_cache() { ", from its cache" } else { "" }
                    ),
                );
                if let Some(b) = ov.build_seconds() {
                    row("overview read", format!("{:.0} ms", b * 1000.0));
                }
                if let Some(stats) = &s.stats {
                    row(
                        "statistics (GPU + readback / copy)",
                        format!("{:.1} + {:.1} ms", stats.gpu_ms, stats.ms - stats.gpu_ms),
                    );
                }
                if let Some(r) = s.ready_ms {
                    row("open to statistics", format!("{r:.0} ms"));
                }
                let c = &s.s.full_res;
                row(
                    "full-resolution cache",
                    if c.started() {
                        format!(
                            "{}/{} dates, {:.2} GB, {}",
                            c.dates_done(),
                            s.info.t(),
                            c.bytes() as f64 / 1e9,
                            c.codec()
                        )
                    } else if c.error().is_empty() {
                        "off (series on an SSD)".into()
                    } else {
                        c.error()
                    },
                );
                if c.series_hits() > 0 {
                    row(
                        "series from the cache",
                        format!("{} ({:.2} ms the last)", c.series_hits(), c.last_series_ms()),
                    );
                }
                row("disk", if s.s.rotational { "spinning (HDD): one reader".into() } else { "SSD".to_string() });
            }
            let f = &self.perf;
            if let Some((p50, p95, max)) = f.summary() {
                row("frame CPU, interface (p50 / p95 / max)", format!("{p50:.2} / {p95:.2} / {max:.2} ms"));
            }
            if let Some(c) = f.last_cpu {
                row("frame CPU, whole (eframe)", format!("{:.2} ms", c * 1000.0));
            }
        });
    }
}
