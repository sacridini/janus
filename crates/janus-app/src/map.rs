//! The map panel (src/app.cpp: drawMap, mapInput, drawMapMarks): the cube drawn by the GPU into the panel's
//! target, shown as an image; the wheel zooms (smoothed, as the C++), a drag moves, a click drops a pin, the right
//! button removes one; the cursor's pixel, the pins, the colour bar and the status are drawn over it.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, vec2};
use janus_gpu::Mode;
use janus_session::ViewRect;

use crate::app::State;
use crate::theme;

/// The C++'s wheel zoom: an exponential approach with this time constant (s).
const ZOOM_TAU: f64 = 0.06;

#[derive(Default)]
pub struct MapView {
    /// the overview pixel at the panel's centre
    pub center: [f64; 2],
    /// screen points per overview pixel
    pub scale: f64,
    target_scale: f64,
    /// where the wheel was turned: the screen point and the image point under it, kept together while zooming
    anchor: Option<(Pos2, [f64; 2])>,
    pub fit_pending: bool,
    /// moved or zoomed since the last fit (a layer made active keeps the view only then)
    pub touched: bool,
    /// the map's size on the screen (points), last frame
    pub canvas: [f32; 2],
    texture: Option<egui::TextureId>,
    /// what the target holds: the mode, dates, range, colormap, the image's place and the tiles on the GPU
    drawn: Option<DrawKey>,
    pub dirty: bool,
    /// the view last drawn, in source pixels at target pixels per source pixel (what an RGB composite reads)
    pub last_view: Option<ViewRect>,
}

type DrawKey = (Mode, [i32; 3], [u32; 2], usize, [u32; 4], usize, Option<usize>);

/// The map's background where there is no data (the C++'s, app.cpp renderMap).
pub(crate) const MAP_BACKGROUND: [f32; 4] = [0.10, 0.10, 0.115, 1.0];

impl MapView {
    pub fn fit(&mut self) {
        self.fit_pending = true;
        self.touched = false;
    }

    /// The map centred on an overview pixel at `scale` screen points per overview pixel.
    pub fn set_view(&mut self, center: [f64; 2], scale: f64) {
        self.center = center;
        self.scale = scale;
        self.target_scale = scale;
        self.anchor = None;
        self.fit_pending = false;
        self.dirty = true;
    }
}

