//! The Scatter tab's trajectory (View > Analysis > Scatter): a place's series on one layer against its series on
//! another, date by date, over the cloud of pixels: the path a pixel takes in the plane of the two (NDVI x NDMI,
//! vegetation x rain). A season's loop (vegetation answering the rain late: hysteresis), a disturbance leaving the
//! forest's cluster and the years it takes to come back. Coloured by time, arrows the way it goes.

use egui::Color32;
use egui_plot::{Arrows, Line, PlotPoint, PlotPoints, PlotUi, Points, Text};

use crate::app::State;
use crate::theme;

/// The index of the time in `t` (sorted) nearest to `x`, the earlier on a tie.
fn nearest(t: &[f64], x: f64) -> Option<usize> {
    let k = t.partition_point(|&v| v < x);
    [k.checked_sub(1), (k < t.len()).then_some(k)]
        .into_iter()
        .flatten()
        .min_by(|&a, &b| (t[a] - x).abs().total_cmp(&(t[b] - x).abs()))
}

/// The pairs (i, j) of X's and Y's dates (sorted) at the same time: every index when the dates are the same, else
/// the dates each the other's nearest, within half the larger of the two series' usual (median) spacings (a yearly
/// Y meets a monthly X at the month nearest to it, not at the first within reach).
pub fn pair_dates(xt: &[f64], yt: &[f64]) -> Vec<(usize, usize)> {
    if xt == yt {
        return (0..xt.len()).map(|i| (i, i)).collect();
    }
    let spacing = |t: &[f64]| {
        let mut g: Vec<f64> = t.windows(2).map(|w| (w[1] - w[0]).abs()).filter(|g| *g > 0.0).collect();
        if g.is_empty() {
            return 0.0;
        }
        let k = g.len() / 2;
        *g.select_nth_unstable_by(k, f64::total_cmp).1
    };
    let tol = 0.5 * spacing(xt).max(spacing(yt));
    xt.iter()
        .enumerate()
        .filter_map(|(i, &x)| {
            let j = nearest(yt, x)?;
            (nearest(xt, yt[j]) == Some(i) && (yt[j] - x).abs() <= tol).then_some((i, j))
        })
        .collect()
}

/// A trajectory ready to draw: its points in time order, the date of each (X's), the one at the current date.
pub struct Trajectory {
    pub name: String,
    pub pts: Vec<[f64; 2]>,
    pub labels: Vec<String>,
    pub current: Option<usize>,
}

impl State {
    /// The series of layer `li` a trajectory can follow: the cursor, a pin ("pin<id>") or the ROI's mean ("roi").
    fn series_in(&self, li: usize, key: &str) -> Option<&[f32]> {
        let (hover, pins, roi) = if li == self.active {
            (&self.series.hover, &self.series.pins, &self.series.roi)
        } else {
            let s = &self.layers.get(li)?.series;
            (&s.hover, &s.pins, &s.roi)
        };
        match key {
            "cursor" => hover.as_ref().map(|h| h.values.as_slice()),
            "roi" => (!roi.mean.is_empty()).then_some(roi.mean.as_slice()),
            k => {
                let id: u32 = k.strip_prefix("pin")?.parse().ok()?;
                pins.iter().find(|p| p.id == id).map(|p| p.values.as_slice())
            }
        }
    }

    /// What a trajectory can follow (key, name): the cursor, the pins, the ROI's mean, as the chart has them.
    pub(crate) fn trajectory_sources(&self) -> Vec<(String, String)> {
        let mut v = vec![("off".to_string(), "Off".to_string())];
        if let Some(h) = &self.series.hover {
            v.push(("cursor".into(), format!("Cursor ({}, {})", h.x, h.y)));
        }
        for p in &self.series.pins {
            v.push((format!("pin{}", p.id), format!("Pin {} ({}, {})", p.id, p.x, p.y)));
        }
        if !self.series.roi.mean.is_empty() {
            v.push(("roi".into(), "ROI mean".into()));
        }
        v
    }

