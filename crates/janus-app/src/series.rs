//! The series on the chart (src/app.cpp: pumpSeries, approxSeries, addPin, clearRoi, updateRoiSeries; app_layers.cpp:
//! the other layers' series): the pixel under the cursor, the pins, the ROI's statistics date by date, and the
//! same places on every other visible layer. A series is approximated from the overview at once and replaced by the
//! exact one once read (the cursor's after the mouse rests 0.12 s).

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use egui::Color32;
use janus_cube::stats::{SeriesStats, series_stats};
use janus_session::RoiData;

use crate::app::State;
use crate::session::Session;
use crate::theme;

/// A series shown on the chart: the cursor's or a pin's.
#[derive(Clone)]
pub struct SeriesView {
    /// pin number (stable: removing one does not renumber the others); 0 for the cursor
    pub id: u32,
    pub x: usize,
    pub y: usize,
    pub values: Vec<f32>,
    /// false: approximated from the overview (until the read finishes)
    pub exact: bool,
    pub stats: SeriesStats,
    pub color: Color32,
}

/// Pins' colours: vivid against the colormaps and the dark background; no yellow (the ROI) and no orange (the
/// date's line).
pub fn pin_color(id: u32) -> Color32 {
    const COLORS: [[f32; 3]; 8] = [
        [1.00, 0.36, 0.42],
        [0.25, 0.80, 1.00],
        [0.76, 0.52, 1.00],
        [0.55, 0.95, 0.40],
        [1.00, 0.55, 0.85],
        [0.30, 1.00, 0.80],
        [1.00, 0.66, 0.52],
        [0.56, 0.70, 1.00],
    ];
    let c = COLORS[(id.max(1) as usize - 1) % COLORS.len()];
    Color32::from_rgb((c[0] * 255.0).round() as u8, (c[1] * 255.0).round() as u8, (c[2] * 255.0).round() as u8)
}

/// The ROI's yellow.
pub const ROI_COLOR: Color32 = Color32::from_rgb(255, 209, 61);

/// The series of a pixel from the overview: the dates on the GPU, the others NaN.
pub fn approx_series(s: &Session, x: usize, y: usize) -> Vec<f32> {
    let ov = &s.s.overview;
    let ox = (x * ov.w / s.info.width).min(ov.w - 1);
    let oy = (y * ov.h / s.info.height).min(ov.h - 1);
    (0..ov.t)
        .map(|t| if s.uploaded[t] { ov.layer(t).map_or(f32::NAN, |l| l[oy * ov.w + ox]) } else { f32::NAN })
        .collect()
}

fn view(s: &Session, id: u32, x: usize, y: usize, color: Color32) -> SeriesView {
    let values = approx_series(s, x, y);
    let stats = series_stats(&s.years, &values);
    SeriesView { id, x, y, values, exact: false, stats, color }
}

/// The exact series of `v`, if read.
fn take_exact(s: &Session, v: &mut SeriesView) -> bool {
    if v.exact {
        return false;
    }
    let Some(r) = s.exact(v.x, v.y) else { return false };
    v.values = r.values;
    v.exact = true;
    v.stats = series_stats(&s.years, &v.values);
    true
}

/// The ROI's series: the mean and the percentiles of its pixels, date by date.
#[derive(Default)]
pub struct RoiSeries {
    pub data: Option<Arc<RoiData>>,
    /// dates done when last read (usize::MAX: not yet)
    seen: usize,
    pub mean: Vec<f32>,
    pub p10: Vec<f32>,
    pub p90: Vec<f32>,
    pub p25: Vec<f32>,
    pub p50: Vec<f32>,
    pub p75: Vec<f32>,
    pub stats: SeriesStats,
    /// the map's ROI generation it belongs to
    pub gen_: u64,
}

impl RoiSeries {
    fn start(data: Arc<RoiData>, gen_: u64) -> Self {
        RoiSeries { data: Some(data), seen: usize::MAX, gen_, ..Default::default() }
    }

    pub fn clear(&mut self) {
        if let Some(d) = &self.data {
            d.cancel.store(true, Ordering::Relaxed);
        }
        *self = RoiSeries { seen: usize::MAX, ..Default::default() };
    }

