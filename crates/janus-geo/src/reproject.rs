//! Reprojection (src/reproject.cpp): a layer whose CRS differs from the active layer's, or whose grid is rotated,
//! placed on the map (the active layer's pixel grid). Exact on the CPU through OGR/PROJ (cursor, pins, ROI, tile
//! placement) and, for drawing, a grid of the layer's coordinates at nodes of map space that the display shader
//! interpolates bilinearly (janus_gpu::Warp).

use std::sync::Mutex;
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use janus_cube::CubeInfo;

/// Inverse of a geotransform (CRS -> pixel); None if it is singular.
pub(crate) fn invert(g: &[f64; 6]) -> Option<[f64; 6]> {
    let det = g[1] * g[5] - g[2] * g[4];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let mut i = [0.0; 6];
    i[1] = g[5] / det;
    i[2] = -g[2] / det;
    i[4] = -g[4] / det;
    i[5] = g[1] / det;
    i[0] = -(i[1] * g[0] + i[2] * g[3]);
    i[3] = -(i[4] * g[0] + i[5] * g[3]);
    Some(i)
}

fn apply(g: &[f64; 6], x: &mut f64, y: &mut f64) {
    let (gx, gy) = (g[0] + *x * g[1] + *y * g[2], g[3] + *x * g[4] + *y * g[5]);
    *x = gx;
    *y = gy;
}

/// Points outside a projection's area of use fail one by one: expected, not logged.
pub(crate) struct Quiet;

impl Quiet {
    pub(crate) fn new() -> Self {
        unsafe { gdal_sys::CPLPushErrorHandler(Some(gdal_sys::CPLQuietErrorHandler)) };
        Quiet
    }
}

impl Drop for Quiet {
    fn drop(&mut self) {
        unsafe { gdal_sys::CPLPopErrorHandler() };
    }
}

/// `per_edge` points per edge around the rectangle, clockwise from (x0, y0).
fn edge_points(x0: f64, y0: f64, x1: f64, y1: f64, per_edge: usize) -> (Vec<f64>, Vec<f64>) {
    let (cx, cy) = ([x0, x1, x1, x0, x0], [y0, y0, y1, y1, y0]);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for e in 0..4 {
        for i in 0..per_edge {
            let f = i as f64 / per_edge as f64;
            xs.push(cx[e] + f * (cx[e + 1] - cx[e]));
            ys.push(cy[e] + f * (cy[e + 1] - cy[e]));
        }
    }
    (xs, ys)
}

/// An OGR transformation, used by one thread at a time.
pub(crate) struct Transform(pub(crate) Mutex<CoordTransform>);

// OGR's transformations are not thread-safe; the mutex makes each use exclusive
unsafe impl Send for Transform {}
unsafe impl Sync for Transform {}

impl Transform {
    /// Points in place; ok[i] = false where it failed.
    pub(crate) fn run(&self, x: &mut [f64], y: &mut [f64], ok: &mut [i32]) {
        let ct = self.0.lock().unwrap();
        let _q = Quiet::new();
        unsafe {
            gdal_sys::OCTTransformEx(
                ct.to_c_hct(),
                x.len() as i32,
                x.as_mut_ptr(),
                y.as_mut_ptr(),
                std::ptr::null_mut(),
                ok.as_mut_ptr(),
            );
        }
    }
}

pub struct Reprojection {
    /// the CubeInfo ids it was made for
    pub active_id: u64,
    pub layer_id: u64,
    /// only the grids differ (rotation): an affine map
    pub same_crs: bool,
    /// the layer's CRS as shown (e.g. "EPSG:32722")
    pub crs_text: String,
    /// GPU grid: nodes over `domain` (map-space rectangle x0, y0, x1, y1; empty: nothing to draw), row by row, each
    /// the layer's uv (pixel / size, 0..1 over the layer; NaN where the transformation fails)
    pub domain: [f64; 4],
    pub grid_w: usize,
    pub grid_h: usize,
    /// grid_w * grid_h * 2
    pub grid: Vec<f32>,
    /// max |grid - exact| at the cells' centres, in the layer's pixels
    pub grid_error: f64,
    pub build_ms: f64,
    /// geotransforms (pixel -> CRS) and their inverses
    ga: [f64; 6],
    gb: [f64; 6],
    ia: [f64; 6],
    ib: [f64; 6],
    /// the layer's size
    lw: usize,
    lh: usize,
    /// active CRS -> layer CRS and back (None: same CRS)
    fwd: Option<Transform>,
    inv: Option<Transform>,
}

