//! The time series chart (src/app.cpp, uiSeries): the cursor's series (the overview's at once, the exact one when
//! read), the pins', the ROI's mean with its spread, and those of every other visible layer; in the chart's style,
//! as values, anomalies or z-scores, with trends; the current date as a line, a click or a drag picks the date. The
//! axis is a date axis: its marks fall on days, months or years (ImPlot's time axis), labelled as precisely as they
//! need.

use egui::{Color32, ComboBox, Ui};
use egui_plot::{BoxElem, BoxPlot, BoxSpread, GridInput, GridMark, Line, MarkerShape, Plot, PlotPoints, Points, VLine};
use janus_cube::dates::{civil_from_days, days_from_civil};
use janus_cube::stats::SeriesStats;

use crate::app::State;

const DAY: f64 = 86400.0;
const MONTH: f64 = 30.44 * DAY;
const YEAR: f64 = 365.2425 * DAY;

/// Marks of a date axis: the finest of days, months or years spaced enough for a label, and coarser ones.
pub fn time_marks(input: GridInput) -> Vec<GridMark> {
    let (a, b) = input.bounds;
    // a label needs about 12 times the smallest spacing egui asks for
    let min_step = input.base_step_size * 12.0;
    let steps: [(f64, u32); 16] = [
        (DAY, 0),
        (2.0 * DAY, 0),
        (7.0 * DAY, 0),
        (14.0 * DAY, 0),
        (MONTH, 1),
        (2.0 * MONTH, 2),
        (3.0 * MONTH, 3),
        (6.0 * MONTH, 6),
        (YEAR, 12),
        (2.0 * YEAR, 24),
        (5.0 * YEAR, 60),
        (10.0 * YEAR, 120),
        (20.0 * YEAR, 240),
        (50.0 * YEAR, 600),
        (100.0 * YEAR, 1200),
        (500.0 * YEAR, 6000),
    ];
    let i = steps.iter().position(|s| s.0 >= min_step).unwrap_or(steps.len() - 1);
    let mut marks: Vec<GridMark> = Vec::new();
    for &(step, months) in &steps[i..(i + 2).min(steps.len())] {
        let mut level = Vec::new();
        if months == 0 {
            let k = (step / DAY) as i64;
            let first = ((a / DAY).floor() as i64).div_euclid(k) * k;
            let mut d = first;
            while (d as f64) * DAY <= b && level.len() < 2000 {
                level.push(GridMark { value: d as f64 * DAY, step_size: step });
                d += k;
            }
        } else {
            let (y0, m0, _) = civil_from_days((a / DAY).floor() as i64);
            let mut idx = (i64::from(y0) * 12 + i64::from(m0) - 1).div_euclid(i64::from(months)) * i64::from(months);
            loop {
                let (y, m) = (idx.div_euclid(12) as i32, idx.rem_euclid(12) as u32 + 1);
                let v = days_from_civil(y, m, 1) as f64 * DAY;
                if v > b || level.len() >= 2000 {
                    break;
                }
                if v >= a {
                    level.push(GridMark { value: v, step_size: step });
                }
                idx += i64::from(months);
            }
        }
        // a coarser mark on a finer one: one mark, of the coarser step (one label)
        for m in level {
            match marks.iter_mut().find(|x| x.value == m.value) {
                Some(x) => x.step_size = m.step_size,
                None => marks.push(m),
            }
        }
    }
    marks
}

/// A mark's label: a year, a month or a day, as fine as its step.
pub fn time_label(mark: GridMark) -> String {
    let (y, m, d) = civil_from_days((mark.value / DAY).round() as i64);
    if mark.step_size >= YEAR * 0.99 {
        format!("{y}")
    } else if mark.step_size >= MONTH * 0.99 {
        format!("{y}-{m:02}")
    } else {
        format!("{y}-{m:02}-{d:02}")
    }
}

