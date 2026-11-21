//! Figures (phase 60): a panel's drawing as a PNG or an SVG at a size in millimetres and a resolution in dpi, in a
//! light look whatever the interface's theme. A figure is a closure that draws into an `egui::Ui` (the panel's own
//! drawing code, given the area and the style); it runs in an `egui::Context` of its own, without a window, whose
//! screen is the figure's size in points (1 pt = 1/72 inch, so the font's size in pt is the figure's), and the shapes
//! it paints are written out:
//!
//! - SVG: the clipped shapes as elements (paths, rectangles, circles); text stays text (a `<text>` a row of the
//!   galley, in the font's family and size, editable in Inkscape or Illustrator); a mesh (a gradient, an image) as an
//!   embedded PNG of its box.
//! - PNG: the same shapes tessellated and drawn into a pixel buffer by the CPU (triangles over the font atlas, egui's
//!   own feathered edges for the anti-aliasing), with the dpi in the file. The GPU is not needed, so a figure is
//!   written in the background like the other exports, and the tests run without one.

use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;

use egui::epaint::{ClippedShape, Mesh, Primitive};
use egui::{Color32, Context, Pos2, Rect, Shape, TextureId, Ui, Vec2, pos2, vec2};

/// The file format of a figure.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Png,
    Svg,
}

impl Format {
    pub fn ext(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Svg => "svg",
        }
    }
}

/// How a figure is made.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Mode {
    /// The panel as it is on screen: the interface's own look, fonts and size in points, drawn by the GPU through
    /// egui_wgpu (the renderer the app uses), at 1 to 4 pixels per screen pixel (`FigureOptions::scale`).
    AsShown,
    /// A light figure at a size in mm, a font in pt and a resolution in dpi, drawn by our own rasterizer.
    #[default]
    Publication,
}

/// A figure's choices.
#[derive(Clone, Debug)]
pub struct FigureOptions {
    pub mode: Mode,
    /// as shown: pixels per screen pixel (1 to 4); the layout is the screen's, only the pixels are more
    pub scale: u32,
    /// as shown: the chart keeps the line of the current date (it is on the screen)
    pub moment: bool,
    pub width_mm: f32,
    pub height_mm: f32,
    /// pixels per inch of a PNG (150, 300, 600)
    pub dpi: u32,
    /// the text's size, in points
    pub font_pt: f32,
    /// no background (PNG: alpha 0; SVG: no rectangle) instead of white
    pub transparent: bool,
    pub format: Format,
}

impl Default for FigureOptions {
    fn default() -> Self {
        FigureOptions {
            mode: Mode::Publication,
            scale: 1,
            moment: true,
            width_mm: 85.0,
            height_mm: 60.0,
            dpi: 300,
            font_pt: 8.0,
            transparent: false,
            format: Format::Png,
        }
    }
}

/// The largest figure's side and size in pixels (a PNG of 16384 x 16384 is 1 GB of RGBA).
pub const MAX_SIDE_PX: u32 = 16384;
pub const MAX_PIXELS: f64 = 100e6;

impl FigureOptions {
    /// The PNG's size in pixels: mm / 25.4 x dpi.
    pub fn pixels(&self) -> [u32; 2] {
        let px = |mm: f32| ((f64::from(mm) / 25.4 * f64::from(self.dpi)).round() as u32).max(1);
        [px(self.width_mm), px(self.height_mm)]
    }

    /// Pixels per point of the figure's screen (a point is 1/72 inch).
    pub fn ppp(&self) -> f32 {
        self.dpi as f32 / 72.0
    }

    /// The screen's size in points (the pixels' exactly, so the PNG has the size asked).
    pub fn size_pt(&self) -> Vec2 {
        let [w, h] = self.pixels();
        vec2(w as f32, h as f32) / self.ppp()
    }

    /// Whether the figure is of a size we can draw.
    pub fn fits(&self) -> bool {
        let [w, h] = self.pixels();
        w <= MAX_SIDE_PX && h <= MAX_SIDE_PX && f64::from(w) * f64::from(h) <= MAX_PIXELS
    }
}

thread_local! {
    static MODE: Cell<Option<Mode>> = const { Cell::new(None) };
    /// The strip of controls at the top of the panel being marked or drawn: (the y where the panel starts, the y under
    /// its leading controls).
    static STRIP: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
}

/// Whether a publication figure is being drawn on this thread: the panels leave their controls out, and what is of
/// the moment, and fit their text to a column.
pub fn active() -> bool {
    MODE.with(Cell::get) == Some(Mode::Publication)
}

/// Whether a figure of any mode is being drawn on this thread.
pub fn rendering() -> bool {
    MODE.with(Cell::get).is_some()
}

struct ActiveGuard;

impl ActiveGuard {
    fn enter(mode: Mode) -> Self {
        MODE.with(|a| a.set(Some(mode)));
        ActiveGuard
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        MODE.with(|a| a.set(None));
        STRIP.with(|s| s.set(None));
    }
}

/// Adds a row of controls spanning `from` to `to` to the strip, if it is the strip's next row.
fn strip_row(ui: &Ui, from: f32, to: f32) {
    STRIP.with(|s| {
        if let Some((top, below)) = s.get()
            && from - below <= ui.spacing().item_spacing.y + 2.0
        {
            s.set(Some((top, to.max(below))));
        }
    });
}

/// A panel's controls. On screen they are drawn as usual. In a publication figure they run in an invisible `Ui` away
/// from the figure (their choices are still read, nothing of them is painted, no room taken). In a figure as shown
/// they run invisible where they are and keep their room, so that what is under them has the size it has on screen;
/// the figure is then cut under them (`Rendered::crop`).
pub fn chrome<R>(ui: &mut Ui, f: impl FnOnce(&mut Ui) -> R) -> R {
    match MODE.with(Cell::get) {
        None => {
            let from = ui.cursor().min.y;
            let r = f(ui);
            strip_row(ui, from, ui.min_rect().max.y);
            r
        }
        Some(Mode::Publication) => {
            let away = Rect::from_min_size(pos2(-1.0e5, -1.0e5), vec2(ui.available_width().max(200.0), 1.0e4));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(away).invisible());
            f(&mut child)
        }
        Some(Mode::AsShown) => {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(ui.available_rect_before_wrap()).invisible());
            let r = f(&mut child);
            let used = child.min_rect();
            ui.allocate_rect(used, egui::Sense::hover());
            strip_row(ui, used.min.y, used.max.y);
            r
        }
    }
}