    /// The dates computed since the last call; whether any came.
    fn update(&mut self, years: &[f64]) -> bool {
        let Some(d) = &self.data else { return false };
        let done = d.done.load(Ordering::Acquire);
        if done == self.seen {
            return false;
        }
        self.seen = done;
        let n = d.per_t.len();
        for v in [&mut self.mean, &mut self.p10, &mut self.p90, &mut self.p25, &mut self.p50, &mut self.p75] {
            *v = vec![f32::NAN; n];
        }
        for (t, st) in d.per_t.iter().enumerate() {
            let Some(st) = st.get().filter(|st| st.n > 0) else { continue };
            self.mean[t] = st.mean;
            self.p10[t] = st.p10;
            self.p90[t] = st.p90;
            self.p25[t] = st.p25;
            self.p50[t] = st.p50;
            self.p75[t] = st.p75;
        }
        self.stats = series_stats(years, &self.mean);
        true
    }

    pub fn complete(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.complete())
    }

    pub fn sampled(&self) -> bool {
        self.data.as_ref().is_some_and(|d| d.sampled)
    }
}

/// The chart's and the map's series state (the C++'s hover_, pins_, roi_...).
pub struct SeriesState {
    pub hover: Option<SeriesView>,
    /// the cursor moved: its exact series is asked for once it rests
    hover_since: Option<Instant>,
    /// overview dates its approximation had
    approx_layers: usize,
    pub pins: Vec<SeriesView>,
    pub next_pin: u32,
    /// the ROI rectangle in the active layer's pixels (x0, y0, x1, y1): the polygon's bounding box if it has one
    pub roi_rect: Option<[usize; 4]>,
    /// the ROI's polygon in the active layer's pixels (empty: the rectangle)
    pub roi_poly: janus_geo::plane::Polygon,
    /// R: a polygon being drawn
    pub poly: crate::roi::PolygonDraft,
    /// waiting for an HDD's overview
    pub roi_pending: bool,
    pub roi: RoiSeries,
    /// every new or cleared ROI
    pub roi_gen: u64,
    /// a Shift + drag being made: its corners (active pixels)
    pub roi_drag: Option<([f64; 2], [f64; 2])>,
    /// the chart: 0 the active layer only, 1 every visible layer
    pub chart_layers: i32,
}

impl Default for SeriesState {
    fn default() -> Self {
        SeriesState {
            hover: None,
            hover_since: None,
            approx_layers: 0,
            pins: Vec::new(),
            next_pin: 1,
            roi_rect: None,
            roi_poly: Vec::new(),
            poly: Default::default(),
            roi_pending: false,
            roi: RoiSeries { seen: usize::MAX, ..Default::default() },
            roi_gen: 1,
            roi_drag: None,
            chart_layers: 0,
        }
    }
}

/// A non-active layer's series at the same places.
#[derive(Default)]
pub struct OtherSeries {
    pub hover: Option<SeriesView>,
    pub pins: Vec<SeriesView>,
    pub roi: RoiSeries,
}

impl State {
    /// The cursor over the active layer's pixel (x, y): its series approximated now, read once it rests.
    pub fn set_hover(&mut self, at: Option<(usize, usize)>) {
        let Some(s) = &self.session else { return };
        let sr = &mut self.series;
        match at {
            None => {}
            Some((x, y)) if sr.hover.as_ref().is_some_and(|h| h.x == x && h.y == y) => {}
            Some((x, y)) => {
                sr.hover = Some(view(s, 0, x, y, theme::cursor_series()));
                sr.approx_layers = s.s.overview.layers_done();
                sr.hover_since = Some(Instant::now());
                self.update_other_hover();
            }
        }
    }

    pub fn add_pin(&mut self, x: usize, y: usize) {
        let Some(s) = self.session.as_ref().filter(|s| x < s.info.width && y < s.info.height) else { return };
        let id = self.series.next_pin;
        self.series.next_pin += 1;
        let p = view(s, id, x, y, pin_color(id));
        if !s.s.defer_random_reads() {
            s.request_pin(x, y);
        }
        self.add_other_pins(&p);
        self.series.pins.push(p);
    }

    pub fn remove_pin(&mut self, id: u32) {
        self.series.pins.retain(|p| p.id != id);
        for l in &mut self.layers {
            l.series.pins.retain(|p| p.id != id);
        }
    }

    pub fn clear_pins(&mut self) {
        self.series.pins.clear();
        for l in &mut self.layers {
            l.series.pins.clear();
        }
    }

    /// The ROI gone (with every layer's).
    pub fn clear_roi(&mut self) {
        let sr = &mut self.series;
        sr.roi.clear();
        sr.roi_pending = false;
        sr.roi_rect = None;
        sr.roi_poly.clear();
        sr.roi_gen += 1;
        for l in &mut self.layers {
            l.series.roi.clear();
        }
        self.map.dirty = true;
    }

