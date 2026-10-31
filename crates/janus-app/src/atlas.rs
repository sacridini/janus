//! Analysis > Atlas: every pixel's series in one picture. The series of the ROI (or the whole image) are sampled on
//! a grid (up to MAX_SAMPLES pixels), reduced to their principal components and laid out in a plane by UMAP
//! (atlas_math.rs): pixels that behave alike over time land close together, so the kinds of behaviour of the place
//! (a crop, a forest, water, a clearing) are the clusters. Drawn around with the mouse, up to three groups: each its
//! colour on the map (every pixel of the region placed with the sample nearest to it), its share of the region, its
//! mean curve and the range of its pixels (10th to 90th percentile) over time. The pointer over the plane lights
//! the pixels like the ones under it on the map; the pointer over the map rings its pixel in the plane. The work
//! runs on a thread of its own (the processing threads), the plane settling as UMAP runs.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

use egui::{Color32, Pos2, Rect, Stroke, Ui, vec2};
use egui_plot::{Plot, PlotPoint, VLine};
use janus_cache::Overview;
use janus_cube::multidim::format_g;
use rayon::prelude::*;

use crate::app::State;
use crate::atlas_math as am;
use crate::theme;

/// The most pixels laid out (the others placed with the nearest of them).
const MAX_SAMPLES: usize = 20_000;
/// The pointer's reach over the plane (points): the pixels lit on the map.
const BRUSH: f32 = 12.0;
const NONE: u32 = u32::MAX;
const COLOURINGS: [&str; 6] = ["Density", "Groups", "Value at the date", "Mean", "Amplitude", "Trend"];
const NAMES: [&str; 3] = ["A", "B", "C"];
const YEAR: f64 = 365.2425 * 86400.0;

/// The density's ramp, from the background to the densest: blue, as the palette's sequential scale (light on a
/// dark panel, dark on a light one).
pub(crate) fn ramp(dark: bool, bg: Color32) -> [Color32; 5] {
    let hex = |v: u32| Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8);
    if dark {
        [bg, hex(0x104281), hex(0x2a78d6), hex(0x86b6ef), hex(0xcde2fb)]
    } else {
        [bg, hex(0xcde2fb), hex(0x86b6ef), hex(0x2a78d6), hex(0x104281)]
    }
}

/// The ramp's colour at `t` (0-1).
pub(crate) fn ramp_at(r: &[Color32; 5], t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0) * 4.0;
    let k = (t.floor() as usize).min(3);
    let f = t - k as f32;
    let (a, b) = (r[k], r[k + 1]);
    let mix = |x: u8, y: u8| (f32::from(x) + f * (f32::from(y) - f32::from(x))).round() as u8;
    Color32::from_rgb(mix(a.r(), b.r()), mix(a.g(), b.g()), mix(a.b(), b.b()))
}

/// The groups' colours: the palette's first three categorical hues (distinct for every kind of colour vision).
pub(crate) fn group_colour(k: usize) -> Color32 {
    [Color32::from_rgb(0x39, 0x87, 0xe5), Color32::from_rgb(0xd9, 0x59, 0x26), Color32::from_rgb(0x19, 0x9e, 0x70)][k]
}

/// What the atlas is of: the region of the overview and how series are compared.
#[derive(Clone, PartialEq, Debug)]
struct Request {
    session: u64,
    overview: usize,
    /// overview pixels: x0, y0, x1, y1
    rect: [usize; 4],
    poly: u64,
    shapes: bool,
}

/// The atlas: its samples, their place in the plane and the place of every pixel of the region.
pub struct Atlas {
    /// the overview's size and the source's
    ow: usize,
    oh: usize,
    src: (usize, usize),
    rect: [usize; 4],
    n: usize,
    /// each sample's overview pixel
    pixel: Vec<[u32; 2]>,
    nb: usize,
    /// each bin's time (or band) on the curves' axis
    bin_x: Vec<f64>,
    /// each sample's values, its dates averaged into the bins, the gaps filled
    raw: Vec<f32>,
    mean: Vec<f32>,
    amp: Vec<f32>,
    /// per year (per band for a stack or a spectrum)
    trend: Vec<f32>,
    xy: Vec<f32>,
    /// the sample nearest to each pixel of the region (rows of the rect), NONE outside it or without values
    nearest: Vec<u32>,
    /// the region's pixels each sample stands for (all 1 until the pixels are placed)
    weight: Vec<u32>,
    dims: usize,
    explained: f64,
    step: usize,
}

impl Atlas {
    fn region(&self) -> (usize, usize) {
        (self.rect[2] - self.rect[0], self.rect[3] - self.rect[1])
    }

    /// The sample a source pixel is placed with.
    fn sample_at(&self, x: usize, y: usize) -> Option<usize> {
        let ox = ((x as f64 + 0.5) * self.ow as f64 / self.src.0 as f64) as usize;
        let oy = ((y as f64 + 0.5) * self.oh as f64 / self.src.1 as f64) as usize;
        let [x0, y0, x1, y1] = self.rect;
        if ox < x0 || ox >= x1 || oy < y0 || oy >= y1 || self.nearest.is_empty() {
            return None;
        }
        let s = self.nearest[(oy - y0) * (x1 - x0) + ox - x0];
        (s != NONE).then_some(s as usize)
    }
}