/// A panel as it is on screen, in points: what a figure as shown takes its size from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PanelSeen {
    /// the panel's content: its width and its height
    pub size: Vec2,
    /// the height of the controls at its top, which a figure leaves out
    pub crop: f32,
    /// where its top left corner is in the window
    pub at: Pos2,
    /// the frame it was drawn in (`Context::cumulative_frame_nr`): a panel behind another tab is not drawn
    pub frame: u64,
}

impl PanelSeen {
    /// What a panel not yet seen on screen is drawn in.
    pub const FALLBACK: PanelSeen = PanelSeen { size: Vec2::new(640.0, 380.0), crop: 0.0, at: Pos2::ZERO, frame: 0 };

    /// The figure's size in points: the panel's, less its controls.
    pub fn figure_size(&self) -> Vec2 {
        vec2(self.size.x, (self.size.y - self.crop).max(8.0))
    }

    /// The figure's size in pixels at `ppp` pixels per point.
    pub fn pixels(&self, ppp: f32) -> [u32; 2] {
        let s = self.figure_size() * ppp;
        [(s.x.round() as u32).max(1), (s.y.round() as u32).max(1)]
    }
}

fn seen_id(key: u32) -> egui::Id {
    egui::Id::new(("figure panel as seen", key))
}

/// The panel `key` as it was last drawn on screen.
pub fn seen(ctx: &Context, key: u32) -> Option<PanelSeen> {
    ctx.data(|d| d.get_temp(seen_id(key)))
}

/// A panel being drawn on screen, to be recorded (`PanelSeen`) when it ends.
pub struct Mark {
    key: u32,
    start: Rect,
    live: bool,
}

/// Starts to record the panel `key` drawn into `ui` (nothing is recorded while a figure is drawn).
pub fn mark(ui: &Ui, key: u32) -> Mark {
    let live = !rendering();
    let start = ui.available_rect_before_wrap();
    if live {
        STRIP.with(|s| s.set(Some((start.min.y, start.min.y))));
    }
    Mark { key, start, live }
}

impl Mark {
    /// Records the panel: the width it was given, the height it took (at most the height it was given).
    pub fn end(self, ui: &Ui) {
        if !self.live {
            return;
        }
        let below = STRIP.with(|s| s.take()).map_or(self.start.min.y, |(_, b)| b);
        let used = (ui.min_rect().max.y - self.start.min.y).max(0.0);
        let h = if self.start.height().is_finite() && self.start.height() < 1.0e6 {
            used.min(self.start.height())
        } else {
            used
        };
        let w = if self.start.width().is_finite() && self.start.width() < 1.0e6 {
            self.start.width()
        } else {
            ui.min_rect().width()
        };
        if w < 8.0 || h < 8.0 {
            return;
        }
        let crop = (below - self.start.min.y).clamp(0.0, (h - 16.0).max(0.0));
        let seen = PanelSeen { size: vec2(w, h), crop, at: self.start.min, frame: ui.ctx().cumulative_frame_nr() };
        ui.ctx().data_mut(|d| d.insert_temp(seen_id(self.key), seen));
    }
}

/// `ui.horizontal_wrapped` of a panel's controls (see `chrome`).
pub fn controls_wrapped(ui: &mut Ui, f: impl FnOnce(&mut Ui)) {
    chrome(ui, |ui| {
        ui.horizontal_wrapped(f);
    });
}

/// `ui.horizontal` of a panel's controls (see `chrome`).
pub fn controls(ui: &mut Ui, f: impl FnOnce(&mut Ui)) {
    chrome(ui, |ui| {
        ui.horizontal(f);
    });
}

/// The X axis of a plot with its labels: egui_plot hides the labels of marks under 60 points apart, which on a
/// figure of a column's width is most of them; in a figure they are kept at 20 points.
pub trait FigurePlot<'a> {
    /// `x_axis_label`.
    fn x_title(self, title: impl Into<egui::WidgetText>) -> Self;
    /// `x_axis_formatter`.
    fn x_labels(self, f: impl Fn(egui_plot::GridMark, &std::ops::RangeInclusive<f64>) -> String + 'a) -> Self;
}

fn spaced(a: egui_plot::AxisHints<'_>) -> egui_plot::AxisHints<'_> {
    if active() { a.label_spacing(20.0..=21.0) } else { a }
}

impl<'a> FigurePlot<'a> for egui_plot::Plot<'a> {
    fn x_title(self, title: impl Into<egui::WidgetText>) -> Self {
        self.custom_x_axes(vec![spaced(egui_plot::AxisHints::new_x().label(title))])
    }

    fn x_labels(self, f: impl Fn(egui_plot::GridMark, &std::ops::RangeInclusive<f64>) -> String + 'a) -> Self {
        self.custom_x_axes(vec![spaced(egui_plot::AxisHints::new_x().formatter(f))])
    }
}

/// A figure drawn: the shapes of its screen and the textures they use.
pub struct Rendered {
    pub(crate) ctx: Context,
    pub(crate) shapes: Vec<ClippedShape>,
    textures: Textures,
    /// every texture change the passes made, in order (what the GPU needs to draw the shapes)
    pub(crate) deltas: Vec<(TextureId, egui::epaint::ImageDelta)>,
    pub(crate) ppp: f32,
    /// the screen, in points
    pub(crate) size: Vec2,
    /// as shown: the height at the top of the screen that is left out (the panel's controls), in points
    pub(crate) crop: f32,
    /// the screen's colour under the shapes (as shown: the panel's)
    pub(crate) background: Color32,
    pub mode: Mode,
    pub transparent: bool,
    pub dpi: u32,
}

