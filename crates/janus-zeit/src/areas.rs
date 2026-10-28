//! The area of each class at each date, or of a change map's years (IDEIAS phase 42): pixels counted at full
//! resolution, each with its true area. In an equal-area projection that is the geotransform's (constant); in
//! longitudes and latitudes or another projection (UTM: its scale factor) each pixel's geodesic area on the CRS's
//! ellipsoid, computed on a grid of at most 129 x 129 pixels across the window (their corners transformed at once,
//! measured by geographiclib) and interpolated between them (the area varies slowly: an error under 1e-5).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Dataset;
use gdal::spatial_ref::SpatialRef;

/// How a pixel's area is known.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AreaKind {
    /// an equal-area projection: the geotransform's, the same for every pixel
    EqualArea,
    /// longitudes and latitudes: geodesic, by latitude
    Geographic,
    /// another projection: geodesic (its scale factor varies)
    Projected,
    /// no CRS: the geotransform's units squared
    Planar,
}

/// The projection methods (WKT1 names) whose map area is the ellipsoid's.
const EQUAL_AREA: [&str; 9] = [
    "equal_area",
    "albers",
    "mollweide",
    "sinusoidal",
    "eckert_iv",
    "eckert_vi",
    "homolosine",
    "equal_earth",
    "quartic_authalic",
];

/// The area of each pixel of a window.
pub struct PixelAreas {
    pub kind: AreaKind,
    /// the geotransform's pixel area in m² (in degrees² for longitudes and latitudes)
    pub nominal: f64,
    /// the true area at the window's centre, and the least and largest across it (m²)
    pub centre: f64,
    pub range: (f64, f64),
    /// the samples: at columns xs and rows ys of the window, ys.len() rows of xs.len()
    xs: Vec<usize>,
    ys: Vec<usize>,
    grid: Vec<f64>,
}

/// The samples' columns (or rows): every `step` pixels, the last one included.
fn nodes(n: usize, step: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..n).step_by(step.max(1)).collect();
    if *v.last().unwrap_or(&0) != n.saturating_sub(1) {
        v.push(n - 1);
    }
    v
}

/// The interpolation between the samples `ns` at `x`: the sample before and the weight of the one after.
fn between(ns: &[usize], x: usize) -> (usize, f64) {
    if ns.len() < 2 {
        return (0, 0.0);
    }
    let i = ns.partition_point(|&n| n <= x).saturating_sub(1).min(ns.len() - 2);
    (i, (x as f64 - ns[i] as f64) / (ns[i + 1] - ns[i]) as f64)
}