    /// The trajectory of `key` from layer `xl` (X) to layer `yl` (Y), or why there is none.
    pub(crate) fn trajectory(&self, key: &str, xl: usize, yl: usize) -> Result<Trajectory, String> {
        let (Some(xs), Some(ys)) = (self.layer_session(xl), self.layer_session(yl)) else { return Err(String::new()) };
        if xl == yl {
            return Err("X and Y are the same layer: its trajectory is the line Y = X.".into());
        }
        let others = self.series.chart_layers == 1;
        let (Some(vx), Some(vy)) = (self.series_in(xl, key), self.series_in(yl, key)) else {
            return Err(if others {
                "Not on both layers here (outside one of them?).".into()
            } else {
                "The other layer's series are read with \"All visible layers\" on the chart.".into()
            });
        };
        let xt: Vec<f64> = xs.info.layers.iter().map(|l| l.time).collect();
        let yt: Vec<f64> = ys.info.layers.iter().map(|l| l.time).collect();
        if vx.len() != xt.len() || vy.len() != yt.len() {
            return Err(String::new());
        }
        let cur = if xl == self.active { self.display.t } else { self.layers[xl].disp.t };
        let mut t = Trajectory { name: String::new(), pts: Vec::new(), labels: Vec::new(), current: None };
        for (i, j) in pair_dates(&xt, &yt) {
            let (a, b) = (vx[i], vy[j]);
            if a.is_finite() && b.is_finite() {
                if i == cur {
                    t.current = Some(t.pts.len());
                }
                t.pts.push([f64::from(a), f64::from(b)]);
                t.labels.push(xs.info.layers[i].label.clone());
            }
        }
        if t.pts.len() < 2 {
            return Err("Fewer than two dates with both values.".into());
        }
        Ok(t)
    }
}

/// A trajectory on the scatter: a segment per step coloured by time (plasma, early dark to late bright), an arrow
/// about every fifteenth of the way, its first and last dates named, the current date ringed.
pub fn draw(plot: &mut PlotUi, tr: &Trajectory) {
    let n = tr.pts.len();
    let lut = janus_gpu::colormaps::all()[janus_gpu::colormaps::PLASMA].lut();
    let colour = |k: usize| {
        // plasma's darkest end is lost on a dark plot: from 15 %
        let c = lut[((0.15 + 0.85 * k as f32 / (n - 1).max(1) as f32) * 255.0) as usize];
        theme::on_plot(Color32::from_rgb(c[0], c[1], c[2]))
    };
    let outline = Color32::from_rgba_unmultiplied(13, 13, 20, 200);
    for k in 1..n {
        let seg = vec![tr.pts[k - 1], tr.pts[k]];
        // the outline unnamed: the legend in the trajectory's colour
        plot.line(Line::new("", PlotPoints::from(seg.clone())).color(outline).width(4.0).allow_hover(false));
        plot.line(Line::new(&tr.name, PlotPoints::from(seg)).color(colour(k)).width(2.0));
    }
    let every = n.div_ceil(15).max(1);
    for k in (1..n).filter(|k| k % every == 0) {
        let (a, b) = (tr.pts[k - 1], tr.pts[k]);
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        plot.arrows(
            Arrows::new(&tr.name, PlotPoints::from(vec![a]), PlotPoints::from(vec![mid]))
                .color(colour(k))
                .tip_length(9.0),
        );
    }
    for k in 0..n {
        plot.points(Points::new(&tr.name, PlotPoints::from(vec![tr.pts[k]])).color(colour(k)).radius(2.5));
    }
    let text = theme::on_plot(Color32::from_gray(235));
    for k in [0, n - 1] {
        let p = PlotPoint::new(tr.pts[k][0], tr.pts[k][1]);
        plot.text(
            Text::new("", p, egui::RichText::new(format!("  {}", tr.labels[k])).color(text))
                .anchor(egui::Align2::LEFT_BOTTOM),
        );
    }
    if let Some(c) = tr.current {
        plot.points(
            Points::new("", PlotPoints::from(vec![tr.pts[c]]))
                .color(theme::on_plot(Color32::from_rgb(255, 153, 51)))
                .radius(6.0)
                .filled(false)
                .allow_hover(false),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_paired() {
        // the same dates: every index
        assert_eq!(pair_dates(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]), vec![(0, 0), (1, 1), (2, 2)]);
        // monthly X (days 0, 30, 60, 90) and Y two days later, missing day 62: 60 has no partner within 15 days
        let x = [0.0, 30.0, 60.0, 90.0];
        let y = [2.0, 32.0, 92.0];
        assert_eq!(pair_dates(&x, &y), vec![(0, 0), (1, 1), (3, 2)]);
        // yearly Y against monthly X: each year once, at its nearest month
        let x: Vec<f64> = (0..24).map(|m| f64::from(m) * 30.0).collect();
        let y = [0.0, 360.0];
        assert_eq!(pair_dates(&x, &y), vec![(0, 0), (12, 1)]);
    }
}