    /// A new ROI rectangle (active pixels, any corners): the active layer's statistics start (or wait for the HDD's
    /// overview); the other layers' follow.
    pub fn set_roi_rect(&mut self, r: [i64; 4]) {
        self.set_roi_shape(r, Vec::new());
    }

    /// A new ROI: its bounding box and polygon (empty: the rectangle).
    pub(crate) fn set_roi_shape(&mut self, r: [i64; 4], poly: janus_geo::plane::Polygon) {
        self.clear_roi();
        let Some(s) = &self.session else { return };
        let (w, h) = (s.info.width as i64, s.info.height as i64);
        // clamped as Session::start_roi does
        let x0 = r[0].min(r[2]).clamp(0, w - 1);
        let y0 = r[1].min(r[3]).clamp(0, h - 1);
        let x1 = r[0].max(r[2]).clamp(x0 + 1, w);
        let y1 = r[1].max(r[3]).clamp(y0 + 1, h);
        let rect = [x0 as usize, y0 as usize, x1 as usize, y1 as usize];
        let sr = &mut self.series;
        sr.roi_rect = Some(rect);
        sr.roi_poly = poly;
        sr.roi_gen += 1;
        if s.s.defer_random_reads() {
            sr.roi_pending = true;
        } else {
            sr.roi = RoiSeries::start(crate::roi::start_roi(s, rect, &sr.roi_poly), sr.roi_gen);
        }
    }

