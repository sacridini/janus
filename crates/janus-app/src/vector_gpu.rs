//! Vector layers drawn by the GPU (janus-gpu's vector pipelines), as a GIS draws large layers: the geometry
//! uploaded once, nothing per frame but a transform and the ranges in view.
//!
//! - Levels of detail: each feature's rings simplified (Douglas-Peucker) at tolerances doubling from a fraction of
//!   a source pixel; a frame draws the coarsest level whose tolerance stays under half a pixel of the target, so a
//!   country's municipalities zoomed out draw a few hundred thousand points, and every point zoomed in.
//! - Pieces: the features in the order of a Morton curve over their centres, cut into pieces of at most
//!   PIECE_POINTS points; each piece's points relative to its centre (float32 keeps its precision zoomed in, the
//!   centres given to the shader in two floats). A level is one buffer, its pieces one after the other: the pieces
//!   in view, near each other along the curve, make a few runs, a draw call each.
//! - Fills (when the layer is filled): each polygon's triangles (earcut), its holes the rings turning the other
//!   way.
//!
//! Made on a thread (in parallel over the pieces) from the layer as placed on the active layer's grid; drawn into
//! the map's frame in the drawing order of the layers (order.rs). Picking and the highlighted feature stay on the
//! CPU (vectors.rs).

use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use janus_geo::plane::Polygon;
use janus_geo::vectors::Kind;
use janus_gpu::{Gpu, MapFrame, VectorFeatures, VectorGeometry, VectorKind, VectorPoint, VectorStyle, VectorView};
use rayon::prelude::*;

/// Points of a piece, at most (at full detail).
const PIECE_POINTS: usize = 1 << 14;
/// The finest simplified level's tolerance, in source pixels (the others double it).
const FIRST_TOLERANCE: f64 = 1.0 / 32.0;

/// A level of a layer: its points (the pieces one after the other), its fills' triangles, each piece's ranges of
/// both.
#[derive(Default)]
struct Level {
    pts: Vec<VectorPoint>,
    tris: Vec<u32>,
    ranges: Vec<Range<u32>>,
    tri_ranges: Vec<Range<u32>>,
}

/// A layer's geometry made for the GPU.
pub struct Built {
    pub key: u64,
    tols: Vec<f64>,
    bboxes: Vec<[f64; 4]>,
    origins: Vec<[f64; 2]>,
    piece_of: Vec<u32>,
    levels: Vec<Level>,
    /// the point features
    points: Level,
    pub ms: f64,
}

/// A level on the GPU: its geometry, each piece's ranges of points and of triangles.
type GpuLevel = (VectorGeometry, Vec<Range<u32>>, Vec<Range<u32>>);

/// A layer on the GPU.
pub struct VectorGpu {
    pub key: u64,
    tols: Vec<f64>,
    bboxes: Vec<[f64; 4]>,
    levels: Vec<GpuLevel>,
    points: Option<(VectorGeometry, Vec<Range<u32>>)>,
    features: VectorFeatures,
    colors_hash: u64,
    pub bytes: usize,
    /// points of each level
    pub level_points: Vec<usize>,
    pub build_ms: f64,
}

/// Douglas-Peucker of an open polyline at every tolerance at once: into `keep2`, each point's largest tolerance
/// (squared) at which it is kept (the ends: always). The splits do not depend on the tolerance (a segment splits
/// at its farthest point), so at a tolerance t a point is kept when every split leading to it is beyond t: its
/// value is the least of them.
fn dp_tolerances(p: &[[f64; 2]], keep2: &mut [f64]) {
    let n = p.len();
    if n == 0 {
        return;
    }
    keep2[0] = f64::INFINITY;
    keep2[n - 1] = f64::INFINITY;
    let mut stack = vec![(0usize, n - 1, f64::INFINITY)];
    while let Some((a, b, cap)) = stack.pop() {
        if b <= a + 1 {
            continue;
        }
        let (ax, ay) = (p[a][0], p[a][1]);
        let (dx, dy) = (p[b][0] - ax, p[b][1] - ay);
        let len2 = dx * dx + dy * dy;
        let (mut best, mut at) = (-1.0, a);
        for (i, q) in p.iter().enumerate().take(b).skip(a + 1) {
            let (qx, qy) = (q[0] - ax, q[1] - ay);
            let d2 = if len2 > 0.0 {
                let c = qx * dy - qy * dx;
                c * c / len2
            } else {
                qx * qx + qy * qy
            };
            if d2 > best {
                (best, at) = (d2, i);
            }
        }
        let v = best.min(cap);
        keep2[at] = v;
        stack.push((a, at, v));
        stack.push((at, b, v));
    }
}