/// The chart's options (the C++'s plotStyle_, plotValues_, trendKind_, yFromMap_, roiSpread_).
pub struct ChartOptions {
    /// 0 lines, 1 lines + markers, 2 markers, 3 stems, 4 stairs
    pub style: usize,
    /// 0 values, 1 anomaly (- the series' mean), 2 z-score
    pub values: usize,
    /// 0 none, 1 OLS, 2 Sen
    pub trend: usize,
    /// the Y axis locked to the map's colour range
    pub y_from_map: bool,
    /// the ROI: 0 mean only, 1 + the p10-p90 band, 2 + a box plot per date
    pub roi_spread: usize,
}

impl Default for ChartOptions {
    fn default() -> Self {
        ChartOptions { style: 1, values: 0, trend: 1, y_from_map: false, roi_spread: 1 }
    }
}

/// One marker shape per layer (the other layers' series).
fn layer_marker(i: usize) -> MarkerShape {
    const M: [MarkerShape; 6] = [
        MarkerShape::Circle,
        MarkerShape::Square,
        MarkerShape::Diamond,
        MarkerShape::Up,
        MarkerShape::Down,
        MarkerShape::Cross,
    ];
    M[i % M.len()]
}

/// A series' value as shown: as stored, minus its mean, or standardized.
fn shown(v: f64, st: &SeriesStats, values: usize) -> f64 {
    match values {
        1 => v - st.mean,
        2 if st.std > 0.0 => (v - st.mean) / st.std,
        2 => 0.0,
        _ => v,
    }
}

/// Points (x, y) of the valid values in each run of dates (a spectrum's pieces), a line not drawn across a gap.
fn pieces(xs: &[f64], ys: &[f64], runs: &[(usize, usize)]) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    for &(b, e) in runs {
        let mut run = Vec::new();
        for t in b..e.min(xs.len()) {
            if ys[t].is_nan() {
                if run.len() > 1 {
                    out.push(std::mem::take(&mut run));
                }
                run.clear();
            } else {
                run.push([xs[t], ys[t]]);
            }
        }
        if run.len() > 1 {
            out.push(run);
        }
    }
    out
}

struct Plotted<'a> {
    xs: &'a [f64],
    years: &'a [f64],
    runs: &'a [(usize, usize)],
    o: &'a ChartOptions,
    categorical: bool,
    spectral: bool,
}

impl Plotted<'_> {
    /// A series in the chart's style, with its trend (same name: hidden together).
    /// `look`: its colour and line width.
    fn series(
        &self,
        plot: &mut egui_plot::PlotUi,
        name: &str,
        vals: &[f32],
        st: &SeriesStats,
        (color, width): (Color32, f32),
        trend: bool,
    ) {
        let n = self.xs.len();
        if vals.len() != n {
            return;
        }
        // legible on the chart's background (the light themes darken the data colours)
        let color = crate::theme::on_plot(color);
        let values = if self.categorical { 0 } else { self.o.values };
        let ys: Vec<f64> = vals.iter().map(|&v| shown(f64::from(v), st, values)).collect();
        let valid: Vec<[f64; 2]> = (0..n).filter(|&t| !ys[t].is_nan()).map(|t| [self.xs[t], ys[t]]).collect();
        let stairs = |plot: &mut egui_plot::PlotUi| {
            // a value holds until the next date
            let mut pts = Vec::new();
            for (k, p) in valid.iter().enumerate() {
                pts.push(*p);
                if let Some(next) = valid.get(k + 1) {
                    pts.push([next[0], p[1]]);
                }
            }
            plot.line(Line::new(name, PlotPoints::from(pts)).color(color).width(width));
        };
        if self.categorical {
            // a class holds until the next date: steps, no trend
            stairs(plot);
            plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(2.5));
            return;
        }
        let lines = |plot: &mut egui_plot::PlotUi| {
            for piece in pieces(self.xs, &ys, self.runs) {
                plot.line(Line::new(name, PlotPoints::from(piece)).color(color).width(width));
            }
        };
        match self.o.style {
            0 => lines(plot),
            1 => {
                lines(plot);
                plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(2.5));
            }
            2 => plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(3.5)),
            3 => {
                let base = if values == 0 { st.mean } else { 0.0 };
                for p in &valid {
                    plot.line(Line::new(name, PlotPoints::from(vec![[p[0], base], *p])).color(color).width(width));
                }
                plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(2.5));
            }
            _ => stairs(plot),
        }
        if trend && self.o.trend != 0 && st.n >= 2 && !self.spectral && n >= 1 {
            let (a, b) =
                if self.o.trend == 1 { (st.ols_intercept, st.ols_slope) } else { (st.sen_intercept, st.sen_slope) };
            let y = |yr: f64| shown(a + b * yr, st, values);
            let pts = vec![[self.xs[0], y(self.years[0])], [self.xs[n - 1], y(self.years[n - 1])]];
            let c = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 153);
            plot.line(Line::new(name, PlotPoints::from(pts)).color(c).width(1.2));
        }
    }
}

