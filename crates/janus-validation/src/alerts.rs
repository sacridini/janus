//! Official alerts against a map of change (IDEIAS: alertas oficiais): deforestation and disturbance alerts (INPE's
//! DETER polygons, UMD's GLAD and Wageningen's RADD rasters) placed on the grid of a map of change dates (a Zeit
//! result in decimal years: LandTrendr, CCDC, BFAST, CODED...) and compared with it over the area both cover and a
//! window of dates: the area of change in both, only in the map, only in the alerts; the share of the alerts the
//! map detects and of the map the alerts confirm, by area and by count (alert polygons or patches, the map's
//! patches); the lag between the map's date and the alert's (the map's minus the alert's, in days), its quantiles
//! and histogram; by alert class (DETER's classname) or confidence (GLAD, RADD).
//!
//! The rules, the same in tools/bench_alerts_vs_python.py (geopandas, rasterio, numpy, scipy):
//! - the grid is the map's; a polygon covers the pixels whose centres it holds (GDALRasterizeGeometries, as
//!   rasterio.features.rasterize), burned from the latest alert to the earliest so that where alerts overlap the
//!   earliest one holds the pixel; a raster of alerts is warped onto the grid by its nearest pixel (gdalwarp -r
//!   near -et 0: the exact transformer, as rasterio's WarpedVRT with tolerance=1e-9);
//! - a map pixel changed when its value is a date (finite, > 0) in the window; an alert pixel when its alert's date
//!   is in the window; pixels outside the alerts' cover (a raster's tiles) are left out of everything;
//! - an alert (polygon, or a raster's patch: 8-connected alert pixels of the window) is detected when at least one
//!   of its pixels changed in the map; a patch's date is its earliest, its confidence its highest; the map's
//!   patches (8-connected) are confirmed when one of their pixels is an alert's;
//! - quantiles interpolate linearly between the sorted values (numpy's default); the histogram's bins are
//!   `bin_days` wide from 0 (floor(lag / bin_days)).

use std::collections::BTreeMap;
use std::path::Path;

use gdal::Dataset;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::{Geometry, LayerAccess};

use crate::reader::MapReader;
use crate::rules::{Grid, read_grid, row_areas};

/// No date.
pub const NO_DAY: i32 = i32::MIN;

// ---------------------------------------------------------------------------------------------------------------
// Dates: days since 1970-01-01

pub fn days_from_civil(y: i32, m: u32, d: u32) -> i32 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = (u64::from(m) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + u64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64 - 719468) as i32
}

pub fn civil_from_days(z: i32) -> (i32, u32, u32) {
    let z = i64::from(z) + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    ((i64::from(yoe) + era * 400 + i64::from(m <= 2)) as i32, m, d)
}

fn year_len(y: i32) -> i32 {
    if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 366 } else { 365 }
}

/// YYYY-MM-DD.
pub fn ymd(day: i32) -> String {
    if day == NO_DAY {
        return String::new();
    }
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A date's day: "2023-05-12", "2023/05/12", "2023-05-12T10:00:00Z" (the first three numbers).
pub fn parse_day(s: &str) -> Option<i32> {
    let mut it = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || !(1000..=9999).contains(&y) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// A decimal year's day (Janus's decimal years: the year plus the days since its 1 January over its length), to
/// the nearest day; None when it is no date (not finite, <= 0).
pub fn day_of_decimal_year(v: f64) -> Option<i32> {
    if !v.is_finite() || v <= 0.0 {
        return None;
    }
    let y = v.floor() as i32;
    Some(days_from_civil(y, 1, 1) + ((v - f64::from(y)) * f64::from(year_len(y))).round() as i32)
}

/// A day as a decimal year.
pub fn decimal_year(day: i32) -> f64 {
    let (y, _, _) = civil_from_days(day);
    f64::from(y) + f64::from(day - days_from_civil(y, 1, 1)) / f64::from(year_len(y))
}

// ---------------------------------------------------------------------------------------------------------------
// Alerts on a grid

/// An alert: a polygon (DETER) or a patch of a raster's alert pixels (made by `compare`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AlertUnit {
    /// the polygon's FID, or the patch's first pixel (row * width + column)
    pub id: i64,
    pub day: i32,
    /// index into AlertsOnGrid::classes
    pub class: u16,
    /// a polygon's own area (ha, in the grid's CRS); a patch's: its pixels'
    pub area_ha: f64,
}

/// Alerts placed on a map's grid, pixel by pixel.
#[derive(Clone, Debug, Default)]
pub struct AlertsOnGrid {
    pub grid: Grid,
    /// the alert's date at each pixel (NO_DAY: none)
    pub day: Vec<i32>,
    /// its class or confidence (index into `classes`)
    pub class: Vec<u16>,
    /// polygons: 1 + the index of the polygon holding the pixel in `units` (0 none); rasters: empty
    pub unit: Vec<u32>,
    /// the pixels the alerts' source covers (empty: every pixel)
    pub covered: Vec<bool>,
    /// classes (DETER's classnames, sorted) or confidences (from the lowest)
    pub classes: Vec<String>,
    /// the polygons (rasters: none, patches made by `compare`)
    pub units: Vec<AlertUnit>,
    /// polygons without a date, or with a geometry that could not be placed
    pub skipped: usize,
    /// what they are (for the report)
    pub source: String,
}

impl AlertsOnGrid {
    pub fn polygons(&self) -> bool {
        !self.unit.is_empty()
    }

    pub fn is_covered(&self, i: usize) -> bool {
        self.covered.is_empty() || self.covered[i]
    }

    /// The alerts' dates as decimal years (NaN none): their map.
    pub fn years(&self) -> Vec<f32> {
        self.day.iter().map(|&d| if d == NO_DAY { f32::NAN } else { decimal_year(d) as f32 }).collect()
    }

    /// The alerts' days in the window `from..=to` (the dates of the alerts of the window).
    pub fn days_in(&self, from: i32, to: i32) -> (i32, i32, usize) {
        let mut out = (i32::MAX, i32::MIN, 0);
        for &d in &self.day {
            if d != NO_DAY && d >= from && d <= to {
                out = (out.0.min(d), out.1.max(d), out.2 + 1);
            }
        }
        out
    }
}

fn to_grid_crs(layer_srs: Option<SpatialRef>, wkt: &str) -> Result<Option<CoordTransform>, String> {
    let (Some(mut src), false) = (layer_srs, wkt.is_empty()) else { return Ok(None) };
    let mut dst = SpatialRef::from_wkt(wkt).map_err(|e| format!("the map's CRS: {e}"))?;
    if src == dst {
        return Ok(None);
    }
    src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    CoordTransform::new(&src, &dst).map(Some).map_err(|e| format!("no transformation to the map's CRS ({e})"))
}

/// Burns `values` of the geometries (in the grid's CRS) on the grid, later ones over earlier ones, the pixels whose
/// centres they hold; 0 elsewhere.
pub fn burn(grid: &Grid, geoms: &[Geometry], values: &[f64]) -> Result<Vec<i32>, String> {
    let mem = gdal::DriverManager::get_driver_by_name("MEM").map_err(|e| e.to_string())?;
    let mut ds = mem.create_with_band_type::<i32, _>("", grid.w, grid.h, 1).map_err(|e| e.to_string())?;
    ds.set_geo_transform(&grid.gt).map_err(|e| e.to_string())?;
    if !grid.wkt.is_empty() {
        ds.set_projection(&grid.wkt).map_err(|e| e.to_string())?;
    }
    if !geoms.is_empty() {
        gdal::raster::rasterize(&mut ds, &[1], geoms, values, None).map_err(|e| format!("rasterizing: {e}"))?;
    }
    let b = ds.rasterband(1).map_err(|e| e.to_string())?;
    let buf = b.read_as::<i32>((0, 0), (grid.w, grid.h), (grid.w, grid.h), None).map_err(|e| e.to_string())?;
    Ok(buf.data().to_vec())
}

/// Polygons with a date (and a class) of a vector layer (`layer` empty: the first) placed on `grid`.
pub fn alerts_from_vector(
    path: &str,
    layer: &str,
    date_field: &str,
    class_field: &str,
    grid: &Grid,
) -> Result<AlertsOnGrid, String> {
    let ds = Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut l =
        if layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(layer) }.map_err(|e| format!("{path}: {e}"))?;
    let fields: Vec<String> = l.defn().fields().map(|f| f.name()).collect();
    let find = |name: &str| fields.iter().position(|f| f.eq_ignore_ascii_case(name));
    let di = find(date_field).ok_or(format!("{path}: no field {date_field}"))?;
    let ci = if class_field.is_empty() {
        None
    } else {
        Some(find(class_field).ok_or(format!("{path}: no field {class_field}"))?)
    };
    let ct = to_grid_crs(l.spatial_ref(), &grid.wkt)?;
    let mut units = Vec::new();
    let mut names = Vec::new();
    let mut geoms = Vec::new();
    let mut skipped = 0;
    for f in l.features() {
        let day = f.field_as_string(di).ok().flatten().and_then(|s| parse_day(&s));
        let Some(g) = f.geometry() else {
            skipped += 1;
            continue;
        };
        let g = match &ct {
            Some(ct) => match g.transform(ct) {
                Ok(g) => g,
                Err(_) => {
                    skipped += 1;
                    continue;
                }
            },
            None => g.clone(),
        };
        let g = linear(g);
        let Some(day) = day else {
            skipped += 1;
            continue;
        };
        let class = ci.and_then(|i| f.field_as_string(i).ok().flatten()).unwrap_or_default();
        let unit = row_areas(grid).1;
        let area = if unit == "ha" { g.area() * linear_factor(&grid.wkt).powi(2) / 1e4 } else { g.area() };
        units.push(AlertUnit {
            id: f.fid().map_or(units.len() as i64 + 1, |v| v as i64),
            day,
            class: 0,
            area_ha: area,
        });
        names.push(class);
        geoms.push(g);
    }
    // classes sorted; a unit's index into them
    let mut classes: Vec<String> = names.clone();
    classes.sort();
    classes.dedup();
    if classes.is_empty() {
        classes.push(String::new());
    }
    for (u, n) in units.iter_mut().zip(&names) {
        u.class = classes.iter().position(|c| c == n).unwrap_or(0) as u16;
    }
    // burned from the latest (and the last read) to the earliest: the earliest holds a pixel
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|&a, &b| units[b].day.cmp(&units[a].day).then(units[b].id.cmp(&units[a].id)));
    let og: Vec<Geometry> = order.iter().map(|&k| geoms[k].clone()).collect();
    let ov: Vec<f64> = order.iter().map(|&k| (k + 1) as f64).collect();
    let burned = burn(grid, &og, &ov)?;
    let n = grid.w * grid.h;
    let (mut day, mut class, mut unit) = (vec![NO_DAY; n], vec![0u16; n], vec![0u32; n]);
    for i in 0..n {
        let k = burned[i];
        if k > 0 {
            let u = &units[k as usize - 1];
            (day[i], class[i], unit[i]) = (u.day, u.class, k as u32);
        }
    }
    let classes = classes.into_iter().map(|c| if c.is_empty() { "(no class)".to_string() } else { c }).collect();
    Ok(AlertsOnGrid {
        grid: grid.clone(),
        day,
        class,
        unit,
        covered: vec![],
        classes,
        units,
        skipped,
        source: format!("{path} ({} polygons)", geoms.len()),
    })
}

