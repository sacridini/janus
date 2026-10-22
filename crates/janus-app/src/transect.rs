//! Space-time transect, a Hovmoeller diagram (src/app_transect.cpp): a line drawn on the map (Ctrl + drag, or T
//! then drag) or a polyline (T, then a click per vertex: along a river, a road, a valley) is sampled at every
//! date, and the Transect panel shows the distance along it (X) against the dates (Y, oldest on top) in the
//! layer's colours. It is filled at once from the overview in RAM, then refined with full-resolution values read
//! in the background (a job per date on the session's interactive pool; on an HDD only once the overview is
//! built). The image is drawn by the map renderer: the values are a float texture drawn like a detail tile into a
//! target of its own, so colormaps and class colours are the map's.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use egui::{Color32, Pos2, Rect, Response, Shape, Stroke, Ui, vec2};
use egui_plot::{GridMark, Plot, PlotImage, PlotPoint};
use janus_cube::multidim::format_g;
use janus_gpu::{GpuTexture, LayerParams, Mode};

use crate::app::State;
use crate::map::body_font;
use crate::theme;

/// Columns of the image: longer lines are subsampled.
const MAX_SAMPLES: usize = 1024;
/// The target the image is drawn into.
pub const TRANSECT_SLOT: i32 = 96;
const LINE: Color32 = Color32::from_rgb(80, 230, 255);

/// Clips the segment a-b to [0, w] x [0, h] (Liang-Barsky).
fn clip_segment(a: &mut [f64; 2], b: &mut [f64; 2], w: f64, h: f64) -> bool {
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let p = [-dx, dx, -dy, dy];
    let q = [a[0], w - a[0], a[1], h - a[1]];
    for i in 0..4 {
        if p[i] == 0.0 {
            if q[i] < 0.0 {
                return false;
            }
            continue;
        }
        let r = q[i] / p[i];
        if p[i] < 0.0 {
            t0 = t0.max(r);
        } else {
            t1 = t1.min(r);
        }
    }
    if t0 >= t1 {
        return false;
    }
    let a0 = *a;
    *a = [a0[0] + t0 * dx, a0[1] + t0 * dy];
    *b = [a0[0] + t1 * dx, a0[1] + t1 * dy];
    true
}

fn format_distance(d: f64, unit: &str) -> String {
    if unit == "m" && d.abs() >= 10000.0 {
        format!("{:.2} km", d / 1000.0)
    } else {
        format!("{} {unit}", format_g(d, 5))
    }
}

/// Full-resolution rows, written by the readers (a job per date).
struct TransectExact {
    px: Vec<i64>,
    py: Vec<i64>,
    rows: Vec<OnceLock<Vec<f32>>>,
    done: AtomicUsize,
    cancel: AtomicBool,
}

#[derive(Default)]
pub struct Transect {
    pub on: bool,
    pub cube: u64,
    /// the vertices in the active layer's pixels (a straight line clipped to the layer)
    pub pts: Vec<[f64; 2]>,
    /// the line as drawn, georeferenced: another layer samples its own part of it
    geo: Option<Vec<(f64, f64)>>,
    /// "m", "map units" or "px"
    pub unit: String,
    /// the distance along the line at each vertex
    pub vdist: Vec<f64>,
    pub length: f64,
    /// samples along it, dates
    pub n: usize,
    pub t_n: usize,
    /// each sample's pixel (-1: off the layer) and place
    pub px: Vec<i64>,
    pub py: Vec<i64>,
    pub at: Vec<[f64; 2]>,
    /// [t][i]: the overview's at first, then the files'
    pub values: Vec<f32>,
    /// per date: 0 nothing yet, 1 from the overview, 2 at full resolution
    pub state: Vec<u8>,
    /// what is drawn: the values, or their anomalies
    pub shown: Vec<f32>,
    /// its own range (while the map has none for the mode)
    range: (f32, f32),
    tex: Option<GpuTexture>,
    version: u64,
    dirty: bool,
    published: Option<Instant>,
    job: Option<Arc<TransectExact>>,
    /// HDD: full resolution once the overview is built
    pub pending: bool,
    /// 0 values, 1 anomaly (each place minus its mean through time)
    pub kind: usize,
    /// the plot's bounds set again (a new line)
    pub fit: bool,
    /// the cell hovered in the panel (marked on the map)
    pub hover_i: Option<usize>,
    drawn_key: u64,
}

/// A transect being drawn on a map canvas.
#[derive(Default)]
pub struct TransectDraft {
    /// T: drawing (a drag or clicked vertices)
    pub mode: bool,
    /// the vertices clicked
    pub vertices: Vec<[f64; 2]>,
    panel: Option<egui::Id>,
    start: [f64; 2],
    end: [f64; 2],
    dragged: bool,
    cancel: bool,
}

