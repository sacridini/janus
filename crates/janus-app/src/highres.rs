//! High-resolution imagery in the Chips panel: the Esri World Imagery Wayback images of the point being interpreted,
//! one card a year, and a lightbox with every image of a year. The listing and the rendering run on background
//! threads (at most one listing and one rendering at a time, each using up to four connections inside
//! `janus_geo::wayback`); the UI only reads what they have finished.
//!
//! The high-resolution cards and the lightbox follow Validation Viewer by Dr. Matthias Baumann (Humboldt-Universität
//! zu Berlin), https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used under GPL-3.0-or-later.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui, pos2, vec2};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::CubeInfo;
use janus_geo::wayback::{self, RgbImage, WaybackImage};

use crate::chips::{
    CardCaption, Overlay, View, card, card_view_input, draw_overlay, legend, point_in_card, view_label, view_uv,
    zoom_max,
};
use crate::validation::Job;

/// The orange of the WAYBACK badge (#f2994a).
const BADGE_ORANGE: Color32 = Color32::from_rgb(0xf2, 0x99, 0x4a);
/// The visible width of the cards at first, in metres (Validation Viewer's hr_view_m).
const HR_VIEW_M: f32 = 60.0;
/// The chip's side in metres at first (Validation Viewer's chip_size_m).
const CHIP_M: f32 = 150.0;
/// The points kept in memory (their lists and images).
const KEEP_POINTS: usize = 12;
/// The cards' images are at most this many pixels a side (the lightbox has them at the most Wayback gives).
const CARD_PX: u32 = 640;
/// The size of a thumbnail in the lightbox's strip.
const THUMB: f32 = 84.0;

fn cache_dir() -> PathBuf {
    janus_cache::overview::default_cache_dir().join("wayback")
}

/// What identifies a point's imagery: the cube, the pixel and the chip's side in metres.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct PointKey {
    cube: u64,
    cx: usize,
    cy: usize,
    chip_m: u32,
}

impl PointKey {
    pub(crate) fn new(cube: u64, cx: usize, cy: usize, chip_m: f32) -> PointKey {
        PointKey { cube, cx, cy, chip_m: chip_m.round().max(1.0) as u32 }
    }
}

/// A point to show: its key, the cube and the anchor (the fractional pixel).
pub(crate) struct Req {
    pub key: PointKey,
    pub info: Arc<CubeInfo>,
    pub anchor: [f64; 2],
}

/// Where a chip is and what the overlay draws on it.
#[derive(Clone, Debug)]
struct Geom {
    lon: f64,
    lat: f64,
    /// the chip's centre in the cube's CRS (the centre of the pixel that contains the point)
    center: (f64, f64),
    /// the chip's side in the CRS's units
    size_crs: f64,
    z: u32,
    /// the chip's side in pixels at the finest zoom (at most Wayback's MAX_PX)
    native_px: u32,
    crs: String,
    /// the pixel of the cube and the exact point, as fractions of the chip (y down)
    pixel: Rect,
    point: Pos2,
}

/// The centre of the pixel that contains `anchor` (a fractional pixel of the cube), that pixel's footprint and the
/// anchor itself as fractions of a chip of side `size` (CRS units) centred on that centre, y down.
fn footprint(gt: &[f64; 6], anchor: [f64; 2], size: f64) -> ((f64, f64), Rect, Pos2) {
    let map = |px: f64, py: f64| (gt[0] + px * gt[1] + py * gt[2], gt[3] + px * gt[4] + py * gt[5]);
    let (cx, cy) = (anchor[0].floor(), anchor[1].floor());
    let center = map(cx + 0.5, cy + 0.5);
    let frac = |p: (f64, f64)| pos2(((p.0 - center.0) / size + 0.5) as f32, (0.5 - (p.1 - center.1) / size) as f32);
    let corners = [(cx, cy), (cx + 1.0, cy), (cx, cy + 1.0), (cx + 1.0, cy + 1.0)].map(|(x, y)| frac(map(x, y)));
    let mut r = Rect::from_min_max(corners[0], corners[0]);
    for c in &corners[1..] {
        r.extend_with(*c);
    }
    (center, r, frac(map(anchor[0], anchor[1])))
}

fn make_geom(info: &CubeInfo, anchor: [f64; 2], chip_m: f64) -> Result<Geom, String> {
    let gt = info.geo_transform.ok_or("the cube has no geotransform, so the imagery cannot be placed")?;
    let mut srs =
        SpatialRef::from_wkt(&info.crs_wkt).map_err(|_| "the cube has no CRS, so the imagery cannot be placed")?;
    if srs.is_geographic() {
        return Err("the cube's CRS is geographic; the chips' size in metres needs a projected CRS".into());
    }
    srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let size_crs = chip_m / srs.linear_units();
    let (center, pixel, point) = footprint(&gt, anchor, size_crs);
    let mut wgs = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
    wgs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let (mut x, mut y, mut z) = ([center.0], [center.1], [0.0]);
    CoordTransform::new(&srs, &wgs)
        .and_then(|ct| ct.transform_coords(&mut x, &mut y, &mut z))
        .map_err(|e| format!("the point cannot be put on longitude and latitude: {e}"))?;
    let (lon, lat) = (x[0], y[0]);
    if !(lon.is_finite() && lat.is_finite() && lat.abs() < 85.0) {
        return Err("the point is outside the area Web Mercator covers".into());
    }
    let zoom = wayback::zoom_for(chip_m, lat, wayback::MAX_Z);
    Ok(Geom {
        lon,
        lat,
        center,
        size_crs,
        z: zoom,
        native_px: wayback::out_pixels(chip_m, lat, zoom),
        crs: info.crs_wkt.clone(),
        pixel,
        point,
    })
}