/// A geometry with curves (GML's surfaces) as straight segments: GDAL burns no curve.
fn linear(g: Geometry) -> Geometry {
    // SAFETY: the geometry's handle, read only; the linear copy made here destroyed once.
    unsafe {
        let h = g.c_geometry();
        if gdal_sys::OGR_G_HasCurveGeometry(h, 0) == 0 {
            return g;
        }
        let l = gdal_sys::OGR_G_GetLinearGeometry(h, 0.0, std::ptr::null_mut());
        if l.is_null() {
            return g;
        }
        let mut wkb = vec![0u8; gdal_sys::OGR_G_WkbSize(l).max(0) as usize];
        let ok = gdal_sys::OGR_G_ExportToIsoWkb(l, gdal_sys::OGRwkbByteOrder::wkbNDR, wkb.as_mut_ptr());
        gdal_sys::OGR_G_DestroyGeometry(l);
        if ok != gdal_sys::OGRErr::OGRERR_NONE {
            return g;
        }
        Geometry::from_wkb(&wkb).unwrap_or(g)
    }
}

/// The CRS's linear unit in metres (1 without one).
fn linear_factor(wkt: &str) -> f64 {
    SpatialRef::from_wkt(wkt).ok().filter(|s| !s.is_geographic()).map_or(1.0, |s| s.linear_units())
}

// ---------------------------------------------------------------------------------------------------------------
// Alert rasters (GLAD, RADD): their encodings

/// A product of alert rasters and how it encodes its dates and confidences.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Product {
    /// UMD GLAD-L (Landsat), a year's pair of files: alertYY (2 probable, 3 confirmed) and alertDateYY (day of
    /// the year)
    #[default]
    GladL,
    /// UMD GLAD-S2 (Sentinel-2): alert (1 loss in the most recent observation only, 2 low, 3 medium, 4 high
    /// confidence) and alertDate (days since 2018-12-31)
    GladS2,
    /// RADD (Wageningen, Sentinel-1) as GFW serves it: one band, 2 or 3 (low, high confidence) then the days since
    /// 2014-12-31 (20000 + days, 30000 + days)
    Radd,
    /// GFW's integrated alerts: 2, 3 or 4 (low, high, highest) then the days since 2014-12-31
    Integrated,
}

impl Product {
    pub const ALL: [Product; 4] = [Product::GladL, Product::GladS2, Product::Radd, Product::Integrated];

    pub fn name(self) -> &'static str {
        match self {
            Product::GladL => "GLAD-L (Landsat, UMD)",
            Product::GladS2 => "GLAD-S2 (Sentinel-2, UMD)",
            Product::Radd => "RADD (Sentinel-1, Wageningen)",
            Product::Integrated => "Integrated alerts (GFW)",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Product::GladL => "glad-l",
            Product::GladS2 => "glad-s2",
            Product::Radd => "radd",
            Product::Integrated => "integrated",
        }
    }

    pub fn from_id(s: &str) -> Option<Product> {
        Product::ALL.into_iter().find(|p| p.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Its confidence classes, from the lowest.
    pub fn classes(self) -> &'static [&'static str] {
        match self {
            Product::GladL => &["probable", "confirmed"],
            Product::GladS2 => &["most recent observation only", "low", "medium", "high"],
            Product::Radd => &["low", "high"],
            Product::Integrated => &["low", "high", "highest"],
        }
    }

    /// Two files (confidence, date) per source: GLAD's; one band holding both: GFW's encodings.
    pub fn two_files(self) -> bool {
        matches!(self, Product::GladL | Product::GladS2)
    }

    /// A pixel's (day, class) from its values: `conf` (the alert band, or the encoded value) and `date` (the date
    /// band, NaN when the product has none); `year` the GLAD-L file's year. None: no alert.
    pub fn decode(self, conf: f32, date: f32, year: i32) -> Option<(i32, u16)> {
        if !conf.is_finite() || conf <= 0.0 {
            return None;
        }
        match self {
            Product::GladL => {
                let c = conf as i32;
                let d = date as i32;
                if !(2..=3).contains(&c) || !date.is_finite() || d < 1 || d > year_len(year) {
                    return None;
                }
                Some((days_from_civil(year, 1, 1) + d - 1, (c - 2) as u16))
            }
            Product::GladS2 => {
                let c = conf as i32;
                let d = date as i32;
                if !(1..=4).contains(&c) || !date.is_finite() || d < 1 {
                    return None;
                }
                Some((days_from_civil(2018, 12, 31) + d, (c - 1) as u16))
            }
            Product::Radd | Product::Integrated => {
                let v = conf as i32;
                let (c, d) = (v / 10000, v % 10000);
                let lo = 2;
                let hi = if self == Product::Radd { 3 } else { 4 };
                if !(lo..=hi).contains(&c) || d < 1 {
                    return None;
                }
                Some((days_from_civil(2014, 12, 31) + d, (c - lo) as u16))
            }
        }
    }
}

/// What a product is and where its rasters are (its producer's documentation).
pub fn product_help(p: Product) -> &'static str {
    match p {
        Product::GladL => {
            "UMD's GLAD Landsat alerts (Hansen et al. 2016), 0.00025 degree, 10-degree tiles of 30N-30S updated daily:\n\
             alertYY 2 probable, 3 confirmed loss; alertDateYY the day of the year of the first loss observation.\n\
             Keyless: the bucket earthenginepartners-hansen (GLADalert/C2/current, 2021 on)"
        }
        Product::GladS2 => {
            "UMD's GLAD Sentinel-2 alerts, 0.0001 degree, the Amazon basin's humid forest since\n\
             2019: alert 1 loss in the most recent observation only, 2 low, 3 medium, 4 high confidence; alertDate\n\
             the days since 2018-12-31. Keyless: the bucket earthenginepartners-hansen (S2alert)"
        }
        Product::Radd => {
            "Wageningen's RADD Sentinel-1 alerts (Reiche et al. 2021) as GFW encodes them (date_conf): 2 low, 3 high\n\
             confidence, then the days since 2014-12-31 (30055: high, 2015-02-24). Its GeoTIFFs need a GFW API key:\n\
             open them from disk, or read GFW's public map tiles (zoom 14, about 10 m) for the map's area"
        }
        Product::Integrated => {
            "GFW's integrated deforestation alerts (GLAD-L, GLAD-S2, RADD): 2 low, 3 high, 4 highest (several systems)\n\
             confidence, then the days since 2014-12-31 of the earliest detection. GeoTIFFs need a GFW API key: open\n\
             them from disk, or read GFW's public map tiles (zoom 14, about 10 m) for the map's area"
        }
    }
}