impl State {
    /// A transect along the active layer's pixels `pts` (2: a straight line, clipped to the layer; more: a
    /// polyline, without data where it leaves the layer).
    pub fn set_transect(&mut self, mut pts: Vec<[f64; 2]>) {
        self.clear_transect();
        let Some(s) = &self.session else { return };
        if pts.len() < 2 {
            return;
        }
        let info = s.info.clone();
        let tr = &mut self.transect;
        let geo: Option<Vec<(f64, f64)>> = pts.iter().map(|p| info.pixel_to_geo(p[0], p[1])).collect();
        tr.geo = geo.filter(|_| info.geo_transform.is_some());
        if pts.len() == 2 {
            let (mut a, mut b) = (pts[0], pts[1]);
            if !clip_segment(&mut a, &mut b, info.width as f64, info.height as f64) {
                self.clear_transect();
                return;
            }
            pts = vec![a, b];
        }
        let tr = &mut self.transect;
        tr.on = true;
        tr.cube = info.id;
        // each segment's length: metres on the ellipsoid's mean sphere for geographic CRSs (EPSG:4xxx, degrees),
        // map units otherwise (metres for an EPSG projected CRS), else pixels
        let auth = &info.crs_authority;
        let degrees = auth.len() == 9 && auth.starts_with("EPSG:4");
        let georef = info.geo_transform.is_some();
        tr.unit = if !georef {
            "px"
        } else if degrees || !auth.is_empty() {
            "m"
        } else {
            "map units"
        }
        .to_string();
        let segs = pts.len() - 1;
        let len_px: Vec<f64> =
            (0..segs).map(|k| (pts[k + 1][0] - pts[k][0]).hypot(pts[k + 1][1] - pts[k][1])).collect();
        let total_px: f64 = len_px.iter().sum();
        let len: Vec<f64> = (0..segs)
            .map(|k| {
                let g = georef
                    .then(|| {
                        info.pixel_to_geo(pts[k][0], pts[k][1]).zip(info.pixel_to_geo(pts[k + 1][0], pts[k + 1][1]))
                    })
                    .flatten();
                match g {
                    None => len_px[k],
                    Some(((g0x, g0y), (g1x, g1y))) if degrees => {
                        let (rad, r) = (std::f64::consts::PI / 180.0, 6371008.8);
                        let (la1, la2) = (g0y * rad, g1y * rad);
                        let (dla, dlo) = (la2 - la1, (g1x - g0x) * rad);
                        let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
                        2.0 * r * h.sqrt().min(1.0).asin()
                    }
                    Some(((g0x, g0y), (g1x, g1y))) => (g1x - g0x).hypot(g1y - g0y),
                }
            })
            .collect();
        let dists = |l: &[f64]| -> Vec<f64> {
            let mut v = vec![0.0];
            for &x in l {
                v.push(v.last().unwrap() + x);
            }
            v
        };
        tr.vdist = dists(&len);
        tr.length = *tr.vdist.last().unwrap();
        if tr.length.is_nan() || tr.length <= 0.0 {
            // a degenerate line, or no georeferencing: pixels
            tr.unit = "px".into();
            tr.vdist = dists(&len_px);
            tr.length = *tr.vdist.last().unwrap();
        }
        if tr.length.is_nan() || tr.length <= 0.0 {
            self.clear_transect();
            return;
        }
        // samples evenly spaced along the line (in distance), about one per pixel
        let n = ((total_px.ceil() as usize) + 1).clamp(2, MAX_SAMPLES);
        let t_n = info.t();
        (tr.n, tr.t_n) = (n, t_n);
        (tr.px, tr.py, tr.at) = (vec![-1; n], vec![-1; n], vec![[0.0; 2]; n]);
        let mut k = 0;
        let mut inside = 0;
        for i in 0..n {
            let d = tr.length * i as f64 / (n - 1) as f64;
            while k + 1 < segs && tr.vdist[k + 1] < d {
                k += 1;
            }
            let seg = tr.vdist[k + 1] - tr.vdist[k];
            let f = if seg > 0.0 { ((d - tr.vdist[k]) / seg).clamp(0.0, 1.0) } else { 0.0 };
            let q = [pts[k][0] + (pts[k + 1][0] - pts[k][0]) * f, pts[k][1] + (pts[k + 1][1] - pts[k][1]) * f];
            tr.at[i] = q;
            let (x, y) = (q[0].floor() as i64, q[1].floor() as i64);
            let (w, h) = (info.width as i64, info.height as i64);
            // a straight line's ends may sit on the far edge (clipped): kept on the layer
            let inn = segs == 1 || (x >= 0 && y >= 0 && x < w && y < h);
            if inn {
                (tr.px[i], tr.py[i]) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
                inside += 1;
            }
        }
        tr.pts = pts;
        if inside == 0 {
            self.clear_transect();
            return;
        }
        tr.values = vec![f32::NAN; n * t_n];
        tr.state = vec![0; t_n];
        if s.s.defer_random_reads() {
            tr.pending = true; // HDD still building the overview
        } else {
            self.start_transect_exact();
        }
        self.update_transect_rows();
        self.publish_transect();
    }