/// A ring ready to be simplified at any tolerance: its points (a closed one closed again), the tolerance (squared)
/// up to which Douglas-Peucker keeps each one, its box's longer side.
struct Simplified {
    pts: Vec<[f64; 2]>,
    keep2: Vec<f64>,
    size: f64,
    closed: bool,
}

impl Simplified {
    /// A ring (x, y pairs); a closed one split at the point farthest from its first: two open halves.
    fn new(r: &[f64], closed: bool) -> Self {
        let mut pts: Vec<[f64; 2]> = r.as_chunks::<2>().0.to_vec();
        let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for q in &pts {
            (x0, y0, x1, y1) = (x0.min(q[0]), y0.min(q[1]), x1.max(q[0]), y1.max(q[1]));
        }
        let mut keep2 = vec![0.0; pts.len() + 1];
        if closed && !pts.is_empty() {
            let far = (0..pts.len())
                .max_by(|&a, &b| {
                    let d = |i: usize| (pts[i][0] - pts[0][0]).powi(2) + (pts[i][1] - pts[0][1]).powi(2);
                    d(a).total_cmp(&d(b))
                })
                .unwrap_or(0);
            if pts.first() != pts.last() {
                pts.push(pts[0]);
            }
            dp_tolerances(&pts[..=far], &mut keep2[..=far]);
            dp_tolerances(&pts[far..], &mut keep2[far..pts.len()]);
        } else {
            dp_tolerances(&pts, &mut keep2[..pts.len()]);
        }
        keep2.truncate(pts.len());
        Simplified { pts, keep2, size: (x1 - x0).max(y1 - y0), closed }
    }

    /// Its points at `tol` (0: as it is), into `out`: closed again for a closed one; smaller than the tolerance,
    /// its first point twice (a dot). False if it has none.
    fn at(&self, tol: f64, out: &mut impl FnMut([f64; 2])) -> bool {
        let p = &self.pts;
        if p.is_empty() {
            return false;
        }
        if tol <= 0.0 {
            p.iter().for_each(|q| out(*q));
        } else if self.size < tol {
            out(p[0]);
            out(p[0]);
        } else {
            let t2 = tol * tol;
            let n = if self.closed { p.len() - 1 } else { p.len() };
            p[..n].iter().zip(&self.keep2).filter(|(_, k)| **k > t2).for_each(|(q, _)| out(*q));
            if self.closed {
                out(p[0]);
            }
        }
        true
    }
}

/// Twice the signed area of a ring.
fn signed_area(r: &[[f64; 2]]) -> f64 {
    let n = r.len();
    (0..n).map(|i| r[i][0] * r[(i + 1) % n][1] - r[(i + 1) % n][0] * r[i][1]).sum()
}

/// The triangles of a feature's rings (even-odd: each ring turning the first one's way starts a polygon, the
/// others are its holes), as indices of the rings' points (`at`: where each ring's first point is).
fn triangulate(rings: &[Vec<[f64; 2]>], at: &[u32], out: &mut Vec<u32>) {
    let mut ec = earcut::Earcut::new();
    let (mut tris, mut data, mut holes, mut index): (Vec<u32>, Vec<[f64; 2]>, Vec<u32>, Vec<u32>) =
        (vec![], vec![], vec![], vec![]);
    let mut flush = |group: &mut Vec<usize>, out: &mut Vec<u32>| {
        if group.is_empty() {
            return;
        }
        data.clear();
        holes.clear();
        index.clear();
        for (k, &ri) in group.iter().enumerate() {
            let r = &rings[ri];
            // the closing point left out (earcut closes the rings)
            let n = if r.len() > 1 && r.first() == r.last() { r.len() - 1 } else { r.len() };
            if k > 0 {
                holes.push(data.len() as u32);
            }
            for (j, q) in r.iter().take(n).enumerate() {
                data.push(*q);
                index.push(at[ri] + j as u32);
            }
        }
        ec.earcut(data.iter().copied(), &holes, &mut tris);
        out.extend(tris.iter().map(|&i| index[i as usize]));
        group.clear();
    };
    let (mut first_sign, mut group) = (0.0, Vec::new());
    for (ri, r) in rings.iter().enumerate() {
        if r.len() < 4 {
            continue;
        }
        let s = signed_area(r).signum();
        if first_sign == 0.0 {
            first_sign = s;
        }
        if s == first_sign {
            flush(&mut group, out);
        }
        group.push(ri);
    }
    flush(&mut group, out);
}