/// What a listing job gives: the images of a point, or why not.
type Listing = Result<Vec<WaybackImage>, String>;

#[derive(Clone)]
enum ListState {
    Pending,
    Running,
    Done(Arc<Vec<WaybackImage>>),
    Failed(String),
}

enum Img {
    Ready(TextureHandle),
    Failed(String),
}

struct Point {
    key: PointKey,
    geom: Result<Geom, String>,
    list: ListState,
    /// the cards' images, by acquisition date
    imgs: HashMap<String, Img>,
    used: f64,
}

struct RenderJob {
    key: PointKey,
    date: String,
    big: bool,
    job: Job<Result<RgbImage, String>>,
}

struct Lightbox {
    key: PointKey,
    year_i: usize,
    img_i: usize,
    /// the images of the year at the finest size, by date
    big: HashMap<String, Img>,
    view: View,
    /// the egui pass that drew it last (a lightbox not drawn any more lets go of the keys)
    drawn: u64,
}

/// The images of a year: their indices in the point's list (by date) and the one a card shows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct YearGroup {
    pub year: i32,
    pub images: Vec<usize>,
    pub default: usize,
}

/// The years of `images` (by date) with their images, and for each the card's choice (`wayback::pick_per_year`).
pub(crate) fn year_groups(images: &[WaybackImage], months: &[u32]) -> Vec<YearGroup> {
    let year_of = |im: &WaybackImage| im.date.get(0..4).and_then(|s| s.parse::<i32>().ok());
    wayback::pick_per_year(images, months)
        .into_iter()
        .map(|(year, pick, _)| {
            let list: Vec<usize> = (0..images.len()).filter(|&i| year_of(&images[i]) == Some(year)).collect();
            let default = list.iter().position(|&i| i == pick).unwrap_or(0);
            YearGroup { year, images: list, default }
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Step {
    PrevImage,
    NextImage,
    PrevYear,
    NextYear,
}

/// The lightbox's (year, image) after a key: images step inside the year and stop at its ends; years step to the
/// previous or next one, at the image its card shows.
pub(crate) fn lb_step(groups: &[YearGroup], cur: (usize, usize), s: Step) -> (usize, usize) {
    if groups.is_empty() {
        return (0, 0);
    }
    let y = cur.0.min(groups.len() - 1);
    let i = cur.1.min(groups[y].images.len().saturating_sub(1));
    match s {
        Step::PrevImage => (y, i.saturating_sub(1)),
        Step::NextImage => (y, (i + 1).min(groups[y].images.len().saturating_sub(1))),
        Step::PrevYear if y > 0 => (y - 1, groups[y - 1].default),
        Step::NextYear if y + 1 < groups.len() => (y + 1, groups[y + 1].default),
        _ => (y, i),
    }
}

/// The text at the right of a card's caption: "MM-DD  \u{b7}  0.20 m  \u{b7}  description".
pub(crate) fn caption_right(im: &WaybackImage) -> String {
    let mut parts = vec![im.date.get(5..10).unwrap_or(&im.date).to_string()];
    if let Some(r) = im.res_m {
        parts.push(format!("{r:.2} m"));
    }
    if !im.desc.is_empty() {
        parts.push(im.desc.clone());
    }
    parts.join(" \u{b7} ")
}

/// `s` cut to at most `max` characters, with an ellipsis if it was cut.
pub(crate) fn fit_text(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    format!("{}\u{2026}", s.chars().take(max - 1).collect::<String>().trim_end())
}

/// The cards' view at first: `HR_VIEW_M` metres of the chip, centred.
pub(crate) fn initial_view(chip_m: f32) -> View {
    let z = (chip_m / HR_VIEW_M).max(1.0);
    View { z, x: (1.0 - z) / 2.0, y: (1.0 - z) / 2.0 }
}

fn overlay(g: &Geom, v: View, grid: bool) -> Overlay {
    Overlay {
        pixel: Rect::from_min_max(point_in_card(g.pixel.min, v), point_in_card(g.pixel.max, v)),
        point: point_in_card(g.point, v),
        grid,
    }
}

fn to_texture(ctx: &egui::Context, im: &RgbImage) -> TextureHandle {
    let img = egui::ColorImage::from_rgb([im.w as usize, im.h as usize], &im.data);
    ctx.load_texture("wayback", img, egui::TextureOptions::LINEAR)
}

/// The state of the high-resolution section and its lightbox.
pub(crate) struct HighRes {
    /// the section on (off: no request to Esri at all)
    pub enabled: bool,
    /// the chip's side in metres
    pub chip_m: f32,
    chip_edit: f32,
    /// the months (January first) that choose a year's image; none: its last
    pub months: [bool; 12],
    /// the zoom and pan shared by the cards (None: not set yet for this chip size)
    view: Option<View>,
    points: Vec<Point>,
    list_job: Option<(PointKey, Job<Listing>)>,
    render_job: Option<RenderJob>,
    cur: Option<PointKey>,
    lb: Option<Lightbox>,
    placeholder: Option<TextureHandle>,
}

impl Default for HighRes {
    fn default() -> Self {
        HighRes {
            enabled: true,
            chip_m: CHIP_M,
            chip_edit: CHIP_M,
            months: [false; 12],
            view: None,
            points: Vec::new(),
            list_job: None,
            render_job: None,
            cur: None,
            lb: None,
            placeholder: None,
        }
    }
}

impl HighRes {
    fn months_list(&self) -> Vec<u32> {
        (1..=12u32).filter(|&m| self.months[m as usize - 1]).collect()
    }

    fn point(&self, key: PointKey) -> Option<&Point> {
        self.points.iter().find(|p| p.key == key)
    }

    fn images_of(&self, key: PointKey) -> Option<Arc<Vec<WaybackImage>>> {
        match &self.point(key)?.list {
            ListState::Done(v) => Some(v.clone()),
            _ => None,
        }
    }

    fn placeholder(&mut self, ctx: &egui::Context) -> egui::TextureId {
        self.placeholder
            .get_or_insert_with(|| {
                let img = egui::ColorImage::filled([1, 1], Color32::from_gray(40));
                ctx.load_texture("wayback-placeholder", img, egui::TextureOptions::NEAREST)
            })
            .id()
    }

    /// Forget the failures of a point, so that its listing and images are asked again.
    fn retry(&mut self, key: PointKey) {
        if let Some(p) = self.points.iter_mut().find(|p| p.key == key) {
            if matches!(p.list, ListState::Failed(_)) && p.geom.is_ok() {
                p.list = ListState::Pending;
            }
            p.imgs.retain(|_, i| matches!(i, Img::Ready(_)));
        }
        if let Some(lb) = self.lb.as_mut().filter(|l| l.key == key) {
            lb.big.retain(|_, i| matches!(i, Img::Ready(_)));
        }
    }

    fn ensure(&mut self, r: &Req, now: f64, touch: bool) {
        if let Some(p) = self.points.iter_mut().find(|p| p.key == r.key) {
            if touch {
                p.used = now;
            }
            return;
        }
        let geom = make_geom(&r.info, r.anchor, f64::from(r.key.chip_m));
        let list = match &geom {
            Ok(_) => ListState::Pending,
            Err(e) => ListState::Failed(e.clone()),
        };
        self.points.push(Point { key: r.key, geom, list, imgs: HashMap::new(), used: now });
    }

    fn evict(&mut self) {
        while self.points.len() > KEEP_POINTS {
            let cur = self.cur;
            let Some(old) = (0..self.points.len())
                .filter(|&i| Some(self.points[i].key) != cur)
                .min_by(|&a, &b| self.points[a].used.total_cmp(&self.points[b].used))
            else {
                break;
            };
            self.points.remove(old);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((key, job)) = self.list_job.as_mut()
            && let Some(r) = job.poll()
        {
            let key = *key;
            self.list_job = None;
            if let Some(p) = self.points.iter_mut().find(|p| p.key == key) {
                p.list = match r {
                    Ok(v) => ListState::Done(Arc::new(v)),
                    Err(e) => ListState::Failed(e),
                };
            }
        }
        if let Some(rj) = self.render_job.as_mut()
            && let Some(r) = rj.job.poll()
        {
            let (key, date, big) = (rj.key, std::mem::take(&mut rj.date), rj.big);
            self.render_job = None;
            let img = match r {
                Ok(im) => Img::Ready(to_texture(ctx, &im)),
                Err(e) => Img::Failed(e),
            };
            if big {
                if let Some(lb) = self.lb.as_mut().filter(|l| l.key == key) {
                    lb.big.insert(date, img);
                }
            } else if let Some(p) = self.points.iter_mut().find(|p| p.key == key) {
                p.imgs.insert(date, img);
            }
        }
    }

    /// The listing and rendering that go on in the background. `cur` is the point being interpreted, `next` the
    /// one after it (only listed, once the current one is). Call it every frame, before the keys are read.
    pub(crate) fn pump(&mut self, ctx: &egui::Context, cur: Option<Req>, next: Option<Req>) {
        let now = ctx.input(|i| i.time);
        self.poll(ctx);
        self.cur = cur.as_ref().map(|r| r.key);
        if !self.enabled || self.cur.is_none() {
            self.lb = None;
            return;
        }
        let Some(cur) = cur else { return };
        self.ensure(&cur, now, true);
        self.lightbox_keys(ctx);
        let cur_ready = self.point(cur.key).is_some_and(|p| !matches!(p.list, ListState::Pending | ListState::Running));
        let next_key = next.as_ref().map(|n| n.key);
        if cur_ready && let Some(n) = &next {
            self.ensure(n, now - 1.0, false);
        }
        self.evict();
        self.start_list(ctx, cur.key, next_key);
        self.start_render(ctx);
    }

    fn start_list(&mut self, ctx: &egui::Context, cur: PointKey, next: Option<PointKey>) {
        if self.list_job.is_some() {
            return;
        }
        for key in [Some(cur), next].into_iter().flatten() {
            let Some(p) = self.points.iter_mut().find(|p| p.key == key) else { continue };
            match (&p.list, &p.geom) {
                (ListState::Pending, Ok(g)) => {
                    let (lon, lat, z) = (g.lon, g.lat, g.z);
                    p.list = ListState::Running;
                    self.list_job = Some((key, Job::start(move || wayback::list(lon, lat, z, &cache_dir()), ctx)));
                    return;
                }
                // the current point's listing goes before the next one's
                (ListState::Running, _) => return,
                _ => {}
            }
        }
    }

    /// The next image to render: the lightbox's (the one shown first), then the cards', in order.
    fn next_render(&self) -> Option<(PointKey, String, bool)> {
        let cur = self.cur?;
        let images = self.images_of(cur)?;
        let groups = year_groups(&images, &self.months_list());
        if let Some(lb) = self.lb.as_ref().filter(|l| l.key == cur)
            && let Some(g) = groups.get(lb.year_i)
        {
            let first = g.images.get(lb.img_i).copied();
            for i in first.into_iter().chain(g.images.iter().copied()) {
                if !lb.big.contains_key(&images[i].date) {
                    return Some((cur, images[i].date.clone(), true));
                }
            }
        }
        let p = self.point(cur)?;
        groups
            .iter()
            .map(|g| g.images[g.default])
            .find(|&i| !p.imgs.contains_key(&images[i].date))
            .map(|i| (cur, images[i].date.clone(), false))
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        if self.render_job.is_some() {
            return;
        }
        let Some((key, date, big)) = self.next_render() else { return };
        let Some(images) = self.images_of(key) else { return };
        let Some(im) = images.iter().find(|i| i.date == date) else { return };
        let Some(Ok(g)) = self.point(key).map(|p| p.geom.clone()) else { return };
        let num = im.release;
        let px = if big { g.native_px } else { g.native_px.min(CARD_PX) };
        let job = Job::start(
            move || {
                let dir = cache_dir();
                let rel = wayback::releases(&dir)?
                    .into_iter()
                    .find(|r| r.num == num)
                    .ok_or_else(|| format!("Wayback's release {num} is not in the list any more"))?;
                wayback::render(&rel, g.z, &g.crs, g.center, g.size_crs, px, &dir)
            },
            ctx,
        );
        self.render_job = Some(RenderJob { key, date, big, job });
    }

    fn open_lightbox(&mut self, ctx: &egui::Context, key: PointKey, year_i: usize, img_i: usize) {
        self.lb = Some(Lightbox {
            key,
            year_i,
            img_i,
            big: HashMap::new(),
            view: initial_view(self.chip_m),
            drawn: ctx.cumulative_pass_nr(),
        });
    }

    /// The lightbox takes its keys before the rest of the app sees them: Left and Right the image of the year, Up
    /// and Down the year, Esc closes.
    fn lightbox_keys(&mut self, ctx: &egui::Context) {
        let Some(lb) = self.lb.as_ref() else { return };
        if Some(lb.key) != self.cur || lb.drawn + 3 < ctx.cumulative_pass_nr() {
            self.lb = None;
            return;
        }
        let none = egui::Modifiers::NONE;
        let (esc, left, right, up, down) = ctx.input_mut(|i| {
            (
                i.consume_key(none, egui::Key::Escape),
                i.consume_key(none, egui::Key::ArrowLeft),
                i.consume_key(none, egui::Key::ArrowRight),
                i.consume_key(none, egui::Key::ArrowUp),
                i.consume_key(none, egui::Key::ArrowDown),
            )
        });
        if esc {
            self.lb = None;
            return;
        }
        let step = if left {
            Step::PrevImage
        } else if right {
            Step::NextImage
        } else if up {
            Step::PrevYear
        } else if down {
            Step::NextYear
        } else {
            return;
        };
        self.lb_go(step);
        ctx.request_repaint();
    }

    fn lb_go(&mut self, step: Step) {
        let Some(key) = self.lb.as_ref().map(|l| l.key) else { return };
        let Some(images) = self.images_of(key) else { return };
        let groups = year_groups(&images, &self.months_list());
        if let Some(lb) = self.lb.as_mut() {
            let (y, i) = lb_step(&groups, (lb.year_i, lb.img_i), step);
            self.lb_set(&groups, y, i);
        }
    }

    fn lb_set(&mut self, groups: &[YearGroup], y: usize, i: usize) {
        let Some(key) = self.lb.as_ref().map(|l| l.key) else { return };
        let Some(images) = self.images_of(key) else { return };
        if let (Some(lb), Some(g)) = (self.lb.as_mut(), groups.get(y)) {
            (lb.year_i, lb.img_i) = (y, i);
            // only the year's images are kept
            lb.big.retain(|d, _| g.images.iter().any(|&k| images[k].date == *d));
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui) {
        egui::CollapsingHeader::new("Imagery settings").id_salt("hr-settings").show(ui, |ui| {
            ui.checkbox(&mut self.enabled, "High-resolution imagery (Esri Wayback)").on_hover_text(
                "Off: nothing is asked from Esri. The images are fetched in the background and kept on disk",
            );
            ui.horizontal(|ui| {
                ui.label("Chip size");
                let r = ui.add(egui::Slider::new(&mut self.chip_edit, 60.0..=600.0).fixed_decimals(0).suffix(" m"));
                // applied when the slider is let go (a new size asks Wayback again)
                if r.drag_stopped() || r.lost_focus() || (r.changed() && !r.dragged()) {
                    let m = self.chip_edit.round();
                    if m != self.chip_m {
                        self.chip_m = m;
                        self.view = None;
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("Phenology months").on_hover_text(
                    "A year's card shows the latest image inside these months (none chosen: the year's last image)",
                );
                ui.spacing_mut().item_spacing.x = 2.0;
                const NAMES: [&str; 12] = [
                    "January",
                    "February",
                    "March",
                    "April",
                    "May",
                    "June",
                    "July",
                    "August",
                    "September",
                    "October",
                    "November",
                    "December",
                ];
                for (i, c) in "JFMAMJJASOND".chars().enumerate() {
                    if ui.add(egui::Button::selectable(self.months[i], c.to_string())).on_hover_text(NAMES[i]).clicked()
                    {
                        self.months[i] = !self.months[i];
                    }
                }
            });
        });
    }
}

/// The high-resolution section at the top of the Chips panel. `key` is the current point (None: no valid one);
/// `sz` the card size; `grid` the overlay's subgrid. Returns whether it drew the overlay's legend.
pub(crate) fn section(ui: &mut Ui, hr: &mut HighRes, key: Option<PointKey>, sz: f32, grid: &mut bool) -> bool {
    let ctx = ui.ctx().clone();
    let weak = ui.visuals().weak_text_color();
    let snap = key.and_then(|k| hr.point(k)).map(|p| (p.geom.clone(), p.list.clone()));
    let months = hr.months_list();
    let images = match &snap {
        Some((_, ListState::Done(v))) => Some(v.clone()),
        _ => None,
    };
    let groups = images.as_ref().map(|im| year_groups(im, &months)).unwrap_or_default();
    let (mut zoom_in, mut zoom_out, mut reset, mut retry) = (false, false, false, false);
    let geom_ok = matches!(snap, Some((Ok(_), _)));
    let zmax = match &snap {
        Some((Ok(g), _)) => zoom_max((g.native_px.min(CARD_PX) / 8) as usize),
        _ => 1.0,
    };
    let chip_m = hr.chip_m;
    let view = hr.view.unwrap_or_else(|| initial_view(chip_m));
    ui.horizontal_wrapped(|ui| {
        ui.strong("High-resolution imagery");
        if !hr.enabled {
            ui.colored_label(weak, "off");
            return;
        }
        if let Some(im) = &images {
            ui.colored_label(
                weak,
                format!(
                    "{} years \u{b7} {} images \u{b7} Wayback \u{b7} {:.0} m chips",
                    groups.len(),
                    im.len(),
                    chip_m
                ),
            );
        }
        if geom_ok {
            if ui.small_button("\u{2212}").on_hover_text("Zoom out").clicked() {
                zoom_out = true;
            }
            if ui
                .small_button(view_label(chip_m.round() as usize, view.z, Some(1.0)))
                .on_hover_text("Visible width. Click to reset the zoom. Ctrl + wheel over a card zooms, drag pans")
                .clicked()
            {
                reset = true;
            }
            if ui.small_button("+").on_hover_text("Zoom in").clicked() {
                zoom_in = true;
            }
            ui.checkbox(grid, "grid")
                .on_hover_text("The 3 x 3 subgrid in the point's pixel and its neighbouring pixels (G)");
        }
    });
    hr.settings_ui(ui);
    if !hr.enabled {
        return false;
    }
    let mut view = view;
    if zoom_out {
        view = view.zoom_at(1.0 / 1.25, 0.5, 0.5, zmax);
    }
    if zoom_in {
        view = view.zoom_at(1.25, 0.5, 0.5, zmax);
    }
    if reset {
        view = initial_view(chip_m);
    }
    let (Some(key), Some((geom, list))) = (key, snap) else {
        ui.colored_label(weak, "Waiting for a point.");
        return false;
    };
    let geom = match geom {
        Ok(g) => g,
        Err(e) => {
            ui.colored_label(weak, format!("No imagery for this cube: {e}."));
            return false;
        }
    };
    let mut open: Option<usize> = None;
    let mut drew_legend = false;
    match &list {
        ListState::Pending | ListState::Running => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.colored_label(weak, "Listing the Wayback images at this point...");
            });
        }
        ListState::Failed(e) => {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(Color32::from_rgb(240, 90, 80), e);
                retry = ui.button("Retry").clicked();
            });
        }
        ListState::Done(images) => {
            if groups.is_empty() {
                ui.colored_label(weak, "Wayback has no image of this place.");
            }
            let ph = hr.placeholder(&ctx);
            let p = hr.point(key);
            let state = |date: &str| p.and_then(|p| p.imgs.get(date));
            let ready = groups.iter().filter(|g| state(&images[g.images[g.default]].date).is_some()).count();
            let failed = groups
                .iter()
                .filter(|g| matches!(state(&images[g.images[g.default]].date), Some(Img::Failed(_))))
                .count();
            if ready < groups.len() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.colored_label(weak, format!("loading\u{2026} {ready} / {} images", groups.len()));
                });
            }
            if failed > 0 {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(Color32::from_rgb(240, 90, 80), format!("{failed} images failed (hover a card)"));
                    retry = ui.button("Retry").clicked();
                });
            }
            let width = ui.available_width().min(ui.clip_rect().width());
            let per_row = ((width / (sz + ui.spacing().item_spacing.x)) as usize).max(1);
            let unit = Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0));
            let chars = ((sz - 150.0) / 6.2).max(0.0) as usize;
            let mut new_view = view;
            for row in groups.chunks(per_row) {
                ui.horizontal(|ui| {
                    for g in row {
                        let im = &images[g.images[g.default]];
                        let (tex, note) = match state(&im.date) {
                            Some(Img::Ready(t)) => (t.id(), None),
                            Some(Img::Failed(e)) => (ph, Some(format!("failed: {e}"))),
                            None => (ph, Some("loading\u{2026}".to_string())),
                        };
                        let extra = g.images.len() - 1;
                        let caption = CardCaption {
                            year: g.year.to_string(),
                            badge: ("WAYBACK".into(), BADGE_ORANGE),
                            right: fit_text(&caption_right(im), chars),
                            extra: (extra > 0).then_some(extra as u32),
                            border: None,
                        };
                        let resp = card(ui, tex, view_uv(unit, view), sz, &caption, &overlay(&geom, view, *grid));
                        if let Some(n) = &note {
                            let r = crate::chips::card_image_rect(resp.rect, sz);
                            let short = fit_text(n, ((sz - 12.0) / 6.0) as usize);
                            ui.painter().text(
                                r.center(),
                                egui::Align2::CENTER_CENTER,
                                short,
                                egui::FontId::proportional(12.0),
                                Color32::from_gray(190),
                            );
                        }
                        let mut tip = format!(
                            "{} \u{b7} release {} ({})\n{}",
                            im.date,
                            im.release,
                            im.release_date,
                            caption_right(im)
                        );
                        if let Some(n) = &note {
                            tip = format!("{tip}\n{n}");
                        }
                        let resp = resp.on_hover_text(format!("{tip}\nClick to see all the images of {}", g.year));
                        if resp.clicked()
                            && let Some(k) = groups.iter().position(|x| x.year == g.year)
                        {
                            open = Some(k);
                        }
                        let nv = card_view_input(ui, &resp, sz, view, zmax);
                        if nv != view {
                            new_view = nv;
                        }
                    }
                });
            }
            view = new_view;
            ui.add_space(6.0);
            legend(ui);
            drew_legend = true;
        }
    }
    ui.colored_label(weak, format!("Imagery: Esri Wayback \u{2014} {}", wayback::ATTRIBUTION));
    ui.add_space(4.0);
    let new_view = Some(view);
    if hr.view != new_view {
        hr.view = new_view;
        ctx.request_repaint();
    }
    if retry {
        hr.retry(key);
    }
    if let Some(k) = open {
        hr.open_lightbox(&ctx, key, k, groups[k].default);
    }
    drew_legend
}