/// UMD's bucket of GLAD alerts.
pub const GLAD_BUCKET: &str = "https://storage.googleapis.com/earthenginepartners-hansen";
/// GFW's map tiles (keyless): date and confidence in their colours.
pub const GFW_TILES: &str = "https://tiles.globalforestwatch.org";
/// The zoom of GFW's tiles read (their native one, about 10 m).
pub const GFW_ZOOM: u32 = 14;
/// GFW's tiles read at most for an area.
pub const GFW_MAX_TILES: usize = 400;

/// How a product is read for an area without a file (a hover text), or why it cannot be.
pub fn remote_supported(p: Product) -> Result<String, String> {
    Ok(match p {
        Product::GladL => {
            "UMD's current GeoTIFFs (2021 on) read over the network for the map's area: no download".into()
        }
        Product::GladS2 => "UMD's GeoTIFFs read over the network for the map's area: no download".into(),
        Product::Radd | Product::Integrated => format!(
            "GFW's public map tiles of the map's area (zoom {GFW_ZOOM}, at most {GFW_MAX_TILES}), decoded from their \
             colours: the GeoTIFFs need a GFW API key"
        ),
    })
}

/// GLAD's 10-degree tile of a point: "070W_20S_060W_10S" (xmin_ymin_xmax_ymax).
pub fn glad_tile(lon: f64, lat: f64) -> String {
    let x0 = ((lon / 10.0).floor() * 10.0) as i32;
    let y0 = ((lat / 10.0).floor() * 10.0) as i32;
    let ew = |v: i32| format!("{:03}{}", v.abs(), if v < 0 { 'W' } else { 'E' });
    let ns = |v: i32| format!("{:02}{}", v.abs(), if v < 0 { 'S' } else { 'N' });
    format!("{}_{}_{}_{}", ew(x0), ns(y0), ew(x0 + 10), ns(y0 + 10))
}

/// GLAD's tiles of a box.
pub fn glad_tiles(b: [f64; 4]) -> Vec<String> {
    let mut out = Vec::new();
    let mut lat = (b[1] / 10.0).floor() * 10.0 + 5.0;
    while lat - 5.0 < b[3] {
        let mut lon = (b[0] / 10.0).floor() * 10.0 + 5.0;
        while lon - 5.0 < b[2] {
            out.push(glad_tile(lon, lat));
            lon += 10.0;
        }
        lat += 10.0;
    }
    out
}

/// Web Mercator tiles of a box at zoom `z`: (x0, y0, x1, y1), inclusive.
pub fn web_tiles(b: [f64; 4], z: u32) -> [u32; 4] {
    let n = f64::from(1u32 << z);
    let tx = |lon: f64| ((lon + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0) as u32;
    let ty = |lat: f64| {
        let r = lat.clamp(-85.05, 85.05).to_radians();
        ((1.0 - r.tan().asinh() / std::f64::consts::PI) / 2.0 * n).floor().clamp(0.0, n - 1.0) as u32
    };
    [tx(b[0]), ty(b[3]), tx(b[2]), ty(b[1])]
}

/// The rasters of a product for a box (longitude, latitude) and a window of days: GLAD's read over the network
/// (/vsicurl/), GFW's tiles as a source alerts_from_rasters reads ("gfw-tiles:DATASET:Z:X0:Y0:X1:Y1").
pub fn remote_files(p: Product, b: [f64; 4], from: i32, to: i32) -> Result<Vec<RasterFile>, String> {
    if b[1] > 30.0 || b[3] < -30.0 {
        return Err("GLAD's and GFW's alerts cover the tropics (30N to 30S) only".into());
    }
    let url = |rest: String| format!("/vsicurl/{GLAD_BUCKET}/{rest}");
    match p {
        Product::GladL => {
            let today = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
                / 86400) as i32;
            let (y0, _, _) = civil_from_days(from.max(days_from_civil(2021, 1, 1)));
            let (y1, _, _) = civil_from_days(to.min(today));
            let mut out = Vec::new();
            for y in y0..=y1 {
                for t in glad_tiles(b) {
                    let yy = y % 100;
                    out.push(RasterFile {
                        conf: url(format!("GLADalert/C2/current/alert{yy:02}_{t}.tif")),
                        date: url(format!("GLADalert/C2/current/alertDate{yy:02}_{t}.tif")),
                        year: y,
                    });
                }
            }
            if out.is_empty() {
                return Err("UMD's current GLAD-L files start in 2021".into());
            }
            Ok(out)
        }
        Product::GladS2 => Ok(glad_tiles(b)
            .into_iter()
            .map(|t| RasterFile {
                conf: url(format!("S2alert/alert/{t}.tif")),
                date: url(format!("S2alert/alertDate/{t}.tif")),
                year: 0,
            })
            .collect()),
        Product::Radd | Product::Integrated => {
            let t = web_tiles(b, GFW_ZOOM);
            let n = (t[2] - t[0] + 1) as usize * (t[3] - t[1] + 1) as usize;
            if n > GFW_MAX_TILES {
                return Err(format!(
                    "the map's area needs {n} of GFW's tiles (at most {GFW_MAX_TILES}): open its GeoTIFFs instead"
                ));
            }
            let ds = if p == Product::Radd { "wur_radd_alerts" } else { "gfw_integrated_alerts" };
            Ok(vec![RasterFile {
                conf: format!("gfw-tiles:{ds}:{GFW_ZOOM}:{}:{}:{}:{}", t[0], t[1], t[2], t[3]),
                date: String::new(),
                year: 0,
            }])
        }
    }
}

/// A GFW tile's pixel (RGBA; A 255 when the tile has no alpha) as GFW's date_conf value (0: no alert): the day
/// since 2014-12-31 is R * 255 + G, the confidence B / 100 - 1 (0 low, 1 high) and, for the integrated alerts,
/// A >> 2 (1 low, 2 high, 4 highest): gfw-data-api's symbology.py (date_conf_rgb_calc).
pub fn gfw_pixel(rgba: [u8; 4], integrated: bool) -> u16 {
    let day = u32::from(rgba[0]) * 255 + u32::from(rgba[1]);
    if day == 0 || day >= 10000 {
        return 0;
    }
    let a = rgba[3] >> 2;
    let c = if integrated && rgba[3] != 255 && a != 0 {
        if a & 4 != 0 {
            4
        } else if a & 2 != 0 {
            3
        } else {
            2
        }
    } else {
        2 + (u32::from(rgba[2]) / 100).saturating_sub(1).min(1)
    };
    (c * 10000 + day) as u16
}