/// Draws the passes of a figure in `ctx` (the plots measure their axes in one frame and use them in the next; the
/// last frame is the figure). `after` runs after each pass.
fn run_passes(
    ctx: &Context,
    size: Vec2,
    ppp: f32,
    crop_strip: bool,
    after: &dyn Fn(&Context),
    draw: &mut dyn FnMut(&mut Ui),
) -> (Vec<ClippedShape>, Vec<(TextureId, egui::epaint::ImageDelta)>, f32) {
    let mut deltas = Vec::new();
    let mut shapes = Vec::new();
    let mut crop = 0.0;
    for pass in 0..(if crop_strip { 4 } else { 3 }) {
        let mut input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(f64::from(pass) * 0.05),
            ..Default::default()
        };
        input.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(ppp);
        STRIP.with(|s| s.set(crop_strip.then_some((0.0, 0.0))));
        let mut out = ctx.run_ui(input, |ui| draw(ui));
        crop = STRIP.with(|s| s.take()).map_or(0.0, |(_, below)| below);
        let mut delta = std::mem::take(&mut out.textures_delta);
        for (id, parts) in std::mem::take(&mut delta.set) {
            deltas.extend(parts.into_iter().map(|d| (id, d)));
        }
        delta.clear();
        shapes = out.shapes;
        after(ctx);
    }
    (shapes, deltas, crop)
}

fn textures_of(deltas: &[(TextureId, egui::epaint::ImageDelta)]) -> Textures {
    let mut t = Textures::default();
    for (id, d) in deltas {
        t.apply(*id, d);
    }
    t
}

/// Runs `draw` in a context of its own, as `o` says, and keeps what it paints (a publication figure).
pub fn render(o: &FigureOptions, draw: &mut dyn FnMut(&mut Ui)) -> Rendered {
    let _look = crate::theme::FigureScope::enter();
    let _active = ActiveGuard::enter(Mode::Publication);
    let ctx = Context::default();
    ctx.set_fonts(crate::theme::figure_fonts());
    ctx.set_theme(egui::Theme::Light);
    let pt = o.font_pt.max(2.0);
    ctx.style_mut_of(egui::Theme::Light, |s| {
        s.visuals = crate::theme::figure_visuals();
        s.animation_time = 0.0;
        s.spacing.item_spacing = vec2(pt * 0.5, pt * 0.3);
        s.spacing.button_padding = vec2(pt * 0.4, pt * 0.2);
        s.spacing.interact_size = vec2(pt * 2.0, pt * 1.4);
        s.text_styles = [
            (egui::TextStyle::Small, egui::FontId::proportional(pt * 0.85)),
            (egui::TextStyle::Body, egui::FontId::proportional(pt)),
            (egui::TextStyle::Button, egui::FontId::proportional(pt)),
            (egui::TextStyle::Heading, crate::theme::strong_font(pt * 1.15)),
            (egui::TextStyle::Monospace, egui::FontId::monospace(pt * 0.92)),
        ]
        .into();
    });
    let size = o.size_pt();
    let ppp = o.ppp();
    // room at the edges for the last tick's label (centred on the edge of the plot)
    let side = (pt * 0.4).round().clamp(1.0, 12.0) as i8;
    let margin = egui::Margin { left: side, right: (pt * 1.6).round().clamp(2.0, 30.0) as i8, top: side, bottom: side };
    let (mut shapes, deltas, _) = run_passes(&ctx, size, ppp, false, &|_| {}, &mut |ui| {
        egui::Frame::NONE.inner_margin(margin).show(ui, |ui| draw(ui));
    });
    // what was painted away from the figure (a panel's controls) is not part of it
    let screen = Rect::from_min_size(Pos2::ZERO, size);
    shapes.retain(|s| s.clip_rect.intersects(screen) && s.shape.visual_bounding_rect().intersects(screen));
    let textures = textures_of(&deltas);
    Rendered {
        ctx,
        shapes,
        textures,
        deltas,
        ppp,
        size,
        crop: 0.0,
        background: Color32::WHITE,
        mode: Mode::Publication,
        transparent: o.transparent,
        dpi: o.dpi,
    }
}

/// A panel drawn as it is on screen: in a context of its own with the interface's style, fonts and pixels per point
/// (times `scale`), into a screen of the panel's size in points (`seen`). `seed` copies what the panel keeps in the
/// main context's memory (which series are hidden...) into the new one, after each pass. The panel's leading controls
/// keep their room and are cut off (`Rendered::crop`).
pub fn render_as_shown(
    main: &Context,
    seen: PanelSeen,
    scale: u32,
    transparent: bool,
    seed: &dyn Fn(&Context),
    draw: &mut dyn FnMut(&mut Ui),
) -> Rendered {
    let _active = ActiveGuard::enter(Mode::AsShown);
    let ctx = Context::default();
    ctx.set_fonts(crate::theme::current_fonts());
    let theme = main.theme();
    ctx.set_theme(theme);
    ctx.set_style_of(theme, main.global_style());
    ctx.style_mut_of(theme, |s| s.animation_time = 0.0);
    let ppp = main.pixels_per_point() * scale.clamp(1, 4) as f32;
    let size = seen.size;
    let (mut shapes, deltas, crop) = run_passes(&ctx, size, ppp, true, seed, &mut |ui| {
        let r = Rect::from_min_size(Pos2::ZERO, size);
        ui.scope_builder(egui::UiBuilder::new().max_rect(r), |ui| draw(ui));
    });
    let crop = crop.clamp(0.0, (size.y - 8.0).max(0.0));
    let screen = Rect::from_min_max(pos2(0.0, crop), pos2(size.x, size.y));
    shapes.retain(|s| s.clip_rect.intersects(screen) && s.shape.visual_bounding_rect().intersects(screen));
    let fill = main.global_style().visuals.panel_fill;
    let textures = textures_of(&deltas);
    Rendered {
        ctx,
        shapes,
        textures,
        deltas,
        ppp,
        size,
        crop,
        background: Color32::from_rgb(fill.r(), fill.g(), fill.b()),
        mode: Mode::AsShown,
        transparent,
        dpi: (96.0 * ppp).round() as u32,
    }
}

impl Rendered {
    /// The figure's size in pixels.
    pub fn pixels(&self) -> [usize; 2] {
        [(self.size.x * self.ppp).round() as usize, ((self.size.y - self.crop) * self.ppp).round().max(1.0) as usize]
    }

    /// The figure drawn by the CPU: RGBA8 rows top-down, not premultiplied.
    pub fn to_rgba(&self) -> (Vec<u8>, usize, usize) {
        let [w, h] = self.pixels();
        let bg = self.background.to_array();
        let mut r = Raster::new(w, h, if self.transparent { [0; 4] } else { bg });
        let prims = self.ctx.tessellate(self.shapes.clone(), self.ppp);
        for p in &prims {
            if let Primitive::Mesh(m) = &p.primitive {
                r.mesh(m, pos2(0.0, self.crop), self.ppp, p.clip_rect, &self.textures);
            }
        }
        (r.straight(), w, h)
    }

