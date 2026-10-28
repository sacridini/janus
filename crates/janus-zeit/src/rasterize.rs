//! A vector layer rasterized on a series' grid (IDEIAS phase 44): its features read, transformed to the grid's CRS
//! and burned by GDALRasterizeGeometries (what gdal_rasterize calls, in the features' order: a later one over an
//! earlier one), a value each: 1 (a mask, 0 elsewhere), the feature's number (1, 2... in reading order), or an
//! attribute's (a number as it is; a text as a class, its names on the band); the centre of each pixel or every
//! pixel touched. A GeoTIFF on the grid.

use std::ffi::{CString, c_void};
use std::path::Path;
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Dataset;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::{Geometry, LayerAccess};

use crate::warp::Grid;

#[derive(Clone, Debug, PartialEq)]
pub enum Burn {
    /// 1 inside, 0 elsewhere
    One,
    /// the feature's number in reading order (1, 2...), 0 elsewhere
    Index,
    /// an attribute's value: a number as it is, a text as a class
    Attribute(String),
}

#[derive(Clone, Debug)]
pub struct RasterizeOptions {
    /// the OGR layer (empty: the first)
    pub layer: String,
    pub burn: Burn,
    pub all_touched: bool,
    pub grid: Grid,
}

pub struct Rasterized {
    /// features burned (with a geometry), features read
    pub burned: usize,
    pub features: usize,
    /// a text attribute's classes (value, text), by text
    pub classes: Vec<(i64, String)>,
    /// "mask", "index", "number", "classes"
    pub kind: &'static str,
}

/// Tableau 20, the classes' colours.
const PALETTE: [[u8; 3]; 20] = [
    [31, 119, 180],
    [255, 127, 14],
    [44, 160, 44],
    [214, 39, 40],
    [148, 103, 189],
    [140, 86, 75],
    [227, 119, 194],
    [127, 127, 127],
    [188, 189, 34],
    [23, 190, 207],
    [174, 199, 232],
    [255, 187, 120],
    [152, 223, 138],
    [255, 152, 150],
    [197, 176, 213],
    [196, 156, 148],
    [247, 182, 210],
    [199, 199, 199],
    [219, 219, 141],
    [158, 218, 229],
];

/// The value of a feature's attribute.
enum Value {
    Number(f64),
    Text(String),
    None,
}