enum Act {
    Close,
    Select(usize),
    Step(Step),
}

/// The lightbox over the whole window: the image of the year at its largest, its caption, the year's thumbnails.
pub(crate) fn lightbox(ctx: &egui::Context, hr: &mut HighRes, grid: bool) {
    let Some(key) = hr.lb.as_ref().map(|l| l.key) else { return };
    let (Some(images), Some(Ok(geom))) = (hr.images_of(key), hr.point(key).map(|p| p.geom.clone())) else {
        hr.lb = None;
        return;
    };
    let groups = year_groups(&images, &hr.months_list());
    if groups.is_empty() {
        hr.lb = None;
        return;
    }
    let ph = hr.placeholder(ctx);
    let Some(lb) = hr.lb.as_mut() else { return };
    lb.drawn = ctx.cumulative_pass_nr();
    let yi = lb.year_i.min(groups.len() - 1);
    let ii = lb.img_i.min(groups[yi].images.len() - 1);
    let g = &groups[yi];
    let im = &images[g.images[ii]];
    let screen = ctx.content_rect();
    let (w, h) = (screen.width() * 0.8, screen.height() * 0.8);
    let strip_h = THUMB + 26.0;
    let side = (w - 140.0).min(h - strip_h - 110.0).max(120.0);
    let zmax = zoom_max((geom.native_px / 8) as usize);
    let mut view = lb.view;
    let mut act: Option<Act> = None;
    let weak = ctx.global_style().visuals.weak_text_color();
    let big = &lb.big;
    let tex_of = |date: &str| match big.get(date) {
        Some(Img::Ready(t)) => Some(t.id()),
        _ => None,
    };
    let m = egui::Modal::new(egui::Id::new("wayback lightbox")).show(ctx, |ui| {
        ui.set_width(w);
        ui.set_height(h);
        ui.horizontal(|ui| {
            ui.heading(format!("{} \u{b7} {}", g.year, im.date));
            ui.colored_label(weak, format!("image {} of {}", ii + 1, g.images.len()));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Close").on_hover_text("Esc").clicked() {
                    act = Some(Act::Close);
                }
                if ui.button("Next year").on_hover_text("Down").clicked() {
                    act = Some(Act::Step(Step::NextYear));
                }
                if ui.button("Previous year").on_hover_text("Up").clicked() {
                    act = Some(Act::Step(Step::PrevYear));
                }
            });
        });
        ui.horizontal(|ui| {
            ui.add_space(((w - side - 120.0) / 2.0).max(0.0));
            if ui.add_sized([40.0, side], egui::Button::new("\u{2039}")).on_hover_text("Previous image (Left)").clicked() {
                act = Some(Act::Step(Step::PrevImage));
            }
            let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click_and_drag());
            let painter = ui.painter_at(rect);
            match big.get(&im.date) {
                Some(Img::Ready(t)) => {
                    painter.image(t.id(), rect, view_uv(Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0)), view), Color32::WHITE);
                    draw_overlay(&painter, rect, &overlay(&geom, view, grid));
                }
                Some(Img::Failed(e)) => {
                    painter.rect_filled(rect, 4.0, Color32::from_gray(30));
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        fit_text(&format!("failed: {e}"), 90),
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(200),
                    );
                }
                None => {
                    painter.rect_filled(rect, 4.0, Color32::from_gray(30));
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "loading\u{2026}",
                        egui::FontId::proportional(14.0),
                        Color32::from_gray(200),
                    );
                }
            }
            view = card_view_input(ui, &resp, side, view, zmax);
            if ui.add_sized([40.0, side], egui::Button::new("\u{203a}")).on_hover_text("Next image (Right)").clicked() {
                act = Some(Act::Step(Step::NextImage));
            }
        });
        let res = im.res_m.map_or(String::new(), |r| format!(" \u{b7} {r:.2} m"));
        ui.label(format!("{}{res} \u{b7} {}", im.date, if im.desc.is_empty() { "no description" } else { &im.desc }));
        ui.colored_label(
            weak,
            format!(
                "Wayback release {} ({}) \u{b7} Left/Right: image of the year, Up/Down: year, Esc: close, Ctrl + wheel zooms, drag pans \u{b7} Imagery: Esri Wayback \u{2014} {}",
                im.release,
                im.release_date,
                wayback::ATTRIBUTION
            ),
        );
        egui::ScrollArea::horizontal().id_salt("wayback strip").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (k, &i) in g.images.iter().enumerate() {
                    let t = &images[i];
                    let (r, resp) = ui.allocate_exact_size(vec2(THUMB, THUMB), Sense::click());
                    let painter = ui.painter_at(r);
                    painter.image(
                        tex_of(&t.date).unwrap_or(ph),
                        r,
                        Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    painter.text(
                        r.left_bottom() + vec2(3.0, -2.0),
                        egui::Align2::LEFT_BOTTOM,
                        &t.date,
                        egui::FontId::proportional(10.0),
                        Color32::WHITE,
                    );
                    if k == ii {
                        painter.rect_stroke(r, 2.0, Stroke::new(2.0, Color32::from_rgb(0xf2, 0xc9, 0x4c)), StrokeKind::Inside);
                    }
                    if resp.on_hover_text(caption_right(t)).clicked() {
                        act = Some(Act::Select(k));
                    }
                }
            });
        });
    });
    lb.view = view;
    match act {
        Some(Act::Close) => hr.lb = None,
        Some(Act::Select(k)) => hr.lb_set(&groups, yi, k),
        Some(Act::Step(s)) => hr.lb_go(s),
        None if m.should_close() => hr.lb = None,
        None => {}
    }
    ctx.request_repaint_after(std::time::Duration::from_millis(100));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(date: &str, release: u32, res: Option<f64>, desc: &str) -> WaybackImage {
        WaybackImage {
            date: date.into(),
            release,
            release_date: String::new(),
            res_m: res,
            desc: desc.into(),
            zoom: 19,
        }
    }

    #[test]
    fn the_pixel_and_the_point_in_the_chip() {
        // 10 m pixels, north up; the point at (5.25, 7.5): pixel (5, 7), whose centre is the chip's centre
        let gt = [500_000.0, 10.0, 0.0, 4_000_000.0, 0.0, -10.0];
        let (c, pixel, point) = footprint(&gt, [5.25, 7.5], 150.0);
        assert_eq!(c, (500_055.0, 3_999_925.0));
        let (a, b) = (0.5 - 5.0 / 150.0, 0.5 + 5.0 / 150.0);
        assert!((pixel.min.x - a as f32).abs() < 1e-6 && (pixel.max.x - b as f32).abs() < 1e-6);
        assert!((pixel.min.y - a as f32).abs() < 1e-6 && (pixel.max.y - b as f32).abs() < 1e-6);
        // 2.5 m west of the centre, at its row
        assert!((f64::from(point.x) - (0.5 - 2.5 / 150.0)).abs() < 1e-6 && (point.y - 0.5).abs() < 1e-6);
        // a point at the pixel's north-east corner is the pixel's top-right
        let (_, p2, pt) = footprint(&gt, [6.0 - 1e-9, 7.0], 150.0);
        assert_eq!(p2, footprint(&gt, [5.5, 7.5], 150.0).1);
        assert!((pt.x - p2.max.x).abs() < 1e-4 && (pt.y - p2.min.y).abs() < 1e-4);
        // a CRS in feet: the same pixel in a chip of the size in its units
        let (_, q, _) = footprint(&gt, [5.25, 7.5], 15.0);
        assert!((q.width() - 10.0 / 15.0).abs() < 1e-6);
    }

    #[test]
    fn the_caption_text() {
        let i = img("2019-08-14", 7, Some(0.3), "Vexcel");
        assert_eq!(caption_right(&i), "08-14 \u{b7} 0.30 m \u{b7} Vexcel");
        assert_eq!(caption_right(&img("2019-08-14", 7, None, "")), "08-14");
        assert_eq!(fit_text("08-14 \u{b7} 0.30 m \u{b7} Vexcel", 14), "08-14 \u{b7} 0.30\u{2026}");
        assert_eq!(fit_text("08-14", 14), "08-14");
        assert_eq!(fit_text("abc", 0), "");
    }

    fn groups() -> Vec<YearGroup> {
        let ims = [
            img("2015-03-01", 1, None, ""),
            img("2015-07-10", 2, None, ""),
            img("2015-12-01", 3, None, ""),
            img("2016-01-05", 4, None, ""),
            img("2018-06-30", 5, None, ""),
            img("2018-08-01", 6, None, ""),
        ];
        year_groups(&ims, &[6, 7, 8])
    }

    #[test]
    fn years_group_the_images() {
        let g = groups();
        assert_eq!(
            g,
            [
                YearGroup { year: 2015, images: vec![0, 1, 2], default: 1 },
                YearGroup { year: 2016, images: vec![3], default: 0 },
                YearGroup { year: 2018, images: vec![4, 5], default: 1 },
            ]
        );
    }

    #[test]
    fn the_lightbox_steps_inside_the_year_and_between_years() {
        let g = groups();
        // images stop at the ends of the year
        assert_eq!(lb_step(&g, (0, 1), Step::NextImage), (0, 2));
        assert_eq!(lb_step(&g, (0, 2), Step::NextImage), (0, 2));
        assert_eq!(lb_step(&g, (0, 0), Step::PrevImage), (0, 0));
        assert_eq!(lb_step(&g, (2, 1), Step::PrevImage), (2, 0));
        // years land on the image their card shows, and stop at the first and the last
        assert_eq!(lb_step(&g, (0, 2), Step::NextYear), (1, 0));
        assert_eq!(lb_step(&g, (1, 0), Step::NextYear), (2, 1));
        assert_eq!(lb_step(&g, (2, 0), Step::NextYear), (2, 0));
        assert_eq!(lb_step(&g, (2, 0), Step::PrevYear), (1, 0));
        assert_eq!(lb_step(&g, (0, 1), Step::PrevYear), (0, 1));
        // out of range is clamped; nothing to step in
        assert_eq!(lb_step(&g, (9, 9), Step::PrevImage), (2, 0));
        assert_eq!(lb_step(&[], (3, 3), Step::NextYear), (0, 0));
    }

    #[test]
    fn the_cards_start_on_sixty_metres() {
        let v = initial_view(150.0);
        assert_eq!(v.z, 2.5);
        assert!((v.x + 0.75).abs() < 1e-6 && (v.y + 0.75).abs() < 1e-6);
        // a chip of 60 m or less shows all of it
        assert_eq!(initial_view(60.0), View::default());
        assert_eq!(initial_view(30.0), View::default());
    }

    #[test]
    fn a_point_key_rounds_the_chip_size() {
        assert_eq!(PointKey::new(1, 2, 3, 149.6), PointKey::new(1, 2, 3, 150.0));
        assert_ne!(PointKey::new(1, 2, 3, 150.0), PointKey::new(1, 2, 3, 200.0));
    }
}
