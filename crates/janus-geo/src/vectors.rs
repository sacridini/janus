//! Vector data (field plots, samples, parcels...) read through OGR, which comes with GDAL (src/vectors.cpp):
//! GeoJSON, Shapefile, GeoPackage, KML, FlatGeobuf, GML, and CSV tables of points (x/lon and y/lat columns).
//! Geometries are kept in the file's coordinates; `Placer` puts them on a raster's grid (any CRS, through PROJ).

use std::ffi::{CStr, CString, c_char};
use std::path::Path;
use std::ptr::null_mut;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal_sys::OGRwkbGeometryType::*;
use rayon::prelude::*;

use crate::plane::{self, Polygon, Ring};
use crate::reproject::{Transform, invert};

/// GDALOpenEx: vector data, read-only.
const GDAL_OF_VECTOR: u32 = 0x04;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// no geometry (kept: rows stay in the file's order)
    #[default]
    None,
    Point,
    Line,
    Area,
}

#[derive(Clone, Debug, Default)]
pub struct Feature {
    pub fid: i64,
    pub kind: Kind,
    /// points: one (x, y) per part; lines: one open ring per line; areas: their rings (shells and holes,
    /// even-odd)
    pub parts: Polygon,
    /// a point on it (inside an area), in the file's coordinates
    pub cx: f64,
    pub cy: f64,
    /// one per field, as text ("" = null)
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct VectorData {
    /// the file and its OGR layer
    pub path: String,
    pub layer: String,
    /// "" = none in the file
    pub crs_wkt: String,
    pub crs_name: String,
    /// no CRS, coordinates within lon/lat ranges: read as WGS 84
    pub lon_lat_assumed: bool,
    pub fields: Vec<String>,
    pub features: Vec<Feature>,
    pub vertices: usize,
    pub points: usize,
    pub lines: usize,
    pub areas: usize,
    pub empty: usize,
    /// stopped at the vertex limit
    pub truncated: bool,
    pub load_ms: f64,
}

impl VectorData {
    /// The fields' values came with the geometries (else each feature's `values` is empty: read_attributes reads
    /// them when asked).
    pub fn has_values(&self) -> bool {
        self.fields.is_empty() || self.features.first().is_none_or(|f| f.values.len() == self.fields.len())
    }

