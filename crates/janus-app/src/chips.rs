//! Chips (src/app_validation.cpp: pumpChips, uiChips): the feature being interpreted at every date of the active
//! layer, around it, read ahead for the next ones (the disk seeks happen before they are needed); a click shows a
//! date on the map, a double click makes it the date of the change.
//!
//! The cards and the pixel/point overlay follow Validation Viewer by Dr. Matthias Baumann (Humboldt-Universität zu
//! Berlin), https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used under GPL-3.0-or-later.
//! Fraction cover (the n x n cells painted over the pixel, fraction.rs) follows it too.

use std::sync::Arc;

use egui::{Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, pos2, vec2};
use janus_cube::CubeInfo;
use janus_cube::roles::{BandRoles, guess_band_roles};

/// The yellow of the pixel that contains the point (#f2c94c).
const PIXEL_YELLOW: Color32 = Color32::from_rgb(0xf2, 0xc9, 0x4c);
/// The red of the exact point (#ff4d6d).
const POINT_RED: Color32 = Color32::from_rgb(0xff, 0x4d, 0x6d);
/// The border of the card of the date shown on the map.
const SHOWN_BORDER: Color32 = Color32::from_rgb(0xfa, 0xcc, 0x15);
/// The border of the card of the date of the change.
const CHANGE_BORDER: Color32 = Color32::from_rgb(240, 70, 60);
/// The blue of a badge of the cube's own imagery (the GOOGLE of Validation Viewer).
pub(crate) const BADGE_BLUE: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
/// The height of a card's caption bar.
const CAPTION_H: f32 = 24.0;

/// The zoom and pan shared by every card, as Validation Viewer's S.views: the scale `z` (1: the whole window read)
/// and the offset (`x`, `y`) of the window's corner in card widths, always inside [1 - z, 0].
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct View {
    pub z: f32,
    pub x: f32,
    pub y: f32,
}

impl Default for View {
    fn default() -> Self {
        View { z: 1.0, x: 0.0, y: 0.0 }
    }
}

impl View {
    fn clamped(mut self, zmax: f32) -> View {
        self.z = self.z.clamp(1.0, zmax.max(1.0));
        self.x = self.x.clamp(1.0 - self.z, 0.0);
        self.y = self.y.clamp(1.0 - self.z, 0.0);
        self
    }

    /// Zoom by the factor `f` around (u, w), given as fractions of the card.
    pub(crate) fn zoom_at(self, f: f32, u: f32, w: f32, zmax: f32) -> View {
        let z = (self.z * f).clamp(1.0, zmax.max(1.0));
        View { z, x: u - (u - self.x) * z / self.z, y: w - (w - self.y) * z / self.z }.clamped(zmax)
    }

    /// Pan by (dx, dy) card widths.
    pub(crate) fn pan(self, dx: f32, dy: f32, zmax: f32) -> View {
        View { z: self.z, x: self.x + dx, y: self.y + dy }.clamped(zmax)
    }
}

/// The deepest zoom: a window of about 3 pixels.
pub(crate) fn zoom_max(w: usize) -> f32 {
    (w as f32 / 3.0).max(1.0)
}

/// The part of a chip's cell in the texture (`base`, in uv) that the view shows.
pub(crate) fn view_uv(base: Rect, v: View) -> Rect {
    Rect::from_min_size(
        base.min + vec2(-v.x / v.z * base.width(), -v.y / v.z * base.height()),
        vec2(base.width() / v.z, base.height() / v.z),
    )
}

/// A position given as a fraction of the window read, as a fraction of the card under the view.
pub(crate) fn point_in_card(frac: Pos2, v: View) -> Pos2 {
    pos2(frac.x * v.z + v.x, frac.y * v.z + v.y)
}

/// The text of the zoom control: the visible width in metres (pixels if the CRS is geographic or unknown).
pub(crate) fn view_label(w: usize, z: f32, pixel_m: Option<f64>) -> String {
    let px = w as f64 / f64::from(z);
    match pixel_m {
        Some(m) if px * m >= 10.0 => format!("{:.0} m", px * m),
        Some(m) => format!("{:.1} m", px * m),
        None if px >= 10.0 => format!("{px:.0} px"),
        None => format!("{px:.1} px"),
    }
}

/// The size of a pixel in metres, if the cube has a projected CRS and a geotransform.
fn pixel_metres(info: &CubeInfo) -> Option<f64> {
    let gt = info.geo_transform?;
    let srs = gdal::spatial_ref::SpatialRef::from_wkt(&info.crs_wkt).ok()?;
    if srs.is_geographic() {
        return None;
    }
    Some(gt[1].hypot(gt[4]) * srs.linear_units())
}

/// The band combination of the cube's cards, as Validation Viewer's #comboSel: the map's own colours, or three
/// bands as red, green and blue.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Combo {
    /// what the map shows (its composite if it is on, else the quantity with its colormap)
    #[default]
    Map,
    /// true colour: red, green, blue
    Rgb,
    /// colour infrared: NIR, red, green
    Cir,
    /// SWIR1 (else SWIR2), NIR, red
    Swir,
}

impl Combo {
    pub(crate) const ALL: [Combo; 4] = [Combo::Map, Combo::Rgb, Combo::Cir, Combo::Swir];

    /// The name kept in the settings.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Combo::Map => "map",
            Combo::Rgb => "rgb",
            Combo::Cir => "cir",
            Combo::Swir => "swir",
        }
    }

    /// The combination of a name kept in the settings (the map's if unknown).
    pub(crate) fn parse(s: &str) -> Combo {
        let s = s.trim().to_ascii_lowercase();
        Combo::ALL.into_iter().find(|c| c.key() == s).unwrap_or_default()
    }

    fn label(self) -> &'static str {
        match self {
            Combo::Map => "Map",
            Combo::Rgb => "RGB",
            Combo::Cir => "CIR",
            Combo::Swir => "SWIR",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Combo::Map => "As the map shows the layer (its band combination if on, else its colours)",
            Combo::Rgb => "True colour: red, green, blue",
            Combo::Cir => "Colour infrared: NIR, red, green",
            Combo::Swir => "SWIR1 (else SWIR2), NIR, red",
        }
    }

    /// For each channel the roles (see `guess_band_roles`) that can fill it, first choice first, and the wavelength
    /// in nm of its band for a cube whose bands only have that.
    fn channels(self) -> Option<([&'static [&'static str]; 3], [f64; 3])> {
        match self {
            Combo::Map => None,
            Combo::Rgb => Some(([&["red"], &["green"], &["blue"]], [650.0, 560.0, 480.0])),
            Combo::Cir => Some(([&["nir"], &["red"], &["green"]], [860.0, 650.0, 560.0])),
            Combo::Swir => Some(([&["swir1", "swir2"], &["nir"], &["red"]], [1610.0, 860.0, 650.0])),
        }
    }
}

