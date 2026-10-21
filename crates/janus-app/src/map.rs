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
    /// the map's size on the screen (points), last frame
    pub canvas: [f32; 2],
    texture: Option<egui::TextureId>,
    /// what the target holds: the mode, dates, range, colormap, the image's place and the tiles on the GPU
    drawn: Option<DrawKey>,
    pub dirty: bool,
}

type DrawKey = (Mode, [i32; 3], [u32; 2], usize, [u32; 4], usize);

/// The map's background where there is no data (the C++'s, app.cpp renderMap).
const MAP_BACKGROUND: [f32; 4] = [0.10, 0.10, 0.115, 1.0];

/// A pixel under the cursor or pinned: of the overview and of the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelAt {
    pub ov: (usize, usize),
    pub src: (usize, usize),
}

pub struct Pin {
    pub at: PixelAt,
    pub color: Color32,
}

impl MapView {
    pub fn fit(&mut self) {
        self.fit_pending = true;
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
        // up to 32 screen points per source pixel; down to the whole image
        let max_scale = 32.0 * ow / w;
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
        if resp.dragged_by(egui::PointerButton::Primary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            m.center[0] -= f64::from(d.x) / m.scale;
            m.center[1] -= f64::from(d.y) / m.scale;
            m.anchor = None;
        }
        // the image's centre stays on it
        m.center = [m.center[0].clamp(0.0, ow), m.center[1].clamp(0.0, oh)];

        // the pixel under the cursor
        let pixel_at = |m: &MapView, p: Pos2| -> Option<PixelAt> {
            let [ix, iy] = image_at(m, p);
            (ix >= 0.0 && iy >= 0.0 && ix < ow && iy < oh).then(|| PixelAt {
                ov: (ix as usize, iy as usize),
                src: (((ix * w / ow) as usize).min(w as usize - 1), ((iy * h / oh) as usize).min(h as usize - 1)),
            })
        };
        let hover = resp.hover_pos().and_then(|p| pixel_at(m, p));
        if hover != self.hover {
            self.hover = hover;
            if let Some(hv) = hover {
                s.request_hover(hv.src.0, hv.src.1);
            }
        }
        if resp.clicked()
            && let Some(hv) = hover
        {
            let color = theme::pin_color(self.pins.len());
            self.pins.push(Pin { at: hv, color });
            s.request_pin(hv.src.0, hv.src.1);
        }
        let m = &mut self.map;
        let screen_of = |m: &MapView, ix: f64, iy: f64| -> Pos2 {
            Pos2::new(
                rect.center().x + ((ix - m.center[0]) * m.scale) as f32,
                rect.center().y + ((iy - m.center[1]) * m.scale) as f32,
            )
        };
        if resp.secondary_clicked()
            && let Some(p) = resp.interact_pointer_pos()
        {
            // the nearest pin within 8 points
            let near = self.pins.iter().enumerate().min_by(|a, b| {
                let d = |pin: &Pin| screen_of(m, pin.at.ov.0 as f64 + 0.5, pin.at.ov.1 as f64 + 0.5).distance(p);
                d(a.1).total_cmp(&d(b.1))
            });
            if let Some((i, pin)) = near
                && screen_of(m, pin.at.ov.0 as f64 + 0.5, pin.at.ov.1 as f64 + 0.5).distance(p) < 8.0
            {
                self.pins.remove(i);
            }
        }

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
        let cmap = self.display.cmap[params.mode as usize];
        let mode = params.mode;
        let t = self.display.t;
        let ready = match mode {
            Mode::Rgb => params.layers.iter().all(|&l| s.uploaded[l as usize]),
            Mode::Diff => s.uploaded[t] && s.uploaded[params.layers[1] as usize],
            Mode::Composite => true,
            m if m.needs_stats() => s.stats.is_some(),
            _ => s.uploaded[t],
        };
        // the detail tiles of the view: the visible part in source pixels, at screen pixels per source pixel
        let (v0, v1) = (image_at(m, rect.min), image_at(m, rect.max));
        let view = ViewRect {
            x0: v0[0] * fx,
            y0: v0[1] * fy,
            x1: v1[0] * fx,
            y1: v1[1] * fy,
            scale: m.scale * f64::from(ppp) / fx,
        };
        let detail = matches!(mode, Mode::Value | Mode::Diff) && !s.s.defer_random_reads();
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
        );
        if m.dirty || m.drawn.as_ref() != Some(&key) {
            let mut f = gpu.begin_map(0, size, MAP_BACKGROUND);
            if let (true, Some(cube)) = (ready, &s.cube) {
                gpu.draw_cube(&mut f, cube, to_px(m, 0.0, 0.0, ow, oh), &params, cmap, 1.0);
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
                        let r = to_px(m, tile.x / fx, tile.y / fy, (tile.x + tile.sw) / fx, (tile.y + tile.sh) / fy);
                        gpu.draw_tile(&mut f, cube, &tile.tex, r, &params, cmap, 1.0);
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

        // the cursor's source pixel when large enough to see, and the pins
        let src_scale = m.scale * ow / w;
        if let Some(hv) = self.hover
            && src_scale >= 6.0
        {
            let a = screen_of(m, hv.src.0 as f64 * ow / w, hv.src.1 as f64 * oh / h);
            let b = screen_of(m, (hv.src.0 + 1) as f64 * ow / w, (hv.src.1 + 1) as f64 * oh / h);
            painter.rect_stroke(
                Rect::from_min_max(a, b),
                0.0,
                Stroke::new(1.0, theme::cursor_series()),
                egui::StrokeKind::Outside,
            );
        }
        for (i, pin) in self.pins.iter().enumerate() {
            let c = screen_of(m, (pin.at.src.0 as f64 + 0.5) * ow / w, (pin.at.src.1 as f64 + 0.5) * oh / h);
            painter.circle_stroke(c, 5.0, Stroke::new(3.0, Color32::BLACK));
            painter.circle_stroke(c, 5.0, Stroke::new(1.5, pin.color));
            painter.text(
                c + vec2(7.0, -7.0),
                Align2::LEFT_BOTTOM,
                format!("{}", i + 1),
                FontId::proportional(13.0),
                pin.color,
            );
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
        let mut status = format!("{}  {}", s.info.layers[self.display.t].label, crate::display::mode_name(mode));
        if let Some(hv) = self.hover {
            let v = ov.layer(self.display.t).map_or(f32::NAN, |l| l[hv.ov.1 * ov.w + hv.ov.0]);
            status += &format!("   pixel ({}, {})  {v:.4}", hv.src.0, hv.src.1);
        }
        shadow_text(&painter, rect.left_bottom() + vec2(8.0, -8.0), Align2::LEFT_BOTTOM, &status);
    }
}

/// Text readable over any map: a dark shadow under it.
fn shadow_text(painter: &egui::Painter, at: Pos2, align: Align2, text: &str) {
    let font = FontId::proportional(13.0);
    painter.text(at + vec2(1.0, 1.0), align, text, font.clone(), Color32::from_black_alpha(220));
    painter.text(at, align, text, font, Color32::WHITE);
}
