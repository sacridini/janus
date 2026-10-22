//! The season and the temporal composites' window (src/app_season.cpp). The season is a window of days of the year
//! (e.g. 150-270, or Nov-Feb across the new year): the dates outside it are left out of the temporal statistics
//! (their time is NaN in the cube's dates on the GPU), the composites, the playback and the arrows, and the chart
//! highlights it. A composite ("Temporal composite" mode) reduces the dates of a window around the shown date (its
//! year, some days before and after, or the whole series) to one map, in the map shader over the overview's dates.

use egui::{ComboBox, Slider, Ui};
use janus_cube::CubeInfo;
use janus_cube::dates::{civil_from_days, days_from_civil};

use crate::app::State;
use crate::display::{Display, REDUCERS};
use crate::theme;

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// The day of the year (1-366) and the year of a time in Unix seconds.
pub fn day_of_year(unix: f64) -> (i32, i32) {
    let day = (unix / 86400.0).floor() as i64;
    let (y, _, _) = civil_from_days(day);
    ((day - days_from_civil(y, 1, 1)) as i32 + 1, y)
}

/// "Jun 1" for a day of the year (of a common year).
pub fn month_day(doy: i32) -> String {
    if doy == 366 {
        return "Dec 31+".into();
    }
    let (_, m, d) = civil_from_days(days_from_civil(2001, 1, 1) + i64::from(doy.clamp(1, 366)) - 1);
    format!("{} {d}", MONTHS[m as usize - 1])
}

/// The first day of a month of a common year.
pub fn month_start(m: u32) -> i32 {
    (days_from_civil(2001, m, 1) - days_from_civil(2001, 1, 1)) as i32 + 1
}