/// The bands (1-based, in the date's bands) of a combination, if the cube has them all: by the roles the band names
/// give, else by wavelength (`band_nm`, the nearest within 60 nm). The map's combination has none.
pub(crate) fn combo_bands_of(roles: &BandRoles, band_nm: &[f64], c: Combo) -> Option<[i32; 3]> {
    let (want, nm) = c.channels()?;
    let mut out = [0i32; 3];
    for k in 0..3 {
        out[k] = want[k].iter().find_map(|r| roles.get(r).copied()).unwrap_or(0);
        if out[k] == 0 {
            let mut d = 60.0;
            for (b, &w) in band_nm.iter().enumerate() {
                if (w - nm[k]).abs() < d {
                    d = (w - nm[k]).abs();
                    out[k] = b as i32 + 1;
                }
            }
        }
    }
    out.iter().all(|&b| b > 0).then_some(out)
}

/// The 2nd and 98th percentiles of each of the 3 bands over all the dates of a chip set ([t][band][y][x], windows
/// of `w` x `w`), the valid values together, so that dates compare. A band without values gets (0, 1).
pub(crate) fn percentile_stretch(v: &[f32], t_n: usize, w: usize) -> ([f32; 3], [f32; 3]) {
    let (mut lo, mut hi) = ([0.0f32; 3], [1.0f32; 3]);
    for b in 0..3 {
        let mut vals: Vec<f32> = Vec::new();
        for t in 0..t_n {
            let base = (t * 3 + b) * w * w;
            if let Some(chunk) = v.get(base..base + w * w) {
                vals.extend(chunk.iter().copied().filter(|x| x.is_finite()));
            }
        }
        if vals.is_empty() {
            continue;
        }
        vals.sort_unstable_by(f32::total_cmp);
        let q = |p: f64| vals[(p * (vals.len() - 1) as f64) as usize];
        let (a, mut z) = (q(0.02), q(0.98));
        if z <= a {
            z = a + 1e-6;
        }
        (lo[b], hi[b]) = (a, z);
    }
    (lo, hi)
}

/// The caption bar of a card.
pub(crate) struct CardCaption {
    /// the year (in strong text, left)
    pub year: String,
    /// the source's badge: its text and colour
    pub badge: (String, Color32),
    /// the text at the right (the month-day)
    pub right: String,
    /// "+n": the other images of the year
    pub extra: Option<u32>,
    /// the border of a highlighted card
    pub border: Option<Color32>,
}

/// The cells of the pixel's subgrid in the label mode "Fraction cover": the grid's side and the class colour of each
/// cell, row by row from the top left (None: not painted).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cells {
    pub n: usize,
    pub colors: Vec<Option<Color32>>,
}

/// The overlay of a card, in fractions of the card's image.
pub(crate) struct Overlay {
    /// the footprint of the pixel that contains the point
    pub pixel: Rect,
    /// the exact point
    pub point: Pos2,
    /// the 3 x 3 subgrid in the pixel and the dashed neighbours
    pub grid: bool,
    /// the n x n cells painted instead of the 3 x 3 subgrid (the label mode "Fraction cover")
    pub cells: Option<Cells>,
}

impl Overlay {
    /// The overlay for a point at `frac` (a fraction of the window of `w` pixels read, the point in the pixel
    /// (r, r) of it) under the view.
    pub(crate) fn new(r: i32, w: usize, frac: Pos2, v: View, grid: bool) -> Overlay {
        let (a, b) = (r as f32 / w as f32, (r + 1) as f32 / w as f32);
        Overlay {
            pixel: Rect::from_min_max(point_in_card(pos2(a, a), v), point_in_card(pos2(b, b), v)),
            point: point_in_card(frac, v),
            grid,
            cells: None,
        }
    }

    /// The same overlay with the cells of the label mode "Fraction cover" (None: the 3 x 3 subgrid).
    pub(crate) fn with_cells(mut self, cells: Option<Cells>) -> Overlay {
        self.cells = cells;
        self
    }
}

fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke) {
    let c = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    for s in c.windows(2) {
        painter.extend(Shape::dashed_line(s, stroke, 3.0, 3.0));
    }
}

/// Draws the overlay over the image `img` (the painter clips to it).
pub(crate) fn draw_overlay(painter: &egui::Painter, img: Rect, o: &Overlay) {
    let painter = painter.with_clip_rect(img);
    let to = |p: Pos2| img.min + vec2(p.x * img.width(), p.y * img.height());
    let pix = Rect::from_min_max(to(o.pixel.min), to(o.pixel.max));
    let (pw, ph) = (pix.width(), pix.height());
    if let Some(c) = &o.cells {
        // the cells painted in their class's colour, under the grid
        for (k, col) in c.colors.iter().enumerate() {
            if let Some(col) = col {
                let (cx, cy) = ((k % c.n) as f32, (k / c.n) as f32);
                let min = pos2(pix.left() + cx * pw / c.n as f32, pix.top() + cy * ph / c.n as f32);
                let r = Rect::from_min_size(min, vec2(pw / c.n as f32, ph / c.n as f32));
                painter.rect_filled(
                    r,
                    0.0,
                    Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), crate::fraction::FILL_ALPHA),
                );
            }
        }
    }
    if o.grid {
        let white = Stroke::new(1.0, Color32::from_white_alpha(128));
        for dy in -1..=1 {
            for dx in -1..=1 {
                if dx != 0 || dy != 0 {
                    dashed_rect(&painter, pix.translate(vec2(dx as f32 * pw, dy as f32 * ph)), white);
                }
            }
        }
    }
    // the subgrid in the pixel: 3 x 3, or the n x n cells of the fraction mode (always drawn there)
    if o.grid || o.cells.is_some() {
        let thin = Stroke::new(1.0, PIXEL_YELLOW.gamma_multiply(0.5));
        let sub = o.cells.as_ref().map_or(3, |c| c.n.max(1));
        for i in 1..sub {
            let f = i as f32 / sub as f32;
            painter.line_segment([pos2(pix.left() + f * pw, pix.top()), pos2(pix.left() + f * pw, pix.bottom())], thin);
            painter.line_segment([pos2(pix.left(), pix.top() + f * ph), pos2(pix.right(), pix.top() + f * ph)], thin);
        }
    }
    painter.rect_stroke(pix, 0.0, Stroke::new(2.0, PIXEL_YELLOW), StrokeKind::Middle);
    painter.circle(to(o.point), 3.5, POINT_RED, Stroke::new(1.0, Color32::BLACK));
}