    pub fn clear_transect(&mut self) {
        let tr = &mut self.transect;
        if let Some(j) = &tr.job {
            j.cancel.store(true, Ordering::Relaxed);
        }
        let kind = tr.kind;
        *tr = Transect { kind, ..Default::default() };
        if let Some(gpu) = &mut self.gpu {
            gpu.release_target(TRANSECT_SLOT);
        }
        self.map_textures.remove(&TRANSECT_SLOT);
    }

    /// The full-resolution rows: a job per date on the interactive pool, after the cursor series, tiles and the
    /// ROI; oldest date first.
    fn start_transect_exact(&mut self) {
        let Some(s) = &self.session else { return };
        let tr = &mut self.transect;
        let job = Arc::new(TransectExact {
            px: tr.px.clone(),
            py: tr.py.clone(),
            rows: (0..tr.t_n).map(|_| OnceLock::new()).collect(),
            done: AtomicUsize::new(0),
            cancel: AtomicBool::new(false),
        });
        for t in 0..tr.t_n {
            let (job, info) = (job.clone(), s.info.clone());
            s.s.fg().submit(20 + t as i32, move || {
                let n = job.px.len();
                let mut row = vec![f32::NAN; n];
                let gone = janus_cube::with_thread_reader(&info, |r| {
                    for i in 0..n {
                        if job.cancel.load(Ordering::Relaxed) {
                            return true;
                        }
                        let (x, y) = (job.px[i], job.py[i]);
                        row[i] = if x < 0 {
                            f32::NAN // off the layer
                        } else if i > 0 && x == job.px[i - 1] && y == job.py[i - 1] {
                            row[i - 1]
                        } else {
                            r.read_pixel(t, x as usize, y as usize).unwrap_or(f32::NAN)
                        };
                    }
                    false
                });
                if !gone {
                    let _ = job.rows[t].set(row);
                    job.done.fetch_add(1, Ordering::Release);
                }
            });
        }
        tr.job = Some(job);
        tr.pending = false;
    }

    /// Rows from the full-resolution job, else from the overview (the same pixels as the approximate series).
    pub(crate) fn update_transect_rows(&mut self) -> bool {
        let Some(s) = &self.session else { return false };
        let tr = &mut self.transect;
        let ov = &s.s.overview;
        let mut changed = false;
        for t in 0..tr.t_n {
            if tr.state[t] == 2 {
                continue;
            }
            let row = &mut tr.values[t * tr.n..(t + 1) * tr.n];
            if let Some(exact) = tr.job.as_ref().and_then(|j| j.rows[t].get()) {
                row.copy_from_slice(exact);
                tr.state[t] = 2;
                changed = true;
            } else if tr.state[t] == 0
                && s.uploaded.get(t).copied().unwrap_or(false)
                && let Some(l) = ov.layer(t)
            {
                for ((v, &x), &y) in row.iter_mut().zip(&tr.px).zip(&tr.py) {
                    if x < 0 {
                        continue; // off the layer: NaN
                    }
                    let ox = ((x as f64 * ov.w as f64 / s.info.width as f64) as usize).min(ov.w - 1);
                    let oy = ((y as f64 * ov.h as f64 / s.info.height as f64) as usize).min(ov.h - 1);
                    *v = l[oy * ov.w + ox];
                }
                tr.state[t] = 1;
                changed = true;
            }
        }
        tr.dirty |= changed;
        changed
    }

    /// Dates read at full resolution.
    pub(crate) fn transect_rows_read(&self) -> usize {
        self.transect.job.as_ref().map_or(0, |j| j.done.load(Ordering::Acquire))
    }

    fn transect_anomaly(&self) -> bool {
        self.transect.kind == 1 && !self.classes.on()
    }