enum Msg {
    Stage(&'static str, f32),
    Base(Box<Atlas>),
    Layout(Vec<f32>, f32),
    Placed(Vec<u32>, Vec<u32>),
    Failed(String),
}

struct Job {
    cancel: Arc<AtomicBool>,
    rx: Receiver<Msg>,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A group drawn around points of the plane.
struct Group {
    /// its colour and name: 0..3, kept while it lives
    slot: usize,
    /// the outline, in the plane
    poly: Vec<[f32; 2]>,
}

/// A group's curve: its mean and the range of its pixels at each bin.
struct Curve {
    colour: Color32,
    mean: Vec<f32>,
    lo: Vec<f32>,
    hi: Vec<f32>,
}

/// The tab's state.
#[derive(Default)]
pub struct AtlasUi {
    /// the series' shapes compared (each standardized over its own dates), not their values
    pub shapes: bool,
    pub colouring: usize,
    /// the groups not shown on the map
    pub hide_map: bool,
    /// groups found by themselves once laid out (for the screenshots: --analysis atlas,groups)
    pub find_groups: bool,
    request: Option<Request>,
    job: Option<Job>,
    atlas: Option<Atlas>,
    stage: (&'static str, f32),
    error: Option<String>,
    /// changed with the layout, the places and the groups
    generation: u64,
    groups: Vec<Group>,
    /// each sample's group (255: none)
    group_of: Vec<u8>,
    group_key: u64,
    /// the view of the plane: its centre and the units a point; None: the whole layout
    view: Option<([f32; 2], f32)>,
    lasso: Vec<[f32; 2]>,
    image: Option<(u64, egui::TextureHandle)>,
    overlay: Option<(u64, egui::TextureHandle)>,
    brushed: Vec<u32>,
    hovered: Option<usize>,
    curves: Option<(u64, Vec<Curve>)>,
    attr: Option<(u64, Vec<f32>)>,
}

fn hash<T: Hash>(v: T) -> u64 {
    let mut h = DefaultHasher::new();
    v.hash(&mut h);
    h.finish()
}

/// What the worker needs.
struct Input {
    ov: Arc<Overview>,
    rect: [usize; 4],
    poly: janus_geo::plane::Polygon,
    src: (usize, usize),
    xs: Vec<f64>,
    shapes: bool,
    dated: bool,
    threads: usize,
}

fn worker(input: Input, cancel: Arc<AtomicBool>, tx: Sender<Msg>, ctx: egui::Context) {
    let send = |m: Msg| {
        let _ = tx.send(m);
        ctx.request_repaint();
    };
    let pool = match rayon::ThreadPoolBuilder::new().num_threads(input.threads.max(1)).build() {
        Ok(p) => p,
        Err(e) => return send(Msg::Failed(e.to_string())),
    };
    pool.install(|| {
        if let Err(e) = work(&input, &cancel, &send) {
            send(Msg::Failed(e));
        }
    });
}

fn work(input: &Input, cancel: &AtomicBool, send: &dyn Fn(Msg)) -> Result<(), String> {
    let ov = &input.ov;
    let (ow, t) = (ov.w, ov.t);
    let layers: Vec<&[f32]> = (0..t).map(|k| ov.layer(k)).collect::<Option<_>>().ok_or("dates not read yet")?;
    let [x0, y0, x1, y1] = input.rect;
    let (rw, rh) = (x1 - x0, y1 - y0);
    let (sx, sy) = (input.src.0 as f64 / ow as f64, input.src.1 as f64 / ov.h as f64);
    let poly = &input.poly;
    let inside = |ox: usize, oy: usize| {
        poly.is_empty() || janus_geo::plane::contains(poly, (ox as f64 + 0.5) * sx, (oy as f64 + 0.5) * sy)
    };
    let edges = am::bin_edges(t);
    let nb = edges.len() - 1;
    let bin_x: Vec<f64> = (0..nb)
        .map(|b| input.xs[edges[b]..edges[b + 1]].iter().sum::<f64>() / (edges[b + 1] - edges[b]) as f64)
        .collect();
    // the samples: a grid over the region, as many as MAX_SAMPLES inside it
    send(Msg::Stage("Reading the series", 0.0));
    let area: usize = if poly.is_empty() {
        rw * rh
    } else {
        (y0..y1).into_par_iter().map(|y| (x0..x1).filter(|&x| inside(x, y)).count()).sum()
    };
    let step = ((area as f64 / MAX_SAMPLES as f64).sqrt().ceil() as usize).max(1);
    let cells: Vec<[u32; 2]> = (y0 + step / 2..y1)
        .step_by(step)
        .flat_map(|y| (x0 + step / 2..x1).step_by(step).map(move |x| [x as u32, y as u32]))
        .filter(|p| inside(p[0] as usize, p[1] as usize))
        .collect();
    // each sample: its pixel, its values in the bins, its features
    type Row = ([u32; 2], Vec<f32>, Vec<f32>);
    let rows: Vec<Option<Row>> = cells
        .par_iter()
        .map(|&p| {
            let idx = p[1] as usize * ow + p[0] as usize;
            let values: Vec<f32> = layers.iter().map(|l| l[idx]).collect();
            let mut raw = vec![0f32; nb];
            if !am::features(&values, &edges, false, &mut raw) {
                return None;
            }
            let mut f = vec![0f32; nb];
            am::features(&values, &edges, input.shapes, &mut f);
            Some((p, raw, f))
        })
        .collect();
    let rows: Vec<_> = rows.into_iter().flatten().collect();
    let n = rows.len();
    if n < 30 {
        return Err(format!("Only {n} pixels with values at half the dates or more: too few to lay out."));
    }
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let pixel: Vec<[u32; 2]> = rows.iter().map(|r| r.0).collect();
    let raw: Vec<f32> = rows.iter().flat_map(|r| r.1.iter().copied()).collect();
    let x: Vec<f32> = rows.iter().flat_map(|r| r.2.iter().copied()).collect();
    drop(rows);
    // what the plane can be coloured by
    let per = if input.dated { YEAR } else { 1.0 };
    let xm = bin_x.iter().sum::<f64>() / nb as f64;
    let sxx: f64 = bin_x.iter().map(|v| (v - xm).powi(2)).sum::<f64>().max(1e-300);
    let mut mean = vec![0f32; n];
    let mut amp = vec![0f32; n];
    let mut trend = vec![0f32; n];
    mean.par_iter_mut().zip(amp.par_iter_mut()).zip(trend.par_iter_mut()).enumerate().for_each(|(i, ((m, a), tr))| {
        let r = &raw[i * nb..(i + 1) * nb];
        let mu = r.iter().map(|&v| f64::from(v)).sum::<f64>() / nb as f64;
        *m = mu as f32;
        let mut s = r.to_vec();
        s.sort_by(f32::total_cmp);
        let q = |p: f64| s[((nb - 1) as f64 * p).round() as usize];
        *a = q(0.9) - q(0.1);
        let sxy: f64 = r.iter().zip(&bin_x).map(|(&v, &t)| (f64::from(v) - mu) * (t - xm)).sum();
        *tr = (sxy / sxx * per) as f32;
    });
    // the principal components, and the layout from the first two
    let pca = am::Pca::fit(&x, n, nb, am::COMPONENTS);
    let d = pca.d;
    let mut p = vec![0f32; n * d];
    p.par_chunks_mut(d).zip(x.par_chunks(nb)).for_each(|(o, row)| pca.project(row, o));
    drop(x);
    let init = am::initial_layout(&p, n, d);
    send(Msg::Base(Box::new(Atlas {
        ow,
        oh: ov.h,
        src: input.src,
        rect: input.rect,
        n,
        pixel,
        nb,
        bin_x,
        raw,
        mean,
        amp,
        trend,
        xy: init.clone(),
        nearest: Vec::new(),
        weight: vec![1; n],
        dims: d,
        explained: pca.explained,
        step,
    })));
    send(Msg::Stage("Finding each pixel's neighbours", 0.0));
    let Some((idx, dist)) = am::knn(&p, n, d, am::NEIGHBOURS, cancel) else { return Ok(()) };
    send(Msg::Stage("Laying out", 0.0));
    let epochs = am::epochs_for(n);
    let Some(xy) = am::umap(&idx, &dist, n, am::NEIGHBOURS, &init, epochs, cancel, &mut |y, share| {
        send(Msg::Layout(y.to_vec(), share));
    }) else {
        return Ok(());
    };
    send(Msg::Layout(xy, 1.0));
    drop((idx, dist));
    // every pixel of the region with the sample nearest to it in the components
    send(Msg::Stage("Placing every pixel", 0.0));
    let ivf = am::Ivf::build(&p, n, d);
    let mut nearest = vec![NONE; rw * rh];
    nearest.par_chunks_mut(rw).enumerate().for_each(|(ry, row)| {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let oy = y0 + ry;
        // the row's series, pixel by pixel
        let mut buf = vec![0f32; rw * t];
        for (k, l) in layers.iter().enumerate() {
            for (rx, &v) in l[oy * ow + x0..oy * ow + x1].iter().enumerate() {
                buf[rx * t + k] = v;
            }
        }
        let mut f = vec![0f32; nb];
        let mut q = vec![0f32; d];
        for (rx, o) in row.iter_mut().enumerate() {
            if !inside(x0 + rx, oy) || !am::features(&buf[rx * t..(rx + 1) * t], &edges, input.shapes, &mut f) {
                continue;
            }
            pca.project(&f, &mut q);
            *o = ivf.nearest(&q);
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let mut weight = vec![0u32; n];
    nearest.iter().filter(|&&s| s != NONE).for_each(|&s| weight[s as usize] += 1);
    send(Msg::Placed(nearest, weight));
    Ok(())
}

/// Whether a point is inside a polygon (even-odd).
fn in_poly(poly: &[[f32; 2]], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[j]);
        if (a[1] > y) != (b[1] > y) && x < (b[0] - a[0]) * (y - a[1]) / (b[1] - a[1]) + a[0] {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The convex hull of points (Andrew's monotone chain).
fn hull(mut pts: Vec<[f32; 2]>) -> Vec<[f32; 2]> {
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]).then(a[1].total_cmp(&b[1])));
    let cross = |o: [f32; 2], a: [f32; 2], b: [f32; 2]| (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0]);
    let mut h: Vec<[f32; 2]> = Vec::new();
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &[f32; 2]>> =
            if pass == 0 { Box::new(pts.iter()) } else { Box::new(pts.iter().rev()) };
        for &p in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], p) <= 0.0 {
                h.pop();
            }
            h.push(p);
        }
        h.pop();
    }
    h
}

