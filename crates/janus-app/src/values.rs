//! Pixel values: what the map shows under the cursor and at the pins, layer by layer, in the place of the chart
//! whenever the active layer is not a series that is shown (hidden, one date, a stack of bands): the maps the runs
//! made (LandTrendr's year of detection, its magnitude...), single images, the other series at their dates. A row
//! clicked (here or in Layers) is described: its histogram beside the table, its statistics and its pixels by value
//! (by class, by year) in Statistics.

use std::collections::BTreeMap;

use egui::{Color32, Grid, RichText, Sense, Ui, vec2};
use egui_plot::{Bar, BarChart, Plot, VLine};
use janus_cube::CubeInfo;
use janus_cube::multidim::format_g;

use crate::app::State;
use crate::contents::Sel;
use crate::order::Item;
use crate::theme;

/// A row of the table: a map the map shows.
#[derive(Clone, Copy, PartialEq)]
enum Src {
    /// a run's map (an index into `zeit.results`)
    Result(usize),
    /// a raster layer (into `layers`) at its date
    Layer(usize),
}

struct ValueRow {
    sel: Sel,
    src: Src,
    name: String,
    /// its date or band (a series), what it was computed on (a run's map)
    detail: String,
}

/// Where a value is read: the cursor, or a pin (its number).
#[derive(Clone, Copy)]
enum At {
    Cursor,
    Pin(u32),
}

/// The panel's state: the row described and its summary.
#[derive(Default)]
pub struct ValuesUi {
    /// the layer described (a click on its row, here or in Layers); None: the top one shown
    pub focus: Option<Sel>,
    /// the Layers panel's selection when it was last one row (a new one becomes the focus)
    last_selected: Option<Sel>,
    summary: Option<(SummaryKey, Summary)>,
}

/// What a summary was computed of: the layer, its data (address, length), its date, the overview's dates read.
#[derive(Clone, PartialEq)]
struct SummaryKey {
    sel: Sel,
    data: (usize, usize),
    t: usize,
    done: usize,
}

/// A map's values summarised: from its copy in memory (a result) or its overview (a layer).
pub(crate) struct Summary {
    /// pixels read with a value, pixels read
    valid: usize,
    total: usize,
    min: f64,
    max: f64,
    mean: f64,
    std: f64,
    /// 2 %, 25 %, 50 %, 75 %, 98 %
    q: [f64; 5],
    /// a map of whole numbers (classes, years), at most 256 of them: each value's pixels read
    discrete: Vec<(f64, usize)>,
    /// the histogram: bins' centres and pixels read, their width
    bins: Vec<(f64, usize)>,
    bin_w: f64,
    /// the layer's pixels per pixel read (a reduced copy, an overview), and hectares per pixel (a projected CRS)
    px_scale: f64,
    ha_per_px: Option<f64>,
    /// where the values come from, when not every pixel
    note: String,
}

/// The value at rank `p` (0..1) of `s` (reordered).
fn quantile(s: &mut [f32], p: f64) -> f64 {
    if s.is_empty() {
        return f64::NAN;
    }
    let k = ((s.len() - 1) as f64 * p).round() as usize;
    f64::from(*s.select_nth_unstable_by(k, f32::total_cmp).1)
}

