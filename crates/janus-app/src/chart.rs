//! The time series chart (src/app.cpp, the Time series panel): the cursor's series (the overview's at once, the
//! exact one when read) and the pins', the current date as a line; a click or a drag picks the date. The axis is
//! a date axis: its marks fall on days, months or years (ImPlot's time axis), labelled as precisely as they need.

use egui::Ui;
use egui_plot::{GridInput, GridMark, Line, Plot, PlotPoints, Points, VLine};
use janus_cube::dates::{civil_from_days, days_from_civil};

use crate::app::State;
use crate::theme;

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

/// A series split where values are missing (a line is not drawn across a gap), with its points.
fn add_series(plot: &mut egui_plot::PlotUi, name: &str, xs: &[f64], values: &[f32], color: egui::Color32, width: f32) {
    let mut run: Vec<[f64; 2]> = Vec::new();
    let mut all = Vec::new();
    let flush = |run: &mut Vec<[f64; 2]>, plot: &mut egui_plot::PlotUi| {
        if run.len() > 1 {
            plot.line(Line::new(name, PlotPoints::from(std::mem::take(run))).color(color).width(width));
        }
        run.clear();
    };
    for (&x, &v) in xs.iter().zip(values) {
        if v.is_nan() {
            flush(&mut run, plot);
        } else {
            run.push([x, f64::from(v)]);
            all.push([x, f64::from(v)]);
        }
    }
    flush(&mut run, plot);
    plot.points(Points::new(name, PlotPoints::from(all)).color(color).radius(2.0));
}

impl State {
    pub fn series_ui(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else {
            ui.weak("Hover the map: the pixel's series. Click: a pin.");
            return;
        };
        let info = &s.info;
        let xs: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let dated = info.time_is_date;
        let mut plot = Plot::new("series")
            .allow_drag(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .legend(egui_plot::Legend::default().position(egui_plot::Corner::RightTop).background_alpha(0.6));
        if dated {
            plot = plot.x_grid_spacer(time_marks).x_axis_formatter(|m, _| time_label(m));
        }
        // the whole series, a twentieth of it beyond each end, and the values of the map's stretch
        if let (Some(&a), Some(&b)) = (xs.first(), xs.last()) {
            let pad = ((b - a) / 20.0).max(if dated { 86400.0 } else { 0.5 });
            let r = self.display.ranges[janus_gpu::Mode::Value as usize];
            plot = plot.include_x(a - pad).include_x(b + pad).include_y(r.lo).include_y(r.hi);
        }
        let info2 = info.clone();
        plot = plot.label_formatter(move |p| {
            let (egui_plot::HoverPosition::NearDataPoint { position, .. }
            | egui_plot::HoverPosition::Elsewhere { position }) = p;
            let x = if info2.time_is_date { info2.format_time(position.x) } else { format!("{:.1}", position.x + 1.0) };
            Some(format!("{x}\n{:.4}", position.y))
        });
        let t = self.display.t;
        let response = plot.show(ui, |plot| {
            if let Some(hv) = self.hover {
                let (values, exact) = match s.hover_exact(hv.src.0, hv.src.1) {
                    Some(e) => (e.values, true),
                    None => (s.overview.series(hv.ov.0, hv.ov.1), false),
                };
                let name = if exact { "cursor" } else { "cursor (overview)" };
                add_series(plot, name, &xs, &values, theme::cursor_series(), if exact { 1.5 } else { 1.0 });
            }
            for (i, pin) in self.pins.iter().enumerate() {
                let values = s
                    .pin_exact(pin.at.src.0, pin.at.src.1)
                    .map_or_else(|| s.overview.series(pin.at.ov.0, pin.at.ov.1), |e| e.values);
                add_series(plot, &format!("pin {}", i + 1), &xs, &values, pin.color, 1.5);
            }
            plot.vline(VLine::new("date", xs[t]).color(egui::Color32::from_rgb(242, 154, 56)).width(1.0));
            let primary = plot.response().is_pointer_button_down_on() && plot.ctx().input(|i| i.pointer.primary_down());
            primary.then(|| plot.pointer_coordinate()).flatten()
        });
        // a click or a drag on the chart picks the nearest date
        if let Some(p) = response.inner {
            let nt = info.nearest_layer(p.x);
            if nt != self.display.t {
                self.display.t = nt;
                s.overview.set_focus(nt);
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