/// Up to three groups found in the plane: k-means of the points, each group the hull of its core (the points
/// nearest its centre, its strays left out).
fn suggest_groups(xy: &[f32], n: usize, k: usize) -> Vec<Vec<[f32; 2]>> {
    let pt = |i: usize| [xy[2 * i], xy[2 * i + 1]];
    let d2 = |a: [f32; 2], b: [f32; 2]| (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2);
    // k-means++ (each next centre drawn in proportion to its squared distance), five starts, the tightest kept
    let mut best: Option<(f64, Vec<[f32; 2]>, Vec<usize>)> = None;
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut draw = || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    for _start in 0..5 {
        let mut centres = vec![pt((draw() * n as f64) as usize % n)];
        while centres.len() < k {
            let dist: Vec<f64> = (0..n)
                .map(|i| f64::from(centres.iter().map(|&c| d2(pt(i), c)).fold(f32::INFINITY, f32::min)))
                .collect();
            let total: f64 = dist.iter().sum();
            let mut r = draw() * total;
            let next = dist.iter().position(|&d| {
                r -= d;
                r <= 0.0
            });
            centres.push(pt(next.unwrap_or(n - 1)));
        }
        let mut assign = vec![0usize; n];
        for _ in 0..30 {
            for (i, a) in assign.iter_mut().enumerate() {
                *a = (0..k).min_by(|&p, &q| d2(pt(i), centres[p]).total_cmp(&d2(pt(i), centres[q]))).unwrap_or(0);
            }
            for (c, centre) in centres.iter_mut().enumerate() {
                let (mut s, mut m) = ([0f64; 2], 0usize);
                for i in (0..n).filter(|&i| assign[i] == c) {
                    s[0] += f64::from(xy[2 * i]);
                    s[1] += f64::from(xy[2 * i + 1]);
                    m += 1;
                }
                if m > 0 {
                    *centre = [(s[0] / m as f64) as f32, (s[1] / m as f64) as f32];
                }
            }
        }
        let inertia: f64 = (0..n).map(|i| f64::from(d2(pt(i), centres[assign[i]]))).sum();
        if best.as_ref().is_none_or(|b| inertia < b.0) {
            best = Some((inertia, centres, assign));
        }
    }
    let Some((_, centres, assign)) = best else { return Vec::new() };
    (0..k)
        .filter_map(|c| {
            let mut mine: Vec<(f32, [f32; 2])> =
                (0..n).filter(|&i| assign[i] == c).map(|i| (d2(pt(i), centres[c]), pt(i))).collect();
            mine.sort_by(|a, b| a.0.total_cmp(&b.0));
            // its strays left out: farther than 2.5 times the median distance
            let median = mine.get(mine.len() / 2).map_or(0.0, |m| m.0);
            mine.retain(|m| m.0 <= median * 6.25);
            (mine.len() >= 3).then(|| hull(mine.into_iter().map(|m| m.1).collect()))
        })
        .collect()
}

/// Grid marks at round steps, about five over the range.
fn nice_marks((lo, hi): (f64, f64)) -> Vec<egui_plot::GridMark> {
    let raw = (hi - lo).abs().max(1e-300) / 5.0;
    let mag = 10f64.powf(raw.log10().floor());
    let step = [1.0, 2.0, 2.5, 5.0, 10.0].iter().map(|m| m * mag).find(|&s| s >= raw).unwrap_or(10.0 * mag);
    let first = (lo / step).ceil() as i64;
    (first..)
        .map(|k| k as f64 * step)
        .take_while(|&v| v <= hi)
        .map(|value| egui_plot::GridMark { value, step_size: step })
        .collect()
}

/// The robust bounds of the layout (0.2 % trimmed each side).
fn bounds(xy: &[f32]) -> [f32; 4] {
    let q = |mut v: Vec<f32>| {
        let n = v.len();
        let k = n / 500;
        v.select_nth_unstable_by(k, f32::total_cmp);
        let lo = v[k];
        v.select_nth_unstable_by(n - 1 - k, f32::total_cmp);
        (lo, v[n - 1 - k])
    };
    let (x0, x1) = q(xy.iter().step_by(2).copied().collect());
    let (y0, y1) = q(xy.iter().skip(1).step_by(2).copied().collect());
    [x0, y0, x1, y1]
}

fn mix(a: Color32, b: Color32, f: f32) -> Color32 {
    let m = |x: u8, y: u8| (f32::from(x) + f.clamp(0.0, 1.0) * (f32::from(y) - f32::from(x))).round() as u8;
    Color32::from_rgb(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()))
}

/// How the points are coloured: their group, a value in a colormap, or their density alone.
enum Paint<'a> {
    Density,
    Groups(&'a [u8], Color32),
    Values(&'a [f32], (f32, f32), &'a janus_gpu::colormaps::Colormap),
}