/// A 2-D Morton code of a point in [0, 1)^2 (16 bits each axis).
fn morton(x: f64, y: f64) -> u64 {
    let q = |v: f64| ((v.clamp(0.0, 1.0) * 65535.0) as u64) & 0xffff;
    let spread = |mut v: u64| {
        v = (v | (v << 8)) & 0x00ff_00ff;
        v = (v | (v << 4)) & 0x0f0f_0f0f;
        v = (v | (v << 2)) & 0x3333_3333;
        (v | (v << 1)) & 0x5555_5555
    };
    spread(q(x)) | (spread(q(y)) << 1)
}

/// A layer's pieces and levels (in parallel over the pieces): `px` its features' rings in the active layer's
/// pixels, `fill` with the areas' triangles.
pub fn build(px: &[Polygon], kinds: &[Kind], fill: bool, key: u64) -> Built {
    let t0 = std::time::Instant::now();
    let boxes: Vec<Option<[f64; 4]>> = px.par_iter().map(|p| janus_geo::plane::bounds(p)).collect();
    let mut all = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for b in boxes.iter().flatten() {
        all = [all[0].min(b[0]), all[1].min(b[1]), all[2].max(b[2]), all[3].max(b[3])];
    }
    let span = (all[2] - all[0]).max(all[3] - all[1]).max(1e-9);
    // tolerances: 0 (as it is), then doubling until a level is as coarse as a 512-pixel view of the whole layer
    let mut tols = vec![0.0];
    let mut t = FIRST_TOLERANCE;
    while t < span / 512.0 && tols.len() < 32 {
        tols.push(t);
        t *= 2.0;
    }
    // the features along a Morton curve, cut into pieces
    let mut order: Vec<usize> = (0..px.len()).filter(|&i| boxes[i].is_some() && kinds[i] != Kind::None).collect();
    order.sort_by_cached_key(|&i| {
        let b = boxes[i].unwrap();
        morton(((b[0] + b[2]) / 2.0 - all[0]) / span, ((b[1] + b[3]) / 2.0 - all[1]) / span)
    });
    let mut pieces_of: Vec<Vec<usize>> = vec![];
    let mut count = PIECE_POINTS;
    for &i in &order {
        let k: usize = px[i].iter().map(|r| r.len() / 2 + 2).sum();
        if count + k > PIECE_POINTS && count > 0 {
            pieces_of.push(vec![]);
            count = 0;
        }
        pieces_of.last_mut().unwrap().push(i);
        count += k;
    }
    let mut piece_of = vec![0u32; px.len()];
    for (k, feats) in pieces_of.iter().enumerate() {
        for &i in feats {
            piece_of[i] = k as u32;
        }
    }
    // each piece: its box, origin, point features and levels
    type Piece = ([f64; 4], [f64; 2], Vec<VectorPoint>, Vec<(Vec<VectorPoint>, Vec<u32>)>);
    let pieces: Vec<Piece> = pieces_of
        .par_iter()
        .map(|feats| {
            let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for &i in feats {
                let c = boxes[i].unwrap();
                b = [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[2]), b[3].max(c[3])];
            }
            let origin = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
            let rel = |q: [f64; 2], f: usize| VectorPoint {
                x: (q[0] - origin[0]) as f32,
                y: (q[1] - origin[1]) as f32,
                f: f as u32,
            };
            let mut points = vec![];
            for &i in feats.iter().filter(|&&i| kinds[i] == Kind::Point) {
                points.extend(px[i].iter().filter(|r| r.len() >= 2).map(|r| rel([r[0], r[1]], i)));
            }
            // each ring's Douglas-Peucker once, every level then a filter of it
            let shapes: Vec<(usize, Vec<Simplified>)> = feats
                .iter()
                .filter(|&&i| matches!(kinds[i], Kind::Line | Kind::Area))
                .map(|&i| (i, px[i].iter().map(|r| Simplified::new(r, kinds[i] == Kind::Area)).collect()))
                .collect();
            let mut levels = Vec::with_capacity(tols.len());
            let (mut at, mut ring) = (Vec::new(), Vec::new());
            let mut rings: Vec<Vec<[f64; 2]>> = Vec::new();
            for &tol in &tols {
                let (mut pts, mut tris) = (Vec::new(), Vec::new());
                for (i, s) in &shapes {
                    let filled = fill && kinds[*i] == Kind::Area;
                    // each ring a line of its own: a break after it
                    at.clear();
                    rings.clear();
                    for r in s {
                        let start = pts.len();
                        if !r.at(tol, &mut |q| pts.push(rel(q, *i))) {
                            continue;
                        }
                        at.push(start as u32);
                        if filled {
                            ring.clear();
                            r.at(tol, &mut |q| ring.push(q));
                            rings.push(ring.clone());
                        }
                        pts.push(VectorPoint::BREAK);
                    }
                    if filled {
                        triangulate(&rings, &at, &mut tris);
                    }
                }
                levels.push((pts, tris));
            }
            (b, origin, points, levels)
        })
        .collect();
    // a level kept only if it has at most 60% of the points of the finer one kept (a frame then draws at most
    // twice the detail it needs; levels nearly as large as the one before them cost memory for nothing)
    let totals: Vec<usize> = (0..tols.len()).map(|l| pieces.iter().map(|p| p.3[l].0.len()).sum()).collect();
    let mut keep = vec![false; tols.len()];
    let mut last = usize::MAX;
    for (l, &n) in totals.iter().enumerate() {
        if l == 0 || n as f64 <= 0.6 * last as f64 {
            (keep[l], last) = (true, n);
        }
    }
    let tols: Vec<f64> = tols.iter().zip(&keep).filter(|(_, k)| **k).map(|(t, _)| *t).collect();
    // the pieces one after the other, level by level
    let mut levels: Vec<Level> = (0..tols.len()).map(|_| Level::default()).collect();
    let mut points = Level::default();
    let (mut bboxes, mut origins) = (Vec::with_capacity(pieces.len()), Vec::with_capacity(pieces.len()));
    for (b, o, pp, ls) in pieces {
        bboxes.push(b);
        origins.push(o);
        let s = points.pts.len() as u32;
        points.pts.extend(pp);
        points.ranges.push(s..points.pts.len() as u32);
        let ls = ls.into_iter().zip(&keep).filter(|(_, k)| **k).map(|(l, _)| l);
        for (l, (pts, tris)) in levels.iter_mut().zip(ls) {
            let (s, ts) = (l.pts.len() as u32, l.tris.len() as u32);
            l.tris.extend(tris.iter().map(|t| t + s));
            l.pts.extend(pts);
            l.ranges.push(s..l.pts.len() as u32);
            l.tri_ranges.push(ts..l.tris.len() as u32);
        }
    }
    Built { key, tols, bboxes, origins, piece_of, levels, points, ms: t0.elapsed().as_secs_f64() * 1000.0 }
}