pub fn month_end(m: u32) -> i32 {
    if m == 12 { 365 } else { month_start(m + 1) - 1 }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Season {
    pub on: bool,
    /// days of the year (1-366), inclusive; start > end wraps over the new year
    pub start: i32,
    pub end: i32,
}

impl Default for Season {
    fn default() -> Self {
        Season { on: false, start: 152, end: 243 }
    }
}

impl Season {
    pub fn active(&self, info: &CubeInfo) -> bool {
        self.on && info.time_is_date && !info.spectral
    }

    pub fn key(&self) -> u64 {
        if self.on { self.start as u64 * 1000 + self.end as u64 + 1 } else { 0 }
    }

    /// Whether date t is in the season (every date without one).
    pub fn contains(&self, info: &CubeInfo, t: usize) -> bool {
        if !self.active(info) || t >= info.t() {
            return true;
        }
        let (d, _) = day_of_year(info.layers[t].time);
        if self.start <= self.end { d >= self.start && d <= self.end } else { d >= self.start || d <= self.end }
    }

    /// The year a date's season ends in: a season across the new year (e.g. Nov-Feb) belongs to the year it ends in.
    pub fn year(&self, info: &CubeInfo, t: usize) -> i32 {
        let (d, y) = day_of_year(info.layers[t].time);
        if self.active(info) && self.start > self.end && d >= self.start { y + 1 } else { y }
    }

    pub fn text(&self) -> String {
        format!("{} - {}", month_day(self.start), month_day(self.end))
    }

    /// The dates' times on the GPU: NaN outside the season (left out of the statistics and the composites).
    pub fn times(&self, info: &CubeInfo, times_years: &[f32]) -> Vec<f32> {
        (0..times_years.len()).map(|t| if self.contains(info, t) { times_years[t] } else { f32::NAN }).collect()
    }
}

impl Display {
    /// The composite's dates around date t: its (season) year, `days` before and after it, or the whole series.
    pub fn temporal_window(&self, info: &CubeInfo, t: usize) -> (usize, usize) {
        let n = info.t();
        let t = t.min(n - 1);
        if !info.time_is_date || info.spectral || self.window == 2 {
            return (0, n - 1);
        }
        let (mut t0, mut t1) = (t, t);
        if self.window == 0 {
            // the date's year (dates are sorted: its dates are contiguous)
            let y = self.season.year(info, t);
            while t0 > 0 && self.season.year(info, t0 - 1) == y {
                t0 -= 1;
            }
            while t1 < n - 1 && self.season.year(info, t1 + 1) == y {
                t1 += 1;
            }
        } else {
            let (half, c) = (f64::from(self.days) * 86400.0, info.layers[t].time);
            while t0 > 0 && info.layers[t0 - 1].time >= c - half {
                t0 -= 1;
            }
            while t1 < n - 1 && info.layers[t1 + 1].time <= c + half {
                t1 += 1;
            }
        }
        (t0, t1)
    }

    /// E.g. "Median of 9 dates, 2021 (Jun 1 - Aug 31)".
    pub fn temporal_text(&self, info: &CubeInfo, t: usize) -> String {
        let (t0, t1) = self.temporal_window(info, t);
        let n = (t0..=t1).filter(|&k| self.season.contains(info, k)).count();
        let window = if !info.time_is_date || info.spectral || self.window == 2 {
            "all dates".to_string()
        } else if self.window == 0 {
            let y = self.season.year(info, t);
            if self.season.active(info) && self.season.start > self.season.end {
                format!("{}/{:02}", y - 1, y % 100)
            } else {
                y.to_string()
            }
        } else {
            format!("+-{} days of {}", self.days, info.layers[t.min(info.t() - 1)].label)
        };
        let mut s =
            format!("{} of {n} date{}, {window}", REDUCERS[self.reducer as usize], if n == 1 { "" } else { "s" });
        if self.season.active(info) {
            s += &format!(" ({})", self.season.text());
        }
        s
    }

    /// The next (dir 1) or previous (-1) date in the season, wrapping around; a composite of the date's year goes
    /// year by year. Unwrapped (t + dir) without a season.
    pub fn step_date(&self, info: &CubeInfo, t: usize, dir: i64) -> i64 {
        let n = info.t() as i64;
        let (mut t, mut dir) = (t as i64, dir);
        if self.mode == janus_gpu::Mode::Composite && self.window == 0 && info.time_is_date && !info.spectral {
            let (t0, t1) = self.temporal_window(info, t as usize);
            t = if dir > 0 { (t1 as i64 + 1) % n } else { (t0 as i64 - 1 + n) % n };
            if dir < 0 {
                // the first date of that year
                t = self.temporal_window(info, t as usize).0 as i64;
            }
            dir = dir.signum();
            if self.season.contains(info, t as usize) {
                return t;
            }
        }
        if !self.season.active(info) {
            return t + dir;
        }
        for k in 1..=n {
            let c = ((t + dir * k) % n + n) % n;
            if self.season.contains(info, c as usize) {
                return c;
            }
        }
        t + dir // no date in the season
    }
}

impl State {
    /// The date before or after the shown one (the arrows, playback): in the season, wrapping around the series.
    pub fn step_date(&mut self, dir: i64) {
        let Some(s) = &self.session else { return };
        let n = s.info.t() as i64;
        let t = self.display.step_date(&s.info, self.display.t, dir);
        self.set_date(((t % n + n) % n) as usize);
    }

    /// The season's dates on the GPU: the others' times NaN, the statistics computed again with them.
    pub(crate) fn pump_season(&mut self) {
        let season = self.display.season;
        let sessions = self.session.iter_mut().chain(self.layers.iter_mut().filter_map(|l| l.session.as_mut()));
        for s in sessions {
            let active = season.active(&s.info);
            let key = (if active { season.key() } else { 0 }).wrapping_mul(1000003).wrapping_add(s.info.id);
            if s.season_applied == key {
                continue;
            }
            let first = s.season_applied == 0 && !active;
            s.season_applied = key;
            if first {
                continue; // a new series without a season: its dates are already on the GPU
            }
            let times = season.times(&s.info, &s.s.times_years);
            s.recompute_stats(&times);
            self.map.dirty = true;
        }
    }
}

/// The Display panel's options of the temporal composite. Returns whether they changed.
pub fn temporal_options_ui(ui: &mut Ui, d: &mut Display, info: &CubeInfo) -> bool {
    {
        let mut changed = false;
        ComboBox::from_id_salt("reducer")
            .selected_text(REDUCERS[d.reducer as usize])
            .width(ui.available_width())
            .show_ui(ui, |ui| {
                for (i, r) in REDUCERS.iter().enumerate() {
                    changed |= ui.selectable_value(&mut d.reducer, i as i32, *r).changed();
                }
            })
            .response
            .on_hover_text(
                "Of each pixel's valid observations in the window (and the season):\nthe median (of up to 64 of \
                 them: every k-th one beyond), the mean,\nthe maximum (e.g. greenest NDVI), the minimum, or how many \
                 there are",
            );
        if info.time_is_date && !info.spectral {
            const WINDOWS: [&str; 3] = ["The date's year", "Days around the date", "The whole series"];
            ComboBox::from_id_salt("window")
                .selected_text(WINDOWS[d.window as usize])
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (i, w) in WINDOWS.iter().enumerate() {
                        changed |= ui.selectable_value(&mut d.window, i as i32, *w).changed();
                    }
                })
                .response
                .on_hover_text(
                    "Which dates go into the composite: those of the shown date's year (the\nslider and playback \
                     then go year by year), those up to some days\nbefore and after it (a moving window), or all of \
                     them",
                );
            if d.window == 1 {
                changed |=
                    ui.add(Slider::new(&mut d.days, 1..=365).logarithmic(true).prefix("+- ").suffix(" days")).changed();
            }
        }
        ui.weak(d.temporal_text(info, d.t));
        ui.weak("Computed on the GPU from the overview.");
        changed
    }
}