/// The plane as an image of `w` x `h` pixels: each point splatted (bilinear, then a 3 x 3 blur), the density
/// lightening (Density) or making opaque the points' colour over the background.
#[allow(clippy::too_many_arguments)]
fn render(
    xy: &[f32],
    n: usize,
    to: &dyn Fn(f32, f32) -> (f32, f32),
    w: usize,
    h: usize,
    paint: &Paint,
    dark: bool,
    bg: Color32,
) -> egui::ColorImage {
    let mut acc = vec![[0f32; 4]; w * h];
    let lut: Vec<[u8; 4]> = match paint {
        Paint::Values(_, _, cm) => (0..256).map(|k| cm.sample(k as f32 / 255.0)).collect(),
        _ => Vec::new(),
    };
    for i in 0..n {
        let (px, py) = to(xy[2 * i], xy[2 * i + 1]);
        if !(px >= 0.0 && py >= 0.0 && px < (w - 1) as f32 && py < (h - 1) as f32) {
            continue;
        }
        let (c, wt) = match paint {
            Paint::Density => ([0.0; 3], 1.0),
            Paint::Groups(g, weak) => match g.get(i).copied().unwrap_or(255) {
                255 => ([weak.r(), weak.g(), weak.b()].map(f32::from), 0.35),
                k => {
                    let c = group_colour(k as usize);
                    ([c.r(), c.g(), c.b()].map(f32::from), 1.0)
                }
            },
            Paint::Values(v, (lo, hi), _) => {
                let x = v[i];
                if x.is_finite() {
                    let f = ((x - lo) / (hi - lo).max(1e-30)).clamp(0.0, 1.0);
                    let c = lut[(f * 255.0).round() as usize];
                    ([c[0], c[1], c[2]].map(f32::from), 1.0)
                } else {
                    ([128.0; 3], 0.35)
                }
            }
        };
        let (ix, iy) = (px.floor() as usize, py.floor() as usize);
        let (fx, fy) = (px - ix as f32, py - iy as f32);
        for (dx, dy, f) in
            [(0, 0, (1.0 - fx) * (1.0 - fy)), (1, 0, fx * (1.0 - fy)), (0, 1, (1.0 - fx) * fy), (1, 1, fx * fy)]
        {
            let a = &mut acc[(iy + dy) * w + ix + dx];
            let k = f * wt;
            a[0] += k;
            a[1] += k * c[0];
            a[2] += k * c[1];
            a[3] += k * c[2];
        }
    }
    // a 3 x 3 blur (1 2 1), rows then columns
    let blur = |src: &[[f32; 4]], stride: usize, len: usize, count: usize| -> Vec<[f32; 4]> {
        let mut out = vec![[0f32; 4]; src.len()];
        for line in 0..count {
            for k in 0..len {
                let at = |k: usize| if stride == 1 { line * len + k } else { k * stride + line };
                let mut s = [0f32; 4];
                for (o, wgt) in [(-1i64, 0.25f32), (0, 0.5), (1, 0.25)] {
                    let j = k as i64 + o;
                    if j >= 0 && (j as usize) < len {
                        let v = src[at(j as usize)];
                        (0..4).for_each(|c| s[c] += wgt * v[c]);
                    }
                }
                out[at(k)] = s;
            }
        }
        out
    };
    let acc = blur(&blur(&acc, 1, w, h), w, h, w);
    let ramp_c = ramp(dark, bg);
    // the density's scale: its 99th percentile among the pixels with points
    let mut lit: Vec<f32> = acc.iter().map(|a| a[0]).filter(|&v| v > 1e-4).collect();
    let top = if lit.is_empty() {
        1.0
    } else {
        let k = ((lit.len() - 1) as f32 * 0.99) as usize;
        *lit.select_nth_unstable_by(k, f32::total_cmp).1
    };
    let pixels = acc
        .par_iter()
        .map(|a| {
            let c = a[0];
            if c <= 1e-4 {
                return bg;
            }
            match paint {
                Paint::Density => {
                    let t = (1.0 + 6.0 * c / top).ln() / 7f32.ln();
                    ramp_at(&ramp_c, 0.18 + 0.82 * t.min(1.0))
                }
                _ => {
                    let col =
                        Color32::from_rgb((a[1] / c).round() as u8, (a[2] / c).round() as u8, (a[3] / c).round() as u8);
                    mix(bg, col, 1.0 - (-3.0 * c / top.max(0.3) * 2.0).exp())
                }
            }
        })
        .collect();
    egui::ColorImage { size: [w, h], pixels, source_size: vec2(w as f32, h as f32) }
}

impl State {
    /// Analysis > Atlas.
    pub(crate) fn atlas_tab(&mut self, ui: &mut Ui) {
        let Some((info, ov)) = self.session.as_ref().map(|s| (s.info.clone(), s.s.overview.clone())) else { return };
        let read = (0..ov.t).filter(|&k| ov.layer(k).is_some()).count();
        // the controls
        let a = &mut self.atlas;
        let mut restart = false;
        let mut find = false;
        ui.horizontal(|ui| {
            restart |= ui
                .radio_value(&mut a.shapes, false, "Values")
                .on_hover_text("Pixels alike when their values are, date by date")
                .changed();
            restart |= ui
                .radio_value(&mut a.shapes, true, "Shapes")
                .on_hover_text(
                    "Pixels alike when their curves have the same form whatever their level (each standardized \
                     over its dates): the same crop calendar on poorer and richer soils together",
                )
                .changed();
            ui.separator();
            ui.label("Colour");
            egui::ComboBox::from_id_salt("atlas colouring")
                .width(ui.text_style_height(&egui::TextStyle::Body) * 9.0)
                .selected_text(COLOURINGS[a.colouring.min(COLOURINGS.len() - 1)])
                .show_ui(ui, |ui| {
                    for (k, name) in COLOURINGS.iter().enumerate() {
                        ui.selectable_value(&mut a.colouring, k, *name);
                    }
                })
                .response
                .on_hover_text("What the points' colour shows: how many pixels are there, their group or a value");
            ui.separator();
            let mut on_map = !a.hide_map;
            if ui.checkbox(&mut on_map, "On the map").on_hover_text("The groups' pixels coloured on the map").changed()
            {
                a.hide_map = !on_map;
            }
            let laid_out = a.job.is_none() && a.atlas.is_some();
            if ui
                .add_enabled(laid_out, egui::Button::new("Find groups"))
                .on_hover_text("Three groups found in the plane (k-means), in place of the ones drawn")
                .clicked()
            {
                find = true;
            }
            if !a.groups.is_empty() && ui.button("Clear groups").clicked() {
                a.groups.clear();
                a.generation += 1;
            }
        });
        if read < ov.t {
            ui.weak(format!("Reading the dates: {read} of {}...", ov.t));
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        // the region: the ROI's box on the overview (its polygon followed), or the whole image
        let (w, h) = (info.width, info.height);
        let rect = match self.series.roi_rect {
            Some([x0, y0, x1, y1]) => {
                let fx = |x: usize| (x * ov.w).div_ceil(w).min(ov.w);
                let fy = |y: usize| (y * ov.h).div_ceil(h).min(ov.h);
                let r = [(x0 * ov.w / w).min(ov.w), (y0 * ov.h / h).min(ov.h), fx(x1), fy(y1)];
                if r[2] > r[0] && r[3] > r[1] { r } else { [0, 0, ov.w, ov.h] }
            }
            None => [0, 0, ov.w, ov.h],
        };
        let poly = if self.series.roi_rect.is_some() { self.series.roi_poly.clone() } else { Vec::new() };
        let request = Request {
            session: info.id,
            overview: Arc::as_ptr(&ov) as usize,
            rect,
            poly: hash(poly.iter().flatten().map(|v| v.to_bits()).collect::<Vec<_>>()),
            shapes: self.atlas.shapes,
        };
        let a = &mut self.atlas;
        if restart || a.request.as_ref() != Some(&request) {
            let dated = !info.stack && !info.spectral;
            let input = Input {
                ov: ov.clone(),
                rect,
                poly,
                src: (w, h),
                xs: info.layers.iter().enumerate().map(|(k, l)| if dated { l.time } else { k as f64 }).collect(),
                shapes: a.shapes,
                dated,
                threads: 0,
            };
            let cancel = Arc::new(AtomicBool::new(false));
            let (tx, rx) = channel();
            a.job = Some(Job { cancel: cancel.clone(), rx });
            a.request = Some(request);
            a.atlas = None;
            a.error = None;
            a.groups.clear();
            a.view = None;
            a.generation += 1;
            a.stage = ("Reading the series", 0.0);
            let input = Input { threads: self.processing_threads(), ..input };
            let ctx = ui.ctx().clone();
            std::thread::Builder::new().name("atlas".into()).spawn(move || worker(input, cancel, tx, ctx)).ok();
        }
        // the worker's news
        let a = &mut self.atlas;
        if let Some(job) = &a.job {
            let mut end = false;
            while let Ok(m) = job.rx.try_recv() {
                match m {
                    Msg::Stage(s, f) => a.stage = (s, f),
                    Msg::Base(b) => a.atlas = Some(*b),
                    Msg::Layout(xy, f) => {
                        a.stage = ("Laying out", f);
                        if let Some(at) = a.atlas.as_mut() {
                            at.xy = xy;
                        }
                    }
                    Msg::Placed(nearest, weight) => {
                        if let Some(at) = a.atlas.as_mut() {
                            at.nearest = nearest;
                            at.weight = weight;
                        }
                        end = true;
                    }
                    Msg::Failed(e) => {
                        a.error = Some(e);
                        end = true;
                    }
                }
                a.generation += 1;
            }
            if end {
                a.job = None;
                if a.find_groups && a.atlas.is_some() {
                    find = true;
                }
            }
        }
        if let Some(e) = &a.error {
            ui.weak(e);
            return;
        }
        let Some(at) = a.atlas.as_ref() else {
            ui.weak(format!("{}...", a.stage.0));
            return;
        };
        if find {
            a.groups = suggest_groups(&at.xy, at.n, 3)
                .into_iter()
                .enumerate()
                .map(|(slot, poly)| Group { slot, poly })
                .collect();
            a.generation += 1;
            if a.colouring == 0 {
                a.colouring = 1;
            }
        }
        // each sample's group: the first whose outline holds it
        let gk = hash((a.generation, a.groups.len()));
        if a.group_key != gk || a.group_of.len() != at.n {
            a.group_of = (0..at.n)
                .into_par_iter()
                .map(|i| {
                    let (x, y) = (at.xy[2 * i], at.xy[2 * i + 1]);
                    a.groups.iter().find(|g| in_poly(&g.poly, x, y)).map_or(255, |g| g.slot as u8)
                })
                .collect();
            a.group_key = gk;
        }
        // each group's share of the region's pixels
        let total: f64 = at.weight.iter().map(|&w| f64::from(w)).sum::<f64>().max(1.0);
        let mut shares = [0f64; 3];
        for (&g, &w) in a.group_of.iter().zip(&at.weight) {
            if g != 255 {
                shares[g as usize] += f64::from(w) / total;
            }
        }
        let placed = !at.nearest.is_empty();
        ui.weak(format!(
            "{} pixels{}  ·  {} values each  ·  {} components ({:.0} % of the variation)  ·  close: alike over time",
            at.n,
            if at.step > 1 { format!(" (1 in {} x {})", at.step, at.step) } else { String::new() },
            at.nb,
            at.dims,
            100.0 * at.explained
        ));
        // the layout: the plane at the left, the groups and their curves at the right (or under it when narrow)
        let body = ui.text_style_height(&egui::TextStyle::Body);
        let avail = ui.available_size();
        let wide = avail.x > avail.y * 1.5;
        let plane = if wide {
            vec2((avail.x * 0.48).min(avail.y * 1.25), avail.y - 4.0)
        } else {
            vec2(avail.x, (avail.y * 0.58).max(160.0))
        };
        let side_w = body * 10.5;
        let mut clicked_pin = None;
        let mut date = None;
        let mut side = |ui: &mut Ui, s: &mut State, curves: egui::Vec2| {
            ui.vertical(|ui| {
                ui.set_width(side_w);
                s.atlas_legend(ui, &shares);
            });
            date = s.atlas_curves(ui, curves);
        };
        if wide {
            ui.horizontal_top(|ui| {
                clicked_pin = self.atlas_plane(ui, plane, placed);
                ui.add_space(10.0);
                let w = (ui.available_width() - side_w - 10.0).max(160.0);
                side(ui, self, vec2(w, plane.y));
            });
        } else {
            clicked_pin = self.atlas_plane(ui, plane, placed);
            ui.add_space(6.0);
            let w = (ui.available_width() - side_w - 10.0).max(160.0);
            let h = ui.available_height().max(80.0);
            ui.horizontal_top(|ui| side(ui, self, vec2(w, h)));
        }
        if let Some((x, y)) = clicked_pin {
            self.drop_pin(x, y);
        }
        if let Some(t) = date {
            self.set_date(t);
        }
        if self.atlas.job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
        }
    }