impl PixelAreas {
    /// The areas of the pixels of window (x0, y0, w, h) of a grid (its geotransform and CRS).
    pub fn new(gt: &[f64; 6], wkt: &str, x0: usize, y0: usize, w: usize, h: usize) -> PixelAreas {
        let det = (gt[1] * gt[5] - gt[2] * gt[4]).abs();
        let flat = |kind, a: f64| PixelAreas {
            kind,
            nominal: a,
            centre: a,
            range: (a, a),
            xs: vec![0],
            ys: vec![0],
            grid: vec![a],
        };
        let Some(srs) = (!wkt.is_empty()).then(|| SpatialRef::from_wkt(wkt).ok()).flatten() else {
            return flat(AreaKind::Planar, det);
        };
        let geographic = srs.is_geographic();
        if !geographic {
            let units = srs.linear_units();
            let method = srs.get_attr_value("PROJECTION", 0).ok().flatten().unwrap_or_default().to_lowercase();
            if EQUAL_AREA.iter().any(|m| method.contains(m)) {
                return flat(AreaKind::EqualArea, det * units * units);
            }
        }
        let Some(geod) = crate::polygonize::Geodesic::of(&srs) else {
            return flat(AreaKind::Planar, det);
        };
        // at most 129 samples a side
        let step = w.max(h).div_ceil(128).max(1);
        let (xs, ys) = (nodes(w, step), nodes(h, step));
        // the corners of each sampled pixel, transformed at once
        let mut px = Vec::with_capacity(xs.len() * ys.len() * 4);
        let mut py = Vec::with_capacity(px.capacity());
        for &y in &ys {
            for &x in &xs {
                for (dx, dy) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                    let (c, r) = ((x0 + x) as f64 + dx, (y0 + y) as f64 + dy);
                    px.push(gt[0] + c * gt[1] + r * gt[2]);
                    py.push(gt[3] + c * gt[4] + r * gt[5]);
                }
            }
        }
        let grid = match geod.lon_lat(&mut px, &mut py) {
            true => px.chunks(4).zip(py.chunks(4)).map(|(x, y)| geod.ring_area(x, y)).collect::<Vec<_>>(),
            false => return flat(AreaKind::Planar, det),
        };
        let (lo, hi) = grid.iter().fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
        let mut p = PixelAreas {
            kind: if geographic { AreaKind::Geographic } else { AreaKind::Projected },
            nominal: if geographic { det } else { det * srs.linear_units().powi(2) },
            centre: 0.0,
            range: (lo, hi),
            xs,
            ys,
            grid,
        };
        let mut row = vec![0.0; w];
        p.row(h / 2, &mut row);
        p.centre = row.get(w / 2).copied().unwrap_or(lo);
        p
    }

    /// The areas (m²) of row `y` of the window's pixels.
    pub fn row(&self, y: usize, out: &mut [f64]) {
        if self.grid.len() == 1 {
            out.fill(self.grid[0]);
            return;
        }
        let nx = self.xs.len();
        let (j, ty) = between(&self.ys, y);
        let at = |i: usize| {
            let a = self.grid[j * nx + i];
            if ty > 0.0 { a + (self.grid[(j + 1) * nx + i] - a) * ty } else { a }
        };
        let col: Vec<f64> = (0..nx).map(at).collect();
        for (x, o) in out.iter_mut().enumerate() {
            let (i, tx) = between(&self.xs, x);
            *o = if tx > 0.0 { col[i] + (col[i + 1] - col[i]) * tx } else { col[i] };
        }
    }

    /// What the areas are, for the table's notes.
    pub fn describe(&self) -> String {
        let off = |a: f64| 100.0 * (a / self.nominal - 1.0);
        match self.kind {
            AreaKind::EqualArea => format!("an equal-area projection: every pixel {:.6} m²", self.nominal),
            AreaKind::Planar => format!("no CRS: every pixel {} square units of the grid", self.nominal),
            AreaKind::Geographic => format!(
                "longitudes and latitudes: each pixel's geodesic area, {:.2} to {:.2} m² ({:.2} at the centre)",
                self.range.0, self.range.1, self.centre
            ),
            AreaKind::Projected => format!(
                "each pixel's geodesic area, {:.3} to {:.3} m²: the nominal {:.3} m² off by {:+.3}% to {:+.3}% \
                 (the projection's scale)",
                self.range.0,
                self.range.1,
                self.nominal,
                off(self.range.0),
                off(self.range.1)
            ),
        }
    }
}

/// A map to count: a file's band (a date of a series, a run's map).
#[derive(Clone, Debug)]
pub struct AreaSource {
    pub path: String,
    pub band: usize,
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct AreaOptions {
    /// (x0, y0, x1, y1) in the maps' pixels (None: all of them)
    pub window: Option<[usize; 4]>,
    /// the values' whole part (a map of decimal years by year), else rounded
    pub floor: bool,
    pub threads: usize,
}

/// Each source's pixels and area (m²) of every value.
pub struct AreaTable {
    pub labels: Vec<String>,
    /// the values found, ascending
    pub values: Vec<i64>,
    /// [source][value]
    pub pixels: Vec<Vec<u64>>,
    pub m2: Vec<Vec<f64>>,
    pub kind: AreaKind,
    pub note: String,
    /// the window counted (x0, y0, x1, y1)
    pub window: [usize; 4],
}

impl AreaTable {
    /// A source's area with a value (m²).
    pub fn total(&self, s: usize) -> f64 {
        self.m2[s].iter().sum()
    }