/// The Display panel's season section, for dated series. Returns whether it changed (the date moved into it).
pub fn season_ui(ui: &mut Ui, d: &mut Display, info: &CubeInfo) -> bool {
    {
        if !info.time_is_date || info.spectral || info.t() < 2 {
            return false;
        }
        ui.separator();
        ui.strong("Season");
        let season = &mut d.season;
        let mut changed = ui
            .checkbox(&mut season.on, "Only part of the year")
            .on_hover_text(
                "Days of the year (e.g. a crop's summer, the dry months): the dates outside\nare left out of the \
                 temporal statistics (mean, trend...), the composites,\nplayback and the arrow keys, and the chart \
                 highlights the season.",
            )
            .changed();
        if season.on {
            const PRESETS: [(&str, u32, u32); 8] = [
                ("Dec - Feb (DJF)", 12, 2),
                ("Mar - May (MAM)", 3, 5),
                ("Jun - Aug (JJA)", 6, 8),
                ("Sep - Nov (SON)", 9, 11),
                ("Apr - Sep", 4, 9),
                ("Oct - Mar", 10, 3),
                ("May - Oct", 5, 10),
                ("Nov - Apr", 11, 4),
            ];
            let current = PRESETS
                .iter()
                .find(|p| season.start == month_start(p.1) && season.end == month_end(p.2))
                .map_or("Months...", |p| p.0);
            ComboBox::from_id_salt("season preset").selected_text(current).width(ui.available_width()).show_ui(
                ui,
                |ui| {
                    for p in PRESETS {
                        if ui.selectable_label(current == p.0, p.0).clicked() {
                            season.start = month_start(p.1);
                            season.end = month_end(p.2);
                            changed = true;
                        }
                    }
                },
            );
            let (from, to) = (month_day(season.start), month_day(season.end));
            changed |= ui
                .add(
                    Slider::new(&mut season.start, 1..=366)
                        .custom_formatter(move |v, _| format!("from {from} (day {v})")),
                )
                .changed();
            changed |= ui
                .add(Slider::new(&mut season.end, 1..=366).custom_formatter(move |v, _| format!("to {to} (day {v})")))
                .on_hover_text(
                    "A start after the end wraps over the new year (e.g. Nov 1 - Feb 28);\nsuch a season belongs to \
                     the year it ends in",
                )
                .changed();
            let n = (0..info.t()).filter(|&t| season.contains(info, t)).count();
            if n == 0 {
                ui.colored_label(theme::warning(), "No date of the series falls in the season.");
            } else {
                ui.weak(format!("{n} of {} dates in the season", info.t()));
            }
        }
        if changed {
            if !d.season.contains(info, d.t) {
                let n = info.t() as i64;
                let t = d.step_date(info, d.t, 1);
                d.t = ((t % n + n) % n) as usize;
            }
            for r in &mut d.ranges {
                r.invalidate();
            }
        }
        changed
    }
}