    /// What is drawn (the values, or each place minus its mean through time), its own range, its texture.
    pub(crate) fn publish_transect(&mut self) {
        let anomaly = self.transect_anomaly();
        let tr = &mut self.transect;
        let (n, t_n) = (tr.n, tr.t_n);
        tr.shown = tr.values.clone();
        if anomaly {
            for i in 0..n {
                let (mut sum, mut k) = (0.0f64, 0);
                for t in 0..t_n {
                    let v = tr.values[t * n + i];
                    if v.is_finite() {
                        sum += f64::from(v);
                        k += 1;
                    }
                }
                let mean = if k > 0 { (sum / k as f64) as f32 } else { f32::NAN };
                for t in 0..t_n {
                    tr.shown[t * n + i] -= mean;
                }
            }
        }
        let step = (tr.shown.len() / 200_000).max(1);
        let mut smp: Vec<f32> = tr.shown.iter().step_by(step).copied().filter(|v| v.is_finite()).collect();
        if !smp.is_empty() {
            tr.range = crate::display::auto_range(if anomaly { Mode::Anomaly } else { Mode::Value }, &mut smp);
        }
        tr.tex = self.gpu.as_ref().map(|g| g.create_tile(n as u32, t_n as u32, &tr.shown));
        tr.version += 1;
        tr.dirty = false;
        tr.published = Some(Instant::now());
    }

    /// The map's colormap and range for the mode when it has one (automatic range already computed, or set by
    /// hand), else the transect's own range.
    pub(crate) fn transect_colors(&self) -> (usize, f32, f32) {
        let m = if self.transect_anomaly() { Mode::Anomaly } else { Mode::Value } as usize;
        let r = &self.display.ranges[m];
        let (lo, hi) = if r.is_set() { (r.lo, r.hi) } else { self.transect.range };
        (self.display.cmap[m], lo, hi)
    }

    /// Once a frame: another active layer takes the same place on it; rows read come in; the image is remade at
    /// most 5 times a second while they keep coming.
    pub(crate) fn pump_transect(&mut self, ctx: &egui::Context) {
        if !self.transect.on {
            return;
        }
        let Some(s) = &self.session else {
            self.clear_transect();
            return;
        };
        if s.info.id != self.transect.cube {
            // another active layer (or the same one reopened): the same place on it
            let pts: Option<Vec<[f64; 2]>> = self.transect.geo.as_ref().and_then(|g| {
                let gt = s.info.geo_transform?;
                Some(g.iter().map(|&(x, y)| [(x - gt[0]) / gt[1], (y - gt[3]) / gt[5]]).collect())
            });
            match pts {
                Some(p) => self.set_transect(p), // cleared if it is outside this layer
                None => self.clear_transect(),
            }
            return;
        }
        if self.transect.pending && !s.s.defer_random_reads() {
            self.start_transect_exact();
        }
        self.update_transect_rows();
        let tr = &self.transect;
        let reading = tr.job.is_some();
        let full = if reading { 2 } else { 1 };
        let waiting = tr.state.iter().filter(|&&s| s != full).count();
        if tr.dirty && (waiting == 0 || tr.published.is_none_or(|p| p.elapsed().as_secs_f64() >= 0.2)) {
            self.publish_transect();
        }
        if reading && waiting > 0 {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
    }

    /// Ctrl + drag, or T then drag: a straight line; T then clicks: a polyline (a double click, or a click on the
    /// last vertex, ends it). Returns whether the pointer was taken.
    pub(crate) fn transect_input(&mut self, ui: &Ui, resp: &Response, p: Option<[f64; 2]>) -> bool {
        let k = self.screen_per_px();
        let d = &mut self.transect_draft;
        let (pressed, shift, ctrl) = ui.input(|i| (i.pointer.primary_pressed(), i.modifiers.shift, i.modifiers.ctrl));
        if resp.hovered() && pressed && !shift && (ctrl || d.mode) {
            let Some(p) = p else { return false };
            (d.panel, d.cancel, d.dragged, d.start, d.end) = (Some(resp.id), false, false, p, p);
        }
        if d.panel != Some(resp.id) {
            return false;
        }
        let Some(p) = p else { return true };
        if resp.is_pointer_button_down_on() {
            if !d.cancel {
                d.end = p;
            }
            let moved = ui.input(|i| {
                i.pointer.press_origin().zip(i.pointer.interact_pos()).is_some_and(|(a, b)| a.distance(b) > 4.0)
            });
            if d.vertices.is_empty() && moved {
                d.dragged = true;
            }
            return true;
        }
        // released: a drag is a straight line of a few points at least; a click in transect mode a vertex
        d.panel = None;
        if d.cancel {
            return true;
        }
        if d.dragged {
            let (a, b) = (d.start, d.end);
            if ((b[0] - a[0]) * k).hypot((b[1] - a[1]) * k) >= 5.0 {
                d.mode = false;
                self.set_transect(vec![a, b]);
            }
            return true;
        }
        if !d.mode {
            return true; // Ctrl + click: nothing
        }
        if let Some(&last) = d.vertices.last()
            && (resp.double_clicked() || ((p[0] - last[0]) * k).hypot((p[1] - last[1]) * k) <= 6.0)
        {
            self.finish_transect_draft();
            return true;
        }
        d.vertices.push(p);
        true
    }

    /// The vertices clicked become the transect (without repeated ones: a double click is two clicks).
    pub fn finish_transect_draft(&mut self) {
        let k = self.screen_per_px();
        let mut pts: Vec<[f64; 2]> = Vec::new();
        for v in std::mem::take(&mut self.transect_draft.vertices) {
            if pts.last().is_none_or(|l| (v[0] - l[0]).hypot(v[1] - l[1]) * k > 1.0) {
                pts.push(v);
            }
        }
        if pts.len() < 2 {
            return;
        }
        if pts.len() == 2 {
            // clicked: not clipped
            pts.insert(1, [(pts[0][0] + pts[1][0]) / 2.0, (pts[0][1] + pts[1][1]) / 2.0]);
        }
        self.set_transect(pts);
        self.transect.fit = true;
        self.transect_draft.mode = false;
    }

    /// T toggles the drawing; Esc ends it (and a drag being made); while clicking vertices Enter ends the line,
    /// Backspace / Delete take the last vertex back.
    pub(crate) fn transect_shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Key;
        let (t, enter, back, esc) = ctx.input(|i| {
            (
                i.key_pressed(Key::T) && i.modifiers.is_none(),
                i.key_pressed(Key::Enter),
                i.key_pressed(Key::Backspace) || i.key_pressed(Key::Delete),
                i.key_pressed(Key::Escape),
            )
        });
        let d = &mut self.transect_draft;
        if t {
            d.mode = !d.mode;
        }
        if esc {
            d.mode = false;
            if d.panel.is_some() {
                d.cancel = true;
            }
        }
        if !d.mode {
            d.vertices.clear();
            return;
        }
        if enter {
            self.finish_transect_draft();
        }
        if back {
            self.transect_draft.vertices.pop();
        }
    }