/// GFW's tiles "gfw-tiles:DATASET:Z:X0:Y0:X1:Y1" read and decoded into a GeoTIFF of date_conf values in Web
/// Mercator (`out`); a tile the server has not (404) holds no alert.
pub fn gfw_mosaic(spec: &str, out: &Path, progress: &dyn Fn(usize, usize)) -> Result<(), String> {
    let p: Vec<&str> = spec.split(':').collect();
    let n = |k: usize| p.get(k).and_then(|v| v.parse::<u32>().ok()).ok_or(format!("{spec}: not GFW's tiles"));
    let (ds, z, x0, y0, x1, y1) = (p.get(1).copied().unwrap_or(""), n(2)?, n(3)?, n(4)?, n(5)?, n(6)?);
    let integrated = ds == "gfw_integrated_alerts";
    let (tw, th) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let w = tw * 256;
    let mut v = vec![0f32; w * th * 256];
    for ty in 0..th {
        for tx in 0..tw {
            progress(ty * tw + tx, tw * th);
            let url = format!("{GFW_TILES}/{ds}/latest/default/{z}/{}/{}.png", x0 + tx as u32, y0 + ty as u32);
            let bytes = match janus_geo::ogc::get(&url, false) {
                Ok(b) => b,
                Err(e) if e.contains("404") => continue,
                Err(e) => return Err(e),
            };
            let name = format!("/vsimem/janus_gfw_{}_{tx}_{ty}.png", std::process::id());
            gdal::vsi::create_mem_file(&name, bytes).map_err(|e| e.to_string())?;
            let read = || -> Result<Vec<[u8; 4]>, String> {
                let d = Dataset::open(&name).map_err(|e| format!("{url}: {e}"))?;
                if d.raster_size() != (256, 256) {
                    return Err(format!("{url}: not a tile of 256 x 256"));
                }
                let mut px = vec![[0u8, 0, 0, 255]; 256 * 256];
                for b in 0..d.raster_count().min(4) {
                    let band = d.rasterband(b + 1).map_err(|e| e.to_string())?;
                    let buf = band.read_as::<u8>((0, 0), (256, 256), (256, 256), None).map_err(|e| e.to_string())?;
                    for (q, &x) in px.iter_mut().zip(buf.data()) {
                        q[b] = x;
                    }
                }
                Ok(px)
            };
            let r = read();
            let _ = gdal::vsi::unlink_mem_file(&name);
            for (k, q) in r?.iter().enumerate() {
                let (x, y) = (tx * 256 + k % 256, ty * 256 + k / 256);
                v[y * w + x] = f32::from(gfw_pixel(*q, integrated));
            }
        }
    }
    let e = 20037508.342789244;
    let t = 2.0 * e / f64::from(1u32 << z);
    let grid = Grid {
        w,
        h: th * 256,
        gt: [-e + f64::from(x0) * t, t / 256.0, 0.0, e - f64::from(y0) * t, 0.0, -t / 256.0],
        has_gt: true,
        wkt: SpatialRef::from_epsg(3857).map_err(|e| e.to_string())?.to_wkt().map_err(|e| e.to_string())?,
    };
    write_f32(&out.to_string_lossy(), &grid, &v)
}

/// GFW's tiles of a source as a GeoTIFF of the day (kept in the temporary folder), or the source's own path.
fn resolve(conf: &str) -> Result<String, String> {
    if !conf.starts_with("gfw-tiles:") {
        return Ok(conf.to_string());
    }
    let day = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) / 86400;
    let name: String = conf.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    let dir = std::env::temp_dir().join("janus_alerts");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let p = dir.join(format!("{name}_{day}.tif"));
    if !p.exists() {
        let part = dir.join(format!("{name}_{day}.part.tif"));
        gfw_mosaic(conf, &part, &|_, _| {})?;
        std::fs::rename(&part, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(p.to_string_lossy().into_owned())
}

/// A source of alert rasters: the confidence (or encoded) file, its date file (GLAD's) and the year (GLAD-L's).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RasterFile {
    pub conf: String,
    pub date: String,
    pub year: i32,
}

/// Alert rasters warped onto `grid` (nearest pixel); later files over earlier ones where both have an alert, the
/// earliest alert kept. `progress` gets the file being read.
pub fn alerts_from_rasters(
    product: Product,
    files: &[RasterFile],
    grid: &Grid,
    threads: usize,
    progress: &dyn Fn(usize),
) -> Result<AlertsOnGrid, String> {
    let n = grid.w * grid.h;
    let (mut day, mut class, mut covered) = (vec![NO_DAY; n], vec![0u16; n], vec![false; n]);
    let mut conf_row = vec![0f32; grid.w];
    let mut date_row = vec![f32::NAN; grid.w];
    janus_geo::ogc::http_config();
    let _ = gdal::config::set_thread_local_config_option("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR");
    for (k, f) in files.iter().enumerate() {
        progress(k);
        let c = MapReader::open_exact(&resolve(&f.conf)?, 1, Some(grid), threads)?;
        let d = if product.two_files() {
            if f.date.is_empty() {
                return Err(format!("{}: {} needs its file of dates", f.conf, product.name()));
            }
            Some(MapReader::open_exact(&f.date, 1, Some(grid), threads)?)
        } else {
            None
        };
        for y in 0..grid.h {
            c.rows(y, 1, &mut conf_row)?;
            if let Some(d) = &d {
                d.rows(y, 1, &mut date_row)?;
            }
            for x in 0..grid.w {
                let i = y * grid.w + x;
                let v = conf_row[x];
                if v.is_nan() {
                    continue;
                }
                covered[i] = true;
                if let Some((dd, cc)) = product.decode(v, date_row[x], f.year)
                    && (day[i] == NO_DAY || dd < day[i])
                {
                    (day[i], class[i]) = (dd, cc);
                }
            }
        }
    }
    let names: Vec<String> = files.iter().map(|f| file_name(&f.conf)).collect();
    Ok(AlertsOnGrid {
        grid: grid.clone(),
        day,
        class,
        unit: vec![],
        covered,
        classes: product.classes().iter().map(|s| s.to_string()).collect(),
        units: vec![],
        skipped: 0,
        source: format!("{} ({})", product.name(), names.join(", ")),
    })
}

fn file_name(p: &str) -> String {
    Path::new(p).file_name().map_or(p.to_string(), |f| f.to_string_lossy().into_owned())
}

// ---------------------------------------------------------------------------------------------------------------
// The map of change

/// A map of change dates (band 1, decimal years) and its grid.
pub fn read_change_map(path: &str, threads: usize) -> Result<(Grid, Vec<f32>), String> {
    let grid = read_grid(path)?;
    if !grid.has_gt {
        return Err(format!("{path}: the map has no geotransform"));
    }
    let r = MapReader::open(path, 1, None, threads)?;
    let mut v = vec![0f32; grid.w * grid.h];
    r.rows(0, grid.h, &mut v)?;
    Ok((grid, v))
}

/// How a map's values give its dates: a Zeit run's maps are in decimal years; a map from elsewhere may be in
/// year and day of the year, YYYYMMDD or days since 1970.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DateCode {
    /// the encoding its largest value suggests (`detect`)
    #[default]
    Auto,
    /// 2019.5 (whole years too: 2019)
    DecimalYear,
    /// 2019183: the year times 1000 plus the day of the year
    YearDay,
    /// 20190702
    Ymd,
    /// days since 1970-01-01
    Days1970,
}

impl DateCode {
    pub const ALL: [DateCode; 5] =
        [DateCode::Auto, DateCode::DecimalYear, DateCode::YearDay, DateCode::Ymd, DateCode::Days1970];

    pub fn name(self) -> &'static str {
        match self {
            DateCode::Auto => "detected",
            DateCode::DecimalYear => "decimal years (2019.5, 2019)",
            DateCode::YearDay => "year and day (2019183)",
            DateCode::Ymd => "YYYYMMDD (20190702)",
            DateCode::Days1970 => "days since 1970-01-01",
        }
    }

    /// The encoding of a map whose largest date is `max`: up to 3000 years, 1000001-3000366 a year and day,
    /// 10000101-30001231 YYYYMMDD, else days since 1970.
    pub fn detect(max: f64) -> DateCode {
        if max <= 3000.0 {
            DateCode::DecimalYear
        } else if (1_000_001.0..=3_000_366.0).contains(&max) {
            DateCode::YearDay
        } else if (10_000_101.0..=30_001_231.0).contains(&max) {
            DateCode::Ymd
        } else {
            DateCode::Days1970
        }
    }

    /// A value as a decimal year; NaN when it is no date (not finite, <= 0, no such day). Auto: as decimal years.
    pub fn decimal(self, v: f64) -> f64 {
        if !v.is_finite() || v <= 0.0 {
            return f64::NAN;
        }
        let day = match self {
            DateCode::Auto | DateCode::DecimalYear => return v,
            DateCode::YearDay => {
                let v = v.round() as i64;
                let (y, d) = ((v / 1000) as i32, (v % 1000) as i32);
                (d >= 1 && d <= year_len(y)).then(|| days_from_civil(y, 1, 1) + d - 1)
            }
            DateCode::Ymd => {
                let v = v.round() as i64;
                let (y, m, d) = ((v / 10_000) as i32, (v / 100 % 100) as u32, (v % 100) as u32);
                let ok = (1..=12).contains(&m) && d >= 1 && civil_from_days(days_from_civil(y, m, d)) == (y, m, d);
                ok.then(|| days_from_civil(y, m, d))
            }
            DateCode::Days1970 => Some(v.round() as i32),
        };
        day.map_or(f64::NAN, decimal_year)
    }
}