/// The chart's background with the season over it: a band over each year's part of it (the C++ draws them inside
/// the plot, under the series). Painted in a slot reserved before the plot, which then shows no background of its
/// own, so the bands neither enter its bounds nor its legend.
pub fn chart_background(ui: &Ui, tr: &egui_plot::PlotTransform, d: &Display, info: &CubeInfo) -> egui::Shape {
    let frame = *tr.frame();
    let v = ui.visuals();
    let mut shapes = vec![egui::Shape::Rect(egui::epaint::RectShape::new(
        frame,
        2,
        theme::plot_fill(),
        v.widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    ))];
    let season = d.season;
    let b = tr.bounds();
    let (_, y0) = day_of_year(b.min()[0]);
    let (_, y1) = day_of_year(b.max()[0]);
    // zoomed out over centuries: nothing readable
    if season.active(info) && y1 - y0 <= 400 {
        let accent = theme::accent();
        let fill = egui::Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 26);
        for y in y0 - 1..=y1 {
            let jan1 = days_from_civil(y, 1, 1) as f64 * 86400.0;
            let a = jan1 + f64::from(season.start - 1) * 86400.0;
            let mut e = jan1 + f64::from(season.end) * 86400.0;
            if season.start > season.end {
                e += (days_from_civil(y + 1, 1, 1) - days_from_civil(y, 1, 1)) as f64 * 86400.0;
            }
            let pa = tr.position_from_point(&egui_plot::PlotPoint::new(a, b.max()[1]));
            let pe = tr.position_from_point(&egui_plot::PlotPoint::new(e, b.min()[1]));
            let r = egui::Rect::from_min_max(pa, pe).intersect(frame);
            if r.is_positive() {
                shapes.push(egui::Shape::rect_filled(r, 0.0, fill));
            }
        }
    }
    egui::Shape::Vec(shapes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_cube::Layer;

    /// Monthly dates from Jan 2019 to Dec 2021, on the 15th.
    fn monthly() -> CubeInfo {
        let layers = (0..36)
            .map(|i| {
                let (y, m) = (2019 + i / 12, (i % 12) as u32 + 1);
                Layer {
                    time: days_from_civil(y, m, 15) as f64 * 86400.0,
                    label: format!("{y}-{m:02}"),
                    ..Default::default()
                }
            })
            .collect();
        CubeInfo { layers, time_is_date: true, ..Default::default() }
    }

    #[test]
    fn days_and_months() {
        assert_eq!(day_of_year(0.0), (1, 1970));
        assert_eq!(day_of_year(days_from_civil(2020, 12, 31) as f64 * 86400.0 + 3600.0), (366, 2020));
        assert_eq!(month_day(152), "Jun 1");
        assert_eq!(month_day(366), "Dec 31+");
        assert_eq!((month_start(6), month_end(8), month_end(12)), (152, 243, 365));
    }

    #[test]
    fn seasons_and_windows() {
        let info = monthly();
        let mut d =
            Display { season: Season { on: true, start: month_start(6), end: month_end(8) }, ..Default::default() };
        let inside: Vec<usize> = (0..36).filter(|&t| d.season.contains(&info, t)).collect();
        assert_eq!(inside, vec![5, 6, 7, 17, 18, 19, 29, 30, 31]);
        // the arrows: Aug 2019 -> Jun 2020, back to Aug 2019; Aug 2021 wraps to Jun 2019
        assert_eq!(d.step_date(&info, 7, 1), 17);
        assert_eq!(d.step_date(&info, 17, -1), 7);
        assert_eq!(d.step_date(&info, 31, 1), 5);
        // a season across the new year belongs to the year it ends in
        d.season = Season { on: true, start: month_start(11), end: month_end(2) };
        assert_eq!(d.season.year(&info, 10), 2020); // Nov 2019
        assert_eq!(d.season.year(&info, 13), 2020); // Feb 2020
        assert_eq!(d.temporal_window(&info, 13), (10, 21)); // Nov 2019 .. Oct 2020
        d.mode = janus_gpu::Mode::Composite;
        assert!(d.temporal_text(&info, 13).starts_with("Median of 4 dates, 2019/20"), "{}", d.temporal_text(&info, 13));
        // the composite of a year goes year by year
        assert_eq!(d.step_date(&info, 13, 1), 22); // Nov 2020
        // days around the date, the whole series
        d.season.on = false;
        d.window = 1;
        d.days = 40;
        assert_eq!(d.temporal_window(&info, 13), (12, 14));
        d.window = 2;
        assert_eq!(d.temporal_window(&info, 13), (0, 35));
        assert_eq!(d.step_date(&info, 35, 1), 36); // unwrapped without a season
    }
}