    /// The plane: the points, the groups' outlines, the lasso; a pin's place if clicked.
    fn atlas_plane(&mut self, ui: &mut Ui, size: egui::Vec2, placed: bool) -> Option<(usize, usize)> {
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        let dark = ui.visuals().dark_mode;
        let bg = ui.visuals().extreme_bg_color;
        let ink = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        let t_now = self.display.t;
        let value_mode = janus_gpu::Mode::Value as usize;
        let (cmap_value, cmap_anom) =
            (self.display.cmap[value_mode], self.display.cmap[janus_gpu::Mode::Anomaly as usize]);
        let range = (self.display.ranges[value_mode].lo, self.display.ranges[value_mode].hi);
        let map_hover = self.series.hover.as_ref().map(|h| (h.x, h.y));
        let ov = self.session.as_ref().map(|s| s.s.overview.clone());
        let a = &mut self.atlas;
        let at = a.atlas.as_ref()?;
        // the view: fitted to the layout unless moved
        let fit = || {
            let b = bounds(&at.xy);
            let upp = ((b[2] - b[0]) / rect.width()).max((b[3] - b[1]) / rect.height()).max(1e-6) * 1.25;
            ([0.5 * (b[0] + b[2]), 0.5 * (b[1] + b[3])], upp)
        };
        let (mut c, mut upp) = a.view.unwrap_or_else(fit);
        if resp.double_clicked() {
            a.view = None;
            (c, upp) = fit();
        }
        if let Some(p) = resp.hover_pos() {
            let wheel = ui.input(|i| i.smooth_scroll_delta.y);
            if wheel != 0.0 {
                let z = (-wheel / 300.0).exp();
                let before = [c[0] + (p.x - rect.center().x) * upp, c[1] + (p.y - rect.center().y) * upp];
                upp *= z;
                c = [before[0] - (p.x - rect.center().x) * upp, before[1] - (p.y - rect.center().y) * upp];
                a.view = Some((c, upp));
                ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
            }
        }
        if resp.dragged_by(egui::PointerButton::Secondary) || resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            c = [c[0] - d.x * upp, c[1] - d.y * upp];
            a.view = Some((c, upp));
        }
        let to_screen = |x: f32, y: f32| rect.center() + vec2((x - c[0]) / upp, (y - c[1]) / upp);
        let to_plane = |p: Pos2| [c[0] + (p.x - rect.center().x) * upp, c[1] + (p.y - rect.center().y) * upp];
        // the lasso
        let laid_out = a.job.is_none();
        if laid_out
            && resp.dragged_by(egui::PointerButton::Primary)
            && let Some(p) = resp.interact_pointer_pos()
        {
            a.lasso.push(to_plane(p));
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary) && a.lasso.len() >= 3 {
            let lasso = std::mem::take(&mut a.lasso);
            let free = (0..3).find(|s| a.groups.iter().all(|g| g.slot != *s));
            if let Some(slot) = free {
                a.groups.push(Group { slot, poly: lasso });
                a.groups.sort_by_key(|g| g.slot);
                a.generation += 1;
                if matches!(a.colouring, 0) {
                    a.colouring = 1;
                }
            }
        } else if !resp.dragged() {
            a.lasso.clear();
        }
        // what the points show
        let attr_key = hash((a.colouring, t_now, a.generation));
        let attr: Option<&[f32]> = match a.colouring {
            2 => {
                if a.attr.as_ref().is_none_or(|x| x.0 != attr_key) {
                    let layer = ov.as_ref().and_then(|o| o.layer(t_now.min(o.t - 1)));
                    let v = at
                        .pixel
                        .iter()
                        .map(|p| layer.map_or(f32::NAN, |l| l[p[1] as usize * at.ow + p[0] as usize]))
                        .collect();
                    a.attr = Some((attr_key, v));
                }
                a.attr.as_ref().map(|x| &x.1[..])
            }
            3 => Some(&at.mean[..]),
            4 => Some(&at.amp[..]),
            5 => Some(&at.trend[..]),
            _ => None,
        };
        let spread = |v: &[f32], lo: f64, hi: f64| {
            let mut s: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
            if s.is_empty() {
                return (0.0, 1.0);
            }
            let n = s.len() - 1;
            let a = *s.select_nth_unstable_by((n as f64 * lo) as usize, f32::total_cmp).1;
            let b = *s.select_nth_unstable_by((n as f64 * hi) as usize, f32::total_cmp).1;
            (a, b.max(a + 1e-12))
        };
        let colormaps = janus_gpu::colormaps::all();
        let (paint, legend): (Paint, Option<(usize, f32, f32)>) = match (a.colouring, attr) {
            (1, _) => (Paint::Groups(&a.group_of, weak), None),
            (2 | 3, Some(v)) => (Paint::Values(v, range, &colormaps[cmap_value]), Some((cmap_value, range.0, range.1))),
            (4, Some(v)) => {
                let r = spread(v, 0.02, 0.98);
                (Paint::Values(v, r, &colormaps[cmap_value]), Some((cmap_value, r.0, r.1)))
            }
            (5, Some(v)) => {
                let (lo, hi) = spread(v, 0.02, 0.98);
                let m = lo.abs().max(hi.abs());
                (Paint::Values(v, (-m, m), &colormaps[cmap_anom]), Some((cmap_anom, -m, m)))
            }
            _ => (Paint::Density, None),
        };
        let ppp = ui.ctx().pixels_per_point();
        let (pw, ph) =
            ((rect.width() * ppp).round().max(8.0) as usize, (rect.height() * ppp).round().max(8.0) as usize);
        let key = hash((
            (a.generation, a.colouring, attr_key),
            pw,
            ph,
            c.map(f32::to_bits),
            upp.to_bits(),
            dark,
            bg.to_array(),
            cmap_value,
            cmap_anom,
            range.0.to_bits(),
            range.1.to_bits(),
        ));
        if a.image.as_ref().is_none_or(|i| i.0 != key) {
            let to_px = |x: f32, y: f32| {
                let p = to_screen(x, y) - rect.min;
                (p.x * ppp, p.y * ppp)
            };
            let image = render(&at.xy, at.n, &to_px, pw, ph, &paint, dark, bg);
            match a.image.as_mut() {
                Some((k, tex)) => {
                    tex.set(image, egui::TextureOptions::LINEAR);
                    *k = key;
                }
                None => a.image = Some((key, ui.ctx().load_texture("atlas", image, egui::TextureOptions::LINEAR))),
            }
        }
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, bg);
        if let Some((_, tex)) = &a.image {
            painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
        }
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, weak.gamma_multiply(0.35)), egui::StrokeKind::Inside);
        // the groups' outlines and names
        let body = ui.text_style_height(&egui::TextStyle::Body);
        for g in &a.groups {
            let pts: Vec<Pos2> = g.poly.iter().map(|p| to_screen(p[0], p[1])).collect();
            let col = group_colour(g.slot);
            let mut closed = pts.clone();
            closed.push(pts[0]);
            painter.add(egui::Shape::line(closed.clone(), Stroke::new(3.5, bg.gamma_multiply(0.8))));
            painter.add(egui::Shape::line(closed, Stroke::new(1.6, col)));
            // the name at the top of the outline
            let top = pts.iter().copied().min_by(|a, b| a.y.total_cmp(&b.y)).unwrap_or(rect.center());
            let galley = painter.layout_no_wrap(
                NAMES[g.slot].to_string(),
                egui::FontId::proportional(body * 0.95),
                Color32::WHITE,
            );
            let r =
                Rect::from_center_size(top - vec2(0.0, galley.size().y * 0.5 + 4.0), galley.size() + vec2(10.0, 2.0));
            painter.rect_filled(r, r.height() * 0.5, col);
            painter.galley(r.center() - galley.size() * 0.5, galley, Color32::WHITE);
        }
        if a.lasso.len() >= 2 {
            let pts: Vec<Pos2> = a.lasso.iter().map(|p| to_screen(p[0], p[1])).collect();
            let free = (0..3).find(|s| a.groups.iter().all(|g| g.slot != *s));
            let col = free.map_or(weak, group_colour);
            painter.add(egui::Shape::dashed_line(&pts, Stroke::new(1.5, col), 6.0, 4.0));
            if free.is_none() {
                painter.text(
                    rect.center_top() + vec2(0.0, 8.0),
                    egui::Align2::CENTER_TOP,
                    "Three groups already: clear one first",
                    egui::FontId::proportional(body * 0.85),
                    ink,
                );
            }
        }
        // the pixel the pointer is over on the map
        let map_sample = map_hover.and_then(|(x, y)| at.sample_at(x, y));
        if let Some(s) = map_sample {
            let p = to_screen(at.xy[2 * s], at.xy[2 * s + 1]);
            painter.circle_stroke(p, 7.0, Stroke::new(3.5, bg));
            painter.circle_stroke(p, 7.0, Stroke::new(1.6, ink));
            painter.circle_filled(p, 2.0, ink);
        }
        // the pointer: the nearest point, the points in its reach lit on the map
        a.hovered = None;
        let mut brushed = Vec::new();
        let mut pin = None;
        if let Some(p) = resp.hover_pos().filter(|_| !resp.dragged()) {
            let r2 = (BRUSH * upp).powi(2);
            let q = to_plane(p);
            let mut best = (f32::INFINITY, 0usize);
            for i in 0..at.n {
                let d = (at.xy[2 * i] - q[0]).powi(2) + (at.xy[2 * i + 1] - q[1]).powi(2);
                if d < r2 {
                    brushed.push(i as u32);
                }
                if d < best.0 {
                    best = (d, i);
                }
            }
            painter.circle_stroke(p, BRUSH, Stroke::new(1.0, ink.gamma_multiply(0.4)));
            if best.0 < r2 {
                let s = best.1;
                a.hovered = Some(s);
                let sp = to_screen(at.xy[2 * s], at.xy[2 * s + 1]);
                painter.circle_stroke(sp, 4.5, Stroke::new(1.5, ink));
                let [ox, oy] = at.pixel[s];
                let (sx, sy) = (
                    ((ox as f64 + 0.5) * at.src.0 as f64 / at.ow as f64) as usize,
                    ((oy as f64 + 0.5) * at.src.1 as f64 / at.oh as f64) as usize,
                );
                let unit = if at.trend.is_empty() { "" } else { " a year" };
                let text = format!(
                    "pixel {sx}, {sy}\nmean {}  ·  range {}  ·  trend {:+}{unit}\n{} like it nearby (lit on the map)\n\
                     click: a pin here  ·  drag: draw a group",
                    format_g(f64::from(at.mean[s]), 3),
                    format_g(f64::from(at.amp[s]), 3),
                    format_g(f64::from(at.trend[s]), 2),
                    brushed.len()
                );
                if resp.clicked() {
                    pin = Some((sx.min(at.src.0 - 1), sy.min(at.src.1 - 1)));
                }
                resp.clone().on_hover_text(text);
            }
        }
        // under way: the stage and a thin bar
        if let Some(_job) = &a.job {
            let (stage, f) = a.stage;
            painter.text(
                rect.left_top() + vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                format!("{stage}{}", if f > 0.0 { format!("  {:.0} %", 100.0 * f) } else { "...".into() }),
                egui::FontId::proportional(body * 0.85),
                weak,
            );
            let bar =
                Rect::from_min_size(rect.left_top() + vec2(1.0, 1.0), vec2((rect.width() - 2.0) * f.max(0.02), 2.0));
            painter.rect_filled(bar, 1.0, theme::on_plot(group_colour(0)));
        } else if !placed {
            painter.text(
                rect.left_top() + vec2(10.0, 8.0),
                egui::Align2::LEFT_TOP,
                "Placing every pixel...",
                egui::FontId::proportional(body * 0.85),
                weak,
            );
        }
        if a.groups.is_empty() && a.job.is_none() {
            painter.text(
                rect.center_bottom() - vec2(0.0, 8.0),
                egui::Align2::CENTER_BOTTOM,
                "Drag around points to make a group  ·  scroll: zoom  ·  right drag: move  ·  double click: all",
                egui::FontId::proportional(body * 0.8),
                weak,
            );
        }
        // the colour's scale
        if let Some((cm, lo, hi)) = legend {
            let bar = Rect::from_min_size(rect.right_bottom() - vec2(150.0, 30.0), vec2(136.0, 8.0));
            let cmap = &janus_gpu::colormaps::all()[cm];
            for i in 0..32 {
                let col = cmap.sample((i as f32 + 0.5) / 32.0);
                let x = |k: usize| bar.left() + bar.width() * k as f32 / 32.0;
                painter.rect_filled(
                    Rect::from_x_y_ranges(x(i)..=x(i + 1), bar.y_range()),
                    0.0,
                    Color32::from_rgb(col[0], col[1], col[2]),
                );
            }
            let small = egui::FontId::proportional(body * 0.75);
            painter.text(
                bar.left_bottom() + vec2(0.0, 2.0),
                egui::Align2::LEFT_TOP,
                format_g(f64::from(lo), 3),
                small.clone(),
                weak,
            );
            painter.text(
                bar.right_bottom() + vec2(0.0, 2.0),
                egui::Align2::RIGHT_TOP,
                format_g(f64::from(hi), 3),
                small,
                weak,
            );
        }
        a.brushed = brushed;
        self.atlas_overlay(ui.ctx());
        pin
    }

    /// The groups and the pixels like the pointer's on the map: a texture over the region.
    fn atlas_overlay(&mut self, ctx: &egui::Context) {
        let a = &mut self.atlas;
        let Some(at) = a.atlas.as_ref().filter(|at| !at.nearest.is_empty()) else {
            a.overlay = None;
            return;
        };
        let any = !a.groups.is_empty() || !a.brushed.is_empty();
        if !any || a.hide_map {
            a.overlay = None;
            return;
        }
        let key = hash((a.generation, a.group_key, &a.brushed));
        if a.overlay.as_ref().is_some_and(|o| o.0 == key) {
            return;
        }
        // each sample's colour on the map
        let mut colour: Vec<Color32> =
            a.group_of
                .iter()
                .map(|&g| {
                    if g == 255 { Color32::from_black_alpha(90) } else { group_colour(g as usize).gamma_multiply(0.62) }
                })
                .collect();
        for &s in &a.brushed {
            colour[s as usize] = Color32::from_white_alpha(235);
        }
        let (rw, rh) = at.region();
        let pixels: Vec<Color32> =
            at.nearest.par_iter().map(|&s| if s == NONE { Color32::TRANSPARENT } else { colour[s as usize] }).collect();
        let image = egui::ColorImage { size: [rw, rh], pixels, source_size: vec2(rw as f32, rh as f32) };
        match a.overlay.as_mut() {
            Some((k, tex)) => {
                tex.set(image, egui::TextureOptions::NEAREST);
                *k = key;
            }
            None => a.overlay = Some((key, ctx.load_texture("atlas on the map", image, egui::TextureOptions::NEAREST))),
        }
    }

    /// Over a map canvas: the groups' pixels (while Analysis shows the tab).
    pub(crate) fn draw_atlas_marks(&self, painter: &egui::Painter, at: &dyn Fn(f64, f64) -> Pos2) {
        let a = &self.atlas;
        let (Some((_, tex)), Some(atlas)) = (&a.overlay, &a.atlas) else { return };
        if !self.analysis.show || self.shown_analysis_tab() != 9 {
            return;
        }
        let (fx, fy) = (atlas.src.0 as f64 / atlas.ow as f64, atlas.src.1 as f64 / atlas.oh as f64);
        let [x0, y0, x1, y1] = atlas.rect;
        let r = Rect::from_min_max(at(x0 as f64 * fx, y0 as f64 * fy), at(x1 as f64 * fx, y1 as f64 * fy));
        painter.image(tex.id(), r, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
    }

    /// The groups: each one's colour, name and share of the region.
    fn atlas_legend(&mut self, ui: &mut Ui, shares: &[f64; 3]) {
        let a = &mut self.atlas;
        ui.label(egui::RichText::new("Groups").strong());
        if a.groups.is_empty() {
            ui.weak("None yet: drag around a cluster of points, or Find groups.");
        }
        let mut remove = None;
        for g in &a.groups {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 3.0, group_colour(g.slot));
                ui.label(egui::RichText::new(NAMES[g.slot]).strong());
                ui.label(format!("{:.1} %", 100.0 * shares[g.slot]))
                    .on_hover_text("Its share of the region's pixels (each placed with its nearest point)");
                if ui.small_button("×").on_hover_text("Remove the group").clicked() {
                    remove = Some(g.slot);
                }
            });
        }
        if let Some(s) = remove {
            a.groups.retain(|g| g.slot != s);
            a.generation += 1;
        }
        let rest = 1.0 - (0..3).filter(|s| a.groups.iter().any(|g| g.slot == *s)).map(|s| shares[s]).sum::<f64>();
        if !a.groups.is_empty() {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 3.0, ui.visuals().weak_text_color().gamma_multiply(0.5));
                ui.weak(format!("the others  {:.1} %", 100.0 * rest.max(0.0)));
            });
        }
    }

    /// The groups' curves over time (all the pixels' when there is no group), the pointer's pixel's too; a click's
    /// date.
    fn atlas_curves(&mut self, ui: &mut Ui, size: egui::Vec2) -> Option<usize> {
        let info = self.session.as_ref()?.info.clone();
        let dated = !info.stack && !info.spectral;
        let cur = self.display.t.min(info.t() - 1);
        let cur_x = if dated { info.layers[cur].time } else { cur as f64 };
        let a = &mut self.atlas;
        let at = a.atlas.as_ref()?;
        let weak = ui.visuals().weak_text_color();
        let ink = ui.visuals().text_color();
        let key = hash((a.generation, a.group_key, at.n));
        if a.curves.as_ref().is_none_or(|c| c.0 != key) {
            let nb = at.nb;
            let slots: Vec<Option<usize>> =
                if a.groups.is_empty() { vec![None] } else { a.groups.iter().map(|g| Some(g.slot)).collect() };
            let curves = slots
                .into_iter()
                .filter_map(|slot| {
                    let members: Vec<usize> =
                        (0..at.n).filter(|&i| slot.is_none_or(|s| a.group_of[i] as usize == s)).collect();
                    if members.is_empty() {
                        return None;
                    }
                    let mut mean = vec![0f32; nb];
                    let mut lo = vec![0f32; nb];
                    let mut hi = vec![0f32; nb];
                    mean.par_iter_mut().zip(lo.par_iter_mut()).zip(hi.par_iter_mut()).enumerate().for_each(
                        |(b, ((m, l), h))| {
                            let mut v: Vec<f32> = members.iter().map(|&i| at.raw[i * nb + b]).collect();
                            *m = v.iter().sum::<f32>() / v.len() as f32;
                            let n = v.len() - 1;
                            *l = *v.select_nth_unstable_by((n as f32 * 0.1) as usize, f32::total_cmp).1;
                            *h = *v.select_nth_unstable_by((n as f32 * 0.9) as usize, f32::total_cmp).1;
                        },
                    );
                    let colour = slot.map_or(ink.gamma_multiply(0.8), |s| theme::on_plot(group_colour(s)));
                    Some(Curve { colour, mean, lo, hi })
                })
                .collect();
            a.curves = Some((key, curves));
        }
        let curves = &a.curves.as_ref()?.1;
        let (ylo, yhi) = curves.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(l, h), c| {
            (c.lo.iter().copied().fold(l, f32::min), c.hi.iter().copied().fold(h, f32::max))
        });
        let hovered = a.hovered.map(|s| &at.raw[s * at.nb..(s + 1) * at.nb]);
        let mut plot = Plot::new(("atlas curves", info.id))
            .width(size.x)
            .height(size.y)
            .include_x(at.bin_x[0])
            .include_x(at.bin_x[at.nb - 1])
            .include_y(f64::from(ylo))
            .include_y(f64::from(yhi))
            .set_margin_fraction(vec2(0.02, 0.08))
            .show_background(false)
            .allow_drag(false)
            .allow_scroll(false)
            .allow_zoom(false)
            .allow_boxed_zoom(false)
            .show_x(false)
            .show_y(false)
            .y_axis_min_width(ui.text_style_height(&egui::TextStyle::Body) * 3.0)
            .y_grid_spacer(move |g| nice_marks(g.bounds))
            .custom_y_axes(vec![
                egui_plot::AxisHints::new_y()
                    .formatter(|m, _| format_g(m.value, 3))
                    .tick_label_color(weak)
                    .label_spacing(14.0..=15.0),
            ]);
        if dated {
            plot = crate::chart::time_axis(plot, Some(weak));
        }
        let resp = plot.show(ui, |p| {
            p.vline(VLine::new("", cur_x).color(ink.gamma_multiply(0.5)).width(1.0).allow_hover(false));
            p.pointer_coordinate()
        });
        let t = resp.transform;
        let painter = ui.painter_at(*t.frame());
        let px = |x: f64, y: f32| t.position_from_point(&PlotPoint::new(x, f64::from(y)));
        // the ranges (a mesh, without seams), then the means over them
        for c in curves {
            let mut mesh = egui::Mesh::default();
            let fill = c.colour.gamma_multiply(if curves.len() == 1 { 0.16 } else { 0.2 });
            for b in 0..at.nb {
                mesh.colored_vertex(px(at.bin_x[b], c.lo[b]), fill);
                mesh.colored_vertex(px(at.bin_x[b], c.hi[b]), fill);
                if b > 0 {
                    let k = 2 * b as u32;
                    mesh.add_triangle(k - 2, k - 1, k);
                    mesh.add_triangle(k - 1, k + 1, k);
                }
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        for c in curves {
            let pts: Vec<Pos2> = (0..at.nb).map(|b| px(at.bin_x[b], c.mean[b])).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(2.0, c.colour)));
        }
        if let Some(r) = hovered {
            let pts: Vec<Pos2> = (0..at.nb).map(|b| px(at.bin_x[b], r[b])).collect();
            painter.add(egui::Shape::line(pts, Stroke::new(1.0, ink)));
        }
        // the pointer: the values at its bin
        let mut date = None;
        if let Some(pt) = resp.inner.filter(|_| resp.response.hovered()) {
            let b = (0..at.nb).min_by(|&p, &q| (at.bin_x[p] - pt.x).abs().total_cmp(&(at.bin_x[q] - pt.x).abs()))?;
            let x = px(at.bin_x[b], 0.0).x;
            let frame = *t.frame();
            painter.line_segment([Pos2::new(x, frame.top()), Pos2::new(x, frame.bottom())], Stroke::new(1.0, weak));
            let when =
                if dated { info.format_time(at.bin_x[b]) } else { format!("band {}", at.bin_x[b].round() + 1.0) };
            let mut text = when;
            for (k, c) in curves.iter().enumerate() {
                let name = a.groups.get(k).map_or("all", |g| NAMES[g.slot]);
                text += &format!(
                    "\n{name}: {}  ({} to {})",
                    format_g(f64::from(c.mean[b]), 3),
                    format_g(f64::from(c.lo[b]), 3),
                    format_g(f64::from(c.hi[b]), 3)
                );
                painter.circle_filled(px(at.bin_x[b], c.mean[b]), 3.5, c.colour);
            }
            if resp.response.clicked() {
                // the bin's date nearest to the pointer
                date = (0..info.t()).min_by(|&p, &q| {
                    let xp = if dated { info.layers[p].time } else { p as f64 };
                    let xq = if dated { info.layers[q].time } else { q as f64 };
                    (xp - pt.x).abs().total_cmp(&(xq - pt.x).abs())
                });
            }
            resp.response.on_hover_text(text + "\nclick: show this date on the map");
        }
        date
    }

    /// The atlas laid out and its pixels placed (or not asked for).
    pub(crate) fn atlas_settled(&self) -> bool {
        !(self.analysis.show && self.shown_analysis_tab() == 9)
            || self.atlas.request.is_some() && self.atlas.job.is_none() && self.atlas.overlay_ready()
    }
}

