//! A map polygonized (IDEIAS phase 41): the patches of a class or change map (pixels of one value connected 4- or
//! 8-wise) as polygons in a GeoPackage, with their value, class name, date (a map of years), area in hectares
//! (geodesic), perimeter, pixels and the mean of another map of the same grid (a loss's magnitude). A minimum area
//! first merges the smaller patches into their neighbours (the sieve of the post-processing, zeit_core::postproc);
//! one with no neighbour to join (a lone loss in the forest) is left out. Optionally simplified.

use std::ffi::CString;
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Dataset;

/// What a polygon's value is besides a number.
#[derive(Clone, Debug, Default)]
pub struct PolyOptions {
    /// band of the file
    pub band: usize,
    pub eight: bool,
    /// patches under this area (ha) merged into a neighbour, else left out (0: none)
    pub min_area_ha: f64,
    /// the polygons simplified to this tolerance in metres (0: not)
    pub simplify_m: f64,
    /// class names by value
    pub classes: Vec<(i64, String)>,
    /// the values are years (decimal years): a date column
    pub years: bool,
    /// the mean of another map (its name, file and band) in each polygon
    pub mean_of: Option<(String, String, usize)>,
    pub layer: String,
}

pub struct Polygonized {
    pub polygons: usize,
    pub area_ha: f64,
    /// patches left out (under the minimum area with no neighbour)
    pub dropped: usize,
}

/// GDAL's last error message.
pub(crate) fn last_error() -> String {
    // SAFETY: GDAL's error state of this thread.
    unsafe {
        let m = gdal_sys::CPLGetLastErrorMsg();
        if m.is_null() { String::new() } else { std::ffi::CStr::from_ptr(m).to_string_lossy().into_owned() }
    }
}

/// A band read whole as f64, its no data NaN.
fn read_band(ds: &Dataset, band: usize) -> Result<(Vec<f64>, usize, usize), String> {
    let (w, h) = ds.raster_size();
    let b = ds.rasterband(band).map_err(|e| e.to_string())?;
    let nd = b.no_data_value();
    let mut v = b.read_as::<f64>((0, 0), (w, h), (w, h), None).map_err(|e| e.to_string())?.data().to_vec();
    if let Some(nd) = nd.filter(|n| !n.is_nan()) {
        v.iter_mut().filter(|x| **x == nd).for_each(|x| *x = f64::NAN);
    }
    Ok((v, w, h))
}

/// The patches: a label per pixel (0: no value, from 1), each patch's value and pixels.
fn label(values: &[f64], w: usize, h: usize, eight: bool) -> (Vec<i32>, Vec<f64>, Vec<u64>) {
    let n = w * h;
    let mut parent: Vec<u32> = (0..n as u32).collect();
    fn find(p: &mut [u32], mut x: u32) -> u32 {
        while p[x as usize] != x {
            p[x as usize] = p[p[x as usize] as usize];
            x = p[x as usize];
        }
        x
    }
    let union = |p: &mut [u32], a: usize, b: usize| {
        let (ra, rb) = (find(p, a as u32), find(p, b as u32));
        if ra != rb {
            let (lo, hi) = (ra.min(rb), ra.max(rb));
            p[hi as usize] = lo;
        }
    };
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let v = values[i];
            if v.is_nan() {
                continue;
            }
            if x > 0 && values[i - 1] == v {
                union(&mut parent, i, i - 1);
            }
            if y > 0 {
                if values[i - w] == v {
                    union(&mut parent, i, i - w);
                }
                if eight {
                    if x > 0 && values[i - w - 1] == v {
                        union(&mut parent, i, i - w - 1);
                    }
                    if x + 1 < w && values[i - w + 1] == v {
                        union(&mut parent, i, i - w + 1);
                    }
                }
            }
        }
    }
    let mut labels = vec![0i32; n];
    let mut of_root = std::collections::HashMap::new();
    let (mut value, mut pixels) = (Vec::new(), Vec::new());
    for i in 0..n {
        if values[i].is_nan() {
            continue;
        }
        let r = find(&mut parent, i as u32);
        let k = *of_root.entry(r).or_insert_with(|| {
            value.push(values[i]);
            pixels.push(0u64);
            value.len() as i32
        });
        labels[i] = k;
        pixels[k as usize - 1] += 1;
    }
    (labels, value, pixels)
}

