//! Analysis > Bands: every band of a series of several bands a date (surface reflectance: Blue .. SWIR2), date by
//! date, at the cursor, a pin or over the ROI, as a heatmap: the dates across, the bands down (by wavelength when
//! known). Each band in its own range, all of them in one, or as z-scores (each band minus its mean, over its
//! standard deviation, in the map's anomaly colours): a clearing shows as red and SWIR rising while NIR falls. The
//! dates the quality band (Fmask, QA_PIXEL, SCL) masks are left empty, as on the chart. A place's bands are read on
//! a thread, the newest request only.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, Rect, Stroke, Ui, vec2};
use egui_plot::{Plot, PlotPoint};
use janus_cube::multidim::format_g;
use janus_cube::{CubeInfo, CubeReader, QaRule};
use janus_geo::plane;

use crate::app::State;
use crate::theme;

/// The ROI's bands are averaged on a grid of at most this many cells a side (read resampled, nearest).
const ROI_CELLS: usize = 128;

/// Where the bands are read: a pixel, or the ROI (its rectangle and polygon, in the layer's pixels).
#[derive(Clone)]
enum Place {
    Pixel(usize, usize),
    Roi([usize; 4], plane::Polygon),
}

/// What a grid was read for: the series, the place (a pixel, or the ROI's generation) and the quality rule.
#[derive(Clone, PartialEq, Debug)]
struct Key {
    cube: u64,
    x: usize,
    y: usize,
    roi: u64,
    qa: (i32, i32),
}

struct Request {
    key: Key,
    info: Arc<CubeInfo>,
    place: Place,
    bands: Vec<i32>,
    qa: Option<(i32, QaRule)>,
}

/// Every band (rows, in `bands`' order) at every date: NaN where there is no value or the date is masked.
struct Grid {
    key: Key,
    bands: Vec<i32>,
    values: Vec<f32>,
    dates: usize,
}

impl Grid {
    fn at(&self, row: usize, t: usize) -> f32 {
        self.values[row * self.dates + t]
    }
}

/// The tab's state.
#[derive(Default)]
pub struct BandsUi {
    source: String,
    /// 0 each band its own range, 1 one range for every band, 2 z-scores
    pub scale: usize,
    grid: Option<Grid>,
    asked: Option<Key>,
    tx: Option<Sender<Request>>,
    rx: Option<Receiver<Grid>>,
}