/// A card: a square image (the part `uv` of the texture), the overlay over it and a caption bar under it. The
/// response senses clicks and drags over the whole card; the image is the card's top `size` x `size`.
pub(crate) fn card(
    ui: &mut Ui,
    tex_id: egui::TextureId,
    uv: Rect,
    size: f32,
    caption: &CardCaption,
    overlay: &Overlay,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size + CAPTION_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect.expand(3.0));
    let vis = ui.visuals();
    painter.rect_filled(rect, 4.0, vis.extreme_bg_color);
    let img = card_image_rect(rect, size);
    painter.image(tex_id, img, uv, Color32::WHITE);
    draw_overlay(&painter, img, overlay);
    // the caption: year (strong), badge, "+n", and the month-day at the right
    let (strong, weak) = (vis.strong_text_color(), vis.weak_text_color());
    let cy = img.bottom() + CAPTION_H / 2.0;
    let year_font = FontId::proportional(14.0);
    let mut x = rect.left() + 6.0;
    // faux bold: the text twice, half a pixel apart
    for dx in [0.0, 0.6] {
        painter.text(pos2(x + dx, cy), egui::Align2::LEFT_CENTER, &caption.year, year_font.clone(), strong);
    }
    x += painter.layout_no_wrap(caption.year.clone(), year_font, strong).size().x + 8.0;
    let bf = FontId::proportional(9.5);
    let g = painter.layout_no_wrap(caption.badge.0.clone(), bf, Color32::WHITE);
    let br = Rect::from_min_size(pos2(x, cy - 7.5), vec2(g.size().x + 8.0, 15.0));
    painter.rect_filled(br, 3.0, caption.badge.1);
    painter.galley(pos2(br.left() + 4.0, cy - g.size().y / 2.0), g, Color32::WHITE);
    x = br.right() + 5.0;
    if let Some(n) = caption.extra {
        painter.text(pos2(x, cy), egui::Align2::LEFT_CENTER, format!("+{n}"), FontId::proportional(11.0), weak);
    }
    painter.text(
        pos2(rect.right() - 6.0, cy),
        egui::Align2::RIGHT_CENTER,
        &caption.right,
        FontId::proportional(12.0),
        weak,
    );
    if let Some(col) = caption.border {
        painter.rect_stroke(rect, 4.0, Stroke::new(2.0, col), StrokeKind::Inside);
    }
    resp
}

/// The legend of the overlay's three symbols, under the cards.
pub(crate) fn legend(ui: &mut Ui) {
    let weak = ui.visuals().weak_text_color();
    ui.horizontal_wrapped(|ui| {
        let icon = |ui: &mut Ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, ui.visuals().extreme_bg_color);
            r
        };
        let r = icon(ui);
        ui.painter().rect_stroke(r.shrink(2.0), 0.0, Stroke::new(2.0, PIXEL_YELLOW), StrokeKind::Middle);
        ui.colored_label(weak, "pixel that contains the point \u{2013} the label applies to this square");
        ui.add_space(10.0);
        let r = icon(ui);
        ui.painter().circle(r.center(), 3.5, POINT_RED, Stroke::new(1.0, Color32::BLACK));
        ui.colored_label(weak, "exact point location");
        ui.add_space(10.0);
        let r = icon(ui);
        dashed_rect(ui.painter(), r.shrink(2.0), Stroke::new(1.0, Color32::from_white_alpha(160)));
        ui.colored_label(weak, "neighbouring pixels");
    });
}

/// The image part of a card of the given response rectangle.
pub(crate) fn card_image_rect(rect: Rect, size: f32) -> Rect {
    Rect::from_min_size(rect.min, vec2(size, size))
}