/// The summary of `data` (NaN: no value).
fn summarize(data: &[f32], px_scale: f64, ha_per_px: Option<f64>, note: String) -> Summary {
    let (mut n, mut sum, mut sum2) = (0usize, 0f64, 0f64);
    let (mut min, mut max) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut counts: BTreeMap<i64, usize> = BTreeMap::new();
    let mut whole = true;
    for &v in data.iter().filter(|v| v.is_finite()) {
        let x = f64::from(v);
        n += 1;
        sum += x;
        sum2 += x * x;
        min = min.min(x);
        max = max.max(x);
        if whole {
            if x.fract() != 0.0 {
                whole = false;
            } else {
                *counts.entry(x as i64).or_default() += 1;
                whole = counts.len() <= 256;
            }
        }
    }
    let mean = if n > 0 { sum / n as f64 } else { f64::NAN };
    let std = if n > 1 { ((sum2 - sum * mean) / (n - 1) as f64).max(0.0).sqrt() } else { f64::NAN };
    // the quantiles from a sample of at most a million values
    let step = (n / 1_000_000).max(1);
    let mut sample: Vec<f32> = data.iter().copied().filter(|v| v.is_finite()).step_by(step).collect();
    let q = [0.02, 0.25, 0.5, 0.75, 0.98].map(|p| quantile(&mut sample, p));
    let discrete: Vec<(f64, usize)> =
        if whole { counts.into_iter().map(|(v, c)| (v as f64, c)).collect() } else { Vec::new() };
    let (bins, bin_w) = if !discrete.is_empty() {
        (discrete.clone(), 1.0)
    } else if n > 0 {
        // 64 bins over the values but their extreme half percents (counted in the end bins)
        let (lo, hi) = (quantile(&mut sample, 0.005), quantile(&mut sample, 0.995));
        let (lo, hi) = if hi > lo { (lo, hi) } else { (min, max.max(min + 1e-9)) };
        const K: usize = 64;
        let w = (hi - lo) / K as f64;
        let mut c = vec![0usize; K];
        for &v in data.iter().filter(|v| v.is_finite()) {
            c[(((f64::from(v) - lo) / w).floor().max(0.0) as usize).min(K - 1)] += 1;
        }
        (c.into_iter().enumerate().map(|(i, c)| (lo + (i as f64 + 0.5) * w, c)).collect(), w)
    } else {
        (Vec::new(), 1.0)
    };
    Summary { valid: n, total: data.len(), min, max, mean, std, q, discrete, bins, bin_w, px_scale, ha_per_px, note }
}

/// A pixel's area in hectares, on a grid of a projected CRS.
fn hectares_per_pixel(info: &CubeInfo) -> Option<f64> {
    let g = info.geo_transform?;
    let srs = gdal::spatial_ref::SpatialRef::from_wkt(&info.crs_wkt).ok()?;
    if srs.is_geographic() {
        return None;
    }
    let m = srs.linear_units();
    Some((g[1] * g[5] - g[2] * g[4]).abs() * m * m / 10_000.0)
}

/// A count of pixels read as pixels of the layer, "≈" when scaled up from a reduced copy.
fn pixels_text(c: usize, scale: f64) -> String {
    let n = (c as f64 * scale).round() as u64;
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    if scale > 1.0 { format!("≈ {out}") } else { out }
}

impl State {
    /// The chart is shown (else Pixel values): the active layer is a series shown, of several dates, or a layer of
    /// embeddings.
    pub(crate) fn chart_shown(&self) -> bool {
        let (Some(s), Some(l)) = (&self.session, self.layers.get(self.active)) else { return false };
        !l.canvas && l.visible && (l.emb.is_some() || (s.info.t() > 1 && !s.info.stack))
    }

    /// The rows: every map shown, top of the drawing order first.
    fn value_rows(&self) -> Vec<ValueRow> {
        let mut rows = Vec::new();
        for it in self.draw_order().into_iter().rev() {
            match it {
                Item::Result(k) => {
                    let r = &self.zeit.results[k];
                    if r.visible && r.place.aligned {
                        let detail = String::new();
                        rows.push(ValueRow {
                            sel: Sel::Result(r.id),
                            src: Src::Result(k),
                            name: r.name.clone(),
                            detail,
                        });
                    }
                }
                Item::Series(i) => {
                    let l = &self.layers[i];
                    let Some(s) = self.layer_session(i) else { continue };
                    if l.canvas || !l.visible || !l.place.aligned || l.emb.is_some() {
                        continue;
                    }
                    let t = if i == self.active { self.display.t } else { l.disp.t };
                    let detail = if s.info.t() > 1 {
                        s.info.layers.get(t).map_or(String::new(), |x| x.label.clone())
                    } else {
                        String::new()
                    };
                    rows.push(ValueRow {
                        sel: Sel::Series(s.info.id),
                        src: Src::Layer(i),
                        name: l.name.clone(),
                        detail,
                    });
                }
                Item::Vector(_) => {}
            }
        }
        rows
    }

    /// The active layer's pixel of the cursor or of a pin.
    fn active_pixel(&self, at: At) -> Option<(usize, usize)> {
        match at {
            At::Cursor => self.series.hover.as_ref().map(|h| (h.x, h.y)),
            At::Pin(id) => self.series.pins.iter().find(|p| p.id == id).map(|p| (p.x, p.y)),
        }
    }