impl State {
    pub fn map_ui(&mut self, ui: &mut Ui) {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let Some(s) = &self.session else {
            let msg = match (&self.opening, &self.error) {
                (Some(o), _) => format!("Opening {}...", o.inputs.join(", ")),
                (_, Some(e)) => e.clone(),
                _ => "Open a series: File > Open folder, or drop it here".to_string(),
            };
            painter.text(rect.center(), Align2::CENTER_CENTER, msg, FontId::proportional(13.0), theme::weak_text());
            return;
        };
        let (ow, oh) = (s.s.overview.w as f64, s.s.overview.h as f64);
        let (w, h) = (s.info.width as f64, s.info.height as f64);
        // the whole image with a margin, as the C++'s fitView
        let fit = 0.98 * (f64::from(rect.width()) / ow).min(f64::from(rect.height()) / oh);
        let m = &mut self.map;
        m.canvas = [rect.width(), rect.height()];
        if m.fit_pending || m.scale <= 0.0 {
            m.center = [ow / 2.0, oh / 2.0];
            m.scale = fit;
            m.target_scale = fit;
            m.anchor = None;
            m.fit_pending = false;
        }
        // up to 64 screen points per source pixel; down to the whole image
        let max_scale = 64.0 * ow / w;
        let image_at = |m: &MapView, p: Pos2| -> [f64; 2] {
            [
                m.center[0] + f64::from(p.x - rect.center().x) / m.scale,
                m.center[1] + f64::from(p.y - rect.center().y) / m.scale,
            ]
        };

        // input: the wheel (and a trackpad's pinch) zooms about the cursor, a drag moves
        if resp.hovered() {
            // the wheel's own steps (in points), smoothed here as the C++ smooths them
            let (scroll, pinch) = ui.input(|i| {
                let scroll: f32 = i
                    .raw
                    .events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::MouseWheel { unit, delta, modifiers, .. }
                            if !modifiers.ctrl && !modifiers.command =>
                        {
                            Some(match unit {
                                egui::MouseWheelUnit::Point => delta.y,
                                egui::MouseWheelUnit::Line => delta.y * 50.0,
                                egui::MouseWheelUnit::Page => delta.y * rect.height(),
                            })
                        }
                        _ => None,
                    })
                    .sum();
                (scroll, i.zoom_delta())
            });
            if scroll != 0.0 || pinch != 1.0 {
                let factor = 1.25f64.powf(f64::from(scroll) / 50.0) * f64::from(pinch);
                m.touched = true;
                m.target_scale = (m.target_scale * factor).clamp(fit.min(max_scale), max_scale.max(fit));
                if let Some(p) = resp.hover_pos() {
                    m.anchor = Some((p, image_at(m, p)));
                }
            }
        }
        let dt = f64::from(ui.input(|i| i.stable_dt)).min(0.1);
        if (m.target_scale / m.scale).ln().abs() > 1e-4 {
            let k = 1.0 - (-dt / ZOOM_TAU).exp();
            m.scale *= (m.target_scale / m.scale).powf(k);
            if let Some((sp, ip)) = m.anchor {
                m.center = [
                    ip[0] - f64::from(sp.x - rect.center().x) / m.scale,
                    ip[1] - f64::from(sp.y - rect.center().y) / m.scale,
                ];
            }
            ui.ctx().request_repaint();
        } else if m.scale != m.target_scale {
            m.scale = m.target_scale;
            if m.scale <= fit * 1.0001 {
                // zoomed out to the whole image: it ends centred
                m.center = [ow / 2.0, oh / 2.0];
            }
        }
        // the source pixel under the pointer (the active layer's)
        let (fx, fy) = (w / ow, h / oh);
        let src_at = |m: &MapView, p: Pos2| -> [f64; 2] {
            let [ix, iy] = image_at(m, p);
            [ix * fx, iy * fy]
        };
        let shift = ui.input(|i| i.modifiers.shift);
        // Shift + drag: an ROI rectangle (no pan, no pin)
        if resp.drag_started_by(egui::PointerButton::Primary)
            && shift
            && let Some(p) = resp.interact_pointer_pos()
        {
            let a = src_at(m, p);
            self.series.roi_drag = Some((a, a));
        }
        if let Some((a, _)) = self.series.roi_drag {
            if let Some(p) = resp.interact_pointer_pos() {
                self.series.roi_drag = Some((a, src_at(m, p)));
            }
            if resp.drag_stopped() || !resp.is_pointer_button_down_on() {
                let (a, b) = self.series.roi_drag.take().unwrap();
                if (b[0] - a[0]).abs() >= 1.0 && (b[1] - a[1]).abs() >= 1.0 {
                    let r = [
                        a[0].min(b[0]).floor() as i64,
                        a[1].min(b[1]).floor() as i64,
                        a[0].max(b[0]).ceil() as i64,
                        a[1].max(b[1]).ceil() as i64,
                    ];
                    self.set_roi_rect(r);
                }
            }
            ui.ctx().request_repaint();
        } else if resp.dragged_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            m.touched |= d != egui::Vec2::ZERO;
            m.center[0] -= f64::from(d.x) / m.scale;
            m.center[1] -= f64::from(d.y) / m.scale;
            m.anchor = None;
        }
        let m = &mut self.map;
        // the image's centre stays on it
        m.center = [m.center[0].clamp(0.0, ow), m.center[1].clamp(0.0, oh)];

        // the pixel under the cursor
        let pointer = resp.hover_pos().map(|p| src_at(m, p));
        let inside =
            pointer.filter(|[x, y]| *x >= 0.0 && *y >= 0.0 && *x < w && *y < h).map(|[x, y]| (x as usize, y as usize));
        let screen_of = |m: &MapView, ix: f64, iy: f64| -> Pos2 {
            Pos2::new(
                rect.center().x + ((ix - m.center[0]) * m.scale) as f32,
                rect.center().y + ((iy - m.center[1]) * m.scale) as f32,
            )
        };
        // a source pixel's point on the screen
        let src_screen = |m: &MapView, x: f64, y: f64| screen_of(m, x / fx, y / fy);
        let m_snapshot = (m.center, m.scale);
        if inside.is_some() && !self.opts.scripted() {
            self.set_hover(inside);
        }
        // a click drops a pin; the right button removes the one under the cursor; Delete the last one
        if resp.clicked()
            && !shift
            && let Some((x, y)) = inside
        {
            self.add_pin(x, y);
        }
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            let mv = MapView { center: m_snapshot.0, scale: m_snapshot.1, ..Default::default() };
            let near = self
                .series
                .pins
                .iter()
                .map(|pin| (pin.id, src_screen(&mv, pin.x as f64 + 0.5, pin.y as f64 + 0.5).distance(p)))
                .filter(|(_, d)| *d < 12.0)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((id, _)) = near {
                self.remove_pin(id);
            }
        }
        if resp.hovered()
            && ui.input(|i| i.key_pressed(egui::Key::Delete) || i.key_pressed(egui::Key::Backspace))
            && let Some(id) = self.series.pins.last().map(|p| p.id)
        {
            self.remove_pin(id);
        }
        let m = &mut self.map;

        // the cube drawn into the panel's target, at the screen's pixels, the detail tiles over it
        let ppp = ui.ctx().pixels_per_point();
        let size = [(rect.width() * ppp).round() as u32, (rect.height() * ppp).round() as u32];
        let Some(gpu) = self.gpu.as_mut() else { return };
        if gpu.ensure_target(0, size) {
            let t = gpu.target(0).unwrap();
            let rs = &self.render_state;
            let mut r = rs.renderer.write();
            match m.texture {
                Some(id) => r.update_egui_texture_from_wgpu_texture(&rs.device, &t.view, wgpu::FilterMode::Nearest, id),
                None => m.texture = Some(r.register_native_texture(&rs.device, &t.view, wgpu::FilterMode::Nearest)),
            }
            m.dirty = true;
        }
        let Some(s) = self.session.as_mut() else { return };
        let (fx, fy) = (w / ow, h / oh); // source pixels per overview pixel
        let to_px = |m: &MapView, x0: f64, y0: f64, x1: f64, y1: f64| -> [f32; 4] {
            let (a, b) = (screen_of(m, x0, y0), screen_of(m, x1, y1));
            [(a.x - rect.min.x) * ppp, (a.y - rect.min.y) * ppp, (b.x - rect.min.x) * ppp, (b.y - rect.min.y) * ppp]
        };
        let mut params = self.display.layer_params(s);
        if self.classes.on() && params.mode == Mode::Value {
            params.class_lut = self.classes.lut.clone();
        }
        let cmap = self.display.cmap[params.mode as usize];
        let mode = params.mode;
        let t = self.display.t;
        let ready = self.display.ready(s, &params);
        // the detail tiles of the view: the visible part in source pixels, at screen pixels per source pixel
        let (v0, v1) = (image_at(m, rect.min), image_at(m, rect.max));
        let view = ViewRect {
            x0: v0[0] * fx,
            y0: v0[1] * fy,
            x1: v1[0] * fx,
            y1: v1[1] * fy,
            scale: m.scale * f64::from(ppp) / fx,
        };
        m.last_view = Some(view);
        // an RGB composite reads its own detail
        let composite = self.composite.as_ref().is_some_and(|c| c.on);
        let detail = matches!(mode, Mode::Value | Mode::Diff) && !s.s.defer_random_reads() && !composite;
        let prefetch = self.display.playing.then(|| (t + 1) % s.info.t());
        if detail {
            s.tiles.update(s.s.fg(), t, &view, prefetch);
            if mode == Mode::Diff {
                s.tiles.update(s.s.fg(), params.layers[1] as usize, &view, None);
            }
        }
        let key = (
            mode,
            params.layers,
            params.range.map(f32::to_bits),
            cmap,
            to_px(m, 0.0, 0.0, ow, oh).map(f32::to_bits),
            s.tiles.gpu_tiles(),
            params.class_lut.as_ref().map(|l| std::sync::Arc::as_ptr(l) as usize),
        );
        if m.dirty || m.drawn.as_ref() != Some(&key) {
            let mut f = gpu.begin_map(0, size, MAP_BACKGROUND);
            // the basemap under the layers, in map space (the active layer's source pixels)
            self.basemap
                .draw(gpu, &mut f, &view, MAP_BACKGROUND, |q| to_px(m, q[0] / fx, q[1] / fy, q[2] / fx, q[3] / fy));
            let to_map = |q: [f64; 4]| to_px(m, q[0] / fx, q[1] / fy, q[2] / fx, q[3] / fy);
            // every layer, bottom to top; the active one with its detail
            for li in 0..self.layers.len() {
                let l = &mut self.layers[li];
                if !l.visible || !l.place.aligned {
                    continue;
                }
                if li != self.active {
                    State::draw_layer(l, gpu, &mut f, &to_map);
                    continue;
                }
                let alpha = l.opacity;
                if let Some(c) = self.composite.as_mut().filter(|c| c.on) {
                    // three bands as RGB, in detail when zoomed in
                    State::draw_composite(c, gpu, &mut f, t, s.info.t(), alpha, |w| {
                        to_px(m, w[0] / fx, w[1] / fy, w[2] / fx, w[3] / fy)
                    });
                } else if let (true, Some(cube)) = (ready, &s.cube) {
                    gpu.draw_cube(&mut f, cube, to_px(m, 0.0, 0.0, ow, oh), &params, cmap, alpha);
                    if detail {
                        let tiles: Vec<_> = if mode == Mode::Diff {
                            s.tiles
                                .visible_pairs(t, params.layers[1] as usize, &view)
                                .into_iter()
                                .map(|(a, b)| (a, Some(b)))
                                .collect()
                        } else {
                            s.tiles.visible(t, &view).into_iter().map(|a| (a, None)).collect()
                        };
                        for (tile, reference) in tiles {
                            params.tile2 = reference.map(|r| r.tex.clone());
                            let r =
                                to_px(m, tile.x / fx, tile.y / fy, (tile.x + tile.sw) / fx, (tile.y + tile.sh) / fy);
                            gpu.draw_tile(&mut f, cube, &tile.tex, r, &params, cmap, alpha);
                        }
                    }
                }
            }
            gpu.end_map(f);
            m.drawn = Some(key);
            m.dirty = false;
        }
        let s = self.session.as_ref().unwrap();
        if let Some(tex) = m.texture {
            painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }

        // the outline of the pixel under the cursor (when large enough), the pins, the ROI
        let src_scale = m.scale * ow / w;
        let at = |x: f64, y: f64| screen_of(m, x / fx, y / fy);
        if let Some((x, y)) = inside
            && src_scale >= 6.0
        {
            let r = Rect::from_min_max(at(x as f64, y as f64), at(x as f64 + 1.0, y as f64 + 1.0));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_white_alpha(200)), egui::StrokeKind::Middle);
        }
        for pin in &self.series.pins {
            // a dark ring and a white one: visible over any colormap
            let c = at(pin.x as f64 + 0.5, pin.y as f64 + 0.5);
            painter.circle_stroke(c, 8.5, Stroke::new(3.0, Color32::from_black_alpha(220)));
            painter.circle_filled(c, 6.5, pin.color);
            painter.circle_stroke(c, 6.5, Stroke::new(1.5, Color32::WHITE));
            let font = FontId::proportional(13.0);
            let galley = painter.layout_no_wrap(pin.id.to_string(), font, pin.color);
            let tp = c + vec2(11.0, -18.0);
            painter.rect_filled(
                Rect::from_min_max(tp - vec2(3.0, 1.0), tp + galley.size() + vec2(3.0, 1.0)),
                3.0,
                Color32::from_black_alpha(190),
            );
            painter.galley(tp, galley, pin.color);
        }
        let roi = crate::series::ROI_COLOR;
        let outline = |r: [usize; 4], thick: f32| {
            let rr = Rect::from_min_max(at(r[0] as f64, r[1] as f64), at(r[2] as f64, r[3] as f64));
            painter.rect_stroke(
                rr,
                0.0,
                Stroke::new(thick + 2.0, Color32::from_black_alpha(160)),
                egui::StrokeKind::Middle,
            );
            painter.rect_stroke(rr, 0.0, Stroke::new(thick, roi), egui::StrokeKind::Middle);
        };
        let sr = &self.series;
        if let Some((a, b)) = sr.roi_drag {
            let rr = Rect::from_two_pos(at(a[0], a[1]), at(b[0], b[1]));
            painter.rect_stroke(rr, 0.0, Stroke::new(2.0, roi), egui::StrokeKind::Middle);
        } else if let Some(r) = sr.roi_rect {
            if sr.roi_pending {
                outline(r, 1.0);
                painter.text(
                    at(r[0] as f64, r[1] as f64) + vec2(4.0, 2.0),
                    Align2::LEFT_TOP,
                    "ROI: waiting for the overview (HDD)",
                    FontId::proportional(13.0),
                    roi,
                );
            } else if let Some(d) = &sr.roi.data {
                outline(r, 2.0);
                let (done, n) = (d.done.load(std::sync::atomic::Ordering::Relaxed), d.per_t.len());
                if done < n {
                    painter.text(
                        at(d.x0 as f64, d.y0 as f64) + vec2(4.0, 2.0),
                        Align2::LEFT_TOP,
                        format!("ROI {done}/{n}"),
                        FontId::proportional(13.0),
                        roi,
                    );
                }
            }
        }

        // loading, the colour bar and the status at the bottom
        let ov = &s.s.overview;
        if !ov.complete() {
            let f = ov.layers_done() as f32 / ov.t.max(1) as f32;
            let bar = Rect::from_min_size(rect.left_top() + vec2(8.0, 8.0), vec2(200.0, 4.0));
            painter.rect_filled(bar, 0.0, Color32::from_black_alpha(160));
            painter.rect_filled(
                Rect::from_min_size(bar.min, vec2(200.0 * f, 4.0)),
                0.0,
                Color32::from_rgb(230, 179, 0),
            );
            let what = if ov.from_cache() { "from its cache" } else { "reading" };
            shadow_text(
                &painter,
                bar.left_bottom() + vec2(0.0, 4.0),
                Align2::LEFT_TOP,
                &format!("overview {}/{} ({what})", ov.layers_done(), ov.t),
            );
        }
        let mode = self.display.mode;
        let range = self.display.ranges[mode as usize];
        if mode != janus_gpu::Mode::Rgb {
            let cm = &janus_gpu::colormaps::all()[self.display.cmap[mode as usize]];
            let bar = Rect::from_min_size(rect.left_bottom() + vec2(8.0, -40.0), vec2(160.0, 10.0));
            for i in 0..64 {
                let c = cm.sample(i as f32 / 63.0);
                let x0 = bar.min.x + bar.width() * i as f32 / 64.0;
                let r =
                    Rect::from_min_max(Pos2::new(x0, bar.min.y), Pos2::new(x0 + bar.width() / 64.0 + 0.5, bar.max.y));
                painter.rect_filled(r, 0.0, Color32::from_rgb(c[0], c[1], c[2]));
            }
            shadow_text(&painter, bar.left_top() - vec2(0.0, 2.0), Align2::LEFT_BOTTOM, &format!("{:.4}", range.lo));
            shadow_text(&painter, bar.right_top() - vec2(0.0, 2.0), Align2::RIGHT_BOTTOM, &format!("{:.4}", range.hi));
        }
        let composite = self.composite.as_ref().filter(|c| c.on);
        let mut status = match composite {
            Some(c) => c.title(&s.info, self.display.t),
            None => self.map_title(),
        };
        if let Some((x, y)) = inside {
            let (ox, oy) = ((x * ov.w / s.info.width).min(ov.w - 1), (y * ov.h / s.info.height).min(ov.h - 1));
            let v = ov.layer(self.display.t).map_or(f32::NAN, |l| l[oy * ov.w + ox]);
            status += &format!("   pixel ({x}, {y})  {v:.4}");
            if let Some(c) = composite {
                status += &c.status_at(x as f64 + 0.5, y as f64 + 0.5, self.display.t, s.info.t());
            }
        }
        shadow_text(&painter, rect.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, &status);
        self.basemap_note(&painter, rect);
    }
}

