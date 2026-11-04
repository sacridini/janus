//! Profiles along lines: a line on a raster series' grid sampled along its length about a pixel apart (the
//! transect's sampling: the app's Transect panel uses `sample_line` too), and every line of a vector layer through
//! the dates chosen. The pixels the samples fall on are read by windows of up to 128 x 128 pixels around them (each
//! window once a date, a date per thread), the values the chart shows (janus_cube::CubeReader, or the full-resolution
//! cache in RAM); written as a long CSV (a row per sample and date) or a wide one (a row per sample).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use janus_cache::FullResCache;
use janus_cube::multidim::format_g;
use janus_cube::{CubeInfo, CubeReader};
use rayon::prelude::*;

use crate::vectors::{Kind, Placer, VectorData, csv_field};

/// A window of the grid: x, y, width, height.
type Window = (usize, usize, usize, usize);

/// The side of the windows read around the samples.
const TILE: usize = 128;

/// A line sampled on a series' grid: its vertices' distances, its length, and evenly spaced samples along it.
#[derive(Clone, Debug, Default)]
pub struct LineSamples {
    /// "m" (metres: on the sphere for a CRS in degrees), "map units" or "px"
    pub unit: String,
    /// the distance along the line at each vertex
    pub vdist: Vec<f64>,
    pub length: f64,
    /// samples
    pub n: usize,
    /// each sample's place (the grid's pixels, continuous) and pixel (-1: off the grid)
    pub at: Vec<[f64; 2]>,
    pub px: Vec<i64>,
    pub py: Vec<i64>,
    /// samples on the grid
    pub inside: usize,
}

impl LineSamples {
    /// Sample i's distance from the line's start.
    pub fn distance(&self, i: usize) -> f64 {
        self.length * i as f64 / (self.n - 1) as f64
    }
}

/// The line through `pts` (the grid's pixels, two or more) sampled about once per pixel: n = its length in pixels,
/// rounded up, plus one, at most `max_samples`, evenly spaced in distance. Distances in metres for an EPSG CRS or a
/// geographic one (haversine on the mean sphere in degrees), map units for another CRS, else pixels. `clipped`: a straight line
/// clipped to the grid, its ends kept on the grid. None: a line of no length.
pub fn sample_line(info: &CubeInfo, pts: &[[f64; 2]], max_samples: usize, clipped: bool) -> Option<LineSamples> {
    if pts.len() < 2 {
        return None;
    }
    let auth = &info.crs_authority;
    // longitude and latitude: an EPSG:4xxx code, or a geographic CRS without a code (a VRT's WKT often has none)
    let wkt = info.crs_wkt.trim_start();
    let degrees = auth.len() == 9 && auth.starts_with("EPSG:4")
        || auth.is_empty() && ["GEOGCS[", "GEOGCRS[", "GEODCRS["].iter().any(|k| wkt.starts_with(k));
    let georef = info.geo_transform.is_some();
    let mut s = LineSamples {
        unit: if !georef {
            "px"
        } else if degrees || !auth.is_empty() {
            "m"
        } else {
            "map units"
        }
        .to_string(),
        ..Default::default()
    };
    let segs = pts.len() - 1;
    let len_px: Vec<f64> = (0..segs).map(|k| (pts[k + 1][0] - pts[k][0]).hypot(pts[k + 1][1] - pts[k][1])).collect();
    let total_px: f64 = len_px.iter().sum();
    let len: Vec<f64> = (0..segs)
        .map(|k| {
            let g = georef
                .then(|| info.pixel_to_geo(pts[k][0], pts[k][1]).zip(info.pixel_to_geo(pts[k + 1][0], pts[k + 1][1])))
                .flatten();
            match g {
                None => len_px[k],
                Some(((g0x, g0y), (g1x, g1y))) if degrees => {
                    let (rad, r) = (std::f64::consts::PI / 180.0, 6371008.8);
                    let (la1, la2) = (g0y * rad, g1y * rad);
                    let (dla, dlo) = (la2 - la1, (g1x - g0x) * rad);
                    let h = (dla / 2.0).sin().powi(2) + la1.cos() * la2.cos() * (dlo / 2.0).sin().powi(2);
                    2.0 * r * h.sqrt().min(1.0).asin()
                }
                Some(((g0x, g0y), (g1x, g1y))) => (g1x - g0x).hypot(g1y - g0y),
            }
        })
        .collect();
    let dists = |l: &[f64]| -> Vec<f64> {
        let mut v = vec![0.0];
        for &x in l {
            v.push(v.last().unwrap() + x);
        }
        v
    };
    s.vdist = dists(&len);
    s.length = *s.vdist.last().unwrap();
    if s.length.is_nan() || s.length <= 0.0 {
        // a degenerate line, or no georeferencing: pixels
        s.unit = "px".into();
        s.vdist = dists(&len_px);
        s.length = *s.vdist.last().unwrap();
    }
    if s.length.is_nan() || s.length <= 0.0 {
        return None;
    }
    // samples evenly spaced along the line (in distance), about one per pixel
    let n = ((total_px.ceil() as usize) + 1).clamp(2, max_samples.max(2));
    s.n = n;
    (s.px, s.py, s.at) = (vec![-1; n], vec![-1; n], vec![[0.0; 2]; n]);
    let (w, h) = (info.width as i64, info.height as i64);
    let mut k = 0;
    for i in 0..n {
        let d = s.length * i as f64 / (n - 1) as f64;
        while k + 1 < segs && s.vdist[k + 1] < d {
            k += 1;
        }
        let seg = s.vdist[k + 1] - s.vdist[k];
        let f = if seg > 0.0 { ((d - s.vdist[k]) / seg).clamp(0.0, 1.0) } else { 0.0 };
        let q = [pts[k][0] + (pts[k + 1][0] - pts[k][0]) * f, pts[k][1] + (pts[k + 1][1] - pts[k][1]) * f];
        s.at[i] = q;
        let (x, y) = (q[0].floor() as i64, q[1].floor() as i64);
        // a straight line's ends may sit on the far edge (clipped): kept on the grid
        if (clipped && segs == 1) || (x >= 0 && y >= 0 && x < w && y < h) {
            (s.px[i], s.py[i]) = (x.clamp(0, w - 1), y.clamp(0, h - 1));
            s.inside += 1;
        }
    }
    Some(s)
}

