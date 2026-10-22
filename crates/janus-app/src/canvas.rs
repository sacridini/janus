//! An RGBA8 image (straight alpha, rows top-down) with the few marks a figure needs, drawn like the map draws them
//! (src/app_export.cpp: Canvas, drawLegends, drawCredit, drawScaleBar). Text is rasterized from ProggyClean (the
//! interface's font) or ProggyForever (its scalable companion, the animations' date stamp) at the export's size,
//! so it stays sharp at 2x and 4x; anywhere, not only on the main thread.
//!
//! ProggyForever-Regular-minimal.ttf: MIT license, Copyright (c) 2026 Disco Hello, Copyright (c) 2019, 2023 Tristan
//! Grimmer (ImGui's AddFontDefaultVector).

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

/// ImGui's font size the figures use (whatever the interface's): 13 px at 1x.
pub const FIGURE_FONT_SIZE: f32 = 13.0;

/// The bitmap font (labels, legends) or the scalable one (the date stamp).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Face {
    Bitmap,
    Vector,
}

fn font(face: Face) -> &'static FontRef<'static> {
    static BITMAP: OnceLock<FontRef<'static>> = OnceLock::new();
    static VECTOR: OnceLock<FontRef<'static>> = OnceLock::new();
    match face {
        Face::Bitmap => BITMAP
            .get_or_init(|| FontRef::try_from_slice(include_bytes!("../assets/ProggyClean.ttf")).expect("ProggyClean")),
        Face::Vector => VECTOR.get_or_init(|| {
            FontRef::try_from_slice(include_bytes!("../assets/ProggyForever-Regular-minimal.ttf"))
                .expect("ProggyForever")
        }),
    }
}

/// A colour as RGBA bytes.
pub type Rgba = [u8; 4];

pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Rgba {
    [r, g, b, a]
}

pub struct Canvas<'a> {
    pub w: i32,
    pub h: i32,
    pub px: &'a mut [u8],
}