impl State {
    /// The chart's series as CSV in the clipboard: the cursor's, the pins', the ROI's.
    fn copy_csv(&self, ctx: &egui::Context) {
        let Some(s) = &self.session else { return };
        let info = &s.info;
        let sr = &self.series;
        let mut csv = String::from(if info.spectral { "wavelength_nm" } else { "date" });
        if let Some(h) = &sr.hover {
            csv += &format!(",cursor_{}_{}", h.x, h.y);
        }
        for p in &sr.pins {
            csv += &format!(",pin{}_{}_{}", p.id, p.x, p.y);
        }
        let roi = !sr.roi.mean.is_empty();
        if roi {
            csv += ",roi_mean,roi_p10,roi_p90,roi_p25,roi_median,roi_p75"; // new columns at the end
        }
        csv += "\n";
        let add = |csv: &mut String, v: f32| {
            csv.push(',');
            if !v.is_nan() {
                csv.push_str(&janus_cube::multidim::format_g(f64::from(v), 7));
            }
        };
        for t in 0..info.t() {
            if info.spectral {
                csv += &janus_cube::multidim::format_g6(info.layers[t].time);
            } else {
                csv += &info.layers[t].label;
            }
            if let Some(h) = &sr.hover {
                add(&mut csv, h.values[t]);
            }
            for p in &sr.pins {
                add(&mut csv, p.values[t]);
            }
            if roi {
                let r = &sr.roi;
                for v in [&r.mean, &r.p10, &r.p90, &r.p25, &r.p50, &r.p75] {
                    add(&mut csv, v[t]);
                }
            }
            csv += "\n";
        }
        ctx.copy_text(csv);
    }

    pub fn series_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else {
            ui.weak("Open a series and hover over the map.");
            return;
        };
        let info = s.info.clone();
        let n = info.t();
        let xs: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let years = s.years.clone();
        let categorical = self.classes.on();