/// A decimal year as a date (YYYY-MM-DD), a whole year as itself.
fn date_text(v: f64) -> String {
    if v.fract() == 0.0 {
        return format!("{v:.0}");
    }
    let y = v.floor() as i32;
    let start = janus_cube::dates::days_from_civil(y, 1, 1);
    let len = janus_cube::dates::days_from_civil(y + 1, 1, 1) - start;
    let (yy, m, d) = janus_cube::dates::civil_from_days(start + (v.fract() * len as f64).floor() as i64);
    format!("{yy:04}-{m:02}-{d:02}")
}

/// The area (m²) of one pixel at the window's centre: geodesic with a CRS, else the geotransform's.
fn pixel_area(gt: &[f64; 6], wkt: &str, w: usize, h: usize) -> f64 {
    let planar = (gt[1] * gt[5] - gt[2] * gt[4]).abs();
    let Ok(srs) = gdal::spatial_ref::SpatialRef::from_wkt(wkt) else { return planar };
    let (cx, cy) = ((w / 2) as f64, (h / 2) as f64);
    let at = |x: f64, y: f64| (gt[0] + x * gt[1] + y * gt[2], gt[3] + x * gt[4] + y * gt[5]);
    let pts: Vec<(f64, f64)> = [(cx, cy), (cx + 1.0, cy), (cx + 1.0, cy + 1.0), (cx, cy + 1.0), (cx, cy)]
        .iter()
        .map(|&(x, y)| at(x, y))
        .collect();
    let wkt_poly = format!("POLYGON(({}))", pts.iter().map(|(x, y)| format!("{x} {y}")).collect::<Vec<_>>().join(","));
    let Ok(mut g) = gdal::vector::Geometry::from_wkt(&wkt_poly) else { return planar };
    g.set_spatial_ref(srs);
    // SAFETY: a valid geometry with its CRS.
    let a = unsafe { gdal_sys::OGR_G_GeodesicArea(g.c_geometry()) };
    if a.is_finite() && a > 0.0 { a } else { planar }
}

/// Geodesic measures of the map's polygons: their rings transformed to longitudes and latitudes by one
/// transformation for them all, measured on the CRS's ellipsoid by Karney's algorithms (geographiclib, as GDAL's
/// OGR_G_GeodesicArea, which sets up a transformation each call: 30 times gdal_polygonize's time).
pub(crate) struct Geodesic {
    ct: gdal_sys::OGRCoordinateTransformationH,
    ellipsoid: geographiclib_rs::Geodesic,
}

impl Geodesic {
    /// For a CRS (None: no ellipsoid or no transformation to it).
    pub(crate) fn of(srs: &gdal::spatial_ref::SpatialRef) -> Option<Geodesic> {
        // SAFETY: a valid spatial reference, alive during the call.
        unsafe { Geodesic::new(srs.to_c_hsrs()) }
    }

    /// Points of the CRS to longitudes (x) and latitudes (y), in place; false if they could not be.
    pub(crate) fn lon_lat(&self, x: &mut [f64], y: &mut [f64]) -> bool {
        if self.ct.is_null() {
            return true;
        }
        let mut z = vec![0.0; x.len()];
        // SAFETY: three buffers of the points' number.
        unsafe { gdal_sys::OCTTransform(self.ct, x.len() as i32, x.as_mut_ptr(), y.as_mut_ptr(), z.as_mut_ptr()) != 0 }
    }