/// A colour as the shader reads it (RGBA bytes, red in the low byte).
pub fn pack(c: egui::Color32) -> u32 {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    u32::from_le_bytes([r, g, b, a])
}

/// The runs of consecutive pieces in `view`, as ranges of `ranges` (each piece's).
fn runs(bboxes: &[[f64; 4]], view: [f64; 4], ranges: &[Range<u32>]) -> Vec<Range<u32>> {
    let mut out: Vec<Range<u32>> = Vec::new();
    let mut last = usize::MAX;
    for (k, b) in bboxes.iter().enumerate() {
        if b[2] < view[0] || b[0] > view[2] || b[3] < view[1] || b[1] > view[3] {
            continue;
        }
        let r = ranges[k].clone();
        match out.last_mut() {
            Some(o) if last + 1 == k => o.end = r.end,
            _ => out.push(r),
        }
        last = k;
    }
    out
}

impl VectorGpu {
    /// The built layer on the GPU, its features' colours `colors` (`sig` their signature).
    pub fn upload(gpu: &Gpu, b: Built, colors: &[u32], sig: u64) -> VectorGpu {
        let features = gpu.create_vector_features(colors, &b.piece_of, &b.origins);
        let mut bytes = features.bytes;
        let level_points = b.levels.iter().map(|l| l.pts.len()).collect();
        let levels = b
            .levels
            .into_iter()
            .map(|l| {
                let g = gpu.upload_vector_geometry(&l.pts, &l.tris, &features);
                bytes += g.bytes;
                (g, l.ranges, l.tri_ranges)
            })
            .collect();
        let points = (!b.points.pts.is_empty()).then(|| {
            let g = gpu.upload_vector_geometry(&b.points.pts, &[], &features);
            bytes += g.bytes;
            (g, b.points.ranges)
        });
        VectorGpu {
            key: b.key,
            tols: b.tols,
            bboxes: b.bboxes,
            levels,
            points,
            features,
            colors_hash: sig,
            bytes,
            level_points,
            build_ms: b.ms,
        }
    }