    /// On a map canvas: the line (A and B at its ends, a dot at each bend), the one being drawn, the cell hovered in
    /// the Transect panel, the hint of the mode.
    pub(crate) fn draw_transect_marks(&self, painter: &egui::Painter, rect: Rect, at: &dyn Fn(f64, f64) -> Pos2) {
        let label = |p: Pos2, name: &str| {
            painter.circle_filled(p, 4.5, LINE);
            painter.circle_stroke(p, 4.5, Stroke::new(1.5, Color32::from_black_alpha(220)));
            let font = body_font(painter);
            painter.text(
                p + vec2(7.0, -15.0),
                egui::Align2::LEFT_TOP,
                name,
                font.clone(),
                Color32::from_black_alpha(220),
            );
            painter.text(p + vec2(6.0, -16.0), egui::Align2::LEFT_TOP, name, font, LINE);
        };
        let polyline = |pts: &[[f64; 2]], ends: bool| {
            let s: Vec<Pos2> = pts.iter().map(|v| at(v[0], v[1])).collect();
            if s.len() >= 2 {
                painter.add(Shape::line(s.clone(), Stroke::new(4.0, Color32::from_black_alpha(200))));
                painter.add(Shape::line(s.clone(), Stroke::new(2.0, LINE)));
            }
            for &c in s.iter().skip(1).take(s.len().saturating_sub(2)) {
                painter.circle_filled(c, 3.0, LINE);
                painter.circle_stroke(c, 3.0, Stroke::new(1.0, Color32::from_black_alpha(220)));
            }
            if let Some(&a) = s.first() {
                label(a, "A");
            }
            if ends && s.len() >= 2 {
                label(*s.last().unwrap(), "B");
            }
        };
        let d = &self.transect_draft;
        let hover = painter.ctx().pointer_hover_pos().filter(|p| rect.contains(*p));
        if d.panel.is_some() && !d.cancel && d.dragged {
            polyline(&[d.start, d.end], true);
        } else if !d.vertices.is_empty() {
            // a polyline being clicked, and the edge to the cursor
            let mut pts = d.vertices.clone();
            if let Some(h) = hover
                && let Some(g) = self.grid()
            {
                let [ix, iy] = self.map.image_at(rect, h);
                pts.push([ix * g.fx, iy * g.fy]);
            }
            polyline(&pts, false);
        } else if self.transect.on {
            let tr = &self.transect;
            polyline(&tr.pts, true);
            if let Some(&c) = tr.hover_i.and_then(|i| tr.at.get(i)) {
                let c = at(c[0], c[1]);
                painter.circle_stroke(c, 7.0, Stroke::new(4.0, Color32::from_black_alpha(220)));
                painter.circle_stroke(c, 7.0, Stroke::new(2.0, Color32::WHITE));
            }
        }
        if d.mode
            && d.panel.is_none()
            && let Some(h) = hover
        {
            let hint = if d.vertices.is_empty() {
                "drag: straight transect; click: first vertex of a polyline (Esc cancels)"
            } else {
                "click: next vertex; double click / Enter: done; Backspace: undo; Esc: cancel"
            };
            let p = h + vec2(16.0, 12.0);
            painter.text(
                p + vec2(1.0, 1.0),
                egui::Align2::LEFT_TOP,
                hint,
                body_font(painter),
                Color32::from_black_alpha(220),
            );
            painter.text(p, egui::Align2::LEFT_TOP, hint, body_font(painter), LINE);
        }
    }

