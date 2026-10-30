//! Analysis > Cycles: a series' cycles over time, as its wavelet scalogram (wavelet.rs): the dates across, the
//! period down (two steps at the top, half the series' length at the bottom, on a log scale), the power in colour.
//! A yearly cycle is a band at 1 year; a second crop, a band at 6 months from the year it began; a clearing, the
//! band broken. Outlined: power above red noise at 95 %; darkened: the cone of influence, where the series' ends
//! weigh on the power; pale: the dates interpolated across a gap. At the right the global spectrum (the power's
//! mean over time) and its 95 % level. The series is the chart's: the cursor's, a pin's or the ROI's mean, the dates
//! the quality band masks left out.

use std::hash::{DefaultHasher, Hash, Hasher};

use egui::{Color32, Stroke, Ui, vec2};
use egui_plot::{AxisHints, GridMark, HLine, Line, LineStyle, Plot, PlotImage, PlotPoint, PlotPoints, VLine};
use janus_cube::multidim::format_g;

use crate::app::State;
use crate::theme;
use crate::wavelet::{DJ, Scalogram, YEAR_DAYS, scalogram};

const ORANGE: Color32 = Color32::from_rgb(255, 153, 51);
const MONTH_DAYS: f64 = YEAR_DAYS / 12.0;

/// The period axis' marks: days, and their names.
const MARKS: [(f64, &str); 14] = [
    (7.0, "1 week"),
    (14.0, "2 weeks"),
    (MONTH_DAYS, "1 month"),
    (2.0 * MONTH_DAYS, "2 months"),
    (3.0 * MONTH_DAYS, "3 months"),
    (6.0 * MONTH_DAYS, "6 months"),
    (YEAR_DAYS, "1 year"),
    (2.0 * YEAR_DAYS, "2 years"),
    (4.0 * YEAR_DAYS, "4 years"),
    (8.0 * YEAR_DAYS, "8 years"),
    (16.0 * YEAR_DAYS, "16 years"),
    (32.0 * YEAR_DAYS, "32 years"),
    (64.0 * YEAR_DAYS, "64 years"),
    (128.0 * YEAR_DAYS, "128 years"),
];

/// The tab's state.
pub struct CyclesUi {
    source: String,
    /// the linear trend taken out first (a trend is power at the longest periods, over everything else)
    pub detrend: bool,
    /// the scalogram shown and what it was computed for (a hash of the dates, the values and `detrend`)
    shown: Option<(u64, Result<Scalogram, String>)>,
    /// its image, and the colours' range (log2 of the power)
    image: Option<(u64, egui::TextureHandle, f32, f32)>,
}

impl Default for CyclesUi {
    fn default() -> Self {
        Self { source: String::new(), detrend: true, shown: None, image: None }
    }
}

/// A period's text: days, months or years.
fn period_text(days: f64) -> String {
    if days < 61.0 {
        format!("{days:.0} days")
    } else if days < 1.5 * YEAR_DAYS {
        format!("{:.1} months", days / MONTH_DAYS)
    } else {
        format!("{:.1} years", days / YEAR_DAYS)
    }
}

/// A period's place on the plot's Y: its log2, the short ones at the top.
fn y_of(days: f64) -> f64 {
    -days.log2()
}

fn key_of(times: &[f64], values: &[f32], detrend: bool) -> u64 {
    let mut h = DefaultHasher::new();
    times.iter().for_each(|t| t.to_bits().hash(&mut h));
    values.iter().for_each(|v| v.to_bits().hash(&mut h));
    detrend.hash(&mut h);
    h.finish()
}

/// The value at quantile `q` (0..=1).
fn quantile(v: &mut [f32], q: f64) -> f32 {
    let k = ((v.len() - 1) as f64 * q).round() as usize;
    *v.select_nth_unstable_by(k, f32::total_cmp).1
}