    /// A row's value there, and whether it is exact (a series' value is approximated from its overview until read).
    fn row_value(&self, row: &ValueRow, at: At) -> (f32, bool) {
        match row.src {
            Src::Result(k) => {
                let r = &self.zeit.results[k];
                let v = self
                    .active_pixel(at)
                    .zip(r.grid.as_ref())
                    .and_then(|((x, y), g)| r.place.pixel(g, x as f64 + 0.5, y as f64 + 0.5))
                    .map_or(f32::NAN, |(x, y)| r.value_at(x, y));
                (v, true)
            }
            Src::Layer(i) => {
                let (series, t) = if i == self.active {
                    let sr = &self.series;
                    let v = match at {
                        At::Cursor => sr.hover.as_ref(),
                        At::Pin(id) => sr.pins.iter().find(|p| p.id == id),
                    };
                    (v, self.display.t)
                } else {
                    let l = &self.layers[i];
                    let v = match at {
                        At::Cursor => l.series.hover.as_ref(),
                        At::Pin(id) => l.series.pins.iter().find(|p| p.id == id),
                    };
                    (v, l.disp.t)
                };
                series.map_or((f32::NAN, true), |v| (v.values.get(t).copied().unwrap_or(f32::NAN), v.exact))
            }
        }
    }

    /// A row's value as text: its class's name, a year, a number.
    fn value_text(&self, src: Src, v: f64) -> String {
        if !v.is_finite() {
            return "-".into();
        }
        match src {
            Src::Result(k) => {
                let r = &self.zeit.results[k];
                if r.is_segments() {
                    format!("segment {v:.0}")
                } else if !r.classes.is_empty() {
                    let k = v.round() as usize;
                    k.checked_sub(1).and_then(|i| r.classes.get(i)).map_or(format!("{k}"), |c| format!("{c} ({k})"))
                } else if r.unit == "year" || r.unit == "years" {
                    format!("{v:.0}")
                } else {
                    format_g(v, 6)
                }
            }
            Src::Layer(i) => {
                let c = if i == self.active { &self.classes } else { &self.layers[i].classes };
                if c.on() { c.name_of(v as f32) } else { format_g(v, 6) }
            }
        }
    }

    /// The colour a row's value is drawn in on the map (None: not one colour of a colormap, e.g. RGB).
    fn value_colour(&self, src: Src, v: f64) -> Option<Color32> {
        let maps = janus_gpu::colormaps::all();
        let sample = |cmap: usize, lo: f64, hi: f64| {
            let f = if hi > lo { ((v - lo) / (hi - lo)).clamp(0.0, 1.0) } else { 0.5 };
            let [r, g, b, _] = maps[cmap].sample(f as f32);
            Color32::from_rgb(r, g, b)
        };
        match src {
            Src::Result(k) => {
                let r = &self.zeit.results[k];
                (!r.is_segments()).then(|| sample(r.cmap, f64::from(r.lo), f64::from(r.hi)))
            }
            Src::Layer(i) => {
                let (d, c) = if i == self.active {
                    (&self.display, &self.classes)
                } else {
                    (&self.layers[i].disp, &self.layers[i].classes)
                };
                if c.on() {
                    return c.find(v.round() as i32).map(|e| Color32::from_rgb(e.color[0], e.color[1], e.color[2]));
                }
                if d.mode == janus_gpu::Mode::Rgb {
                    return None;
                }
                let m = janus_gpu::Mode::Value as usize;
                let r = d.ranges[m];
                (r.lo.is_finite() && r.hi.is_finite()).then(|| sample(d.cmap[m], f64::from(r.lo), f64::from(r.hi)))
            }
        }
    }

