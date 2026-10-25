//! The Analysis panel (src/app_analysis.cpp; View > Analysis): views that need many dates or two layers at once.
//! Seasonal: a series by day of the year (one line per year, the mean of every year by month, a year x
//! day-of-year heatmap). Classes: the share of each class at every date and the transitions between two dates
//! (categorical layers). Scatter: the values of two layers or two dates, pixel by pixel, in the ROI or the whole
//! image. Classes and scatter use the overview (what the map shows before zooming in), so they are immediate on
//! any series.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Stroke, Ui, vec2};
use egui_plot::{FilledArea, GridMark, Legend, Line, Plot, PlotPoint, PlotPoints, VLine};
use janus_cube::multidim::format_g;
use janus_cube::stats::{observations_per_year, season_year_and_day, seasonal_grid};
use janus_gpu::Mode;

use crate::app::State;
use crate::theme;

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// day of the year each month starts
const MONTH_START: [f64; 13] = [1.0, 32.0, 60.0, 91.0, 121.0, 152.0, 182.0, 213.0, 244.0, 274.0, 305.0, 335.0, 367.0];
const ORANGE: Color32 = Color32::from_rgb(255, 153, 51);

fn month_of(day: i32) -> usize {
    (0..11).take_while(|&m| f64::from(day) >= MONTH_START[m + 1]).count()
}

/// A seasonal year's name: 2019, or 2019/20 when it starts after January.
fn year_label(y: i32, start: usize) -> String {
    if start == 0 { format!("{y}") } else { format!("{y}/{:02}", (y + 1).rem_euclid(100)) }
}

pub(crate) fn percent(share: f64) -> String {
    if share >= 0.0995 { format!("{:.1}%", share * 100.0) } else { format!("{:.2}%", share * 100.0) }
}

/// The colours of a colormap, 256 of them.
fn lut(cmap: usize) -> [[u8; 4]; 256] {
    janus_gpu::colormaps::all()[cmap].lut()
}

fn lut_at(l: &[[u8; 4]; 256], t: f32) -> Color32 {
    let c = l[((t.clamp(0.0, 1.0) * 255.0).round() as usize).min(255)];
    Color32::from_rgb(c[0], c[1], c[2])
}