impl AtlasUi {
    fn overlay_ready(&self) -> bool {
        self.groups.is_empty() || self.overlay.is_some() || self.hide_map
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hulls_and_polygons() {
        let pts = vec![[0.0, 0.0], [2.0, 0.0], [1.0, 1.0], [2.0, 2.0], [0.0, 2.0], [1.0, 0.5]];
        let h = hull(pts);
        assert_eq!(h.len(), 4);
        assert!(in_poly(&h, 1.0, 1.0) && !in_poly(&h, 3.0, 1.0));
    }

    #[test]
    fn groups_are_found_apart() {
        // three blobs
        let mut xy = Vec::new();
        for (cx, cy) in [(0.0, 0.0), (10.0, 0.0), (5.0, 9.0)] {
            for k in 0..200 {
                let a = k as f32 * 0.7;
                let r = (k % 17) as f32 * 0.08;
                xy.extend([cx + r * a.cos(), cy + r * a.sin()]);
            }
        }
        let groups = suggest_groups(&xy, 600, 3);
        assert_eq!(groups.len(), 3);
        for (cx, cy) in [(0.0, 0.0), (10.0, 0.0), (5.0, 9.0)] {
            assert_eq!(groups.iter().filter(|g| in_poly(g, cx, cy)).count(), 1);
        }
    }
}
