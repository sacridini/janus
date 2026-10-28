//! Zonal statistics over time (IDEIAS phase 45): each feature of a vector layer is a zone, its pixels those whose
//! centre is inside (or every pixel touched), burned once on the maps' grid (the features' numbers, rasterize's
//! strips); a pixel in several features counts in each, as in rasterstats and exactextract (the features over it
//! burned one by one on their extent). Then one pass per map (a date of a series, a run's map) on the processing
//! threads: for a continuous series each zone's number of values, mean, standard deviation, least, percentiles 10,
//! 25, 50, 75 and 90 (numpy's, linear) and largest; for classes each class's pixels and area (each pixel's true
//! area, areas::PixelAreas) and the transitions between two dates; for a map of change dates the area by year.

use std::collections::HashMap;
use std::ffi::CString;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Dataset;
use gdal::vector::{Geometry, LayerAccess};

use crate::areas::{AreaSource, PixelAreas};
use crate::rasterize::{burn_strips, to_crs};
use crate::warp::Grid;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ZonalKind {
    /// values: their statistics
    #[default]
    Continuous,
    /// classes: their areas (values rounded)
    Classes,
    /// change dates (decimal years): the area by year (their whole part)
    Years,
}

/// A zone's statistics at a date, in this order (NaN without a value).
pub const STATS: [&str; 10] = ["n", "mean", "std", "min", "p10", "p25", "median", "p75", "p90", "max"];

#[derive(Clone, Debug, Default)]
pub struct ZonalOptions {
    /// the OGR layer (empty: the first)
    pub layer: String,
    /// the attribute naming the zones (empty: their FIDs)
    pub id_field: String,
    pub all_touched: bool,
    pub kind: ZonalKind,
    /// only within (x0, y0, x1, y1) of the grid (None: the whole grid)
    pub window: Option<[usize; 4]>,
    /// classes: the transitions between these two maps (their indexes in the sources)
    pub transitions: Option<(usize, usize)>,
    pub threads: usize,
}

/// A class's or a year's count in a zone: (zone, value, pixels, m²).
pub type Count = (u32, i64, u64, f64);
/// A transition in a zone: (zone, from, to, pixels, m²).
pub type Transition = (u32, i64, i64, u64, f64);

pub struct ZonalTable {
    pub kind: ZonalKind,
    /// the maps' labels (dates)
    pub labels: Vec<String>,
    /// each zone's feature id and pixels on the grid
    pub fids: Vec<i64>,
    /// each zone's name: its id_field, else its FID
    pub ids: Vec<String>,
    pub pixels: Vec<u64>,
    /// continuous: [map][zone] STATS
    pub stats: Vec<Vec<[f64; 10]>>,
    /// classes and years: [map] the counts, by zone and value
    pub counts: Vec<Vec<Count>>,
    /// the values found, ascending
    pub values: Vec<i64>,
    pub transitions: Vec<Transition>,
    /// the pixels' areas
    pub note: String,
    /// the window read (x0, y0, x1, y1) and the pixels in more than one zone
    pub window: [usize; 4],
    pub shared: usize,
}

/// A zone's column name part: letters, digits and _.
fn column(s: &str) -> String {
    let c: String = s.chars().map(|ch| if ch.is_alphanumeric() { ch.to_ascii_lowercase() } else { '_' }).collect();
    c.trim_matches('_').to_string()
}