/// A map of change dates (band 1) in decimal years, its values read as `code` says (Auto: detected from its
/// largest value), and the encoding used.
pub fn read_change_map_as(path: &str, code: DateCode, threads: usize) -> Result<(Grid, Vec<f32>, DateCode), String> {
    let code = match code {
        DateCode::Auto => {
            let ds = crate::reader::open_raster(path, threads)?;
            let band = ds.rasterband(1).map_err(|e| format!("{path}: {e}"))?;
            band.compute_raster_min_max(true).map_or(DateCode::DecimalYear, |m| DateCode::detect(m.max))
        }
        c => c,
    };
    if code == DateCode::DecimalYear {
        let (g, v) = read_change_map(path, threads)?;
        return Ok((g, v, code));
    }
    // the values exactly (YYYYMMDD is past float's whole numbers), a band of rows at a time
    let grid = read_grid(path)?;
    if !grid.has_gt {
        return Err(format!("{path}: the map has no geotransform"));
    }
    let ds = crate::reader::open_raster(path, threads)?;
    let band = ds.rasterband(1).map_err(|e| format!("{path}: {e}"))?;
    let nodata = band.no_data_value().filter(|v| !v.is_nan());
    let mut out = vec![f32::NAN; grid.w * grid.h];
    let mut buf = vec![0f64; grid.w * 256];
    for y0 in (0..grid.h).step_by(256) {
        let n = 256.min(grid.h - y0);
        let b = &mut buf[..grid.w * n];
        band.read_into_slice::<f64>((0, y0 as isize), (grid.w, n), (grid.w, n), b, None).map_err(|e| e.to_string())?;
        for (o, &v) in out[y0 * grid.w..(y0 + n) * grid.w].iter_mut().zip(b.iter()) {
            if Some(v) != nodata {
                *o = code.decimal(v) as f32;
            }
        }
    }
    Ok((grid, out, code))
}

/// The grid's box in longitude and latitude (west, south, east, north), its edges densified.
pub fn lonlat_bounds(grid: &Grid) -> Result<[f64; 4], String> {
    if grid.wkt.is_empty() || !grid.has_gt {
        return Err("the map has no CRS".into());
    }
    let mut src = SpatialRef::from_wkt(&grid.wkt).map_err(|e| e.to_string())?;
    let mut dst = SpatialRef::from_epsg(4326).map_err(|e| format!("EPSG:4326 is not known to PROJ ({e})"))?;
    src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let ct = CoordTransform::new(&src, &dst).map_err(|_| "no transformation to longitude and latitude")?;
    let g = &grid.gt;
    let (w, h) = (grid.w as f64, grid.h as f64);
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for i in 0..=16 {
        let f = f64::from(i) / 16.0;
        for (px, py) in [(0.0, f * h), (w, f * h), (f * w, 0.0), (f * w, h)] {
            let (mut x, mut y) = ([g[0] + px * g[1] + py * g[2]], [g[3] + px * g[4] + py * g[5]]);
            if ct.transform_coords(&mut x, &mut y, &mut []).is_ok() && x[0].is_finite() && y[0].is_finite() {
                b = [b[0].min(x[0]), b[1].min(y[0]), b[2].max(x[0]), b[3].max(y[0])];
            }
        }
    }
    if b[2] <= b[0] || b[3] <= b[1] {
        return Err("the map could not be placed in longitude and latitude".into());
    }
    Ok(b)
}

// ---------------------------------------------------------------------------------------------------------------
// The comparison

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CompareOptions {
    /// the window of dates (days since 1970, inclusive)
    pub from: i32,
    pub to: i32,
    /// the lags' histogram's bins (days)
    pub bin_days: i32,
}

impl Default for CompareOptions {
    fn default() -> Self {
        CompareOptions { from: i32::MIN + 1, to: i32::MAX, bin_days: 30 }
    }
}

/// Quantiles and moments of the lags (days; NaN when there are none).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LagStats {
    pub n: usize,
    pub mean: f64,
    pub min: f64,
    pub q10: f64,
    pub q25: f64,
    pub median: f64,
    pub q75: f64,
    pub q90: f64,
    pub max: f64,
    /// share of the lags within 30, 90 and 365 days either way
    pub within_30: f64,
    pub within_90: f64,
    pub within_365: f64,
}

/// numpy's quantile (linear) of sorted values.
pub fn quantile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p;
    let (lo, hi) = (h.floor() as usize, h.ceil() as usize);
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

fn lag_stats(lags: &mut [f64]) -> LagStats {
    lags.sort_by(f64::total_cmp);
    let n = lags.len();
    if n == 0 {
        let nan = f64::NAN;
        return LagStats {
            n,
            mean: nan,
            min: nan,
            q10: nan,
            q25: nan,
            median: nan,
            q75: nan,
            q90: nan,
            max: nan,
            within_30: nan,
            within_90: nan,
            within_365: nan,
        };
    }
    let within = |d: f64| lags.iter().filter(|v| v.abs() <= d).count() as f64 / n as f64;
    LagStats {
        n,
        mean: lags.iter().sum::<f64>() / n as f64,
        min: lags[0],
        q10: quantile(lags, 0.1),
        q25: quantile(lags, 0.25),
        median: quantile(lags, 0.5),
        q75: quantile(lags, 0.75),
        q90: quantile(lags, 0.9),
        max: lags[n - 1],
        within_30: within(30.0),
        within_90: within(90.0),
        within_365: within(365.0),
    }
}

/// An alert compared: a polygon or a raster's patch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UnitResult {
    pub unit: AlertUnit,
    /// its pixels on the grid (in the alerts' cover) and those the map changed
    pub pixels: usize,
    pub detected_pixels: usize,
    pub ha: f64,
    pub detected_ha: f64,
    /// the median of its pixels' lags (days; NaN: not detected)
    pub lag_median: f64,
}

impl UnitResult {
    pub fn detected(&self) -> bool {
        self.detected_pixels > 0
    }
}

/// A class's (or confidence's) numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassRow {
    pub name: String,
    pub alerts: usize,
    pub detected: usize,
    pub ha: f64,
    pub detected_ha: f64,
    pub lag_median: f64,
}

/// The agreement map's values (0: neither, or outside the alerts' cover).
pub const BOTH: u8 = 1;
pub const ONLY_MAP: u8 = 2;
pub const ONLY_ALERTS: u8 = 3;
pub const AGREEMENT_CLASSES: [&str; 3] = ["Both", "Only the map", "Only the alerts"];

#[derive(Clone, Debug, Default)]
pub struct Comparison {
    pub from: i32,
    pub to: i32,
    /// "ha", or "pixels" on a grid without a CRS
    pub unit: String,
    pub covered_ha: f64,
    pub covered_pixels: usize,
    /// pixels and areas: both, only the map, only the alerts
    pub both: usize,
    pub only_map: usize,
    pub only_alerts: usize,
    pub both_ha: f64,
    pub only_map_ha: f64,
    pub only_alerts_ha: f64,
    /// alerts (polygons or patches) of the window on the grid, those the map detects; polygons of the window too
    /// small to hold a pixel's centre
    pub alerts: usize,
    pub alerts_detected: usize,
    pub alerts_too_small: usize,
    /// the map's patches of change of the window, those an alert confirms
    pub map_patches: usize,
    pub map_patches_confirmed: usize,
    /// the map's dates: whole years only (LandTrendr's), their lags then from 1 January
    pub map_years_only: bool,
    pub lag: LagStats,
    /// (bin's start, its end, count): the map's date minus the alert's, days
    pub histogram: Vec<(i32, i32, usize)>,
    pub units: Vec<UnitResult>,
    pub classes: Vec<ClassRow>,
    /// BOTH, ONLY_MAP, ONLY_ALERTS, 0 elsewhere
    pub agreement: Vec<u8>,
    pub seconds: f64,
}

impl Comparison {
    pub fn map_ha(&self) -> f64 {
        self.both_ha + self.only_map_ha
    }

    pub fn alerts_ha(&self) -> f64 {
        self.both_ha + self.only_alerts_ha
    }

    /// The share of the alerts' area the map detects (NaN: no alerts).
    pub fn alerts_detected_share(&self) -> f64 {
        self.both_ha / self.alerts_ha()
    }

    /// The share of the map's change the alerts confirm.
    pub fn map_confirmed_share(&self) -> f64 {
        self.both_ha / self.map_ha()
    }

    /// Dice's coefficient (F1) of the two areas.
    pub fn dice(&self) -> f64 {
        2.0 * self.both_ha / (self.map_ha() + self.alerts_ha())
    }

    pub fn alerts_detected_count_share(&self) -> f64 {
        self.alerts_detected as f64 / self.alerts as f64
    }

    pub fn map_confirmed_count_share(&self) -> f64 {
        self.map_patches_confirmed as f64 / self.map_patches as f64
    }