    /// The other layers' cursor series at the active layer's cursor pixel.
    fn update_other_hover(&mut self) {
        if self.series.chart_layers == 0 {
            return;
        }
        let Some(h) = &self.series.hover else { return };
        let (x, y) = (h.x as f64 + 0.5, h.y as f64 + 0.5);
        let active = self.active;
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(s) = l.session.as_ref().filter(|_| i != active) else { continue };
            let Some((lx, ly)) = l.place.pixel(&s.info, x, y).filter(|_| l.visible) else {
                l.series.hover = None;
                continue;
            };
            if l.series.hover.as_ref().is_some_and(|h| h.x == lx && h.y == ly) {
                continue;
            }
            l.series.hover = Some(view(s, 0, lx, ly, theme::cursor_series()));
        }
    }

    /// The cursor as if it had rested (the tour): its exact series asked for now.
    pub fn rest_hover(&mut self) {
        self.series.hover_since = None;
        if let (Some(s), Some(h)) = (&self.session, &self.series.hover)
            && !s.s.defer_random_reads()
        {
            s.request_hover(h.x, h.y);
        }
        self.request_other_series(true);
    }

    /// A pin of the active layer on every other layer it falls on.
    fn add_other_pins(&mut self, pin: &SeriesView) {
        let (x, y) = (pin.x as f64 + 0.5, pin.y as f64 + 0.5);
        let active = self.active;
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(s) = l.session.as_ref().filter(|_| i != active) else { continue };
            let Some((lx, ly)) = l.place.pixel(&s.info, x, y) else { continue };
            if !s.s.defer_random_reads() && l.visible && self.series.chart_layers == 1 {
                s.request_pin(lx, ly);
            }
            l.series.pins.push(view(s, pin.id, lx, ly, pin.color));
        }
    }

    /// "All visible layers" turned on or off: the other layers' series at the cursor and the pins.
    pub fn set_chart_layers(&mut self, v: i32) {
        self.series.chart_layers = v;
        for l in &mut self.layers {
            l.series.hover = None;
            l.series.pins.clear();
        }
        if v == 1 {
            self.update_other_hover();
            let pins = self.series.pins.clone();
            for p in &pins {
                self.add_other_pins(p);
            }
            self.request_other_series(true);
        }
    }

    /// Exact reads for the other layers (after the same rest as the active one's).
    fn request_other_series(&mut self, hover: bool) {
        if self.series.chart_layers == 0 {
            return;
        }
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(s) = l.session.as_ref().filter(|s| i != self.active && l.visible && !s.s.defer_random_reads())
            else {
                continue;
            };
            if hover && let Some(h) = l.series.hover.as_ref().filter(|h| !h.exact) {
                s.request_hover(h.x, h.y);
            }
            for p in l.series.pins.iter().filter(|p| !p.exact) {
                s.request_pin(p.x, p.y);
            }
        }
    }

    /// Once per frame: exact series that came, the cursor's read once it rests, the ROI's dates.
    pub(crate) fn pump_series(&mut self) {
        let Some(s) = &self.session else { return };
        s.collect_series(); // those read since the session's pump
        let sr = &mut self.series;
        if let Some(h) = &mut sr.hover {
            take_exact(s, h);
            // approximated again as the overview fills
            if !h.exact && s.s.overview.layers_done() != sr.approx_layers {
                sr.approx_layers = s.s.overview.layers_done();
                h.values = approx_series(s, h.x, h.y);
                h.stats = series_stats(&s.years, &h.values);
            }
        }
        for p in &mut sr.pins {
            take_exact(s, p);
        }
        let mut request_others = false;
        if sr.hover_since.is_some_and(|t| t.elapsed().as_secs_f64() >= 0.12) {
            sr.hover_since = None;
            if let Some(h) = &sr.hover
                && !s.s.defer_random_reads()
            {
                s.request_hover(h.x, h.y);
            }
            request_others = true;
        }
        if !s.s.defer_random_reads() {
            // pins made while an HDD's overview was building
            for p in sr.pins.iter().filter(|p| !p.exact) {
                if s.exact(p.x, p.y).is_none() {
                    s.request_pin(p.x, p.y);
                }
            }
            if sr.roi_pending
                && let Some(r) = sr.roi_rect
            {
                sr.roi = RoiSeries::start(crate::roi::start_roi(s, r, &sr.roi_poly), sr.roi_gen);
                sr.roi_pending = false;
            }
        }
        sr.roi.update(&s.years);
        if request_others {
            self.request_other_series(true);
        }
        self.pump_other_series();
    }

    /// The other layers: their exact series, their ROI (with "All visible layers").
    fn pump_other_series(&mut self) {
        let (rect, gen_, all) = (self.series.roi_rect, self.series.roi_gen, self.series.chart_layers == 1);
        let active = self.active;
        // the ROI on each layer it starts on (a polygon there; empty: the rectangle, straight)
        let polys: Vec<Option<janus_geo::plane::Polygon>> = self
            .layers
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let starts =
                    i != active && rect.is_some() && all && l.visible && l.place.aligned && l.series.roi.gen_ != gen_;
                starts.then(|| self.roi_on_layer(l))
            })
            .collect();
        let straight = self.series.roi_poly.is_empty();
        for (i, l) in self.layers.iter_mut().enumerate() {
            let Some(s) = l.session.as_ref().filter(|_| i != active) else { continue };
            if let Some(h) = &mut l.series.hover {
                take_exact(s, h);
            }
            for p in &mut l.series.pins {
                take_exact(s, p);
            }
            // the map's ROI on this layer: the pixels whose centres are inside it
            if let Some(r) = rect
                && all
                && l.visible
                && l.place.aligned
                && l.series.roi.gen_ != gen_
                && !s.s.defer_random_reads()
            {
                l.series.roi.clear();
                l.series.roi.gen_ = gen_;
                let poly = polys[i].clone().unwrap_or_default();
                let bx = if !poly.is_empty() {
                    janus_geo::plane::bounds(&poly)
                } else if l.place.reproj.is_some() || !straight {
                    None // nothing of it transforms onto this layer
                } else {
                    let (a, b) =
                        (l.place.to_layer(r[0] as f64, r[1] as f64), l.place.to_layer(r[2] as f64, r[3] as f64));
                    a.zip(b).map(|(a, b)| [a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1)])
                };
                if let Some(bx) = bx {
                    // pixels whose centres are inside: [ceil(b0 - 0.5), ceil(b1 - 0.5))
                    let lx0 = (bx[0] - 0.5).ceil().max(0.0) as usize;
                    let ly0 = (bx[1] - 0.5).ceil().max(0.0) as usize;
                    let lx1 = ((bx[2] - 0.5).ceil().max(0.0) as usize).min(s.info.width);
                    let ly1 = ((bx[3] - 0.5).ceil().max(0.0) as usize).min(s.info.height);
                    if lx1 > lx0 && ly1 > ly0 {
                        l.series.roi = RoiSeries::start(crate::roi::start_roi(s, [lx0, ly0, lx1, ly1], &poly), gen_);
                    }
                }
            }
            l.series.roi.update(&s.years);
        }
    }

    /// The pins carried to a new active layer's grid (their pixels there), read again.
    pub(crate) fn rebuild_pins(&mut self, at: Vec<(u32, Color32, usize, usize)>) {
        let Some(s) = &self.session else { return };
        self.series.pins = at
            .into_iter()
            .map(|(id, color, x, y)| {
                if !s.s.defer_random_reads() {
                    s.request_pin(x, y);
                }
                view(s, id, x, y, color)
            })
            .collect();
        for l in &mut self.layers {
            l.series.pins.clear();
            l.series.hover = None;
        }
        let pins = self.series.pins.clone();
        for p in &pins {
            self.add_other_pins(p);
        }
        self.series.hover = None;
    }
}