        // the chart's options (taken out while the toolbar has the State)
        let mut chart = std::mem::take(&mut self.chart);
        let o = &mut chart;
        let mut chart_layers = self.series.chart_layers;
        let (n_layers, has_roi, has_pins) =
            (self.layers.len(), self.series.roi_rect.is_some(), !self.series.pins.is_empty());
        let (mut clear_pins, mut clear_roi, mut copy) = (false, false, false);
        ui.horizontal_wrapped(|ui| {
            const STYLES: [&str; 5] = ["Lines", "Lines + markers", "Markers", "Stems", "Stairs"];
            ComboBox::from_id_salt("style").width(130.0).selected_text(STYLES[o.style]).show_ui(ui, |ui| {
                for (i, n) in STYLES.iter().enumerate() {
                    ui.selectable_value(&mut o.style, i, *n);
                }
            });
            const VALUES: [&str; 3] = ["Values", "Anomaly (- mean)", "Standardized (z-score)"];
            ComboBox::from_id_salt("values")
                .width(160.0)
                .selected_text(VALUES[o.values])
                .show_ui(ui, |ui| {
                    for (i, n) in VALUES.iter().enumerate() {
                        ui.selectable_value(&mut o.values, i, *n);
                    }
                })
                .response
                .on_hover_text(
                    "Values: as stored in the raster\nAnomaly: each series minus its own mean (compares shape, not \
                     level)\nZ-score: (value - mean) / std (compares series of different scales)",
                );
            const TRENDS: [&str; 3] = ["No trend", "OLS trend", "Sen trend"];
            ComboBox::from_id_salt("trend")
                .width(110.0)
                .selected_text(TRENDS[o.trend])
                .show_ui(ui, |ui| {
                    for (i, n) in TRENDS.iter().enumerate() {
                        ui.selectable_value(&mut o.trend, i, *n);
                    }
                })
                .response
                .on_hover_text(
                    "Trend line of each series (same color, thinner)\nOLS: least squares, sensitive to outliers\nSen: \
                     median of pairwise slopes, robust to outliers",
                );
            ui.add_enabled(o.values == 0, egui::Checkbox::new(&mut o.y_from_map, "Y = map range"))
                .on_hover_text("Locks the Y axis to the map's color range (compare without the axis jumping)");
            if n_layers > 1 {
                const LAYERS: [&str; 2] = ["Active layer", "All visible layers"];
                ComboBox::from_id_salt("chart layers")
                    .width(140.0)
                    .selected_text(LAYERS[chart_layers as usize])
                    .show_ui(ui, |ui| {
                        for (i, n) in LAYERS.iter().enumerate() {
                            ui.selectable_value(&mut chart_layers, i as i32, *n);
                        }
                    })
                    .response
                    .on_hover_text(
                        "Series of the active layer only, or of every visible layer at the same\nplace (different \
                         marker per layer; other layers use their own dates)",
                    );
            }
            if has_roi {
                const SPREAD: [&str; 3] = ["ROI: mean", "ROI: p10-p90", "ROI: box plot"];
                ComboBox::from_id_salt("roi spread")
                    .width(120.0)
                    .selected_text(SPREAD[o.roi_spread])
                    .show_ui(ui, |ui| {
                        for (i, n) in SPREAD.iter().enumerate() {
                            ui.selectable_value(&mut o.roi_spread, i, *n);
                        }
                    })
                    .response
                    .on_hover_text(
                        "Spread of the ROI's pixels at each date, around the mean:\np10-p90: a band between the 10th \
                         and 90th percentiles\nBox plot: a box from p25 to p75 with the median, whiskers to p10 and p90",
                    );
            }
            copy = ui.button("Copy CSV").on_hover_text("Copies the series (cursor, pins, ROI) to the clipboard").clicked();
            self.copy_python_ui(ui);
            if has_pins {
                clear_pins = ui.button("Clear pins").clicked();
            }
            if has_roi {
                clear_roi = ui.button("Clear ROI").clicked();
            }
            ui.weak("(?)").on_hover_text(
                "Chart lines:\n  white ............. pixel under the cursor\n  colored ........... pins (same number \
                 and color as on the map)\n  yellow + band ..... ROI mean and p10-p90\n  thin, same color .. each \
                 series' trend (OLS or Sen)\n  orange vertical ... current date (drag or click to change)\n\nClick a \
                 legend entry to hide/show that series.\nPins: right click the pin on the map, Delete (last one)\nor \
                 the x in the statistics table removes them.",
            );
        });
        self.chart = chart;
        if copy {
            self.copy_csv(ui.ctx());
        }
        if clear_pins {
            self.clear_pins();
        }
        if clear_roi {
            self.clear_roi();
        }
        if chart_layers != self.series.chart_layers {
            self.set_chart_layers(chart_layers);
        }

