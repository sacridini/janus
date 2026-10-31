//! Analysis > Dates: the visible area at every date in a grid of small maps (or the dates of one month, every year;
//! or only the clean ones), from the overview, in the layer's colours (the Value mode's colormap and stretch, or its
//! classes), each with its date and its share of valid pixels; a click goes to that date. Clouds, gaps and a change
//! are seen at once, and which dates are clean. The availability strip under the time slider (map.rs, time_bar)
//! shares the share of valid pixels.

use std::time::{Duration, Instant};

use egui::{Color32, ColorImage, Sense, Stroke, TextureHandle, TextureOptions, Ui, vec2};
use janus_gpu::Mode;

use crate::app::State;
use crate::session::Session;
use crate::theme;

const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
/// At most this many small maps.
const MOST: usize = 600;

/// A small map: its date, its picture and its share of valid pixels.
type SmallMap = (usize, TextureHandle, f32);

pub struct MosaicUi {
    /// 0 every date, 1 the shown date's month of every year
    pub which: usize,
    /// only the dates with this share of valid pixels in view (%), or more
    pub min_valid: f32,
    /// a small map's width (points)
    pub size: f32,
    /// the small maps (date, texture, share of valid pixels) and their key; when they were made
    cache: Option<(u64, Vec<SmallMap>)>,
    made: Option<Instant>,
}

impl Default for MosaicUi {
    fn default() -> Self {
        MosaicUi { which: 0, min_valid: 0.0, size: 132.0, cache: None, made: None }
    }
}

/// The overview pixels in view (x0, y0, x1, y1, fractional; clamped to the image), from the main map's centre,
/// scale and size.
pub(crate) fn visible_overview(st: &State) -> Option<[f64; 4]> {
    let s = st.session.as_ref()?;
    let ov = &s.s.overview;
    let m = &st.map;
    if m.scale <= 0.0 || m.canvas[0] <= 0.0 {
        return Some([0.0, 0.0, ov.w as f64, ov.h as f64]);
    }
    let (hw, hh) = (f64::from(m.canvas[0]) / 2.0 / m.scale, f64::from(m.canvas[1]) / 2.0 / m.scale);
    let r = [
        (m.center[0] - hw).max(0.0),
        (m.center[1] - hh).max(0.0),
        (m.center[0] + hw).min(ov.w as f64),
        (m.center[1] + hh).min(ov.h as f64),
    ];
    (r[2] > r[0] && r[3] > r[1]).then_some(r)
}

/// The share of valid (not NaN) overview pixels of date t inside `r`, from at most about `most` of them; NaN while
/// the date is not read.
pub(crate) fn valid_share(s: &Session, r: [f64; 4], t: usize, most: usize) -> f32 {
    let ov = &s.s.overview;
    let Some(layer) = ov.layer(t).filter(|_| s.uploaded.get(t).copied().unwrap_or(false)) else { return f32::NAN };
    let (x0, y0) = (r[0].floor() as usize, r[1].floor() as usize);
    let (x1, y1) = ((r[2].ceil() as usize).min(ov.w), (r[3].ceil() as usize).min(ov.h));
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if w == 0 || h == 0 {
        return f32::NAN;
    }
    let step = ((w * h) as f64 / most.max(1) as f64).sqrt().ceil().max(1.0) as usize;
    let (mut n, mut ok) = (0usize, 0usize);
    for y in (y0..y1).step_by(step) {
        for x in (x0..x1).step_by(step) {
            n += 1;
            ok += usize::from(!layer[y * ov.w + x].is_nan());
        }
    }
    if n == 0 { f32::NAN } else { ok as f32 / n as f32 }
}