    /// The figure as PNG bytes, the dpi in them.
    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let (rgba, w, h) = self.to_rgba();
        png_bytes(&rgba, w, h, self.dpi)
    }

    /// The figure as an image for the clipboard.
    pub fn to_color_image(&self) -> egui::ColorImage {
        let (rgba, w, h) = self.to_rgba();
        egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
    }

    /// The figure as an SVG document: `width` and `height` in millimetres, its user units the points.
    pub fn to_svg(&self, width_mm: f32, height_mm: f32) -> String {
        self.svg_document(&format!("{}mm", num(width_mm)), &format!("{}mm", num(height_mm)))
    }

    /// The figure as an SVG document of the size the screen had, in points.
    pub fn to_svg_shown(&self) -> String {
        let h = self.size.y - self.crop;
        self.svg_document(&num(self.size.x), &num(h))
    }

    fn svg_document(&self, width: &str, height: &str) -> String {
        let mut w = Svg { clips: Vec::new(), body: String::new(), r: self };
        let screen = Rect::from_min_size(Pos2::ZERO, self.size);
        for s in &self.shapes {
            let clip = s.clip_rect.intersect(screen);
            let id = w.clip(clip);
            let mut one = String::new();
            w.shape(&s.shape, clip, &mut one);
            if one.is_empty() {
                continue;
            }
            match id {
                Some(id) => {
                    let _ = writeln!(w.body, "<g clip-path=\"url(#c{id})\">\n{one}</g>");
                }
                None => w.body += &one,
            }
        }
        let mut out = String::new();
        let (sw, sh) = (num(self.size.x), num(self.size.y - self.crop));
        let top = num(self.crop);
        let _ = writeln!(out, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>");
        let _ = writeln!(
            out,
            "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{width}\" \
             height=\"{height}\" viewBox=\"0 {top} {sw} {sh}\" font-family=\"{SANS}\">"
        );
        if !w.clips.is_empty() {
            out += "<defs>\n";
            for (i, c) in w.clips.iter().enumerate() {
                let _ = writeln!(
                    out,
                    "<clipPath id=\"c{i}\"><rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/></clipPath>",
                    num(c.min.x),
                    num(c.min.y),
                    num(c.width()),
                    num(c.height())
                );
            }
            out += "</defs>\n";
        }
        if !self.transparent {
            let [r, g, b, _] = self.background.to_array();
            let _ = writeln!(out, "<rect y=\"{top}\" width=\"{sw}\" height=\"{sh}\" fill=\"#{r:02x}{g:02x}{b:02x}\"/>");
        }
        out += &w.body;
        out += "</svg>\n";
        out
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// SVG
// ---------------------------------------------------------------------------------------------------------------------

const SANS: &str = "Inter, Helvetica, Arial, sans-serif";

/// A number as short as it can be at a hundredth of a point.
fn num(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.to_string() }
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o += "&amp;",
            '<' => o += "&lt;",
            '>' => o += "&gt;",
            '"' => o += "&quot;",
            c if (c as u32) < 32 && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

/// `name="#rrggbb"` and `name-opacity` for a colour (egui's are premultiplied).
fn paint(name: &str, c: Color32, opacity: f32) -> String {
    if c.a() == 0 || opacity <= 0.0 {
        return format!(" {name}=\"none\"");
    }
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    let a = f32::from(a) / 255.0 * opacity;
    let mut s = format!(" {name}=\"#{r:02x}{g:02x}{b:02x}\"");
    if a < 0.999 {
        let _ = write!(s, " {name}-opacity=\"{}\"", num_n(a, 3));
    }
    s
}

fn num_n(v: f32, digits: usize) -> String {
    let s = format!("{v:.digits$}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn stroke_attrs(width: f32, c: Color32) -> String {
    if width <= 0.0 || c.a() == 0 {
        return " stroke=\"none\"".into();
    }
    format!(
        "{} stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"",
        paint("stroke", c, 1.0),
        num(width)
    )
}

struct Svg<'a> {
    /// the clip rectangles in use (a clipPath each)
    clips: Vec<Rect>,
    body: String,
    r: &'a Rendered,
}

impl Svg<'_> {
    /// The clipPath of `clip` (None: it covers the figure).
    fn clip(&mut self, clip: Rect) -> Option<usize> {
        let screen = Rect::from_min_size(Pos2::ZERO, self.r.size);
        if clip.contains_rect(screen) {
            return None;
        }
        if let Some(i) = self.clips.iter().position(|c| *c == clip) {
            return Some(i);
        }
        self.clips.push(clip);
        Some(self.clips.len() - 1)
    }

    fn shape(&mut self, s: &Shape, clip: Rect, out: &mut String) {
        match s {
            Shape::Noop | Shape::Callback(_) => {}
            Shape::Vec(v) => {
                for s in v {
                    self.shape(s, clip, out);
                }
            }
            Shape::LineSegment { points, stroke } => {
                if stroke.width > 0.0 && stroke.color.a() > 0 {
                    let _ = writeln!(
                        out,
                        "<path d=\"M{} {}L{} {}\" fill=\"none\"{}/>",
                        num(points[0].x),
                        num(points[0].y),
                        num(points[1].x),
                        num(points[1].y),
                        stroke_attrs(stroke.width, stroke.color)
                    );
                }
            }
            Shape::Path(p) => {
                let color = match p.stroke.color {
                    egui::epaint::ColorMode::Solid(c) => c,
                    egui::epaint::ColorMode::UV(_) => Color32::GRAY,
                };
                self.path(&p.points, p.closed, p.fill, p.stroke.width, color, out);
            }
            Shape::Rect(r) => {
                let w = r.stroke.width;
                let rect = match r.stroke_kind {
                    egui::StrokeKind::Inside => r.rect.shrink(w * 0.5),
                    egui::StrokeKind::Middle => r.rect,
                    egui::StrokeKind::Outside => r.rect.expand(w * 0.5),
                };
                let c = r.corner_radius;
                let rx = f32::from(c.nw.max(c.ne).max(c.sw).max(c.se));
                if r.fill.a() == 0 && (w <= 0.0 || r.stroke.color.a() == 0) {
                    return;
                }
                let _ = write!(
                    out,
                    "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"",
                    num(rect.min.x),
                    num(rect.min.y),
                    num(rect.width().max(0.0)),
                    num(rect.height().max(0.0))
                );
                if rx > 0.0 {
                    let _ = write!(out, " rx=\"{}\"", num(rx));
                }
                let _ = writeln!(out, "{}{}/>", paint("fill", r.fill, 1.0), stroke_attrs(w, r.stroke.color));
            }
            Shape::Circle(c) => {
                let _ = writeln!(
                    out,
                    "<circle cx=\"{}\" cy=\"{}\" r=\"{}\"{}{}/>",
                    num(c.center.x),
                    num(c.center.y),
                    num(c.radius),
                    paint("fill", c.fill, 1.0),
                    stroke_attrs(c.stroke.width, c.stroke.color)
                );
            }
            Shape::Ellipse(e) => {
                let _ = writeln!(
                    out,
                    "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" transform=\"rotate({} {} {})\"{}{}/>",
                    num(e.center.x),
                    num(e.center.y),
                    num(e.radius.x),
                    num(e.radius.y),
                    num(e.angle.to_degrees()),
                    num(e.center.x),
                    num(e.center.y),
                    paint("fill", e.fill, 1.0),
                    stroke_attrs(e.stroke.width, e.stroke.color)
                );
            }
            Shape::QuadraticBezier(b) => {
                let color = match b.stroke.color {
                    egui::epaint::ColorMode::Solid(c) => c,
                    egui::epaint::ColorMode::UV(_) => Color32::GRAY,
                };
                self.path(&b.flatten(None), b.closed, b.fill, b.stroke.width, color, out);
            }
            Shape::CubicBezier(b) => {
                let color = match b.stroke.color {
                    egui::epaint::ColorMode::Solid(c) => c,
                    egui::epaint::ColorMode::UV(_) => Color32::GRAY,
                };
                self.path(&b.flatten(None), b.closed, b.fill, b.stroke.width, color, out);
            }
            Shape::Text(t) => self.text(t, out),
            Shape::Mesh(m) => self.mesh(m, clip, out),
        }
    }

    fn path(&self, pts: &[Pos2], closed: bool, fill: Color32, width: f32, color: Color32, out: &mut String) {
        if pts.len() < 2 || (fill.a() == 0 && (width <= 0.0 || color.a() == 0)) {
            return;
        }
        let mut d = String::with_capacity(pts.len() * 12);
        for (i, p) in pts.iter().enumerate() {
            let _ = write!(d, "{}{} {}", if i == 0 { 'M' } else { 'L' }, num(p.x), num(p.y));
        }
        if closed {
            d.push('Z');
        }
        let _ = writeln!(out, "<path d=\"{d}\"{}{}/>", paint("fill", fill, 1.0), stroke_attrs(width, color));
    }

    /// A text a `<text>` a row of its galley, at the row's first glyph, its baseline, in the section's font and colour.
    fn text(&self, t: &egui::epaint::TextShape, out: &mut String) {
        let g = &t.galley;
        // the glyphs are one a character of the job's text, without its line breaks
        let mut offsets = g.job.text.char_indices().filter(|(_, c)| *c != '\n').map(|(i, _)| i);
        for row in &g.rows {
            let glyphs = &row.row.glyphs;
            let Some(first) = glyphs.first() else { continue };
            let at = offsets.next();
            for _ in 1..glyphs.len() {
                offsets.next();
            }
            let text: String = glyphs.iter().map(|g| g.chr).collect();
            if text.trim().is_empty() {
                continue;
            }
            let section = at
                .and_then(|o| g.job.sections.iter().find(|s| s.byte_range.contains(&egui::epaint::text::ByteIndex(o))))
                .or_else(|| g.job.sections.first());
            let (font, mut colour, italics) = section.map_or((egui::FontId::default(), Color32::BLACK, false), |s| {
                (s.format.font_id.clone(), s.format.color, s.format.italics)
            });
            if colour == Color32::PLACEHOLDER {
                colour = t.override_text_color.unwrap_or(t.fallback_color);
            } else if let Some(o) = t.override_text_color {
                colour = o;
            }
            let (x, y) = (t.pos.x + row.pos.x + first.pos.x, t.pos.y + row.pos.y + first.pos.y);
            let (family, weight) = match &font.family {
                egui::FontFamily::Monospace => ("monospace".to_string(), ""),
                egui::FontFamily::Name(n) if n.contains("semibold") => (SANS.to_string(), " font-weight=\"600\""),
                _ => (SANS.to_string(), ""),
            };
            let mut s = format!(
                "<text x=\"{}\" y=\"{}\" font-family=\"{family}\" font-size=\"{}\"{weight}{}",
                num(x),
                num(y),
                num(font.size),
                paint("fill", colour, t.opacity_factor)
            );
            if italics {
                s += " font-style=\"italic\"";
            }
            if t.angle != 0.0 {
                let _ =
                    write!(s, " transform=\"rotate({} {} {})\"", num(t.angle.to_degrees()), num(t.pos.x), num(t.pos.y));
            }
            let _ = writeln!(out, "{s} xml:space=\"preserve\">{}</text>", escape(&text));
        }
    }

    /// A mesh (a gradient, an image): drawn into a PNG of its box, at the figure's dpi, embedded.
    fn mesh(&self, m: &std::sync::Arc<Mesh>, clip: Rect, out: &mut String) {
        let bounds = m.calc_bounds().intersect(clip);
        if !bounds.is_positive() || m.indices.is_empty() {
            return;
        }
        let ppp = self.r.ppp;
        let (w, h) = ((bounds.width() * ppp).ceil() as usize, (bounds.height() * ppp).ceil() as usize);
        if w == 0 || h == 0 || w * h > 60_000_000 {
            return;
        }
        let mut r = Raster::new(w, h, [0; 4]);
        r.mesh(m, bounds.min, ppp, clip, &self.r.textures);
        let Ok(png) = png_bytes(&r.straight(), w, h, self.r.dpi) else { return };
        let _ = writeln!(
            out,
            "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" \
             xlink:href=\"data:image/png;base64,{}\"/>",
            num(bounds.min.x),
            num(bounds.min.y),
            num(w as f32 / ppp),
            num(h as f32 / ppp),
            base64(&png)
        );
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n =
            (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

// ---------------------------------------------------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------------------------------------------------

/// An RGBA8 image (not premultiplied) as PNG bytes with its resolution (a pHYs chunk).
pub fn png_bytes(rgba: &[u8], w: usize, h: usize, dpi: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder;
    let mut buf = Vec::new();
    image::codecs::png::PngEncoder::new_with_quality(
        &mut buf,
        image::codecs::png::CompressionType::Default,
        image::codecs::png::FilterType::Adaptive,
    )
    .write_image(rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
    .map_err(|e| format!("could not encode the PNG: {e}"))?;
    // the resolution: pixels per metre, after the header chunk (8 bytes of signature, 25 of IHDR)
    if buf.len() > 33 {
        let ppm = (f64::from(dpi) / 0.0254).round() as u32;
        let mut chunk = Vec::with_capacity(21);
        chunk.extend_from_slice(&9u32.to_be_bytes());
        chunk.extend_from_slice(b"pHYs");
        chunk.extend_from_slice(&ppm.to_be_bytes());
        chunk.extend_from_slice(&ppm.to_be_bytes());
        chunk.push(1);
        let crc = crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        buf.splice(33..33, chunk);
    }
    Ok(buf)
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

/// The textures a figure's meshes use (the font atlas, images).
#[derive(Default)]
struct Textures(HashMap<TextureId, Tex>);

struct Tex {
    w: usize,
    h: usize,
    px: Vec<Color32>,
    /// magnified without smoothing (a density's cells)
    nearest: bool,
}

impl Textures {
    fn apply(&mut self, id: TextureId, d: &egui::epaint::ImageDelta) {
        let egui::ImageData::Color(img) = &d.image;
        let [w, h] = img.size;
        match d.pos {
            None => {
                let nearest = d.options.magnification == egui::TextureFilter::Nearest;
                self.0.insert(id, Tex { w, h, px: img.pixels.clone(), nearest });
            }
            Some([x0, y0]) => {
                let Some(t) = self.0.get_mut(&id) else { return };
                for y in 0..h {
                    for x in 0..w {
                        if x0 + x < t.w && y0 + y < t.h {
                            t.px[(y0 + y) * t.w + x0 + x] = img.pixels[y * w + x];
                        }
                    }
                }
            }
        }
    }

    /// The texture at (u, v) in 0..1, bilinear, premultiplied RGBA in 0..1 (white where the texture is not known).
    fn sample(&self, id: TextureId, u: f32, v: f32) -> [f32; 4] {
        let Some(t) = self.0.get(&id).filter(|t| t.w > 0 && t.h > 0) else { return [1.0; 4] };
        let (x, y) = if t.nearest {
            ((u * t.w as f32).floor(), (v * t.h as f32).floor())
        } else {
            (u * t.w as f32 - 0.5, v * t.h as f32 - 0.5)
        };
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let at = |xi: f32, yi: f32| {
            let c = t.px[(yi.clamp(0.0, (t.h - 1) as f32) as usize) * t.w + xi.clamp(0.0, (t.w - 1) as f32) as usize];
            [f32::from(c.r()), f32::from(c.g()), f32::from(c.b()), f32::from(c.a())]
        };
        let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
        let mut o = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * fx;
            let bottom = c[k] + (d[k] - c[k]) * fx;
            o[k] = (top + (bottom - top) * fy) / 255.0;
        }
        o
    }
}

/// An RGBA8 buffer, premultiplied, that meshes are drawn into.
struct Raster {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Raster {
    fn new(w: usize, h: usize, fill: [u8; 4]) -> Self {
        let mut px = Vec::with_capacity(w * h * 4);
        for _ in 0..w * h {
            px.extend_from_slice(&fill);
        }
        Raster { w, h, px }
    }

    /// `m` (in points, `origin` the buffer's top left) over the buffer at `ppp` pixels a point, inside `clip` (points):
    /// triangles with the vertices' colours interpolated and the texture's, blended as egui's renderer does (premultiplied,
    /// in the colours' own space).
    fn mesh(&mut self, m: &Mesh, origin: Pos2, ppp: f32, clip: Rect, tex: &Textures) {
        let to_px = |p: Pos2| pos2((p.x - origin.x) * ppp, (p.y - origin.y) * ppp);
        let cx0 = (((clip.min.x - origin.x) * ppp).round().max(0.0)) as usize;
        let cy0 = (((clip.min.y - origin.y) * ppp).round().max(0.0)) as usize;
        let cx1 = (((clip.max.x - origin.x) * ppp).round().max(0.0) as usize).min(self.w);
        let cy1 = (((clip.max.y - origin.y) * ppp).round().max(0.0) as usize).min(self.h);
        if cx0 >= cx1 || cy0 >= cy1 {
            return;
        }
        let col = |c: Color32| [f32::from(c.r()), f32::from(c.g()), f32::from(c.b()), f32::from(c.a())];
        for tri in m.indices.as_chunks::<3>().0 {
            let (Some(a), Some(b), Some(c)) =
                (m.vertices.get(tri[0] as usize), m.vertices.get(tri[1] as usize), m.vertices.get(tri[2] as usize))
            else {
                continue;
            };
            let (pa, pb, pc) = (to_px(a.pos), to_px(b.pos), to_px(c.pos));
            let area = (pb.x - pa.x) * (pc.y - pa.y) - (pb.y - pa.y) * (pc.x - pa.x);
            if area.abs() < 1e-9 {
                continue;
            }
            let x0 = (pa.x.min(pb.x).min(pc.x).floor().max(cx0 as f32)) as usize;
            let y0 = (pa.y.min(pb.y).min(pc.y).floor().max(cy0 as f32)) as usize;
            let x1 = (pa.x.max(pb.x).max(pc.x).ceil().min(cx1 as f32)).max(0.0) as usize;
            let y1 = (pa.y.max(pb.y).max(pc.y).ceil().min(cy1 as f32)).max(0.0) as usize;
            let (ca, cb, cc) = (col(a.color), col(b.color), col(c.color));
            // one texel for the triangle when its corners share a uv (every solid shape: the atlas' white pixel)
            let solid = (a.uv == b.uv && a.uv == c.uv).then(|| tex.sample(m.texture_id, a.uv.x, a.uv.y));
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = pos2(x as f32 + 0.5, y as f32 + 0.5);
                    let w0 = ((pb.x - p.x) * (pc.y - p.y) - (pb.y - p.y) * (pc.x - p.x)) / area;
                    let w1 = ((pc.x - p.x) * (pa.y - p.y) - (pc.y - p.y) * (pa.x - p.x)) / area;
                    let w2 = 1.0 - w0 - w1;
                    if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                        continue;
                    }
                    let t = solid.unwrap_or_else(|| {
                        tex.sample(
                            m.texture_id,
                            w0 * a.uv.x + w1 * b.uv.x + w2 * c.uv.x,
                            w0 * a.uv.y + w1 * b.uv.y + w2 * c.uv.y,
                        )
                    });
                    let i = (y * self.w + x) * 4;
                    let mut src = [0.0f32; 4];
                    for (k, s) in src.iter_mut().enumerate() {
                        *s = (w0 * ca[k] + w1 * cb[k] + w2 * cc[k]) * t[k];
                    }
                    let inv = 1.0 - src[3] / 255.0;
                    for (d, s) in self.px[i..i + 4].iter_mut().zip(src) {
                        *d = (s + f32::from(*d) * inv).round().clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }

    /// The pixels not premultiplied.
    fn straight(&self) -> Vec<u8> {
        let mut out = self.px.clone();
        for p in out.as_chunks_mut::<4>().0 {
            let a = u32::from(p[3]);
            if a != 0 && a != 255 {
                for c in &mut p[..3] {
                    *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal well-formedness check of an XML document: tags nest and close, attributes are quoted. Returns the
    /// count of each element's name.
    fn check_xml(xml: &str) -> HashMap<String, usize> {
        let mut counts: HashMap<String, usize> = HashMap::new();
        let mut stack: Vec<String> = Vec::new();
        let b = xml.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'<' {
                i += 1;
                continue;
            }
            if xml[i..].starts_with("<?") {
                i += xml[i..].find("?>").expect("the declaration ends") + 2;
                continue;
            }
            // a tag ends at the first '>' outside quotes
            let mut j = i + 1;
            let mut quote = 0u8;
            while j < b.len() {
                match (quote, b[j]) {
                    (0, b'"' | b'\'') => quote = b[j],
                    (q, c) if q != 0 && c == q => quote = 0,
                    (0, b'>') => break,
                    _ => {}
                }
                j += 1;
            }
            assert!(j < b.len(), "a tag is never closed: {}", &xml[i..(i + 40).min(xml.len())]);
            let tag = &xml[i + 1..j];
            if let Some(name) = tag.strip_prefix('/') {
                assert_eq!(stack.pop().as_deref(), Some(name.trim()), "mismatched closing tag {name}");
            } else {
                let selfclosing = tag.ends_with('/');
                let name: String = tag.chars().take_while(|c| !c.is_whitespace() && *c != '/').collect();
                *counts.entry(name.clone()).or_default() += 1;
                // every attribute has a quoted value
                let attrs = &tag[name.len()..];
                assert_eq!(attrs.matches('"').count() % 2, 0, "unbalanced quotes in <{tag}>");
                if !selfclosing {
                    stack.push(name);
                }
            }
            i = j + 1;
        }
        assert!(stack.is_empty(), "unclosed tags: {stack:?}");
        counts
    }

    /// A known plot: a line of three points and a text, over the whole figure.
    fn known(ui: &mut Ui) {
        let (rect, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
        let p = ui.painter_at(rect);
        let pts = vec![rect.left_bottom(), rect.center(), rect.right_top()];
        p.add(Shape::line(pts, egui::Stroke::new(1.5, Color32::from_rgb(200, 30, 30))));
        p.text(
            rect.center_top(),
            egui::Align2::CENTER_TOP,
            "Mean & trend <2020>",
            egui::FontId::proportional(8.0),
            Color32::BLACK,
        );
        p.text(rect.left_bottom(), egui::Align2::LEFT_BOTTOM, "NDVI", egui::FontId::proportional(8.0), Color32::BLUE);
    }

    #[test]
    fn svg_of_a_known_plot() {
        let o = FigureOptions { width_mm: 85.0, height_mm: 50.0, format: Format::Svg, ..Default::default() };
        let r = render(&o, &mut known);
        let svg = r.to_svg(o.width_mm, o.height_mm);
        let counts = check_xml(&svg);
        assert_eq!(counts.get("svg"), Some(&1));
        // the line is one path; the texts stay texts, one a row
        assert_eq!(counts.get("path"), Some(&1), "{svg}");
        assert_eq!(counts.get("text"), Some(&2), "{svg}");
        assert!(svg.contains("Mean &amp; trend &lt;2020&gt;"), "{svg}");
        assert!(svg.contains(">NDVI</text>"));
        assert!(svg.contains("width=\"85mm\"") && svg.contains("height=\"50mm\""));
        // a white background unless transparent
        assert!(svg.contains("fill=\"#ffffff\""));
        let t = render(&FigureOptions { transparent: true, ..o.clone() }, &mut known).to_svg(85.0, 50.0);
        assert!(!t.contains("<rect width"), "{t}");
        // the line's colour
        assert!(svg.contains("stroke=\"#c81e1e\""));
    }

    #[test]
    fn svg_text_sits_on_its_baseline() {
        // the glyph "H" has no descender: its mesh's bottom is the baseline the SVG gives
        let o = FigureOptions { width_mm: 60.0, height_mm: 30.0, font_pt: 12.0, ..Default::default() };
        let mut baseline = None;
        let r = render(&o, &mut |ui: &mut Ui| {
            let g = ui.painter().layout_no_wrap("HHH".into(), egui::FontId::proportional(12.0), Color32::BLACK);
            let (rect, _) = ui.allocate_exact_size(g.size(), egui::Sense::hover());
            baseline = Some(rect.min.y + g.rows[0].pos.y + g.rows[0].row.glyphs[0].pos.y);
            ui.painter().galley(rect.min, g, Color32::BLACK);
        });
        let svg = r.to_svg(60.0, 30.0);
        let y: f32 = svg
            .split("<text x=\"")
            .nth(1)
            .unwrap()
            .split("y=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!((y - baseline.unwrap()).abs() < 0.02);
        // and the PNG has ink around it: the H's bottom row at the baseline (a pixel dark within the row below it)
        let (rgba, w, _) = r.to_rgba();
        let row = (y * r.ppp).round() as usize;
        let dark = |row: usize| (0..w).any(|x| rgba[(row * w + x) * 4] < 100);
        assert!(dark(row - 1) && !dark(row + 2), "the baseline of the text and the SVG's agree");
    }

    #[test]
    fn png_is_mm_by_dpi() {
        for (mm, dpi) in [(85.0, 300), (180.0, 600), (85.0, 150), (123.4, 300)] {
            let o = FigureOptions { width_mm: mm, height_mm: 40.0, dpi, ..Default::default() };
            let png = render(&o, &mut known).to_png().unwrap();
            let img = image::load_from_memory(&png).unwrap();
            let want = (mm / 25.4 * dpi as f32).round() as u32;
            assert_eq!(img.width(), want, "{mm} mm at {dpi} dpi");
            assert_eq!(img.height(), (40.0 / 25.4 * dpi as f32).round() as u32);
            assert_eq!([img.width(), img.height()], o.pixels());
        }
        // 85 mm at 300 dpi: 1004 pixels
        assert_eq!(FigureOptions::default().pixels()[0], 1004);
    }

    #[test]
    fn png_has_ink_the_dpi_and_a_background() {
        let o = FigureOptions { width_mm: 40.0, height_mm: 30.0, dpi: 150, ..Default::default() };
        let r = render(&o, &mut known);
        let png = r.to_png().unwrap();
        // the pHYs chunk: 150 dpi = 5906 pixels a metre
        let at = png.windows(4).position(|w| w == b"pHYs").expect("a pHYs chunk");
        assert_eq!(u32::from_be_bytes(png[at + 4..at + 8].try_into().unwrap()), 5906);
        let (rgba, w, h) = r.to_rgba();
        assert_eq!(&rgba[..4], &[255, 255, 255, 255], "white background");
        // the red line's pixels are somewhere, and the centre of the line is red
        let is_red = |p: &[u8]| p[0] > 180 && p[1] < 80 && p[2] < 80;
        let red = rgba.as_chunks::<4>().0.iter().filter(|p| is_red(&p[..])).count();
        assert!(red > 100, "{red} red pixels");
        // near the centre (the plot's, inside its margins)
        let near = (h / 2 - 20..h / 2 + 20).flat_map(|y| (w / 2 - 20..w / 2 + 20).map(move |x| (x, y)));
        assert!(near.filter(|&(x, y)| is_red(&rgba[(y * w + x) * 4..])).count() > 5, "the line passes the centre");
        // transparent: nothing drawn is alpha 0
        let t = render(&FigureOptions { transparent: true, ..o }, &mut known);
        let (rgba, _, _) = t.to_rgba();
        assert_eq!(rgba[3], 0);
        assert!(rgba.as_chunks::<4>().0.iter().any(|p| p[3] == 255));
    }

    #[test]
    fn base64_and_crc() {
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn controls_take_no_room_in_a_figure() {
        let o = FigureOptions { width_mm: 60.0, height_mm: 30.0, format: Format::Svg, ..Default::default() };
        let r = render(&o, &mut |ui: &mut Ui| {
            chrome(ui, |ui| {
                let _ = ui.button("Copy CSV");
            });
            known(ui);
        });
        let svg = r.to_svg(60.0, 30.0);
        assert!(!svg.contains("Copy CSV"), "{svg}");
        assert_eq!(check_xml(&svg).get("path"), Some(&1));
        // and on screen (not a figure) the closure just runs
        assert!(!active());
    }

    #[test]
    fn as_shown_keeps_the_controls_room_and_cuts_them() {
        let main = Context::default();
        let seen = PanelSeen { size: vec2(300.0, 200.0), crop: 0.0, at: Pos2::ZERO, frame: 0 };
        let mut under = 0.0;
        let r = render_as_shown(&main, seen, 1, false, &|_| {}, &mut |ui: &mut Ui| {
            crate::figure::controls(ui, |ui| {
                let _ = ui.button("Copy CSV");
            });
            under = ui.available_height();
        });
        assert_eq!(r.mode, Mode::AsShown);
        // the controls' row is cut off; what is under it has the room it has on screen
        assert!(r.crop > 8.0 && r.crop < 60.0, "crop {}", r.crop);
        assert!(under < 200.0 - r.crop + 0.5 && under > 200.0 - r.crop - 14.0, "room {under}, crop {}", r.crop);
        let svg = r.to_svg_shown();
        assert!(!svg.contains("Copy CSV"), "{svg}");
        // the figure's size: the panel's width, its height less the controls; 2x has twice the pixels
        let ppp = main.pixels_per_point();
        assert_eq!(r.pixels()[0], (300.0 * ppp).round() as usize);
        assert_eq!(r.pixels()[1], ((200.0 - r.crop) * ppp).round() as usize);
        let r2 = render_as_shown(&main, seen, 2, false, &|_| {}, &mut |ui: &mut Ui| {
            crate::figure::controls(ui, |ui| {
                let _ = ui.button("Copy CSV");
            });
        });
        assert_eq!(r2.pixels()[0], 2 * r.pixels()[0]);
        assert!(r2.pixels()[1].abs_diff(2 * r.pixels()[1]) <= 1);
        // the interface's colours, not the light figure's
        assert!(!active() && !rendering());
    }

    #[test]
    fn a_panel_on_screen_is_recorded() {
        let ctx = Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(400.0, 300.0))),
            ..Default::default()
        };
        let _ = ctx.run_ui(input, |ui| {
            let m = mark(ui, 7);
            controls(ui, |ui| {
                ui.label("controls");
            });
            ui.allocate_space(vec2(100.0, 80.0));
            m.end(ui);
        });
        let s = seen(&ctx, 7).expect("the panel should be recorded");
        assert!((s.size.x - 400.0).abs() < 1.0, "{s:?}");
        assert!(s.crop > 8.0 && s.crop < 60.0, "{s:?}");
        assert!(s.size.y > s.crop + 80.0 && s.size.y <= 300.0, "{s:?}");
        assert_eq!(s.figure_size(), vec2(s.size.x, s.size.y - s.crop));
        assert_eq!(s.pixels(2.0), [(s.size.x * 2.0).round() as u32, ((s.size.y - s.crop) * 2.0).round() as u32]);
        assert!(seen(&ctx, 8).is_none());
    }

    #[test]
    fn publication_is_the_default_of_the_options_and_as_shown_of_the_dialog() {
        assert_eq!(FigureOptions::default().mode, Mode::Publication);
        assert_eq!(FigureOptions::default().scale, 1);
        assert_eq!(crate::figure_ui::FigureUi::default().opts.mode, Mode::AsShown);
    }
}
