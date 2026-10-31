//! Rasters read beside a series as a Random Forest's further predictors: a band of a file on the series' grid (the
//! same pixel size, aligned; it may cover only part of the series), such as Zeit's texture of the series, a DEM or a
//! distance map. A pixel has one value of each, whatever the date: the forest reads the series' values of every date
//! and then these.

use gdal::Dataset;
use janus_cube::CubeInfo;
use serde::{Deserialize, Serialize};

use crate::data::Series;

/// A raster read as a predictor.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Extra {
    /// the file
    pub path: String,
    /// its band (1-based)
    pub band: usize,
    /// its name (the layer's)
    pub name: String,
}

/// An extra opened: its offset on the series' grid (its pixel 0, 0 is the series' dx, dy) and its size.
struct Opened {
    ds: Dataset,
    band: usize,
    dx: i64,
    dy: i64,
    w: usize,
    h: usize,
    nodata: Option<f64>,
}

fn open(e: &Extra, info: &CubeInfo) -> Result<Opened, String> {
    let ds = Dataset::open(&e.path).map_err(|err| format!("{} ({}): {err}", e.name, e.path))?;
    let (w, h) = ds.raster_size();
    if e.band == 0 || e.band > ds.raster_count() {
        return Err(format!("{} ({}): no band {}", e.name, e.path, e.band));
    }
    let g = ds.geo_transform().map_err(|err| format!("{} ({}): {err}", e.name, e.path))?;
    let s = info.geo_transform.ok_or("the series has no georeferencing: its pixels cannot be matched")?;
    let same = |a: f64, b: f64| (a - b).abs() <= 1e-6 * b.abs().max(1e-12);
    if !same(g[1], s[1]) || !same(g[5], s[5]) || g[2] != 0.0 || g[4] != 0.0 || s[2] != 0.0 || s[4] != 0.0 {
        return Err(format!(
            "{}: not on the series' grid (its pixels {} x {}, the series' {} x {}); warp it to the series first",
            e.name, g[1], g[5], s[1], s[5]
        ));
    }
    let (fx, fy) = ((g[0] - s[0]) / s[1], (g[3] - s[3]) / s[5]);
    if (fx - fx.round()).abs() > 1e-3 || (fy - fy.round()).abs() > 1e-3 {
        return Err(format!("{}: its pixels are offset from the series' ({fx:.3}, {fy:.3} pixels)", e.name));
    }
    let nodata = ds.rasterband(e.band).ok().and_then(|b| b.no_data_value()).filter(|v| !v.is_nan());
    Ok(Opened { ds, band: e.band, dx: fx.round() as i64, dy: fy.round() as i64, w, h, nodata })
}

impl Opened {
    /// Its values over a rectangle of the series' pixels (x, y, w, h), row by row: NaN outside it or at its nodata.
    fn rect(&self, x: usize, y: usize, w: usize, h: usize) -> Result<Vec<f32>, String> {
        let mut out = vec![f32::NAN; w * h];
        // the rectangle in the raster's pixels, cut to it
        let (rx0, ry0) = (x as i64 - self.dx, y as i64 - self.dy);
        let (ax, ay) = (rx0.max(0), ry0.max(0));
        let (bx, by) = ((rx0 + w as i64).min(self.w as i64), (ry0 + h as i64).min(self.h as i64));
        if bx <= ax || by <= ay {
            return Ok(out);
        }
        let (cw, ch) = ((bx - ax) as usize, (by - ay) as usize);
        let band = self.ds.rasterband(self.band).map_err(|e| e.to_string())?;
        let buf =
            band.read_as::<f64>((ax as isize, ay as isize), (cw, ch), (cw, ch), None).map_err(|e| e.to_string())?;
        let data = buf.data();
        let (ox, oy) = ((ax - rx0) as usize, (ay - ry0) as usize);
        for r in 0..ch {
            for c in 0..cw {
                let v = data[r * cw + c];
                if v.is_finite() && self.nodata.is_none_or(|n| v != n) {
                    out[(oy + r) * w + ox + c] = v as f32;
                }
            }
        }
        Ok(out)
    }
}

/// Whether the extras can be read beside the series (their files, bands and grids).
pub fn check(extras: &[Extra], info: &CubeInfo) -> Result<(), String> {
    for e in extras {
        open(e, info)?;
    }
    Ok(())
}

/// The extras' values at the series' pixels `points`: each point's `extras.len()` values (NaN where a raster has
/// none), read in strips of rows.
pub fn read_points(extras: &[Extra], info: &CubeInfo, points: &[(usize, usize)]) -> Result<Vec<f32>, String> {
    const STRIP: usize = 64;
    let e = extras.len();
    let mut out = vec![f32::NAN; points.len() * e];
    let mut order: Vec<usize> = (0..points.len()).collect();
    order.sort_by_key(|&i| points[i].1);
    for (k, ex) in extras.iter().enumerate() {
        let o = open(ex, info)?;
        let mut at = 0;
        while at < order.len() {
            let y0 = points[order[at]].1 / STRIP * STRIP;
            let mut end = at;
            while end < order.len() && points[order[end]].1 < y0 + STRIP {
                end += 1;
            }
            let strip = &order[at..end];
            let x0 = strip.iter().map(|&i| points[i].0).min().unwrap_or(0);
            let x1 = strip.iter().map(|&i| points[i].0).max().unwrap_or(0);
            let (w, h) = (x1 - x0 + 1, STRIP);
            let v = o.rect(x0, y0, w, h)?;
            for &i in strip {
                let (x, y) = points[i];
                out[i * e + k] = v[(y - y0) * w + x - x0];
            }
            at = end;
        }
    }
    Ok(out)
}