impl State {
    /// The colours of the layer's values as the Value mode draws them: its classes, or its colormap and stretch.
    fn value_colours(&self, s: &Session) -> impl Fn(f32) -> Option<Color32> + use<> {
        let classes: Vec<(i32, Color32, bool)> = if self.classes.on() {
            self.classes
                .list
                .iter()
                .map(|e| (e.value, Color32::from_rgb(e.color[0], e.color[1], e.color[2]), e.visible))
                .collect()
        } else {
            vec![]
        };
        let d = &self.display;
        let mut r = d.ranges[Mode::Value as usize];
        if !r.is_set()
            && let Some((lo, hi)) = d.auto_range_of(Mode::Value, s)
        {
            (r.lo, r.hi) = (lo, hi);
        }
        let (lo, hi) = (r.lo, r.hi.max(r.lo + 1e-12));
        let lut = janus_gpu::colormaps::all()[d.cmap[Mode::Value as usize]].lut();
        let on = !classes.is_empty();
        move |v: f32| {
            if v.is_nan() {
                return None;
            }
            if on {
                let k = v.round() as i32;
                return match classes.binary_search_by_key(&k, |c| c.0) {
                    Ok(i) if classes[i].2 => Some(classes[i].1),
                    Ok(_) => None,
                    Err(_) => Some(Color32::GRAY),
                };
            }
            let c = lut[(((v - lo) / (hi - lo)).clamp(0.0, 1.0) * 255.0).round() as usize];
            Some(Color32::from_rgb(c[0], c[1], c[2]))
        }
    }

    /// The dates the mosaic shows, by its choice of month.
    pub(crate) fn mosaic_dates(&self, s: &Session) -> Vec<usize> {
        let info = &s.info;
        let n = info.t();
        let mut ts: Vec<usize> = (0..n).collect();
        if self.analysis.mosaic.which == 1 && info.time_is_date {
            let month = |t: usize| janus_cube::dates::civil_from_days((info.layers[t].time / 86400.0).floor() as i64).1;
            let m = month(self.display.t.min(n - 1));
            ts.retain(|&t| month(t) == m);
        }
        ts
    }