    /// The image in whole cells of pixels (crisp when scaled), with the map's colours.
    pub(crate) fn render_transect(&mut self, size: [u32; 2]) {
        let (cmap, lo, hi) = self.transect_colors();
        let lut = self.classes.lut.clone().filter(|_| self.classes.on());
        let Some(gpu) = self.gpu.as_mut() else { return };
        let mut f = gpu.begin_map(TRANSECT_SLOT, size, [0.10, 0.10, 0.115, 1.0]); // no data
        let mut p = LayerParams::new(Mode::Value, [0, 0, 0], [lo, hi]);
        p.class_lut = lut;
        if let Some(tex) = &self.transect.tex {
            gpu.draw_values(&mut f, tex, [0.0, 0.0, size[0] as f32, size[1] as f32], &p, cmap, 1.0);
        }
        gpu.end_map(f);
    }

    /// The Transect panel: the line's length and state, values or anomalies, Copy CSV, the image (hover: the
    /// distance, date and value, marked on the map; click: that date).
    pub(crate) fn transect_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        if !self.transect.on {
            ui.weak("No transect. Ctrl + drag on the map (or T, then drag; T, then clicks: a polyline).");
            return;
        }
        let classes_on = self.classes.on();
        ui.horizontal_wrapped(|ui| {
            if let Some(l) = self.layers.get(self.active) {
                ui.colored_label(theme::accent(), &l.name);
            }
            let tr = &self.transect;
            let exact = tr.state.iter().filter(|&&s| s == 2).count();
            let bends = if tr.pts.len() > 2 { format!(", {} bends", tr.pts.len() - 2) } else { String::new() };
            let status = if tr.pending {
                "overview; full resolution once the overview is built (HDD)"
            } else if exact == tr.t_n {
                "full resolution"
            } else if tr.job.is_some() {
                "overview, full resolution coming..."
            } else {
                "overview"
            };
            ui.weak(format!("{}{bends}, {} samples  |  {status}", format_distance(tr.length, &tr.unit), tr.n));
            if tr.job.is_some() && exact < tr.t_n {
                ui.weak(format!("{exact}/{}", tr.t_n));
            }
        });
        let mut clear = false;
        ui.horizontal(|ui| {
            let kinds = ["Values", "Anomaly (- sample mean)"];
            let mut kind = self.transect.kind;
            ui.add_enabled_ui(!classes_on, |ui| {
                egui::ComboBox::from_id_salt("transect kind").width(170.0).selected_text(kinds[kind]).show_ui(
                    ui,
                    |ui| {
                        for (i, k) in kinds.iter().enumerate() {
                            ui.selectable_value(&mut kind, i, *k);
                        }
                    },
                )
            })
            .response
            .on_hover_text(
                "Values: as on the map ('Value at date' colours and range)\n\
                 Anomaly: each place minus its own mean through time",
            );
            if kind != self.transect.kind {
                self.transect.kind = kind;
                self.publish_transect();
            }
            if ui
                .button("Copy CSV")
                .on_hover_text(
                    "Copies the transect to the clipboard: one row per date, one column per\n\
                     sample (header: distance from A)",
                )
                .clicked()
            {
                ui.ctx().copy_text(self.transect_csv());
            }
            clear = ui.button("Clear").clicked();
            ui.weak("(?)").on_hover_text(
                "Distance along the line from A (X) against the dates (Y, oldest on top).\n\
                 Hover: distance, date and value, marked on the map. Click: go to that date.\n\
                 Draw another line: Ctrl + drag on the map (or T, then drag); a polyline:\n\
                 T, then a click per vertex (double click or Enter ends it).\n\
                 Dashed verticals: the bends. Filled from the overview at once, then with\n\
                 full-resolution values.",
            );
        });
        if clear {
            self.clear_transect();
            return;
        }