/// What a card does to the shared view: Ctrl + wheel zooms around the cursor, a drag pans (unless `painting`: the
/// drag is a stroke of the label mode "Fraction cover").
pub(crate) fn card_view_input(ui: &Ui, resp: &egui::Response, size: f32, v: View, zmax: f32, painting: bool) -> View {
    let mut out = v;
    if resp.hovered() {
        let zd = ui.input(|i| i.zoom_delta());
        if (zd - 1.0).abs() > 1e-4
            && let Some(p) = ui.input(|i| i.pointer.hover_pos())
        {
            let img = card_image_rect(resp.rect, size);
            out = out.zoom_at(zd, (p.x - img.left()) / size, (p.y - img.top()) / size, zmax);
        }
        if v.z > 1.0 {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
    }
    if resp.dragged_by(egui::PointerButton::Primary) && v.z > 1.0 && !painting {
        let d = resp.drag_delta();
        out = out.pan(d.x / size, d.y / size, zmax);
    }
    out
}

use crate::app::State;
use crate::validation::Job;

/// The window read around a feature: [t][band][y][x], 1 or 3 bands.
pub(crate) struct ChipSet {
    cube: u64,
    cx: usize,
    cy: usize,
    w: usize,
    t_n: usize,
    /// the bands read (1 or 3 bands a date): None, the shown quantity
    bands: Option<[i32; 3]>,
    rgb: bool,
    /// the stretch of the three bands over every date, if asked for (a combination of the Chips' own)
    stretch: Option<([f32; 3], [f32; 3])>,
    v: Vec<f32>,
}

struct ChipJob {
    key: u64,
    job: Job<Arc<ChipSet>>,
    set: Option<Arc<ChipSet>>,
    used: f64,
}

pub(crate) struct Chips {
    jobs: Vec<ChipJob>,
    pub radius: i32,
    pub per_year: bool,
    pub size: f32,
    /// the subgrid in the point's pixel and its dashed neighbours
    pub grid: bool,
    /// the zoom and pan shared by every card
    pub view: View,
    /// the size of a pixel in metres, of the cube with this id (its WKT parsed once)
    pixel_m: Option<(u64, Option<f64>)>,
    tex: Option<egui::TextureHandle>,
    tex_key: u64,
    cols: usize,
    cell: usize,
    tex_w: usize,
    tex_h: usize,
    /// the dates drawn, in the texture's order
    pub dates: Vec<usize>,
    /// the high-resolution imagery section (highres.rs)
    pub(crate) hr: crate::highres::HighRes,
    /// the band combination of the cube's cards
    pub combo: Combo,
    /// the bands of each combination (Rgb, Cir, Swir) of the cube with this id
    combos: Option<(u64, [Option<[i32; 3]>; 3])>,
    /// the options read from the settings (once)
    loaded: bool,
}

impl Default for Chips {
    fn default() -> Self {
        Chips {
            jobs: Vec::new(),
            radius: 12,
            per_year: false,
            size: 220.0,
            grid: true,
            view: View::default(),
            pixel_m: None,
            tex: None,
            tex_key: 0,
            cols: 0,
            cell: 0,
            tex_w: 0,
            tex_h: 0,
            dates: Vec::new(),
            hr: Default::default(),
            combo: Combo::Map,
            combos: None,
            loaded: false,
        }
    }
}

impl Chips {
    pub fn has_texture(&self) -> bool {
        self.tex.is_some()
    }

    /// The bands of a combination of the cube (None: the map's, or the cube lacks them).
    fn bands_of(&self, c: Combo) -> Option<[i32; 3]> {
        let i = Combo::ALL.iter().position(|&x| x == c)?.checked_sub(1)?;
        self.combos.and_then(|(_, b)| b[i])
    }

    /// The combination drawn: the chosen one if the cube has its bands, else the map's.
    pub(crate) fn active_combo(&self) -> Combo {
        if self.bands_of(self.combo).is_some() { self.combo } else { Combo::Map }
    }
}

/// The window of 2r + 1 pixels around (cx, cy) at every date: the shown quantity, or the composite's 3 bands.
fn read_chips(
    info: Arc<CubeInfo>,
    cx: usize,
    cy: usize,
    r: i64,
    bands: Option<[i32; 3]>,
    stretch: bool,
) -> Arc<ChipSet> {
    let w = (2 * r + 1) as usize;
    let nb = if bands.is_some() { 3 } else { 1 };
    let mut c = ChipSet {
        cube: info.id,
        cx,
        cy,
        w,
        t_n: info.t(),
        bands,
        rgb: bands.is_some(),
        stretch: None,
        v: vec![f32::NAN; info.t() * nb * w * w],
    };
    let (wx0, wy0) = (cx as i64 - r, cy as i64 - r);
    // the part of the window on the layer
    let (x0, y0) = (wx0.max(0) as usize, wy0.max(0) as usize);
    let x1 = (wx0 + w as i64).min(info.width as i64).max(0) as usize;
    let y1 = (wy0 + w as i64).min(info.height as i64).max(0) as usize;
    if x1 <= x0 || y1 <= y0 {
        return Arc::new(c);
    }
    let (bw, bh) = (x1 - x0, y1 - y0);
    let mut buf = vec![0f32; bw * bh];
    janus_cube::with_thread_reader(&info, |rd| {
        for t in 0..c.t_n {
            for b in 0..nb {
                let ok = match bands {
                    Some(bs) => rd.read_band_window(t, bs[b], (x0, y0, bw, bh), &mut buf, (bw, bh)),
                    None => rd.read_window(t, (x0, y0, bw, bh), &mut buf, (bw, bh)),
                };
                if !ok {
                    continue;
                }
                let base = (t * nb + b) * w * w;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let (dy, dx) = ((y as i64 - wy0) as usize, (x as i64 - wx0) as usize);
                        c.v[base + dy * w + dx] = buf[(y - y0) * bw + (x - x0)];
                    }
                }
            }
        }
    });
    if stretch && bands.is_some() {
        c.stretch = Some(percentile_stretch(&c.v, c.t_n, c.w));
    }
    Arc::new(c)
}

impl State {
    /// The bands the cards are read with: those of the chosen combination, else the active layer's composite if it
    /// is drawn as one, else none (the layer's quantity).
    fn chip_bands(&self) -> Option<[i32; 3]> {
        let c = &self.val.chips;
        c.bands_of(c.active_combo()).or_else(|| self.composite.as_ref().filter(|c| c.on).map(|c| c.bands))
    }

    /// The bands of the combinations of the cube open (found once for each cube).
    fn refresh_combos(&mut self) {
        let Some(s) = &self.session else { return };
        let c = &mut self.val.chips;
        if c.combos.is_none_or(|(id, _)| id != s.info.id) {
            let roles = guess_band_roles(&s.info);
            let of = |k: Combo| combo_bands_of(&roles, &s.info.band_nm, k);
            c.combos = Some((s.info.id, [of(Combo::Rgb), of(Combo::Cir), of(Combo::Swir)]));
        }
    }

    /// The Chips' and the high-resolution options between the settings and the panel: read once, written back
    /// when they change.
    fn sync_chip_settings(&mut self) {
        let force_on = self.shots.is_some();
        let no_net = self.opts.selftest_ui;
        let c = &mut self.val.chips;
        let s = &mut self.settings.s;
        if !c.loaded {
            c.loaded = true;
            c.combo = Combo::parse(&s.chip_combo);
            c.hr.load(s);
            // the UI self-test has no network (CI), the tours of the guides do
            if no_net {
                c.hr.enabled = false;
            } else if force_on {
                c.hr.enabled = true;
                c.hr.offline = false;
            }
            return;
        }
        let mut changed = false;
        if s.chip_combo != c.combo.key() {
            s.chip_combo = c.combo.key().to_string();
            changed = true;
        }
        // what a run forced is not kept as the user's choice
        changed |= c.hr.store(s, no_net || force_on, force_on);
        if changed {
            self.settings.changed();
        }
    }

