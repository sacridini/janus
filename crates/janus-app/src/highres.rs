//! High-resolution imagery in the Chips panel: the Esri World Imagery Wayback images of the point being interpreted,
//! one card a year, and a lightbox with every image of a year. The listing and the rendering run on background
//! threads (at most one listing and one rendering at a time, each using up to four connections inside
//! `janus_geo::wayback`); the UI only reads what they have finished.
//!
//! Around the current point the cards of the nearest ones are made ready too (the listing, then the tiles of one image
//! a year, onto the disk; nothing but the current point's cards is kept as textures), the listings are kept on disk
//! for a month (a day when some dates are missing), an Offline switch lets nothing out to the network, and "Download all points" does the same for every
//! point of the layer being interpreted.
//!
//! The high-resolution cards and the lightbox, and the prefetch of the points around the current one, the cache on
//! disk and the offline work, follow Validation Viewer by Dr. Matthias Baumann (Humboldt-Universität zu Berlin),
//! https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used under GPL-3.0-or-later.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Ui, pos2, vec2};
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::CubeInfo;
use janus_geo::wayback::{self, Net, RgbImage, WaybackImage};

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
/// The points kept in memory (their lists and images) at least.
const KEEP_POINTS: usize = 12;
/// The textures of the cards kept, in bytes (the current point's are always kept). A card is up to 640 x 640 x 4 =
/// 1.6 MB, a point 10 to 25 of them, so this is about six points; the points around the current one hold no textures
/// (their tiles are only fetched onto the disk), and a point whose textures are let go is drawn again from the disk.
const MAX_TEX_BYTES: usize = 256 << 20;
/// How many points on each side are made ready ahead (at first, at most).
pub(crate) const DEFAULT_WINDOW: usize = 5;
pub(crate) const MAX_WINDOW: usize = 20;
/// The colours of the cache card's squares.
const CELL_IDLE: Color32 = Color32::from_gray(70);
const CELL_WORKING: Color32 = Color32::from_rgb(0xf2, 0xc9, 0x4c);
const CELL_READY: Color32 = Color32::from_rgb(0x2d, 0xd4, 0xbf);
const CELL_FAILED: Color32 = Color32::from_rgb(240, 90, 80);
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
    /// the point's place in the order of the interpretation, from 1
    pub pos: usize,
}

/// What `HighRes::pump` is given every frame.
pub(crate) struct Plan {
    /// the point being interpreted
    pub cur: Option<Req>,
    /// the points around it, the nearest first (see `window_order`)
    pub near: Vec<Req>,
    /// every point of the layer, once the user asked to download them all
    pub all: Option<Vec<Req>>,
    /// how many points there are
    pub total: usize,
}

/// The places around `cur` (of `n`) to make ready, `radius` on each side: the point itself, then the nearest first,
/// the next before the previous (cur, cur+1, cur-1, cur+2, ...), those outside 0..n left out.
pub(crate) fn window_order(cur: usize, n: usize, radius: usize) -> Vec<usize> {
    if cur >= n {
        return Vec::new();
    }
    let mut v = vec![cur];
    for d in 1..=radius {
        if cur + d < n {
            v.push(cur + d);
        }
        if cur >= d {
            v.push(cur - d);
        }
    }
    v
}

/// The points whose textures are let go to bring the total under `budget` bytes: the least recently used first,
/// never `keep`. `items` are (last use, bytes of textures) of each point.
pub(crate) fn texture_evictions(items: &[(f64, usize)], keep: Option<usize>, budget: usize) -> Vec<usize> {
    let mut total: usize = items.iter().map(|i| i.1).sum();
    let mut order: Vec<usize> = (0..items.len()).filter(|&i| Some(i) != keep && items[i].1 > 0).collect();
    order.sort_by(|&a, &b| items[a].0.total_cmp(&items[b].0));
    let mut out = Vec::new();
    for i in order {
        if total <= budget {
            break;
        }
        total -= items[i].1;
        out.push(i);
    }
    out
}

/// How many points are kept in memory when the window has `window` of them.
pub(crate) fn keep_points(window: usize) -> usize {
    KEEP_POINTS.max(window + 3)
}

/// The months (January first) as the bits of a number, and back.
pub(crate) fn months_mask(m: &[bool; 12]) -> u16 {
    m.iter().enumerate().fold(0, |a, (i, &on)| a | (u16::from(on) << i))
}

pub(crate) fn months_from_mask(mask: u16) -> [bool; 12] {
    std::array::from_fn(|i| mask >> i & 1 == 1)
}

/// What a square of the cache card says about a point.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CellState {
    /// nothing yet
    Idle,
    /// being listed or fetched
    Working,
    /// the listing and the tiles of its cards are on disk
    Ready,
    /// a listing or an image failed (or is not downloaded)
    Failed,
}