impl State {
    /// What the map shows (app.cpp, the map's title): the dates of the RGB or the difference, the composite's
    /// window, the date and the mode, or the mode over the dates (and the season).
    fn map_title(&self) -> String {
        let (Some(s), d) = (&self.session, &self.display) else { return String::new() };
        let info = &s.info;
        let label = |t: usize| info.layers[t.min(info.t() - 1)].label.as_str();
        match d.mode {
            Mode::Rgb => format!("RGB: {} / {} / {}", label(d.rgb[0]), label(d.rgb[1]), label(d.rgb[2])),
            Mode::Diff => format!("{} - {}  |  Difference", label(d.t), label(d.diff_ref)),
            Mode::Composite => d.temporal_text(info, d.t),
            m if crate::display::time_dependent(m) => format!("{}  |  {}", label(d.t), crate::display::mode_name(m)),
            m => format!(
                "{}  |  {} {}{}",
                crate::display::mode_name(m),
                info.t(),
                if info.spectral { "bands" } else { "dates" },
                if d.season.active(info) { format!("  |  {}", d.season.text()) } else { String::new() }
            ),
        }
    }
}

/// Text readable over any map: a dark shadow under it.
fn shadow_text(painter: &egui::Painter, at: Pos2, align: Align2, text: &str) {
    let font = FontId::proportional(13.0);
    painter.text(at + vec2(1.0, 1.0), align, text, font.clone(), Color32::from_black_alpha(220));
    painter.text(at, align, text, font, Color32::WHITE);
}