    /// A ring's perimeter (m) and area (m²) on the ellipsoid, its points in longitudes and latitudes (not closed).
    pub(crate) fn ring(&self, x: &[f64], y: &[f64]) -> (f64, f64) {
        let mut pa = geographiclib_rs::PolygonArea::new(&self.ellipsoid, geographiclib_rs::Winding::CounterClockwise);
        for (&lon, &lat) in x.iter().zip(y) {
            pa.add_point(lat, lon);
        }
        // signed: unsigned, a clockwise ring would be the rest of the ellipsoid
        let (p, a, _) = pa.compute(true);
        (p, a.abs())
    }

    pub(crate) fn ring_area(&self, x: &[f64], y: &[f64]) -> f64 {
        self.ring(x, y).1
    }

    /// SAFETY: `srs` a valid spatial reference (or null: none).
    unsafe fn new(srs: gdal_sys::OGRSpatialReferenceH) -> Option<Geodesic> {
        use gdal_sys::OSRAxisMappingStrategy::OAMS_TRADITIONAL_GIS_ORDER;
        if srs.is_null() {
            return None;
        }
        // SAFETY: handles made here, the transformation released by Drop.
        unsafe {
            let mut err = 0;
            let a = gdal_sys::OSRGetSemiMajor(srs, &mut err);
            let inv_f = gdal_sys::OSRGetInvFlattening(srs, &mut err);
            if err != 0 || a.is_nan() || a <= 0.0 {
                return None;
            }
            let ellipsoid = geographiclib_rs::Geodesic::new(a, if inv_f > 0.0 { 1.0 / inv_f } else { 0.0 });
            if gdal_sys::OSRIsGeographic(srs) != 0 {
                // longitudes and latitudes already (x, y as written)
                return Some(Geodesic { ct: null_mut(), ellipsoid });
            }
            let geog = gdal_sys::OSRCloneGeogCS(srs);
            if geog.is_null() {
                return None;
            }
            gdal_sys::OSRSetAxisMappingStrategy(geog, OAMS_TRADITIONAL_GIS_ORDER);
            let src = gdal_sys::OSRClone(srs);
            gdal_sys::OSRSetAxisMappingStrategy(src, OAMS_TRADITIONAL_GIS_ORDER);
            let ct = gdal_sys::OCTNewCoordinateTransformation(src, geog);
            gdal_sys::OSRRelease(src);
            gdal_sys::OSRRelease(geog);
            (!ct.is_null()).then_some(Geodesic { ct, ellipsoid })
        }
    }

    /// A polygon's area (m², its holes out) and perimeter (m, its holes' too).
    /// SAFETY: `g` a valid polygon in the map's CRS.
    unsafe fn measure(&self, g: gdal_sys::OGRGeometryH) -> Option<(f64, f64)> {
        let (mut area, mut perimeter) = (0.0, 0.0);
        // SAFETY: the rings of `g`, read into buffers of their size.
        unsafe {
            for i in 0..gdal_sys::OGR_G_GetGeometryCount(g) {
                let r = gdal_sys::OGR_G_GetGeometryRef(g, i);
                let n = gdal_sys::OGR_G_GetPointCount(r).max(0) as usize;
                let (mut x, mut y) = (vec![0.0; n], vec![0.0; n]);
                gdal_sys::OGR_G_GetPoints(r, x.as_mut_ptr().cast(), 8, y.as_mut_ptr().cast(), 8, null_mut(), 0);
                if !self.lon_lat(&mut x, &mut y) {
                    return None;
                }
                // the closing point is the first again
                let m = n.saturating_sub(1);
                let (p, a) = self.ring(&x[..m], &y[..m]);
                area += if i == 0 { a } else { -a };
                perimeter += p;
            }
        }
        Some((area, perimeter))
    }
}

impl Drop for Geodesic {
    fn drop(&mut self) {
        if !self.ct.is_null() {
            // SAFETY: made by new.
            unsafe { gdal_sys::OCTDestroyCoordinateTransformation(self.ct) };
        }
    }
}