/// Rasterizes the layer of `vector` on the grid into the GeoTIFF `out` (replaced).
pub fn rasterize(
    vector: &str,
    opts: &RasterizeOptions,
    out: &Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Rasterized, String> {
    let ds = Dataset::open(vector).map_err(|e| format!("{vector}: {e}"))?;
    let mut layer = if opts.layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(&opts.layer) }
        .map_err(|e| format!("{vector}: {e}"))?;
    let g = &opts.grid;
    let ct = to_crs(&layer, &g.wkt)?;
    // the attribute: a number or a text
    let (mut text, mut real) = (false, false);
    if let Burn::Attribute(name) = &opts.burn {
        let f = layer.defn().fields().find(|f| f.name() == *name).ok_or(format!("{vector}: no attribute {name}"))?;
        use gdal_sys::OGRFieldType::{OFTInteger, OFTInteger64, OFTReal};
        let t = f.field_type();
        if t == OFTReal {
            real = true;
        } else if t != OFTInteger && t != OFTInteger64 {
            text = true;
        }
    }
    progress(0.0, "reading the features");
    let total = layer.feature_count().max(1);
    let (mut geoms, mut values): (Vec<Geometry>, Vec<Value>) = (vec![], vec![]);
    let mut features = 0;
    for f in layer.features() {
        features += 1;
        if features % 5000 == 0 {
            progress(0.3 * features as f64 / total as f64, &format!("{features} features read"));
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
        }
        let Some(geom) = f.geometry() else { continue };
        let geom = match &ct {
            Some(ct) => geom.transform(ct).map_err(|e| format!("feature {features}: {e}"))?,
            None => geom.clone(),
        };
        let v = match &opts.burn {
            Burn::One => Value::Number(1.0),
            Burn::Index => Value::Number(features as f64),
            Burn::Attribute(name) => {
                let i = f.field_index(name).map_err(|e| e.to_string())?;
                if text {
                    f.field_as_string(i).ok().flatten().map_or(Value::None, Value::Text)
                } else {
                    f.field_as_double(i).ok().flatten().map_or(Value::None, Value::Number)
                }
            }
        };
        if matches!(v, Value::None) {
            continue;
        }
        geoms.push(geom);
        values.push(v);
    }
    // a text's classes: its values sorted, numbered from 1
    let mut classes: Vec<(i64, String)> = vec![];
    if text {
        let mut names: Vec<String> =
            values.iter().filter_map(|v| if let Value::Text(s) = v { Some(s.clone()) } else { None }).collect();
        names.sort();
        names.dedup();
        classes = names.into_iter().enumerate().map(|(k, n)| (k as i64 + 1, n)).collect();
    }
    let burn: Vec<f64> = values
        .iter()
        .map(|v| match v {
            Value::Number(x) => *x,
            Value::Text(s) => classes.iter().find(|c| c.1 == *s).map_or(0.0, |c| c.0 as f64),
            Value::None => f64::NAN,
        })
        .collect();
    let (kind, dtype, init, nodata) = match (&opts.burn, text, real) {
        (Burn::One, ..) => ("mask", gdal_sys::GDALDataType::GDT_Byte, 0.0, None),
        (Burn::Index, ..) => ("index", gdal_sys::GDALDataType::GDT_Int32, 0.0, Some(0.0)),
        (_, true, _) => {
            let t =
                if classes.len() < 255 { gdal_sys::GDALDataType::GDT_Byte } else { gdal_sys::GDALDataType::GDT_Int32 };
            ("classes", t, 0.0, Some(0.0))
        }
        (_, _, true) => ("number", gdal_sys::GDALDataType::GDT_Float64, f64::NAN, Some(f64::NAN)),
        _ => ("number", gdal_sys::GDALDataType::GDT_Int32, i32::MIN as f64, Some(i32::MIN as f64)),
    };
    progress(0.3, &format!("burning {} features", geoms.len()));
    let c = |s: &str| CString::new(s).unwrap_or_default();
    let _ = std::fs::remove_file(out);
    // SAFETY: GDAL's C API on a dataset made and closed here; the geometries alive during the call.
    unsafe {
        gdal_sys::CPLErrorReset();
        let drv = gdal_sys::GDALGetDriverByName(c("GTiff").as_ptr());
        let mut co = null_mut();
        for o in ["TILED=YES", "COMPRESS=DEFLATE", "BIGTIFF=IF_SAFER"] {
            co = gdal_sys::CSLAddString(co, c(o).as_ptr());
        }
        let h = gdal_sys::GDALCreate(drv, c(&out.to_string_lossy()).as_ptr(), g.w as i32, g.h as i32, 1, dtype, co);
        gdal_sys::CSLDestroy(co);
        if h.is_null() {
            return Err(format!("{}: {}", out.display(), crate::polygonize::last_error()));
        }
        let mut gt = g.gt;
        gdal_sys::GDALSetGeoTransform(h, gt.as_mut_ptr());
        gdal_sys::GDALSetProjection(h, c(&g.wkt).as_ptr());
        let band = gdal_sys::GDALGetRasterBand(h, 1);
        if let Some(nd) = nodata {
            gdal_sys::GDALSetRasterNoDataValue(band, nd);
        }
        let handles: Vec<gdal_sys::OGRGeometryH> = geoms.iter().map(|g| g.c_geometry()).collect();
        let ro: &[&str] = if opts.all_touched { &["ALL_TOUCHED=TRUE"] } else { &[] };
        let step = |d: f64| {
            progress(0.3 + 0.7 * d, "burning");
            !cancel.load(Ordering::Relaxed)
        };
        // each strip written to the GeoTIFF
        let write = |y: usize, n: usize, p: *mut c_void| {
            let e = gdal_sys::GDALRasterIO(
                band,
                gdal_sys::GDALRWFlag::GF_Write,
                0,
                y as i32,
                g.w as i32,
                n as i32,
                p,
                g.w as i32,
                n as i32,
                dtype,
                0,
                0,
            );
            if e == gdal_sys::CPLErr::CE_None { Ok(()) } else { Err(crate::polygonize::last_error()) }
        };
        let window = [0, 0, g.w, g.h];
        let r = if dtype == gdal_sys::GDALDataType::GDT_Byte {
            burn_strips::<u8>(&handles, &burn, g, window, dtype, init, ro, &step, &mut |y, n, b| {
                write(y, n, b.as_ptr() as *mut c_void)
            })
        } else if dtype == gdal_sys::GDALDataType::GDT_Int32 {
            burn_strips::<i32>(&handles, &burn, g, window, dtype, init, ro, &step, &mut |y, n, b| {
                write(y, n, b.as_ptr() as *mut c_void)
            })
        } else {
            burn_strips::<f64>(&handles, &burn, g, window, dtype, init, ro, &step, &mut |y, n, b| {
                write(y, n, b.as_ptr() as *mut c_void)
            })
        };
        let r = match r {
            Ok(()) => gdal_sys::CPLErr::CE_None,
            Err(_) => gdal_sys::CPLErr::CE_Failure,
        };
        if r == gdal_sys::CPLErr::CE_None && !classes.is_empty() {
            // the classes' names and colours on the band (index = value)
            let max = classes.last().map_or(0, |c| c.0) as usize;
            let mut names = null_mut();
            for k in 0..=max {
                let n = if k == 0 { "" } else { classes[k - 1].1.as_str() };
                names = gdal_sys::CSLAddString(names, c(n).as_ptr());
            }
            gdal_sys::GDALSetRasterCategoryNames(band, names);
            gdal_sys::CSLDestroy(names);
            if max < 256 {
                let ct = gdal_sys::GDALCreateColorTable(gdal_sys::GDALPaletteInterp::GPI_RGB);
                for k in 0..=max {
                    let [r, g, b] = if k == 0 { [0, 0, 0] } else { PALETTE[(k - 1) % PALETTE.len()] };
                    let e = gdal_sys::GDALColorEntry {
                        c1: r.into(),
                        c2: g.into(),
                        c3: b.into(),
                        c4: if k == 0 { 0 } else { 255 },
                    };
                    gdal_sys::GDALSetColorEntry(ct, k as i32, &e);
                }
                gdal_sys::GDALSetRasterColorTable(band, ct);
                gdal_sys::GDALDestroyColorTable(ct);
            }
        }
        gdal_sys::GDALClose(h);
        if r != gdal_sys::CPLErr::CE_None {
            let _ = std::fs::remove_file(out);
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelled".into());
            }
            return Err(format!("rasterize: {}", crate::polygonize::last_error()));
        }
    }
    progress(1.0, "done");
    Ok(Rasterized { burned: geoms.len(), features, classes, kind })
}