    /// The alerts detected's share of the alerts' own area (polygons: their pixels').
    pub fn alerts_detected_area_share(&self) -> f64 {
        let (a, d) =
            self.units.iter().fold((0.0, 0.0), |(a, d), u| (a + u.ha, d + if u.detected() { u.ha } else { 0.0 }));
        d / a
    }
}

/// 8-connected patches of `mask`: each pixel's patch (0 none, else 1 + the patch's number, numbered in the order
/// their first pixels come in rows), and the patches' first pixels.
pub fn patches(mask: &[bool], w: usize, h: usize) -> (Vec<u32>, Vec<usize>) {
    fn find(p: &mut [u32], mut a: u32) -> u32 {
        while p[a as usize] != a {
            p[a as usize] = p[p[a as usize] as usize];
            a = p[a as usize];
        }
        a
    }
    let mut lab = vec![0u32; w * h];
    let mut parent: Vec<u32> = vec![0];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !mask[i] {
                continue;
            }
            let mut nb = [0u32; 4];
            if x > 0 {
                nb[0] = lab[i - 1];
            }
            if y > 0 {
                let r = i - w;
                if x > 0 {
                    nb[1] = lab[r - 1];
                }
                nb[2] = lab[r];
                if x + 1 < w {
                    nb[3] = lab[r + 1];
                }
            }
            let mut l = 0;
            for &n in nb.iter().filter(|&&n| n > 0) {
                if l == 0 {
                    l = find(&mut parent, n);
                } else {
                    let (a, b) = (find(&mut parent, l), find(&mut parent, n));
                    if a != b {
                        let (lo, hi) = (a.min(b), a.max(b));
                        parent[hi as usize] = lo;
                        l = lo;
                    }
                }
            }
            if l == 0 {
                l = parent.len() as u32;
                parent.push(l);
            }
            lab[i] = l;
        }
    }
    // numbered by their first pixels
    let mut number = vec![0u32; parent.len()];
    let mut first = Vec::new();
    for i in 0..w * h {
        if lab[i] == 0 {
            continue;
        }
        let r = find(&mut parent, lab[i]) as usize;
        if number[r] == 0 {
            first.push(i);
            number[r] = first.len() as u32;
        }
        lab[i] = number[r];
    }
    (lab, first)
}

/// The map of change (decimal years on `alerts.grid`) against the alerts.
pub fn compare(change: &[f32], alerts: &AlertsOnGrid, o: &CompareOptions) -> Comparison {
    let t0 = std::time::Instant::now();
    let g = &alerts.grid;
    let (w, h, n) = (g.w, g.h, g.w * g.h);
    let (areas, unit) = row_areas(g);
    let mut c = Comparison { from: o.from, to: o.to, unit, agreement: vec![0; n], ..Default::default() };
    let in_window = |d: i32| d != NO_DAY && d >= o.from && d <= o.to;
    let map_day: Vec<i32> = change
        .iter()
        .map(|&v| {
            let d = day_of_decimal_year(f64::from(v));
            d.filter(|&d| in_window(d)).unwrap_or(NO_DAY)
        })
        .collect();
    c.map_years_only = change.iter().any(|v| v.is_finite() && *v > 0.0)
        && change.iter().filter(|v| v.is_finite() && **v > 0.0).all(|v| v.fract() == 0.0);
    let mut lags = Vec::new();
    let mut map_mask = vec![false; n];
    let mut alert_mask = vec![false; n];
    for i in 0..n {
        if !alerts.is_covered(i) {
            continue;
        }
        let a = areas[i / w];
        c.covered_pixels += 1;
        c.covered_ha += a;
        let m = map_day[i] != NO_DAY;
        let al = in_window(alerts.day[i]);
        (map_mask[i], alert_mask[i]) = (m, al);
        match (m, al) {
            (true, true) => {
                c.both += 1;
                c.both_ha += a;
                c.agreement[i] = BOTH;
                lags.push(f64::from(map_day[i] - alerts.day[i]));
            }
            (true, false) => {
                c.only_map += 1;
                c.only_map_ha += a;
                c.agreement[i] = ONLY_MAP;
            }
            (false, true) => {
                c.only_alerts += 1;
                c.only_alerts_ha += a;
                c.agreement[i] = ONLY_ALERTS;
            }
            _ => {}
        }
    }
    // the histogram of the lags
    let b = o.bin_days.max(1);
    let mut bins: BTreeMap<i32, usize> = BTreeMap::new();
    for &l in &lags {
        *bins.entry((l as i32).div_euclid(b)).or_default() += 1;
    }
    c.histogram = bins.into_iter().map(|(k, v)| (k * b, (k + 1) * b, v)).collect();
    c.lag = lag_stats(&mut lags);
    // the map's patches, confirmed by an alert
    let (mp, mfirst) = patches(&map_mask, w, h);
    c.map_patches = mfirst.len();
    let mut confirmed = vec![false; mfirst.len()];
    for i in 0..n {
        if mp[i] > 0 && alert_mask[i] {
            confirmed[mp[i] as usize - 1] = true;
        }
    }
    c.map_patches_confirmed = confirmed.iter().filter(|&&v| v).count();
    // the alerts: polygons, or the raster's patches
    let (unit_of, mut units): (Vec<u32>, Vec<AlertUnit>) = if alerts.polygons() {
        let u: Vec<u32> = (0..n).map(|i| if alert_mask[i] { alerts.unit[i] } else { 0 }).collect();
        (u, alerts.units.clone())
    } else {
        let (lab, first) = patches(&alert_mask, w, h);
        let mut units: Vec<AlertUnit> =
            first.iter().map(|&f| AlertUnit { id: f as i64, day: i32::MAX, class: 0, area_ha: 0.0 }).collect();
        for i in 0..n {
            if lab[i] > 0 {
                let u = &mut units[lab[i] as usize - 1];
                u.day = u.day.min(alerts.day[i]);
                u.class = u.class.max(alerts.class[i]);
                u.area_ha += areas[i / w];
            }
        }
        (lab, units)
    };
    let mut res: Vec<UnitResult> = units.drain(..).map(|unit| UnitResult { unit, ..Default::default() }).collect();
    let mut unit_lags: Vec<Vec<f64>> = vec![vec![]; res.len()];
    let mut class_lags: Vec<Vec<f64>> = vec![vec![]; alerts.classes.len()];
    for i in 0..n {
        let k = unit_of[i];
        if k == 0 {
            continue;
        }
        let a = areas[i / w];
        let r = &mut res[k as usize - 1];
        r.pixels += 1;
        r.ha += a;
        if map_mask[i] {
            r.detected_pixels += 1;
            r.detected_ha += a;
            let l = f64::from(map_day[i] - alerts.day[i]);
            unit_lags[k as usize - 1].push(l);
            if let Some(v) = class_lags.get_mut(r.unit.class as usize) {
                v.push(l);
            }
        }
    }
    for (r, l) in res.iter_mut().zip(unit_lags.iter_mut()) {
        l.sort_by(f64::total_cmp);
        r.lag_median = quantile(l, 0.5);
    }
    // polygons of the window that hold no pixel: smaller than a pixel, or under earlier alerts
    if alerts.polygons() {
        c.alerts_too_small = res.iter().filter(|r| in_window(r.unit.day) && r.pixels == 0).count();
    }
    res.retain(|r| r.pixels > 0);
    c.alerts = res.len();
    c.alerts_detected = res.iter().filter(|r| r.detected()).count();
    // by class
    c.classes = alerts
        .classes
        .iter()
        .enumerate()
        .map(|(k, name)| {
            let mine: Vec<&UnitResult> = res.iter().filter(|r| r.unit.class as usize == k).collect();
            let l = &mut class_lags[k];
            l.sort_by(f64::total_cmp);
            ClassRow {
                name: name.clone(),
                alerts: mine.len(),
                detected: mine.iter().filter(|r| r.detected()).count(),
                ha: mine.iter().map(|r| r.ha).sum(),
                detected_ha: mine.iter().map(|r| r.detected_ha).sum(),
                lag_median: quantile(l, 0.5),
            }
        })
        .collect();
    c.units = res;
    c.seconds = t0.elapsed().as_secs_f64();
    c
}

// ---------------------------------------------------------------------------------------------------------------
// Outputs