impl Canvas<'_> {
    /// Colour `col` over pixel (x, y), with coverage `cov` (straight alpha, as the C++).
    pub fn blend(&mut self, x: i32, y: i32, col: Rgba, cov: f32) {
        if x < 0 || y < 0 || x >= self.w || y >= self.h || cov <= 0.0 {
            return;
        }
        let sa = f32::from(col[3]) / 255.0 * cov.min(1.0);
        let i = ((y as usize) * self.w as usize + x as usize) * 4;
        let d = &mut self.px[i..i + 4];
        let da = f32::from(d[3]) / 255.0;
        let oa = sa + da * (1.0 - sa);
        if oa <= 0.0 {
            return;
        }
        for c in 0..3 {
            let s = f32::from(col[c]);
            d[c] = ((s * sa + f32::from(d[c]) * da * (1.0 - sa)) / oa).round() as u8;
        }
        d[3] = (oa * 255.0).round() as u8;
    }

    /// Pixels covered by the rectangle (partly at fractional edges).
    pub fn fill_rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, col: Rgba) {
        for y in y0.floor() as i32..y1.ceil() as i32 {
            let cy = y1.min(y as f32 + 1.0) - y0.max(y as f32);
            for x in x0.floor() as i32..x1.ceil() as i32 {
                self.blend(x, y, col, cy * (x1.min(x as f32 + 1.0) - x0.max(x as f32)));
            }
        }
    }

    pub fn rect(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, col: Rgba, t: f32) {
        self.fill_rect(x0 - t / 2.0, y0 - t / 2.0, x1 + t / 2.0, y0 + t / 2.0, col);
        self.fill_rect(x0 - t / 2.0, y1 - t / 2.0, x1 + t / 2.0, y1 + t / 2.0, col);
        self.fill_rect(x0 - t / 2.0, y0 + t / 2.0, x0 + t / 2.0, y1 - t / 2.0, col);
        self.fill_rect(x1 - t / 2.0, y0 + t / 2.0, x1 + t / 2.0, y1 - t / 2.0, col);
    }

    /// A segment of thickness t, antialiased (coverage from the distance to it).
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, col: Rgba, t: f32) {
        let (ext, dx, dy) = (t / 2.0 + 1.0, x1 - x0, y1 - y0);
        let len2 = dx * dx + dy * dy;
        let (xa, xb) = ((x0.min(x1) - ext).floor() as i32, (x0.max(x1) + ext).ceil() as i32);
        let (ya, yb) = ((y0.min(y1) - ext).floor() as i32, (y0.max(y1) + ext).ceil() as i32);
        for y in ya.max(0)..=yb.min(self.h - 1) {
            for x in xa.max(0)..=xb.min(self.w - 1) {
                let (px, py) = (x as f32 + 0.5 - x0, y as f32 + 0.5 - y0);
                let k = if len2 > 0.0 { ((px * dx + py * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
                let d = (px - k * dx).hypot(py - k * dy);
                self.blend(x, y, col, (t / 2.0 + 0.5 - d).clamp(0.0, 1.0));
            }
        }
    }

    /// An outline through pts (x, y pairs); joints drawn twice look the same at these widths.
    pub fn polyline(&mut self, pts: &[f32], col: Rgba, t: f32, closed: bool) {
        let n = pts.len() / 2;
        let segs = if closed { n } else { n.saturating_sub(1) };
        for i in 0..segs {
            let j = (i + 1) % n;
            self.line(pts[i * 2], pts[i * 2 + 1], pts[j * 2], pts[j * 2 + 1], col, t);
        }
    }

    /// A disc (t = 0) or a ring of radius r, antialiased.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, col: Rgba, t: f32) {
        let ext = r + t / 2.0 + 1.0;
        for y in (cy - ext).floor() as i32..=(cy + ext).ceil() as i32 {
            for x in (cx - ext).floor() as i32..=(cx + ext).ceil() as i32 {
                let d = (x as f32 + 0.5 - cx).hypot(y as f32 + 0.5 - cy);
                let cov = if t > 0.0 { t / 2.0 + 0.5 - (d - r).abs() } else { r + 0.5 - d };
                self.blend(x, y, col, cov.clamp(0.0, 1.0));
            }
        }
    }

    /// The width of `s` at `size` pixels.
    pub fn text_width(s: &str, size: f32, face: Face) -> f32 {
        let f = font(face).as_scaled(PxScale::from(size));
        s.chars().map(|c| f.h_advance(f.glyph_id(c))).sum()
    }

    /// `s` with its line's top left at (x, y), `size` pixels high.
    pub fn text(&mut self, x: f32, y: f32, s: &str, size: f32, col: Rgba, face: Face) {
        let f = font(face);
        let sf = f.as_scaled(PxScale::from(size));
        // the baseline on a whole pixel, as ImGui snaps its glyphs
        let base = (y + sf.ascent()).round();
        let mut pen = x;
        for c in s.chars() {
            let id = sf.glyph_id(c);
            let g = id.with_scale_and_position(PxScale::from(size), ab_glyph::point(pen.round(), base));
            if let Some(o) = f.outline_glyph(g) {
                let b = o.px_bounds();
                o.draw(|gx, gy, cov| self.blend(b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32, col, cov));
            }
            pen += sf.h_advance(id);
        }
    }
}

/// A legend block: a caption and a colour bar (lo..hi) or a list of classes.
pub struct Legend {
    pub caption: String,
    /// colour bar
    pub cmap: Option<usize>,
    pub lo: f32,
    pub hi: f32,
    pub classes: Vec<(Rgba, String)>,
}

/// printf's %.4g.
fn g4(v: f32) -> String {
    janus_validation::fmt_g(f64::from(v), 4)
}

/// Legends from the bottom left corner up, each on a dark box (readable over any map and on a white background).
/// Sizes in points x ps.
pub fn draw_legends(cv: &mut Canvas, legends: &[Legend], ps: f32) {
    let fs = FIGURE_FONT_SIZE * ps;
    let (line, pad) = (fs + 3.0 * ps, 6.0 * ps);
    let (bx, fg) = (rgba(0, 0, 0, 165), rgba(235, 235, 235, 255));
    let mut bottom = cv.h as f32 - 10.0 * ps;
    for l in legends {
        const MAX_ROWS: usize = 20;
        let rows = l.classes.len().min(MAX_ROWS) + usize::from(l.classes.len() > MAX_ROWS);
        let (bar_w, bar_h) = (220.0 * ps, 10.0 * ps);
        let mut width: f32 = if l.cmap.is_some() { bar_w } else { 0.0 };
        width = width.max(Canvas::text_width(&l.caption, fs, Face::Bitmap));
        for (_, name) in l.classes.iter().take(MAX_ROWS) {
            width = width.max(18.0 * ps + Canvas::text_width(name, fs, Face::Bitmap));
        }
        let height = if l.caption.is_empty() { 0.0 } else { line }
            + if l.cmap.is_some() { bar_h + 2.0 * ps + line } else { rows as f32 * line };
        let (x0, y0) = (10.0 * ps, bottom - height);
        cv.fill_rect(x0 - pad, y0 - pad, x0 + width + pad, bottom + pad - 2.0 * ps, bx);
        let mut y = y0;
        if !l.caption.is_empty() {
            cv.text(x0, y, &l.caption, fs, fg, Face::Bitmap);
            y += line;
        }
        if let Some(cmap) = l.cmap {
            let cm = &janus_gpu::colormaps::all()[cmap];
            let n = (bar_w as i32).max(1); // one colour per pixel
            for i in 0..n {
                let c = cm.sample((i as f32 + 0.5) / n as f32);
                let (a, b) = (x0 + bar_w * i as f32 / n as f32, x0 + bar_w * (i + 1) as f32 / n as f32);
                cv.fill_rect(a, y, b, y + bar_h, c);
            }
            cv.rect(x0, y, x0 + bar_w, y + bar_h, rgba(0, 0, 0, 255), ps);
            let (lo, hi) = (g4(l.lo), g4(l.hi));
            cv.text(x0, y + bar_h + 2.0 * ps, &lo, fs, fg, Face::Bitmap);
            cv.text(
                x0 + bar_w - Canvas::text_width(&hi, fs, Face::Bitmap),
                y + bar_h + 2.0 * ps,
                &hi,
                fs,
                fg,
                Face::Bitmap,
            );
        } else {
            for k in 0..rows {
                if k == MAX_ROWS {
                    let more = format!("... {} more", l.classes.len() - MAX_ROWS);
                    cv.text(x0, y, &more, fs, fg, Face::Bitmap);
                    break;
                }
                cv.fill_rect(x0, y + ps, x0 + 12.0 * ps, y + 13.0 * ps, l.classes[k].0);
                cv.rect(x0, y + ps, x0 + 12.0 * ps, y + 13.0 * ps, rgba(0, 0, 0, 255), ps);
                cv.text(x0 + 18.0 * ps, y, &l.classes[k].1, fs, fg, Face::Bitmap);
                y += line;
            }
        }
        bottom = y0 - pad - 8.0 * ps;
    }
}

/// The basemap's attribution, bottom right on a dark box, in lines that fit the image; the top of the box (the
/// image's height without one).
pub fn draw_credit(cv: &mut Canvas, text: &str, ps: f32) -> f32 {
    if text.is_empty() {
        return cv.h as f32;
    }
    let fs = FIGURE_FONT_SIZE * 0.85 * ps;
    let (line, pad) = (fs + 2.0 * ps, 3.0 * ps);
    let max_w = (80.0 * ps).max(cv.w as f32 - 20.0 * ps);
    let mut lines = vec![String::new()];
    // word by word
    for word in text.split_inclusive(' ') {
        let last = lines.last_mut().unwrap();
        if !last.is_empty() && Canvas::text_width(&format!("{last}{word}"), fs, Face::Bitmap) > max_w {
            lines.push(String::new());
        }
        lines.last_mut().unwrap().push_str(word);
    }
    let w = lines.iter().map(|l| Canvas::text_width(l, fs, Face::Bitmap)).fold(0.0, f32::max);
    let (x1, y1) = (cv.w as f32 - 5.0 * ps, cv.h as f32 - 5.0 * ps);
    let (x0, y0) = (x1 - w, y1 - line * lines.len() as f32);
    cv.fill_rect(x0 - pad, y0 - pad, x1 + pad, y1 + pad, rgba(0, 0, 0, 140));
    for (k, l) in lines.iter().enumerate() {
        let tx = x1 - Canvas::text_width(l, fs, Face::Bitmap);
        cv.text(tx, y0 + line * k as f32, l, fs, rgba(220, 220, 220, 255), Face::Bitmap);
    }
    y0 - pad
}

/// A scale bar bottom right, its bottom at `bottom`: the longest round length (1, 2 or 5 x 10^k m) that fits in
/// ~120 points, white with a dark outline on a dark box, its length above it.
pub fn draw_scale_bar(cv: &mut Canvas, metres_per_px: f64, ps: f32, bottom: f32) {
    if metres_per_px.is_nan() || metres_per_px <= 0.0 || !metres_per_px.is_finite() {
        return;
    }
    let max_m = 120.0 * f64::from(ps) * metres_per_px;
    let mut len = 10f64.powf(max_m.log10().floor());
    for k in [5.0, 2.0] {
        if len * k <= max_m {
            len *= k;
            break;
        }
    }
    let bar_w = (len / metres_per_px) as f32;
    let g = |v: f64| janus_validation::fmt_g(v, 6);
    let text = if len >= 1000.0 {
        format!("{} km", g(len / 1000.0))
    } else if len >= 1.0 {
        format!("{} m", g(len))
    } else {
        format!("{} cm", g(len * 100.0))
    };
    let fs = FIGURE_FONT_SIZE * ps;
    let (pad, bar_h) = (5.0 * ps, 5.0 * ps);
    let x1 = cv.w as f32 - 10.0 * ps;
    let x0 = x1 - bar_w;
    let y1 = bottom - 8.0 * ps;
    let y0 = y1 - bar_h;
    let tw = Canvas::text_width(&text, fs, Face::Bitmap);
    cv.fill_rect(x0.min(x1 - tw) - pad, y0 - fs - 3.0 * ps - pad, x1 + pad, y1 + pad, rgba(0, 0, 0, 140));
    cv.fill_rect(x0 - ps, y0 - ps, x1 + ps, y1 + ps, rgba(0, 0, 0, 255));
    cv.fill_rect(x0, y0, x1, y1, rgba(255, 255, 255, 255));
    cv.fill_rect(x0 + bar_w / 2.0 - ps / 2.0, y0, x0 + bar_w / 2.0 + ps / 2.0, y1, rgba(0, 0, 0, 255)); // the half
    cv.text(x1 - tw, y0 - fs - 3.0 * ps, &text, fs, rgba(255, 255, 255, 255), Face::Bitmap);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_shapes() {
        let (w, h) = (120, 40);
        let mut px = vec![0u8; w * h * 4];
        let mut cv = Canvas { w: w as i32, h: h as i32, px: &mut px };
        cv.fill_rect(0.0, 0.0, 120.0, 40.0, rgba(0, 0, 0, 255));
        cv.text(4.0, 4.0, "2004-07-15", 13.0, rgba(255, 255, 255, 255), Face::Bitmap);
        let white = px.chunks(4).filter(|p| p[0] > 200).count();
        assert!(white > 30, "white text pixels: {white}");
        // ProggyClean: 7 px a character at 13 px
        assert_eq!(Canvas::text_width("2004", 13.0, Face::Bitmap), 28.0);
        assert!(Canvas::text_width("2004", 30.0, Face::Vector) > 40.0);
    }
}