/// The cache card's title: the window and how many of its points are on disk.
pub(crate) fn cache_summary(radius: usize, on_disk: usize) -> String {
    format!("window \u{b1}{radius} \u{b7} {on_disk} point{} on disk", if on_disk == 1 { "" } else { "s" })
}

/// The cache card's activity line: the places being worked on (at most four named) and how many wait.
pub(crate) fn activity_text(working: &[usize], queued: usize) -> String {
    if working.is_empty() {
        return if queued > 0 { format!("{queued} queued") } else { "idle".into() };
    }
    let names: Vec<String> = working.iter().take(4).map(|p| format!("#{p}")).collect();
    format!("working: {}{} \u{b7} {queued} queued", names.join(", "), if working.len() > 4 { " \u{2026}" } else { "" })
}

/// What a failed image says: the offline mode's refusal is not an error.
fn fail_text(e: &str) -> String {
    if e == wayback::NOT_DOWNLOADED { "not downloaded".into() } else { format!("failed: {e}") }
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
    /// the dates of the cards whose tiles are on disk
    fetched: HashSet<String>,
    /// the dates whose tiles could not be had (why), until Retry
    fetch_err: HashMap<String, String>,
    /// the next listing asks Wayback again (a Retry)
    refresh: bool,
    used: f64,
}

impl Point {
    /// The bytes of the textures the point holds.
    fn tex_bytes(&self) -> usize {
        self.imgs.values().map(|i| if let Img::Ready(t) = i { t.byte_size() } else { 0 }).sum()
    }
}

/// What a rendering job makes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// a card of the current point
    Card,
    /// an image of the lightbox, at the finest size
    Big,
    /// the tiles of a card onto the disk, no image made
    Fetch,
}

struct RenderJob {
    key: PointKey,
    date: String,
    kind: Kind,
    job: Job<Result<RgbImage, String>>,
}

/// The download of every point's cards.
struct Download {
    reqs: Vec<Req>,
    /// the next of `reqs` to take
    next: usize,
    /// the point being worked on
    cur: Option<PointKey>,
    done: usize,
    failed: usize,
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

/// The text at the right of a card's caption, with the zoom and the real ground size of a pixel where the image is
/// coarser than the zoom asked.
pub(crate) fn caption_full(im: &WaybackImage, asked_z: u32, lat: f64) -> String {
    let mut s = caption_right(im);
    if im.zoom < asked_z {
        s += &format!(" \u{b7} z{} \u{b7} {:.1} m/px", im.zoom, wayback::ground_res(lat, im.zoom));
    }
    s
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

fn overlay(g: &Geom, v: View, grid: bool, cells: Option<crate::chips::Cells>) -> Overlay {
    Overlay {
        cells,
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
    /// how many points on each side of the current one are made ready ahead
    pub window: usize,
    /// no request to the network at all: only what is on disk
    pub offline: bool,
    /// the user asked to download every point (the Chips panel gives the points)
    pub dl_wanted: bool,
    offline_seen: bool,
    points: Vec<Point>,
    list_job: Option<(PointKey, Job<Listing>)>,
    render_job: Option<RenderJob>,
    cur: Option<PointKey>,
    /// the points of the window (the current one first) with their place in the interpretation
    window_pts: Vec<(PointKey, usize)>,
    /// the points known to be ready (the listing and the cards' tiles on disk), for the months of the key
    done: HashSet<(PointKey, u16)>,
    dl: Option<Download>,
    /// how the last download ended
    dl_last: Option<String>,
    total_points: usize,
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
            window: DEFAULT_WINDOW,
            offline: false,
            dl_wanted: false,
            offline_seen: false,
            points: Vec::new(),
            list_job: None,
            render_job: None,
            cur: None,
            window_pts: Vec::new(),
            done: HashSet::new(),
            dl: None,
            dl_last: None,
            total_points: 0,
            lb: None,
            placeholder: None,
        }
    }
}

impl HighRes {
    fn months_list(&self) -> Vec<u32> {
        (1..=12u32).filter(|&m| self.months[m as usize - 1]).collect()
    }

    /// The options kept in the settings.
    pub(crate) fn load(&mut self, s: &crate::settings::Settings) {
        self.enabled = s.hr_enabled;
        self.chip_m = s.hr_chip_m as f32;
        self.chip_edit = self.chip_m;
        self.months = months_from_mask(s.hr_months);
        self.window = s.hr_window.min(MAX_WINDOW);
        self.offline = s.hr_offline;
    }

    /// The options into the settings (`keep_enabled`, `keep_offline`: those a run forced are left as they were);
    /// whether anything changed.
    pub(crate) fn store(&self, s: &mut crate::settings::Settings, keep_enabled: bool, keep_offline: bool) -> bool {
        let before = (s.hr_enabled, s.hr_chip_m, s.hr_months, s.hr_window, s.hr_offline);
        if !keep_enabled {
            s.hr_enabled = self.enabled;
        }
        if !keep_offline {
            s.hr_offline = self.offline;
        }
        s.hr_chip_m = self.chip_m.round().clamp(1.0, 5000.0) as u32;
        s.hr_months = months_mask(&self.months);
        s.hr_window = self.window.min(MAX_WINDOW);
        before != (s.hr_enabled, s.hr_chip_m, s.hr_months, s.hr_window, s.hr_offline)
    }