/// What to read: the dates (places in the series), the fields written with each line, the threads.
#[derive(Clone, Debug, Default)]
pub struct ProfileOptions {
    pub dates: Vec<usize>,
    /// the layer's fields (their places) written with each line
    pub fields: Vec<usize>,
    pub threads: usize,
}

/// A line (a part of a feature) sampled on the grid.
#[derive(Clone, Debug)]
pub struct ProfileLine {
    /// the feature (its place in the layer) and its FID, the part of it
    pub feature: usize,
    pub fid: i64,
    pub part: usize,
    pub s: LineSamples,
}

/// Every line's samples through the dates.
#[derive(Clone, Debug, Default)]
pub struct Profiles {
    pub lines: Vec<ProfileLine>,
    /// the dates read (places in the series) and their labels
    pub dates: Vec<usize>,
    pub labels: Vec<String>,
    /// per line, [k * n + i]: date dates[k], sample i (NaN: no value, or off the grid)
    pub values: Vec<Vec<f32>>,
    /// the fields chosen, and their values per feature (empty without fields)
    pub field_names: Vec<String>,
    pub attrs: Vec<Vec<String>>,
    /// the series' geotransform (the samples' map coordinates)
    pub gt: Option<[f64; 6]>,
    /// lines of the layer left out (no part on the grid)
    pub off: usize,
    /// distinct pixels read, windows read per date
    pub pixels: usize,
    pub windows: usize,
}