/// numpy's percentile (linear) of sorted values.
fn percentile(v: &[f32], q: f64) -> f64 {
    let pos = q * (v.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    let (a, b) = (v[lo] as f64, v[hi] as f64);
    a + (b - a) * (pos - lo as f64)
}

/// The statistics of a zone's values (sorted here).
fn statistics(v: &mut [f32]) -> [f64; 10] {
    let mut s = [f64::NAN; 10];
    s[0] = v.len() as f64;
    if v.is_empty() {
        return s;
    }
    v.sort_unstable_by(f32::total_cmp);
    let n = v.len() as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let var = v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n;
    s[1] = mean;
    s[2] = var.sqrt();
    s[3] = v[0] as f64;
    for (k, q) in [0.1, 0.25, 0.5, 0.75, 0.9].iter().enumerate() {
        s[4 + k] = percentile(v, *q);
    }
    s[9] = v[v.len() - 1] as f64;
    s
}

impl ZonalTable {
    /// The table as CSV, a row per zone and date (and class).
    pub fn long_csv(&self, names: &dyn Fn(i64) -> String) -> String {
        let ids = &self.ids;
        let q = |s: &str| format!("\"{}\"", s.replace('"', "'"));
        let id = |z: usize| q(ids.get(z).map_or("", |s| s.as_str()));
        let mut out = String::new();
        match self.kind {
            ZonalKind::Continuous => {
                out += &format!("id,fid,date,{}\n", STATS.join(","));
                for (s, label) in self.labels.iter().enumerate() {
                    for (z, st) in self.stats[s].iter().enumerate() {
                        let cells: Vec<String> =
                            st.iter().map(|x| if x.is_nan() { String::new() } else { format!("{x}") }).collect();
                        out += &format!("{},{},{label},{}\n", id(z), self.fids[z], cells.join(","));
                    }
                }
            }
            _ => {
                let what = if self.kind == ZonalKind::Years { "year" } else { "class" };
                out += &format!("id,fid,date,value,{what},pixels,area_m2,area_ha,percent\n");
                for (s, label) in self.labels.iter().enumerate() {
                    let mut total: HashMap<u32, f64> = HashMap::new();
                    for c in &self.counts[s] {
                        *total.entry(c.0).or_default() += c.3;
                    }
                    for &(z, v, p, a) in &self.counts[s] {
                        let t = total[&z];
                        out += &format!(
                            "{},{},{label},{v},{},{p},{a:.3},{:.6},{:.6}\n",
                            id(z as usize),
                            self.fids[z as usize],
                            q(&names(v)),
                            a / 1e4,
                            if t > 0.0 { 100.0 * a / t } else { 0.0 }
                        );
                    }
                }
            }
        }
        out
    }

    /// The transitions as CSV: a row per zone, class before and class after.
    pub fn transitions_csv(&self, names: &dyn Fn(i64) -> String) -> String {
        let ids = &self.ids;
        let q = |s: &str| format!("\"{}\"", s.replace('"', "'"));
        let mut out = String::from("id,fid,from,from_class,to,to_class,pixels,area_m2,area_ha\n");
        for &(z, a, b, p, m2) in &self.transitions {
            out += &format!(
                "{},{},{a},{},{b},{},{p},{m2:.3},{:.6}\n",
                q(ids.get(z as usize).map_or("", |s| s.as_str())),
                self.fids[z as usize],
                q(&names(a)),
                q(&names(b)),
                m2 / 1e4
            );
        }
        out
    }

    /// A column per date (of the statistic `stat`; of each class's ha; of each year's ha) and a row per zone.
    pub fn wide(&self, stat: usize, names: &dyn Fn(i64) -> String) -> (Vec<String>, Vec<Vec<f64>>) {
        let zones = self.fids.len();
        let mut cols = vec![];
        let mut rows = vec![vec![]; zones];
        match self.kind {
            ZonalKind::Continuous => {
                for (s, label) in self.labels.iter().enumerate() {
                    cols.push(format!("{}_{}", STATS[stat.min(9)], column(label)));
                    for (z, row) in rows.iter_mut().enumerate() {
                        row.push(self.stats[s][z][stat.min(9)]);
                    }
                }
            }
            _ => {
                let one = self.labels.len() == 1;
                for (s, label) in self.labels.iter().enumerate() {
                    for &v in &self.values {
                        let name = if self.kind == ZonalKind::Years { v.to_string() } else { column(&names(v)) };
                        cols.push(if one { format!("ha_{name}") } else { format!("ha_{name}_{}", column(label)) });
                        let at = rows[0].len();
                        for row in rows.iter_mut() {
                            row.push(0.0);
                        }
                        for &(z, cv, _, a) in &self.counts[s] {
                            if cv == v {
                                rows[z as usize][at] = a / 1e4;
                            }
                        }
                    }
                }
            }
        }
        (cols, rows)
    }
}

/// What a block of rows gave: continuous values by zone, or counts.
enum Part {
    Values(Vec<(u32, f32)>),
    Counts(HashMap<(u32, i64), (u64, f64)>),
    Transitions(HashMap<(u32, i64, i64), (u64, f64)>),
}

/// The statistics or areas of each feature of `vector` in each map of `sources` (on `grid`).
pub fn zonal(
    vector: &str,
    grid: &Grid,
    sources: &[AreaSource],
    opts: &ZonalOptions,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<ZonalTable, String> {
    if sources.is_empty() {
        return Err("no map".into());
    }
    let ds = Dataset::open(vector).map_err(|e| format!("{vector}: {e}"))?;
    let mut layer = if opts.layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(&opts.layer) }
        .map_err(|e| format!("{vector}: {e}"))?;
    let ct = to_crs(&layer, &grid.wkt)?;
    progress(0.0, "reading the features");
    let field = match opts.id_field.as_str() {
        "" => None,
        n => Some(layer.defn().field_index(n).map_err(|e| format!("{vector}: {n}: {e}"))?),
    };
    let (mut fids, mut ids, mut geoms): (Vec<i64>, Vec<String>, Vec<Option<Geometry>>) = (vec![], vec![], vec![]);
    for f in layer.features() {
        fids.push(f.fid().map_or(-1, |x| x as i64));
        ids.push(match field {
            Some(i) => f.field_as_string(i).ok().flatten().unwrap_or_default(),
            None => fids[fids.len() - 1].to_string(),
        });
        let g = match (f.geometry().filter(|g| !g.is_empty()), &ct) {
            (Some(g), Some(ct)) => Some(g.transform(ct).map_err(|e| format!("feature {}: {e}", fids.len()))?),
            (Some(g), None) => Some(g.clone()),
            _ => None,
        };
        geoms.push(g);
    }
    let zones = fids.len();
    if zones >= i32::MAX as usize {
        return Err(format!("{zones} features: too many"));
    }
    // the window: the features' extent on the grid (a pixel around), within opts.window
    let [wx0, wy0, wx1, wy1] = opts.window.unwrap_or([0, 0, grid.w, grid.h]);
    let gt = grid.gt;
    let det = gt[1] * gt[5] - gt[2] * gt[4];
    if det == 0.0 {
        return Err("the grid's geotransform cannot be inverted".into());
    }
    // a point to the grid's (column, row)
    let pixel = |x: f64, y: f64| {
        let (dx, dy) = (x - gt[0], y - gt[3]);
        ((gt[5] * dx - gt[2] * dy) / det, (-gt[4] * dx + gt[1] * dy) / det)
    };
    // a feature's extent in pixels (x0, y0, x1, y1) within the window
    let extent = |g: &Geometry| -> Option<[usize; 4]> {
        let e = g.envelope();
        let corners = [pixel(e.MinX, e.MinY), pixel(e.MinX, e.MaxY), pixel(e.MaxX, e.MinY), pixel(e.MaxX, e.MaxY)];
        let (mut c0, mut r0, mut c1, mut r1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (c, r) in corners {
            (c0, r0, c1, r1) = (c0.min(c), r0.min(r), c1.max(c), r1.max(r));
        }
        let clamp = |v: f64, lo: usize, hi: usize| (v.max(lo as f64).min(hi as f64)) as usize;
        let (x0, y0) = (clamp(c0.floor() - 1.0, wx0, wx1), clamp(r0.floor() - 1.0, wy0, wy1));
        let (x1, y1) = (clamp(c1.ceil() + 1.0, wx0, wx1), clamp(r1.ceil() + 1.0, wy0, wy1));
        (x1 > x0 && y1 > y0).then_some([x0, y0, x1, y1])
    };
    let extents: Vec<Option<[usize; 4]>> = geoms.iter().map(|g| g.as_ref().and_then(extent)).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for e in extents.iter().flatten() {
        (x0, y0, x1, y1) = (x0.min(e[0]), y0.min(e[1]), x1.max(e[2]), y1.max(e[3]));
    }
    if x1 <= x0 || y1 <= y0 {
        return Err("no feature on the maps' grid (within the window)".into());
    }
    let (w, h) = (x1 - x0, y1 - y0);
    if w.saturating_mul(h) > 2_000_000_000 {
        return Err(format!("the features cover {w} x {h} pixels: too many at once (use the ROI)"));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    // each pixel's zone + 1 (0 none, -1 more than one), then each feature over a shared pixel burned alone
    progress(0.02, &format!("burning {zones} features"));
    let ro: &[&str] = if opts.all_touched { &["ALL_TOUCHED=TRUE"] } else { &[] };
    let on: Vec<usize> = (0..zones).filter(|&z| geoms[z].is_some() && extents[z].is_some()).collect();
    // SAFETY: the geometries outlive the handles (geoms is not changed after this).
    let handles: Vec<gdal_sys::OGRGeometryH> =
        on.iter().map(|&z| unsafe { geoms[z].as_ref().unwrap().c_geometry() }).collect();
    let numbers: Vec<f64> = on.iter().map(|&z| z as f64 + 1.0).collect();
    let ones = vec![1.0; on.len()];
    let window = [x0, y0, w, h];
    let mut index = vec![0i32; w * h];
    let stop = |f: f64, a: f64, b: f64| {
        progress(a + (b - a) * f, "burning the features");
        !cancel.load(Ordering::Relaxed)
    };
    let failed = |e: String| if cancel.load(Ordering::Relaxed) { "cancelled".to_string() } else { e };
    // SAFETY: the geometries are alive and unchanged during the calls.
    unsafe {
        burn_strips::<i32>(
            &handles,
            &numbers,
            grid,
            window,
            gdal_sys::GDALDataType::GDT_Int32,
            0.0,
            ro,
            &|f| stop(f, 0.02, 0.08),
            &mut |y, n, b| {
                index[y * w..(y + n) * w].copy_from_slice(b);
                Ok(())
            },
        )
        .map_err(failed)?;
    }
    let mut shared: Vec<usize> = vec![];
    let add: Vec<String> = ro.iter().map(|s| s.to_string()).chain(["MERGE_ALG=ADD".to_string()]).collect();
    let add: Vec<&str> = add.iter().map(|s| s.as_str()).collect();
    // SAFETY: as above.
    unsafe {
        burn_strips::<i32>(
            &handles,
            &ones,
            grid,
            window,
            gdal_sys::GDALDataType::GDT_Int32,
            0.0,
            &add,
            &|f| stop(f, 0.08, 0.14),
            &mut |y, n, b| {
                for (i, &k) in b[..n * w].iter().enumerate() {
                    if k > 1 {
                        index[y * w + i] = -1;
                        shared.push(y * w + i);
                    }
                }
                Ok(())
            },
        )
        .map_err(failed)?;
    }
    // (pixel, zone) of the shared pixels, by pixel
    let mut extra: Vec<(usize, u32)> = vec![];
    if !shared.is_empty() {
        for &z in &on {
            let [ex0, ey0, ex1, ey1] = extents[z].unwrap();
            let (lx0, lx1) = (ex0 - x0, ex1 - x0);
            let any = (ey0 - y0..ey1 - y0).any(|r| {
                let lo = shared.partition_point(|&p| p < r * w + lx0);
                lo < shared.len() && shared[lo] < r * w + lx1
            });
            if !any {
                continue;
            }
            let ew = ex1 - ex0;
            // SAFETY: as above.
            let g = [unsafe { geoms[z].as_ref().unwrap().c_geometry() }];
            // SAFETY: as above.
            unsafe {
                burn_strips::<u8>(
                    &g,
                    &[1.0],
                    grid,
                    [ex0, ey0, ew, ey1 - ey0],
                    gdal_sys::GDALDataType::GDT_Byte,
                    0.0,
                    ro,
                    &|_| !cancel.load(Ordering::Relaxed),
                    &mut |y, n, b| {
                        for r in 0..n {
                            for c in 0..ew {
                                if b[r * ew + c] == 1 {
                                    let p = (ey0 - y0 + y + r) * w + ex0 - x0 + c;
                                    if shared.binary_search(&p).is_ok() {
                                        extra.push((p, z as u32));
                                    }
                                }
                            }
                        }
                        Ok(())
                    },
                )
                .map_err(failed)?;
            }
        }
        extra.sort_unstable();
    }
    let mut pixels = vec![0u64; zones];
    for &k in &index {
        if k > 0 {
            pixels[k as usize - 1] += 1;
        }
    }
    for &(_, z) in &extra {
        pixels[z as usize] += 1;
    }
    let pa = (opts.kind != ZonalKind::Continuous).then(|| PixelAreas::new(&gt, &grid.wkt, x0, y0, w, h));
    let floor = opts.kind == ZonalKind::Years;
    // blocks of rows of every map (then of the transitions), taken by the threads in turn
    let rows = (8_000_000 / w).clamp(16, 1024).min(h);
    let per = h.div_ceil(rows);
    let trans = opts.transitions.filter(|&(a, b)| opts.kind == ZonalKind::Classes && a.max(b) < sources.len());
    let jobs = sources.len() + usize::from(trans.is_some());
    let blocks = jobs * per;
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let left: Vec<AtomicUsize> = (0..jobs).map(|_| AtomicUsize::new(per)).collect();
    let parts: Vec<Mutex<Option<Part>>> = (0..blocks).map(|_| Mutex::new(None)).collect();
    let stats: Vec<Mutex<Vec<[f64; 10]>>> = (0..sources.len()).map(|_| Mutex::new(vec![])).collect();
    let counts: Vec<Mutex<Vec<Count>>> = (0..jobs).map(|_| Mutex::new(vec![])).collect();
    let error: Mutex<Option<String>> = Mutex::new(None);
    let (gw, gh) = (grid.w, grid.h);
    // a block of rows of a map, as f64 (no data NaN)
    let read = |open: &mut HashMap<usize, Dataset>, s: usize, by: usize, n: usize| -> Result<Vec<f64>, String> {
        let src = &sources[s];
        let d = match open.entry(s) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let d = Dataset::open(&src.path).map_err(|e| format!("{}: {e}", src.path))?;
                if d.raster_size() != (gw, gh) {
                    return Err(format!("{} is not on the grid ({gw} x {gh})", src.path));
                }
                e.insert(d)
            }
        };
        let band = d.rasterband(src.band.max(1)).map_err(|e| format!("{}: {e}", src.path))?;
        let nd = band.no_data_value().filter(|x| !x.is_nan());
        let mut v = band
            .read_as::<f64>((x0 as isize, (y0 + by) as isize), (w, n), (w, n), None)
            .map_err(|e| format!("{}: {e}", src.path))?
            .into_shape_and_vec()
            .1;
        if let Some(nd) = nd {
            v.iter_mut().filter(|x| **x == nd).for_each(|x| *x = f64::NAN);
        }
        Ok(v)
    };
    // the zones of each pixel of rows by..by+n: (offset in the block, zone)
    let zones_of = |by: usize, n: usize, out: &mut Vec<(usize, u32)>| {
        out.clear();
        for (i, &k) in index[by * w..(by + n) * w].iter().enumerate() {
            if k > 0 {
                out.push((i, k as u32 - 1));
            }
        }
        let lo = extra.partition_point(|e| e.0 < by * w);
        for &(p, z) in extra[lo..].iter().take_while(|e| e.0 < (by + n) * w) {
            out.push((p - by * w, z));
        }
    };
    let key = |v: f64| if floor { v.floor() as i64 } else { v.round() as i64 };
    // a map's (or the transitions') blocks put together, in their order
    let finish = |j: usize| {
        let mine: Vec<Part> = (0..per).filter_map(|b| parts[j * per + b].lock().unwrap().take()).collect();
        if j < sources.len() && opts.kind == ZonalKind::Continuous {
            let mut n = vec![0usize; zones + 1];
            for p in &mine {
                if let Part::Values(v) = p {
                    for &(z, _) in v {
                        n[z as usize + 1] += 1;
                    }
                }
            }
            for z in 0..zones {
                n[z + 1] += n[z];
            }
            let mut at = n.clone();
            let mut all = vec![0f32; n[zones]];
            for p in &mine {
                if let Part::Values(v) = p {
                    for &(z, x) in v {
                        all[at[z as usize]] = x;
                        at[z as usize] += 1;
                    }
                }
            }
            *stats[j].lock().unwrap() = (0..zones).map(|z| statistics(&mut all[n[z]..n[z + 1]])).collect();
        } else {
            let mut c: HashMap<(u32, i64, i64), (u64, f64)> = HashMap::new();
            for p in mine {
                match p {
                    Part::Counts(m) => {
                        for ((z, v), (p, a)) in m {
                            let e = c.entry((z, v, 0)).or_default();
                            (e.0, e.1) = (e.0 + p, e.1 + a);
                        }
                    }
                    Part::Transitions(m) => {
                        for (k, (p, a)) in m {
                            let e = c.entry(k).or_default();
                            (e.0, e.1) = (e.0 + p, e.1 + a);
                        }
                    }
                    Part::Values(_) => {}
                }
            }
            let mut v: Vec<(u32, i64, i64, u64, f64)> = c.into_iter().map(|(k, e)| (k.0, k.1, k.2, e.0, e.1)).collect();
            v.sort_unstable_by_key(|e| (e.0, e.1, e.2));
            *counts[j].lock().unwrap() = if j < sources.len() {
                v.into_iter().map(|e| (e.0, e.1, e.3, e.4)).collect()
            } else {
                // the transitions, kept as (zone, from, pixels, m²) with `to` in the value's place: unpacked below
                v.into_iter().map(|e| (e.0, e.1 * (1 << 32) + e.2, e.3, e.4)).collect()
            };
        }
    };
    std::thread::scope(|sc| {
        for _ in 0..opts.threads.clamp(1, blocks.max(1)) {
            sc.spawn(|| {
                let mut open: HashMap<usize, Dataset> = HashMap::new();
                let (mut zs, mut area) = (Vec::new(), vec![0.0; w]);
                loop {
                    let b = next.fetch_add(1, Ordering::Relaxed);
                    if b >= blocks || cancel.load(Ordering::Relaxed) || error.lock().unwrap().is_some() {
                        return;
                    }
                    let (j, by) = (b / per, (b % per) * rows);
                    let n = rows.min(h - by);
                    let r = (|| -> Result<Part, String> {
                        zones_of(by, n, &mut zs);
                        let at = |i: usize, area: &mut Vec<f64>, last: &mut usize| {
                            if let Some(pa) = &pa
                                && (*last != i / w || i == 0)
                            {
                                *last = i / w;
                                pa.row(by + i / w, area);
                            }
                        };
                        if j < sources.len() && opts.kind == ZonalKind::Continuous {
                            let v = read(&mut open, j, by, n)?;
                            return Ok(Part::Values(
                                zs.iter().filter(|e| !v[e.0].is_nan()).map(|&(i, z)| (z, v[i] as f32)).collect(),
                            ));
                        }
                        let mut last = usize::MAX;
                        if j < sources.len() {
                            let v = read(&mut open, j, by, n)?;
                            let mut m: HashMap<(u32, i64), (u64, f64)> = HashMap::new();
                            for &(i, z) in &zs {
                                if v[i].is_nan() {
                                    continue;
                                }
                                at(i, &mut area, &mut last);
                                let e = m.entry((z, key(v[i]))).or_default();
                                (e.0, e.1) = (e.0 + 1, e.1 + area[i % w]);
                            }
                            return Ok(Part::Counts(m));
                        }
                        let (sa, sb) = trans.unwrap();
                        let (a, bv) = (read(&mut open, sa, by, n)?, read(&mut open, sb, by, n)?);
                        let mut m: HashMap<(u32, i64, i64), (u64, f64)> = HashMap::new();
                        for &(i, z) in &zs {
                            if a[i].is_nan() || bv[i].is_nan() {
                                continue;
                            }
                            at(i, &mut area, &mut last);
                            let e = m.entry((z, key(a[i]), key(bv[i]))).or_default();
                            (e.0, e.1) = (e.0 + 1, e.1 + area[i % w]);
                        }
                        Ok(Part::Transitions(m))
                    })();
                    match r {
                        Ok(p) => *parts[b].lock().unwrap() = Some(p),
                        Err(e) => {
                            error.lock().unwrap().get_or_insert(e);
                            return;
                        }
                    }
                    if left[j].fetch_sub(1, Ordering::AcqRel) == 1 {
                        finish(j);
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(0.15 + 0.85 * d as f64 / blocks as f64, &format!("{d} of {blocks} blocks of rows"));
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
    let mut counts: Vec<Vec<Count>> = counts.into_iter().map(|c| c.into_inner().unwrap()).collect();
    let transitions: Vec<Transition> = if trans.is_some() {
        counts
            .pop()
            .unwrap_or_default()
            .into_iter()
            .map(|(z, k, p, a)| (z, k >> 32, k - ((k >> 32) << 32), p, a))
            .collect()
    } else {
        vec![]
    };
    let mut values: Vec<i64> = counts.iter().flatten().map(|c| c.1).collect();
    values.sort_unstable();
    values.dedup();
    Ok(ZonalTable {
        kind: opts.kind,
        labels: sources.iter().map(|s| s.label.clone()).collect(),
        fids,
        ids,
        pixels,
        stats: stats.into_iter().map(|s| s.into_inner().unwrap()).collect(),
        counts: if opts.kind == ZonalKind::Continuous { vec![] } else { counts },
        values,
        transitions,
        note: pa.map_or(String::new(), |p| p.describe()),
        window: [x0, y0, x1, y1],
        shared: shared.len(),
    })
}

/// The layer of `vector` written to the GeoPackage `out` (replaced) with `columns` added (a row per feature, in
/// their order; NaN as null).
pub fn write_gpkg(vector: &str, layer: &str, out: &Path, columns: &[String], rows: &[Vec<f64>]) -> Result<(), String> {
    let ds = Dataset::open(vector).map_err(|e| format!("{vector}: {e}"))?;
    let mut src = if layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(layer) }.map_err(|e| e.to_string())?;
    let c = |s: &str| CString::new(s).unwrap_or_default();
    let _ = std::fs::remove_file(out);
    let last = crate::polygonize::last_error;
    // SAFETY: GDAL's C API on a dataset made and closed here and on the open source layer.
    unsafe {
        let sl = src.c_layer();
        let drv = gdal_sys::GDALGetDriverByName(c("GPKG").as_ptr());
        let dst = gdal_sys::GDALCreate(
            drv,
            c(&out.to_string_lossy()).as_ptr(),
            0,
            0,
            0,
            gdal_sys::GDALDataType::GDT_Unknown,
            null_mut(),
        );
        if dst.is_null() {
            return Err(format!("{}: {}", out.display(), last()));
        }
        let fail = |e: String| {
            gdal_sys::GDALClose(dst);
            let _ = std::fs::remove_file(out);
            Err(e)
        };
        let name = if layer.is_empty() { "zonal".to_string() } else { layer.to_string() };
        let l = gdal_sys::GDALDatasetCreateLayer(
            dst,
            c(&name).as_ptr(),
            gdal_sys::OGR_L_GetSpatialRef(sl),
            gdal_sys::OGR_L_GetGeomType(sl),
            null_mut(),
        );
        if l.is_null() {
            return fail(format!("the layer: {}", last()));
        }
        let sd = gdal_sys::OGR_L_GetLayerDefn(sl);
        for i in 0..gdal_sys::OGR_FD_GetFieldCount(sd) {
            gdal_sys::OGR_L_CreateField(l, gdal_sys::OGR_FD_GetFieldDefn(sd, i), 1);
        }
        for col in columns {
            let fd = gdal_sys::OGR_Fld_Create(c(col).as_ptr(), gdal_sys::OGRFieldType::OFTReal);
            let r = gdal_sys::OGR_L_CreateField(l, fd, 1);
            gdal_sys::OGR_Fld_Destroy(fd);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                return fail(format!("the column {col}: {}", last()));
            }
        }
        let defn = gdal_sys::OGR_L_GetLayerDefn(l);
        let at: Vec<i32> = columns.iter().map(|col| gdal_sys::OGR_FD_GetFieldIndex(defn, c(col).as_ptr())).collect();
        gdal_sys::GDALDatasetStartTransaction(dst, 0);
        for (k, f) in src.features().enumerate() {
            let nf = gdal_sys::OGR_F_Create(defn);
            gdal_sys::OGR_F_SetFrom(nf, f.c_feature(), 1);
            if let Some(row) = rows.get(k) {
                for (&i, &x) in at.iter().zip(row) {
                    if i < 0 {
                        continue;
                    }
                    if x.is_nan() {
                        gdal_sys::OGR_F_SetFieldNull(nf, i);
                    } else {
                        gdal_sys::OGR_F_SetFieldDouble(nf, i, x);
                    }
                }
            }
            let r = gdal_sys::OGR_L_CreateFeature(l, nf);
            gdal_sys::OGR_F_Destroy(nf);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                gdal_sys::GDALDatasetRollbackTransaction(dst);
                return fail(format!("feature {k}: {}", last()));
            }
        }
        if gdal_sys::GDALDatasetCommitTransaction(dst) != gdal_sys::OGRErr::OGRERR_NONE {
            return fail(format!("the features: {}", last()));
        }
        gdal_sys::GDALClose(dst);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gdal::spatial_ref::SpatialRef;

    /// A 10 x 10 map at 30 m in UTM (value = row * 10 + column; one pixel without a value) and two overlapping
    /// squares and a feature without geometry: each square's pixels counted, the shared ones in both.
    #[test]
    fn zones() {
        let dir = std::env::temp_dir().join(format!("janus_zonal_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wkt = SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap();
        let gt = [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
        let mk = |name: &str, f: &dyn Fn(usize, usize) -> f64| {
            let p = dir.join(name);
            let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut d = drv.create_with_band_type::<f64, _>(&p, 10, 10, 1).unwrap();
            d.set_geo_transform(&gt).unwrap();
            d.set_projection(&wkt).unwrap();
            let mut b = d.rasterband(1).unwrap();
            b.set_no_data_value(Some(-1.0)).unwrap();
            let mut buf = gdal::raster::Buffer::new((10, 10), (0..100).map(|i| f(i / 10, i % 10)).collect());
            b.write((0, 0), (10, 10), &mut buf).unwrap();
            p.to_string_lossy().into_owned()
        };
        let a = mk("a.tif", &|r, c| if (r, c) == (6, 1) { -1.0 } else { (r * 10 + c) as f64 });
        let classes = mk("c.tif", &|r, _| if r < 5 { 1.0 } else { 2.0 });
        let later = mk("d.tif", &|_, c| if c < 3 { 1.0 } else { 2.0 });
        let gj = dir.join("v.geojson");
        // square A: columns 0-4, rows 5-9; square B: columns 3-6, rows 3-6 (pixels' centres)
        std::fs::write(
            &gj,
            r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::32723"}},
"features":[
{"type":"Feature","properties":{"name":"A"},"geometry":{"type":"Polygon","coordinates":[[[600000,7499700],[600150,7499700],[600150,7499850],[600000,7499850],[600000,7499700]]]}},
{"type":"Feature","properties":{"name":"B"},"geometry":{"type":"Polygon","coordinates":[[[600095,7499785],[600205,7499785],[600205,7499905],[600095,7499905],[600095,7499785]]]}},
{"type":"Feature","properties":{"name":"none"},"geometry":null}
]}"#,
        )
        .unwrap();
        let grid = Grid { wkt: wkt.clone(), gt, w: 10, h: 10 };
        let none = AtomicBool::new(false);
        let src = |p: &str, l: &str| AreaSource { path: p.into(), band: 1, label: l.into() };
        let opts = ZonalOptions { threads: 3, ..Default::default() };
        let v = gj.to_string_lossy().into_owned();
        let t = zonal(&v, &grid, &[src(&a, "2020"), src(&a, "2021")], &opts, &|_, _| {}, &none).unwrap();
        assert_eq!(t.pixels, vec![25, 16, 0]);
        assert_eq!(t.shared, 4); // rows 5-6, columns 3-4
        // A: rows 5-9, columns 0-4, (6, 1) without a value
        let want: Vec<f32> = (5..10)
            .flat_map(|r| (0..5).map(move |c| (r, c)))
            .filter(|&p| p != (6, 1))
            .map(|(r, c)| (r * 10 + c) as f32)
            .collect();
        let mut sorted = want.clone();
        let s = statistics(&mut sorted);
        assert_eq!(t.stats[0][0], s);
        assert_eq!(t.stats[1][0], s);
        assert_eq!(t.stats[0][0][0], 24.0);
        // B: rows 3-6, columns 3-6
        let b: Vec<f64> = (3..7).flat_map(|r| (3..7).map(move |c| (r * 10 + c) as f64)).collect();
        assert_eq!(t.stats[0][1][1], b.iter().sum::<f64>() / 16.0);
        assert_eq!(t.stats[0][1][3], 33.0);
        assert_eq!(t.stats[0][1][9], 66.0);
        assert_eq!(t.stats[0][1][6], 49.5); // numpy's median of 33..66
        assert!(t.stats[0][2][1].is_nan() && t.stats[0][2][0] == 0.0);
        // numpy.percentile([0, 1, 2, 3, 4], 10) = 0.4
        assert!((percentile(&[0.0, 1.0, 2.0, 3.0, 4.0], 0.1) - 0.4).abs() < 1e-12);
        // classes: A is 2 in rows 5-9; B rows 3-4 class 1, 5-6 class 2; the transitions to `later`
        let opts =
            ZonalOptions { kind: ZonalKind::Classes, transitions: Some((0, 1)), threads: 2, ..Default::default() };
        let t = zonal(&v, &grid, &[src(&classes, "2020"), src(&later, "2021")], &opts, &|_, _| {}, &none).unwrap();
        let px = |s: usize, z: u32, v: i64| t.counts[s].iter().find(|c| c.0 == z && c.1 == v).map_or(0, |c| c.2);
        assert_eq!((px(0, 0, 1), px(0, 0, 2)), (0, 25));
        assert_eq!((px(0, 1, 1), px(0, 1, 2)), (8, 8));
        assert_eq!((px(1, 0, 1), px(1, 0, 2)), (15, 10));
        let tr =
            |z: u32, a: i64, b: i64| t.transitions.iter().find(|e| (e.0, e.1, e.2) == (z, a, b)).map_or(0, |e| e.3);
        assert_eq!((tr(0, 2, 1), tr(0, 2, 2)), (15, 10));
        assert_eq!((tr(1, 1, 2), tr(1, 2, 2)), (8, 8));
        // about 900 m² a pixel in UTM near its central meridian
        let a0: f64 = t.counts[0].iter().filter(|c| c.0 == 0).map(|c| c.3).sum();
        assert!((a0 / 25.0 / 900.0 - 1.0).abs() < 2e-3, "{a0}");
        // the GeoPackage: the columns back on the features
        let (cols, rows) = t.wide(1, &|v| format!("c{v}"));
        assert_eq!(cols[..2], ["ha_c1_2020".to_string(), "ha_c2_2020".to_string()]);
        let out = dir.join("z.gpkg");
        write_gpkg(&v, "", &out, &cols, &rows).unwrap();
        let d = Dataset::open(&out).unwrap();
        let mut l = d.layer(0).unwrap();
        assert_eq!(l.feature_count(), 3);
        let f = l.features().next().unwrap();
        assert_eq!(f.field_as_string(f.field_index("name").unwrap()).unwrap().as_deref(), Some("A"));
        assert!((f.field_as_double(f.field_index("ha_c2_2020").unwrap()).unwrap().unwrap() - a0 / 1e4).abs() < 1e-9);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