    fn mask(&self) -> u16 {
        months_mask(&self.months)
    }

    fn point(&self, key: PointKey) -> Option<&Point> {
        self.points.iter().find(|p| p.key == key)
    }

    fn point_mut(&mut self, key: PointKey) -> Option<&mut Point> {
        self.points.iter_mut().find(|p| p.key == key)
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

    /// Forget the failures of a point, so that its listing and images are asked again (`fresh`: the listing too
    /// from Wayback, not from the disk).
    fn retry_point(p: &mut Point, fresh: bool) {
        if matches!(p.list, ListState::Failed(_)) && p.geom.is_ok() {
            p.list = ListState::Pending;
            p.refresh = fresh;
        }
        p.imgs.retain(|_, i| matches!(i, Img::Ready(_)));
        p.fetch_err.clear();
    }

    fn retry(&mut self, key: PointKey) {
        if let Some(p) = self.point_mut(key) {
            Self::retry_point(p, true);
        }
        if let Some(lb) = self.lb.as_mut().filter(|l| l.key == key) {
            lb.big.retain(|_, i| matches!(i, Img::Ready(_)));
        }
    }

    fn new_point(r: &Req, now: f64) -> Point {
        let geom = make_geom(&r.info, r.anchor, f64::from(r.key.chip_m));
        let list = match &geom {
            Ok(_) => ListState::Pending,
            Err(e) => ListState::Failed(e.clone()),
        };
        Point {
            key: r.key,
            geom,
            list,
            imgs: HashMap::new(),
            fetched: HashSet::new(),
            fetch_err: HashMap::new(),
            refresh: false,
            used: now,
        }
    }

    fn ensure(&mut self, r: &Req, now: f64, touch: bool) {
        if let Some(p) = self.point_mut(r.key) {
            if touch {
                p.used = now;
            }
            return;
        }
        self.points.push(Self::new_point(r, now));
    }

    /// Whether a point's listing is there and the tiles of its cards (one a year, for the months chosen) are on
    /// disk (or its images drawn).
    fn point_ready(&self, p: &Point) -> bool {
        let ListState::Done(images) = &p.list else { return false };
        year_groups(images, &self.months_list()).iter().all(|g| {
            let d = &images[g.images[g.default]].date;
            p.fetched.contains(d) || matches!(p.imgs.get(d), Some(Img::Ready(_)))
        })
    }

    /// The keys that are worked on, in order: the window's (the current point first), then the download's.
    fn work_keys(&self) -> Vec<PointKey> {
        let mut v: Vec<PointKey> = self.window_pts.iter().map(|w| w.0).collect();
        if let Some(k) = self.dl.as_ref().and_then(|d| d.cur) {
            v.push(k);
        }
        v
    }

    /// What a square of the cache card shows for a point.
    fn cell(&self, key: PointKey) -> CellState {
        if self.done.contains(&(key, self.mask())) {
            return CellState::Ready;
        }
        let working = self.list_job.as_ref().is_some_and(|j| j.0 == key)
            || self.render_job.as_ref().is_some_and(|j| j.key == key);
        if working {
            return CellState::Working;
        }
        match self.point(key) {
            Some(p)
                if (matches!(p.list, ListState::Failed(_)) && p.geom.is_ok())
                    || !p.fetch_err.is_empty()
                    || p.imgs.values().any(|i| matches!(i, Img::Failed(_))) =>
            {
                CellState::Failed
            }
            _ => CellState::Idle,
        }
    }

    fn evict(&mut self) {
        let keep = keep_points(self.window_pts.len());
        let protected: HashSet<PointKey> = self.work_keys().into_iter().collect();
        while self.points.len() > keep {
            let Some(old) = (0..self.points.len())
                .filter(|&i| !protected.contains(&self.points[i].key))
                .min_by(|&a, &b| self.points[a].used.total_cmp(&self.points[b].used))
            else {
                break;
            };
            self.points.remove(old);
        }
        // the textures: the current point's stay, the least recently used of the others go
        let items: Vec<(f64, usize)> = self.points.iter().map(|p| (p.used, p.tex_bytes())).collect();
        let cur = self.points.iter().position(|p| Some(p.key) == self.cur);
        for i in texture_evictions(&items, cur, MAX_TEX_BYTES) {
            self.points[i].imgs.clear();
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some((key, job)) = self.list_job.as_mut()
            && let Some(r) = job.poll()
        {
            let key = *key;
            self.list_job = None;
            if let Some(p) = self.point_mut(key) {
                p.list = match r {
                    Ok(v) => ListState::Done(Arc::new(v)),
                    Err(e) => ListState::Failed(e),
                };
            }
        }
        if let Some(rj) = self.render_job.as_mut()
            && let Some(r) = rj.job.poll()
        {
            let (key, date, kind) = (rj.key, std::mem::take(&mut rj.date), rj.kind);
            self.render_job = None;
            match kind {
                Kind::Fetch => {
                    if let Some(p) = self.point_mut(key) {
                        match r {
                            Ok(_) => {
                                p.fetched.insert(date);
                            }
                            Err(e) => {
                                p.fetch_err.insert(date, e);
                            }
                        }
                    }
                }
                Kind::Big | Kind::Card => {
                    let ok = r.is_ok();
                    let img = match r {
                        Ok(im) => Img::Ready(to_texture(ctx, &im)),
                        Err(e) => Img::Failed(e),
                    };
                    if kind == Kind::Big {
                        if let Some(lb) = self.lb.as_mut().filter(|l| l.key == key) {
                            lb.big.insert(date, img);
                        }
                    } else if let Some(p) = self.point_mut(key) {
                        if ok {
                            p.fetched.insert(date.clone());
                        }
                        p.imgs.insert(date, img);
                    }
                }
            }
        }
    }

    /// The listing and rendering that go on in the background: the current point's cards, then, one at a time, the
    /// listing and the tiles of the points around it (nearest first), then those of the download. Call it every
    /// frame, before the keys are read.
    pub(crate) fn pump(&mut self, ctx: &egui::Context, plan: Plan) {
        let now = ctx.input(|i| i.time);
        self.poll(ctx);
        self.cur = plan.cur.as_ref().map(|r| r.key);
        self.total_points = plan.total;
        self.window_pts = plan.cur.iter().chain(&plan.near).map(|r| (r.key, r.pos)).collect();
        if self.offline != self.offline_seen {
            // what failed for lack of the network, or of the permission to use it, is asked again
            self.offline_seen = self.offline;
            for p in &mut self.points {
                Self::retry_point(p, false);
            }
            if self.offline {
                self.dl = None;
            }
        }
        if !self.enabled || self.cur.is_none() {
            self.lb = None;
            self.dl = None;
            self.dl_wanted = false;
            return;
        }
        let Some(cur) = plan.cur else { return };
        self.ensure(&cur, now, true);
        self.lightbox_keys(ctx);
        for (i, n) in plan.near.iter().enumerate() {
            self.ensure(n, now - 1.0 - i as f64 * 1e-3, false);
        }
        if let Some(all) = plan.all {
            self.dl_wanted = false;
            self.start_download(all);
        }
        self.advance_download(now);
        self.evict();
        // the points known to be ready
        let mask = self.mask();
        let ready: Vec<PointKey> =
            self.work_keys().into_iter().filter(|&k| self.point(k).is_some_and(|p| self.point_ready(p))).collect();
        self.done.extend(ready.into_iter().map(|k| (k, mask)));
        self.start_list(ctx);
        self.start_render(ctx);
        if self.list_job.is_some() || self.render_job.is_some() || self.dl.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn start_download(&mut self, reqs: Vec<Req>) {
        if self.offline || !self.enabled {
            return;
        }
        self.dl_last = None;
        self.dl = Some(Download { reqs, next: 0, cur: None, done: 0, failed: 0 });
    }

    fn cancel_download(&mut self) {
        if let Some(d) = self.dl.take() {
            self.dl_last = Some(format!("Download stopped at {} of {} points", d.done + d.failed, d.reqs.len()));
        }
    }

    /// The download goes on to the next point once the one it works on is ready (or failed).
    fn advance_download(&mut self, now: f64) {
        if self.dl.is_none() {
            return;
        }
        let mask = self.mask();
        for _ in 0..256 {
            if let Some(key) = self.dl.as_ref().and_then(|d| d.cur) {
                let p = self.point(key);
                let ready = p.is_some_and(|p| self.point_ready(p));
                let failed = p.is_none_or(|p| matches!(p.list, ListState::Failed(_)) || !p.fetch_err.is_empty());
                if !ready && !failed {
                    return;
                }
                if ready {
                    self.done.insert((key, mask));
                }
                if let Some(d) = self.dl.as_mut() {
                    if ready {
                        d.done += 1;
                    } else {
                        d.failed += 1;
                    }
                    d.cur = None;
                }
            }
            let Some(d) = self.dl.as_mut() else { return };
            let Some(i) = (d.next < d.reqs.len()).then_some(d.next) else {
                let msg = if d.failed == 0 {
                    format!("{} points downloaded", d.done)
                } else {
                    format!("{} points downloaded, {} failed (see their squares)", d.done, d.failed)
                };
                self.dl = None;
                self.dl_last = Some(msg);
                return;
            };
            d.next += 1;
            let key = d.reqs[i].key;
            if self.done.contains(&(key, mask)) {
                d.done += 1;
                continue;
            }
            d.cur = Some(key);
            // moved out of the way of `ensure`'s borrow of self
            let r = Req { key, info: d.reqs[i].info.clone(), anchor: d.reqs[i].anchor, pos: d.reqs[i].pos };
            self.ensure(&r, now - 1.0, false);
        }
    }

    fn start_list(&mut self, ctx: &egui::Context) {
        if self.list_job.is_some() {
            return;
        }
        let offline = self.offline;
        for key in self.work_keys() {
            let Some(p) = self.point_mut(key) else { continue };
            if let (ListState::Pending, Ok(g)) = (&p.list, &p.geom) {
                let (lon, lat, z) = (g.lon, g.lat, g.z);
                let fresh = std::mem::take(&mut p.refresh);
                p.list = ListState::Running;
                self.list_job = Some((
                    key,
                    Job::start(
                        move || {
                            let dir = cache_dir();
                            if offline {
                                wayback::list_cached(lon, lat, z, &dir)
                            } else if fresh {
                                wayback::list_fresh(lon, lat, z, &dir)
                            } else {
                                wayback::list(lon, lat, z, &dir)
                            }
                        },
                        ctx,
                    ),
                ));
                return;
            }
        }
    }

    /// The next image to render: the lightbox's (the one shown first), then the current point's cards, in order,
    /// then the tiles of the cards of the other points (nearest first).
    fn next_render(&self) -> Option<(PointKey, String, Kind)> {
        let months = self.months_list();
        if let Some(cur) = self.cur
            && let Some(images) = self.images_of(cur)
        {
            let groups = year_groups(&images, &months);
            if let Some(lb) = self.lb.as_ref().filter(|l| l.key == cur)
                && let Some(g) = groups.get(lb.year_i)
            {
                let first = g.images.get(lb.img_i).copied();
                for i in first.into_iter().chain(g.images.iter().copied()) {
                    if !lb.big.contains_key(&images[i].date) {
                        return Some((cur, images[i].date.clone(), Kind::Big));
                    }
                }
            }
            let p = self.point(cur)?;
            if let Some(i) = groups.iter().map(|g| g.images[g.default]).find(|&i| !p.imgs.contains_key(&images[i].date))
            {
                return Some((cur, images[i].date.clone(), Kind::Card));
            }
        }
        for key in self.work_keys() {
            let Some(p) = self.point(key).filter(|p| Some(p.key) != self.cur && p.geom.is_ok()) else { continue };
            let ListState::Done(images) = &p.list else { continue };
            for g in year_groups(images, &months) {
                let d = &images[g.images[g.default]].date;
                let have = p.fetched.contains(d) || matches!(p.imgs.get(d), Some(Img::Ready(_)));
                if !have && !p.fetch_err.contains_key(d) {
                    return Some((key, d.clone(), Kind::Fetch));
                }
            }
        }
        None
    }

    fn start_render(&mut self, ctx: &egui::Context) {
        if self.render_job.is_some() {
            return;
        }
        let Some((key, date, kind)) = self.next_render() else { return };
        let Some(images) = self.images_of(key) else { return };
        let Some(im) = images.iter().find(|i| i.date == date) else { return };
        let Some(Ok(g)) = self.point(key).map(|p| p.geom.clone()) else { return };
        let num = im.release;
        // each image at the zoom it was found at (a coarser one where Wayback has no finer tiles)
        let z = im.zoom;
        let native = wayback::out_pixels(f64::from(key.chip_m), g.lat, z);
        let px = if kind == Kind::Big { native } else { native.min(CARD_PX) };
        let offline = self.offline;
        let job = Job::start(
            move || {
                let dir = cache_dir();
                let rels = if offline { wayback::releases_cached(&dir)? } else { wayback::releases(&dir)? };
                let rel = rels
                    .into_iter()
                    .find(|r| r.num == num)
                    .ok_or_else(|| format!("Wayback's release {num} is not in the list any more"))?;
                match (kind, offline) {
                    (Kind::Fetch, _) => {
                        let net = if offline { Net::CacheOnly } else { Net::Allow };
                        wayback::prefetch(&rel, z, &g.crs, g.center, g.size_crs, &dir, net).map(|()| RgbImage {
                            w: 0,
                            h: 0,
                            data: Vec::new(),
                        })
                    }
                    (_, true) => wayback::render_cached(&rel, z, &g.crs, g.center, g.size_crs, px, &dir),
                    (_, false) => wayback::render(&rel, z, &g.crs, g.center, g.size_crs, px, &dir),
                }
            },
            ctx,
        );
        self.render_job = Some(RenderJob { key, date, kind, job });
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

    /// The cache card: a square for each point of the window, the activity, the prefetch window, the offline switch
    /// and the download of every point.
    fn cache_ui(&mut self, ui: &mut Ui) {
        let weak = ui.visuals().weak_text_color();
        let mut cells: Vec<(usize, PointKey, CellState)> =
            self.window_pts.iter().map(|&(k, pos)| (pos, k, self.cell(k))).collect();
        cells.sort_by_key(|c| c.0);
        let on_disk = cells.iter().filter(|c| c.2 == CellState::Ready).count();
        let working: Vec<usize> = cells.iter().filter(|c| c.2 == CellState::Working).map(|c| c.0).collect();
        // waiting: the window's points not ready, not working and not failed, and the download's points left
        let queued = cells.iter().filter(|c| c.2 == CellState::Idle).count()
            + self.dl.as_ref().map_or(0, |d| d.reqs.len().saturating_sub(d.next));
        let title = if self.offline {
            format!("Cache \u{b7} offline \u{b7} {on_disk} of {} window points on disk", cells.len())
        } else {
            format!("Cache \u{b7} {}", cache_summary(self.window, on_disk))
        };
        egui::CollapsingHeader::new(title).id_salt("hr-cache").default_open(true).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(3.0, 3.0);
                for &(pos, key, st) in &cells {
                    let (r, resp) = ui.allocate_exact_size(vec2(12.0, 12.0), Sense::hover());
                    let fill = match st {
                        CellState::Idle => CELL_IDLE,
                        CellState::Working => CELL_WORKING,
                        CellState::Ready => CELL_READY,
                        CellState::Failed => CELL_FAILED,
                    };
                    ui.painter().rect_filled(r, 2.0, fill);
                    if Some(key) == self.cur {
                        ui.painter().rect_stroke(
                            r.expand(1.5),
                            2.0,
                            Stroke::new(1.5, Color32::WHITE),
                            StrokeKind::Outside,
                        );
                    }
                    let what = match st {
                        CellState::Idle => "waiting",
                        CellState::Working => "being fetched",
                        CellState::Ready => "on disk",
                        CellState::Failed => "failed or not downloaded",
                    };
                    resp.on_hover_text(format!(
                        "Point {pos}: {what}{}",
                        if Some(key) == self.cur { " (current)" } else { "" }
                    ));
                }
            });
            ui.colored_label(weak, activity_text(&working, queued));
            ui.horizontal_wrapped(|ui| {
                ui.label("Prefetch").on_hover_text(
                    "How many points on each side of the current one have their listing and imagery fetched ahead, \
                     nearest first (0: only the current point)",
                );
                ui.add(egui::Slider::new(&mut self.window, 0..=MAX_WINDOW).prefix("\u{b1}"));
                ui.checkbox(&mut self.offline, "Offline (cache only)").on_hover_text(
                    "No request to Esri at all: the listings and tiles on disk only; what is missing says \
                     \"not downloaded\"",
                );
            });
            ui.horizontal_wrapped(|ui| {
                let busy = self.dl.is_some();
                let can = !busy && !self.offline && self.total_points > 0;
                if ui
                    .add_enabled(can, egui::Button::new("Download all points"))
                    .on_hover_text(
                        "Fetches, in the background and one point at a time, the listing and the tiles of the cards \
                         (one image a year, in the months chosen) of every point of the layer being interpreted, so \
                         that it can be interpreted offline. The points around the current one go first",
                    )
                    .clicked()
                {
                    self.dl_wanted = true;
                }
                if let Some(d) = &self.dl {
                    let total = d.reqs.len().max(1);
                    let k = d.done + d.failed;
                    ui.add(
                        egui::ProgressBar::new(k as f32 / total as f32)
                            .desired_width(160.0)
                            .text(format!("{k} / {} points", d.reqs.len())),
                    );
                    if ui.button("Cancel").clicked() {
                        self.cancel_download();
                    }
                } else if let Some(m) = &self.dl_last {
                    ui.colored_label(weak, m);
                }
            });
        });
    }