    /// Analysis > Dates.
    pub(crate) fn mosaic_tab(&mut self, ui: &mut Ui) {
        let Some(s) = &self.session else { return };
        let info = s.info.clone();
        let n = info.t();
        let shown_t = self.display.t.min(n - 1);
        let month = if info.time_is_date {
            let m = janus_cube::dates::civil_from_days((info.layers[shown_t].time / 86400.0).floor() as i64).1;
            MONTHS[(m as usize).clamp(1, 12) - 1]
        } else {
            ""
        };
        let a = &mut self.analysis.mosaic;
        ui.horizontal(|ui| {
            ui.radio_value(&mut a.which, 0, "Every date");
            if info.time_is_date {
                ui.radio_value(&mut a.which, 1, format!("{month} of every year"))
                    .on_hover_text("The dates in the month of the date shown, year after year");
            }
            ui.separator();
            ui.add(egui::Slider::new(&mut a.min_valid, 0.0..=100.0).suffix(" %").integer().text("valid at least"))
                .on_hover_text("Only the dates with this share of the visible area valid (no cloud, no gap), or more");
            ui.separator();
            ui.add(egui::Slider::new(&mut a.size, 64.0..=320.0).integer().text("size"));
        });
        let Some(r) = visible_overview(self) else {
            ui.weak("The map shows none of the series.");
            return;
        };
        let s = self.session.as_ref().unwrap();
        let ov = &s.s.overview;
        // the small maps' pixels: the visible area's shape
        let size = self.analysis.mosaic.size;
        let aspect = ((r[3] - r[1]) / (r[2] - r[0])).clamp(0.2, 5.0) as f32;
        let tw = (size.round() as usize).clamp(16, 400);
        let th = ((size * aspect).round() as usize).clamp(8, 800);
        let dates = self.mosaic_dates(s);
        let d = &self.display;
        let rv = d.ranges[Mode::Value as usize];
        let classes_key = self.classes.list.iter().fold(u64::from(self.classes.on()), |k, e| {
            k.wrapping_mul(31).wrapping_add(e.value as u64 * 7 + u64::from(e.visible) + u64::from(e.color[0]))
        });
        let mut key = (ov.layers_done() as u64)
            .wrapping_mul(1_000_003)
            .wrapping_add((tw * 1000 + th) as u64)
            .wrapping_add(classes_key.wrapping_mul(0x9E37_79B9))
            .wrapping_add((d.cmap[Mode::Value as usize] as u64) << 40)
            .wrapping_add(u64::from(rv.lo.to_bits()).wrapping_mul(13) ^ u64::from(rv.hi.to_bits()).wrapping_mul(17))
            .wrapping_add(info.id.wrapping_mul(7919));
        for v in r {
            key = key.wrapping_mul(31).wrapping_add((v * 4.0).round() as u64);
        }
        for &t in &dates {
            key = key.wrapping_mul(131).wrapping_add(t as u64);
        }
        let a = &self.analysis.mosaic;
        // while the map moves: made again at most 5 times a second
        let stale = a.cache.as_ref().is_none_or(|c| c.0 != key);
        let due = a.made.is_none_or(|m| m.elapsed() > Duration::from_millis(200));
        if stale && due {
            let colour = self.value_colours(s);
            let mut maps = Vec::new();
            for &t in dates.iter().take(MOST) {
                let Some(layer) = ov.layer(t).filter(|_| s.uploaded[t]) else { continue };
                let mut img = ColorImage::new([tw, th], vec![Color32::from_gray(28); tw * th]);
                let (mut n_in, mut n_ok) = (0usize, 0usize);
                for j in 0..th {
                    let y = r[1] + (j as f64 + 0.5) * (r[3] - r[1]) / th as f64;
                    let oy = (y.floor() as usize).min(ov.h - 1);
                    for i in 0..tw {
                        let x = r[0] + (i as f64 + 0.5) * (r[2] - r[0]) / tw as f64;
                        let ox = (x.floor() as usize).min(ov.w - 1);
                        n_in += 1;
                        if let Some(c) = colour(layer[oy * ov.w + ox]) {
                            img.pixels[j * tw + i] = c;
                            n_ok += 1;
                        }
                    }
                }
                let share = if n_in > 0 { n_ok as f32 / n_in as f32 } else { 0.0 };
                let tex = ui.ctx().load_texture(format!("mosaic {t}"), img, TextureOptions::NEAREST);
                maps.push((t, tex, share));
            }
            let a = &mut self.analysis.mosaic;
            a.cache = Some((key, maps));
            a.made = Some(Instant::now());
        } else if stale {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
        let a = &self.analysis.mosaic;
        let Some((_, maps)) = &a.cache else { return };
        let min_valid = a.min_valid / 100.0;
        let shown: Vec<&SmallMap> = maps.iter().filter(|m| m.2 >= min_valid - 1e-6).collect();
        let loading = if ov.complete() { String::new() } else { format!(", {} of {n} dates read", ov.layers_done()) };
        let cut = if dates.len() > MOST { format!(" (the first {MOST})") } else { String::new() };
        ui.weak(format!(
            "{} of {} dates{cut}{loading}. The visible area in the layer's colours (Value at date); the share of it \
             valid under each. A click goes to the date.",
            shown.len(),
            dates.len()
        ));
        let mut go = None;
        let accent = theme::accent();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                for (t, tex, share) in shown {
                    let label = &info.layers[*t].label;
                    let cell = vec2(tw as f32, th as f32 + 16.0);
                    let (rect, resp) = ui.allocate_exact_size(cell, Sense::click());
                    let img_rect = egui::Rect::from_min_size(rect.min, vec2(tw as f32, th as f32));
                    let p = ui.painter();
                    p.image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    let current = *t == shown_t;
                    let stroke = if current {
                        Stroke::new(2.5, accent)
                    } else if resp.hovered() {
                        Stroke::new(1.5, ui.visuals().text_color())
                    } else {
                        Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color)
                    };
                    p.rect_stroke(img_rect, 0.0, stroke, egui::StrokeKind::Outside);
                    let text = format!("{label}  {:.0} %", share * 100.0);
                    let colour = if current { accent } else { ui.visuals().text_color() };
                    p.text(
                        rect.left_bottom() + vec2(1.0, -1.0),
                        egui::Align2::LEFT_BOTTOM,
                        text,
                        egui::FontId::proportional(11.0),
                        colour,
                    );
                    if resp
                        .on_hover_text(format!("{label}: {:.1} % of the visible area valid", share * 100.0))
                        .clicked()
                    {
                        go = Some(*t);
                    }
                }
            });
        });
        if let Some(t) = go {
            self.set_date(t);
            self.map.dirty = true;
        }
    }
}