    /// The current feature and the next two read (kept: the six used last).
    pub(crate) fn pump_chips(&mut self, ctx: &egui::Context) {
        self.sync_chip_settings();
        if !self.val.show_chips || self.session.is_none() {
            return;
        }
        self.refresh_combos();
        let Some(li) = self.labels.vector.and_then(|id| self.vector_index(id)) else { return };
        if self.vectors[li].anchor.is_empty() {
            return;
        }
        let info = self.session.as_ref().unwrap().info.clone();
        let bands = self.chip_bands();
        let stretch = self.val.chips.active_combo() != Combo::Map;
        let r = self.val.chips.radius;
        let key_of = |cx: usize, cy: usize| {
            let mut k = info.id.wrapping_mul(1000003) ^ (((cx as u64) << 32) | cy as u64);
            k = k.wrapping_mul(31).wrapping_add(r as u64 * 2 + u64::from(stretch));
            for b in bands.unwrap_or([0; 3]) {
                k = k.wrapping_mul(131).wrapping_add(b as u64);
            }
            k
        };
        // the current feature and the next two
        let mut want = Vec::new();
        if let Some(c) = self.labels.current {
            want.push(c);
            let mut cur = c;
            for _ in 0..2 {
                let f = &self.labels.filter;
                let nx = if !f.is_empty() {
                    f.iter().position(|&x| x == cur).and_then(|p| f.get(p + 1).copied())
                } else {
                    (cur + 1 < self.vectors[li].anchor.len()).then_some(cur + 1)
                };
                let Some(nx) = nx else { break };
                want.push(nx);
                cur = nx;
            }
        }
        let now = ctx.input(|i| i.time);
        for &fi in &want {
            let Some(&[ax, ay]) = self.vectors[li].anchor.get(fi) else { continue };
            if ax.is_nan() || ax < 0.0 || ay < 0.0 || ax >= info.width as f64 || ay >= info.height as f64 {
                continue;
            }
            let (cx, cy) = (ax as usize, ay as usize);
            let key = key_of(cx, cy);
            let chips = &mut self.val.chips;
            if let Some(j) = chips.jobs.iter_mut().find(|j| j.key == key) {
                j.used = now;
                continue;
            }
            let inf = info.clone();
            chips.jobs.push(ChipJob {
                key,
                job: Job::start(move || read_chips(inf, cx, cy, i64::from(r), bands, stretch), ctx),
                set: None,
                used: now - if Some(fi) == self.labels.current { 0.0 } else { 0.5 },
            });
        }
        let chips = &mut self.val.chips;
        for j in &mut chips.jobs {
            if let Some(s) = j.job.poll() {
                j.set = Some(s);
            }
        }
        // keep a few
        while chips.jobs.len() > 6 {
            let old = (0..chips.jobs.len()).min_by(|&a, &b| chips.jobs[a].used.total_cmp(&chips.jobs[b].used)).unwrap();
            chips.jobs.remove(old);
        }
        // the high-resolution imagery of the current point and of those around it (the window of the interpretation)
        let (hr_m, win, want_all) = (chips.hr.chip_m, chips.hr.window, chips.hr.dl_wanted);
        let (anchors, f) = (&self.vectors[li].anchor, &self.labels.filter);
        let n = if f.is_empty() { anchors.len() } else { f.len() };
        let req = |fi: usize, pos: usize| {
            let [ax, ay] = *anchors.get(fi)?;
            if ax.is_nan() || ax < 0.0 || ay < 0.0 || ax >= info.width as f64 || ay >= info.height as f64 {
                return None;
            }
            let key = crate::highres::PointKey::new(info.id, ax as usize, ay as usize, hr_m);
            Some(crate::highres::Req { key, info: info.clone(), anchor: [ax, ay], pos })
        };
        let feature = |p: usize| if f.is_empty() { Some(p) } else { f.get(p).copied() };
        let cur_pos =
            self.labels.current.and_then(|c| if f.is_empty() { Some(c) } else { f.iter().position(|&x| x == c) });
        let cur = self.labels.current.and_then(|c| req(c, cur_pos.map_or(0, |p| p + 1)));
        let near = match cur_pos {
            Some(cp) => crate::highres::window_order(cp, n, win)
                .into_iter()
                .skip(1)
                .filter_map(|p| req(feature(p)?, p + 1))
                .collect(),
            None => Vec::new(),
        };
        let all = want_all.then(|| (0..n).filter_map(|p| req(feature(p)?, p + 1)).collect());
        self.val.chips.hr.pump(ctx, crate::highres::Plan { cur, near, all, total: n });
    }

    /// The Chips window.
    pub(crate) fn chips_window(&mut self, ctx: &egui::Context) {
        if !self.val.show_chips || self.docked(crate::app::Tab::Chips) {
            return;
        }
        let mut open = true;
        crate::panels::window("Chips", ctx)
            .open(&mut open)
            .default_size([560.0, 460.0])
            .show(ctx, |ui| self.chips_ui(ui));
        if !open {
            self.val.show_chips = false;
        }
    }