    /// The features' colours again, when they changed (`sig` their signature: labels, a class column).
    pub fn set_colors(&mut self, gpu: &Gpu, colors: &[u32], sig: u64) -> bool {
        if sig == self.colors_hash {
            return false;
        }
        gpu.write_vector_colors(&self.features, colors);
        self.colors_hash = sig;
        true
    }

    /// The layer drawn: `view` where (target pixel = a * (source pixel - centre) + t0), `seen` the source pixels in
    /// view; `ppp` target pixels per screen point; `fill` its fills' opacity (0: none). The level drawn.
    pub fn draw(&self, gpu: &Gpu, f: &mut MapFrame, view: &VectorView, seen: [f64; 4], ppp: f32, fill: f32) -> usize {
        // the coarsest level within half a target pixel
        let allowed = 0.5 / view.a[0].abs().max(1e-12);
        let level = self.tols.iter().rposition(|&t| t <= allowed).unwrap_or(0);
        let dark = [0.0, 0.0, 0.0, 170.0 / 255.0];
        if let Some((g, ranges, tri_ranges)) = self.levels.get(level) {
            if fill > 0.0 {
                let style = VectorStyle { width: 0.0, halo: 0.0, halo_color: dark, alpha: fill };
                gpu.draw_vector(f, g, VectorKind::Fill, runs(&self.bboxes, seen, tri_ranges), view, &style);
            }
            let style = VectorStyle { width: ppp, halo: ppp, halo_color: dark, alpha: 1.0 };
            gpu.draw_vector(f, g, VectorKind::Lines, runs(&self.bboxes, seen, ranges), view, &style);
        }
        if let Some((g, ranges)) = &self.points {
            let style = VectorStyle { width: 8.0 * ppp, halo: 1.5 * ppp, halo_color: dark, alpha: 1.0 };
            gpu.draw_vector(f, g, VectorKind::Points { round: true }, runs(&self.bboxes, seen, ranges), view, &style);
        }
        level
    }
}

/// A build under way: the layer's placement it is for and its result.
pub struct Job {
    pub key: u64,
    pub rx: Receiver<Built>,
}

/// Starts building a layer for the GPU on a thread.
pub fn start(px: Arc<Vec<Polygon>>, kinds: Vec<Kind>, fill: bool, key: u64, wake: impl Fn() + Send + 'static) -> Job {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("vector-gpu".into())
        .spawn(move || {
            let _ = tx.send(build(&px, &kinds, fill, key));
            wake();
        })
        .ok();
    Job { key, rx }
}

/// A vector layer's GPU side: its geometry being built, or built for a placement.
#[derive(Default)]
pub struct LayerGpu {
    pub job: Option<Job>,
    pub ready: Option<VectorGpu>,
    /// its colours' signature, and the packed colours for it
    colors_sig: u64,
    colors: Vec<u32>,
}

impl LayerGpu {
    /// Whether the GPU draws this placement of the layer (the CPU then draws only its highlighted feature).
    pub fn draws(&self, placed_for: u64) -> bool {
        self.ready.as_ref().is_some_and(|g| g.key == placed_for)
    }
}