    /// For the guides' tours: nothing left to do for the current point (its listing done or failed, each card
    /// drawn or failed, no job on it). `None` while there is no current point.
    pub(crate) fn shots_settled(&self) -> Option<bool> {
        let p = self.point(self.cur?)?;
        let busy = self.list_job.is_some() || self.render_job.as_ref().is_some_and(|j| j.key == p.key);
        let cards = match &p.list {
            ListState::Done(images) => year_groups(images, &self.months_list())
                .iter()
                .all(|g| p.imgs.contains_key(&images[g.images[g.default]].date)),
            ListState::Failed(_) => true,
            _ => false,
        };
        Some(cards && !busy)
    }

    /// For the guides' tours: the lightbox of the current point opened on `year` (else the first year with an
    /// image); whether it could be.
    pub(crate) fn shots_open_lightbox(&mut self, ctx: &egui::Context, year: i32) -> bool {
        let Some(key) = self.cur else { return false };
        let Some(images) = self.images_of(key) else { return false };
        let groups = year_groups(&images, &self.months_list());
        let Some(k) = groups.iter().position(|g| g.year == year).or((!groups.is_empty()).then_some(0)) else {
            return false;
        };
        self.open_lightbox(ctx, key, k, groups[k].default);
        true
    }

    /// For the guides' tours: the lightbox is open and its image drawn (or failed).
    pub(crate) fn shots_lightbox_ready(&self) -> bool {
        let Some(lb) = &self.lb else { return false };
        let Some(images) = self.images_of(lb.key) else { return false };
        let groups = year_groups(&images, &self.months_list());
        groups
            .get(lb.year_i)
            .and_then(|g| g.images.get(lb.img_i))
            .is_some_and(|&i| lb.big.contains_key(&images[i].date))
    }