/// The extras' values over a window of the series (x, y, w, h): each pixel's `extras.len()` values, row by row.
pub fn read_window(extras: &[Extra], info: &CubeInfo, win: (usize, usize, usize, usize)) -> Result<Vec<f32>, String> {
    let (x, y, w, h) = win;
    let e = extras.len();
    let mut out = vec![f32::NAN; w * h * e];
    for (k, ex) in extras.iter().enumerate() {
        let v = open(ex, info)?.rect(x, y, w, h)?;
        for (i, &v) in v.iter().enumerate() {
            out[i * e + k] = v;
        }
    }
    Ok(out)
}

/// The series with each pixel's extras (`ex`, `e` a pixel) on every date as further values (b + e): so that they
/// go through what the series' values go through (a place's mean, the pixels without values left out).
pub fn repeat(s: &Series, ex: &[f32], e: usize) -> Series {
    let (t, b) = (s.t, s.b);
    let mut values = Vec::with_capacity(s.n * t * (b + e));
    for i in 0..s.n {
        let px = s.pixel(i);
        for d in 0..t {
            values.extend_from_slice(&px[d * b..(d + 1) * b]);
            values.extend_from_slice(&ex[i * e..(i + 1) * e]);
        }
    }
    Series { n: s.n, t, b: b + e, values }
}

/// [`repeat`] undone: the series (b - e values a date) and each pixel's extras (those of its first date).
pub fn split(s: &Series, e: usize) -> (Series, Vec<f32>) {
    let (t, b) = (s.t, s.b - e);
    let mut values = Vec::with_capacity(s.n * t * b);
    let mut ex = Vec::with_capacity(s.n * e);
    for i in 0..s.n {
        let px = s.pixel(i);
        for d in 0..t {
            values.extend_from_slice(&px[d * (b + e)..d * (b + e) + b]);
        }
        if t > 0 {
            ex.extend_from_slice(&px[b..b + e]);
        } else {
            ex.extend(std::iter::repeat_n(f32::NAN, e));
        }
    }
    (Series { n: s.n, t, b, values }, ex)
}

/// What the forest reads: each pixel's series (every date's values) and then its extras, as one date of
/// t * b + e values.
pub fn append(s: &Series, ex: &[f32], e: usize) -> Series {
    let p = s.t * s.b;
    let mut values = Vec::with_capacity(s.n * (p + e));
    for i in 0..s.n {
        values.extend_from_slice(s.pixel(i));
        values.extend_from_slice(&ex[i * e..(i + 1) * e]);
    }
    Series { n: s.n, t: 1, b: p + e, values }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_split_append_keep_the_values() {
        // 2 pixels, 3 dates, 2 values; 1 extra
        let s = Series { n: 2, t: 3, b: 2, values: (0..12).map(|v| v as f32).collect() };
        let ex = vec![100.0, 200.0];
        let r = repeat(&s, &ex, 1);
        assert_eq!((r.t, r.b), (3, 3));
        assert_eq!(r.pixel(1), &[6.0, 7.0, 200.0, 8.0, 9.0, 200.0, 10.0, 11.0, 200.0]);
        let (back, ex2) = split(&r, 1);
        assert_eq!(back.values, s.values);
        assert_eq!(ex2, ex);
        let a = append(&s, &ex, 1);
        assert_eq!((a.t, a.b), (1, 7));
        assert_eq!(a.pixel(0), &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 100.0]);
    }

    #[test]
    fn a_raster_over_part_of_the_series_is_read_on_its_grid() {
        use gdal::DriverManager;
        use gdal::raster::Buffer;
        let dir = std::env::temp_dir().join(format!("janus_extra_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tex.tif");
        // the series: 10 x 8 pixels of 10 m from (1000, 2000); the raster: 4 x 3 from the series' pixel (5, 2)
        let info = CubeInfo {
            width: 10,
            height: 8,
            geo_transform: Some([1000.0, 10.0, 0.0, 2000.0, 0.0, -10.0]),
            ..Default::default()
        };
        {
            let drv = DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut ds = drv.create_with_band_type::<f32, _>(&path, 4, 3, 1).unwrap();
            ds.set_geo_transform(&[1050.0, 10.0, 0.0, 1980.0, 0.0, -10.0]).unwrap();
            let mut b = ds.rasterband(1).unwrap();
            b.set_no_data_value(Some(-1.0)).unwrap();
            let mut v: Vec<f32> = (0..12).map(|k| k as f32).collect();
            v[5] = -1.0;
            b.write((0, 0), (4, 3), &mut Buffer::new((4, 3), v)).unwrap();
        }
        let ex = [Extra { path: path.to_string_lossy().into_owned(), band: 1, name: "tex".into() }];
        check(&ex, &info).unwrap();
        // (5, 2) is its first pixel, (8, 4) its last; (6, 3) its nodata; (0, 0) outside
        let p = read_points(&ex, &info, &[(5, 2), (8, 4), (6, 3), (0, 0), (7, 2)]).unwrap();
        assert_eq!(p[0], 0.0);
        assert_eq!(p[1], 11.0);
        assert!(p[2].is_nan() && p[3].is_nan());
        assert_eq!(p[4], 2.0);
        let w = read_window(&ex, &info, (4, 2, 3, 2)).unwrap();
        assert!(w[0].is_nan());
        assert_eq!(&w[1..3], &[0.0, 1.0]);
        assert!(w[3].is_nan());
        assert_eq!(w[4], 4.0);
        assert!(w[5].is_nan());
        // another pixel size: refused
        let other = CubeInfo { geo_transform: Some([1000.0, 20.0, 0.0, 2000.0, 0.0, -20.0]), ..info.clone() };
        assert!(check(&ex, &other).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