/// A cheap signature of a layer's colours.
fn colors_sig(c: egui::Color32, per: &[egui::Color32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64 ^ u64::from(pack(c));
    for x in per {
        h = (h ^ u64::from(pack(*x))).wrapping_mul(0x0100_0000_01b3);
    }
    h ^ per.len() as u64
}

impl crate::app::State {
    /// Each placed vector layer built for the GPU (on a thread), uploaded once built; its colours kept up to date.
    pub(crate) fn pump_vector_gpu(&mut self, ctx: &egui::Context) {
        let Some(gpu) = self.gpu.as_ref() else { return };
        for v in &mut self.vectors {
            let key = v.placed_for;
            if v.px.is_empty() || key == 0 {
                v.gpu = LayerGpu::default();
                continue;
            }
            let sig = colors_sig(v.color, &v.colors);
            if sig != v.gpu.colors_sig || v.gpu.colors.len() != v.data.features.len() {
                v.gpu.colors =
                    (0..v.data.features.len()).map(|i| pack(v.colors.get(i).copied().unwrap_or(v.color))).collect();
                v.gpu.colors_sig = sig;
            }
            if let Some(job) = &v.gpu.job {
                match job.rx.try_recv() {
                    Ok(b) => {
                        v.gpu.job = None;
                        if b.key == key {
                            v.gpu.ready = Some(VectorGpu::upload(gpu, b, &v.gpu.colors, sig));
                            self.map.dirty = true;
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(_) => v.gpu.job = None,
                }
            }
            if !v.gpu.draws(key) && v.gpu.job.as_ref().is_none_or(|j| j.key != key) {
                let kinds = v.data.features.iter().map(|f| f.kind).collect();
                let c = ctx.clone();
                v.gpu.job = Some(start(v.px.clone(), kinds, false, key, move || c.request_repaint()));
            }
            if let Some(g) = v.gpu.ready.as_mut()
                && g.set_colors(gpu, &v.gpu.colors, sig)
            {
                self.map.dirty = true;
            }
        }
    }

    /// What the GPU draws of the vector layers, as a signature (the map is drawn again when it changes).
    pub(crate) fn vector_gpu_sig(&self) -> u64 {
        let mut h = 0u64;
        for v in &self.vectors {
            let on = v.visible && v.gpu.draws(v.placed_for);
            h = h.wrapping_mul(31).wrapping_add(u64::from(v.id) * 2 + u64::from(on));
            if let Some(g) = v.gpu.ready.as_ref().filter(|_| on) {
                h = h.wrapping_mul(31).wrapping_add(g.key ^ g.colors_hash);
            }
        }
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Douglas-Peucker at one tolerance, directly (the ends kept).
    fn dp(p: &[[f64; 2]], tol: f64) -> Vec<[f64; 2]> {
        let mut keep = vec![false; p.len()];
        keep[0] = true;
        keep[p.len() - 1] = true;
        let mut stack = vec![(0usize, p.len() - 1)];
        while let Some((a, b)) = stack.pop() {
            if b <= a + 1 {
                continue;
            }
            let (dx, dy) = (p[b][0] - p[a][0], p[b][1] - p[a][1]);
            let len2 = dx * dx + dy * dy;
            let (mut best, mut at) = (-1.0, a);
            for (i, q) in p.iter().enumerate().take(b).skip(a + 1) {
                let (qx, qy) = (q[0] - p[a][0], q[1] - p[a][1]);
                let d2 = if len2 > 0.0 { (qx * dy - qy * dx).powi(2) / len2 } else { qx * qx + qy * qy };
                if d2 > best {
                    (best, at) = (d2, i);
                }
            }
            if best > tol * tol {
                keep[at] = true;
                stack.push((a, at));
                stack.push((at, b));
            }
        }
        p.iter().zip(&keep).filter(|(_, k)| **k).map(|(q, _)| *q).collect()
    }

    #[test]
    fn every_tolerance_at_once_is_douglas_peucker() {
        // a random walk (a xorshift's steps), open
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        };
        let (mut x, mut y, mut r) = (0.0, 0.0, Vec::new());
        for _ in 0..3000 {
            (x, y) = (x + rnd(), y + rnd());
            r.extend([x, y]);
        }
        let pts: Vec<[f64; 2]> = r.as_chunks::<2>().0.to_vec();
        let s = Simplified::new(&r, false);
        for tol in [1.0 / 32.0, 0.1, 0.5, 2.0, 8.0] {
            let mut got = Vec::new();
            s.at(tol, &mut |q| got.push(q));
            assert_eq!(got, dp(&pts, tol), "tolerance {tol}");
        }
        let mut all = Vec::new();
        s.at(0.0, &mut |q| all.push(q));
        assert_eq!(all, pts);
    }
}
