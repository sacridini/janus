//! The time series chart (src/app.cpp, uiSeries): the cursor's series (the overview's at once, the exact one when
//! read), the pins', the ROI's mean with its spread, and those of every other visible layer; in the chart's style,
//! as values, anomalies or z-scores, with trends; the current date as a line, a click or a drag picks the date. The
//! axis is a date axis: its marks fall on days, months or years (ImPlot's time axis), labelled as precisely as they
//! need.

use egui::{Color32, ComboBox, Ui};
use egui_plot::{
    Bar, BarChart, BoxElem, BoxPlot, BoxSpread, GridInput, GridMark, Line, MarkerShape, Plot, PlotPoints, Points, VLine,
};
use janus_cube::dates::{civil_from_days, days_from_civil};
use janus_cube::stats::{SeriesStats, cusum, month_of, monthly_climatology};

use crate::app::State;

const DAY: f64 = 86400.0;
const MONTH: f64 = 30.44 * DAY;
const YEAR: f64 = 365.2425 * DAY;

/// Marks of a date axis: the finest of days, months or years spaced enough for a label, and coarser ones.
pub fn time_marks(input: GridInput) -> Vec<GridMark> {
    let (a, b) = input.bounds;
    // egui asks for marks at least 8 points apart: the points of one value unit
    let per_point = input.base_step_size / 8.0;
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
    // the finest step whose labels (a year, a month or a day; ProggyClean's 7 points a character) fit apart, as
    // ImPlot's: every 2 years over 40 on a chart of 800 points
    let label = |months: u32| match months {
        0 => 10.0,
        m if m % 12 == 0 => 4.0,
        _ => 7.0,
    } * 7.0
        + 4.0;
    let i = steps.iter().position(|s| s.0 >= label(s.1) * per_point).unwrap_or(steps.len() - 1);
    let mut marks: Vec<GridMark> = Vec::new();
    // and the next coarser step whose marks are among these (a stronger line on some of them)
    let (s, m) = steps[i];
    let nested = |c: &&(f64, u32)| {
        if m == 0 { c.1 == 0 && (c.0 / s).fract() == 0.0 } else { c.1.is_multiple_of(m) }
    };
    let coarser = steps[i + 1..].iter().find(nested);
    for &(step, months) in std::iter::once(&steps[i]).chain(coarser) {
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

/// A date axis on `plot`: `time_marks`, labelled by `time_label` (all of them: the marks are already spaced for
/// their labels, which egui_plot would hide under 60 points).
pub fn time_axis(plot: Plot<'_>) -> Plot<'_> {
    plot.x_grid_spacer(time_marks)
        .custom_x_axes(vec![egui_plot::AxisHints::new_x().formatter(|m, _| time_label(m)).label_spacing(20.0..=21.0)])
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
    /// 0 lines, 1 lines + markers, 2 markers, 3 stems, 4 stairs, 5 bars
    pub style: usize,
    /// 0 values, 1 anomaly (- the series' mean), 2 z-score, 3 seasonal anomaly (- the month's mean), 4 CUSUM of
    /// the anomalies (seasonal ones when dated)
    pub values: usize,
    /// 0 none, 1 OLS, 2 Sen
    pub trend: usize,
    /// the Y axis locked to the map's colour range
    pub y_from_map: bool,
    /// the ROI: 0 mean only, 1 + the p10-p90 band, 2 + a box plot per date, 3 + a fan (p10-p90, p25-p75, median),
    /// 4 + its values' density
    pub roi_spread: usize,
    /// a strip of the main series' values as colours under the chart (warming stripes)
    pub stripes: bool,
}

impl Default for ChartOptions {
    fn default() -> Self {
        ChartOptions { style: 1, values: 0, trend: 1, y_from_map: false, roi_spread: 1, stripes: false }
    }
}

impl ChartOptions {
    /// Options as `--chart` gives them: style=N,values=N,trend=N,spread=N,stripes=0|1 (those out of range left);
    /// layers=0|1 (the active layer's series or every visible layer's) returned, the State's to set.
    pub fn set(&mut self, spec: &str) -> Option<i32> {
        let mut layers = None;
        for (k, v) in spec.split(',').filter_map(|kv| kv.split_once('=')) {
            let Ok(v) = v.trim().parse::<usize>() else { continue };
            match k.trim() {
                "style" if v < 6 => self.style = v,
                "values" if v < VALUES.len() => self.values = v,
                "trend" if v < 3 => self.trend = v,
                "spread" if v < 5 => self.roi_spread = v,
                "stripes" => self.stripes = v != 0,
                "layers" => layers = Some(i32::from(v != 0)),
                _ => {}
            }
        }
        layers
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

/// The "values" option's names (ChartOptions::values).
const VALUES: [&str; 5] =
    ["Values", "Anomaly (- mean)", "Standardized (z-score)", "Seasonal anomaly", "CUSUM of anomalies"];

/// How a series is shown: as stored, minus its mean, standardized, minus its calendar month's mean (the seasonal
/// cycle taken out), or the running sum of those anomalies (CUSUM: a change of slope is a change of level).
pub(crate) struct AsShown {
    values: usize,
    mean: f64,
    std: f64,
    /// the mean of each calendar month (a dated series' seasonal anomaly and CUSUM)
    clim: Option<[f64; 12]>,
}

impl AsShown {
    /// `xs`: the series' times (Unix seconds when `dated`).
    pub fn new(values: usize, st: &SeriesStats, xs: &[f64], vals: &[f32], dated: bool) -> Self {
        let clim = (dated && values >= 3).then(|| monthly_climatology(xs, vals));
        AsShown { values, mean: st.mean, std: st.std, clim }
    }

    /// The anomaly of a value at time x: from its month's mean (the series' mean for a month without
    /// observations), or from the series' mean.
    fn anomaly(&self, v: f64, x: f64) -> f64 {
        match self.clim.map(|c| c[month_of(x)]) {
            Some(m) if m.is_finite() => v - m,
            _ => v - self.mean,
        }
    }

    /// A value at time x as shown; NaN with CUSUM (a running sum is of a series, not of a value: a model's curve
    /// or a percentile is not drawn).
    pub fn at(&self, v: f64, x: f64) -> f64 {
        match self.values {
            1 => v - self.mean,
            2 if self.std > 0.0 => (v - self.mean) / self.std,
            2 => 0.0,
            3 => self.anomaly(v, x),
            4 => f64::NAN,
            _ => v,
        }
    }

    /// A series as shown (`xs` its times).
    pub fn series(&self, xs: &[f64], vals: &[f32]) -> Vec<f64> {
        if self.values == 4 {
            let a: Vec<f64> = xs.iter().zip(vals).map(|(&x, &v)| self.anomaly(f64::from(v), x)).collect();
            return cusum(&a);
        }
        xs.iter().zip(vals).map(|(&x, &v)| self.at(f64::from(v), x)).collect()
    }

    /// A trend line's value: the anomalies' trend is the series' one less its mean; none of a CUSUM.
    fn trend(&self, v: f64) -> f64 {
        match self.values {
            3 => v - self.mean,
            4 => f64::NAN,
            _ => self.at(v, 0.0),
        }
    }

    /// Whether 0 is the series' level (stems and bars from 0, stripes symmetric about it), not its mean.
    fn relative(&self) -> bool {
        self.values != 0
    }
}

/// Each date's column on the chart (screen x): from halfway to the previous date to halfway to the next, the outer
/// ones as wide on both sides; at least a point wide.
fn date_columns(tr: &egui_plot::PlotTransform, xs: &[f64]) -> Vec<(f32, f32)> {
    let sx = |x: f64| tr.position_from_point(&egui_plot::PlotPoint::new(x, 0.0)).x;
    let n = xs.len();
    let mid = |t: usize| 0.5 * (sx(xs[t]) + sx(xs[t + 1]));
    let frame = tr.frame();
    (0..n)
        .map(|t| {
            let left = if t > 0 {
                mid(t - 1)
            } else if n > 1 {
                2.0 * sx(xs[0]) - mid(0)
            } else {
                frame.left()
            };
            let right = if t + 1 < n {
                mid(t)
            } else if n > 1 {
                2.0 * sx(xs[n - 1]) - mid(n - 2)
            } else {
                frame.right()
            };
            (left, right.max(left + 1.0))
        })
        .collect()
}

/// The smallest gap between consecutive dates (a box's or a bar's width), if any.
fn min_gap(xs: &[f64]) -> Option<f64> {
    let g = xs.windows(2).map(|w| w[1] - w[0]).filter(|g| *g > 0.0).fold(f64::INFINITY, f64::min);
    g.is_finite().then_some(g)
}

/// A band between `lo` and `hi` (shown values) in each run of dates where both are.
fn band(
    plot: &mut egui_plot::PlotUi,
    name: &str,
    xs: &[f64],
    lo: &[f64],
    hi: &[f64],
    runs: &[(usize, usize)],
    fill: Color32,
) {
    for &(rb, re) in runs {
        let idx: Vec<usize> = (rb..re.min(xs.len())).filter(|&t| lo[t].is_finite() && hi[t].is_finite()).collect();
        if idx.len() > 1 {
            let x: Vec<f64> = idx.iter().map(|&t| xs[t]).collect();
            let a: Vec<f64> = idx.iter().map(|&t| lo[t]).collect();
            let b: Vec<f64> = idx.iter().map(|&t| hi[t]).collect();
            plot.add(egui_plot::FilledArea::new(name, &x, &a, &b).fill_color(fill).stroke(egui::Stroke::NONE));
        }
    }
}

/// Points (x, y) of the valid values in each run of dates (a spectrum's pieces), a line not drawn across a gap.
fn pieces(xs: &[f64], ys: &[f64], runs: &[(usize, usize)]) -> Vec<Vec<[f64; 2]>> {
    let mut out = Vec::new();
    for &(b, e) in runs {
        let mut run = Vec::new();
        for t in b..e.min(xs.len()) {
            if !ys[t].is_finite() {
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
    dated: bool,
    /// the bars' colours below and above the level (the map's anomaly colormap's ends)
    bars: (Color32, Color32),
    /// the series drawn as bars, side by side at each date, and the next one's place
    bar_slots: usize,
    bar_slot: std::cell::Cell<usize>,
}

impl Plotted<'_> {
    /// How a series of the active layer is shown (a class as it is).
    fn shown(&self, st: &SeriesStats, vals: &[f32]) -> AsShown {
        AsShown::new(if self.categorical { 0 } else { self.o.values }, st, self.xs, vals, self.dated)
    }

    /// A series in the chart's style, with its trend (same name: hidden together).
    /// `look`: its colour and line width.
    fn series(
        &self,
        plot: &mut egui_plot::PlotUi,
        name: &str,
        vals: &[f32],
        (st, sh): (&SeriesStats, &AsShown),
        (color, width): (Color32, f32),
        trend: bool,
    ) {
        let n = self.xs.len();
        if vals.len() != n {
            return;
        }
        // legible on the chart's background (the light themes darken the data colours)
        let color = crate::theme::on_plot(color);
        let ys = sh.series(self.xs, vals);
        let valid: Vec<[f64; 2]> = (0..n).filter(|&t| ys[t].is_finite()).map(|t| [self.xs[t], ys[t]]).collect();
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
                let base = if sh.relative() { 0.0 } else { st.mean };
                for p in &valid {
                    plot.line(Line::new(name, PlotPoints::from(vec![[p[0], base], *p])).color(color).width(width));
                }
                plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(2.5));
            }
            4 => stairs(plot),
            _ => {
                // bars from the level, coloured by side (a dry year, a wet one), the series' colour on their tops
                let base = if sh.relative() { 0.0 } else { st.mean };
                let w = min_gap(self.xs).map_or(1.0, |g| g * 0.8) / self.bar_slots.max(1) as f64;
                let k = self.bar_slot.replace(self.bar_slot.get() + 1);
                let dx = w * (k as f64 + 0.5 - self.bar_slots.max(1) as f64 * 0.5);
                let valid: Vec<[f64; 2]> = valid.iter().map(|p| [p[0] + dx, p[1]]).collect();
                let (below, above) = self.bars;
                let bars = valid
                    .iter()
                    .map(|p| {
                        Bar::new(p[0], p[1] - base)
                            .base_offset(base)
                            .width(w)
                            .fill(if p[1] >= base { above } else { below }.gamma_multiply(0.8))
                            .stroke(egui::Stroke::NONE)
                    })
                    .collect();
                plot.bar_chart(BarChart::new(name, bars).color(color));
                plot.points(Points::new(name, PlotPoints::from(valid)).color(color).radius(1.8));
            }
        }
        if trend && self.o.trend != 0 && st.n >= 2 && !self.spectral && n >= 1 && self.o.values != 4 {
            let (a, b) =
                if self.o.trend == 1 { (st.ols_intercept, st.ols_slope) } else { (st.sen_intercept, st.sen_slope) };
            let y = |yr: f64| sh.trend(a + b * yr);
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

    /// The warming stripes under the chart: a colour per date of the main series (the cursor's, else the ROI's
    /// mean, else the last pin's), each from halfway to the previous date to halfway to the next, on the chart's
    /// time axis. Values in the map's colours and range; anomalies, z-scores and CUSUM in the anomaly colormap,
    /// symmetric about 0.
    fn stripes_ui(&self, ui: &mut Ui, pl: &Plotted<'_>, tr: &egui_plot::PlotTransform, height: f32) {
        let sr = &self.series;
        let main = sr
            .hover
            .as_ref()
            .map(|h| (&h.values, &h.stats))
            .or_else(|| (!sr.roi.mean.is_empty()).then_some((&sr.roi.mean, &sr.roi.stats)))
            .or_else(|| sr.pins.last().map(|p| (&p.values, &p.stats)));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), egui::Sense::hover());
        let Some((vals, st)) = main.filter(|(v, _)| v.len() == pl.xs.len()) else { return };
        let sh = pl.shown(st, vals);
        let ys = sh.series(pl.xs, vals);
        let colour: Box<dyn Fn(f64) -> Color32> = if sh.relative() {
            let cm = &janus_gpu::colormaps::all()[self.display.cmap[janus_gpu::Mode::Anomaly as usize]];
            let a = ys.iter().filter(|y| y.is_finite()).fold(0f64, |m, y| m.max(y.abs()));
            Box::new(move |y| {
                let c = cm.sample(if a > 0.0 { (0.5 + 0.5 * y / a) as f32 } else { 0.5 });
                Color32::from_rgb(c[0], c[1], c[2])
            })
        } else {
            let cm = &janus_gpu::colormaps::all()[self.display.cmap[janus_gpu::Mode::Value as usize]];
            let r = self.display.ranges[janus_gpu::Mode::Value as usize];
            let (lo, span) = (f64::from(r.lo), f64::from(r.hi - r.lo));
            Box::new(move |y| {
                let c = cm.sample(if span > 0.0 { ((y - lo) / span).clamp(0.0, 1.0) as f32 } else { 0.5 });
                Color32::from_rgb(c[0], c[1], c[2])
            })
        };
        let frame = *tr.frame();
        let painter = ui.painter_at(egui::Rect::from_x_y_ranges(frame.x_range(), rect.y_range()));
        for (t, (left, right)) in date_columns(tr, pl.xs).into_iter().enumerate().filter(|(t, _)| ys[*t].is_finite()) {
            painter.rect_filled(egui::Rect::from_x_y_ranges(left..=right, rect.y_range()), 0.0, colour(ys[t]));
        }
        if let Some(p) = resp.hover_pos().filter(|p| frame.x_range().contains(p.x)) {
            let x = tr.value_from_position(p).x;
            let Some(s) = &self.session else { return };
            let t = s.info.nearest_layer(x);
            if ys[t].is_finite() {
                resp.on_hover_text_at_pointer(format!(
                    "{}\n{}",
                    s.info.layers[t].label,
                    janus_cube::multidim::format_g(ys[t], 4)
                ));
            }
        }
    }

    /// The ROI's density at each date, under the series: its column split where its quantiles are (2.5 % of the
    /// pixels between two), the more opaque the closer they are (the denser the values). Two groups of pixels (a
    /// half cleared) show as two bright bands where the mean runs between them.
    fn roi_density(&self, pl: &Plotted<'_>, tr: &egui_plot::PlotTransform) -> Vec<egui::Shape> {
        let roi = &self.series.roi;
        if roi.q.len() != pl.xs.len() {
            return Vec::new();
        }
        let sh = pl.shown(&roi.stats, &roi.mean);
        let sy = |x: f64, y: f64| tr.position_from_point(&egui_plot::PlotPoint::new(x, y)).y;
        // a column's cells (y top, y bottom, density), in screen points
        let cells: Vec<Vec<(f32, f32, f64)>> = roi
            .q
            .iter()
            .zip(pl.xs)
            .map(|(q, &x)| {
                let v: Vec<f64> = q.iter().map(|&v| sh.at(f64::from(v), x)).collect();
                if v.iter().any(|v| !v.is_finite()) {
                    return Vec::new();
                }
                v.windows(2)
                    .map(|w| {
                        let (a, b) = (sy(x, w[0]), sy(x, w[1]));
                        (b.min(a), a.max(b), 1.0 / f64::from((a - b).abs().max(0.5)))
                    })
                    .collect()
            })
            .collect();
        // opaque from the 90th percentile of the densities up (a column of equal values would be infinitely dense)
        let mut d: Vec<f64> = cells.iter().flatten().map(|c| c.2).collect();
        if d.is_empty() {
            return Vec::new();
        }
        let k = d.len() * 9 / 10;
        let top = *d.select_nth_unstable_by(k, f64::total_cmp).1;
        // light grey (darker on a light theme): the ROI's yellow mean stays legible over it
        let fill = crate::theme::on_plot(Color32::from_gray(225));
        let mut shapes = Vec::new();
        for ((left, right), col) in date_columns(tr, pl.xs).into_iter().zip(&cells) {
            for &(y0, y1, dens) in col {
                let a = (dens / top).clamp(0.0, 1.0).powf(0.7) as f32 * 0.85;
                let r = egui::Rect::from_x_y_ranges(left..=right, y0..=y1.max(y0 + 1.0));
                shapes.push(egui::Shape::rect_filled(r, 0.0, fill.gamma_multiply(a)));
            }
        }
        shapes
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
            // a button moves to the next row whole, its text never wrapped in a sliver of the row's end
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            const STYLES: [&str; 6] = ["Lines", "Lines + markers", "Markers", "Stems", "Stairs", "Bars"];
            ComboBox::from_id_salt("style")
                .width(130.0)
                .selected_text(STYLES[o.style])
                .show_ui(ui, |ui| {
                    for (i, n) in STYLES.iter().enumerate() {
                        ui.selectable_value(&mut o.style, i, *n);
                    }
                })
                .response
                .on_hover_text(
                    "Bars: from the mean (from 0 for anomalies, z-scores and CUSUM), in the map's anomaly colours \
                     below and above it",
                );
            let seasonal = info.time_is_date && !info.spectral;
            ComboBox::from_id_salt("values")
                .width(160.0)
                .selected_text(VALUES[o.values])
                .show_ui(ui, |ui| {
                    for (i, n) in VALUES.iter().enumerate() {
                        ui.add_enabled_ui(i != 3 || seasonal, |ui| ui.selectable_value(&mut o.values, i, *n));
                    }
                })
                .response
                .on_hover_text(
                    "Values: as stored in the raster\nAnomaly: each series minus its own mean (compares shape, not \
                     level)\nZ-score: (value - mean) / std (compares series of different scales)\nSeasonal anomaly: \
                     each value minus the mean of its calendar month over every year (the seasonal cycle taken \
                     out: a drought, an El Nino stand out)\nCUSUM: the running sum of the anomalies (seasonal ones \
                     for dated series); a change of slope marks a change of level, a slow degradation that the raw \
                     series hides in its noise",
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
            ui.add_enabled(!categorical, egui::Checkbox::new(&mut o.stripes, "Stripes")).on_hover_text(
                "A strip of colours under the chart, one per date (warming stripes): the cursor's series (or the \
                 ROI's, or the last pin's) as the map colours it; anomalies, z-scores and CUSUM in the map's anomaly \
                 colours, symmetric about 0",
            );
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
                const SPREAD: [&str; 5] = ["ROI: mean", "ROI: p10-p90", "ROI: box plot", "ROI: fan", "ROI: density"];
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
                         and 90th percentiles\nBox plot: a box from p25 to p75 with the median, whiskers to p10 and p90\n\
                         Fan: p10-p90 and p25-p75 as two bands, darker inside, and the median dashed (clearer than \
                         boxes over many dates)\nDensity: every pixel's value at each date as a shade, the brighter \
                         the more pixels there (shows the ROI splitting in two where the mean runs between)\n(none with \
                         CUSUM: a running sum is of the mean only)",
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
            plot = time_axis(plot);
        } else {
            plot = plot.x_axis_label(if info.spectral { "wavelength (nm)" } else { "date (index)" });
        }
        if o.values > 0 && !categorical {
            plot = plot.y_axis_label(["", "anomaly", "z-score", "seasonal anomaly", "CUSUM"][o.values]);
        }
        // the stripes' strip under the chart
        const STRIP: f32 = 12.0;
        let stripes = o.stripes && !categorical;
        if stripes {
            plot = plot.height((ui.available_height() - STRIP - 4.0).max(40.0));
        }
        if o.y_from_map && o.values == 0 {
            let r = self.display.ranges[janus_gpu::Mode::Value as usize];
            let m = f64::from(r.hi - r.lo) * 0.05;
            plot = plot
                .include_y(f64::from(r.lo) - m)
                .include_y(f64::from(r.hi) + m)
                .auto_bounds(egui::Vec2b::new(true, false))
                .default_y_bounds(f64::from(r.lo) - m, f64::from(r.hi) + m);
        }
        // the X axis from the first date to the last, no margin (after auto_bounds above, which would undo it); half
        // a box or a bar beyond each end, which would be cut in two
        if n > 1 {
            let half = if categorical {
                0.0
            } else if o.style == 5 {
                0.4
            } else if self.series.roi_rect.is_some() && o.roi_spread == 2 {
                0.3
            } else {
                0.0
            };
            let pad = min_gap(&xs).map_or(0.0, |g| g * half);
            plot = plot.default_x_bounds(xs[0] - pad, xs[n - 1] + pad);
        } else {
            plot = plot.include_x(xs[0] - 1.0).include_x(xs[0] + 1.0);
        }
        // nothing to draw yet (no cursor, pin or ROI): 0 to 1, as ImPlot's empty plot, not the date line's span
        let sr = &self.series;
        // the ROI's density: from its lowest values to its highest
        if o.roi_spread == 4 && !categorical && o.values != 4 && sr.roi.q.len() == n {
            let sh = AsShown::new(o.values, &sr.roi.stats, &xs, &sr.roi.mean, dated);
            let ends =
                sr.roi.q.iter().zip(&xs).flat_map(|(q, &x)| [sh.at(f64::from(q[0]), x), sh.at(f64::from(q[40]), x)]);
            for y in ends.filter(|y| y.is_finite()) {
                plot = plot.include_y(y);
            }
        }
        if sr.pins.is_empty() && sr.hover.as_ref().is_none_or(|h| h.values.is_empty()) && sr.roi.mean.is_empty() {
            plot = plot.include_y(0.0).include_y(1.0);
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
        let colormap = |m: janus_gpu::Mode| &janus_gpu::colormaps::all()[self.display.cmap[m as usize]];
        let rgb = |c: [u8; 4]| crate::theme::on_plot(Color32::from_rgb(c[0], c[1], c[2]));
        let anomaly_cmap = colormap(janus_gpu::Mode::Anomaly);
        let pl = Plotted {
            xs: &xs,
            years: &years,
            runs: &runs,
            o,
            categorical,
            spectral: info.spectral,
            dated,
            bars: (rgb(anomaly_cmap.sample(0.12)), rgb(anomaly_cmap.sample(0.88))),
            bar_slots: usize::from(!self.series.roi.mean.is_empty() && !categorical)
                + self.series.pins.len()
                + usize::from(self.series.hover.is_some()),
            bar_slot: std::cell::Cell::new(0),
        };
        let t = self.display.t;
        let sr = &self.series;
        let background = ui.painter().add(egui::Shape::Noop);
        let density = ui.painter().add(egui::Shape::Noop);
        let orange = Color32::from_rgb(255, 153, 51);
        let yellow = |a: u8| Color32::from_rgba_unmultiplied(255, 209, 61, a);
        let response = plot.show(ui, |plot| {
            let roi = &sr.roi;
            if !roi.mean.is_empty() && !categorical {
                let name = if roi.sampled() { "ROI mean (subsampled)" } else { "ROI mean" };
                let sh = pl.shown(&roi.stats, &roi.mean);
                // the percentiles as the mean is shown (none with CUSUM)
                let f = |v: &[f32]| -> Vec<f64> { xs.iter().zip(v).map(|(&x, &p)| sh.at(f64::from(p), x)).collect() };
                match o.roi_spread {
                    1 => band(plot, name, &xs, &f(&roi.p10), &f(&roi.p90), &runs, yellow(46)),
                    3 => {
                        // a fan: the outer band, the inner one over it (darker), the median dashed
                        band(plot, name, &xs, &f(&roi.p10), &f(&roi.p90), &runs, yellow(34));
                        band(plot, name, &xs, &f(&roi.p25), &f(&roi.p75), &runs, yellow(58));
                        let median = crate::theme::on_plot(crate::series::ROI_COLOR);
                        for piece in pieces(&xs, &f(&roi.p50), &runs) {
                            plot.line(
                                Line::new(name, PlotPoints::from(piece))
                                    .color(median.gamma_multiply(0.8))
                                    .width(1.0)
                                    .style(egui_plot::LineStyle::dashed_dense()),
                            );
                        }
                    }
                    _ => {}
                }
                // the trend of a partially computed ROI would be misleading: wait for every date
                pl.series(plot, name, &roi.mean, (&roi.stats, &sh), (crate::series::ROI_COLOR, 2.0), roi.complete());
                crate::zeit::draw_fit(plot, name, self.roi_fit(), &sh, &info);
                if o.roi_spread == 2 {
                    // a box per date: p25 to p75 with the median, whiskers to p10 and p90
                    let width = min_gap(&xs).map_or(0.6, |g| g * 0.6);
                    let (p10, p25, p50, p75, p90) = (f(&roi.p10), f(&roi.p25), f(&roi.p50), f(&roi.p75), f(&roi.p90));
                    let boxes: Vec<BoxElem> = (0..n)
                        .filter(|&t| p25[t].is_finite())
                        .map(|t| {
                            BoxElem::new(xs[t], BoxSpread::new(p10[t], p25[t], p50[t], p75[t], p90[t]))
                                .box_width(width)
                                .whisker_width(width * 0.5)
                                .fill(yellow(56))
                                .stroke(egui::Stroke::new(1.0, crate::series::ROI_COLOR))
                        })
                        .collect();
                    plot.box_plot(BoxPlot::new(name, boxes));
                }
            }
            for p in &sr.pins {
                let name = format!("Pin {} ({}, {}){}", p.id, p.x, p.y, if p.exact { "" } else { " approx." });
                let sh = pl.shown(&p.stats, &p.values);
                pl.series(plot, &name, &p.values, (&p.stats, &sh), (p.color, 1.5), true);
                let fit = self.zeit.fit_of(info.id, crate::zeit::Which::Pin(p.id), p.x, p.y);
                crate::zeit::draw_fit(plot, &name, fit, &sh, &info);
            }
            if let Some(h) = &sr.hover {
                let name = format!("Cursor ({}, {}){}", h.x, h.y, if h.exact { "" } else { " approx." });
                let sh = pl.shown(&h.stats, &h.values);
                pl.series(plot, &name, &h.values, (&h.stats, &sh), (h.color, 2.0), true);
                let fit = self.zeit.fit_of(info.id, crate::zeit::Which::Hover, h.x, h.y);
                crate::zeit::draw_fit(plot, &name, fit, &sh, &info);
                self.draw_class_series(plot, &sh);
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
                    let values = if l.classes.on() { 0 } else { o.values };
                    let shown = |st: &SeriesStats, v: &[f32]| AsShown::new(values, st, &lx, v, li_info.time_is_date);
                    let other = |plot: &mut egui_plot::PlotUi,
                                 name: &str,
                                 v: &[f32],
                                 sh: &AsShown,
                                 color: Color32,
                                 width: f32| {
                        if v.len() != lx.len() {
                            return;
                        }
                        let ys = sh.series(&lx, v);
                        let color = crate::theme::on_plot(color);
                        let c = Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 217);
                        for piece in pieces(&lx, &ys, &lruns) {
                            plot.line(Line::new(name, PlotPoints::from(piece)).color(c).width(width));
                        }
                        let pts: Vec<[f64; 2]> =
                            (0..lx.len()).filter(|&t| ys[t].is_finite()).map(|t| [lx[t], ys[t]]).collect();
                        plot.points(
                            Points::new(name, PlotPoints::from(pts)).color(color).radius(3.0).shape(layer_marker(li)),
                        );
                    };
                    for p in &l.series.pins {
                        let name = format!("Pin {} [{}]{}", p.id, l.name, if p.exact { "" } else { " approx." });
                        let sh = shown(&p.stats, &p.values);
                        other(plot, &name, &p.values, &sh, p.color, 1.2);
                        let fit = self.zeit.fit_of(li_info.id, crate::zeit::Which::Pin(p.id), p.x, p.y);
                        crate::zeit::draw_fit(plot, &name, fit, &sh, li_info);
                    }
                    if let Some(h) = &l.series.hover {
                        let name = format!("Cursor [{}]{}", l.name, if h.exact { "" } else { " approx." });
                        let sh = shown(&h.stats, &h.values);
                        other(plot, &name, &h.values, &sh, h.color, 1.6);
                        let fit = self.zeit.fit_of(li_info.id, crate::zeit::Which::Hover, h.x, h.y);
                        crate::zeit::draw_fit(plot, &name, fit, &sh, li_info);
                    }
                    let r = &l.series.roi;
                    if !r.mean.is_empty() && !l.classes.on() {
                        let name = format!("ROI mean [{}]{}", l.name, if r.sampled() { " (subsampled)" } else { "" });
                        let sh = shown(&r.stats, &r.mean);
                        if o.roi_spread > 0 {
                            let f = |v: &[f32]| -> Vec<f64> {
                                lx.iter().zip(v).map(|(&x, &p)| sh.at(f64::from(p), x)).collect()
                            };
                            band(plot, &name, &lx, &f(&r.p10), &f(&r.p90), &lruns, yellow(31));
                        }
                        other(plot, &name, &r.mean, &sh, crate::series::ROI_COLOR, 1.8);
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
        if self.chart.roi_spread == 4 && !categorical {
            let shapes = self.roi_density(&pl, tr);
            ui.painter().with_clip_rect(*tr.frame()).set(density, egui::Shape::Vec(shapes));
        }
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
        if stripes {
            self.stripes_ui(ui, &pl, tr, STRIP);
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
    fn shown_values() {
        // monthly, 2009-2010 on the 15th: a cycle (the month) and +10 the second year
        let start = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let mut xs = Vec::new();
        for y in [2009, 2010] {
            xs.extend(start.iter().map(|s| (days_from_civil(y, 1, 1) + s + 14) as f64 * DAY));
        }
        let v: Vec<f32> = (0..24).map(|t| (t % 12) as f32 + if t >= 12 { 10.0 } else { 0.0 }).collect();
        let years: Vec<f64> = (0..24).map(|t| 2009.0 + t as f64 / 12.0).collect();
        let st = janus_cube::stats::series_stats(&years, &v);
        // the seasonal anomaly: the cycle gone, -5 then +5
        let a = AsShown::new(3, &st, &xs, &v, true).series(&xs, &v);
        assert!(a.iter().take(12).all(|&y| y == -5.0) && a.iter().skip(12).all(|&y| y == 5.0));
        // its running sum: down to -60 at the turn, back to 0; nothing of a single value
        let c = AsShown::new(4, &st, &xs, &v, true);
        let s = c.series(&xs, &v);
        assert_eq!((s[11], s[23]), (-60.0, 0.0));
        assert!(c.at(3.0, xs[0]).is_nan());
        // undated: from the series' mean
        let u = AsShown::new(3, &st, &xs, &v, false).series(&xs, &v);
        assert_eq!(u[0], -st.mean);
    }

    #[test]
    fn years_and_months() {
        let a = days_from_civil(1985, 1, 1) as f64 * DAY;
        let b = days_from_civil(2025, 1, 1) as f64 * DAY;
        // 40 years over ~800 points (egui's 8 points = 0.3 year): a mark every 2 years, every 10 a stronger one
        let marks = time_marks(GridInput { bounds: (a, b), base_step_size: YEAR * 0.3 });
        let labels: Vec<String> = marks.iter().map(|m| time_label(*m)).collect();
        assert_eq!(labels.first().map(String::as_str), Some("1986"));
        assert_eq!(labels.len(), 20);
        assert_eq!(marks.iter().filter(|m| m.step_size >= 10.0 * YEAR).count(), 4);
        let marks = time_marks(GridInput { bounds: (a, a + 300.0 * DAY), base_step_size: 4.0 * DAY });
        assert!(marks.iter().any(|m| time_label(*m) == "1985-03"));
    }
}