    /// The table as CSV: a row per source and value (its pixels, m², ha, km² and share).
    pub fn csv(&self, names: &dyn Fn(i64) -> String) -> String {
        let mut out = String::from("date,value,class,pixels,area_m2,area_ha,area_km2,percent\n");
        for (s, label) in self.labels.iter().enumerate() {
            let total = self.total(s);
            for (k, &v) in self.values.iter().enumerate() {
                let a = self.m2[s][k];
                let name = names(v).replace('"', "'");
                out += &format!(
                    "{label},{v},\"{name}\",{},{a:.3},{:.6},{:.8},{:.6}\n",
                    self.pixels[s][k],
                    a / 1e4,
                    a / 1e6,
                    if total > 0.0 { 100.0 * a / total } else { 0.0 }
                );
            }
        }
        out
    }
}

/// Values counted in arrays (0 to this - 1).
const DENSE: usize = 65_536;

/// The value counted for a pixel.
fn key(v: f64, floor: bool) -> i64 {
    if floor { v.floor() as i64 } else { v.round() as i64 }
}

/// Counts every source's values in the window, its pixels' areas added; the sources on one grid.
pub fn areas(
    sources: &[AreaSource],
    opts: &AreaOptions,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<AreaTable, String> {
    let first = sources.first().ok_or("no map to count")?;
    let ds = Dataset::open(&first.path).map_err(|e| format!("{}: {e}", first.path))?;
    let (gw, gh) = ds.raster_size();
    let gt = ds.geo_transform().unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let [x0, y0, x1, y1] = opts.window.unwrap_or([0, 0, gw, gh]);
    if x1 <= x0 || y1 <= y0 || x1 > gw || y1 > gh {
        return Err(format!("the window {x0}, {y0} to {x1}, {y1} is not in the map's {gw} x {gh}"));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    progress(0.0, "the pixels' areas");
    let pa = PixelAreas::new(&gt, &ds.projection(), x0, y0, w, h);
    drop(ds);
    // blocks of rows of every source, taken by the threads in turn
    let rows = (8_000_000 / w).clamp(16, 1024).min(h);
    let blocks: Vec<(usize, usize)> =
        (0..sources.len()).flat_map(|s| (0..h).step_by(rows).map(move |y| (s, y))).collect();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    type Counts = HashMap<i64, (u64, f64)>;
    let results: Vec<Mutex<Option<Counts>>> = blocks.iter().map(|_| Mutex::new(None)).collect();
    let error: Mutex<Option<String>> = Mutex::new(None);
    std::thread::scope(|sc| {
        for _ in 0..opts.threads.clamp(1, blocks.len().max(1)) {
            sc.spawn(|| {
                let mut open: HashMap<usize, Dataset> = HashMap::new();
                let (mut v, mut area, mut dense) = (Vec::new(), vec![0.0; w], Vec::new());
                loop {
                    let b = next.fetch_add(1, Ordering::Relaxed);
                    if b >= blocks.len() || cancel.load(Ordering::Relaxed) || error.lock().unwrap().is_some() {
                        return;
                    }
                    let (s, by) = blocks[b];
                    let n = rows.min(h - by);
                    let src = &sources[s];
                    let r = (|| -> Result<Counts, String> {
                        let d = match open.entry(s) {
                            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                            std::collections::hash_map::Entry::Vacant(e) => {
                                let d = Dataset::open(&src.path).map_err(|e| format!("{}: {e}", src.path))?;
                                if d.raster_size() != (gw, gh) {
                                    return Err(format!("{} is not the size of {}", src.path, first.path));
                                }
                                e.insert(d)
                            }
                        };
                        let band = d.rasterband(src.band.max(1)).map_err(|e| format!("{}: {e}", src.path))?;
                        let nd = band.no_data_value().filter(|x| !x.is_nan());
                        v = band
                            .read_as::<f64>(((x0) as isize, (y0 + by) as isize), (w, n), (w, n), None)
                            .map_err(|e| format!("{}: {e}", src.path))?
                            .into_shape_and_vec()
                            .1;
                        // values 0 to 65535 (classes, years) in arrays, the others in a map
                        dense.resize(DENSE, (0, 0.0));
                        let mut c: Counts = HashMap::new();
                        for (r, row) in v.chunks(w).enumerate() {
                            pa.row(by + r, &mut area);
                            for (&x, &a) in row.iter().zip(&area) {
                                if x.is_nan() || nd == Some(x) {
                                    continue;
                                }
                                let k = key(x, opts.floor);
                                let e = match usize::try_from(k).ok().filter(|&k| k < DENSE) {
                                    Some(k) => &mut dense[k],
                                    None => c.entry(k).or_insert((0, 0.0)),
                                };
                                (e.0, e.1) = (e.0 + 1, e.1 + a);
                            }
                        }
                        for (k, e) in dense.iter_mut().enumerate().filter(|(_, e)| e.0 > 0) {
                            c.insert(k as i64, std::mem::take(e));
                        }
                        Ok(c)
                    })();
                    match r {
                        Ok(c) => *results[b].lock().unwrap() = Some(c),
                        Err(e) => {
                            error.lock().unwrap().get_or_insert(e);
                            return;
                        }
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(d as f64 / blocks.len() as f64, &format!("{} of {} blocks of rows", d, blocks.len()));
                }
            });
        }
    });
    if let Some(e) = error.into_inner().unwrap() {
        return Err(e);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    // the blocks added in their order (the same sums every run)
    let mut per: Vec<Counts> = vec![HashMap::new(); sources.len()];
    for (b, r) in blocks.iter().zip(results) {
        for (k, (p, a)) in r.into_inner().unwrap().unwrap_or_default() {
            let e = per[b.0].entry(k).or_insert((0, 0.0));
            (e.0, e.1) = (e.0 + p, e.1 + a);
        }
    }
    let mut values: Vec<i64> = per.iter().flat_map(|c| c.keys().copied()).collect();
    values.sort_unstable();
    values.dedup();
    let get = |c: &Counts, v: i64| c.get(&v).copied().unwrap_or((0, 0.0));
    Ok(AreaTable {
        labels: sources.iter().map(|s| s.label.clone()).collect(),
        pixels: per.iter().map(|c| values.iter().map(|&v| get(c, v).0).collect()).collect(),
        m2: per.iter().map(|c| values.iter().map(|&v| get(c, v).1).collect()).collect(),
        values,
        kind: pa.kind,
        note: pa.describe(),
        window: [x0, y0, x1, y1],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polygonize::Geodesic;

    /// The geodesic area of the rectangle of a window's corners in the CRS, its sides cut in 2000 (so a side along
    /// a parallel is followed, not the geodesic between its ends).
    fn rectangle(gt: &[f64; 6], wkt: &str, w: usize, h: usize) -> f64 {
        let srs = SpatialRef::from_wkt(wkt).unwrap();
        let g = Geodesic::of(&srs).unwrap();
        let (mut x, mut y) = (vec![], vec![]);
        let n = 2000;
        let corners = [(0.0, 0.0), (w as f64, 0.0), (w as f64, h as f64), (0.0, h as f64), (0.0, 0.0)];
        for c in corners.windows(2) {
            for k in 0..n {
                let t = k as f64 / n as f64;
                let (cx, cy) = (c[0].0 + (c[1].0 - c[0].0) * t, c[0].1 + (c[1].1 - c[0].1) * t);
                x.push(gt[0] + cx * gt[1] + cy * gt[2]);
                y.push(gt[3] + cx * gt[4] + cy * gt[5]);
            }
        }
        assert!(g.lon_lat(&mut x, &mut y));
        g.ring_area(&x, &y)
    }

    fn sum(p: &PixelAreas, w: usize, h: usize) -> f64 {
        let mut row = vec![0.0; w];
        (0..h)
            .map(|y| {
                p.row(y, &mut row);
                row.iter().sum::<f64>()
            })
            .sum()
    }

    fn wkt(epsg: u32) -> String {
        SpatialRef::from_epsg(epsg).unwrap().to_wkt().unwrap()
    }

    /// A window's pixels add up to the rectangle's geodesic area, in longitudes and latitudes and in UTM.
    #[test]
    fn rectangles() {
        for (epsg, gt, w, h) in [
            (4326, [-45.0, 0.01, 0.0, -10.0, 0.0, -0.01], 100, 100),
            (4326, [10.0, 0.00025, 0.0, 60.0, 0.0, -0.00025], 4000, 3000),
            (32723, [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0], 1000, 1000),
            (32723, [300000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0], 3000, 2000),
        ] {
            let wk = wkt(epsg);
            let p = PixelAreas::new(&gt, &wk, 0, 0, w, h);
            let (got, want) = (sum(&p, w, h), rectangle(&gt, &wk, w, h));
            assert!((got / want - 1.0).abs() < 2e-6, "{epsg} {gt:?}: {got} against {want} ({})", p.describe());
        }
        // UTM 110 to 200 km west of its central meridian: the nominal 900 m² off by its scale, both ways
        let p = PixelAreas::new(&[300000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0], &wkt(32723), 0, 0, 3000, 2000);
        assert_eq!(p.kind, AreaKind::Projected);
        assert!(p.range.0 < 900.0 && p.range.1 > 900.4, "{}", p.describe());
    }

    #[test]
    fn equal_area() {
        let p = PixelAreas::new(&[0.0, 1000.0, 0.0, 0.0, 0.0, -1000.0], &wkt(6933), 0, 0, 100, 100);
        assert_eq!(p.kind, AreaKind::EqualArea);
        assert_eq!(p.nominal, 1e6);
    }

    /// Each value's pixels and area, no data and NaN left out; by year (the whole part).
    #[test]
    fn counts() {
        let (w, h) = (50usize, 40usize);
        let gt = [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
        let data: Vec<f32> = (0..w * h)
            .map(|i| match i % 7 {
                0 => -1.0,
                1 => f32::NAN,
                k => 2000.0 + k as f32 + 0.25 * (i % 3) as f32,
            })
            .collect();
        let path = "/vsimem/areas_counts.tif";
        {
            let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, 1).unwrap();
            ds.set_geo_transform(&gt).unwrap();
            ds.set_projection(&wkt(32723)).unwrap();
            let mut b = ds.rasterband(1).unwrap();
            b.set_no_data_value(Some(-1.0)).unwrap();
            b.write((0, 0), (w, h), &mut gdal::raster::Buffer::new((w, h), data.clone())).unwrap();
        }
        let src = AreaSource { path: path.into(), band: 1, label: "2020".into() };
        let none = AtomicBool::new(false);
        for floor in [false, true] {
            let opts = AreaOptions { window: Some([5, 3, 45, 33]), floor, threads: 3 };
            let t = areas(&[src.clone(), src.clone()], &opts, &|_, _| {}, &none).unwrap();
            let p = PixelAreas::new(&gt, &wkt(32723), 5, 3, 40, 30);
            let mut want: std::collections::BTreeMap<i64, (u64, f64)> = Default::default();
            let mut row = vec![0.0; 40];
            for y in 0..30 {
                p.row(y, &mut row);
                for x in 0..40 {
                    let v = data[(y + 3) * w + x + 5];
                    if v.is_nan() || v == -1.0 {
                        continue;
                    }
                    let e = want.entry(key(f64::from(v), floor)).or_default();
                    (e.0, e.1) = (e.0 + 1, e.1 + row[x]);
                }
            }
            assert_eq!(t.values, want.keys().copied().collect::<Vec<_>>());
            for s in 0..2 {
                for (k, (px, a)) in want.values().enumerate() {
                    assert_eq!(t.pixels[s][k], *px);
                    assert!((t.m2[s][k] / a - 1.0).abs() < 1e-12);
                }
            }
        }
    }
}