/// The transformation of a layer's features to the CRS `wkt` (None: the layer has no CRS, taken as it is).
pub(crate) fn to_crs(layer: &gdal::vector::Layer, wkt: &str) -> Result<Option<CoordTransform>, String> {
    let mut dst = SpatialRef::from_wkt(wkt).map_err(|e| format!("the grid's CRS: {e}"))?;
    dst.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    match layer.spatial_ref() {
        Some(mut src) => {
            src.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
            Ok(Some(CoordTransform::new(&src, &dst).map_err(|e| format!("to the grid's CRS: {e}"))?))
        }
        None => Ok(None),
    }
}

/// What takes each strip of burn_strips: its first row, rows and values.
pub(crate) type Strip<'a, T> = dyn FnMut(usize, usize, &[T]) -> Result<(), String> + 'a;

/// Burns the geometries (a value each in `burn`) on the window (x0, y0, w, h) of the grid by
/// GDALRasterizeGeometries, in strips of rows in memory, `init` elsewhere, `options` its own (ALL_TOUCHED,
/// MERGE_ALG): `strip(y, rows, values)` with each strip's values, y from the window's top, T of `dtype`'s size.
/// In memory, not into a GeoTIFF: GDAL burns into a GeoTIFF through its block cache, 2 to 30 times slower
/// (gdal_rasterize too: 2.8 s against 0.7 s on 2048 x 2048 Int32). `progress` (0 to 1) returns false to stop.
///
/// # Safety
/// The geometries' handles must be alive and not changed during the call.
#[allow(clippy::too_many_arguments)]
pub(crate) unsafe fn burn_strips<T: Copy + Default>(
    handles: &[gdal_sys::OGRGeometryH],
    burn: &[f64],
    grid: &Grid,
    window: [usize; 4],
    dtype: gdal_sys::GDALDataType::Type,
    init: f64,
    options: &[&str],
    progress: &dyn Fn(f64) -> bool,
    strip: &mut Strip<T>,
) -> Result<(), String> {
    let [x0, y0, w, h] = window;
    // SAFETY: GDAL's C API on memory datasets made and closed here.
    unsafe {
        let size = gdal_sys::GDALGetDataTypeSizeBytes(dtype).max(1) as usize;
        if size != std::mem::size_of::<T>() {
            return Err(format!("a buffer of {} bytes for a type of {size}", std::mem::size_of::<T>()));
        }
        if w == 0 || h == 0 {
            return Ok(());
        }
        let c = |s: &str| CString::new(s).unwrap_or_default();
        // rows of 256 MB at most, a multiple of the tiles' 256 (at least one tile row), all of a small window
        let rows = ((256 << 20) / (w * size) / 256 * 256).max(256).min(h);
        let mem = gdal_sys::GDALGetDriverByName(c("MEM").as_ptr());
        let mut ro = null_mut();
        for o in options {
            ro = gdal_sys::CSLAddString(ro, c(o).as_ptr());
        }
        let mut buf = vec![T::default(); w * rows];
        let bands = [1];
        let mut out = Ok(());
        let mut y = 0;
        while y < h && out.is_ok() {
            let n = rows.min(h - y);
            let m = gdal_sys::GDALCreate(mem, c("").as_ptr(), w as i32, n as i32, 1, dtype, null_mut());
            if m.is_null() {
                out = Err(crate::polygonize::last_error());
                break;
            }
            let g = &grid.gt;
            let (px, py) = (x0 as f64, (y0 + y) as f64);
            let mut sgt = [g[0] + px * g[1] + py * g[2], g[1], g[2], g[3] + px * g[4] + py * g[5], g[4], g[5]];
            gdal_sys::GDALSetGeoTransform(m, sgt.as_mut_ptr());
            gdal_sys::GDALSetProjection(m, c(&grid.wkt).as_ptr());
            let mb = gdal_sys::GDALGetRasterBand(m, 1);
            gdal_sys::GDALFillRaster(mb, init, 0.0);
            let (a, b) = (y as f64 / h as f64, n as f64 / h as f64);
            let step = |d: f64| progress(a + b * d);
            let p = crate::warp::Progress { f: &step };
            let mut r = gdal_sys::GDALRasterizeGeometries(
                m,
                1,
                bands.as_ptr(),
                handles.len() as i32,
                handles.as_ptr(),
                None,
                null_mut(),
                burn.as_ptr(),
                ro,
                Some(crate::warp::on_progress),
                &p as *const crate::warp::Progress as *mut c_void,
            );
            if r == gdal_sys::CPLErr::CE_None {
                r = gdal_sys::GDALRasterIO(
                    mb,
                    gdal_sys::GDALRWFlag::GF_Read,
                    0,
                    0,
                    w as i32,
                    n as i32,
                    buf.as_mut_ptr() as *mut c_void,
                    w as i32,
                    n as i32,
                    dtype,
                    0,
                    0,
                );
            }
            gdal_sys::GDALClose(m);
            out = if r == gdal_sys::CPLErr::CE_None {
                strip(y, n, &buf[..w * n])
            } else {
                Err(crate::polygonize::last_error())
            };
            y += n;
        }
        gdal_sys::CSLDestroy(ro);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two overlapping squares and a text attribute, on a 10 x 10 grid: the later over the earlier, the classes
    /// numbered by text, the centres or every pixel touched.
    #[test]
    fn burns() {
        let dir = std::env::temp_dir().join(format!("janus_rasterize_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let gj = dir.join("v.geojson");
        std::fs::write(
            &gj,
            r#"{"type":"FeatureCollection","crs":{"type":"name","properties":{"name":"urn:ogc:def:crs:EPSG::32723"}},
"features":[
{"type":"Feature","properties":{"kind":"forest","n":2.5},"geometry":{"type":"Polygon","coordinates":[[[600000,7499700],[600150,7499700],[600150,7499850],[600000,7499850],[600000,7499700]]]}},
{"type":"Feature","properties":{"kind":"crop","n":7},"geometry":{"type":"Polygon","coordinates":[[[600095,7499785],[600205,7499785],[600205,7499905],[600095,7499905],[600095,7499785]]]}}
]}"#,
        )
        .unwrap();
        let grid = Grid {
            wkt: SpatialRef::from_epsg(32723).unwrap().to_wkt().unwrap(),
            gt: [600000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0],
            w: 10,
            h: 10,
        };
        let none = AtomicBool::new(false);
        let read = |p: &Path| {
            let d = Dataset::open(p).unwrap();
            let b = d.rasterband(1).unwrap();
            (b.read_as::<f64>((0, 0), (10, 10), (10, 10), None).unwrap().data().to_vec(), d)
        };
        let run = |burn: Burn, all_touched: bool| {
            let out = dir.join("r.tif");
            let o = RasterizeOptions { layer: String::new(), burn, all_touched, grid: grid.clone() };
            let r = rasterize(&gj.to_string_lossy(), &o, &out, &|_, _| {}, &none).unwrap();
            (r, read(&out))
        };
        // rows 0-9 from y 7500000 down: the first square is rows 5-9 (y 7499850..7499700), columns 0-4
        let (r, (v, _)) = run(Burn::Attribute("kind".into()), false);
        assert_eq!(r.classes, vec![(1, "crop".into()), (2, "forest".into())]);
        assert_eq!(v[5 * 10], 2.0); // forest, outside the crop
        assert_eq!(v[3 * 10 + 3], 1.0); // crop (rows 3-6, columns 3-6), over the forest at (5, 3)
        assert_eq!(v[5 * 10 + 3], 1.0);
        assert_eq!(v[0], 0.0);
        let (_, (v, _)) = run(Burn::Attribute("n".into()), false);
        assert_eq!((v[5 * 10], v[3 * 10 + 3]), (2.5, 7.0));
        assert!(v[0].is_nan());
        let (r, (v, _)) = run(Burn::One, false);
        assert_eq!(r.kind, "mask");
        let inside = v.iter().filter(|&&x| x == 1.0).count();
        let (_, (vt, _)) = run(Burn::One, true);
        assert!(vt.iter().filter(|&&x| x == 1.0).count() > inside);
        let (_, (v, _)) = run(Burn::Index, false);
        assert_eq!((v[5 * 10], v[3 * 10 + 3]), (1.0, 2.0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