/// Polygonizes band `opts.band` of `src` into the GeoPackage `out` (replaced).
pub fn polygonize(
    src: &str,
    opts: &PolyOptions,
    out: &Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Polygonized, String> {
    let ds = Dataset::open(src).map_err(|e| format!("{src}: {e}"))?;
    let gt = ds.geo_transform().unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let wkt = ds.projection();
    progress(0.0, "reading the map");
    let (mut values, w, h) = read_band(&ds, opts.band.max(1))?;
    let px_area = pixel_area(&gt, &wkt, w, h);
    let min_pixels = if opts.min_area_ha > 0.0 { (opts.min_area_ha * 1e4 / px_area).ceil() as usize } else { 0 };
    if min_pixels >= 2 {
        progress(0.1, &format!("merging the patches under {min_pixels} pixels into their neighbours"));
        values = zeit_core::postproc::sieve(&values, h, w, min_pixels, opts.eight);
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    progress(0.25, "the patches");
    let (labels, value, pixels) = label(&values, w, h, opts.eight);
    // the other map's sums in each patch
    let mut means = vec![f64::NAN; value.len()];
    if let Some((_, path, band)) = &opts.mean_of {
        let other = Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
        let (v, ow, oh) = read_band(&other, *band)?;
        if (ow, oh) != (w, h) {
            return Err(format!("{path} is {ow} x {oh}, the map {w} x {h}: not the same grid"));
        }
        let mut sum = vec![(0.0, 0u64); value.len()];
        for (&l, &x) in labels.iter().zip(&v) {
            if l > 0 && x.is_finite() {
                let s = &mut sum[l as usize - 1];
                (s.0, s.1) = (s.0 + x, s.1 + 1);
            }
        }
        means = sum.iter().map(|&(s, c)| if c > 0 { s / c as f64 } else { f64::NAN }).collect();
    }
    progress(0.35, &format!("{} patches: polygons", value.len()));
    let c = |s: &str| CString::new(s).unwrap_or_default();
    for side in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{side}", out.display()));
    }
    let classes: std::collections::HashMap<i64, &str> = opts.classes.iter().map(|(k, n)| (*k, n.as_str())).collect();
    let srs = gdal::spatial_ref::SpatialRef::from_wkt(&wkt).ok();
    let geographic = srs.as_ref().is_some_and(|s| s.is_geographic());
    // SAFETY: GDAL's C API on handles made here and closed before returning.
    unsafe {
        gdal_sys::CPLErrorReset();
        // the labels as a raster in memory (0: no value), polygonized into a layer in memory
        let mem = gdal_sys::GDALGetDriverByName(c("MEM").as_ptr());
        let lds = gdal_sys::GDALCreate(
            mem,
            c("").as_ptr(),
            w as i32,
            h as i32,
            1,
            gdal_sys::GDALDataType::GDT_Int32,
            null_mut(),
        );
        if lds.is_null() {
            return Err(format!("the labels: {}", last_error()));
        }
        let mut gt2 = gt;
        gdal_sys::GDALSetGeoTransform(lds, gt2.as_mut_ptr());
        let lb = gdal_sys::GDALGetRasterBand(lds, 1);
        gdal_sys::GDALSetRasterNoDataValue(lb, 0.0);
        let r = gdal_sys::GDALRasterIO(
            lb,
            gdal_sys::GDALRWFlag::GF_Write,
            0,
            0,
            w as i32,
            h as i32,
            labels.as_ptr() as *mut _,
            w as i32,
            h as i32,
            gdal_sys::GDALDataType::GDT_Int32,
            0,
            0,
        );
        if r != gdal_sys::CPLErr::CE_None {
            gdal_sys::GDALClose(lds);
            return Err(format!("the labels: {}", last_error()));
        }
        let vmem = gdal_sys::GDALGetDriverByName(c("MEM").as_ptr());
        let vds = gdal_sys::GDALCreate(vmem, c("").as_ptr(), 0, 0, 0, gdal_sys::GDALDataType::GDT_Unknown, null_mut());
        let hsrs = srs.as_ref().map_or(null_mut(), |s| s.to_c_hsrs());
        let tmp = gdal_sys::GDALDatasetCreateLayer(
            vds,
            c("p").as_ptr(),
            hsrs,
            gdal_sys::OGRwkbGeometryType::wkbPolygon,
            null_mut(),
        );
        let fd = gdal_sys::OGR_Fld_Create(c("label").as_ptr(), gdal_sys::OGRFieldType::OFTInteger);
        gdal_sys::OGR_L_CreateField(tmp, fd, 1);
        gdal_sys::OGR_Fld_Destroy(fd);
        let mut popts = null_mut();
        if opts.eight {
            popts = gdal_sys::CSLSetNameValue(popts, c("8CONNECTED").as_ptr(), c("8").as_ptr());
        }
        let r = gdal_sys::GDALPolygonize(lb, gdal_sys::GDALGetMaskBand(lb), tmp, 0, popts, None, null_mut());
        gdal_sys::CSLDestroy(popts);
        gdal_sys::GDALClose(lds);
        let close_all = |e: String| {
            gdal_sys::GDALClose(vds);
            Err(e)
        };
        if r != gdal_sys::CPLErr::CE_None {
            return close_all(format!("polygonize: {}", last_error()));
        }
        if cancel.load(Ordering::Relaxed) {
            return close_all("cancelled".into());
        }
        // the GeoPackage: a polygon per patch with its columns
        let gpkg = gdal_sys::GDALGetDriverByName(c("GPKG").as_ptr());
        if gpkg.is_null() {
            return close_all("no GeoPackage driver".into());
        }
        let dst = gdal_sys::GDALCreate(
            gpkg,
            c(&out.to_string_lossy()).as_ptr(),
            0,
            0,
            0,
            gdal_sys::GDALDataType::GDT_Unknown,
            null_mut(),
        );
        if dst.is_null() {
            return close_all(format!("{}: {}", out.display(), last_error()));
        }
        let fail = |e: String| {
            gdal_sys::GDALClose(dst);
            gdal_sys::GDALClose(vds);
            let _ = std::fs::remove_file(out);
            Err(e)
        };
        let layer = gdal_sys::GDALDatasetCreateLayer(
            dst,
            c(if opts.layer.is_empty() { "polygons" } else { &opts.layer }).as_ptr(),
            hsrs,
            gdal_sys::OGRwkbGeometryType::wkbPolygon,
            null_mut(),
        );
        if layer.is_null() {
            return fail(format!("the layer: {}", last_error()));
        }
        let mut fields: Vec<(String, gdal_sys::OGRFieldType::Type)> =
            vec![("value".into(), gdal_sys::OGRFieldType::OFTReal)];
        if !classes.is_empty() {
            fields.push(("class".into(), gdal_sys::OGRFieldType::OFTString));
        }
        if opts.years {
            fields.push(("date".into(), gdal_sys::OGRFieldType::OFTString));
        }
        fields.extend([
            ("area_ha".to_string(), gdal_sys::OGRFieldType::OFTReal),
            ("perimeter_m".into(), gdal_sys::OGRFieldType::OFTReal),
            ("pixels".into(), gdal_sys::OGRFieldType::OFTInteger64),
        ]);
        if let Some((name, _, _)) = &opts.mean_of {
            let col: String =
                name.chars().map(|ch| if ch.is_alphanumeric() { ch.to_ascii_lowercase() } else { '_' }).collect();
            fields.push((format!("mean_{}", col.trim_matches('_')), gdal_sys::OGRFieldType::OFTReal));
        }
        for (name, kind) in &fields {
            let fd = gdal_sys::OGR_Fld_Create(c(name).as_ptr(), *kind);
            let r = gdal_sys::OGR_L_CreateField(layer, fd, 1);
            gdal_sys::OGR_Fld_Destroy(fd);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                return fail(format!("the column {name}: {}", last_error()));
            }
        }
        let idx = |name: &str| fields.iter().position(|f| f.0 == name).map(|i| i as i32);
        let defn = gdal_sys::OGR_L_GetLayerDefn(layer);
        let tol = if geographic { opts.simplify_m / 111_320.0 } else { opts.simplify_m };
        let geodesic = Geodesic::new(hsrs);
        gdal_sys::GDALDatasetStartTransaction(dst, 0);
        gdal_sys::OGR_L_ResetReading(tmp);
        let total = gdal_sys::OGR_L_GetFeatureCount(tmp, 1).max(1);
        let (mut written, mut dropped, mut area_total, mut k) = (0usize, 0usize, 0.0, 0i64);
        loop {
            let f = gdal_sys::OGR_L_GetNextFeature(tmp);
            if f.is_null() {
                break;
            }
            k += 1;
            if k % 2000 == 0 {
                progress(0.35 + 0.6 * k as f64 / total as f64, &format!("{k} of {total} polygons"));
                if cancel.load(Ordering::Relaxed) {
                    gdal_sys::OGR_F_Destroy(f);
                    gdal_sys::GDALDatasetRollbackTransaction(dst);
                    return fail("cancelled".into());
                }
            }
            let l = gdal_sys::OGR_F_GetFieldAsInteger(f, 0);
            let Some(p) = (l as usize).checked_sub(1).filter(|&p| p < value.len()) else {
                gdal_sys::OGR_F_Destroy(f);
                continue;
            };
            if min_pixels >= 2 && (pixels[p] as usize) < min_pixels {
                dropped += 1;
                gdal_sys::OGR_F_Destroy(f);
                continue;
            }
            let mut g = gdal_sys::OGR_G_Clone(gdal_sys::OGR_F_GetGeometryRef(f));
            gdal_sys::OGR_F_Destroy(f);
            if tol > 0.0 {
                let s = gdal_sys::OGR_G_SimplifyPreserveTopology(g, tol);
                if !s.is_null() {
                    gdal_sys::OGR_G_DestroyGeometry(g);
                    g = s;
                }
            }
            gdal_sys::OGR_G_AssignSpatialReference(g, hsrs);
            // on the ellipsoid, else the plane's
            let (area, perim) = geodesic.as_ref().and_then(|m| m.measure(g)).unwrap_or_else(|| {
                let b = gdal_sys::OGR_G_Boundary(g);
                let length = if b.is_null() { f64::NAN } else { gdal_sys::OGR_G_Length(b) };
                if !b.is_null() {
                    gdal_sys::OGR_G_DestroyGeometry(b);
                }
                (gdal_sys::OGR_G_Area(g), length)
            });
            let nf = gdal_sys::OGR_F_Create(defn);
            gdal_sys::OGR_F_SetGeometryDirectly(nf, g);
            gdal_sys::OGR_F_SetFieldDouble(nf, 0, value[p]);
            if let Some(i) = idx("class") {
                let v = value[p];
                match classes.get(&(v.round() as i64)).filter(|_| v.fract() == 0.0) {
                    Some(n) => gdal_sys::OGR_F_SetFieldString(nf, i, c(n).as_ptr()),
                    None => gdal_sys::OGR_F_SetFieldNull(nf, i),
                }
            }
            if let Some(i) = idx("date") {
                gdal_sys::OGR_F_SetFieldString(nf, i, c(&date_text(value[p])).as_ptr());
            }
            gdal_sys::OGR_F_SetFieldDouble(nf, idx("area_ha").unwrap_or(0), area / 1e4);
            gdal_sys::OGR_F_SetFieldDouble(nf, idx("perimeter_m").unwrap_or(0), perim);
            gdal_sys::OGR_F_SetFieldInteger64(nf, idx("pixels").unwrap_or(0), pixels[p] as i64);
            if opts.mean_of.is_some() {
                let i = fields.len() as i32 - 1;
                if means[p].is_finite() {
                    gdal_sys::OGR_F_SetFieldDouble(nf, i, means[p]);
                } else {
                    gdal_sys::OGR_F_SetFieldNull(nf, i);
                }
            }
            let r = gdal_sys::OGR_L_CreateFeature(layer, nf);
            gdal_sys::OGR_F_Destroy(nf);
            if r != gdal_sys::OGRErr::OGRERR_NONE {
                gdal_sys::GDALDatasetRollbackTransaction(dst);
                return fail(format!("a polygon: {}", last_error()));
            }
            written += 1;
            area_total += area / 1e4;
        }
        if gdal_sys::GDALDatasetCommitTransaction(dst) != gdal_sys::OGRErr::OGRERR_NONE {
            return fail(format!("the polygons: {}", last_error()));
        }
        gdal_sys::GDALClose(dst);
        gdal_sys::GDALClose(vds);
        progress(1.0, &format!("{written} polygons"));
        Ok(Polygonized { polygons: written, area_ha: area_total, dropped })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches() {
        // 4 x 3: two patches of 1 touching at a corner, a patch of 2, no data
        let n = f64::NAN;
        let v = [1.0, n, 2.0, 2.0, n, 1.0, n, n, n, n, n, 1.0];
        let (l4, val4, px4) = label(&v, 4, 3, false);
        assert_eq!(val4.len(), 4, "{l4:?}");
        assert_eq!(px4.iter().sum::<u64>(), 5);
        let (_, val8, px8) = label(&v, 4, 3, true);
        // 8-wise the 1 at (0,0) joins the 1 at (1,1)
        assert_eq!(val8.len(), 3);
        assert_eq!(px8[0], 2);
    }

    /// The areas and perimeters as GDAL's OGR_G_GeodesicArea and OGR_G_GeodesicLength give them.
    #[test]
    fn geodesic_as_gdal() {
        use gdal::spatial_ref::{AxisMappingStrategy, SpatialRef};
        let cases = [
            (
                32723,
                "POLYGON((600000 7500000,603000 7500000,603000 7497000,601500 7496000,600000 7497000,600000 7500000),                 (601000 7499000,601000 7498000,602000 7498000,601000 7499000))",
            ),
            (4326, "POLYGON((-45 -20,-44.9 -20,-44.9 -20.1,-45 -20.1,-45 -20))"),
        ];
        for (epsg, wkt) in cases {
            let mut srs = SpatialRef::from_epsg(epsg).unwrap();
            srs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
            let mut g = gdal::vector::Geometry::from_wkt(wkt).unwrap();
            g.set_spatial_ref(srs.clone());
            // SAFETY: a valid polygon with its CRS.
            let (want_a, want_p, got) = unsafe {
                let h = g.c_geometry();
                let b = gdal_sys::OGR_G_Boundary(h);
                gdal_sys::OGR_G_AssignSpatialReference(b, srs.to_c_hsrs());
                let p = gdal_sys::OGR_G_GeodesicLength(b);
                gdal_sys::OGR_G_DestroyGeometry(b);
                (gdal_sys::OGR_G_GeodesicArea(h), p, Geodesic::new(srs.to_c_hsrs()).unwrap().measure(h).unwrap())
            };
            assert!((got.0 / want_a - 1.0).abs() < 1e-9, "{epsg}: area {} against {want_a}", got.0);
            assert!((got.1 / want_p - 1.0).abs() < 1e-9, "{epsg}: perimeter {} against {want_p}", got.1);
        }
    }

    #[test]
    fn dates() {
        assert_eq!(date_text(2019.0), "2019");
        assert_eq!(date_text(2020.5), "2020-07-02");
    }
}