    /// For the guides' tours: the lightbox closed.
    pub(crate) fn shots_close_lightbox(&mut self) {
        self.lb = None;
    }
}

/// The high-resolution section at the top of the Chips panel. `key` is the current point (None: no valid one);
/// `sz` the card size; `grid` the overlay's subgrid; `fr` the label mode "Fraction cover" (the cells painted over
/// each year's card, the strokes made). Returns whether it drew the overlay's legend.
pub(crate) fn section(
    ui: &mut Ui,
    hr: &mut HighRes,
    key: Option<PointKey>,
    sz: f32,
    grid: &mut bool,
    fr: &mut crate::fraction::FracUi,
) -> bool {
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
    hr.cache_ui(ui);
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
        ListState::Failed(e) if e == wayback::NOT_DOWNLOADED => {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(weak, "Not downloaded: the imagery of this point is not on disk (Offline is on).");
                retry = ui.button("Retry").clicked();
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
            let failed_with = |nd: bool| {
                groups
                    .iter()
                    .filter(|g| {
                        matches!(state(&images[g.images[g.default]].date),
                            Some(Img::Failed(e)) if (e == wayback::NOT_DOWNLOADED) == nd)
                    })
                    .count()
            };
            let (failed, not_downloaded) = (failed_with(false), failed_with(true));
            if not_downloaded > 0 {
                ui.colored_label(weak, format!("{not_downloaded} images not downloaded (Offline is on)"));
            }
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
                            Some(Img::Failed(e)) => (ph, Some(fail_text(e))),
                            None => (ph, Some("loading\u{2026}".to_string())),
                        };
                        let extra = g.images.len() - 1;
                        let caption = CardCaption {
                            year: g.year.to_string(),
                            badge: ("WAYBACK".into(), BADGE_ORANGE),
                            right: fit_text(&caption_full(im, geom.z, geom.lat), chars),
                            extra: (extra > 0).then_some(extra as u32),
                            border: None,
                        };
                        let ov = overlay(&geom, view, *grid, fr.cells_for(g.year));
                        let resp = card(ui, tex, view_uv(unit, view), sz, &caption, &ov);
                        let paint =
                            crate::fraction::paint_input(ui, &resp, crate::chips::card_image_rect(resp.rect, sz), &ov);
                        fr.push(g.year, &paint);
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
                            caption_full(im, geom.z, geom.lat)
                        );
                        if let Some(n) = &note {
                            tip = format!("{tip}\n{n}");
                        }
                        let click = if fr.on {
                            format!(
                                "Drag over the pixel's cells to paint {} (right-drag erases, Shift+drag pans); a click \
                                 outside the pixel opens the images of the year",
                                g.year
                            )
                        } else {
                            format!("Click to see all the images of {}", g.year)
                        };
                        let resp = resp.on_hover_text(format!("{tip}\n{click}"));
                        if resp.clicked()
                            && !paint.active
                            && let Some(k) = groups.iter().position(|x| x.year == g.year)
                        {
                            open = Some(k);
                        }
                        let nv = card_view_input(ui, &resp, sz, view, zmax, paint.active);
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
pub(crate) fn lightbox(ctx: &egui::Context, hr: &mut HighRes, grid: bool, fr: &mut crate::fraction::FracUi) {
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
            let ov = overlay(&geom, view, grid, fr.cells_for(g.year));
            let mut painting = false;
            match big.get(&im.date) {
                Some(Img::Ready(t)) => {
                    painter.image(t.id(), rect, view_uv(Rect::from_min_size(Pos2::ZERO, vec2(1.0, 1.0)), view), Color32::WHITE);
                    draw_overlay(&painter, rect, &ov);
                    let paint = crate::fraction::paint_input(ui, &resp, rect, &ov);
                    painting = paint.active;
                    fr.push(g.year, &paint);
                }
                Some(Img::Failed(e)) => {
                    painter.rect_filled(rect, 4.0, Color32::from_gray(30));
                    painter.text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        fit_text(&fail_text(e), 90),
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
            view = card_view_input(ui, &resp, side, view, zmax, painting);
            if ui.add_sized([40.0, side], egui::Button::new("\u{203a}")).on_hover_text("Next image (Right)").clicked() {
                act = Some(Act::Step(Step::NextImage));
            }
        });
        let res = im.res_m.map_or(String::new(), |r| format!(" \u{b7} {r:.2} m"));
        ui.label(format!("{}{res} \u{b7} {}", im.date, if im.desc.is_empty() { "no description" } else { &im.desc }));
        if im.zoom < geom.z {
            ui.colored_label(
                weak,
                format!(
                    "Wayback has no tiles here at zoom {}: this image is from zoom {} ({:.1} m a pixel), so it is blurrier",
                    geom.z,
                    im.zoom,
                    wayback::ground_res(geom.lat, im.zoom)
                ),
            );
        }
        ui.colored_label(
            weak,
            format!(
                "Wayback release {} ({}) \u{b7} Left/Right: image of the year, Up/Down: year, Esc: close, Ctrl + wheel zooms, drag pans (fraction cover: drag over the cells paints, right-drag erases, X erases the cell, Shift+drag pans) \u{b7} Imagery: Esri Wayback \u{2014} {}",
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

    #[test]
    fn the_window_goes_out_from_the_current_point() {
        assert_eq!(window_order(5, 20, 2), [5, 6, 4, 7, 3]);
        assert_eq!(window_order(5, 20, 0), [5]);
        // the ends of the list
        assert_eq!(window_order(0, 3, 5), [0, 1, 2]);
        assert_eq!(window_order(2, 3, 5), [2, 1, 0]);
        assert_eq!(window_order(1, 4, 2), [1, 2, 0, 3]);
        assert!(window_order(3, 3, 2).is_empty());
        assert_eq!(window_order(50, 100, MAX_WINDOW).len(), 2 * MAX_WINDOW + 1);
    }

    #[test]
    fn textures_go_oldest_first_but_not_the_kept_one() {
        let mb = 1 << 20;
        // (last use, bytes): the point 1 is the current one and the oldest
        let items = [(5.0, 100 * mb), (1.0, 100 * mb), (3.0, 100 * mb), (2.0, 0)];
        assert!(texture_evictions(&items, Some(1), 300 * mb).is_empty());
        assert_eq!(texture_evictions(&items, Some(1), 250 * mb), [2]);
        assert_eq!(texture_evictions(&items, Some(1), 100 * mb), [2, 0]);
        // the kept one alone over the budget: the rest all go, it stays
        assert_eq!(texture_evictions(&items, Some(1), 0), [2, 0]);
        assert_eq!(keep_points(5), KEEP_POINTS);
        assert_eq!(keep_points(41), 44);
    }

    #[test]
    fn months_are_kept_as_bits() {
        let mut m = [false; 12];
        m[0] = true;
        m[5] = true;
        m[11] = true;
        assert_eq!(months_mask(&m), 0b1000_0010_0001);
        assert_eq!(months_from_mask(months_mask(&m)), m);
        assert_eq!(months_from_mask(0), [false; 12]);
    }

    #[test]
    fn the_cache_card_texts() {
        assert_eq!(cache_summary(5, 11), "window \u{b1}5 \u{b7} 11 points on disk");
        assert_eq!(cache_summary(0, 1), "window \u{b1}0 \u{b7} 1 point on disk");
        assert_eq!(activity_text(&[], 0), "idle");
        assert_eq!(activity_text(&[], 3), "3 queued");
        assert_eq!(activity_text(&[12, 13], 22), "working: #12, #13 \u{b7} 22 queued");
        assert_eq!(activity_text(&[1, 2, 3, 4, 5], 0), "working: #1, #2, #3, #4 \u{2026} \u{b7} 0 queued");
    }

    #[test]
    fn an_image_from_a_coarser_zoom_says_so() {
        let mut i = img("2019-08-14", 7, Some(0.3), "Vexcel");
        i.zoom = 19;
        assert_eq!(caption_full(&i, 19, -7.3), caption_right(&i));
        i.zoom = 17;
        let t = caption_full(&i, 19, -7.3);
        // z17 at 7 degrees south: 156543.03 * cos(7.3 deg) / 2^17 = 1.18 m
        assert!(t.ends_with("\u{b7} z17 \u{b7} 1.2 m/px"), "{t}");
    }

    #[test]
    fn the_options_go_to_the_settings_and_back() {
        let mut s = crate::settings::Settings::default();
        let mut hr = HighRes::default();
        hr.load(&s);
        assert!(hr.enabled && !hr.offline && hr.window == 5 && hr.chip_m == 150.0);
        hr.enabled = false;
        hr.offline = true;
        hr.window = 9;
        hr.chip_m = 200.0;
        hr.months[6] = true;
        assert!(hr.store(&mut s, false, false));
        assert!(!s.hr_enabled && s.hr_offline && s.hr_window == 9 && s.hr_chip_m == 200 && s.hr_months == 64);
        // nothing changed: nothing to save
        assert!(!hr.store(&mut s, false, false));
        let mut back = HighRes::default();
        back.load(&s);
        assert!(!back.enabled && back.offline && back.window == 9 && back.months[6]);
        // a run that forced them off or on leaves the user's choices alone
        hr.enabled = true;
        hr.offline = false;
        assert!(!hr.store(&mut s, true, true));
        assert!(!s.hr_enabled && s.hr_offline);
    }
}