        let o = &self.chart;
        let dated = info.time_is_date;
        let mut plot = Plot::new(("series", info.id))
            .allow_drag(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .show_background(false)
            .legend(
                egui_plot::Legend::default()
                    .position(egui_plot::Corner::LeftTop)
                    .background_alpha(0.6)
                    // a series and its trend (thinner, fainter) are one entry, in the series' colour
                    .color_conflict_handling(egui_plot::ColorConflictHandling::PickFirst),
            );
        if dated {
            plot = plot.x_grid_spacer(time_marks).x_axis_formatter(|m, _| time_label(m));
        } else {
            plot = plot.x_axis_label(if info.spectral { "wavelength (nm)" } else { "date (index)" });
        }
        if o.values > 0 && !categorical {
            plot = plot.y_axis_label(if o.values == 1 { "anomaly" } else { "z-score" });
        }
        // the whole series and 3 % of it beyond each end
        let pad = if n > 1 { (xs[n - 1] - xs[0]) * 0.03 } else { 1.0 };
        plot = plot.include_x(xs[0] - pad).include_x(xs[n - 1] + pad);
        if o.y_from_map && o.values == 0 {
            let r = self.display.ranges[janus_gpu::Mode::Value as usize];
            let m = f64::from(r.hi - r.lo) * 0.05;
            plot = plot
                .include_y(f64::from(r.lo) - m)
                .include_y(f64::from(r.hi) + m)
                .auto_bounds(egui::Vec2b::new(true, false))
                .default_y_bounds(f64::from(r.lo) - m, f64::from(r.hi) + m);
        }
        // categorical: one tick per class, named
        if categorical && !self.classes.list.is_empty() && self.classes.list.len() <= 40 {
            let classes: Vec<(f64, String)> =
                self.classes.list.iter().map(|c| (f64::from(c.value), c.name.clone())).collect();
            let marks = classes.clone();
            plot = plot
                .y_grid_spacer(move |_| marks.iter().map(|(v, _)| GridMark { value: *v, step_size: 1.0 }).collect())
                .y_axis_formatter(move |m, _| {
                    classes.iter().find(|(v, _)| *v == m.value).map_or_else(String::new, |(_, n)| n.clone())
                });
        }
        let info2 = info.clone();
        plot = plot.label_formatter(move |h| {
            let (name, p) = match h {
                egui_plot::HoverPosition::NearDataPoint { plot_name, position, .. } => (*plot_name, position),
                egui_plot::HoverPosition::Elsewhere { position } => ("", position),
            };
            let x = if info2.time_is_date { info2.format_time(p.x) } else { format!("{:.1}", p.x) };
            Some(if name.is_empty() { format!("{x}\n{:.4}", p.y) } else { format!("{name}\n{x}\n{:.4}", p.y) })
        });
        let runs = janus_cube::spectral::plot_runs(&info);
        let pl = Plotted { xs: &xs, years: &years, runs: &runs, o, categorical, spectral: info.spectral };
        let t = self.display.t;
        let sr = &self.series;
        let background = ui.painter().add(egui::Shape::Noop);
        let orange = Color32::from_rgb(255, 153, 51);
        let response = plot.show(ui, |plot| {
            let roi = &sr.roi;
            if !roi.mean.is_empty() && !categorical {
                let name = if roi.sampled() { "ROI mean (subsampled)" } else { "ROI mean" };
                let f =
                    |v: &[f32]| -> Vec<f64> { v.iter().map(|&x| shown(f64::from(x), &roi.stats, o.values)).collect() };
                if o.roi_spread == 1 {
                    let (a, b) = (f(&roi.p10), f(&roi.p90));
                    for &(rb, re) in &runs {
                        let idx: Vec<usize> = (rb..re).filter(|&t| !a[t].is_nan() && !b[t].is_nan()).collect();
                        let (x, lo, hi): (Vec<f64>, Vec<f64>, Vec<f64>) = (
                            idx.iter().map(|&t| xs[t]).collect(),
                            idx.iter().map(|&t| a[t]).collect(),
                            idx.iter().map(|&t| b[t]).collect(),
                        );
                        if x.len() > 1 {
                            plot.add(
                                egui_plot::FilledArea::new(name, &x, &lo, &hi)
                                    .fill_color(Color32::from_rgba_unmultiplied(255, 209, 61, 46))
                                    .stroke(egui::Stroke::NONE),
                            );
                        }
                    }
                }
                // the trend of a partially computed ROI would be misleading: wait for every date
                pl.series(plot, name, &roi.mean, &roi.stats, (crate::series::ROI_COLOR, 2.0), roi.complete());
                crate::zeit::draw_fit(plot, name, self.roi_fit(), &roi.stats, o.values, &info);
                if o.roi_spread == 2 {
                    // a box per date: p25 to p75 with the median, whiskers to p10 and p90
                    let gap = xs.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).fold(f64::INFINITY, f64::min);
                    let width = if gap.is_finite() { gap * 0.6 } else { 0.6 };
                    let (p10, p25, p50, p75, p90) = (f(&roi.p10), f(&roi.p25), f(&roi.p50), f(&roi.p75), f(&roi.p90));
                    let boxes: Vec<BoxElem> = (0..n)
                        .filter(|&t| p25[t].is_finite())
                        .map(|t| {
                            BoxElem::new(xs[t], BoxSpread::new(p10[t], p25[t], p50[t], p75[t], p90[t]))
                                .box_width(width)
                                .whisker_width(width * 0.5)
                                .fill(Color32::from_rgba_unmultiplied(255, 209, 61, 56))
                                .stroke(egui::Stroke::new(1.0, crate::series::ROI_COLOR))
                        })
                        .collect();
                    plot.box_plot(BoxPlot::new(name, boxes));
                }
            }
            for p in &sr.pins {
                let name = format!("Pin {} ({}, {}){}", p.id, p.x, p.y, if p.exact { "" } else { " approx." });
                pl.series(plot, &name, &p.values, &p.stats, (p.color, 1.5), true);
                let fit = self.zeit.fit_of(info.id, crate::zeit::Which::Pin(p.id), p.x, p.y);
                crate::zeit::draw_fit(plot, &name, fit, &p.stats, o.values, &info);
            }
            if let Some(h) = &sr.hover {
                let name = format!("Cursor ({}, {}){}", h.x, h.y, if h.exact { "" } else { " approx." });
                pl.series(plot, &name, &h.values, &h.stats, (h.color, 2.0), true);
                let fit = self.zeit.fit_of(info.id, crate::zeit::Which::Hover, h.x, h.y);
                crate::zeit::draw_fit(plot, &name, fit, &h.stats, o.values, &info);
                self.draw_class_series(plot, &h.stats, o.values);
            }
            if sr.chart_layers == 1 {
                // the other visible layers: their own dates on the same axis, a marker shape per layer
                for (li, l) in self.layers.iter().enumerate() {
                    let Some(ls) = l.session.as_ref().filter(|_| li != self.active && l.visible) else { continue };
                    let li_info = &ls.info;
                    if li_info.time_is_date != info.time_is_date || li_info.spectral != info.spectral {
                        continue;
                    }
                    let lx: Vec<f64> = li_info.layers.iter().map(|l| l.time).collect();
                    let lruns = janus_cube::spectral::plot_runs(li_info);
                    let other = |plot: &mut egui_plot::PlotUi,
                                 name: &str,
                                 v: &[f32],
                                 st: &SeriesStats,
                                 color: Color32,
                                 width: f32| {
                        if v.len() != lx.len() {
                            return;
                        }
                        let ys: Vec<f64> = v.iter().map(|&x| shown(f64::from(x), st, o.values)).collect();
                        let color = crate::theme::on_plot(color);
                        let c = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 217);
                        for piece in pieces(&lx, &ys, &lruns) {
                            plot.line(Line::new(name, PlotPoints::from(piece)).color(c).width(width));
                        }
                        let pts: Vec<[f64; 2]> =
                            (0..lx.len()).filter(|&t| !ys[t].is_nan()).map(|t| [lx[t], ys[t]]).collect();
                        plot.points(
                            Points::new(name, PlotPoints::from(pts)).color(color).radius(3.0).shape(layer_marker(li)),
                        );
                    };
                    for p in &l.series.pins {
                        let name = format!("Pin {} [{}]{}", p.id, l.name, if p.exact { "" } else { " approx." });
                        other(plot, &name, &p.values, &p.stats, p.color, 1.2);
                        let fit = self.zeit.fit_of(li_info.id, crate::zeit::Which::Pin(p.id), p.x, p.y);
                        crate::zeit::draw_fit(plot, &name, fit, &p.stats, o.values, li_info);
                    }
                    if let Some(h) = &l.series.hover {
                        let name = format!("Cursor [{}]{}", l.name, if h.exact { "" } else { " approx." });
                        other(plot, &name, &h.values, &h.stats, h.color, 1.6);
                        let fit = self.zeit.fit_of(li_info.id, crate::zeit::Which::Hover, h.x, h.y);
                        crate::zeit::draw_fit(plot, &name, fit, &h.stats, o.values, li_info);
                    }
                    let r = &l.series.roi;
                    if !r.mean.is_empty() && !l.classes.on() {
                        let name = format!("ROI mean [{}]{}", l.name, if r.sampled() { " (subsampled)" } else { "" });
                        if o.roi_spread > 0 {
                            let m = if o.values == 0 { 0.0 } else { r.stats.mean };
                            let sd = if o.values == 2 && r.stats.std > 0.0 { r.stats.std } else { 1.0 };
                            let (a, b): (Vec<f64>, Vec<f64>) = (
                                r.p10.iter().map(|&v| (f64::from(v) - m) / sd).collect(),
                                r.p90.iter().map(|&v| (f64::from(v) - m) / sd).collect(),
                            );
                            for &(rb, re) in &lruns {
                                let idx: Vec<usize> = (rb..re).filter(|&t| !a[t].is_nan() && !b[t].is_nan()).collect();
                                let x: Vec<f64> = idx.iter().map(|&t| lx[t]).collect();
                                let lo: Vec<f64> = idx.iter().map(|&t| a[t]).collect();
                                let hi: Vec<f64> = idx.iter().map(|&t| b[t]).collect();
                                if x.len() > 1 {
                                    plot.add(
                                        egui_plot::FilledArea::new(&name, &x, &lo, &hi)
                                            .fill_color(Color32::from_rgba_unmultiplied(255, 209, 61, 31))
                                            .stroke(egui::Stroke::NONE),
                                    );
                                }
                            }
                        }
                        other(plot, &name, &r.mean, &r.stats, crate::series::ROI_COLOR, 1.8);
                    }
                }
            }
            plot.vline(VLine::new("date", xs[t]).color(orange).width(1.5).allow_hover(false));
            let primary = plot.response().is_pointer_button_down_on() && plot.ctx().input(|i| i.pointer.primary_down());
            primary.then(|| plot.pointer_coordinate()).flatten()
        });
        // the background and the season under the series; the date's tag on the axis
        let tr = &response.transform;
        ui.painter().set(background, crate::season::chart_background(ui, tr, &self.display, &info));
        let x = tr.position_from_point(&egui_plot::PlotPoint::new(xs[t], tr.bounds().min()[1])).x;
        let frame = *tr.frame();
        if (frame.left()..=frame.right()).contains(&x) {
            let painter = ui.painter_at(frame.expand(1.0));
            let galley =
                painter.layout_no_wrap(info.layers[t].label.clone(), egui::FontId::proportional(12.0), Color32::BLACK);
            let r = egui::Rect::from_min_size(
                egui::pos2(x + 2.0, frame.bottom() - galley.size().y - 2.0),
                galley.size() + egui::vec2(4.0, 2.0),
            );
            painter.rect_filled(r, 2.0, orange);
            painter.galley(r.min + egui::vec2(2.0, 1.0), galley, Color32::BLACK);
        }
        // a click or a drag on the chart picks the nearest date
        if let Some(p) = response.inner {
            let nt = info.nearest_layer(p.x);
            if nt != self.display.t {
                self.set_date(nt);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn years_and_months() {
        let a = days_from_civil(1985, 1, 1) as f64 * DAY;
        let b = days_from_civil(2025, 1, 1) as f64 * DAY;
        // 40 years over ~400 points: marks every 5 years (base step ~ 1/25 year per 6 points)
        let marks = time_marks(GridInput { bounds: (a, b), base_step_size: YEAR * 0.3 });
        let labels: Vec<String> = marks.iter().filter(|m| m.step_size >= 5.0 * YEAR).map(|m| time_label(*m)).collect();
        assert_eq!(labels.first().map(String::as_str), Some("1985"));
        assert!(labels.contains(&"2000".to_string()));
        let marks = time_marks(GridInput { bounds: (a, a + 300.0 * DAY), base_step_size: 4.0 * DAY });
        assert!(marks.iter().any(|m| time_label(*m) == "1985-03"));
    }
}
