//! The side panels: Display (date, mode, colormap, stretch, histogram), Layers (the series), Statistics (the
//! cursor's pixel) and Performance (the timings this phase is judged by, RUST_MIGRATION.md).

use egui::{Color32, ComboBox, DragValue, Grid, Rect, Sense, Slider, Ui, vec2};
use egui_plot::{Bar, BarChart, Plot, VLine};
use janus_gpu::{Mode, colormaps};

use crate::app::State;
use crate::display::mode_name;

/// A colormap's gradient, `width` × 14 points.
fn gradient(ui: &mut Ui, cmap: usize, width: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 14.0), Sense::click());
    let cm = &colormaps::all()[cmap];
    let n = if cm.qualitative { cm.keys().count() } else { 48 };
    for i in 0..n {
        let c = if cm.qualitative { cm.keys().nth(i).unwrap() } else { cm.sample(i as f32 / (n - 1) as f32) };
        let x0 = rect.min.x + rect.width() * i as f32 / n as f32;
        let r =
            Rect::from_min_max(egui::pos2(x0, rect.min.y), egui::pos2(x0 + rect.width() / n as f32 + 0.5, rect.max.y));
        ui.painter().rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
    }
    resp
}

fn date_slider(ui: &mut Ui, label: &str, t: &mut usize, labels: &[String]) -> bool {
    let n = labels.len();
    ui.horizontal(|ui| {
        ui.label(label);
        let r = ui.add(Slider::new(t, 0..=n - 1).show_value(false));
        ui.label(&labels[*t]);
        r.changed()
    })
    .inner
}

impl State {
    pub fn display_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else {
            ui.weak("No series open.");
            return;
        };
        let labels: Vec<String> = s.info.layers.iter().map(|l| l.label.clone()).collect();
        let d = &mut self.display;
        let n = labels.len();
        let info = s.info.clone();
        let wrap = |t: i64| ((t % n as i64 + n as i64) % n as i64) as usize;
        ui.horizontal(|ui| {
            // the date before or after, in the season
            if ui.button("<").clicked() {
                d.t = wrap(d.step_date(&info, d.t, -1));
            }
            ui.add(Slider::new(&mut d.t, 0..=n - 1).show_value(false));
            if ui.button(">").clicked() {
                d.t = wrap(d.step_date(&info, d.t, 1));
            }
            ui.strong(&labels[d.t]);
        });
        s.s.overview.set_focus(d.t);
        ui.horizontal(|ui| {
            ui.checkbox(&mut d.playing, "Play (P)");
            ui.weak(format!("date {} of {n}", d.t + 1));
        });
        ui.separator();

        let stats_ready = s.stats.is_some();
        ComboBox::from_label("Mode").width(220.0).selected_text(mode_name(d.mode)).show_ui(ui, |ui| {
            for m in Mode::ALL {
                let enabled = !m.needs_stats() || stats_ready;
                ui.add_enabled_ui(enabled, |ui| ui.selectable_value(&mut d.mode, m, mode_name(m)));
            }
        });
        match d.mode {
            Mode::Diff => {
                date_slider(ui, "Reference", &mut d.diff_ref, &labels);
            }
            Mode::Rgb => {
                for (i, c) in ["R", "G", "B"].iter().enumerate() {
                    date_slider(ui, c, &mut d.rgb[i], &labels);
                }
            }
            _ => {}
        }

        let (changed, turned_on) = self.classes.ui(ui, s);
        if changed {
            self.map.dirty = true;
        }
        if turned_on {
            d.mode = Mode::Value;
        }
        if d.mode == Mode::Composite && crate::season::temporal_options_ui(ui, d, &info) {
            self.map.dirty = true;
        }
        if crate::season::season_ui(ui, d, &info) {
            self.map.dirty = true;
        }
        let categorical = self.classes.on() && d.mode == Mode::Value;
        if d.mode != Mode::Rgb && !categorical {
            let cmap = &mut d.cmap[d.mode as usize];
            ui.horizontal(|ui| {
                ComboBox::from_id_salt("cmap").width(160.0).selected_text(colormaps::all()[*cmap].name).show_ui(
                    ui,
                    |ui| {
                        for (i, c) in colormaps::all().iter().enumerate() {
                            ui.horizontal(|ui| {
                                if gradient(ui, i, 90.0).clicked() {
                                    *cmap = i;
                                }
                                ui.selectable_value(cmap, i, c.name);
                            });
                        }
                    },
                );
                gradient(ui, *cmap, 120.0);
            });
        }
        let r = &mut d.ranges[d.mode as usize];
        ui.horizontal(|ui| {
            ui.label("Range");
            let speed = ((r.hi - r.lo).abs() / 200.0).max(1e-6);
            let a = ui.add(DragValue::new(&mut r.lo).speed(speed).max_decimals(4)).changed();
            let b = ui.add(DragValue::new(&mut r.hi).speed(speed).max_decimals(4)).changed();
            if a || b {
                r.manual = true;
            }
            if ui.add_enabled(r.manual, egui::Button::new("Auto")).clicked() {
                r.manual = false;
            }
        });

        // the histogram of the values shown, with the stretch
        let (lo, hi) = (f64::from(r.lo), f64::from(r.hi));
        let h = &d.hist;
        let bars: Vec<Bar> = h.xs.iter().zip(&h.ys).map(|(&x, &y)| Bar::new(x, y).width(h.bar_w)).collect();
        Plot::new("hist")
            .height(90.0)
            .show_axes([true, false])
            .show_grid(false)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .show_x(false)
            .show_y(false)
            .show(ui, |p| {
                p.bar_chart(BarChart::new("values", bars).color(Color32::from_rgb(230, 179, 0)));
                for v in [lo, hi] {
                    p.vline(VLine::new("range", v).color(Color32::WHITE).width(1.0));
                }
            });
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
                        ui.colored_label(c.color, egui::RichText::new(&c.name).strong());
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
                        let r = self.display.diff_ref.min(info.t() - 1);
                        ui.weak(format!("Difference vs {}", info.layers[r].label));
                        for c in &cols {
                            let v = c.values.or(c.roi.then_some(sr.roi.mean.as_slice()));
                            let d = v
                                .filter(|_| c.cube.is_none())
                                .and_then(|v| Some(v.get(t)? - v.get(r)?))
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