    /// e.g. "250 points" or "12 polygons, 3 lines"
    pub fn kind_text(&self) -> String {
        let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
        let mut s: Vec<String> = Vec::new();
        if self.points > 0 {
            s.push(n(self.points, "point", "points"));
        }
        if self.areas > 0 {
            s.push(n(self.areas, "polygon", "polygons"));
        }
        if self.lines > 0 {
            s.push(n(self.lines, "line", "lines"));
        }
        if self.empty > 0 {
            s.push(n(self.empty, "row without geometry", "rows without geometry"));
        }
        if s.is_empty() { "no features".into() } else { s.join(", ") }
    }
}

fn lower_ext(path: &str) -> String {
    Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

/// By extension: .geojson, .json, .shp, .gpkg, .kml, .kmz, .gml, .fgb, .csv, .gpx, .tab, .geojsonl.
pub fn is_vector_path(path: &str) -> bool {
    const EXTS: [&str; 12] =
        [".geojson", ".json", ".shp", ".gpkg", ".kml", ".kmz", ".gml", ".fgb", ".csv", ".gpx", ".tab", ".geojsonl"];
    EXTS.contains(&lower_ext(path).as_str())
}

fn c_string(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

fn last_error() -> String {
    c_string(unsafe { gdal_sys::CPLGetLastErrorMsg() })
}

/// "EPSG:32722", else the CRS's name.
unsafe fn crs_text(s: gdal_sys::OGRSpatialReferenceH) -> String {
    let (auth, code) =
        unsafe { (gdal_sys::OSRGetAuthorityName(s, null_mut()), gdal_sys::OSRGetAuthorityCode(s, null_mut())) };
    if !auth.is_null() && !code.is_null() {
        return format!("{}:{}", c_string(auth), c_string(code));
    }
    let name = unsafe { gdal_sys::OSRGetName(s) };
    if name.is_null() { "unknown CRS".into() } else { c_string(name) }
}

unsafe fn wkt2(s: gdal_sys::OGRSpatialReferenceH) -> String {
    let fmt = CString::new("FORMAT=WKT2_2019").unwrap();
    let mut opts = [fmt.as_ptr(), std::ptr::null()];
    let mut w: *mut c_char = null_mut();
    unsafe {
        gdal_sys::OSRExportToWktEx(s, &mut w, opts.as_mut_ptr());
        let out = c_string(w);
        gdal_sys::VSIFree(w.cast());
        out
    }
}

/// A curve's points appended to `r`, in one call (x and y interleaved, 16 bytes apart).
unsafe fn add_points(g: gdal_sys::OGRGeometryH, r: &mut Ring) {
    let n = unsafe { gdal_sys::OGR_G_GetPointCount(g) }.max(0) as usize;
    if n == 0 {
        return;
    }
    let at = r.len();
    r.resize(at + n * 2, 0.0);
    let p = r[at..].as_mut_ptr();
    unsafe { gdal_sys::OGR_G_GetPoints(g, p.cast(), 16, p.add(1).cast(), 16, null_mut(), 0) };
}

/// The parts of a (multi, collection, curved) geometry; `kind` the highest seen.
unsafe fn collect(g: gdal_sys::OGRGeometryH, f: &mut Feature) {
    unsafe {
        if g.is_null() || gdal_sys::OGR_G_IsEmpty(g) != 0 {
            return;
        }
        if gdal_sys::OGR_G_HasCurveGeometry(g, 0) != 0 {
            let lin = gdal_sys::OGR_G_GetLinearGeometry(g, 0.0, null_mut());
            if !lin.is_null() {
                if gdal_sys::OGR_G_HasCurveGeometry(lin, 0) == 0 {
                    collect(lin, f);
                }
                gdal_sys::OGR_G_DestroyGeometry(lin);
            }
            return;
        }
        match gdal_sys::OGR_GT_Flatten(gdal_sys::OGR_G_GetGeometryType(g)) {
            t if t == wkbPoint => {
                f.parts.push(vec![gdal_sys::OGR_G_GetX(g, 0), gdal_sys::OGR_G_GetY(g, 0)]);
                f.kind = f.kind.max(Kind::Point);
            }
            t if t == wkbLineString || t == wkbLinearRing => {
                let mut r = Ring::new();
                add_points(g, &mut r);
                if r.len() >= 4 {
                    f.parts.push(r);
                }
                f.kind = f.kind.max(Kind::Line);
            }
            t if t == wkbPolygon => {
                for i in 0..gdal_sys::OGR_G_GetGeometryCount(g) {
                    let mut r = Ring::new();
                    add_points(gdal_sys::OGR_G_GetGeometryRef(g, i), &mut r);
                    let n = r.len();
                    if n >= 4 && r[0] == r[n - 2] && r[1] == r[n - 1] {
                        r.truncate(n - 2); // closed implicitly
                    }
                    if r.len() >= 6 {
                        f.parts.push(r);
                    }
                }
                f.kind = Kind::Area;
            }
            t if t == wkbMultiPoint
                || t == wkbMultiLineString
                || t == wkbMultiPolygon
                || t == wkbGeometryCollection =>
            {
                for i in 0..gdal_sys::OGR_G_GetGeometryCount(g) {
                    collect(gdal_sys::OGR_G_GetGeometryRef(g, i), f);
                }
            }
            _ => {}
        }
    }
}

/// A point on a feature, in its coordinates: inside an area (plane::interior_point; its box's centre if it has
/// none), a line's centroid, a point's own.
fn anchor(f: &Feature) -> Option<(f64, f64)> {
    match f.kind {
        Kind::Area => plane::interior_point(&f.parts).or_else(|| {
            let b = plane::bounds(&f.parts)?;
            Some(((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0))
        }),
        Kind::Line => plane::line_centroid(&f.parts),
        Kind::Point => Some((f.parts[0][0], f.parts[0][1])),
        Kind::None => None,
    }
}

/// GDAL's drivers, registered once (raw calls do not register them).
fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(gdal::DriverManager::register_all);
}

/// A list of "KEY=VALUE" options for GDAL (null-terminated).
struct Options {
    _strings: Vec<CString>,
    ptrs: Vec<*mut c_char>,
}

impl Options {
    fn new(kv: &[(&str, &str)]) -> Self {
        let strings: Vec<CString> = kv.iter().map(|(k, v)| CString::new(format!("{k}={v}")).unwrap()).collect();
        let mut ptrs: Vec<*mut c_char> = strings.iter().map(|s| s.as_ptr().cast_mut()).collect();
        ptrs.push(null_mut());
        Options { _strings: strings, ptrs }
    }

    fn as_ptr(&self) -> *const *const c_char {
        if self.ptrs.len() == 1 { std::ptr::null() } else { self.ptrs.as_ptr().cast() }
    }
}

/// The file opened for its vector data (a table of points with the same options every time: the same fields).
fn open_vector(path: &str) -> Result<gdal_sys::GDALDatasetH, String> {
    register();
    // a table of points: the coordinate columns by their usual names
    let open = if lower_ext(path) == ".csv" {
        Options::new(&[
            ("X_POSSIBLE_NAMES", "x,lon,long,longitude,lng,easting,coord_x,x_coord,xcoord"),
            ("Y_POSSIBLE_NAMES", "y,lat,latitude,northing,coord_y,y_coord,ycoord"),
            ("KEEP_GEOM_COLUMNS", "YES"),
            ("AUTODETECT_TYPE", "NO"),
        ])
    } else {
        Options::new(&[])
    };
    let cpath = CString::new(path).map_err(|e| e.to_string())?;
    let ds = unsafe {
        gdal_sys::CPLErrorReset();
        gdal_sys::GDALOpenEx(cpath.as_ptr(), GDAL_OF_VECTOR, std::ptr::null(), open.as_ptr(), std::ptr::null())
    };
    if ds.is_null() {
        let m = last_error();
        return Err(format!(
            "cannot open {path} as vector data{}",
            if m.is_empty() { String::new() } else { format!(": {m}") }
        ));
    }
    Ok(ds)
}

/// Feature f's values of its first `n` fields, as text ("" = null).
unsafe fn field_values(f: gdal_sys::OGRFeatureH, n: usize) -> Vec<String> {
    (0..n as i32)
        .map(|k| unsafe {
            if gdal_sys::OGR_F_IsFieldSetAndNotNull(f, k) != 0 {
                c_string(gdal_sys::OGR_F_GetFieldAsString(f, k))
            } else {
                String::new()
            }
        })
        .collect()
}

/// OGR told to skip `names` (fields, or OGR_GEOMETRY) when it reads layer l's features.
unsafe fn ignore_fields(l: gdal_sys::OGRLayerH, names: &[&str]) {
    let names: Vec<CString> = names.iter().filter_map(|n| CString::new(*n).ok()).collect();
    let mut p: Vec<*const c_char> = names.iter().map(|c| c.as_ptr()).collect();
    p.push(std::ptr::null());
    unsafe { gdal_sys::OGR_L_SetIgnoredFields(l, p.as_mut_ptr()) };
}

/// Every layer of the file that has features (a GeoPackage may hold several), with their fields' values; Err if
/// none can be read.
pub fn load_vector_file(path: &str, max_vertices: usize) -> Result<Vec<VectorData>, String> {
    load_vector(path, max_vertices, true)
}

/// As load_vector_file, without the fields' values (their names only): OGR skips them, 2.5 to 4.5 times faster
/// (200 000 polygons of 16 fields: a GeoPackage in 0.23 s instead of 1.07 s, a Shapefile in 0.68 s instead of 1.85 s);
/// read_attributes reads them when asked.
pub fn load_vector_geometry(path: &str, max_vertices: usize) -> Result<Vec<VectorData>, String> {
    load_vector(path, max_vertices, false)
}

fn load_vector(path: &str, max_vertices: usize, attributes: bool) -> Result<Vec<VectorData>, String> {
    let t0 = Instant::now();
    let ds = open_vector(path)?;
    let mut out = Vec::new();
    let mut vertices = 0usize;
    unsafe {
        for li in 0..gdal_sys::GDALDatasetGetLayerCount(ds) {
            let l = gdal_sys::GDALDatasetGetLayer(ds, li);
            if l.is_null() {
                continue;
            }
            let mut v =
                VectorData { path: path.into(), layer: c_string(gdal_sys::OGR_L_GetName(l)), ..Default::default() };
            let s = gdal_sys::OGR_L_GetSpatialRef(l);
            if !s.is_null() {
                v.crs_wkt = wkt2(s);
                v.crs_name = crs_text(s);
            }
            let defn = gdal_sys::OGR_L_GetLayerDefn(l);
            for k in 0..gdal_sys::OGR_FD_GetFieldCount(defn) {
                v.fields.push(c_string(gdal_sys::OGR_Fld_GetNameRef(gdal_sys::OGR_FD_GetFieldDefn(defn, k))));
            }
            if !attributes && !v.fields.is_empty() {
                ignore_fields(l, &v.fields.iter().map(String::as_str).collect::<Vec<_>>());
            }
            gdal_sys::OGR_L_ResetReading(l);
            loop {
                let f = gdal_sys::OGR_L_GetNextFeature(l);
                if f.is_null() {
                    break;
                }
                let mut vf = Feature { fid: gdal_sys::OGR_F_GetFID(f), ..Default::default() };
                if attributes {
                    vf.values = field_values(f, v.fields.len());
                }
                collect(gdal_sys::OGR_F_GetGeometryRef(f), &mut vf);
                if vf.parts.is_empty() {
                    vf.kind = Kind::None;
                }
                gdal_sys::OGR_F_Destroy(f);
                vertices += vf.parts.iter().map(|r| r.len() / 2).sum::<usize>();
                match vf.kind {
                    Kind::Point => v.points += 1,
                    Kind::Line => v.lines += 1,
                    Kind::Area => v.areas += 1,
                    Kind::None => v.empty += 1,
                }
                v.features.push(vf);
                if vertices > max_vertices {
                    v.truncated = true;
                    break;
                }
            }
            if v.features.is_empty() {
                continue;
            }
            // each feature's point (inside an area), in parallel
            v.features.par_iter_mut().for_each(|f| {
                if let Some((x, y)) = anchor(f) {
                    (f.cx, f.cy) = (x, y);
                }
            });
            // no CRS (e.g. a CSV of points): lon/lat when every coordinate fits
            if v.crs_wkt.is_empty()
                && v.features
                    .iter()
                    .flat_map(|f| &f.parts)
                    .all(|r| r.as_chunks::<2>().0.iter().all(|p| p[0].abs() <= 180.0 && p[1].abs() <= 90.0))
            {
                let s = wgs84();
                v.crs_wkt = wkt2(s.to_c_hsrs());
                v.crs_name = "EPSG:4326".into();
                v.lon_lat_assumed = true;
            }
            v.vertices = v.features.iter().flat_map(|f| &f.parts).map(|r| r.len() / 2).sum();
            v.load_ms = t0.elapsed().as_secs_f64() * 1000.0;
            out.push(v);
            if vertices > max_vertices {
                break;
            }
        }
        gdal_sys::GDALClose(ds);
    }
    if out.is_empty() {
        return Err(format!("{path} has no features"));
    }
    Ok(out)
}

/// v's layer in its file, opened again.
fn reopen(v: &VectorData) -> Result<(gdal_sys::GDALDatasetH, gdal_sys::OGRLayerH), String> {
    let ds = open_vector(&v.path)?;
    let name = CString::new(v.layer.as_str()).map_err(|e| e.to_string())?;
    let l = unsafe { gdal_sys::GDALDatasetGetLayerByName(ds, name.as_ptr()) };
    if l.is_null() {
        unsafe { gdal_sys::GDALClose(ds) };
        return Err(format!("{} has no layer {} any more", v.path, v.layer));
    }
    Ok((ds, l))
}

/// The fields' values of v's features (read without them: load_vector_geometry), from its file again, a row per
/// feature in the order they were read; `done` counts the rows read, `cancel` stops it. Err if the file changed
/// since (its features are not the same).
pub fn read_attributes(v: &VectorData, done: &AtomicUsize, cancel: &AtomicBool) -> Result<Vec<Vec<String>>, String> {
    let (ds, l) = reopen(v)?;
    let n = v.features.len();
    let mut rows = Vec::with_capacity(n);
    let changed = || format!("{} changed since it was read: open it again", v.path);
    let r = unsafe {
        // the fields alone
        ignore_fields(l, &["OGR_GEOMETRY", "OGR_STYLE"]);
        gdal_sys::OGR_L_ResetReading(l);
        loop {
            if rows.len() == n {
                break Ok(());
            }
            if cancel.load(Ordering::Relaxed) {
                break Err("cancelled".to_string());
            }
            let f = gdal_sys::OGR_L_GetNextFeature(l);
            if f.is_null() {
                break Err(changed());
            }
            let same = gdal_sys::OGR_F_GetFID(f) == v.features[rows.len()].fid
                && gdal_sys::OGR_F_GetFieldCount(f) as usize == v.fields.len();
            if same {
                rows.push(field_values(f, v.fields.len()));
            }
            gdal_sys::OGR_F_Destroy(f);
            if !same {
                break Err(changed());
            }
            done.store(rows.len(), Ordering::Relaxed);
        }
    };
    unsafe { gdal_sys::GDALClose(ds) };
    r.map(|_| rows)
}

/// The fields' values of every feature of v: those read with its geometries, else read from its file now.
pub fn attributes(v: &VectorData) -> Result<Vec<Vec<String>>, String> {
    if v.has_values() {
        return Ok(v.features.iter().map(|f| f.values.clone()).collect());
    }
    read_attributes(v, &AtomicUsize::new(0), &AtomicBool::new(false))
}

/// One feature's fields at a time, by its FID (the feature under the cursor, the one being interpreted): the file
/// kept open between them.
pub struct FeatureReader {
    ds: gdal_sys::GDALDatasetH,
    l: gdal_sys::OGRLayerH,
    fields: usize,
}

impl FeatureReader {
    pub fn open(v: &VectorData) -> Result<FeatureReader, String> {
        let (ds, l) = reopen(v)?;
        unsafe { ignore_fields(l, &["OGR_GEOMETRY", "OGR_STYLE"]) };
        Ok(FeatureReader { ds, l, fields: v.fields.len() })
    }

    /// Its fields' values (None: no feature `fid` in the file).
    pub fn values(&mut self, fid: i64) -> Option<Vec<String>> {
        unsafe {
            let f = gdal_sys::OGR_L_GetFeature(self.l, fid);
            if f.is_null() {
                return None;
            }
            let ok = gdal_sys::OGR_F_GetFieldCount(f) as usize == self.fields;
            let out = ok.then(|| field_values(f, self.fields));
            gdal_sys::OGR_F_Destroy(f);
            out
        }
    }
}

impl Drop for FeatureReader {
    fn drop(&mut self) {
        unsafe { gdal_sys::GDALClose(self.ds) };
    }
}

/// WGS 84 with x = longitude.
fn wgs84() -> SpatialRef {
    let mut s = SpatialRef::new().expect("a spatial reference");
    let name = CString::new("WGS84").unwrap();
    unsafe { gdal_sys::OSRSetWellKnownGeogCS(s.to_c_hsrs(), name.as_ptr()) };
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    s
}

fn same(a: &SpatialRef, b: &SpatialRef) -> bool {
    unsafe { gdal_sys::OSRIsSame(a.to_c_hsrs(), b.to_c_hsrs()) != 0 }
}

fn transform(a: &SpatialRef, b: &SpatialRef) -> Option<Transform> {
    CoordTransform::new(a, b).ok().map(|t| Transform(Mutex::new(t)))
}

/// File coordinates -> a raster's pixels (x right, y down, continuous) and -> lon/lat.
pub struct Placer {
    /// None: the same CRS
    to_raster: Option<Transform>,
    to_lon_lat: Option<Transform>,
    inv: [f64; 6],
    /// the file is in lon/lat already
    lon_lat_same: bool,
}

impl Placer {
    /// `raster_wkt` empty: the raster has no CRS (placed only if the vector has none either).
    pub fn new(v: &VectorData, raster_wkt: &str, gt: Option<[f64; 6]>) -> Result<Placer, String> {
        Self::make(v, raster_wkt, gt, true)
    }

    /// To pixels only (no `to_lon_lat`, a transformation less to make: one per thread placing a layer).
    pub fn to_pixels(v: &VectorData, raster_wkt: &str, gt: Option<[f64; 6]>) -> Result<Placer, String> {
        Self::make(v, raster_wkt, gt, false)
    }

    fn make(v: &VectorData, raster_wkt: &str, gt: Option<[f64; 6]>, lon_lat: bool) -> Result<Placer, String> {
        let gt = gt.ok_or("the raster has no georeferencing")?;
        let inv = invert(&gt).ok_or("the raster's geotransform cannot be inverted")?;
        let src = (!v.crs_wkt.is_empty()).then(|| crate::reproject::srs(&v.crs_wkt)).flatten();
        let dst = (!raster_wkt.is_empty()).then(|| crate::reproject::srs(raster_wkt)).flatten();
        let mut p = Placer { to_raster: None, to_lon_lat: None, inv, lon_lat_same: false };
        match (&src, &dst) {
            (Some(_), None) => return Err("the raster has no CRS".into()),
            (Some(s), Some(d)) if !same(s, d) => {
                p.to_raster = Some(
                    transform(s, d)
                        .ok_or_else(|| format!("no transformation from {} to the raster's CRS", v.crs_name))?,
                );
            }
            _ => {}
        }
        // lon/lat for tables: from the file's CRS, else (none in the file) from the raster's
        if lon_lat {
            p.set_lon_lat(src.as_ref().or(dst.as_ref()));
        }
        Ok(p)
    }

    /// Only `to_lon_lat` (from the file's CRS; it fails without one).
    pub fn lon_lat(v: &VectorData) -> Placer {
        let mut p =
            Placer { to_raster: None, to_lon_lat: None, inv: [0.0, 1.0, 0.0, 0.0, 0.0, 1.0], lon_lat_same: false };
        let src = (!v.crs_wkt.is_empty()).then(|| crate::reproject::srs(&v.crs_wkt)).flatten();
        p.set_lon_lat(src.as_ref());
        p
    }

    fn set_lon_lat(&mut self, from: Option<&SpatialRef>) {
        let Some(from) = from else { return };
        let wgs = wgs84();
        if from.is_geographic() && same(from, &wgs) {
            self.lon_lat_same = true;
        } else {
            self.to_lon_lat = transform(from, &wgs);
        }
    }

    /// Many points in place; ok[i] = false where it failed.
    pub fn to_pixel_many(&self, x: &mut [f64], y: &mut [f64]) -> Vec<bool> {
        let mut ok = vec![1i32; x.len()];
        if let Some(t) = &self.to_raster {
            t.run(x, y, &mut ok);
        }
        let g = &self.inv;
        x.iter_mut()
            .zip(y.iter_mut())
            .zip(&ok)
            .map(|((x, y), &ok)| {
                if ok == 0 || !x.is_finite() || !y.is_finite() {
                    return false;
                }
                let (gx, gy) = (*x, *y);
                *x = g[0] + gx * g[1] + gy * g[2];
                *y = g[3] + gx * g[4] + gy * g[5];
                true
            })
            .collect()
    }

    pub fn to_pixel(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        let (mut xs, mut ys) = ([x], [y]);
        self.to_pixel_many(&mut xs, &mut ys)[0].then_some((xs[0], ys[0]))
    }

    /// None without a CRS.
    pub fn to_lon_lat(&self, x: f64, y: f64) -> Option<(f64, f64)> {
        if self.lon_lat_same {
            return Some((x, y));
        }
        let t = self.to_lon_lat.as_ref()?;
        let (mut xs, mut ys, mut ok) = ([x], [y], [1i32]);
        t.run(&mut xs, &mut ys, &mut ok);
        (ok[0] != 0 && xs[0].is_finite() && ys[0].is_finite()).then_some((xs[0], ys[0]))
    }

    /// Every vertex to pixels (failed ones left out; a ring kept even if short: parts stay aligned with the
    /// file's).
    pub fn placed(&self, p: &[Ring]) -> Polygon {
        p.iter()
            .map(|r| {
                let (mut xs, mut ys): (Vec<f64>, Vec<f64>) = r.as_chunks::<2>().0.iter().map(|v| (v[0], v[1])).unzip();
                let ok = self.to_pixel_many(&mut xs, &mut ys);
                (0..xs.len()).filter(|&i| ok[i]).flat_map(|i| [xs[i], ys[i]]).collect()
            })
            .collect()
    }
}

/// A CSV field: quoted when it holds a comma, a quote or a line break.
pub fn csv_field(s: &str) -> String {
    if !s.contains([',', '"', '\r', '\n']) {
        return s.to_string();
    }
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// The vector's features with extra columns (e.g. labels) written as GeoPackage, GeoJSON or Shapefile (by
/// extension, through OGR: geometries and fields read again from the source) or as CSV (fields, a point's or an
/// area's representative point in lon/lat, then the extra columns). `extra(i, k)`: the value of extra column k
/// for feature i.
pub fn write_vector_table(
    path: &str,
    v: &VectorData,
    extra_names: &[String],
    extra: &dyn Fn(usize, usize) -> String,
) -> Result<(), String> {
    let ext = lower_ext(path);
    let tmp = format!("{path}.part{ext}"); // OGR picks the format by extension: keep it last
    let _ = std::fs::remove_file(&tmp);
    if ext == ".csv" {
        // plain text: the fields (read from the file now if they were not with the geometries), a representative
        // point in lon/lat, the extra columns
        let read = if v.has_values() { None } else { Some(attributes(v)?) };
        let values = |i: usize| read.as_ref().map_or(&v.features[i].values, |r| &r[i]);
        let p = Placer::lon_lat(v);
        let mut out = String::from("fid");
        for n in &v.fields {
            out += &format!(",{}", csv_field(n));
        }
        out += ",lon,lat";
        for n in extra_names {
            out += &format!(",{}", csv_field(n));
        }
        out.push('\n');
        for (i, f) in v.features.iter().enumerate() {
            out += &f.fid.to_string();
            for s in values(i) {
                out += &format!(",{}", csv_field(s));
            }
            match (f.kind != Kind::None).then(|| p.to_lon_lat(f.cx, f.cy)).flatten() {
                Some((lon, lat)) => out += &format!(",{lon:.8},{lat:.8}"),
                None => out += ",,",
            }
            for k in 0..extra_names.len() {
                out += &format!(",{}", csv_field(&extra(i, k)));
            }
            out.push('\n');
        }
        if let Err(e) = std::fs::write(&tmp, out) {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("cannot write {tmp}: {e}"));
        }
    } else {
        write_ogr(&tmp, &ext, path, v, extra_names, extra)?;
    }
    if ext == ".shp" {
        // a Shapefile is several files: each one renamed
        for side in [".shp", ".shx", ".dbf", ".prj", ".cpg"] {
            let a = format!("{path}.part{side}");
            let b = Path::new(path).with_extension(&side[1..]);
            if Path::new(&a).exists() {
                let _ = std::fs::remove_file(&b);
                let _ = std::fs::rename(&a, &b);
            }
        }
        return Ok(());
    }
    if ext == ".gpkg" {
        // a GeoPackage replaced whole (and its -wal/-shm)
        for side in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{side}"));
        }
    }
    let _ = std::fs::remove_file(path);
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot rename {tmp}: {e}"))
}

fn write_ogr(
    tmp: &str,
    ext: &str,
    path: &str,
    v: &VectorData,
    extra_names: &[String],
    extra: &dyn Fn(usize, usize) -> String,
) -> Result<(), String> {
    let driver = match ext {
        ".gpkg" => "GPKG",
        ".shp" => "ESRI Shapefile",
        ".geojson" | ".json" => "GeoJSON",
        _ => return Err(format!("unsupported format {ext} (use .csv, .gpkg, .geojson or .shp)")),
    };
    register();
    let c = |s: &str| CString::new(s).unwrap();
    unsafe {
        let drv = gdal_sys::GDALGetDriverByName(c(driver).as_ptr());
        if drv.is_null() {
            return Err(format!("no {driver} driver"));
        }
        let src = gdal_sys::GDALOpenEx(
            c(&v.path).as_ptr(),
            GDAL_OF_VECTOR,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        let sl =
            if src.is_null() { null_mut() } else { gdal_sys::GDALDatasetGetLayerByName(src, c(&v.layer).as_ptr()) };
        if sl.is_null() {
            if !src.is_null() {
                gdal_sys::GDALClose(src);
            }
            return Err(format!("cannot read {} again", v.path));
        }
        gdal_sys::CPLErrorReset();
        let dst = gdal_sys::GDALCreate(drv, c(tmp).as_ptr(), 0, 0, 0, gdal_sys::GDALDataType::GDT_Unknown, null_mut());
        let srs = (!v.crs_wkt.is_empty()).then(|| crate::reproject::srs(&v.crs_wkt)).flatten();
        let dl = if dst.is_null() {
            null_mut()
        } else {
            gdal_sys::GDALDatasetCreateLayer(
                dst,
                c(&v.layer).as_ptr(),
                srs.as_ref().map_or(null_mut(), |s| s.to_c_hsrs()),
                gdal_sys::OGR_L_GetGeomType(sl),
                null_mut(),
            )
        };
        let mut ok = !dl.is_null();
        let sd = gdal_sys::OGR_L_GetLayerDefn(sl);
        let n_src = gdal_sys::OGR_FD_GetFieldCount(sd);
        for k in 0..n_src {
            ok = ok && gdal_sys::OGR_L_CreateField(dl, gdal_sys::OGR_FD_GetFieldDefn(sd, k), 1) as i64 == 0;
        }
        for n in extra_names {
            if !ok {
                break;
            }
            let fd = gdal_sys::OGR_Fld_Create(c(n).as_ptr(), gdal_sys::OGRFieldType::OFTString);
            ok = gdal_sys::OGR_L_CreateField(dl, fd, 1) as i64 == 0;
            gdal_sys::OGR_Fld_Destroy(fd);
        }
        if ok {
            gdal_sys::OGR_L_StartTransaction(dl);
        }
        gdal_sys::OGR_L_ResetReading(sl);
        let mut i = 0;
        while ok {
            let sf = gdal_sys::OGR_L_GetNextFeature(sl);
            if sf.is_null() {
                break;
            }
            if i < v.features.len() {
                // the rows in the order they were read
                let df = gdal_sys::OGR_F_Create(gdal_sys::OGR_L_GetLayerDefn(dl));
                gdal_sys::OGR_F_SetFrom(df, sf, 1);
                for k in 0..extra_names.len() {
                    let val = extra(i, k);
                    if !val.is_empty() {
                        gdal_sys::OGR_F_SetFieldString(df, n_src + k as i32, c(&val).as_ptr());
                    }
                }
                ok = gdal_sys::OGR_L_CreateFeature(dl, df) as i64 == 0;
                gdal_sys::OGR_F_Destroy(df);
            }
            gdal_sys::OGR_F_Destroy(sf);
            i += 1;
        }
        if !dl.is_null() {
            ok = gdal_sys::OGR_L_CommitTransaction(dl) as i64 == 0 && ok;
        }
        gdal_sys::GDALClose(src);
        if !dst.is_null() {
            gdal_sys::GDALClose(dst);
        }
        if !ok || gdal_sys::CPLGetLastErrorType() >= gdal_sys::CPLErr::CE_Failure {
            let m = last_error();
            gdal_sys::GDALDeleteDataset(drv, c(tmp).as_ptr());
            let _ = std::fs::remove_file(tmp);
            return Err(format!("cannot write {path}{}", if m.is_empty() { String::new() } else { format!(": {m}") }));
        }
    }
    Ok(())
}

/// Plane geometry of a feature's parts placed on a grid: its bounding box (None without a vertex).
pub fn bounds(p: &[Ring]) -> Option<[f64; 4]> {
    plane::bounds(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("janus-geo-vectors-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        d
    }

    #[test]
    fn geojson_csv_and_tables() {
        let d = dir();
        let gj = d.join("plots.geojson");
        std::fs::write(
            &gj,
            r#"{"type": "FeatureCollection", "features": [
{"type": "Feature", "properties": {"plot": 101, "name": "a, b"}, "geometry": {"type": "Point", "coordinates": [-45.5, -10.25]}},
{"type": "Feature", "properties": {"plot": 102, "name": null}, "geometry": {"type": "Polygon", "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]], [[3, 3], [7, 3], [7, 7], [3, 7], [3, 3]]]}},
{"type": "Feature", "properties": {"plot": 103, "name": "l"}, "geometry": {"type": "LineString", "coordinates": [[0, 0], [1, 1]]}}
]}"#,
        )
        .unwrap();
        let v = load_vector_file(gj.to_str().unwrap(), 1_000_000).unwrap();
        let v = &v[0];
        assert_eq!((v.points, v.areas, v.lines), (1, 1, 1));
        assert_eq!(v.fields, ["plot", "name"]);
        assert_eq!(v.features[0].values, ["101", "a, b"]);
        assert_eq!(v.features[1].values[1], "");
        assert_eq!(v.features[1].parts.len(), 2);
        assert_eq!(v.features[1].parts[0].len(), 8); // closed implicitly
        assert!(plane::contains(&v.features[1].parts, v.features[1].cx, v.features[1].cy));
        assert_eq!(v.crs_name, "EPSG:4326");
        assert_eq!(v.kind_text(), "1 point, 1 polygon, 1 line");
        // a CSV of points without a CRS: lon/lat assumed
        let csv = d.join("samples.csv");
        std::fs::write(&csv, "lon,lat,class\n-45.5,-10.25,c0\n-45.6,-10.3,c1\n").unwrap();
        let c = &load_vector_file(csv.to_str().unwrap(), 1_000_000).unwrap()[0];
        assert!(c.lon_lat_assumed && c.points == 2);
        // placed on a grid of 0.5 degree pixels from (-50, 0)
        let p = Placer::new(c, &v.crs_wkt, Some([-50.0, 0.5, 0.0, 0.0, 0.0, -0.5])).unwrap();
        let (x, y) = p.to_pixel(-45.5, -10.25).unwrap();
        assert!((x - 9.0).abs() < 1e-9 && (y - 20.5).abs() < 1e-9);
        assert_eq!(p.to_lon_lat(-45.5, -10.25), Some((-45.5, -10.25)));
        // tables
        let names = vec!["label".to_string()];
        let extra = |i: usize, _: usize| if i == 0 { "forest".to_string() } else { String::new() };
        let out = d.join("labels.csv");
        write_vector_table(out.to_str().unwrap(), v, &names, &extra).unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "fid,plot,name,lon,lat,label");
        assert!(lines[1].ends_with(",101,\"a, b\",-45.50000000,-10.25000000,forest"), "{}", lines[1]);
        let gpkg = d.join("labels.gpkg");
        write_vector_table(gpkg.to_str().unwrap(), v, &names, &extra).unwrap();
        let back = &load_vector_file(gpkg.to_str().unwrap(), 1_000_000).unwrap()[0];
        assert_eq!(back.fields, ["plot", "name", "label"]);
        assert_eq!(back.features.len(), 3);
        assert_eq!(back.features[0].values[2], "forest");
        assert_eq!(back.features[1].parts.len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn attributes_read_when_asked() {
        // a folder of its own (the other test removes its folder when it ends)
        let d = std::env::temp_dir().join(format!("janus-geo-attributes-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let gj = d.join("plots.geojson");
        std::fs::write(
            &gj,
            r#"{"type": "FeatureCollection", "features": [
{"type": "Feature", "properties": {"plot": 101, "name": "a"}, "geometry": {"type": "Point", "coordinates": [-45.5, -10.25]}},
{"type": "Feature", "properties": {"plot": 102, "name": null}, "geometry": {"type": "Polygon", "coordinates": [[[0, 0], [10, 0], [10, 10], [0, 10], [0, 0]]]}}
]}"#,
        )
        .unwrap();
        let csv = d.join("samples.csv");
        std::fs::write(&csv, "lon,lat,class\n-45.5,-10.25,c0\n-45.6,-10.3,c1\n").unwrap();
        // a GeoPackage written from it (FIDs from 1), and the CSV of points (its coordinates are fields too)
        let full = &load_vector_file(gj.to_str().unwrap(), 1_000_000).unwrap()[0];
        let gpkg = d.join("plots.gpkg");
        write_vector_table(gpkg.to_str().unwrap(), full, &[], &|_, _| String::new()).unwrap();
        for (path, fields, row1) in [
            (&gj, vec!["plot", "name"], vec!["102", ""]),
            (&gpkg, vec!["plot", "name"], vec!["102", ""]),
            (&csv, vec!["lon", "lat", "class"], vec!["-45.6", "-10.3", "c1"]),
        ] {
            let v = &load_vector_geometry(path.to_str().unwrap(), 1_000_000).unwrap()[0];
            assert_eq!(v.fields, fields);
            assert!(!v.has_values() && v.features.iter().all(|f| f.values.is_empty()));
            assert_eq!(v.features.len(), 2);
            assert!(v.features[1].kind != Kind::None, "{}: its geometries without the fields", path.display());
            let done = AtomicUsize::new(0);
            let rows = read_attributes(v, &done, &AtomicBool::new(false)).unwrap();
            assert_eq!((rows.len(), done.load(Ordering::Relaxed)), (2, 2));
            assert_eq!(rows[1], row1);
            assert_eq!(attributes(v).unwrap(), rows);
            let mut r = FeatureReader::open(v).unwrap();
            assert_eq!(r.values(v.features[1].fid).as_ref(), Some(&rows[1]));
            assert_eq!(r.values(v.features[0].fid).as_ref(), Some(&rows[0]));
            assert_eq!(r.values(9999), None);
            // a CSV table of it: the fields read for it
            let out = d.join("out.csv");
            write_vector_table(out.to_str().unwrap(), v, &[], &|_, _| String::new()).unwrap();
            let text = std::fs::read_to_string(&out).unwrap();
            assert!(
                text.lines().nth(2).unwrap().contains(&row1.iter().map(|s| csv_field(s)).collect::<Vec<_>>().join(","))
            );
        }
        // the file changed since: refused
        let v = &load_vector_geometry(gj.to_str().unwrap(), 1_000_000).unwrap()[0];
        std::fs::write(
            &gj,
            r#"{"type": "FeatureCollection", "features": [
{"type": "Feature", "properties": {"plot": 101, "name": "a"}, "geometry": {"type": "Point", "coordinates": [-45.5, -10.25]}}
]}"#,
        )
        .unwrap();
        assert!(read_attributes(v, &AtomicUsize::new(0), &AtomicBool::new(false)).unwrap_err().contains("changed"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