/// Writes a band of `values` on `grid` as a GeoTIFF (Byte with nodata 0, or Float32 with NaN).
pub fn write_u8(path: &str, grid: &Grid, values: &[u8], classes: &[&str]) -> Result<(), String> {
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(path);
    let opts = gdal::raster::RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "TILED=YES"]);
    let mut ds = drv
        .create_with_band_type_with_options::<u8, _>(path, grid.w, grid.h, 1, &opts)
        .map_err(|e| format!("{path}: {e}"))?;
    ds.set_geo_transform(&grid.gt).map_err(|e| e.to_string())?;
    if !grid.wkt.is_empty() {
        ds.set_projection(&grid.wkt).map_err(|e| e.to_string())?;
    }
    let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
    b.set_no_data_value(Some(0.0)).map_err(|e| e.to_string())?;
    let mut buf = gdal::raster::Buffer::new((grid.w, grid.h), values.to_vec());
    b.write((0, 0), (grid.w, grid.h), &mut buf).map_err(|e| format!("{path}: {e}"))?;
    if !classes.is_empty() {
        let mut names = vec![String::new()];
        names.extend(classes.iter().map(|s| s.to_string()));
        let names: Vec<std::ffi::CString> =
            names.iter().map(|s| std::ffi::CString::new(s.as_str()).unwrap_or_default()).collect();
        let mut p: Vec<*mut std::ffi::c_char> = names.iter().map(|s| s.as_ptr() as *mut _).collect();
        p.push(std::ptr::null_mut());
        // SAFETY: a band of the open dataset; the list is copied by GDAL.
        unsafe { gdal_sys::GDALSetRasterCategoryNames(b.c_rasterband(), p.as_mut_ptr()) };
    }
    Ok(())
}

pub fn write_f32(path: &str, grid: &Grid, values: &[f32]) -> Result<(), String> {
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(path);
    let opts = gdal::raster::RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "TILED=YES"]);
    let mut ds = drv
        .create_with_band_type_with_options::<f32, _>(path, grid.w, grid.h, 1, &opts)
        .map_err(|e| format!("{path}: {e}"))?;
    ds.set_geo_transform(&grid.gt).map_err(|e| e.to_string())?;
    if !grid.wkt.is_empty() {
        ds.set_projection(&grid.wkt).map_err(|e| e.to_string())?;
    }
    let mut b = ds.rasterband(1).map_err(|e| e.to_string())?;
    b.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
    let mut buf = gdal::raster::Buffer::new((grid.w, grid.h), values.to_vec());
    b.write((0, 0), (grid.w, grid.h), &mut buf).map_err(|e| format!("{path}: {e}"))?;
    Ok(())
}

fn num(v: f64) -> String {
    if v.is_finite() { crate::rules::fmt_g(v, 10) } else { String::new() }
}