    /// The row described: the focus if shown, else the Layers panel's one selected row if shown, else the top one.
    fn focused_row<'a>(&mut self, rows: &'a [ValueRow]) -> Option<&'a ValueRow> {
        let sel = &self.contents.selected;
        let single = (sel.len() == 1).then(|| *sel.iter().next().unwrap());
        if single != self.values.last_selected {
            self.values.last_selected = single;
            if single.is_some() {
                self.values.focus = single;
            }
        }
        rows.iter().find(|r| Some(r.sel) == self.values.focus).or(rows.first())
    }

    /// The summary of a row's values (computed again when its data changes).
    fn update_summary(&mut self, row: &ValueRow) {
        type Compute<'a> = Box<dyn FnOnce() -> Summary + 'a>;
        let (key, compute): (SummaryKey, Compute) = match row.src {
            Src::Result(k) => {
                let r = &self.zeit.results[k];
                let key = SummaryKey { sel: row.sel, data: (r.data.as_ptr() as usize, r.data.len()), t: 0, done: 0 };
                let scale = (r.w * r.h) as f64 / (r.tw * r.th).max(1) as f64;
                let note = if scale > 1.0 { format!("a copy of 1:{:.1}", scale.sqrt()) } else { String::new() };
                (key, Box::new(move || summarize(&r.data, scale, r.grid.as_deref().and_then(hectares_per_pixel), note)))
            }
            Src::Layer(i) => {
                let Some(s) = self.layer_session(i) else { return };
                let t = if i == self.active { self.display.t } else { self.layers[i].disp.t };
                let ov = &s.s.overview;
                let data = ov.layer(t).unwrap_or(&[]);
                let key =
                    SummaryKey { sel: row.sel, data: (data.as_ptr() as usize, data.len()), t, done: ov.layers_done() };
                let scale = (s.info.width * s.info.height) as f64 / (ov.w * ov.h).max(1) as f64;
                let note = if scale > 1.0 { format!("its overview, 1:{:.1}", ov.factor) } else { String::new() };
                let info = &s.info;
                (key, Box::new(move || summarize(data, scale, hectares_per_pixel(info), note)))
            }
        };
        if self.values.summary.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let sm = compute();
        self.values.summary = Some((key, sm));
    }

    /// The Pixel values panel (in the chart's place).
    pub(crate) fn values_ui(&mut self, ui: &mut Ui) {
        let rows = self.value_rows();
        let pins: Vec<(u32, Color32)> = self.series.pins.iter().map(|p| (p.id, p.color)).collect();
        let (mut clear_pins, mut copy) = (false, false);
        ui.horizontal_wrapped(|ui| {
            let hidden = self.layers.get(self.active).is_some_and(|l| !l.visible && !l.canvas);
            let why = if hidden { "The active series is hidden: the values of what the map shows. " } else { "" };
            ui.weak(format!(
                "{why}Hover over the map; a click drops a pin. Click a row: its histogram and statistics."
            ));
            if !pins.is_empty() && ui.small_button("Clear pins").clicked() {
                clear_pins = true;
            }
            if !rows.is_empty() && ui.small_button("Copy").on_hover_text("The table as text (tab-separated)").clicked()
            {
                copy = true;
            }
        });
        if rows.is_empty() {
            ui.add_space(8.0);
            ui.weak("Nothing shown on the map: turn a layer on in Layers (its eye).");
            return;
        }
        let focus = self.focused_row(&rows).map(|r| r.sel);
        if copy {
            let mut text = String::from("Layer\tCursor");
            for (id, _) in &pins {
                text += &format!("\tPin {id}");
            }
            for r in &rows {
                text += &format!("\n{}", r.name);
                for at in std::iter::once(At::Cursor).chain(pins.iter().map(|p| At::Pin(p.0))) {
                    text += &format!("\t{}", self.value_text(r.src, f64::from(self.row_value(r, at).0)));
                }
            }
            ui.ctx().copy_text(text);
        }
        let wide = ui.available_width() >= 620.0;
        let table = |s: &mut State, ui: &mut Ui| {
            egui::ScrollArea::both().id_salt("values table").auto_shrink([false, true]).show(ui, |ui| {
                s.values_table(ui, &rows, &pins, focus);
            });
        };
        if wide {
            // the table wider than the histogram (a column per pin)
            let (w, h) = (ui.available_width(), ui.available_height());
            let down = egui::Layout::top_down(egui::Align::Min);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(vec2(w * 0.6, h), down, |ui| table(self, ui));
                ui.allocate_ui_with_layout(vec2(ui.available_width(), h), down, |ui| {
                    self.focus_histogram(ui, &rows, &pins)
                });
            });
        } else {
            egui::ScrollArea::vertical().id_salt("values narrow").auto_shrink(false).show(ui, |ui| {
                table(self, ui);
                ui.add_space(6.0);
                self.focus_histogram(ui, &rows, &pins);
            });
        }
        if clear_pins {
            self.clear_pins();
        }
    }

    fn values_table(&mut self, ui: &mut Ui, rows: &[ValueRow], pins: &[(u32, Color32)], focus: Option<Sel>) {
        let row_h = ui.text_style_height(&egui::TextStyle::Body);
        let gap = vec2(ui.spacing().item_spacing.x * 2.0, 3.0);
        Grid::new("values").striped(true).num_columns(2 + pins.len()).min_row_height(row_h).spacing(gap).show(
            ui,
            |ui| {
                ui.strong("Layer");
                ui.colored_label(theme::on_window(theme::cursor_series()), RichText::new("Cursor").strong());
                for (id, c) in pins {
                    ui.colored_label(theme::on_window(*c), RichText::new(format!("Pin {id}")).strong());
                }
                ui.end_row();
                for r in rows {
                    let values: Vec<(f32, bool)> = std::iter::once(At::Cursor)
                        .chain(pins.iter().map(|p| At::Pin(p.0)))
                        .map(|at| self.row_value(r, at))
                        .collect();
                    ui.horizontal(|ui| {
                        // the cursor's value in the colour the map gives it
                        let c = self.value_colour(r.src, f64::from(values[0].0)).filter(|_| values[0].0.is_finite());
                        let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                        match c {
                            Some(c) => ui.painter().rect_filled(rect, 2.0, c),
                            None => ui.painter().rect_stroke(
                                rect,
                                2.0,
                                egui::Stroke::new(1.0, ui.visuals().weak_text_color()),
                                egui::StrokeKind::Inside,
                            ),
                        };
                        let mut text = RichText::new(&r.name);
                        if !r.detail.is_empty() {
                            text = RichText::new(format!("{} [{}]", r.name, r.detail));
                        }
                        if ui
                            .selectable_label(focus == Some(r.sel), text)
                            .on_hover_text("Click: its histogram here, its statistics in Statistics")
                            .clicked()
                        {
                            self.values.focus = Some(r.sel);
                        }
                    });
                    for &(v, exact) in &values {
                        let t = self.value_text(r.src, f64::from(v));
                        if exact {
                            ui.label(t);
                        } else {
                            ui.label(format!("{t}*")).on_hover_text("From the overview until the pixel is read");
                        }
                    }
                    ui.end_row();
                }
            },
        );
    }

    /// The histogram of the row described, in the colours the map draws its values, the cursor's and the pins'
    /// values marked on it.
    fn focus_histogram(&mut self, ui: &mut Ui, rows: &[ValueRow], pins: &[(u32, Color32)]) {
        let Some(row) = rows.iter().find(|r| Some(r.sel) == self.values.focus).or(rows.first()) else { return };
        self.update_summary(row);
        let Some((_, sm)) = &self.values.summary else { return };
        ui.horizontal_wrapped(|ui| {
            ui.strong(&row.name);
            if !sm.note.is_empty() {
                ui.weak(format!("(from {})", sm.note));
            }
        });
        if sm.valid == 0 {
            ui.weak("No pixel with a value (yet).");
            return;
        }
        let bars: Vec<Bar> = sm
            .bins
            .iter()
            .map(|&(x, c)| {
                let fill = self.value_colour(row.src, x).unwrap_or(theme::accent());
                Bar::new(x, c as f64 * sm.px_scale)
                    .width(sm.bin_w * if sm.discrete.is_empty() { 1.0 } else { 0.85 })
                    .fill(fill)
                    .stroke(egui::Stroke::new(0.5, fill.gamma_multiply(0.6)))
                    .name(self.value_text(row.src, x))
            })
            .collect();
        let marks: Vec<(f64, Color32)> = std::iter::once((At::Cursor, theme::cursor_series()))
            .chain(pins.iter().map(|&(id, c)| (At::Pin(id), c)))
            .map(|(at, c)| (f64::from(self.row_value(row, at).0), c))
            .filter(|(v, _)| v.is_finite())
            .collect();
        let src = row.src;
        let labels: BTreeMap<i64, String> = if sm.discrete.is_empty() {
            BTreeMap::new()
        } else {
            sm.discrete.iter().map(|&(v, _)| (v as i64, self.value_text(src, v))).collect()
        };
        let height = ui.available_height().clamp(120.0, 360.0);
        Plot::new(("values histogram", row.sel_key()))
            .height(height)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .show_x(false)
            .show_y(false)
            .y_axis_label("pixels")
            .x_axis_formatter(move |m, _| {
                if labels.is_empty() {
                    format_g(m.value, 4)
                } else if m.value.fract() == 0.0 {
                    labels.get(&(m.value as i64)).cloned().unwrap_or_default()
                } else {
                    String::new()
                }
            })
            .show(ui, |p| {
                p.bar_chart(
                    BarChart::new("pixels", bars)
                        .element_formatter(Box::new(|b, _| format!("{}\n{} pixels", b.name, format_g(b.value, 6)))),
                );
                for (v, c) in marks {
                    p.vline(VLine::new("", v).color(theme::on_plot(c)).width(2.0));
                }
            });
    }

    /// Statistics while Pixel values is shown: the described map's values over the whole map, and its pixels by
    /// value (class, year) when they are whole numbers.
    pub(crate) fn values_statistics_ui(&mut self, ui: &mut Ui) {
        let rows = self.value_rows();
        let Some(row) = self.focused_row(&rows) else {
            ui.weak("Nothing shown on the map.");
            return;
        };
        let (src, name) = (row.src, row.name.clone());
        self.update_summary(row);
        let Some((_, sm)) = &self.values.summary else { return };
        ui.strong(&name);
        if !sm.note.is_empty() {
            ui.weak(format!("From {}: counts scaled to the layer's pixels.", sm.note));
        }
        let num = |v: f64| if v.is_finite() { self.value_text(src, v) } else { "-".into() };
        let plain = |v: f64| if v.is_finite() { format_g(v, 5) } else { "-".into() };
        let row_h = ui.text_style_height(&egui::TextStyle::Body);
        let gap = vec2(ui.spacing().item_spacing.x * 2.0, 3.0);
        Grid::new("values stats").striped(true).num_columns(2).min_row_height(row_h).spacing(gap).show(ui, |ui| {
            let pct = if sm.total > 0 { 100.0 * sm.valid as f64 / sm.total as f64 } else { 0.0 };
            ui.weak("Pixels with a value");
            ui.label(format!("{} ({pct:.1} %)", pixels_text(sm.valid, sm.px_scale)));
            ui.end_row();
            if let Some(ha) = sm.ha_per_px {
                ui.weak("Their area");
                ui.label(format!("{} ha", format_g(sm.valid as f64 * sm.px_scale * ha, 6)));
                ui.end_row();
            }
            for (label, v, as_value) in [
                ("Minimum", sm.min, true),
                ("Maximum", sm.max, true),
                ("Mean", sm.mean, false),
                ("Std. deviation", sm.std, false),
                ("2nd percentile", sm.q[0], true),
                ("Lower quartile", sm.q[1], true),
                ("Median", sm.q[2], true),
                ("Upper quartile", sm.q[3], true),
                ("98th percentile", sm.q[4], true),
            ] {
                ui.weak(label);
                ui.label(if as_value { num(v) } else { plain(v) });
                ui.end_row();
            }
        });
        if sm.discrete.is_empty() {
            return;
        }
        ui.add_space(8.0);
        ui.strong("Pixels by value");
        let ha = sm.ha_per_px;
        Grid::new("values counts")
            .striped(true)
            .num_columns(3 + usize::from(ha.is_some()))
            .min_row_height(row_h)
            .spacing(gap)
            .show(ui, |ui| {
                ui.weak("Value");
                ui.weak("Pixels");
                ui.weak("%");
                if ha.is_some() {
                    ui.weak("Hectares");
                }
                ui.end_row();
                for &(v, c) in &sm.discrete {
                    ui.horizontal(|ui| {
                        if let Some(col) = self.value_colour(src, v) {
                            let (rect, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                            ui.painter().rect_filled(rect, 2.0, col);
                        }
                        ui.label(num(v));
                    });
                    ui.label(pixels_text(c, sm.px_scale));
                    ui.label(format!("{:.2}", 100.0 * c as f64 / sm.valid.max(1) as f64));
                    if let Some(h) = ha {
                        ui.label(format_g(c as f64 * sm.px_scale * h, 6));
                    }
                    ui.end_row();
                }
            });
    }

    /// The run's map selected in Layers, alone: its index.
    pub(crate) fn selected_result(&self) -> Option<usize> {
        let sel = &self.contents.selected;
        let Some(&Sel::Result(id)) = sel.iter().next().filter(|_| sel.len() == 1) else { return None };
        self.zeit.results.iter().position(|r| r.id == id)
    }

    /// The Display panel of a run's map: what it is, its colours (colormap and range, or its classes), its opacity.
    pub(crate) fn result_display_ui(&mut self, ui: &mut Ui, k: usize) {
        let r = &mut self.zeit.results[k];
        ui.colored_label(theme::accent(), &r.name);
        ui.add_space(4.0);
        ui.strong("Map of a run");
        if !r.source.is_empty() {
            ui.label(format!("Computed on {}", r.source));
        }
        ui.label(format!(
            "{} x {} px{}",
            r.w,
            r.h,
            if r.unit.is_empty() { String::new() } else { format!(", {}", r.unit) }
        ));
        ui.add(egui::Label::new(RichText::new(&r.path).weak()).wrap());
        ui.add_space(6.0);
        ui.strong("Colours");
        let mut dirty = false;
        crate::zeit::result_details_ui(ui, r, &mut dirty);
        self.map.dirty |= dirty;
        ui.add_space(6.0);
        ui.weak("Its values at the cursor and the pins, its histogram: Pixel values (with the series hidden, or a single image active). Click a series in Layers for its own display.");
    }

    /// Pixel values' rows at the cursor: name, value as shown (the self-test).
    pub(crate) fn cursor_values(&self) -> Vec<(String, String)> {
        self.value_rows()
            .iter()
            .map(|r| (r.name.clone(), self.value_text(r.src, f64::from(self.row_value(r, At::Cursor).0))))
            .collect()
    }

    /// The described map's pixels by value (the self-test).
    pub(crate) fn focused_counts(&mut self) -> Option<Vec<(f64, usize)>> {
        let rows = self.value_rows();
        let row = self.focused_row(&rows)?;
        self.update_summary(row);
        self.values.summary.as_ref().map(|(_, s)| s.discrete.clone())
    }

    /// The status bar's words on the top map of the runs shown under the cursor (active pixel x, y).
    pub(crate) fn result_status_at(&self, x: usize, y: usize) -> String {
        for k in (0..self.zeit.results.len()).rev() {
            let r = &self.zeit.results[k];
            if !r.visible || !r.place.aligned {
                continue;
            }
            let Some(g) = &r.grid else { continue };
            let Some((rx, ry)) = r.place.pixel(g, x as f64 + 0.5, y as f64 + 0.5) else { continue };
            let v = r.value_at(rx, ry);
            if v.is_finite() {
                return format!("  |  {} {}", r.name, self.value_text(Src::Result(k), f64::from(v)));
            }
        }
        String::new()
    }
}