/// The worker: the newest request in turn; a reader kept for the series.
fn worker(rx: Receiver<Request>, tx: Sender<Grid>, ctx: egui::Context) {
    let mut readers: HashMap<u64, CubeReader> = HashMap::new();
    while let Ok(first) = rx.recv() {
        let r = rx.try_iter().last().unwrap_or(first);
        readers.retain(|&id, _| id == r.info.id);
        let reader = readers.entry(r.info.id).or_insert_with(|| CubeReader::new(r.info.clone()));
        let values = match &r.place {
            Place::Pixel(x, y) => read_pixel(reader, &r, *x, *y),
            Place::Roi(rect, poly) => read_roi(reader, &r, *rect, poly),
        };
        let grid = Grid { key: r.key, bands: r.bands, values, dates: r.info.t() };
        if tx.send(grid).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// A pixel's bands at every date (rows by band), the dates the quality band masks NaN.
fn read_pixel(reader: &mut CubeReader, r: &Request, x: usize, y: usize) -> Vec<f32> {
    let n = r.info.t();
    let mut out = vec![f32::NAN; r.bands.len() * n];
    for t in 0..n {
        if let Some((qb, rule)) = r.qa {
            let code = reader.read_band_pixel(t, qb, x, y, true);
            if !code.is_some_and(|c| c.is_finite() && c >= 0.0 && rule.usable(c as u32)) {
                continue;
            }
        }
        for (row, &b) in r.bands.iter().enumerate() {
            out[row * n + t] = reader.read_band_pixel(t, b, x, y, false).unwrap_or(f32::NAN);
        }
    }
    out
}

/// The mean of the ROI's pixels for each band at every date: on a grid of at most ROI_CELLS a side over its
/// rectangle, the cells in its polygon and usable.
fn read_roi(reader: &mut CubeReader, r: &Request, [x0, y0, x1, y1]: [usize; 4], poly: &plane::Polygon) -> Vec<f32> {
    let n = r.info.t();
    let (w, h) = (x1.saturating_sub(x0).max(1), y1.saturating_sub(y0).max(1));
    let k = ((w * h) as f64 / (ROI_CELLS * ROI_CELLS) as f64).sqrt().ceil().max(1.0) as usize;
    let (bw, bh) = (w.div_ceil(k), h.div_ceil(k));
    // the cells inside the polygon (every one without one)
    let inside: Vec<bool> = (0..bw * bh)
        .map(|c| {
            let (i, j) = (c % bw, c / bw);
            let px = x0 as f64 + (i as f64 + 0.5) * w as f64 / bw as f64;
            let py = y0 as f64 + (j as f64 + 0.5) * h as f64 / bh as f64;
            poly.is_empty() || plane::contains(poly, px, py)
        })
        .collect();
    let mut out = vec![f32::NAN; r.bands.len() * n];
    let (mut buf, mut usable) = (vec![0f32; bw * bh], inside.clone());
    for t in 0..n {
        usable.copy_from_slice(&inside);
        if let Some((qb, rule)) = r.qa {
            if !reader.read_band_window(t, qb, (x0, y0, w, h), &mut buf, (bw, bh)) {
                continue;
            }
            for (u, &c) in usable.iter_mut().zip(&buf) {
                *u &= c.is_finite() && c >= 0.0 && rule.usable(c as u32);
            }
        }
        for (row, &b) in r.bands.iter().enumerate() {
            if !reader.read_band_window(t, b, (x0, y0, w, h), &mut buf, (bw, bh)) {
                continue;
            }
            let (mut sum, mut cnt) = (0f64, 0usize);
            for (&v, _) in buf.iter().zip(&usable).filter(|(v, u)| **u && v.is_finite()) {
                sum += f64::from(v);
                cnt += 1;
            }
            if cnt > 0 {
                out[row * n + t] = (sum / cnt as f64) as f32;
            }
        }
    }
    out
}

/// The bands shown, top to bottom: every band of a date but the quality band, by wavelength when each has one.
fn shown_bands(info: &CubeInfo) -> Vec<i32> {
    let qa = info.sel.qa_band;
    let mut b: Vec<i32> = (1..=info.bands_per_date as i32).filter(|&b| b != qa).collect();
    let nm = |b: i32| info.band_nm.get(b as usize - 1).copied().filter(|v| v.is_finite() && *v > 0.0);
    if b.iter().all(|&x| nm(x).is_some()) {
        b.sort_by(|&p, &q| nm(p).unwrap().total_cmp(&nm(q).unwrap()));
    }
    b
}

fn band_name(info: &CubeInfo, b: i32) -> String {
    info.band_names.get(b as usize - 1).filter(|n| !n.is_empty()).cloned().unwrap_or_else(|| format!("Band {b}"))
}

/// The value at quantile `q` (0..=1) of the finite values, if any.
fn quantile(v: &[f32], q: f64) -> Option<f32> {
    let mut f: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() {
        return None;
    }
    let k = ((f.len() - 1) as f64 * q).round() as usize;
    Some(*f.select_nth_unstable_by(k, f32::total_cmp).1)
}

/// How each row's values become colours: a row's (lo, hi) range, or its mean and standard deviation (z-scores,
/// symmetric about 0 up to `a`).
enum Scale {
    Ranges(Vec<(f32, f32)>),
    Z(Vec<(f32, f32)>, f32),
}

fn scale_of(g: &Grid, mode: usize) -> Scale {
    let rows: Vec<&[f32]> = g.values.chunks(g.dates.max(1)).collect();
    let range = |v: &[f32]| {
        let (lo, hi) = (quantile(v, 0.02).unwrap_or(0.0), quantile(v, 0.98).unwrap_or(1.0));
        if hi > lo { (lo, hi) } else { (lo, lo + 1.0) }
    };
    match mode {
        1 => Scale::Ranges(vec![range(&g.values); rows.len()]),
        2 => {
            let ms: Vec<(f32, f32)> = rows
                .iter()
                .map(|v| {
                    let f: Vec<f64> = v.iter().filter(|x| x.is_finite()).map(|&x| f64::from(x)).collect();
                    let n = f.len().max(1) as f64;
                    let m = f.iter().sum::<f64>() / n;
                    let sd = (f.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
                    (m as f32, if sd > 0.0 { sd as f32 } else { 1.0 })
                })
                .collect();
            // symmetric about 0, to the largest |z| of most dates (at least 1, at most 4)
            let z: Vec<f32> =
                rows.iter().zip(&ms).flat_map(|(v, &(m, sd))| v.iter().map(move |&x| ((x - m) / sd).abs())).collect();
            let a = quantile(&z, 0.98).unwrap_or(2.0).clamp(1.0, 4.0);
            Scale::Z(ms, a)
        }
        _ => Scale::Ranges(rows.iter().map(|v| range(v)).collect()),
    }
}

impl Scale {
    /// A value's place on the colormap (0..=1).
    fn at(&self, row: usize, v: f32) -> f32 {
        match self {
            Scale::Ranges(r) => ((v - r[row].0) / (r[row].1 - r[row].0)).clamp(0.0, 1.0),
            Scale::Z(ms, a) => (0.5 + 0.5 * (v - ms[row].0) / ms[row].1 / a).clamp(0.0, 1.0),
        }
    }
}

impl State {
    /// The bands asked for read (screenshots wait for them while Analysis > Bands is shown).
    pub(crate) fn bands_settled(&self) -> bool {
        let b = &self.bands;
        !(self.analysis.show && self.analysis.tab == 6)
            || b.asked.is_none()
            || b.grid.as_ref().map(|g| &g.key) == b.asked.as_ref()
    }

    /// Analysis > Bands.
    pub(crate) fn bands_tab(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        if info.stack || info.spectral {
            ui.weak("A stack of variables or a spectrum: its bands are the layer's axis already (the chart).");
            return;
        }
        if info.bands_per_date < 2 {
            ui.weak("One band a date: nothing to compare across bands.");
            ui.weak("(For series of several bands a date: surface reflectance, Sentinel-2's bands...)");
            return;
        }
        // where: the cursor, a pin or the ROI, as in Seasonal
        let mut sources: Vec<(String, String, Place)> = Vec::new();
        if let Some(h) = &self.series.hover {
            sources.push(("cursor".into(), format!("Cursor ({}, {})", h.x, h.y), Place::Pixel(h.x, h.y)));
        }
        for p in &self.series.pins {
            sources.push((format!("pin{}", p.id), format!("Pin {} ({}, {})", p.id, p.x, p.y), Place::Pixel(p.x, p.y)));
        }
        if let Some(r) = self.series.roi_rect {
            sources.push(("roi".into(), "ROI mean".into(), Place::Roi(r, self.series.roi_poly.clone())));
        }
        if sources.is_empty() {
            ui.weak("Hover over the map, place a pin or draw an ROI.");
            return;
        }
        let b = &mut self.bands;
        let src = sources.iter().position(|s| s.0 == b.source).unwrap_or(0);
        let mut copy = false;
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("bands source")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 13.0)
                .selected_text(&sources[src].1)
                .show_ui(ui, |ui| {
                    for s in &sources {
                        if ui.selectable_label(s.0 == sources[src].0, &s.1).clicked() {
                            b.source = s.0.clone();
                        }
                    }
                })
                .response
                .on_hover_text("Where the bands are read (the cursor follows the mouse over the map)");
            ui.radio_value(&mut b.scale, 0, "Each band's range")
                .on_hover_text("Each band from its own low to its own high (2nd to 98th percentile): its changes");
            ui.radio_value(&mut b.scale, 1, "One range")
                .on_hover_text("Every band in the same range: which bands are bright, which dark");
            ui.radio_value(&mut b.scale, 2, "Z-scores").on_hover_text(
                "Each band minus its mean, over its standard deviation, in the map's anomaly colours: the dates a \
                 band is unusually high or low (a clearing: red and SWIR up, NIR down)",
            );
            copy = ui.button("Copy CSV").on_hover_text("The dates and every band's value (tab-separated)").clicked();
        });
        let bands = shown_bands(&info);
        let qa =
            (info.sel.qa_band > 0 && info.sel.qa_rule != QaRule::None).then_some((info.sel.qa_band, info.sel.qa_rule));
        let (x, y, roi) = match &sources[src].2 {
            Place::Pixel(x, y) => (*x, *y, 0),
            Place::Roi(..) => (usize::MAX, usize::MAX, self.series.roi_gen),
        };
        let key = Key { cube: info.id, x, y, roi, qa: qa.map_or((0, 0), |(b, r)| (b, r.code())) };
        self.bands_request(&info, key.clone(), sources[src].2.clone(), bands, qa, ui.ctx());
        let b = &self.bands;
        let Some(g) = b.grid.as_ref().filter(|g| g.key.cube == info.id && g.dates == info.t()) else {
            ui.weak("Reading the bands...");
            return;
        };
        if g.key != key {
            ui.weak("Reading the bands here... (the last place's shown meanwhile)");
        }
        if copy {
            let mut text = String::from("date");
            for &band in &g.bands {
                text += &format!("\t{}", band_name(&info, band));
            }
            for t in 0..g.dates {
                text += &format!("\n{}", info.layers[t].label);
                for row in 0..g.bands.len() {
                    let v = g.at(row, t);
                    text += &format!("\t{}", if v.is_finite() { format_g(f64::from(v), 7) } else { String::new() });
                }
            }
            ui.ctx().copy_text(text);
        }
        self.bands_heatmap(ui, &info);
    }

    /// The bands' grid asked for `key` (if it is not the one shown nor the one asked last).
    fn bands_request(
        &mut self,
        info: &Arc<CubeInfo>,
        key: Key,
        place: Place,
        bands: Vec<i32>,
        qa: Option<(i32, QaRule)>,
        ctx: &egui::Context,
    ) {
        let b = &mut self.bands;
        if let Some(rx) = &b.rx {
            while let Ok(g) = rx.try_recv() {
                b.grid = Some(g);
            }
        }
        if b.grid.as_ref().is_some_and(|g| g.key == key) || b.asked.as_ref() == Some(&key) {
            return;
        }
        if b.tx.is_none() {
            let (tx, rx) = channel();
            let (rtx, rrx) = channel();
            let c = ctx.clone();
            std::thread::Builder::new().name("bands".into()).spawn(move || worker(rx, rtx, c)).ok();
            (b.tx, b.rx) = (Some(tx), Some(rrx));
        }
        if let Some(tx) = &b.tx {
            let _ = tx.send(Request { key: key.clone(), info: info.clone(), place, bands, qa });
        }
        b.asked = Some(key);
    }

    /// The heatmap: dates across (the chart's time axis), bands down, a colour scale at the right.
    fn bands_heatmap(&self, ui: &mut Ui, info: &CubeInfo) {
        let Some(g) = &self.bands.grid else { return };
        let mode = self.bands.scale;
        let scale = scale_of(g, mode);
        let cm = &janus_gpu::colormaps::all()
            [self.display.cmap[if mode == 2 { janus_gpu::Mode::Anomaly } else { janus_gpu::Mode::Value } as usize]];
        let colour = |row: usize, v: f32| {
            let c = cm.sample(scale.at(row, v));
            Color32::from_rgb(c[0], c[1], c[2])
        };
        let xs: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
        let rows = g.bands.len();
        let names: Vec<String> = g.bands.iter().map(|&b| band_name(info, b)).collect();
        let weak = ui.visuals().weak_text_color();
        let scale_w = ui.text_style_height(&egui::TextStyle::Body) * 6.0;
        let size = vec2((ui.available_width() - scale_w).max(120.0), ui.available_height().max(80.0));
        ui.horizontal_top(|ui| {
            let label_names = names.clone();
            let mut plot = Plot::new(("bands heat", info.id))
                .width(size.x)
                .height(size.y)
                .include_y(-0.5)
                .include_y(rows as f64 - 0.5)
                .y_axis_formatter(move |m, _| {
                    // the first band at the top
                    let r = m.value.round();
                    if (m.value - r).abs() < 1e-6 && r >= 0.0 && (r as usize) < label_names.len() {
                        label_names[label_names.len() - 1 - r as usize].clone()
                    } else {
                        String::new()
                    }
                })
                .y_grid_spacer(move |_| {
                    (0..rows).map(|r| egui_plot::GridMark { value: r as f64, step_size: 1.0 }).collect()
                })
                .set_margin_fraction(egui::Vec2::ZERO)
                .show_background(false)
                .show_grid(false)
                .allow_drag(false)
                .allow_scroll(false)
                .allow_zoom(false)
                .allow_boxed_zoom(false)
                .show_x(false)
                .show_y(false);
            if let (Some(&a), Some(&z)) = (xs.first(), xs.last()) {
                let half = if xs.len() > 1 { 0.5 * (xs[1] - xs[0]) } else { 1.0 };
                plot = plot.include_x(a - half).include_x(z + half);
            }
            plot = if info.time_is_date {
                crate::chart::time_axis(plot, Some(weak))
            } else {
                plot.custom_x_axes(vec![egui_plot::AxisHints::new_x().tick_label_color(weak)])
            };
            let r = plot.show(ui, |p| p.pointer_coordinate());
            let t = r.transform;
            let painter = ui.painter_at(*t.frame());
            let cols = crate::chart::date_columns(&t, &xs);
            let row_y = |row: usize| {
                let yv = (rows - 1 - row) as f64;
                let a = t.position_from_point(&PlotPoint::new(xs[0], yv + 0.5)).y;
                let b = t.position_from_point(&PlotPoint::new(xs[0], yv - 0.5)).y;
                a..=b
            };
            for row in 0..rows {
                let yr = row_y(row);
                for (k, &(l, rr)) in cols.iter().enumerate() {
                    let v = g.at(row, k);
                    let rect = Rect::from_x_y_ranges(l..=rr, yr.clone());
                    if v.is_finite() {
                        painter.rect_filled(rect, 0.0, colour(row, v));
                    } else {
                        // no value (masked, no data): faint
                        painter.rect_filled(rect, 0.0, ui.visuals().faint_bg_color);
                    }
                }
            }
            // the bands apart
            let line = Stroke::new(1.0, ui.visuals().panel_fill);
            for row in 1..rows {
                let y = *row_y(row).start();
                painter.hline(t.frame().x_range(), y, line);
            }
            // the current date
            let cur = self.display.t.min(xs.len() - 1);
            if let Some(&(l, rr)) = cols.get(cur) {
                painter.rect_stroke(
                    Rect::from_x_y_ranges(l..=rr, t.frame().y_range()),
                    0.0,
                    Stroke::new(2.0, theme::on_plot(Color32::from_rgb(255, 153, 51))),
                    egui::StrokeKind::Middle,
                );
            }
            if let Some(m) = r.inner.filter(|_| r.response.hovered()) {
                let k = info.nearest_layer(m.x);
                let row = (rows as f64 - 0.5 - m.y).floor();
                if row >= 0.0 && (row as usize) < rows {
                    let row = row as usize;
                    let v = g.at(row, k);
                    let what = if !v.is_finite() {
                        "no value (masked or no data)".to_string()
                    } else if let Scale::Z(ms, _) = &scale {
                        format!("{}  (z {:+.2})", format_g(f64::from(v), 5), (v - ms[row].0) / ms[row].1)
                    } else {
                        format_g(f64::from(v), 5)
                    };
                    r.response.on_hover_text(format!("{}\n{}: {what}", info.layers[k].label, names[row]));
                }
            }
            // the colour scale
            match &scale {
                Scale::Z(_, a) => crate::analysis::colour_scale(
                    ui,
                    size.y,
                    self.display.cmap[janus_gpu::Mode::Anomaly as usize],
                    f64::from(-a),
                    f64::from(*a),
                    &|v| format!("z {v:+.1}"),
                ),
                Scale::Ranges(rg) if mode == 1 => crate::analysis::colour_scale(
                    ui,
                    size.y,
                    self.display.cmap[janus_gpu::Mode::Value as usize],
                    f64::from(rg[0].0),
                    f64::from(rg[0].1),
                    &|v| format_g(v, 3),
                ),
                Scale::Ranges(_) => crate::analysis::colour_scale(
                    ui,
                    size.y,
                    self.display.cmap[janus_gpu::Mode::Value as usize],
                    0.0,
                    1.0,
                    &|v| if v > 0.5 { "band's high".into() } else { "band's low".into() },
                ),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(values: Vec<f32>, dates: usize) -> Grid {
        let rows = values.len() / dates;
        let key = Key { cube: 1, x: 0, y: 0, roi: 0, qa: (0, 0) };
        Grid { key, bands: (1..=rows as i32).collect(), values, dates }
    }

    #[test]
    fn scales() {
        // two bands of very different levels: each band's range puts both across the colormap
        let g = grid(vec![100.0, 200.0, 300.0, 1000.0, 3000.0, 5000.0], 3);
        let s = scale_of(&g, 0);
        assert!(s.at(0, 100.0) < 0.05 && s.at(0, 300.0) > 0.95);
        assert!(s.at(1, 1000.0) < 0.05 && s.at(1, 5000.0) > 0.95);
        // one range: the dark band stays dark
        let s = scale_of(&g, 1);
        assert!(s.at(0, 300.0) < 0.1 && s.at(1, 5000.0) > 0.9);
        // z-scores: the mean at the middle, symmetric
        let s = scale_of(&g, 2);
        assert!((s.at(0, 200.0) - 0.5).abs() < 1e-6 && (s.at(1, 3000.0) - 0.5).abs() < 1e-6);
        assert!((s.at(0, 100.0) + s.at(0, 300.0) - 1.0).abs() < 1e-5);
        // NaN (a masked date) left out of the ranges
        let g = grid(vec![1.0, f32::NAN, 3.0], 3);
        let Scale::Ranges(r) = scale_of(&g, 0) else { panic!() };
        assert_eq!(r[0], (1.0, 3.0));
    }
}