fn csv_text(s: &str) -> String {
    if s.contains([',', '"', '\n']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
}

impl Comparison {
    /// The numbers as rows of (name, value): the summary CSV, the JSON of the benchmark.
    pub fn summary(&self) -> Vec<(&'static str, f64)> {
        let l = &self.lag;
        vec![
            ("covered_pixels", self.covered_pixels as f64),
            ("covered_ha", self.covered_ha),
            ("both_pixels", self.both as f64),
            ("only_map_pixels", self.only_map as f64),
            ("only_alerts_pixels", self.only_alerts as f64),
            ("both_ha", self.both_ha),
            ("only_map_ha", self.only_map_ha),
            ("only_alerts_ha", self.only_alerts_ha),
            ("map_ha", self.map_ha()),
            ("alerts_ha", self.alerts_ha()),
            ("alerts_detected_share_by_pixels", self.alerts_detected_share()),
            ("map_confirmed_share_by_pixels", self.map_confirmed_share()),
            ("dice", self.dice()),
            ("alerts", self.alerts as f64),
            ("alerts_detected", self.alerts_detected as f64),
            ("alerts_detected_share_by_count", self.alerts_detected_count_share()),
            ("alerts_detected_share_by_area", self.alerts_detected_area_share()),
            ("alerts_too_small", self.alerts_too_small as f64),
            ("map_patches", self.map_patches as f64),
            ("map_patches_confirmed", self.map_patches_confirmed as f64),
            ("map_confirmed_share_by_count", self.map_confirmed_count_share()),
            ("lag_n", l.n as f64),
            ("lag_mean_days", l.mean),
            ("lag_min_days", l.min),
            ("lag_q10_days", l.q10),
            ("lag_q25_days", l.q25),
            ("lag_median_days", l.median),
            ("lag_q75_days", l.q75),
            ("lag_q90_days", l.q90),
            ("lag_max_days", l.max),
            ("lag_within_30_days", l.within_30),
            ("lag_within_90_days", l.within_90),
            ("lag_within_365_days", l.within_365),
        ]
    }

    /// The summary as CSV (metric,value), headed by what was compared.
    pub fn summary_csv(&self, map: &str, alerts: &str) -> String {
        let mut s = String::from("metric,value\n");
        s += &format!(
            "map,{}\nalerts,{}\nfrom,{}\nto,{}\nunit,{}\n",
            csv_text(map),
            csv_text(alerts),
            ymd(self.from),
            ymd(self.to),
            self.unit
        );
        for (k, v) in self.summary() {
            s += &format!("{k},{}\n", num(v));
        }
        s
    }

    pub fn classes_csv(&self) -> String {
        let mut s = String::from(
            "class,alerts,detected,detected_share,alerts_ha,detected_ha,detected_area_share,lag_median_days\n",
        );
        for c in &self.classes {
            s += &format!(
                "{},{},{},{},{},{},{},{}\n",
                csv_text(&c.name),
                c.alerts,
                c.detected,
                num(c.detected as f64 / c.alerts as f64),
                num(c.ha),
                num(c.detected_ha),
                num(c.detected_ha / c.ha),
                num(c.lag_median)
            );
        }
        s
    }

    pub fn units_csv(&self, classes: &[String]) -> String {
        let mut s =
            String::from("id,date,class,pixels,detected_pixels,ha,detected_ha,detected,lag_median_days,own_area_ha\n");
        for u in &self.units {
            s += &format!(
                "{},{},{},{},{},{},{},{},{},{}\n",
                u.unit.id,
                ymd(u.unit.day),
                csv_text(classes.get(u.unit.class as usize).map_or("", String::as_str)),
                u.pixels,
                u.detected_pixels,
                num(u.ha),
                num(u.detected_ha),
                u.detected(),
                num(u.lag_median),
                num(u.unit.area_ha)
            );
        }
        s
    }

    pub fn histogram_csv(&self) -> String {
        let mut s = String::from("lag_from_days,lag_to_days,pixels\n");
        for (a, b, n) in &self.histogram {
            s += &format!("{a},{b},{n}\n");
        }
        s
    }

    /// Writes the four CSVs and the agreement map into `dir` as `name`_summary.csv... (their paths).
    pub fn export(&self, dir: &Path, name: &str, map: &str, alerts: &AlertsOnGrid) -> Result<Vec<String>, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut out = Vec::new();
        for (suffix, text) in [
            ("summary", self.summary_csv(map, &alerts.source)),
            ("classes", self.classes_csv()),
            ("alerts", self.units_csv(&alerts.classes)),
            ("lags", self.histogram_csv()),
        ] {
            let p = dir.join(format!("{name}_{suffix}.csv"));
            std::fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))?;
            out.push(p.to_string_lossy().into_owned());
        }
        let p = dir.join(format!("{name}_agreement.tif")).to_string_lossy().into_owned();
        write_u8(&p, &alerts.grid, &self.agreement, &AGREEMENT_CLASSES)?;
        out.push(p);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2020, 3, 1), 18322);
        for z in -40000..40000 {
            let (y, m, d) = civil_from_days(z);
            assert_eq!(days_from_civil(y, m, d), z);
        }
        assert_eq!(parse_day("2023/05/12"), Some(days_from_civil(2023, 5, 12)));
        assert_eq!(parse_day("2023-05-12T10:00:00Z"), Some(days_from_civil(2023, 5, 12)));
        assert_eq!(parse_day("x"), None);
        assert_eq!(ymd(days_from_civil(2024, 2, 29)), "2024-02-29");
        // decimal years both ways (Janus's convention)
        for d in [days_from_civil(2020, 1, 1), days_from_civil(2020, 7, 15), days_from_civil(2021, 12, 31)] {
            assert_eq!(day_of_decimal_year(decimal_year(d)), Some(d));
        }
        assert_eq!(day_of_decimal_year(0.0), None);
        assert_eq!(day_of_decimal_year(f64::NAN), None);
        // a map's dates in other encodings, as decimal years
        let day = days_from_civil(2019, 7, 2);
        let dec = decimal_year(day);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(DateCode::detect(2019.5), DateCode::DecimalYear);
        assert_eq!(DateCode::detect(2019183.0), DateCode::YearDay);
        assert_eq!(DateCode::detect(20190702.0), DateCode::Ymd);
        assert_eq!(DateCode::detect(f64::from(day)), DateCode::Days1970);
        assert_eq!(DateCode::YearDay.decimal(2019183.0), dec);
        assert_eq!(DateCode::Ymd.decimal(20190702.0), dec);
        assert_eq!(DateCode::Days1970.decimal(f64::from(day)), dec);
        assert_eq!(DateCode::DecimalYear.decimal(2019.25), 2019.25);
        assert!(DateCode::Ymd.decimal(20190230.0).is_nan());
        assert!(DateCode::YearDay.decimal(2019366.0).is_nan());
        assert!(DateCode::YearDay.decimal(0.0).is_nan());
    }

    #[test]
    fn encodings() {
        // GLAD-L: the day of the year of the file's year
        assert_eq!(Product::GladL.decode(3.0, 32.0, 2021), Some((days_from_civil(2021, 2, 1), 1)));
        assert_eq!(Product::GladL.decode(2.0, 366.0, 2020), Some((days_from_civil(2020, 12, 31), 0)));
        assert_eq!(Product::GladL.decode(0.0, 32.0, 2021), None);
        assert_eq!(Product::GladL.decode(3.0, 0.0, 2021), None);
        // GLAD-S2: days since 2018-12-31
        assert_eq!(Product::GladS2.decode(4.0, 1.0, 0), Some((days_from_civil(2019, 1, 1), 3)));
        assert_eq!(Product::GladS2.decode(1.0, 366.0, 0), Some((days_from_civil(2020, 1, 1), 0)));
        // GFW's: confidence * 10000 + days since 2014-12-31
        assert_eq!(Product::Radd.decode(20001.0, f32::NAN, 0), Some((days_from_civil(2015, 1, 1), 0)));
        assert_eq!(Product::Radd.decode(32000.0, f32::NAN, 0), Some((days_from_civil(2014, 12, 31) + 2000, 1)));
        assert_eq!(Product::Radd.decode(42000.0, f32::NAN, 0), None);
        assert_eq!(Product::Integrated.decode(42000.0, f32::NAN, 0), Some((days_from_civil(2014, 12, 31) + 2000, 2)));
        assert_eq!(Product::Integrated.decode(0.0, f32::NAN, 0), None);
    }

    #[test]
    fn tiles_and_gfw_colours() {
        assert_eq!(glad_tile(-63.0, -9.95), "070W_10S_060W_00N");
        assert_eq!(glad_tile(-63.0, -10.05), "070W_20S_060W_10S");
        assert_eq!(glad_tiles([-63.0, -10.1, -62.9, -9.9]), ["070W_20S_060W_10S", "070W_10S_060W_00N"]);
        assert_eq!(glad_tile(5.0, 2.0), "000E_00N_010E_10N");
        // GFW's Rondonia tile at zoom 14 (lon -63.018 to -62.996, lat -10.012 to -9.990)
        assert_eq!(web_tiles([-63.01, -10.0, -63.0, -9.995], 14), [5324, 8649, 5324, 8649]);
        // gfw-data-api's examples: [16, 232, 104, 4] low on 2026-10-21; [13, 42, 255, 24] highest on 2024-03-10
        let low = gfw_pixel([16, 232, 104, 4], true);
        assert_eq!(Product::Integrated.decode(f32::from(low), f32::NAN, 0), Some((days_from_civil(2026, 10, 21), 0)));
        let hi = gfw_pixel([13, 42, 255, 24], true);
        assert_eq!(Product::Integrated.decode(f32::from(hi), f32::NAN, 0), Some((days_from_civil(2024, 3, 10), 2)));
        // RADD's tiles (RGB): B 155 low, 255 high
        assert_eq!(gfw_pixel([16, 232, 155, 255], false), 24312);
        assert_eq!(gfw_pixel([16, 232, 255, 255], false), 34312);
        assert_eq!(gfw_pixel([0, 0, 100, 0], true), 0);
    }

    #[test]
    fn quantiles_and_patches() {
        let v = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(quantile(&v, 0.5), 2.5);
        assert!((quantile(&v, 0.1) - 1.3).abs() < 1e-12);
        // two patches: an L (8-connected through a corner) and a pixel
        #[rustfmt::skip]
        let m = [
            true, false, false, false,
            false, true, false, true,
            false, true, false, false,
        ];
        let (lab, first) = patches(&m, 4, 3);
        assert_eq!(first, vec![0, 7]);
        assert_eq!(lab, vec![1, 0, 0, 0, 0, 1, 0, 2, 0, 1, 0, 0]);
        // a U joining two branches seen first apart
        #[rustfmt::skip]
        let u = [
            true, false, true,
            true, false, true,
            true, true, true,
        ];
        let (lab, first) = patches(&u, 3, 3);
        assert_eq!(first.len(), 1);
        assert!(lab.iter().zip(&u).all(|(l, m)| (*l == 1) == *m));
    }

    /// A 10 x 10 grid of 10 m pixels; the map changed in the left half, rows 0-4 in 2021-03-01, rows 5-9 in 2022;
    /// a raster of alerts in the top half (columns 2..8), 2021-02-01, confidence by column.
    #[test]
    fn compare_raster_alerts() {
        let grid = Grid {
            w: 10,
            h: 10,
            gt: [500000.0, 10.0, 0.0, 9000000.0, 0.0, -10.0],
            has_gt: true,
            wkt: SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap(),
        };
        let d1 = days_from_civil(2021, 3, 1);
        let change: Vec<f32> = (0..100)
            .map(|i| {
                let (x, y) = (i % 10, i / 10);
                if x >= 5 {
                    f32::NAN
                } else if y < 5 {
                    decimal_year(d1) as f32
                } else {
                    2022.0
                }
            })
            .collect();
        let a = days_from_civil(2021, 2, 1);
        let day: Vec<i32> =
            (0..100).map(|i| if i / 10 < 5 && (2..8).contains(&(i % 10)) { a } else { NO_DAY }).collect();
        let class: Vec<u16> = (0..100).map(|i| u16::from(i % 10 >= 4)).collect();
        let alerts = AlertsOnGrid {
            grid: grid.clone(),
            day,
            class,
            classes: vec!["low".into(), "high".into()],
            covered: vec![true; 100],
            ..Default::default()
        };
        let o = CompareOptions { from: days_from_civil(2021, 1, 1), to: days_from_civil(2021, 12, 31), bin_days: 30 };
        let c = compare(&change, &alerts, &o);
        // the map in the window: columns 0-4 of rows 0-4 (25 px); the alerts: columns 2-7 of rows 0-4 (30 px)
        assert_eq!((c.both, c.only_map, c.only_alerts), (15, 10, 15));
        assert!((c.both_ha - 0.15).abs() < 1e-12);
        assert!((c.alerts_detected_share() - 0.5).abs() < 1e-12);
        assert!((c.map_confirmed_share() - 0.6).abs() < 1e-12);
        assert_eq!((c.alerts, c.alerts_detected, c.map_patches, c.map_patches_confirmed), (1, 1, 1, 1));
        assert_eq!(c.units[0].unit.class, 1);
        assert_eq!(c.units[0].unit.day, a);
        assert_eq!(c.lag.n, 15);
        assert_eq!(c.lag.median, f64::from(d1 - a));
        assert_eq!(c.histogram, vec![(0, 30, 15)]);
        // by confidence: low (columns 2-3) and high (4-7)
        assert_eq!(c.classes[0].alerts, 0);
        assert_eq!(c.classes[1].alerts, 1);
        assert_eq!(c.classes[1].lag_median, 28.0);
        assert_eq!(c.agreement[0], ONLY_MAP);
        assert_eq!(c.agreement[2], BOTH);
        assert_eq!(c.agreement[7], ONLY_ALERTS);
        assert_eq!(c.agreement[55], 0);
        assert!(!c.map_years_only);
    }
}

#[cfg(test)]
mod burn_tests {
    use super::*;

    #[test]
    fn burns_polygons() {
        let grid = Grid {
            w: 300,
            h: 200,
            gt: [500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0],
            has_gt: true,
            wkt: SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap(),
        };
        for wkt in [
            "POLYGON ((508250 7494750,508850 7494750,508850 7495350,508250 7495350,508250 7494750))",
            "CURVEPOLYGON ((508250 7494750,508850 7494750,508850 7495350,508250 7495350,508250 7494750))",
        ] {
            let g = linear(Geometry::from_wkt(wkt).unwrap());
            let b = burn(&grid, &[g], &[7.0]).unwrap();
            assert_eq!(b.iter().filter(|&&v| v == 7).count(), 400, "{wkt}");
        }
    }
}