/// A CRS from WKT with x = easting / longitude.
pub(crate) fn srs(wkt: &str) -> Option<SpatialRef> {
    let mut s = SpatialRef::from_wkt(wkt).ok()?;
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Some(s)
}

impl Reprojection {
    /// Err(why) if the two cannot be related (no georeferencing, unknown CRS, no transformation between the CRSs).
    /// The GPU grid covers the active layer and `margin` × its longer side around it (0.6 for layers).
    pub fn new(a: &CubeInfo, b: &CubeInfo, margin: f64) -> Result<Self, String> {
        let (Some(ga), Some(gb)) = (a.geo_transform, b.geo_transform) else {
            return Err("no georeferencing".into());
        };
        let (Some(ia), Some(ib)) = (invert(&ga), invert(&gb)) else {
            return Err("invalid geotransform".into());
        };
        let mut r = Reprojection {
            active_id: a.id,
            layer_id: b.id,
            same_crs: false,
            crs_text: if b.crs_authority.is_empty() { b.crs_name.clone() } else { b.crs_authority.clone() },
            domain: [0.0; 4],
            grid_w: 0,
            grid_h: 0,
            grid: Vec::new(),
            grid_error: 0.0,
            build_ms: 0.0,
            ga,
            gb,
            ia,
            ib,
            lw: b.width,
            lh: b.height,
            fwd: None,
            inv: None,
        };
        if a.crs_wkt.is_empty() && b.crs_wkt.is_empty() {
            r.same_crs = true; // neither has a CRS: their coordinates are taken as the same
        } else if a.crs_wkt.is_empty() || b.crs_wkt.is_empty() {
            return Err("unknown CRS".into());
        } else {
            let (Some(sa), Some(sb)) = (srs(&a.crs_wkt), srs(&b.crs_wkt)) else {
                return Err("CRS not understood".into());
            };
            if sa == sb {
                r.same_crs = true;
            } else {
                let _q = Quiet::new();
                let (Ok(fwd), Ok(inv)) = (CoordTransform::new(&sa, &sb), CoordTransform::new(&sb, &sa)) else {
                    return Err("no transformation between the CRSs".into());
                };
                r.fwd = Some(Transform(Mutex::new(fwd)));
                r.inv = Some(Transform(Mutex::new(inv)));
            }
        }
        r.build_grid(a, margin);
        Ok(r)
    }

    fn convert(&self, x: &mut [f64], y: &mut [f64], g: &[f64; 6], ct: Option<&Transform>, i: &[f64; 6]) -> Vec<bool> {
        for k in 0..x.len() {
            apply(g, &mut x[k], &mut y[k]);
        }
        let mut ok = vec![1i32; x.len()];
        if let Some(ct) = ct
            && !x.is_empty()
        {
            ct.run(x, y, &mut ok);
        }
        (0..x.len())
            .map(|k| {
                let good = ok[k] != 0 && x[k].is_finite() && y[k].is_finite();
                if good {
                    apply(i, &mut x[k], &mut y[k]);
                }
                good
            })
            .collect()
    }

    /// Active layer's pixels (map space) to the layer's pixels, many points in place; false where it failed (e.g.
    /// outside the projection's area of use).
    pub fn to_layer_many(&self, x: &mut [f64], y: &mut [f64]) -> Vec<bool> {
        self.convert(x, y, &self.ga, self.fwd.as_ref(), &self.ib)
    }

    pub fn to_active_many(&self, x: &mut [f64], y: &mut [f64]) -> Vec<bool> {
        self.convert(x, y, &self.gb, self.inv.as_ref(), &self.ia)
    }

    pub fn to_layer(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (mut xs, mut ys) = ([x], [y]);
        self.to_layer_many(&mut xs, &mut ys)[0].then_some((xs[0], ys[0]))
    }