    pub(crate) fn chips_ui(&mut self, ui: &mut Ui) {
        let pixel_m = self.session.as_ref().and_then(|s| {
            let c = &mut self.val.chips;
            if c.pixel_m.is_none_or(|(id, _)| id != s.info.id) {
                c.pixel_m = Some((s.info.id, pixel_metres(&s.info)));
            }
            c.pixel_m.and_then(|p| p.1)
        });
        let c = &mut self.val.chips;
        // G toggles the grid (Ctrl+G is the app's own, and a text field in focus keeps its keys)
        if !ui.ctx().egui_wants_keyboard_input() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::G))
        {
            c.grid = !c.grid;
        }
        let zw = (2 * c.radius + 1) as usize;
        let zmax = zoom_max(zw);
        let mut view = c.view.clamped(zmax);
        // wrapped: a narrow panel does not grow wider than itself (and the chips with it)
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::Slider::new(&mut c.radius, 4..=40).text("radius (px)"));
            ui.add(egui::Slider::new(&mut c.size, 120.0..=400.0).fixed_decimals(0).text("size"));
            ui.checkbox(&mut c.per_year, "one per year")
                .on_hover_text("For each year, the date nearest the map's day of the year");
            ui.checkbox(&mut c.grid, "grid")
                .on_hover_text("The 3 x 3 subgrid in the point's pixel and its neighbouring pixels (G)");
            // the zoom shared by every card: its label is the visible width
            if ui.small_button("\u{2212}").on_hover_text("Zoom out (the whole window read at most)").clicked() {
                view = view.zoom_at(1.0 / 1.25, 0.5, 0.5, zmax);
            }
            if ui
                .small_button(view_label(zw, view.z, pixel_m))
                .on_hover_text("Visible width. Click to reset the zoom. Ctrl + wheel over a card zooms, drag pans")
                .clicked()
            {
                view = View::default();
            }
            if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                view = view.zoom_at(1.25, 0.5, 0.5, zmax);
            }
        });
        c.view = view;
        let li = self.labels.vector.and_then(|id| self.vector_index(id));
        let cur = self.labels.current;
        let (Some(li), Some(s), Some(cur)) = (li, &self.session, cur) else {
            ui.label(
                "Start an interpretation (a vector layer's Interpret..., or Validation & uncertainty > Interpret the sample): the \
                 feature being interpreted is shown at every date of the active layer, around it.",
            );
            return;
        };
        let Some(&[ax, ay]) = self.vectors[li].anchor.get(cur) else { return };
        if ax.is_nan() {
            return;
        }
        let info = s.info.clone();
        let (cx, cy) = (ax as usize, ay as usize);
        self.refresh_combos();
        let rgb = self.chip_bands();
        let combo = self.val.chips.active_combo();
        let c = &self.val.chips;
        let w = (2 * c.radius + 1) as usize;
        let set = c
            .jobs
            .iter()
            .filter_map(|j| j.set.clone())
            .find(|s| s.cube == info.id && s.cx == cx && s.cy == cy && s.w == w && s.bands == rgb);
        let Some(set) = set else {
            // the imagery does not wait for the dates
            let (sz, mut grid) = (c.size, c.grid);
            let key = crate::highres::PointKey::new(info.id, cx, cy, self.val.chips.hr.chip_m);
            let mut fr = self.frac_ui();
            egui::ScrollArea::both().id_salt("chips").auto_shrink(false).show(ui, |ui| {
                crate::highres::section(ui, &mut self.val.chips.hr, Some(key), sz, &mut grid, &mut fr);
                ui.weak("Reading the dates around the point...");
            });
            self.val.chips.grid = grid;
            crate::highres::lightbox(ui.ctx(), &mut self.val.chips.hr, grid, &mut fr);
            self.paint_fraction(std::mem::take(&mut fr.ops));
            return;
        };
        // the dates shown: all, or one per year (nearest the map's day of the year)
        let t_now = self.display.t;
        let mut dates: Vec<usize> = (0..info.t()).collect();
        if c.per_year && info.time_is_date {
            // (year, day of the year)
            let doy = |day: i64| {
                let (y, m, d) = janus_zeit::numeric::civil(day);
                let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
                const BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
                (y, BEFORE[m as usize - 1] + i64::from(d) - 1 + i64::from(leap && m > 2))
            };
            let (_, d0) = doy(info.unix_day(t_now));
            let mut best: std::collections::BTreeMap<i64, (i64, usize)> = Default::default();
            for t in 0..info.t() {
                let (y, d) = doy(info.unix_day(t));
                let dist = (d - d0).abs();
                if best.get(&y).is_none_or(|b| dist < b.0) {
                    best.insert(y, (dist, t));
                }
            }
            dates = best.values().map(|b| b.1).collect();
        }
        // the colours: the map's (its colormap and range, a class's colour, or the composite's stretch)
        let cols = ((dates.len() as f64).sqrt().ceil() as usize).max(1);
        // the texture stays within 4096 pixels a side (nearest sampling keeps the pixels crisp at any scale)
        let scale = ((c.size / w as f32).ceil().max(1.0) as usize).min((4096 / (cols * w)).max(1));
        let cell = w * scale;
        let rows = dates.len().div_ceil(cols);
        let m = janus_gpu::Mode::Value as usize;
        let (lo, hi) = (self.display.ranges[m].lo, self.display.ranges[m].hi);
        let cmap = self.display.cmap[m];
        let classes = (self.classes.state == crate::classes::ClassState::On).then(|| self.classes.list.clone());
        // the stretch: a combination's (its own, over every date) or the map's composite
        let comp = match (set.stretch, combo) {
            (Some(st), _) => Some(st),
            (None, Combo::Map) => self.composite.as_ref().map(|c| (c.lo, c.hi)),
            (None, _) => None,
        };
        let mut key = (Arc::as_ptr(&set) as u64).wrapping_mul(1000003);
        for &t in &dates {
            key = key.wrapping_mul(31).wrapping_add(t as u64);
        }
        key = key.wrapping_mul(31).wrapping_add(cmap as u64 * 7 + scale as u64);
        key = key.wrapping_mul(31).wrapping_add(combo as u64);
        key = key.wrapping_mul(131).wrapping_add(u64::from(lo.to_bits()));
        key = key.wrapping_mul(131).wrapping_add(u64::from(hi.to_bits()));
        if let (true, Some((clo, chi))) = (set.rgb, comp) {
            for b in 0..3 {
                key = key.wrapping_mul(131).wrapping_add(u64::from(clo[b].to_bits()) * 7 + u64::from(chi[b].to_bits()));
            }
        }
        let c = &mut self.val.chips;
        if key != c.tex_key {
            c.tex_key = key;
            let (tw, th) = (cols * cell, rows * cell);
            let mut rgba = vec![0u8; tw * th * 4];
            let nb = if set.rgb { 3 } else { 1 };
            let lut = janus_gpu::colormaps::all()[cmap].lut();
            for (k, &t) in dates.iter().enumerate() {
                let (ox, oy) = ((k % cols) * cell, (k / cols) * cell);
                for y in 0..w {
                    for x in 0..w {
                        let mut px = [40u8, 40, 40, 255];
                        if set.rgb {
                            let (clo, chi) = comp.unwrap_or(([0.0; 3], [1.0; 3]));
                            let mut any = false;
                            for b in 0..3 {
                                let val = set.v[(t * nb + b) * w * w + y * w + x];
                                if val.is_nan() {
                                    continue;
                                }
                                any = true;
                                let s = (val - clo[b]) / (chi[b] - clo[b]).max(1e-12);
                                px[b] = (255.0 * s.clamp(0.0, 1.0)).round() as u8;
                            }
                            if !any {
                                px = [40, 40, 40, 255];
                            }
                        } else {
                            let val = set.v[t * w * w + y * w + x];
                            if !val.is_nan() {
                                let rgb = match &classes {
                                    Some(list) => {
                                        let v = val.round() as i32;
                                        list.iter().find(|e| e.value == v).map_or([77, 77, 77], |e| e.color)
                                    }
                                    None => {
                                        let s = ((val - lo) / (hi - lo).max(1e-12)).clamp(0.0, 1.0);
                                        let c = janus_gpu::colormaps::all()[cmap].sample(s);
                                        let _ = &lut;
                                        [c[0], c[1], c[2]]
                                    }
                                };
                                px = [rgb[0], rgb[1], rgb[2], 255];
                            }
                        }
                        for sy in 0..scale {
                            for sx in 0..scale {
                                let d = ((oy + y * scale + sy) * tw + ox + x * scale + sx) * 4;
                                rgba[d..d + 4].copy_from_slice(&px);
                            }
                        }
                    }
                }
            }
            let img = egui::ColorImage::from_rgba_unmultiplied([tw, th], &rgba);
            c.tex = Some(ui.ctx().load_texture("chips", img, egui::TextureOptions::NEAREST));
            (c.cols, c.cell, c.tex_w, c.tex_h) = (cols, cell, tw, th);
            c.dates = dates;
        }
        // the grid: click a date to show it on the map, double click to make it the date of the change
        let change_date = self.labels.entries.get(cur).map(|e| e.date.clone()).unwrap_or_default();
        let c = &self.val.chips;
        let (Some(tex), sz) = (c.tex.clone(), c.size) else { return };
        let dates = c.dates.clone();
        let (cols, cell, tw, th, r) = (c.cols, c.cell as f32, c.tex_w as f32, c.tex_h as f32, c.radius);
        let mut go: Option<(usize, bool)> = None;
        let (mut grid, view) = (c.grid, c.view);
        let hr_key = crate::highres::PointKey::new(info.id, cx, cy, c.hr.chip_m);
        let layer_name = std::path::Path::new(&info.layers[0].path)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let w_win = (2 * r + 1) as usize;
        // the point's place in the window read: the fraction of its pixel is the anchor's
        let frac = pos2(
            (r as f32 + (ax - cx as f64) as f32) / w_win as f32,
            (r as f32 + (ay - cy as f64) as f32) / w_win as f32,
        );
        let source = {
            let stem = std::path::Path::new(&info.layers[0].path)
                .file_stem()
                .map(|s| s.to_string_lossy().to_uppercase())
                .unwrap_or_default();
            if stem.chars().count() > 8 {
                format!("{}\u{2026}", stem.chars().take(7).collect::<String>())
            } else {
                stem
            }
        };
        let mut new_view = view;
        let mut hr_legend = false;
        let mut fr = self.frac_ui();
        egui::ScrollArea::both().id_salt("chips").auto_shrink(false).show(ui, |ui| {
            hr_legend = crate::highres::section(ui, &mut self.val.chips.hr, Some(hr_key), sz, &mut grid, &mut fr);
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                ui.strong(format!("{layer_name} \u{b7} {} dates", dates.len()));
                // the combinations the cube has (the map's always; nothing to choose from without the others)
                let avail: Vec<Combo> = Combo::ALL
                    .into_iter()
                    .filter(|&k| k == Combo::Map || self.val.chips.bands_of(k).is_some())
                    .collect();
                if avail.len() > 1 {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for k in avail {
                        if ui
                            .add(egui::Button::selectable(self.val.chips.combo == k, k.label()))
                            .on_hover_text(k.tooltip())
                            .clicked()
                        {
                            self.val.chips.combo = k;
                        }
                    }
                }
            });
            // the cards that fit the panel's width (its clip: the Ui's own grows with wide content)
            let width = ui.available_width().min(ui.clip_rect().width());
            let per_row = ((width / (sz + ui.spacing().item_spacing.x)) as usize).max(1);
            for row in dates.chunks(per_row).enumerate() {
                ui.horizontal(|ui| {
                    for (j, &t) in row.1.iter().enumerate() {
                        let k = row.0 * per_row + j;
                        let base = Rect::from_min_size(
                            pos2((k % cols) as f32 * cell / tw, (k / cols) as f32 * cell / th),
                            vec2(cell / tw, cell / th),
                        );
                        let label = &info.layers[t].label;
                        let (year, md) = if info.time_is_date {
                            let (y, m, d) = janus_zeit::numeric::civil(info.unix_day(t));
                            (format!("{y}"), format!("{m:02}-{d:02}"))
                        } else {
                            (label.clone(), String::new())
                        };
                        // the date shown on the map, and the date of the change
                        let border = if !change_date.is_empty() && *label == change_date {
                            Some(CHANGE_BORDER)
                        } else if t == t_now {
                            Some(SHOWN_BORDER)
                        } else {
                            None
                        };
                        let caption =
                            CardCaption { year, badge: (source.clone(), BADGE_BLUE), right: md, extra: None, border };
                        let img_year = janus_validation::year_of(label);
                        let overlay = Overlay::new(r, w_win, frac, view, grid).with_cells(fr.cells_for(img_year));
                        let resp = card(ui, tex.id(), view_uv(base, view), sz, &caption, &overlay);
                        let paint = crate::fraction::paint_input(ui, &resp, card_image_rect(resp.rect, sz), &overlay);
                        fr.push(img_year, &paint);
                        let tip = if fr.on {
                            format!(
                                "{label}: drag over the pixel's cells to paint {img_year}, right-drag to erase; \
                                 a click elsewhere shows the date"
                            )
                        } else {
                            format!("{label}: click to show it, double click: the date of the change")
                        };
                        let resp = resp.on_hover_text(tip);
                        if paint.active {
                            // a stroke is not a click
                        } else if resp.double_clicked() {
                            go = Some((t, true));
                        } else if resp.clicked() {
                            go = Some((t, false));
                        }
                        let nv = card_view_input(ui, &resp, sz, view, zmax, paint.active);
                        if nv != view {
                            new_view = nv;
                        }
                    }
                });
            }
            // the legend is under the imagery's cards when they are there
            if !hr_legend {
                ui.add_space(6.0);
                legend(ui);
            }
        });
        self.val.chips.grid = grid;
        crate::highres::lightbox(ui.ctx(), &mut self.val.chips.hr, grid, &mut fr);
        self.paint_fraction(std::mem::take(&mut fr.ops));
        if new_view != view {
            self.val.chips.view = new_view;
            ui.ctx().request_repaint();
        }
        if let Some((t, date)) = go {
            self.set_date(t);
            if date {
                self.interp_set_date(false);
            }
            self.map.dirty = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_point_under_the_cursor() {
        let v = View::default().zoom_at(2.0, 0.25, 0.75, 8.0);
        assert_eq!(v.z, 2.0);
        // the window's fraction under the cursor before and after
        let before = (0.25, 0.75);
        let after = ((0.25 - v.x) / v.z, (0.75 - v.y) / v.z);
        assert!((before.0 - after.0).abs() < 1e-6 && (before.1 - after.1).abs() < 1e-6);
        // zooming out stops at the window read, zooming in at about 3 pixels
        assert_eq!(v.zoom_at(0.01, 0.5, 0.5, 8.0), View::default());
        assert_eq!(v.zoom_at(100.0, 0.5, 0.5, 8.0).z, 8.0);
        assert_eq!(zoom_max(25), 25.0 / 3.0);
        assert_eq!(zoom_max(1), 1.0);
    }

    #[test]
    fn pan_stays_inside_the_window() {
        let v = View { z: 2.0, x: -0.5, y: -0.5 };
        assert_eq!(v.pan(0.25, -0.75, 8.0), View { z: 2.0, x: -0.25, y: -1.0 });
        assert_eq!(v.pan(5.0, 5.0, 8.0), View { z: 2.0, x: 0.0, y: 0.0 });
    }

    #[test]
    fn view_uv_crops_the_cell() {
        let base = Rect::from_min_size(pos2(0.5, 0.25), vec2(0.25, 0.25));
        assert_eq!(view_uv(base, View::default()), base);
        let uv = view_uv(base, View { z: 2.0, x: -0.5, y: -1.0 });
        assert_eq!(uv, Rect::from_min_size(pos2(0.5 + 0.0625, 0.25 + 0.125), vec2(0.125, 0.125)));
    }

    #[test]
    fn point_and_pixel_in_the_card() {
        // radius 2: a window of 5 pixels, the point at (0.5, 0.25) of its pixel (2, 2)
        let frac = pos2((2.0 + 0.5) / 5.0, (2.0 + 0.25) / 5.0);
        let o = Overlay::new(2, 5, frac, View::default(), true);
        assert!((o.pixel.min.x - 0.4).abs() < 1e-6 && (o.pixel.max.x - 0.6).abs() < 1e-6);
        assert!((o.point.x - 0.5).abs() < 1e-6 && (o.point.y - 0.45).abs() < 1e-6);
        // zoomed by 2 with the window's centre in the card's centre: the pixel's size doubles
        let v = View { z: 2.0, x: -0.5, y: -0.5 };
        let o = Overlay::new(2, 5, frac, v, true);
        assert!((o.pixel.width() - 0.4).abs() < 1e-6);
        assert!((o.point.x - 0.5).abs() < 1e-6 && (o.point.y - 0.4).abs() < 1e-6);
    }

    #[test]
    fn the_zoom_label_is_the_visible_width() {
        assert_eq!(view_label(25, 1.0, Some(10.0)), "250 m");
        assert_eq!(view_label(25, 5.0, Some(10.0)), "50 m");
        assert_eq!(view_label(25, 25.0 / 3.0, Some(10.0)), "30 m");
        assert_eq!(view_label(25, 5.0, Some(0.5)), "2.5 m");
        assert_eq!(view_label(25, 1.0, None), "25 px");
        assert_eq!(view_label(25, 25.0 / 3.0, None), "3.0 px");
    }

    fn roles(v: &[(&'static str, i32)]) -> BandRoles {
        v.iter().copied().collect()
    }

    #[test]
    fn combos_find_their_bands() {
        let all = roles(&[("blue", 1), ("green", 2), ("red", 3), ("nir", 4), ("swir1", 5), ("swir2", 6)]);
        assert_eq!(combo_bands_of(&all, &[], Combo::Map), None);
        assert_eq!(combo_bands_of(&all, &[], Combo::Rgb), Some([3, 2, 1]));
        assert_eq!(combo_bands_of(&all, &[], Combo::Cir), Some([4, 3, 2]));
        assert_eq!(combo_bands_of(&all, &[], Combo::Swir), Some([5, 4, 3]));
        // SWIR2 stands in for a missing SWIR1; without any SWIR the combination is not there
        let no1 = roles(&[("green", 2), ("red", 3), ("nir", 4), ("swir2", 6)]);
        assert_eq!(combo_bands_of(&no1, &[], Combo::Swir), Some([6, 4, 3]));
        assert_eq!(combo_bands_of(&no1, &[], Combo::Rgb), None);
        let rgb = roles(&[("blue", 1), ("green", 2), ("red", 3)]);
        assert_eq!(combo_bands_of(&rgb, &[], Combo::Rgb), Some([3, 2, 1]));
        assert_eq!(combo_bands_of(&rgb, &[], Combo::Cir), None);
        assert_eq!(combo_bands_of(&rgb, &[], Combo::Swir), None);
        // bands known by wavelength only: the nearest within 60 nm
        let nm = [485.0, 560.0, 660.0, 835.0, 1650.0];
        let none = BandRoles::new();
        assert_eq!(combo_bands_of(&none, &nm, Combo::Rgb), Some([3, 2, 1]));
        assert_eq!(combo_bands_of(&none, &nm, Combo::Cir), Some([4, 3, 2]));
        assert_eq!(combo_bands_of(&none, &nm, Combo::Swir), Some([5, 4, 3]));
        assert_eq!(combo_bands_of(&none, &[485.0, 560.0, 660.0], Combo::Cir), None);
    }

    #[test]
    fn combo_names_are_kept_and_read() {
        for c in Combo::ALL {
            assert_eq!(Combo::parse(c.key()), c);
        }
        assert_eq!(Combo::parse(" RGB "), Combo::Rgb);
        assert_eq!(Combo::parse("nonsense"), Combo::Map);
    }

    #[test]
    fn the_stretch_pools_every_date() {
        // 2 dates of 2 x 2 pixels, 3 bands: band 0 holds 0..=99 spread over the dates (with a NaN), band 1 one
        // value, band 2 nothing valid
        let (t_n, w) = (2usize, 10usize);
        let mut v = vec![f32::NAN; t_n * 3 * w * w];
        for t in 0..t_n {
            for i in 0..50 {
                v[(t * 3) * w * w + i] = (t * 50 + i) as f32;
            }
            v[(t * 3 + 1) * w * w] = 7.0;
        }
        let (lo, hi) = percentile_stretch(&v, t_n, w);
        // 100 sorted values 0..=99: indices floor(0.02 * 99) = 1 and floor(0.98 * 99) = 97
        assert_eq!((lo[0], hi[0]), (1.0, 97.0));
        // all the same: a range that is not empty; nothing valid: 0 to 1
        assert_eq!(lo[1], 7.0);
        assert!(hi[1] > lo[1]);
        assert_eq!((lo[2], hi[2]), (0.0, 1.0));
    }
}