impl ValueRow {
    /// An id for its widgets.
    fn sel_key(&self) -> String {
        format!("{:?}", self.sel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_numbers_are_counted_by_value() {
        let data = [2001.0, 2001.0, f32::NAN, 2005.0, 2003.0, f32::NAN];
        let s = summarize(&data, 1.0, None, String::new());
        assert_eq!((s.valid, s.total), (4, 6));
        assert_eq!(s.discrete, vec![(2001.0, 2), (2003.0, 1), (2005.0, 1)]);
        assert_eq!((s.min, s.max, s.mean), (2001.0, 2005.0, 2002.5));
        assert_eq!(s.q[2], 2003.0);
        assert_eq!(s.bins, s.discrete);
    }

    #[test]
    fn fractions_go_to_bins() {
        let data: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let s = summarize(&data, 4.0, Some(0.09), String::new());
        assert!(s.discrete.is_empty());
        assert_eq!(s.bins.len(), 64);
        assert_eq!(s.bins.iter().map(|b| b.1).sum::<usize>(), 1000);
        assert!((s.q[2] - 0.5).abs() < 0.002);
        assert_eq!(pixels_text(s.valid, s.px_scale), "≈ 4 000");
        assert_eq!(pixels_text(1234567, 1.0), "1 234 567");
    }

    #[test]
    fn nothing_with_a_value() {
        let s = summarize(&[f32::NAN; 4], 1.0, None, String::new());
        assert_eq!(s.valid, 0);
        assert!(s.bins.is_empty() && s.discrete.is_empty() && s.mean.is_nan());
    }
}