        let (n, t_n) = (self.transect.n, self.transect.t_n);
        let sp = self.transect.length / (n - 1) as f64; // sample spacing
        let (x0, x1) = (-sp / 2.0, self.transect.length + sp / 2.0);
        // whole cells of pixels, up to 4096 a side
        let avail = ui.available_size();
        let ppp = ui.ctx().pixels_per_point();
        let cw = ((avail.x * ppp / n as f32).ceil() as usize).clamp(1, (4096 / n).max(1));
        let ch = ((avail.y * ppp / t_n as f32).ceil() as usize).clamp(1, (4096 / t_n).max(1));
        let size = [(n * cw) as u32, (t_n * ch) as u32];
        let (cmap, lo, hi) = self.transect_colors();
        let mut key = self.transect.version.wrapping_mul(1_000_003) ^ ((cmap as u64) << 48) ^ ((cw as u64) << 32);
        key ^= (ch as u64) << 40;
        key = key.wrapping_mul(31).wrapping_add(u64::from(lo.to_bits()));
        key = key.wrapping_mul(31).wrapping_add(u64::from(hi.to_bits()));
        if classes_on {
            for c in &self.classes.list {
                key = key.wrapping_mul(31).wrapping_add(
                    (u64::from(c.color[0]) << 16 | u64::from(c.color[1]) << 8 | u64::from(c.color[2]))
                        + u64::from(c.visible),
                );
            }
        }
        let Some((texture, remade)) = self.target_texture(TRANSECT_SLOT, size) else { return };
        if remade || key != self.transect.drawn_key {
            self.render_transect(size);
            self.transect.drawn_key = key;
        }
        // date ticks: every k-th date, so the labels do not overlap
        let row_h = ui.text_style_height(&egui::TextStyle::Body);
        // (egui_plot leaves out labels closer than 20 points)
        let plot_h = (avail.y - 2.5 * row_h).max(60.0);
        let every = ((t_n as f32 * (row_h * 1.5).max(32.0) / plot_h).ceil() as usize).max(1);
        let labels: Vec<String> = info.layers.iter().map(|l| l.label.clone()).collect();
        let unit = self.transect.unit.clone();
        let fit = std::mem::take(&mut self.transect.fit);
        let mut plot = Plot::new(("transect", self.transect.cube))
            .invert_y(true)
            .show_background(false)
            .show_grid(false) // egui_plot draws it over the image
            .set_margin_fraction(egui::Vec2::ZERO)
            .allow_boxed_zoom(false)
            .x_axis_label(format!("distance from A ({unit})"))
            .include_x(x0)
            .include_x(x1)
            .include_y(0.0)
            .include_y(t_n as f64)
            .y_grid_spacer(move |_| {
                (0..t_n).step_by(every).map(|t| GridMark { value: t as f64 + 0.5, step_size: every as f64 }).collect()
            })
            .y_axis_formatter(move |m, _| {
                let t = (m.value - 0.5).round();
                if t >= 0.0 && (t as usize) < labels.len() { labels[t as usize].clone() } else { String::new() }
            })
            .show_x(false)
            .show_y(false);
        if fit {
            plot = plot.reset();
        }
        let t_now = self.display.t;
        // the map cursor, when it is on the line: its distance along it (within 10 screen points)
        let cursor = self.map.prev_pointer_panel.and(self.series.hover.as_ref()).and_then(|h| {
            let tr = &self.transect;
            let k = self.screen_per_px();
            let p = [h.x as f64 + 0.5, h.y as f64 + 0.5];
            let mut best = (100.0, None);
            for s in 0..tr.pts.len().saturating_sub(1).min(tr.vdist.len().saturating_sub(1)) {
                let (a, b) = (tr.pts[s], tr.pts[s + 1]);
                let ab = [b[0] - a[0], b[1] - a[1]];
                let len2 = ab[0] * ab[0] + ab[1] * ab[1];
                let u = if len2 > 0.0 {
                    (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let off = [(a[0] + ab[0] * u - p[0]) * k, (a[1] + ab[1] * u - p[1]) * k];
                let d2 = off[0] * off[0] + off[1] * off[1];
                if d2 < best.0 {
                    best = (d2, Some(tr.vdist[s] + u * (tr.vdist[s + 1] - tr.vdist[s])));
                }
            }
            best.1
        });
        let response = plot.show(ui, |plot| {
            plot.image(
                PlotImage::new(
                    "transect",
                    texture,
                    PlotPoint::new((x0 + x1) / 2.0, t_n as f64 / 2.0),
                    vec2((x1 - x0) as f32, t_n as f32),
                )
                .uv(Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0))),
            );
            plot.pointer_coordinate()
        });
        let tr_ = response.transform;
        let painter = ui.painter_at(*tr_.frame());
        let px = |x: f64, y: f64| tr_.position_from_point(&PlotPoint::new(x, y));
        // the current date (orange, as on the chart)
        let orange = Color32::from_rgb(255, 153, 51);
        painter.rect_stroke(
            Rect::from_two_pos(px(x0, t_now as f64), px(x1, t_now as f64 + 1.0)),
            0.0,
            Stroke::new(1.5, orange),
            egui::StrokeKind::Middle,
        );
        // the bends of a polyline: thin dashed verticals at their distances
        let tr = &self.transect;
        for &d in tr.vdist.iter().skip(1).take(tr.vdist.len().saturating_sub(2)) {
            let (a, b) = (px(d, 0.0), px(d, t_n as f64));
            let mut y = a.y.min(b.y);
            while y < a.y.max(b.y) {
                let e = (y + 4.0).min(a.y.max(b.y));
                painter
                    .line_segment([Pos2::new(a.x, y), Pos2::new(a.x, e)], Stroke::new(1.0, LINE.gamma_multiply(0.8)));
                y += 8.0;
            }
        }
        if let Some(d) = cursor {
            let (a, b) = (px(d, 0.0), px(d, t_n as f64));
            painter.line_segment([a, b], Stroke::new(3.0, Color32::from_black_alpha(200)));
            painter.line_segment([a, b], Stroke::new(1.0, Color32::from_white_alpha(230)));
        }
        // the hovered cell: outlined, its values in a tooltip, marked on the map; a click goes to its date
        self.transect.hover_i = None;
        let hovered = response.inner.filter(|_| response.response.hovered()).and_then(|m| {
            let (i, t) = ((m.x / sp).round(), m.y.floor());
            (i >= 0.0 && (i as usize) < n && t >= 0.0 && (t as usize) < t_n).then_some((i as usize, t as usize))
        });
        if let Some((i, t)) = hovered {
            self.transect.hover_i = Some(i);
            let tr = &self.transect;
            painter.rect_stroke(
                Rect::from_two_pos(
                    px(i as f64 * sp - sp / 2.0, t as f64),
                    px(i as f64 * sp + sp / 2.0, t as f64 + 1.0),
                ),
                0.0,
                Stroke::new(1.5, Color32::WHITE),
                egui::StrokeKind::Middle,
            );
            let v = tr.values[t * n + i];
            let mut lines = vec![format!("{}  |  {}", format_distance(i as f64 * sp, &tr.unit), info.layers[t].label)];
            lines.push(if classes_on {
                format!("class {}", self.classes.name_of(v))
            } else if tr.kind == 1 {
                format!(
                    "value {}  (anomaly {})",
                    format_g(f64::from(v), 6),
                    format_g(f64::from(tr.shown[t * n + i]), 4)
                )
            } else {
                format!("value {}", format_g(f64::from(v), 6))
            });
            let where_ = if tr.px[i] < 0 {
                "off the layer".to_string()
            } else {
                let state = match tr.state[t] {
                    2 => "",
                    1 => "  (approx., overview)",
                    _ => "  (not loaded)",
                };
                format!("col {}  row {}{state}", tr.px[i], tr.py[i])
            };
            response.response.clone().on_hover_ui_at_pointer(|ui| {
                for l in &lines {
                    ui.label(l);
                }
                ui.weak(&where_);
            });
            if response.response.clicked() {
                self.set_date(t);
            }
        }
    }

    /// One row per date, one column per sample (header: the distance from A).
    pub fn transect_csv(&self) -> String {
        let Some(s) = &self.session else { return String::new() };
        let tr = &self.transect;
        let sp = tr.length / (tr.n - 1) as f64;
        let mut csv = String::from("date");
        for i in 0..tr.n {
            csv.push(',');
            csv += &format_g(i as f64 * sp, 7);
        }
        csv.push('\n');
        for t in 0..tr.t_n {
            csv += &s.info.layers[t].label;
            for i in 0..tr.n {
                csv.push(',');
                let v = tr.shown[t * tr.n + i];
                if !v.is_nan() {
                    csv += &format_g(f64::from(v), 7);
                }
            }
            csv.push('\n');
        }
        csv
    }
}