/// The colours' range of a scalogram: log2 of its power from its 2nd percentile (8 octaves below the top at most)
/// to its 99.5th (2 at least: white noise's power, 1, low on the scale).
fn power_range(s: &Scalogram) -> (f32, f32) {
    let mut logs: Vec<f32> = s.power.iter().map(|p| p.max(1e-12).log2()).collect();
    let hi = quantile(&mut logs, 0.995).max(1.0);
    let lo = quantile(&mut logs, 0.02).max(hi - 8.0).min(hi - 2.0);
    (lo, hi)
}

/// The scalogram as an image (a row a scale, the shortest at the top; a column a step): the power's log2 in
/// viridis, darkened in the cone of influence, paled across the gaps.
fn image_of(s: &Scalogram, (lo, hi): (f32, f32)) -> egui::ColorImage {
    let lut = janus_gpu::colormaps::all()[janus_gpu::colormaps::VIRIDIS].lut();
    let rows = s.periods.len();
    let mut pixels = Vec::with_capacity(s.n * rows);
    for j in 0..rows {
        for i in 0..s.n {
            let t = (s.at(j, i).max(1e-12).log2() - lo) / (hi - lo);
            let c = lut[((t.clamp(0.0, 1.0) * 255.0).round() as usize).min(255)];
            let mut rgb = [f32::from(c[0]), f32::from(c[1]), f32::from(c[2])];
            if s.filled[i] {
                let g = 0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2];
                rgb = rgb.map(|v| v + 0.7 * (g - v));
            }
            if s.periods[j] > s.coi[i] {
                rgb = rgb.map(|v| v * 0.45);
            }
            pixels.push(Color32::from_rgb(rgb[0] as u8, rgb[1] as u8, rgb[2] as u8));
        }
    }
    egui::ColorImage { size: [s.n, rows], pixels, source_size: vec2(s.n as f32, rows as f32) }
}