    pub fn to_active(&self, lx: f64, ly: f64) -> Option<(f64, f64)> {
        let (mut xs, mut ys) = ([lx], [ly]);
        self.to_active_many(&mut xs, &mut ys)[0].then_some((xs[0], ys[0]))
    }

    /// Map-space bounding box (x0, y0, x1, y1) of the layer's pixels [x0, x1) × [y0, y1), from points along their
    /// edges. None if none transforms.
    pub fn footprint(&self, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<[f64; 4]> {
        let (mut xs, mut ys) = edge_points(x0, y0, x1, y1, 16);
        let ok = self.to_active_many(&mut xs, &mut ys);
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for k in (0..xs.len()).filter(|&k| ok[k]) {
            b = [b[0].min(xs[k]), b[1].min(ys[k]), b[2].max(xs[k]), b[3].max(ys[k])];
        }
        (b[2] >= b[0]).then_some(b)
    }

    /// The map-space rectangle [x0, x1) × [y0, y1) in the layer's pixels: a polygon through `per_edge` points per
    /// edge (x, y pairs; failed points left out).
    pub fn polygon(&self, x0: f64, y0: f64, x1: f64, y1: f64, per_edge: usize) -> Vec<f64> {
        let (mut xs, mut ys) = edge_points(x0, y0, x1, y1, per_edge);
        let ok = self.to_layer_many(&mut xs, &mut ys);
        (0..xs.len()).filter(|&k| ok[k]).flat_map(|k| [xs[k], ys[k]]).collect()
    }

    /// Layer pixels per map pixel around map point (x, y) (square root of the Jacobian).
    pub fn layer_px_per_map_px(&self, x: f64, y: f64) -> f64 {
        let (mut px, mut py) = ([x, x + 1.0, x], [y, y, y + 1.0]);
        let ok = self.to_layer_many(&mut px, &mut py);
        if !ok.iter().all(|&o| o) {
            return 0.0;
        }
        ((px[1] - px[0]) * (py[2] - py[0]) - (px[2] - px[0]) * (py[1] - py[0])).abs().sqrt()
    }

    pub fn has_domain(&self) -> bool {
        self.grid_w > 1 && self.domain[2] > self.domain[0] && self.domain[3] > self.domain[1]
    }

    /// The layer's pixel at map point (x, y) through the grid, as the shader does.
    pub fn grid_at(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (u, v) = self.grid_uv(x, y)?;
        let (lx, ly) = (u * self.lw as f64, v * self.lh as f64);
        (lx.is_finite() && ly.is_finite()).then_some((lx, ly))
    }

    /// The layer's uv (0..1 over the layer) at map point (x, y), interpolated in the grid as the shader does.
    pub fn grid_uv(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if !self.has_domain() {
            return None;
        }
        let d = &self.domain;
        let gx = (x - d[0]) / (d[2] - d[0]) * (self.grid_w - 1) as f64;
        let gy = (y - d[1]) / (d[3] - d[1]) * (self.grid_h - 1) as f64;
        let i = (gx.floor() as i64).clamp(0, self.grid_w as i64 - 2) as usize;
        let j = (gy.floor() as i64).clamp(0, self.grid_h as i64 - 2) as usize;
        let (fx, fy) = (gx - i as f64, gy - j as f64);
        let at = |ii: usize, jj: usize, c: usize| f64::from(self.grid[(jj * self.grid_w + ii) * 2 + c]);
        let lerp = |c: usize| {
            (1.0 - fy) * ((1.0 - fx) * at(i, j, c) + fx * at(i + 1, j, c))
                + fy * ((1.0 - fx) * at(i, j + 1, c) + fx * at(i + 1, j + 1, c))
        };
        Some((lerp(0), lerp(1)))
    }

    /// The grid covers the part of the layer the map can show: the active layer and, around it, what a fitted view
    /// of an elongated image leaves free (`margin` × its longer side). 64 cells along the longer side; twice as
    /// many while the bilinear interpolation, checked at every cell's centre against the exact transformation, is
    /// off by more than 0.05 of the layer's pixels (up to 512).
    fn build_grid(&mut self, a: &CubeInfo, margin: f64) {
        let t0 = Instant::now();
        let m = margin * a.width.max(a.height) as f64;
        let around = [-m, -m, a.width as f64 + m, a.height as f64 + m];
        // The layer's edges on the map, plus the points of a 17 × 17 lattice over that area which fall inside the
        // layer (its edges may not transform at all, e.g. a continent in geographic coordinates over a UTM scene).
        let (lw, lh) = (self.lw as f64, self.lh as f64);
        let mut b = self.footprint(0.0, 0.0, lw, lh).unwrap_or([
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ]);
        const LATTICE: usize = 17;
        let (mut mx, mut my) = (Vec::new(), Vec::new());
        for j in 0..LATTICE {
            for i in 0..LATTICE {
                mx.push(around[0] + (around[2] - around[0]) * i as f64 / (LATTICE - 1) as f64);
                my.push(around[1] + (around[3] - around[1]) * j as f64 / (LATTICE - 1) as f64);
            }
        }
        let (mut xs, mut ys) = (mx.clone(), my.clone());
        let ok = self.to_layer_many(&mut xs, &mut ys);
        let cw = (around[2] - around[0]) / (LATTICE - 1) as f64;
        let ch = (around[3] - around[1]) / (LATTICE - 1) as f64;
        for k in 0..xs.len() {
            let inside = mx[k] >= b[0] && mx[k] <= b[2] && my[k] >= b[1] && my[k] <= b[3];
            if ok[k] && xs[k] >= 0.0 && ys[k] >= 0.0 && xs[k] <= lw && ys[k] <= lh && !inside {
                // on the layer, but missed by its edges: with the lattice's cell around it
                b = [b[0].min(mx[k] - cw), b[1].min(my[k] - ch), b[2].max(mx[k] + cw), b[3].max(my[k] + ch)];
            }
        }
        self.domain = [b[0].max(around[0]), b[1].max(around[1]), b[2].min(around[2]), b[3].min(around[3])];
        self.grid_w = 0;
        self.grid_h = 0;
        self.grid.clear();
        let d = self.domain;
        if !(d[2] > d[0] && d[3] > d[1]) {
            return;
        }
        let (dw, dh) = (d[2] - d[0], d[3] - d[1]);
        let mut cells = 64;
        loop {
            let cell = dw.max(dh) / cells as f64;
            let gw = 2.max((dw / cell - 1e-9).ceil() as usize + 1);
            let gh = 2.max((dh / cell - 1e-9).ceil() as usize + 1);
            let n = gw * gh;
            let (mut xs, mut ys) = (vec![0.0; n], vec![0.0; n]);
            for j in 0..gh {
                for i in 0..gw {
                    xs[j * gw + i] = d[0] + dw * i as f64 / (gw - 1) as f64;
                    ys[j * gw + i] = d[1] + dh * j as f64 / (gh - 1) as f64;
                }
            }
            let ok = self.to_layer_many(&mut xs, &mut ys);
            self.grid_w = gw;
            self.grid_h = gh;
            self.grid = (0..n)
                .flat_map(|k| if ok[k] { [(xs[k] / lw) as f32, (ys[k] / lh) as f32] } else { [f32::NAN, f32::NAN] })
                .collect();
            // error at the cells' centres
            let nc = (gw - 1) * (gh - 1);
            let (mut cx, mut cy) = (vec![0.0; nc], vec![0.0; nc]);
            for j in 0..gh - 1 {
                for i in 0..gw - 1 {
                    cx[j * (gw - 1) + i] = d[0] + dw * (i as f64 + 0.5) / (gw - 1) as f64;
                    cy[j * (gw - 1) + i] = d[1] + dh * (j as f64 + 0.5) / (gh - 1) as f64;
                }
            }
            let (mut ex, mut ey) = (cx.clone(), cy.clone());
            let ok = self.to_layer_many(&mut ex, &mut ey);
            self.grid_error = 0.0;
            for k in 0..nc {
                if ok[k]
                    && let Some((gx, gy)) = self.grid_at(cx[k], cy[k])
                {
                    self.grid_error = self.grid_error.max((gx - ex[k]).hypot(gy - ey[k]));
                }
            }
            if self.grid_error <= 0.05 || cells >= 512 {
                break;
            }
            cells *= 2;
        }
        self.build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    }
}