/// A vertical colour scale next to a plot: the colormap from `lo` (bottom) to `hi` (top), its ends labelled.
fn colour_scale(ui: &mut Ui, height: f32, cmap: usize, lo: f64, hi: f64, label: &dyn Fn(f64) -> String) {
    let (rect, _) =
        ui.allocate_exact_size(vec2(ui.text_style_height(&egui::TextStyle::Body) * 5.5, height), egui::Sense::hover());
    let bar = Rect::from_min_size(rect.min + vec2(2.0, 8.0), vec2(14.0, (rect.height() - 16.0).max(10.0)));
    let l = lut(cmap);
    let p = ui.painter();
    let n = 64;
    for k in 0..n {
        let y0 = bar.bottom() - bar.height() * k as f32 / n as f32;
        let y1 = bar.bottom() - bar.height() * (k + 1) as f32 / n as f32;
        p.rect_filled(Rect::from_x_y_ranges(bar.x_range(), y1..=y0), 0.0, lut_at(&l, (k as f32 + 0.5) / n as f32));
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let col = ui.visuals().text_color();
    p.text(Pos2::new(bar.right() + 4.0, bar.top()), egui::Align2::LEFT_CENTER, label(hi), font.clone(), col);
    p.text(Pos2::new(bar.right() + 4.0, bar.bottom()), egui::Align2::LEFT_CENTER, label(lo), font, col);
}

/// The share of each class at every date, counted a few dates a frame.
#[derive(Default)]
pub struct ClassTimeline {
    pub(crate) key: u64,
    pub values: Vec<i32>,
    /// [t][k]
    pub counts: Vec<Vec<u32>>,
    pub valid: Vec<u64>,
    /// dates counted
    pub next: usize,
}

/// Pixels that went from class i to class j between two dates.
#[derive(Default)]
pub struct Transitions {
    pub(crate) key: u64,
    pub from: usize,
    pub to: usize,
    pub values: Vec<i32>,
    /// [i][j], K x K
    pub m: Vec<u32>,
    pub both: u64,
    pub done: bool,
}

/// Two layers' (or dates') values pixel by pixel, and how they relate.
#[derive(Default)]
pub struct ScatterData {
    key: u64,
    pub x: Vec<f64>,
    pub y: Vec<f64>,
    pub n: usize,
    /// Pearson's r, y = a + b x, mean(y - x), its RMS
    pub r: f64,
    pub a: f64,
    pub b: f64,
    pub mean_diff: f64,
    pub rmsd: f64,
    /// a pixel every `step` along each axis
    pub step: usize,
}

#[derive(Default)]
pub struct AnalysisUi {
    pub show: bool,
    /// brought to the front (a button elsewhere opened one of its tabs)
    pub focus: bool,
    /// 0 seasonal, 1 classes, 2 scatter, 3 the trend test
    pub tab: usize,
    seasonal_source: String,
    /// 0 years overlaid, 1 the heatmap
    seasonal_view: usize,
    /// the month the seasonal year starts in (0 January: the calendar year)
    pub seasonal_start: usize,
    pub classes: ClassTimeline,
    class_stacked: bool,
    /// 0 the shares over time, 1 the flows between dates (sankey.rs)
    pub class_view: usize,
    /// the flows' dates (sorted) and their consecutive pairs' transitions
    pub flow_dates: Vec<usize>,
    pub flows: Vec<Transitions>,
    /// the flows: only the pixels that changed class; the zoom (1: the whole diagram in the panel's height) and
    /// how far down it is scrolled (points of the zoomed diagram)
    pub flow_changes: bool,
    pub flow_zoom: f32,
    pub flow_pan: f32,
    /// the classes grouped as Other: an index of sankey::SMALL; the diagram in a window of its own
    pub flow_small: usize,
    pub flow_window: bool,
    trans_from: usize,
    /// None: the current date
    trans_to: Option<usize>,
    pub trans: Transitions,
    /// X and Y: the layer (its cube id) and the date (-1 the current, -2 the one before it)
    scatter_cube: [u64; 2],
    scatter_date: [i64; 2],
    pub scatter: ScatterData,
    /// the series whose trajectory is drawn over the scatter (trajectory.rs): "off", "cursor" (also when empty),
    /// "pin<id>", "roi"
    pub trajectory: String,
    /// the scatter's density image (its key, the texture, the largest count)
    density: Option<(u64, egui::TextureHandle, f64)>,
    /// the trend test (PARTS) over a region (trend_test.rs)
    pub trend: crate::trend_test::TrendTestUi,
}

impl State {
    /// Analysis > Classes in front: the categorical layer's classes over time.
    pub(crate) fn open_class_timeline(&mut self) {
        self.analysis.show = true;
        self.analysis.tab = 1;
        self.analysis.focus = true;
    }

    /// The Analysis panel's tabs.
    pub(crate) fn analysis_ui(&mut self, ui: &mut Ui) {
        if self.session.is_none() {
            ui.weak("Open a series.");
            return;
        }
        ui.horizontal(|ui| {
            for (k, name) in ["Seasonal", "Classes", "Scatter", "Trend test"].iter().enumerate() {
                ui.selectable_value(&mut self.analysis.tab, k, *name);
            }
        });
        ui.separator();
        match self.analysis.tab {
            0 => self.seasonal_ui(ui),
            1 => self.class_timeline_ui(ui),
            2 => self.scatter_ui(ui),
            _ => self.trend_test_ui(ui),
        }
    }

    // -----------------------------------------------------------------------------------------------------------
    // Seasonal
    // -----------------------------------------------------------------------------------------------------------

    fn seasonal_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
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
        let a = &mut self.analysis;
        let src = sources.iter().position(|s| s.0 == a.seasonal_source).unwrap_or(0);
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("seasonal source")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 13.0)
                .selected_text(&sources[src].1)
                .show_ui(ui, |ui| {
                    for s in &sources {
                        if ui.selectable_label(s.0 == sources[src].0, &s.1).clicked() {
                            a.seasonal_source = s.0.clone();
                        }
                    }
                })
                .response
                .on_hover_text("The series shown (the cursor follows the mouse over the map)");
            ui.radio_value(&mut a.seasonal_view, 0, "Years overlaid");
            ui.radio_value(&mut a.seasonal_view, 1, "Year x day heatmap");
            ui.label("Year from");
            egui::ComboBox::from_id_salt("seasonal start")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 4.0)
                .selected_text(MONTHS[a.seasonal_start])
                .show_ui(ui, |ui| {
                    for (k, m) in MONTHS.iter().enumerate() {
                        ui.selectable_value(&mut a.seasonal_start, k, *m);
                    }
                })
                .response
                .on_hover_text(
                    "The month the year starts in: January, the calendar year; July or August, a hydrological year \
                     or a southern summer's crop (October to March) seen whole instead of split at the chart's \
                     edges. A year is named after the one it starts in (2019/20).",
                );
        });
        let v = &sources[src].2;
        let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        if v.len() != times.len() {
            return;
        }
        let per_year = observations_per_year(&times);
        if per_year < 2 {
            ui.weak("About one observation per year: nothing to compare within a year.");
            ui.weak("(For series with several dates a year: monthly, 16-day composites...)");
            return;
        }
        // the seasonal year: from the first day of its month, days 1 to 366 from there
        let s0 = self.analysis.seasonal_start;
        let start = MONTH_START[s0] as i32;
        let off = MONTH_START[s0] - 1.0;
        // a day of the calendar year (1 to 366) in the seasonal one
        let shift = move |d: f64| if d - off < 1.0 { d - off + 366.0 } else { d - off };
        let (cur_year, cur_day) = season_year_and_day(times[self.display.t.min(times.len() - 1)], start);
        let scale_w = ui.text_style_height(&egui::TextStyle::Body) * 6.0;
        let months = move |plot: Plot<'static>| {
            plot.x_grid_spacer(move |_| {
                // in order along the axis, from the seasonal year's first month
                (0..12).map(|k| GridMark { value: shift(MONTH_START[(k + s0) % 12]), step_size: 30.0 }).collect()
            })
            // every month named (egui_plot hides labels under 60 points apart: a narrow panel had none)
            .custom_x_axes(vec![
                egui_plot::AxisHints::new_x()
                    .label(if s0 == 0 { "day of the year" } else { "day of the seasonal year" })
                    .formatter(move |m, _| {
                        (0..12)
                            .find(|&k| shift(MONTH_START[k]) == m.value)
                            .map_or(String::new(), |k| MONTHS[k].to_string())
                    })
                    .label_spacing(20.0..=21.0),
            ])
            .include_x(1.0)
            .include_x(367.0)
        };
        if self.analysis.seasonal_view == 0 {
            // one line per year (colour = year), the current date's year on top; the mean of every year by month,
            // with +/- one standard deviation
            let (y0, y1) =
                (season_year_and_day(times[0], start).0, season_year_and_day(times[times.len() - 1], start).0);
            let viridis = lut(janus_gpu::colormaps::VIRIDIS);
            let mut years: Vec<(i32, Vec<[f64; 2]>)> = Vec::new();
            let (mut m_sum, mut m_sq, mut m_n) = ([0f64; 12], [0f64; 12], [0usize; 12]);
            for (&t, &x) in times.iter().zip(v) {
                let (y, d) = season_year_and_day(t, start);
                if years.last().is_none_or(|l| l.0 != y) {
                    years.push((y, Vec::new()));
                }
                if !x.is_finite() {
                    continue;
                }
                years.last_mut().unwrap().1.push([f64::from(d), f64::from(x)]);
                // its calendar month
                let m = month_of((f64::from(d) + off - 1.0) as i32 % 366 + 1);
                m_sum[m] += f64::from(x);
                m_sq[m] += f64::from(x) * f64::from(x);
                m_n[m] += 1;
            }
            let (mut mx, mut mean, mut lo, mut hi) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            // in the seasonal year's order
            for m in (0..12).map(|k| (k + s0) % 12) {
                if m_n[m] == 0 {
                    continue;
                }
                let n = m_n[m] as f64;
                let mu = m_sum[m] / n;
                let sd = if m_n[m] > 1 { ((m_sq[m] - n * mu * mu) / (n - 1.0)).max(0.0).sqrt() } else { 0.0 };
                mx.push(shift(MONTH_START[m]) + 0.5 * (MONTH_START[m + 1] - MONTH_START[m]));
                mean.push(mu);
                lo.push(mu - sd);
                hi.push(mu + sd);
            }
            let size = vec2(ui.available_width() - scale_w, ui.available_height());
            ui.horizontal_top(|ui| {
                ui.allocate_ui(size, |ui| {
                    months(Plot::new("seasonal years"))
                        .width(size.x)
                        .height(size.y)
                        .legend(Legend::default().position(egui_plot::Corner::LeftTop).background_alpha(0.6))
                        .show_background(false)
                        .allow_drag(false)
                        .allow_scroll(false)
                        .show(ui, |plot| {
                            let grey = theme::on_plot(Color32::from_gray(217));
                            if !mx.is_empty() {
                                let name = "Mean of all years (+/- 1 sd)";
                                plot.add(
                                    FilledArea::new(name, &mx, &lo, &hi)
                                        .fill_color(grey.gamma_multiply(0.15))
                                        .stroke(Stroke::NONE),
                                );
                                let pts: Vec<[f64; 2]> = mx.iter().zip(&mean).map(|(&x, &y)| [x, y]).collect();
                                plot.line(Line::new(name, PlotPoints::from(pts)).color(grey).width(2.0));
                            }
                            // the current year last: on top
                            years.sort_by_key(|(y, _)| *y == cur_year);
                            for (y, pts) in &years {
                                if pts.is_empty() {
                                    continue;
                                }
                                let current = *y == cur_year;
                                let k = if y1 > y0 { (y - y0) as f32 / (y1 - y0) as f32 } else { 0.5 };
                                let col = theme::on_plot(lut_at(&viridis, k));
                                // only the current year in the legend (an item without a name has no entry)
                                let name = if current {
                                    format!("{} (current date)", year_label(*y, s0))
                                } else {
                                    String::new()
                                };
                                let mut line = Line::new(name, PlotPoints::from(pts.clone()))
                                    .color(col)
                                    .width(if current { 2.5 } else { 1.0 });
                                if !current {
                                    line = line.allow_hover(false);
                                }
                                plot.line(line);
                                plot.points(
                                    egui_plot::Points::new(
                                        if current {
                                            format!("{} (current date)", year_label(*y, s0))
                                        } else {
                                            String::new()
                                        },
                                        PlotPoints::from(pts.clone()),
                                    )
                                    .color(col)
                                    .radius(if current { 3.0 } else { 1.5 }),
                                );
                            }
                        });
                });
                colour_scale(ui, size.y, janus_gpu::colormaps::VIRIDIS, f64::from(y0), f64::from(y1), &|v| {
                    year_label(v.round() as i32, s0)
                });
            });
            return;
        }
        // heatmap: years (newest on top) x parts of the year, in the layer's colours
        let bins = (per_year as usize).clamp(2, 46);
        let g = seasonal_grid(&times, v, bins, start);
        let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
        for &m in g.mean.iter().filter(|m| m.is_finite()) {
            lo = lo.min(m);
            hi = hi.max(m);
        }
        if lo.partial_cmp(&hi) != Some(std::cmp::Ordering::Less) {
            hi = lo + 1.0;
        }
        let cmap = self.display.cmap[Mode::Value as usize];
        let colours = lut(cmap);
        let size = vec2(ui.available_width() - scale_w, ui.available_height());
        ui.horizontal_top(|ui| {
            ui.allocate_ui(size, |ui| {
                let (y0, years) = (f64::from(g.year0), g.years as f64);
                let r = months(Plot::new("seasonal heat"))
                    .width(size.x)
                    .height(size.y)
                    .include_y(y0 - 0.5)
                    .include_y(y0 + years - 0.5)
                    .y_axis_formatter(move |m, _| year_label(m.value.round() as i32, s0))
                    .set_margin_fraction(egui::Vec2::ZERO)
                    .show_background(false)
                    .show_grid(false)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .allow_zoom(false)
                    .show(ui, |plot| plot.pointer_coordinate());
                let t = r.transform;
                let painter = ui.painter_at(*t.frame());
                let px = |x: f64, y: f64| t.position_from_point(&PlotPoint::new(x, y));
                let mut hover = None;
                for row in 0..g.years {
                    for b in 0..g.bins {
                        let k = row * g.bins + b;
                        let (x0, x1) =
                            (1.0 + 366.0 * b as f64 / g.bins as f64, 1.0 + 366.0 * (b + 1) as f64 / g.bins as f64);
                        let yb = f64::from(g.year0) + row as f64 - 0.5;
                        if let Some(m) = r.inner.filter(|_| r.response.hovered())
                            && m.x >= x0
                            && m.x < x1
                            && m.y >= yb
                            && m.y < yb + 1.0
                        {
                            hover = Some(k);
                        }
                        if !g.mean[k].is_finite() {
                            continue;
                        }
                        let c = lut_at(&colours, (g.mean[k] - lo) / (hi - lo));
                        painter.rect_filled(Rect::from_two_pos(px(x0, yb + 1.0), px(x1, yb)), 0.0, c);
                    }
                }
                // the current date's cell
                let cb = ((cur_day as usize - 1) * g.bins / 366).min(g.bins - 1);
                let (cx0, cx1) =
                    (1.0 + 366.0 * cb as f64 / g.bins as f64, 1.0 + 366.0 * (cb + 1) as f64 / g.bins as f64);
                painter.rect_stroke(
                    Rect::from_two_pos(px(cx0, f64::from(cur_year) + 0.5), px(cx1, f64::from(cur_year) - 0.5)),
                    0.0,
                    Stroke::new(2.0, theme::on_plot(ORANGE)),
                    egui::StrokeKind::Middle,
                );
                if let Some(k) = hover {
                    let (row, b) = (k / g.bins, k % g.bins);
                    let (d0, d1) = (1 + 366 * b / g.bins, 366 * (b + 1) / g.bins);
                    // the month of the part's first day, in the calendar
                    let month = MONTHS[month_of((d0 as f64 + off - 1.0) as i32 % 366 + 1)];
                    let head = format!("{}, days {d0}-{d1} ({month})", year_label(g.year0 + row as i32, s0));
                    let body = if g.mean[k].is_finite() {
                        format!("{} (mean of {})", format_g(f64::from(g.mean[k]), 4), g.n[k])
                    } else {
                        "no observation".into()
                    };
                    r.response.on_hover_text(format!("{head}\n{body}"));
                }
            });
            colour_scale(ui, size.y, cmap, f64::from(lo), f64::from(hi), &|v| format_g(v, 3));
        });
    }

    // -----------------------------------------------------------------------------------------------------------
    // Classes over time and transitions
    // -----------------------------------------------------------------------------------------------------------

    /// Counts a few dates of the class timeline (all of it in steps, a frame at a time, so a large overview never
    /// stalls the interface); true when complete.
    pub fn count_class_timeline(&mut self, budget_ms: f64) -> bool {
        let Some(s) = &self.session else { return false };
        let ov = &s.s.overview;
        let list = &self.classes.list;
        let mut key = s.info.id.wrapping_mul(1_000_003).wrapping_add(ov.t as u64 * 131).wrapping_add(list.len() as u64);
        for e in list {
            key = key.wrapping_mul(31).wrapping_add(e.value as u64);
        }
        let c = &mut self.analysis.classes;
        if c.key != key {
            *c = ClassTimeline {
                key,
                values: list.iter().map(|e| e.value).collect(),
                counts: vec![vec![0; list.len()]; ov.t],
                valid: vec![0; ov.t],
                next: 0,
            };
        }
        let index: HashMap<i32, usize> = c.values.iter().enumerate().map(|(k, &v)| (v, k)).collect();
        let t0 = std::time::Instant::now();
        while c.next < ov.t {
            let t = c.next;
            let Some(px) = ov.layer(t).filter(|_| s.uploaded.get(t).copied().unwrap_or(false)) else {
                return false; // the overview is still reading it
            };
            let n = &mut c.counts[t];
            let (mut last_v, mut last_k) = (i32::MIN, None);
            for &p in px {
                if !p.is_finite() {
                    continue;
                }
                let v = p.round() as i32;
                if v != last_v {
                    last_v = v;
                    last_k = index.get(&v).copied();
                }
                if let Some(k) = last_k {
                    n[k] += 1;
                    c.valid[t] += 1;
                }
            }
            c.next += 1;
            if t0.elapsed().as_secs_f64() * 1000.0 > budget_ms {
                break;
            }
        }
        c.next >= ov.t
    }

    /// The pixels of each class at `from` and at `to` (the overview's).
    pub fn count_transitions(&self, from: usize, to: usize) -> Transitions {
        let mut tr =
            Transitions { from, to, values: self.classes.list.iter().map(|e| e.value).collect(), ..Default::default() };
        let k_n = tr.values.len();
        tr.m = vec![0; k_n * k_n];
        let Some(s) = &self.session else { return tr };
        let ov = &s.s.overview;
        let up = |t: usize| s.uploaded.get(t).copied().unwrap_or(false);
        let (Some(a), Some(b)) = (ov.layer(from).filter(|_| up(from)), ov.layer(to).filter(|_| up(to))) else {
            return tr;
        };
        let index: HashMap<i32, usize> = tr.values.iter().enumerate().map(|(k, &v)| (v, k)).collect();
        for (&x, &y) in a.iter().zip(b) {
            if !x.is_finite() || !y.is_finite() {
                continue;
            }
            let (Some(&i), Some(&j)) = (index.get(&(x.round() as i32)), index.get(&(y.round() as i32))) else {
                continue;
            };
            tr.m[i * k_n + j] += 1;
            tr.both += 1;
        }
        tr.done = true;
        tr
    }

    fn class_timeline_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        let (ow, oh, factor) = (s.s.overview.w, s.s.overview.h, s.s.overview.factor);
        if !self.classes.on() || self.classes.list.is_empty() {
            ui.weak(
                "For categorical layers (land cover, classes): the share of each class at every\n\
                 date and the transitions between two dates. Display > Categorical switches it on.",
            );
            return;
        }
        let t_n = info.t();
        if !self.count_class_timeline(8.0) {
            ui.weak(format!(
                "Counting the classes: {} of {t_n} dates (the overview's pixels)...",
                self.analysis.classes.next
            ));
            ui.ctx().request_repaint();
        }
        let c = &self.analysis.classes;
        let k_n = c.values.len();
        let names: HashMap<i32, String> = self.classes.list.iter().map(|e| (e.value, e.name.clone())).collect();
        let name_of = |v: i32| names.get(&v).cloned().unwrap_or_else(|| v.to_string());
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.analysis.class_view, 0, "Shares")
                .on_hover_text("The share of each class at every date");
            ui.radio_value(&mut self.analysis.class_view, 1, "Flows").on_hover_text(
                "What became what between two to six dates (a Sankey diagram): the bars are the classes at each \
                 date, the ribbons the pixels that changed",
            );
            ui.separator();
            ui.checkbox(&mut self.analysis.class_stacked, "Stacked")
                .on_hover_text("Stacked areas (the shares add up to 100%), or one line per class");
            let c = &self.analysis.classes;
            if ui.button("Copy CSV").on_hover_text("Copies the share (%) of each class at every date").clicked() {
                let mut csv = String::from("date");
                for &v in &c.values {
                    csv += &format!(",{}", name_of(v));
                }
                csv.push('\n');
                for t in 0..c.next {
                    csv += &info.layers[t].label;
                    for k in 0..k_n {
                        let s =
                            if c.valid[t] > 0 { 100.0 * f64::from(c.counts[t][k]) / c.valid[t] as f64 } else { 0.0 };
                        csv += &format!(",{s:.4}");
                    }
                    csv.push('\n');
                }
                ui.ctx().copy_text(csv);
            }
            ui.weak("(?)").on_hover_text(format!(
                "Share of the pixels with a class at each date, counted on the overview\n\
                 ({ow} x {oh} pixels of the series, every {} source pixels).",
                format_g(factor, 3)
            ));
        });
        let avail = ui.available_width();
        // the flows: the panel's whole width (the matrix of two dates is in their tooltips)
        if self.analysis.class_view == 1 {
            let h = ui.available_height();
            ui.vertical(|ui| self.class_flows_ui(ui, vec2(avail, h), false));
            return;
        }
        let chart_w = (avail * 0.55).max(avail - ui.text_style_height(&egui::TextStyle::Body) * 34.0);
        let xs: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let stacked = self.analysis.class_stacked;
        let t_now = self.display.t;
        let mut pick = None;
        let h = ui.available_height();
        ui.horizontal_top(|ui| {
            ui.allocate_ui(vec2(chart_w, h), |ui| {
                let c = &self.analysis.classes;
                let n = c.next;
                let mut plot = Plot::new("class shares")
                    .width(chart_w)
                    .height(h)
                    .legend(Legend::default().position(egui_plot::Corner::RightTop).background_alpha(0.6))
                    .show_background(false)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .y_axis_label("% of the pixels");
                if info.time_is_date {
                    plot = crate::chart::time_axis(plot, None);
                } else {
                    plot = plot.x_axis_label(if info.spectral { "wavelength (nm)" } else { "date (index)" });
                }
                if stacked {
                    plot = plot.include_y(0.0).include_y(100.0);
                }
                let r = plot.show(ui, |plot| {
                    let mut lower = vec![0f64; n];
                    for (k, &v) in c.values.iter().enumerate() {
                        let e = self.classes.find(v);
                        if e.is_some_and(|e| !e.visible) {
                            continue;
                        }
                        let col = e
                            .map_or(Color32::from_gray(153), |e| Color32::from_rgb(e.color[0], e.color[1], e.color[2]));
                        let share: Vec<f64> = (0..n)
                            .map(|t| {
                                if c.valid[t] > 0 { 100.0 * f64::from(c.counts[t][k]) / c.valid[t] as f64 } else { 0.0 }
                            })
                            .collect();
                        let name = name_of(v);
                        if stacked {
                            let upper: Vec<f64> = lower.iter().zip(&share).map(|(a, b)| a + b).collect();
                            plot.add(
                                FilledArea::new(&name, &xs[..n], &lower, &upper)
                                    .fill_color(col.gamma_multiply(0.85))
                                    .stroke(Stroke::new(1.0, theme::on_plot(col))),
                            );
                            lower = upper;
                        } else {
                            let pts: Vec<[f64; 2]> = (0..n).map(|t| [xs[t], share[t]]).collect();
                            plot.line(
                                Line::new(&name, PlotPoints::from(pts.clone())).color(theme::on_plot(col)).width(2.0),
                            );
                            plot.points(
                                egui_plot::Points::new(&name, PlotPoints::from(pts))
                                    .color(theme::on_plot(col))
                                    .radius(2.5),
                            );
                        }
                    }
                    plot.vline(
                        VLine::new("date", xs[t_now]).color(theme::on_plot(ORANGE)).width(1.5).allow_hover(false),
                    );
                    plot.pointer_coordinate()
                });
                // the shares at the date under the pointer; a click goes to it
                if let Some(p) = r.inner.filter(|_| r.response.hovered()) {
                    let t = info.nearest_layer(p.x);
                    if t < c.next {
                        let mut tip = info.layers[t].label.clone();
                        for (k, &v) in c.values.iter().enumerate() {
                            if c.counts[t][k] > 0 {
                                tip += &format!(
                                    "\n{}: {}",
                                    name_of(v),
                                    percent(f64::from(c.counts[t][k]) / c.valid[t] as f64)
                                );
                            }
                        }
                        r.response.clone().on_hover_text(tip);
                    }
                    if r.response.clicked() {
                        pick = Some(t);
                    }
                }
            });
            ui.vertical(|ui| self.transitions_ui(ui, t_n));
        });
        if let Some(t) = pick {
            self.set_date(t);
        }
    }

    fn transitions_ui(&mut self, ui: &mut Ui, t_n: usize) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        ui.label("Transitions");
        let a = &mut self.analysis;
        a.trans_from = a.trans_from.min(t_n - 1);
        let to = a.trans_to.map_or(self.display.t, |t| t.min(t_n - 1));
        let w = ui.text_style_height(&egui::TextStyle::Body) * 8.0;
        ui.horizontal(|ui| {
            ui.label("from");
            egui::ComboBox::from_id_salt("transitions from")
                .width(w)
                .selected_text(&info.layers[a.trans_from].label)
                .show_ui(ui, |ui| {
                    for (t, l) in info.layers.iter().enumerate() {
                        ui.selectable_value(&mut a.trans_from, t, &l.label);
                    }
                });
            let to_label = if a.trans_to.is_none() {
                format!("{} (current)", info.layers[to].label)
            } else {
                info.layers[to].label.clone()
            };
            ui.label("to");
            egui::ComboBox::from_id_salt("transitions to").width(w).selected_text(to_label).show_ui(ui, |ui| {
                ui.selectable_value(&mut a.trans_to, None, "The current date");
                for (t, l) in info.layers.iter().enumerate() {
                    ui.selectable_value(&mut a.trans_to, Some(t), &l.label);
                }
            });
        });
        let from = self.analysis.trans_from;
        let key = self.analysis.classes.key;
        let tr = &self.analysis.trans;
        if tr.key != key || tr.from != from || tr.to != to || !tr.done {
            let mut t = self.count_transitions(from, to);
            t.key = key;
            self.analysis.trans = t;
        }
        let tr = &self.analysis.trans;
        if !tr.done {
            ui.weak("Waiting for both dates in the overview...");
            return;
        }
        let k_n = tr.values.len();
        let mut moves: Vec<(u32, usize, usize)> = Vec::new();
        let mut changed = 0u64;
        for i in 0..k_n {
            for j in 0..k_n {
                let m = tr.m[i * k_n + j];
                if i != j && m > 0 {
                    changed += u64::from(m);
                    moves.push((m, i, j));
                }
            }
        }
        moves.sort_by_key(|m| std::cmp::Reverse(m.0));
        let both = tr.both.max(1) as f64;
        let name_of = |k: usize| self.classes.find(tr.values[k]).map_or(tr.values[k].to_string(), |e| e.name.clone());
        ui.label(format!("Changed: {} of the pixels with a class at both dates", percent(changed as f64 / both)));
        for &(m, i, j) in moves.iter().take(6) {
            ui.label(format!("  - {} -> {}: {}", name_of(i), name_of(j), percent(f64::from(m) / both)));
        }
        if ui
            .small_button("Copy CSV")
            .on_hover_text("Copies the matrix (pixel counts: rows = from, columns = to)")
            .clicked()
        {
            let mut csv = String::from("from \\ to");
            for j in 0..k_n {
                csv += &format!(",{}", name_of(j));
            }
            csv.push('\n');
            for i in 0..k_n {
                csv += &name_of(i);
                for j in 0..k_n {
                    csv += &format!(",{}", tr.m[i * k_n + j]);
                }
                csv.push('\n');
            }
            ui.ctx().copy_text(csv);
        }
        // the matrix: rows = the class at "from", columns = at "to", % of the pixels
        let rows: Vec<usize> = (0..k_n).filter(|&i| (0..k_n).any(|j| tr.m[i * k_n + j] > 0)).collect();
        let cols: Vec<usize> = (0..k_n).filter(|&j| (0..k_n).any(|i| tr.m[i * k_n + j] > 0)).collect();
        if cols.len() > 30 {
            ui.weak(format!("{} classes: too many for a table (Copy CSV has them all).", cols.len()));
            return;
        }
        let accent = theme::accent();
        egui::ScrollArea::both().id_salt("transitions").show(ui, |ui| {
            egui::Grid::new("transitions grid").striped(false).show(ui, |ui| {
                ui.weak("from \\ to");
                for &j in &cols {
                    ui.weak(name_of(j));
                }
                ui.end_row();
                for &i in &rows {
                    ui.horizontal(|ui| {
                        if let Some(e) = self.classes.find(tr.values[i]) {
                            let s = ui.text_style_height(&egui::TextStyle::Body);
                            let (r, _) = ui.allocate_exact_size(vec2(s, s), egui::Sense::hover());
                            ui.painter().rect_filled(r, 2.0, Color32::from_rgb(e.color[0], e.color[1], e.color[2]));
                        }
                        ui.label(name_of(i));
                    });
                    for &j in &cols {
                        let n = tr.m[i * k_n + j];
                        if n == 0 {
                            ui.label("");
                            continue;
                        }
                        let s = f64::from(n) / both;
                        let text = egui::RichText::new(percent(s));
                        let r = if i != j {
                            // changes stand out, the stronger the more
                            let a = 0.15 + 0.6 * s.sqrt() as f32;
                            let bg =
                                Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), (a * 255.0) as u8);
                            ui.label(text.background_color(bg))
                        } else {
                            ui.label(text)
                        };
                        r.on_hover_text(format!("{} -> {}\n{n} pixels of the overview", name_of(i), name_of(j)));
                    }
                    ui.end_row();
                }
            });
        });
    }

    // -----------------------------------------------------------------------------------------------------------
    // Scatter
    // -----------------------------------------------------------------------------------------------------------

    /// Pixels of the active layer (the ROI, or all of it), each with the value of the X and Y layer / date at the
    /// same place, from the overviews; Pearson's r, the least squares line, the mean difference and its RMS.
    pub fn compute_scatter(&self, x_layer: usize, tx: usize, y_layer: usize, ty: usize) -> ScatterData {
        let mut sc = ScatterData::default();
        let Some(s) = &self.session else { return sc };
        let info = &s.info;
        let [x0, y0, x1, y1] = self.series.roi_rect.unwrap_or([0, 0, info.width, info.height]);
        let n = (x1 - x0) as f64 * (y1 - y0) as f64;
        let step = ((n / 250_000.0).sqrt().ceil() as usize).max(1); // up to ~250k pixels
        let sample = |li: usize, t: usize, ax: f64, ay: f64| -> Option<f64> {
            let ls = self.layer_session(li)?;
            if !ls.uploaded.get(t).copied().unwrap_or(false) {
                return None;
            }
            let ov = &ls.s.overview;
            // the active pixel -> this layer's pixel
            let (lx, ly) = if li == self.active { (ax, ay) } else { self.layers[li].place.to_layer(ax, ay)? };
            if lx < 0.0 || ly < 0.0 || lx >= ls.info.width as f64 || ly >= ls.info.height as f64 {
                return None;
            }
            let ox = ((lx * ov.w as f64 / ls.info.width as f64) as usize).min(ov.w - 1);
            let oy = ((ly * ov.h as f64 / ls.info.height as f64) as usize).min(ov.h - 1);
            let v = ov.layer(t)?[oy * ov.w + ox];
            v.is_finite().then_some(f64::from(v))
        };
        let poly = !self.series.roi_poly.is_empty();
        for y in (y0..y1).step_by(step) {
            for x in (x0..x1).step_by(step) {
                let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                if poly && !janus_geo::plane::contains(&self.series.roi_poly, cx, cy) {
                    continue; // a polygonal ROI
                }
                if let (Some(a), Some(b)) = (sample(x_layer, tx, cx, cy), sample(y_layer, ty, cx, cy)) {
                    sc.x.push(a);
                    sc.y.push(b);
                }
            }
        }
        let m = sc.x.len();
        sc.n = m;
        sc.step = step;
        let mf = m.max(1) as f64;
        let (mx, my) = (sc.x.iter().sum::<f64>() / mf, sc.y.iter().sum::<f64>() / mf);
        let (mut sxx, mut syy, mut sxy, mut sd) = (0.0, 0.0, 0.0, 0.0);
        for (&a, &b) in sc.x.iter().zip(&sc.y) {
            let (dx, dy) = (a - mx, b - my);
            sxx += dx * dx;
            syy += dy * dy;
            sxy += dx * dy;
            sd += (b - a) * (b - a);
        }
        sc.r = if sxx > 0.0 && syy > 0.0 { sxy / (sxx * syy).sqrt() } else { f64::NAN };
        sc.b = if sxx > 0.0 { sxy / sxx } else { f64::NAN };
        sc.a = my - sc.b * mx;
        sc.mean_diff = my - mx;
        sc.rmsd = if m > 0 { (sd / m as f64).sqrt() } else { f64::NAN };
        sc
    }

    fn scatter_ui(&mut self, ui: &mut Ui) {
        // layers on the active layer's grid by an affine map (other CRSs: not here)
        let usable: Vec<usize> = (0..self.layers.len())
            .filter(|&i| i == self.active || (self.layers[i].place.aligned && self.layers[i].place.reproj.is_none()))
            .filter(|&i| self.layer_session(i).is_some())
            .collect();
        if usable.is_empty() {
            return;
        }
        let mut picked = [(0usize, 0usize); 2];
        // the trajectory: the one chosen while it is there, else the first there is (the cursor, a pin, the ROI)
        let sources = self.trajectory_sources();
        let traj_key = sources
            .iter()
            .find(|s| s.0 == self.analysis.trajectory)
            .or_else(|| sources.get(1))
            .map_or("off".to_string(), |s| s.0.clone());
        ui.horizontal_wrapped(|ui| {
            for (k, id) in ["X", "Y"].iter().enumerate() {
                let cube = self.analysis.scatter_cube[k];
                let li = usable
                    .iter()
                    .copied()
                    .find(|&i| self.layer_session(i).is_some_and(|s| s.info.id == cube))
                    // none chosen: X the active layer, Y another one if there is (two layers compared, a place's
                    // trajectory between them)
                    .or_else(|| (k == 1).then(|| usable.iter().copied().find(|&i| i != self.active)).flatten())
                    .unwrap_or(self.active.min(self.layers.len() - 1));
                // (the default not kept: Y goes to a layer added later)
                let Some(info) = self.layer_session(li).map(|s| s.info.clone()) else { continue };
                ui.label(*id);
                let mut chosen = li;
                egui::ComboBox::from_id_salt(("scatter layer", k))
                    .width(ui.text_style_height(&egui::TextStyle::Body) * 11.0)
                    .selected_text(&self.layers[li].name)
                    .show_ui(ui, |ui| {
                        for &u in &usable {
                            ui.selectable_value(&mut chosen, u, &self.layers[u].name);
                        }
                    });
                if chosen != li
                    && let Some(s) = self.layer_session(chosen)
                {
                    self.analysis.scatter_cube[k] = s.info.id;
                }
                // dates: a fixed one, the current date or the one before it (other layers follow the nearest date)
                let t_n = info.t();
                let cur = if li == self.active { self.display.t } else { self.layers[li].disp.t.min(t_n - 1) };
                let date = &mut self.analysis.scatter_date[k];
                let t = match *date {
                    -1 => cur,
                    -2 => cur.saturating_sub(1),
                    d => (d as usize).min(t_n - 1),
                };
                let label = match *date {
                    -1 => format!("{} (current)", info.layers[t].label),
                    -2 => format!("{} (previous)", info.layers[t].label),
                    _ => info.layers[t].label.clone(),
                };
                egui::ComboBox::from_id_salt(("scatter date", k))
                    .width(ui.text_style_height(&egui::TextStyle::Body) * 10.0)
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(date, -1, "The current date");
                        ui.selectable_value(date, -2, "The date before it");
                        for (q, l) in info.layers.iter().enumerate() {
                            ui.selectable_value(date, q as i64, &l.label);
                        }
                    });
                picked[k] = (li, t);
                ui.add_space(ui.text_style_height(&egui::TextStyle::Body));
            }
            let a = &mut self.analysis;
            let cur = sources.iter().position(|s| s.0 == traj_key);
            ui.label("Trajectory");
            egui::ComboBox::from_id_salt("scatter trajectory")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 9.0)
                .selected_text(cur.map_or("Off", |k| sources[k].1.as_str()))
                .show_ui(ui, |ui| {
                    for (key, name) in &sources {
                        if ui.selectable_label(cur.is_some_and(|k| sources[k].0 == *key), name).clicked() {
                            a.trajectory = key.clone();
                        }
                    }
                })
                .response
                .on_hover_text(
                    "A place's series on X's layer against its series on Y's, date by date, over the pixels: \
                     coloured by time (dark to bright), arrows the way it goes, the current date ringed. A \
                     season's loop, a disturbance leaving the cloud and the years it takes to come back.",
                );
        });
        if usable.len() < self.layers.len() {
            ui.weak("Layers in another CRS are not listed (their pixels are not on this grid).");
        }
        let [(xl, tx), (yl, ty)] = picked;
        let traj = (traj_key != "off").then(|| self.trajectory(&traj_key, xl, yl));
        if let Some(Err(why)) = &traj
            && !why.is_empty()
        {
            let mut on = false;
            ui.horizontal(|ui| {
                ui.weak(why);
                if self.series.chart_layers == 0 {
                    on = ui.small_button("Turn it on").clicked();
                }
            });
            if on {
                self.set_chart_layers(1);
            }
        }
        let traj = traj.and_then(Result::ok).map(|mut t| {
            t.name = format!("Trajectory: {}", sources.iter().find(|s| s.0 == traj_key).map_or("", |s| s.1.as_str()));
            t
        });
        let (Some(xs), Some(ys), Some(active)) =
            (self.layer_session(xl), self.layer_session(yl), self.session.as_ref())
        else {
            return;
        };
        let mut key = xs.info.id.wrapping_mul(1_000_003).wrapping_add(tx as u64);
        key = key.wrapping_mul(1_000_003).wrapping_add(ys.info.id).wrapping_mul(1_000_003).wrapping_add(ty as u64);
        key = key
            .wrapping_mul(1_000_003)
            .wrapping_add(self.series.roi_gen * 31 + u64::from(self.series.roi_rect.is_some()));
        key = key
            .wrapping_add(active.info.id.wrapping_mul(7919))
            .wrapping_add(xs.s.overview.layers_done() as u64 * 13 + ys.s.overview.layers_done() as u64);
        let (xs, ys) = (xs.info.clone(), ys.info.clone());
        if self.analysis.scatter.key != key {
            let mut sc = self.compute_scatter(xl, tx, yl, ty);
            sc.key = key;
            self.analysis.scatter = sc;
        }
        let sc = &self.analysis.scatter;
        ui.weak(format!(
            "{}: {} pixels{} of the overview",
            if self.series.roi_rect.is_some() { "ROI" } else { "Whole image" },
            sc.n,
            if sc.step > 1 { " (a regular sample)" } else { "" }
        ));
        if sc.n < 2 {
            ui.weak("No pixel has both values here.");
            return;
        }
        ui.label(format!(
            "r = {:.4}   R2 = {:.4}   Y = {} + {} X   mean(Y - X) = {}   RMS(Y - X) = {}",
            sc.r,
            sc.r * sc.r,
            format_g(sc.a, 4),
            format_g(sc.b, 4),
            format_g(sc.mean_diff, 4),
            format_g(sc.rmsd, 4)
        ));
        let x_name = format!("{}, {}", self.layers[xl].name, xs.layers[tx].label);
        let y_name = format!("{}, {}", self.layers[yl].name, ys.layers[ty].label);
        let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
        for (&a, &b) in sc.x.iter().zip(&sc.y) {
            lo = [lo[0].min(a), lo[1].min(b)];
            hi = [hi[0].max(a), hi[1].max(b)];
        }
        for k in 0..2 {
            if hi[k] <= lo[k] {
                lo[k] -= 0.5;
                hi[k] += 0.5;
            }
        }
        // the plot's extent: the pixels' (the density's image) and the trajectory's
        let (mut plo, mut phi) = (lo, hi);
        for p in traj.iter().flat_map(|t| &t.pts) {
            plo = [plo[0].min(p[0]), plo[1].min(p[1])];
            phi = [phi[0].max(p[0]), phi[1].max(p[1])];
        }
        // the density: 120 x 120 cells, an image in the plot (made again when the pixels change)
        const B: usize = 120;
        let (cw, ch) = ((hi[0] - lo[0]) / B as f64, (hi[1] - lo[1]) / B as f64);
        if self.analysis.density.as_ref().is_none_or(|d| d.0 != key) {
            let mut hist = vec![0u32; B * B];
            for (&a, &b) in sc.x.iter().zip(&sc.y) {
                let i = (((a - lo[0]) / cw) as usize).min(B - 1);
                let j = (((b - lo[1]) / ch) as usize).min(B - 1);
                hist[(B - 1 - j) * B + i] += 1; // the image's first row is the top
            }
            let max_count = f64::from(hist.iter().copied().max().unwrap_or(1).max(1));
            let viridis = lut(janus_gpu::colormaps::VIRIDIS);
            let pixels: Vec<Color32> =
                hist.iter()
                    .map(|&n| {
                        if n == 0 { Color32::TRANSPARENT } else { lut_at(&viridis, (f64::from(n) / max_count) as f32) }
                    })
                    .collect();
            let image = egui::ColorImage { size: [B, B], pixels, source_size: vec2(B as f32, B as f32) };
            let tex = ui.ctx().load_texture("scatter density", image, egui::TextureOptions::NEAREST);
            self.analysis.density = Some((key, tex, max_count));
        }
        let Some((_, tex, max_count)) = self.analysis.density.as_ref().map(|d| (d.0, d.1.id(), d.2)) else { return };
        let scale_w = ui.text_style_height(&egui::TextStyle::Body) * 6.0;
        let (a, b) = (sc.a, sc.b);
        let size = vec2(ui.available_width() - scale_w, ui.available_height());
        ui.horizontal_top(|ui| {
            ui.allocate_ui(size, |ui| {
                Plot::new("scatter")
                    .width(size.x)
                    .height(size.y)
                    .x_axis_label(x_name)
                    .y_axis_label(y_name)
                    .include_x(plo[0])
                    .include_x(phi[0])
                    .include_y(plo[1])
                    .include_y(phi[1])
                    .set_margin_fraction(egui::Vec2::ZERO)
                    .show_background(false)
                    .allow_drag(false)
                    .allow_scroll(false)
                    .legend(Legend::default().position(egui_plot::Corner::LeftTop).background_alpha(0.6))
                    .show(ui, |plot| {
                        let centre = PlotPoint::new((lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0);
                        let extent = vec2((hi[0] - lo[0]) as f32, (hi[1] - lo[1]) as f32);
                        plot.image(egui_plot::PlotImage::new("", tex, centre, extent).allow_hover(false));
                        let (l, h) = (lo[0].max(lo[1]), hi[0].min(hi[1]));
                        if h > l {
                            plot.line(
                                Line::new("Y = X", PlotPoints::from(vec![[l, l], [h, h]]))
                                    .color(theme::on_plot(Color32::from_gray(191))),
                            );
                        }
                        if b.is_finite() {
                            let pts = vec![[lo[0], a + b * lo[0]], [hi[0], a + b * hi[0]]];
                            plot.line(
                                Line::new("Least squares", PlotPoints::from(pts))
                                    .color(theme::on_plot(ORANGE))
                                    .width(2.0),
                            );
                        }
                        if let Some(t) = &traj {
                            crate::trajectory::draw(plot, t);
                        }
                    });
            });
            colour_scale(ui, size.y, janus_gpu::colormaps::VIRIDIS, 0.0, max_count, &|v| format!("{v:.0}"));
        });
    }
}