/// The contours where `f` (rows × columns, row-major) crosses 0, as segments between `(column, row)` positions
/// (marching squares; a saddle split by its centre).
fn contours(f: &[f64], rows: usize, cols: usize) -> Vec<[[f64; 2]; 2]> {
    let mut out = Vec::new();
    if rows < 2 || cols < 2 {
        return out;
    }
    let at = |j: usize, i: usize| f[j * cols + i];
    // where an edge from (x0, y0) at a to (x1, y1) at b crosses 0
    let cross = |(x0, y0, a): (f64, f64, f64), (x1, y1, b): (f64, f64, f64)| {
        let u = a / (a - b);
        [x0 + u * (x1 - x0), y0 + u * (y1 - y0)]
    };
    for j in 0..rows - 1 {
        for i in 0..cols - 1 {
            let (x, y) = (i as f64, j as f64);
            // the corners in turn: top left, top right, bottom right, bottom left
            let c = [
                (x, y, at(j, i)),
                (x + 1.0, y, at(j, i + 1)),
                (x + 1.0, y + 1.0, at(j + 1, i + 1)),
                (x, y + 1.0, at(j + 1, i)),
            ];
            if c.iter().any(|p| !p.2.is_finite()) {
                continue;
            }
            // the edges crossed: 0 top, 1 right, 2 bottom, 3 left
            let e: Vec<Option<[f64; 2]>> = (0..4)
                .map(|k| ((c[k].2 >= 0.0) != (c[(k + 1) % 4].2 >= 0.0)).then(|| cross(c[k], c[(k + 1) % 4])))
                .collect();
            let hits: Vec<[f64; 2]> = e.iter().flatten().copied().collect();
            match hits.len() {
                2 => out.push([hits[0], hits[1]]),
                4 => {
                    let centre = c.iter().map(|p| p.2).sum::<f64>() / 4.0;
                    let (e0, e1, e2, e3) = (e[0].unwrap(), e[1].unwrap(), e[2].unwrap(), e[3].unwrap());
                    if (centre >= 0.0) == (c[0].2 >= 0.0) {
                        out.extend([[e0, e1], [e2, e3]]);
                    } else {
                        out.extend([[e3, e0], [e1, e2]]);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

impl State {
    /// Analysis > Cycles.
    pub(crate) fn cycles_tab(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        if info.stack || info.spectral {
            ui.weak("A stack of variables or a spectrum: its bands have no dates.");
            return;
        }
        if !info.time_is_date {
            ui.weak("The dates of this series are not known (it is ordered by index).");
            return;
        }
        // the series: the cursor, a pin or the ROI mean, as in the chart
        let mut sources: Vec<(String, String, Vec<f32>)> = Vec::new();
        if let Some(h) = &self.series.hover {
            sources.push(("cursor".into(), format!("Cursor ({}, {})", h.x, h.y), h.values.clone()));
        }
        for p in &self.series.pins {
            sources.push((format!("pin{}", p.id), format!("Pin {} ({}, {})", p.id, p.x, p.y), p.values.clone()));
        }
        if !self.series.roi.mean.is_empty() {
            sources.push(("roi".into(), "ROI mean".into(), self.series.roi.mean.clone()));
        }
        if sources.is_empty() {
            ui.weak("Hover over the map, place a pin or draw an ROI.");
            return;
        }
        let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let current = times[self.display.t.min(times.len() - 1)];
        let c = &mut self.cycles;
        let src = sources.iter().position(|s| s.0 == c.source).unwrap_or(0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("cycles source")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 13.0)
                .selected_text(&sources[src].1)
                .show_ui(ui, |ui| {
                    for s in &sources {
                        if ui.selectable_label(s.0 == sources[src].0, &s.1).clicked() {
                            c.source = s.0.clone();
                        }
                    }
                })
                .response
                .on_hover_text("The series analysed (the cursor follows the mouse over the map)");
            ui.checkbox(&mut c.detrend, "Trend out").on_hover_text(
                "The series' linear trend taken out first: a trend is power at the longest periods, which would \
                 dwarf the cycles. Off: the trend kept (a slow change shows as power at long periods).",
            );
        });
        let v = &sources[src].2;
        if v.len() != times.len() {
            return;
        }
        let key = key_of(&times, v, c.detrend);
        if c.shown.as_ref().is_none_or(|s| s.0 != key) {
            c.shown = Some((key, scalogram(&times, v, c.detrend)));
        }
        let s = match &c.shown.as_ref().unwrap().1 {
            Ok(s) => s,
            Err(why) => {
                ui.weak(why);
                return;
            }
        };
        if c.image.as_ref().is_none_or(|t| t.0 != key) {
            let range = power_range(s);
            let tex = ui.ctx().load_texture("cycles", image_of(s, range), egui::TextureOptions::LINEAR);
            c.image = Some((key, tex, range.0, range.1));
        }
        let (tex, lo, hi) = c.image.as_ref().map(|t| (t.1.id(), t.2, t.3)).unwrap();
        ui.weak(format!(
            "{} dates, a step of {:.0} days. Power: over the series' variance ({}; white noise 1). Outlined: above \
             red noise at 95 % (lag-1 correlation {:.2}). Darkened: the cone of influence, where the ends weigh on \
             the power. Pale: across a gap.",
            s.observations,
            s.dt / 86400.0,
            format_g(s.variance, 3),
            s.alpha
        ));

        let (n, rows) = (s.n, s.periods.len());
        let (x0, x1) = (s.time(0) - 0.5 * s.dt, s.time(n - 1) + 0.5 * s.dt);
        let (ytop, ybot) = (y_of(s.periods[0]) + 0.5 * DJ, y_of(s.periods[rows - 1]) - 0.5 * DJ);
        let marks: Vec<(f64, &str)> =
            MARKS.iter().map(|&(d, l)| (y_of(d), l)).filter(|&(y, _)| y <= ytop && y >= ybot).collect();
        let body = ui.text_style_height(&egui::TextStyle::Body);
        let (global_w, scale_w) = (body * 8.0, body * 6.0);
        let size = vec2((ui.available_width() - global_w - scale_w).max(160.0), ui.available_height().max(120.0));
        let weak = ui.visuals().weak_text_color();
        let text = ui.visuals().text_color();
        // the image's row of a Y and column of an X, if inside
        let cell = |x: f64, y: f64| {
            let i = ((x - s.t0) / s.dt).round();
            let j = ((-y - s.periods[0].log2()) / DJ).round();
            (i >= 0.0 && (i as usize) < n && j >= 0.0 && (j as usize) < rows).then_some((j as usize, i as usize))
        };
        ui.horizontal_top(|ui| {
            let (label_marks, grid_marks) = (marks.clone(), marks.clone());
            let period_axis = AxisHints::new_y()
                .label("period")
                .formatter(move |m, _| {
                    label_marks.iter().find(|k| (k.0 - m.value).abs() < 1e-9).map_or(String::new(), |k| k.1.into())
                })
                .label_spacing(12.0..=13.0)
                .min_thickness(body * 4.5);
            let plot = Plot::new(("cycles", info.id))
                .width(size.x)
                .height(size.y)
                .include_x(x0)
                .include_x(x1)
                .include_y(ybot)
                .include_y(ytop)
                .y_grid_spacer(move |_| grid_marks.iter().map(|k| GridMark { value: k.0, step_size: 1.0 }).collect())
                .custom_y_axes(vec![period_axis])
                .set_margin_fraction(egui::Vec2::ZERO)
                .show_background(false)
                .show_grid(false)
                .allow_drag(false)
                .allow_scroll(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .show_x(false)
                .show_y(false);
            let r = crate::chart::time_axis(plot, Some(weak)).show(ui, |p| {
                let centre = PlotPoint::new(0.5 * (x0 + x1), 0.5 * (ytop + ybot));
                p.image(
                    PlotImage::new("", tex, centre, vec2((x1 - x0) as f32, (ytop - ybot) as f32)).allow_hover(false),
                );
                // a year and half of one
                for d in [YEAR_DAYS, 0.5 * YEAR_DAYS] {
                    if (ybot..=ytop).contains(&y_of(d)) {
                        p.hline(
                            HLine::new("", y_of(d))
                                .color(Color32::from_white_alpha(110))
                                .width(1.0)
                                .style(LineStyle::dashed_loose())
                                .allow_hover(false),
                        );
                    }
                }
                // the cone of influence's edge (within the image)
                let edge: Vec<[f64; 2]> = (0..n).map(|i| [s.time(i), y_of(s.coi[i]).clamp(ybot, ytop)]).collect();
                p.line(
                    Line::new("", PlotPoints::from(edge))
                        .color(Color32::from_white_alpha(170))
                        .width(1.2)
                        .style(LineStyle::dashed_dense())
                        .allow_hover(false),
                );
                p.vline(VLine::new("", current).color(theme::on_plot(ORANGE)).width(2.0).allow_hover(false));
                p.pointer_coordinate()
            });
            // the 95 % contours, over the image
            let t = r.transform;
            let painter = ui.painter_at(*t.frame());
            let ratio: Vec<f64> = (0..rows * n).map(|k| f64::from(s.power[k]) / s.signif[k / n] - 1.0).collect();
            let stroke = Stroke::new(1.4, Color32::BLACK);
            for [a, b] in contours(&ratio, rows, n) {
                let at = |p: [f64; 2]| {
                    let y = y_of(s.periods[0]) - p[1] * DJ;
                    t.position_from_point(&PlotPoint::new(s.time(0) + p[0] * s.dt, y))
                };
                painter.line_segment([at(a), at(b)], stroke);
            }
            if let Some(m) = r.inner.filter(|_| r.response.hovered())
                && let Some((j, i)) = cell(m.x, m.y)
            {
                let p = s.at(j, i);
                let mut text = format!(
                    "{}\nperiod {}\npower {} ({:.1} × the 95 % level)",
                    info.format_time(s.time(i)),
                    period_text(s.periods[j]),
                    format_g(f64::from(p), 3),
                    f64::from(p) / s.signif[j]
                );
                if s.periods[j] > s.coi[i] {
                    text += "\nin the cone of influence: the series' ends weigh on it";
                }
                if s.filled[i] {
                    text += "\ninterpolated across a gap";
                }
                r.response.on_hover_text(text);
            }

            // the global spectrum, on the same periods
            let gx = s.global.iter().chain(&s.global_signif).fold(1.0f64, |a, &b| a.max(b)) * 1.05;
            // its axis: 0 and two or three round steps (the panel is narrow)
            let step = {
                let raw = gx / 2.5;
                let p = 10f64.powf(raw.log10().floor());
                [1.0, 2.0, 5.0, 10.0].iter().map(|m| m * p).find(|&v| v >= raw).unwrap_or(10.0 * p)
            };
            let g = Plot::new(("cycles global", info.id))
                .width(global_w)
                .height(size.y)
                .include_x(0.0)
                .include_x(gx)
                .include_y(ybot)
                .include_y(ytop)
                .x_grid_spacer(move |_| {
                    (0..)
                        .map(|k| k as f64 * step)
                        .take_while(|&v| v <= gx)
                        .map(|value| GridMark { value, step_size: step })
                        .collect()
                })
                .custom_x_axes(vec![
                    AxisHints::new_x()
                        .label("mean power")
                        .tick_label_color(weak)
                        .formatter(|m, _| format_g(m.value, 3))
                        .label_spacing(16.0..=17.0),
                ])
                .show_axes([true, false])
                .show_grid([false, false])
                .set_margin_fraction(egui::Vec2::ZERO)
                .show_background(false)
                .allow_drag(false)
                .allow_scroll(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .show_x(false)
                .show_y(false)
                .show(ui, |p| {
                    let ys: Vec<f64> = s.periods.iter().map(|&d| y_of(d)).collect();
                    let line =
                        |v: &[f64]| PlotPoints::from(v.iter().zip(&ys).map(|(&x, &y)| [x, y]).collect::<Vec<_>>());
                    p.line(
                        Line::new("", line(&s.global_signif))
                            .color(theme::on_plot(Color32::from_rgb(214, 39, 40)))
                            .width(1.2)
                            .style(LineStyle::dashed_loose())
                            .allow_hover(false),
                    );
                    p.line(Line::new("", line(&s.global)).color(text).width(1.6).allow_hover(false));
                    p.pointer_coordinate()
                });
            if let Some(m) = g.inner.filter(|_| g.response.hovered()) {
                let j = ((-m.y - s.periods[0].log2()) / DJ).round();
                if j >= 0.0 && (j as usize) < rows {
                    let j = j as usize;
                    g.response.on_hover_text(format!(
                        "period {}\nmean power {} (95 % level {}, dashed)",
                        period_text(s.periods[j]),
                        format_g(s.global[j], 3),
                        format_g(s.global_signif[j], 3)
                    ));
                }
            }
            crate::analysis::colour_scale(
                ui,
                size.y,
                janus_gpu::colormaps::VIRIDIS,
                f64::from(lo),
                f64::from(hi),
                &|v| format_g(2f64.powf(v), 2),
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_circle_contour() {
        // a bump in the middle of a 5 × 5 grid: one closed ring of 4 segments around it
        let f: Vec<f64> = (0..25).map(|k| if k == 12 { 1.0 } else { -1.0 }).collect();
        let segs = contours(&f, 5, 5);
        assert_eq!(segs.len(), 4);
        for [a, b] in &segs {
            for p in [a, b] {
                // half way between the bump and its neighbours
                assert!((p[0] - 2.0).abs() <= 0.5 + 1e-9 && (p[1] - 2.0).abs() <= 0.5 + 1e-9);
            }
        }
        // nothing crosses: none
        assert!(contours(&[1.0; 9], 3, 3).is_empty());
    }

    #[test]
    fn periods_read() {
        assert_eq!(period_text(16.0), "16 days");
        assert_eq!(period_text(YEAR_DAYS / 2.0), "6.0 months");
        assert_eq!(period_text(2.0 * YEAR_DAYS), "2.0 years");
        assert!(y_of(YEAR_DAYS) < y_of(MONTH_DAYS));
    }
}