/// Every line of `v` sampled on `info`'s grid and read at the dates chosen (every date when none is): by windows
/// around the samples, a date per thread (at most `opts.threads`), from `cache` when it holds them. `progress` gets
/// the share done; `cancel` stops it (Err "cancelled").
pub fn profiles(
    v: &VectorData,
    info: &Arc<CubeInfo>,
    cache: Option<&FullResCache>,
    opts: &ProfileOptions,
    progress: &(dyn Fn(f64) + Sync),
    cancel: &AtomicBool,
) -> Result<Profiles, String> {
    let placer = Placer::to_pixels(v, &info.crs_wkt, info.geo_transform)?;
    let dates: Vec<usize> = if opts.dates.is_empty() {
        (0..info.t()).collect()
    } else {
        opts.dates.iter().copied().filter(|&t| t < info.t()).collect()
    };
    let mut out = Profiles {
        labels: dates.iter().map(|&t| info.layers[t].label.clone()).collect(),
        dates,
        gt: info.geo_transform,
        ..Default::default()
    };
    // the lines: each part of every line feature, its repeated vertices dropped
    for (fi, f) in v.features.iter().enumerate() {
        if f.kind != Kind::Line {
            continue;
        }
        let mut any = false;
        for (part, r) in placer.placed(&f.parts).iter().enumerate() {
            let mut pts: Vec<[f64; 2]> = Vec::new();
            for p in r.as_chunks::<2>().0 {
                if pts.last().is_none_or(|l| l[0] != p[0] || l[1] != p[1]) {
                    pts.push([p[0], p[1]]);
                }
            }
            let Some(s) = sample_line(info, &pts, usize::MAX, false).filter(|s| s.inside > 0) else { continue };
            out.lines.push(ProfileLine { feature: fi, fid: f.fid, part, s });
            any = true;
        }
        out.off += usize::from(!any);
    }
    if out.lines.is_empty() {
        return Err("no line of the layer is on the series".into());
    }
    // the distinct pixels, and the windows around them (a tile of the grid each, cut to the pixels in it)
    let mut ids: HashMap<(i64, i64), u32> = HashMap::new();
    let mut pix: Vec<(usize, usize)> = Vec::new();
    let line_pix: Vec<Vec<u32>> = out
        .lines
        .iter()
        .map(|l| {
            (0..l.s.n)
                .map(|i| {
                    let (x, y) = (l.s.px[i], l.s.py[i]);
                    if x < 0 {
                        return u32::MAX;
                    }
                    *ids.entry((x, y)).or_insert_with(|| {
                        pix.push((x as usize, y as usize));
                        (pix.len() - 1) as u32
                    })
                })
                .collect()
        })
        .collect();
    let mut tiles: HashMap<(usize, usize), Vec<u32>> = HashMap::new();
    for (k, &(x, y)) in pix.iter().enumerate() {
        tiles.entry((y / TILE, x / TILE)).or_default().push(k as u32);
    }
    let mut tiles: Vec<((usize, usize), Vec<u32>)> = tiles.into_iter().collect();
    tiles.sort_unstable_by_key(|t| t.0); // row by row: a file in strips decodes each strip once
    let windows: Vec<(Window, Vec<u32>)> = tiles
        .into_iter()
        .map(|(_, ps)| {
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
            for &p in &ps {
                let (x, y) = pix[p as usize];
                (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1));
            }
            ((x0, y0, x1 - x0, y1 - y0), ps)
        })
        .collect();
    (out.pixels, out.windows) = (pix.len(), windows.len());
    // each date read on a thread of its own
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.threads.max(1))
        .thread_name(|i| format!("profiles-{i}"))
        .build()
        .map_err(|e| e.to_string())?;
    let done = AtomicUsize::new(0);
    let n_dates = out.dates.len().max(1);
    let by_date: Vec<Vec<f32>> = pool.install(|| {
        out.dates
            .par_iter()
            .map_init(
                || (CubeReader::new(info.clone()), Vec::<f32>::new()),
                |(r, buf), &t| -> Result<Vec<f32>, String> {
                    let mut vals = vec![f32::NAN; pix.len()];
                    for (win, ps) in &windows {
                        if cancel.load(Ordering::Relaxed) {
                            return Err("cancelled".into());
                        }
                        let size = (win.2, win.3);
                        buf.resize(size.0 * size.1, f32::NAN);
                        let ok = cache.is_some_and(|c| c.read_window(t, *win, buf, size))
                            || r.read_window(t, *win, buf, size);
                        if !ok {
                            continue; // no value
                        }
                        for &p in ps {
                            let (x, y) = pix[p as usize];
                            vals[p as usize] = buf[(y - win.1) * size.0 + (x - win.0)];
                        }
                    }
                    progress((done.fetch_add(1, Ordering::Relaxed) + 1) as f64 / n_dates as f64);
                    Ok(vals)
                },
            )
            .collect::<Result<Vec<_>, String>>()
    })?;
    out.values = line_pix
        .iter()
        .map(|ids| {
            let n = ids.len();
            let mut v = vec![f32::NAN; by_date.len() * n];
            for (k, d) in by_date.iter().enumerate() {
                for (i, &p) in ids.iter().enumerate() {
                    if p != u32::MAX {
                        v[k * n + i] = d[p as usize];
                    }
                }
            }
            v
        })
        .collect();
    if !opts.fields.is_empty() {
        let rows = crate::vectors::attributes(v)?;
        out.field_names = opts.fields.iter().filter_map(|&k| v.fields.get(k).cloned()).collect();
        out.attrs = rows
            .iter()
            .map(|r| {
                opts.fields
                    .iter()
                    .filter(|&&k| k < v.fields.len())
                    .map(|&k| r.get(k).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();
    }
    Ok(out)
}

fn number(v: f64, digits: usize) -> String {
    if v.is_nan() { String::new() } else { format_g(v, digits) }
}

/// A value as the shortest text that reads back as the same f32 (nothing for NaN).
fn value(out: &mut String, v: f32) {
    if !v.is_nan() {
        let _ = write!(out, "{v}");
    }
}

impl Profiles {
    /// The distance's column: "distance_m", "distance_px"...
    fn distance_name(&self) -> String {
        format!("distance_{}", self.lines.first().map_or("px", |l| l.s.unit.as_str()).replace(' ', "_"))
    }

    /// The columns before the values, and each sample's (on the grid) cells.
    fn head(&self) -> String {
        let mut h = String::from("fid");
        for f in &self.field_names {
            h += ",";
            h += &csv_field(f);
        }
        h += &format!(",part,sample,{},x,y,col,row", self.distance_name());
        h
    }

    fn sample_cells(&self, l: &ProfileLine, i: usize, out: &mut String) {
        let _ = write!(out, "{}", l.fid);
        if let Some(a) = self.attrs.get(l.feature) {
            for v in a {
                out.push(',');
                out.push_str(&csv_field(v));
            }
        }
        let [cx, cy] = l.s.at[i];
        let (x, y) = match self.gt {
            Some(g) => (g[0] + cx * g[1] + cy * g[2], g[3] + cx * g[4] + cy * g[5]),
            None => (cx, cy),
        };
        let _ = write!(
            out,
            ",{},{i},{},{},{},{},{}",
            l.part,
            number(l.s.distance(i), 10),
            number(x, 12),
            number(y, 12),
            l.s.px[i],
            l.s.py[i]
        );
    }

    /// A row per sample on the grid and date: the line's FID and fields, its part, the sample, its distance from the
    /// line's start, its place (the series' CRS) and pixel, the date and the value (empty: none). Line by line, date
    /// by date.
    pub fn long_csv(&self) -> String {
        let mut out = self.head() + ",date,value\n";
        let labels: Vec<String> = self.labels.iter().map(|l| csv_field(l)).collect();
        for (li, l) in self.lines.iter().enumerate() {
            let n = l.s.n;
            // each sample's cells, written once for every date
            let on: Vec<usize> = (0..n).filter(|&i| l.s.px[i] >= 0).collect();
            let cells: Vec<String> = on
                .iter()
                .map(|&i| {
                    let mut c = String::new();
                    self.sample_cells(l, i, &mut c);
                    c
                })
                .collect();
            for (k, label) in labels.iter().enumerate() {
                for (&i, c) in on.iter().zip(&cells) {
                    out.push_str(c);
                    out.push(',');
                    out.push_str(label);
                    out.push(',');
                    value(&mut out, self.values[li][k * n + i]);
                    out.push('\n');
                }
            }
        }
        out
    }

    /// A row per sample on the grid, a column per date.
    pub fn wide_csv(&self) -> String {
        let mut out = self.head();
        for l in &self.labels {
            out.push(',');
            out.push_str(&csv_field(l));
        }
        out.push('\n');
        for (li, l) in self.lines.iter().enumerate() {
            let n = l.s.n;
            for i in (0..n).filter(|&i| l.s.px[i] >= 0) {
                self.sample_cells(l, i, &mut out);
                for k in 0..self.labels.len() {
                    out.push(',');
                    value(&mut out, self.values[li][k * n + i]);
                }
                out.push('\n');
            }
        }
        out
    }

    /// The samples on the grid as points in the series' CRS (`crs_wkt`), a GeoPackage, shapefile or GeoJSON by
    /// `path`'s extension: the line's FID and fields, its part, the sample, its distance, its pixel and a column per
    /// date (the wide table's rows). A shapefile's names cut to 10 characters by GDAL, at most 255 columns. The
    /// points written.
    pub fn write_points(&self, path: &str, crs_wkt: &str) -> Result<usize, String> {
        use gdal::vector::{FieldDefn, Geometry, LayerAccess, LayerOptions, OGRFieldType, OGRwkbGeometryType};
        let (driver, layer_options) = crate::polygon_file::vector_driver(path)?;
        let columns = 7 + self.field_names.len() + self.labels.len();
        if driver == "ESRI Shapefile" && columns > 255 {
            return Err(format!("{columns} columns: past a shapefile's 255 (a GeoPackage has no such limit)"));
        }
        crate::vectors::register();
        let drv = gdal::DriverManager::get_driver_by_name(driver).map_err(|e| e.to_string())?;
        let srs = (!crs_wkt.is_empty()).then(|| crate::reproject::srs(crs_wkt)).flatten();
        let name =
            std::path::Path::new(path).file_stem().map_or("profiles".into(), |s| s.to_string_lossy().into_owned());
        let mut ds = drv.create_vector_only(path).map_err(|e| format!("cannot create {path}: {e}"))?;
        let fill = |ds: &mut gdal::Dataset| -> Result<usize, String> {
            let l = ds
                .create_layer(LayerOptions {
                    name: &name,
                    srs: srs.as_ref(),
                    ty: OGRwkbGeometryType::wkbPoint,
                    options: Some(layer_options),
                })
                .map_err(|e| format!("cannot write {path}: {e}"))?;
            let mut defs: Vec<(String, OGRFieldType::Type)> = vec![("fid_line".into(), OGRFieldType::OFTInteger64)];
            defs.extend(self.field_names.iter().map(|f| (f.clone(), OGRFieldType::OFTString)));
            for (n, t) in [
                ("part", OGRFieldType::OFTInteger),
                ("sample", OGRFieldType::OFTInteger),
                (self.distance_name().as_str(), OGRFieldType::OFTReal),
                ("col", OGRFieldType::OFTInteger64),
                ("row", OGRFieldType::OFTInteger64),
            ] {
                defs.push((n.to_string(), t));
            }
            defs.extend(self.labels.iter().map(|d| (d.clone(), OGRFieldType::OFTReal)));
            for (n, t) in &defs {
                FieldDefn::new(n, *t).and_then(|d| d.add_to_layer(&l)).map_err(|e| format!("{n}: {e}"))?;
            }
            let nf = self.field_names.len();
            let mut written = 0;
            for (li, line) in self.lines.iter().enumerate() {
                let n = line.s.n;
                for i in (0..n).filter(|&i| line.s.px[i] >= 0) {
                    let [cx, cy] = line.s.at[i];
                    let (x, y) = match self.gt {
                        Some(g) => (g[0] + cx * g[1] + cy * g[2], g[3] + cx * g[4] + cy * g[5]),
                        None => (cx, cy),
                    };
                    let mut g = Geometry::empty(OGRwkbGeometryType::wkbPoint).map_err(|e| e.to_string())?;
                    g.add_point_2d((x, y));
                    let mut f = gdal::vector::Feature::new(l.defn()).map_err(|e| e.to_string())?;
                    f.set_geometry(g).map_err(|e| e.to_string())?;
                    let e = |r: gdal::errors::Result<()>| r.map_err(|e| e.to_string());
                    e(f.set_field_integer64(0, line.fid))?;
                    if let Some(a) = self.attrs.get(line.feature) {
                        for (k, v) in a.iter().enumerate().take(nf) {
                            e(f.set_field_string(1 + k, v))?;
                        }
                    }
                    e(f.set_field_integer(1 + nf, line.part as i32))?;
                    e(f.set_field_integer(2 + nf, i as i32))?;
                    e(f.set_field_double(3 + nf, line.s.distance(i)))?;
                    e(f.set_field_integer64(4 + nf, line.s.px[i]))?;
                    e(f.set_field_integer64(5 + nf, line.s.py[i]))?;
                    for k in 0..self.labels.len() {
                        let v = self.values[li][k * n + i];
                        if v.is_finite() {
                            e(f.set_field_double(6 + nf + k, f64::from(v)))?;
                        }
                    }
                    f.create(&l).map_err(|e| format!("cannot write {path}: {e}"))?;
                    written += 1;
                }
            }
            Ok(written)
        };
        let written = if driver == "GPKG" {
            let mut tx = ds.start_transaction().map_err(|e| e.to_string())?;
            let n = fill(&mut tx)?;
            tx.commit().map_err(|e| format!("cannot write {path}: {e}"))?;
            n
        } else {
            fill(&mut ds)?
        };
        ds.close().map_err(|e| format!("cannot write {path}: {e}"))?;
        Ok(written)
    }

    /// Samples on the grid, over every line.
    pub fn samples(&self) -> usize {
        self.lines.iter().map(|l| l.s.inside).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vectors::load_vector_file;

    /// A series of `t` dates of w x h pixels of 30 m in UTM 23S, value = date * 10000 + row * w + col (no data at
    /// row 5).
    fn series(dir: &std::path::Path, w: usize, h: usize, t: usize) -> Arc<CubeInfo> {
        crate::vectors::register();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32723).unwrap();
        let mut files = Vec::new();
        for k in 0..t {
            let p = dir.join(format!("v_{}.tif", 2001 + k));
            let mut ds = drv.create_with_band_type::<f32, _>(&p, w, h, 1).unwrap();
            ds.set_geo_transform(&[500000.0, 30.0, 0.0, 7600000.0, 0.0, -30.0]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            let mut b = ds.rasterband(1).unwrap();
            b.set_no_data_value(Some(-9999.0)).unwrap();
            let mut v: Vec<f32> = (0..w * h).map(|i| (k * 10000 + i) as f32).collect();
            for c in 0..w {
                v[5 * w + c] = -9999.0;
            }
            let mut buf = gdal::raster::Buffer::new((w, h), v);
            b.write((0, 0), (w, h), &mut buf).unwrap();
            files.push(p.to_string_lossy().into_owned());
        }
        Arc::new(
            janus_cube::open_cube(&files, &janus_cube::BandSelection { chosen: true, ..Default::default() }).unwrap(),
        )
    }

    #[test]
    fn lines_sampled_and_read() {
        let d = std::env::temp_dir().join(format!("janus-geo-profiles-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let (w, h) = (300, 200);
        let info = series(&d, w, h, 3);
        // pixels -> UTM 23S
        let g = |x: f64, y: f64| (500000.0 + x * 30.0, 7600000.0 - y * 30.0);
        let coords = |pts: &[(f64, f64)]| {
            pts.iter().map(|&(x, y)| g(x, y)).map(|(a, b)| format!("[{a}, {b}]")).collect::<Vec<_>>().join(", ")
        };
        let gj = d.join("lines.geojson");
        std::fs::write(
            &gj,
            format!(
                r#"{{"type": "FeatureCollection", "crs": {{"type": "name", "properties": {{"name": "urn:ogc:def:crs:EPSG::32723"}}}}, "features": [
{{"type": "Feature", "properties": {{"name": "a", "k": 1}}, "geometry": {{"type": "LineString", "coordinates": [{}]}}}},
{{"type": "Feature", "properties": {{"name": "b", "k": 2}}, "geometry": {{"type": "MultiLineString", "coordinates": [[{}], [{}]]}}}},
{{"type": "Feature", "properties": {{"name": "off", "k": 3}}, "geometry": {{"type": "LineString", "coordinates": [{}]}}}},
{{"type": "Feature", "properties": {{"name": "pt", "k": 4}}, "geometry": {{"type": "Point", "coordinates": [500000, 7600000]}}}}
]}}"#,
                coords(&[(10.3, 2.7), (120.6, 40.2), (130.1, 150.9)]),
                coords(&[(250.5, 10.5), (290.25, 10.5)]),
                coords(&[(-20.5, 100.3), (40.5, 100.3)]),
                coords(&[(-50.0, -50.0), (-10.0, -10.0)])
            ),
        )
        .unwrap();
        let v = &load_vector_file(gj.to_str().unwrap(), 1_000_000).unwrap()[0];
        let opts = ProfileOptions { dates: vec![0, 2], fields: vec![0], threads: 2 };
        let p = profiles(v, &info, None, &opts, &|_| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!((p.lines.len(), p.off), (3, 1));
        assert_eq!(p.labels.len(), 2);
        let a = &p.lines[0];
        // about a sample per pixel; metres
        let len_px = (110.3f64).hypot(37.5) + (9.5f64).hypot(110.7);
        assert_eq!(a.s.n, len_px.ceil() as usize + 1);
        assert_eq!(a.s.unit, "m");
        assert!((a.s.length - len_px * 30.0).abs() < 1e-6);
        // every value is the pixel's (date 2's), the no data row empty
        for (li, l) in p.lines.iter().enumerate() {
            for i in 0..l.s.n {
                let (x, y) = (l.s.px[i], l.s.py[i]);
                let got = p.values[li][l.s.n + i];
                if x < 0 {
                    assert!(got.is_nan());
                    continue;
                }
                assert_eq!((x, y), (l.s.at[i][0].floor() as i64, l.s.at[i][1].floor() as i64));
                if y == 5 {
                    assert!(got.is_nan());
                } else {
                    assert_eq!(got, (20000 + y as usize * w + x as usize) as f32);
                }
            }
        }
        // the second part of "b" starts off the grid: its first 21 samples left out
        let b2 = &p.lines[2];
        assert_eq!((b2.part, b2.s.n, b2.s.inside), (1, 62, 41));
        let long = p.long_csv();
        let rows: Vec<&str> = long.lines().collect();
        assert_eq!(rows[0], "fid,name,part,sample,distance_m,x,y,col,row,date,value");
        assert_eq!(rows.len(), 1 + 2 * p.samples());
        assert!(rows[1].starts_with("0,a,0,0,0,500309,7599919,10,2,2001,"), "{}", rows[1]);
        let wide = p.wide_csv();
        assert_eq!(wide.lines().next().unwrap(), "fid,name,part,sample,distance_m,x,y,col,row,2001,2003");
        assert_eq!(wide.lines().count(), 1 + p.samples());
        // the samples as points: a GeoPackage and a shapefile, the wide table's rows
        for name in ["points.gpkg", "points.shp"] {
            let path = d.join(name).to_string_lossy().into_owned();
            assert_eq!(p.write_points(&path, &info.crs_wkt).unwrap(), p.samples(), "{name}");
            let pts = &load_vector_file(&path, 1_000_000).unwrap()[0];
            assert_eq!(pts.features.len(), p.samples(), "{name}");
            assert_eq!(pts.crs_name, "EPSG:32723", "{name}");
            let f = &pts.features[0];
            assert_eq!(f.kind, Kind::Point);
            assert_eq!((f.parts[0][0], f.parts[0][1]), (500309.0, 7599919.0), "{name}");
            let n = pts.fields.len();
            assert_eq!(&f.values[..3], ["0", "a", "0"], "{name}: {:?}", pts.fields);
            let first: Vec<&str> = wide.lines().nth(1).unwrap().split(',').collect();
            for (k, v) in f.values[n - 2..].iter().enumerate() {
                let want: f64 = first[first.len() - 2 + k].parse().unwrap();
                assert_eq!(v.parse::<f64>().unwrap(), want, "{name}");
            }
        }
        // cancelled
        let c = AtomicBool::new(true);
        assert_eq!(profiles(v, &info, None, &opts, &|_| {}, &c).unwrap_err(), "cancelled");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn distances_in_the_crs_units() {
        let d = std::env::temp_dir().join(format!("janus-geo-profiles-units-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let utm = series(&d, 20, 20, 1);
        let line = [[0.5, 0.5], [10.5, 0.5]];
        let unit = |auth: &str, wkt: &str, gt: Option<[f64; 6]>| {
            let mut i = (*utm).clone();
            (i.crs_authority, i.crs_wkt, i.geo_transform) = (auth.into(), wkt.into(), gt);
            let s = sample_line(&i, &line, usize::MAX, false).unwrap();
            (s.unit, s.length)
        };
        assert_eq!(unit("EPSG:32723", "PROJCS[...]", utm.geo_transform), ("m".into(), 300.0));
        // degrees: on the sphere, with a code or (a VRT's WKT) without one
        let deg = Some([-37.0, 0.001, 0.0, -10.0, 0.0, -0.001]);
        let (u, l) = unit("EPSG:4326", "GEOGCS[\"WGS 84\"]", deg);
        assert_eq!(u, "m");
        assert!(
            (l - 10.0 * 0.001 * std::f64::consts::PI / 180.0 * 6371008.8 * (10.0005f64).to_radians().cos()).abs()
                < 0.01
        );
        assert_eq!(unit("", "GEOGCS[\"WGS 84\"]", deg), ("m".into(), l));
        assert_eq!(unit("", "PROJCS[\"local\"]", utm.geo_transform), ("map units".into(), 300.0));
        assert_eq!(unit("", "", None), ("px".into(), 10.0));
        let _ = std::fs::remove_dir_all(&d);
    }
}
