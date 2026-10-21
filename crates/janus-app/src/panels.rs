//! The side panels: Display (date, mode, colormap, stretch, histogram), Layers (the series), Statistics (the
//! cursor's pixel) and Performance (the timings this phase is judged by, RUST_MIGRATION.md).

use egui::{Color32, ComboBox, DragValue, Grid, Rect, Sense, Slider, Ui, vec2};
use egui_plot::{Bar, BarChart, Plot, VLine};
use janus_gpu::{Mode, colormaps};

use crate::app::State;
use crate::display::{REDUCERS, mode_name};

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
        ui.horizontal(|ui| {
            if ui.button("<").clicked() && d.t > 0 {
                d.t -= 1;
            }
            ui.add(Slider::new(&mut d.t, 0..=n - 1).show_value(false));
            if ui.button(">").clicked() && d.t + 1 < n {
                d.t += 1;
            }
            ui.strong(&labels[d.t]);
        });
        s.overview.set_focus(d.t);
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
            Mode::Composite => {
                ComboBox::from_label("Reducer").selected_text(REDUCERS[d.reducer as usize]).show_ui(ui, |ui| {
                    for (i, r) in REDUCERS.iter().enumerate() {
                        ui.selectable_value(&mut d.reducer, i as i32, *r);
                    }
                });
                ui.checkbox(&mut d.whole_series, "The whole series (else the date's year)");
            }
            _ => {}
        }

        if d.mode != Mode::Rgb {
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

    pub fn layers_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else {
            ui.weak("No series open.");
            return;
        };
        let i = &s.info;
        Grid::new("series").num_columns(2).striped(true).show(ui, |ui| {
            let mut row = |k: &str, v: String| {
                ui.weak(k);
                ui.label(v);
                ui.end_row();
            };
            row("series", i.description.clone());
            row("size", format!("{} x {} x {} {}", i.width, i.height, i.t(), i.data_type));
            row("driver", i.driver.clone());
            row(
                "CRS",
                if i.crs_authority.is_empty() {
                    i.crs_name.clone()
                } else {
                    format!("{} ({})", i.crs_name, i.crs_authority)
                },
            );
            if let (Some(a), Some(b)) = (i.layers.first(), i.layers.last()) {
                row("dates", format!("{} to {}", a.label, b.label));
            }
            let ov = &s.overview;
            row("overview", format!("{} x {} (1:{:.2})", ov.w, ov.h, ov.factor));
            row("cache", ov.cache_path().file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
        });
    }

    pub fn statistics_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        let at = self.hover.or(self.pins.last().map(|p| p.at));
        let Some(at) = at else {
            ui.weak("Hover the map for the pixel's statistics.");
            return;
        };
        let ov = &s.overview;
        let p = at.ov.1 * ov.w + at.ov.0;
        ui.weak(format!("pixel ({}, {}), overview ({}, {})", at.src.0, at.src.1, at.ov.0, at.ov.1));
        Grid::new("pixel stats").num_columns(2).striped(true).show(ui, |ui| {
            let mut row = |k: &str, v: f32| {
                ui.weak(k);
                ui.label(if v.is_nan() { "-".to_string() } else { format!("{v:.4}") });
                ui.end_row();
            };
            row("value at the date", ov.layer(self.display.t).map_or(f32::NAN, |l| l[p]));
            if let Some(st) = &s.stats {
                let (s0, s1) = (st.s0[p], st.s1[p]);
                row("mean", s0[0]);
                row("std. deviation", s0[1]);
                row("trend (per year)", s0[2]);
                row("observations", s0[3]);
                row("minimum", s1[0]);
                row("maximum", s1[1]);
                row("trend R²", s1[2]);
                row("largest drop", s1[3]);
            }
        });
        if let Some(st) = &s.stats {
            let dt = st.s2[p];
            if !dt.is_nan() {
                ui.weak(format!("largest drop at {}", s.info.layers[dt as usize].label));
            }
        }
        if let Some(e) = s.hover_exact(at.src.0, at.src.1).or_else(|| s.pin_exact(at.src.0, at.src.1)) {
            let valid: Vec<f32> = e.values.iter().copied().filter(|v| !v.is_nan()).collect();
            let mean = valid.iter().sum::<f32>() / valid.len().max(1) as f32;
            ui.separator();
            ui.weak(format!(
                "exact series: {} of {} dates, mean {mean:.4}, read in {:.1} ms",
                valid.len(),
                e.values.len(),
                e.ms
            ));
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
                let ov = &s.overview;
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
