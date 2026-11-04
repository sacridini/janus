//! The Python of Janus' own tools (rerun.py beside a run's provenance, as a Zeit run's): the same GDAL calls on the
//! same inputs, writing the outputs again in the run's folder's `python` subfolder (Janus' own are left alone). The
//! Python history shows them (history.rs); the self-test runs them and compares their outputs with Janus'.

use std::path::Path;

use janus_zeit::rasterize::{Burn, RasterizeOptions};

use crate::python::py_str;

fn py_list(v: &[String]) -> String {
    let mut o = String::from("[\n");
    for s in v {
        o += &format!("    {},\n", py_str(s));
    }
    o + "]"
}

/// A number as Python reads it back exactly.
fn num(v: f64) -> String {
    if v.is_nan() { "float(\"nan\")".into() } else { format!("{v:?}") }
}

/// The folder the script writes into: the run's `python` subfolder.
fn out_dir(run: &Path) -> String {
    py_str(&run.join("python").to_string_lossy())
}

/// Reproject, resample, clip (janus_zeit::warp::warp_files): gdal.Warp with gdalwarp's options, file by file, each
/// under its own name.
pub fn warp(files: &[String], args: &[String], ext: &str, run: &Path) -> String {
    format!(
        r#""""Reproject, resample, clip as Janus ran it: GDAL's gdalwarp (gdal.Warp) with the same options on each file,
written under its own name. Needs GDAL's Python bindings (osgeo)."""
import os

from osgeo import gdal

gdal.UseExceptions()

files = {files}
# gdalwarp's options, as Janus gave them
options = {args}
out_dir = {out}
ext = {ext}

os.makedirs(out_dir, exist_ok=True)
written = []
for i, f in enumerate(files):
    stem = os.path.splitext(os.path.basename(f))[0]
    out = os.path.join(out_dir, stem + ext)
    if out in written:  # two files of one name, from different folders
        out = os.path.join(out_dir, f"{{stem}}_{{i}}{{ext}}")
    written.append(out)
    gdal.Warp(out, f, options=options)
    print(out)
"#,
        files = py_list(files),
        args = py_list(args),
        out = out_dir(run),
        ext = py_str(&format!(".{ext}")),
    )
}

/// Rasterize on the series' grid (janus_zeit::rasterize): the features in reading order, in the grid's CRS, burned
/// by GDAL (a later one over an earlier one) with the value Janus burns: 1, the feature's number, or an attribute's
/// (a number as it is, a text as a class numbered from 1 in sorted order).
pub fn rasterize(vector: &str, opts: &RasterizeOptions, out_name: &str, run: &Path) -> String {
    let (mode, attribute) = match &opts.burn {
        Burn::One => ("one", String::new()),
        Burn::Index => ("index", String::new()),
        Burn::Attribute(a) => ("attribute", a.clone()),
    };
    let g = &opts.grid;
    let gt: Vec<String> = g.gt.iter().map(|&v| num(v)).collect();
    format!(
        r#""""Rasterize on the series' grid as Janus ran it: the layer's features in reading order, transformed to the
grid's CRS, burned by GDAL (a later feature over an earlier one) with 1, the feature's number (1, 2... counting every
feature read), or an attribute's value (a number as it is, a text as a class numbered from 1 in sorted order). Needs
GDAL's Python bindings (osgeo)."""
import os

from osgeo import gdal, ogr, osr

gdal.UseExceptions()

vector = {vector}
layer_name = {layer}  # "" : the first layer
mode = {mode}  # "one", "index" or "attribute"
attribute = {attribute}
all_touched = {all_touched}
# the series' grid: its CRS, geotransform and size
wkt = {wkt}
geotransform = ({gt})
width, height = {w}, {h}
out = os.path.join({out}, {name})

src = ogr.Open(vector)
layer = src.GetLayerByName(layer_name) if layer_name else src.GetLayer(0)
grid_srs = osr.SpatialReference()
grid_srs.ImportFromWkt(wkt)
grid_srs.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
to_grid = None
if layer.GetSpatialRef() is not None:
    layer_srs = layer.GetSpatialRef().Clone()
    layer_srs.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    to_grid = osr.CoordinateTransformation(layer_srs, grid_srs)

# the attribute: a number (real or integer) or a text
real = text = False
if mode == "attribute":
    defn = layer.GetLayerDefn()
    kind = defn.GetFieldDefn(defn.GetFieldIndex(attribute)).GetType()
    real = kind == ogr.OFTReal
    text = kind not in (ogr.OFTReal, ogr.OFTInteger, ogr.OFTInteger64)

geometries, values = [], []
for number, feature in enumerate(layer, start=1):
    geometry = feature.GetGeometryRef()
    if geometry is None:
        continue
    geometry = geometry.Clone()
    if to_grid is not None:
        geometry.Transform(to_grid)
    if mode == "one":
        value = 1.0
    elif mode == "index":
        value = float(number)
    else:
        if not feature.IsFieldSetAndNotNull(attribute):
            continue
        value = feature.GetFieldAsString(attribute) if text else feature.GetFieldAsDouble(attribute)
    geometries.append(geometry)
    values.append(value)

classes = sorted(set(values)) if text else []
burn = [float(classes.index(v) + 1) for v in values] if text else values
if mode == "one":
    dtype, init, nodata = gdal.GDT_Byte, 0.0, None
elif mode == "index":
    dtype, init, nodata = gdal.GDT_Int32, 0.0, 0.0
elif text:
    dtype, init, nodata = (gdal.GDT_Byte if len(classes) < 255 else gdal.GDT_Int32), 0.0, 0.0
elif real:
    dtype, init, nodata = gdal.GDT_Float64, float("nan"), float("nan")
else:
    dtype, init, nodata = gdal.GDT_Int32, float(-2**31), float(-2**31)

os.makedirs(os.path.dirname(out), exist_ok=True)
ds = gdal.GetDriverByName("GTiff").Create(
    out, width, height, 1, dtype, options=["TILED=YES", "COMPRESS=DEFLATE", "BIGTIFF=IF_SAFER"]
)
ds.SetGeoTransform(geotransform)
ds.SetProjection(wkt)
band = ds.GetRasterBand(1)
if nodata is not None:
    band.SetNoDataValue(nodata)
band.Fill(init)

# the features with their values in a layer in memory, burned in their order
memory = (ogr.GetDriverByName("MEM") or ogr.GetDriverByName("Memory")).CreateDataSource("")
burned = memory.CreateLayer("burn", grid_srs, ogr.wkbUnknown)
burned.CreateField(ogr.FieldDefn("burn", ogr.OFTReal))
for geometry, value in zip(geometries, burn):
    f = ogr.Feature(burned.GetLayerDefn())
    f.SetGeometry(geometry)
    f.SetField("burn", value)
    burned.CreateFeature(f)
gdal.RasterizeLayer(ds, [1], burned, options=["ATTRIBUTE=burn"] + (["ALL_TOUCHED=TRUE"] if all_touched else []))
if classes:
    band.SetCategoryNames([""] + classes)
ds = None
print(out, len(geometries), "features burned")
"#,
        vector = py_str(vector),
        layer = py_str(&opts.layer),
        mode = py_str(mode),
        attribute = py_str(&attribute),
        all_touched = if opts.all_touched { "True" } else { "False" },
        wkt = py_str(&g.wkt),
        gt = gt.join(", "),
        w = g.w,
        h = g.h,
        out = out_dir(run),
        name = py_str(out_name),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_python() {
        let run = Path::new("C:/runs/warp_1");
        let s = warp(&["a\\b c.tif".into()], &["-t_srs".into(), "EPSG:4326".into()], "tif", run);
        assert!(s.contains(r#""a\\b c.tif""#), "{s}");
        assert!(s.contains("gdal.Warp(out, f, options=options)"));
        assert!(s.contains("f\"{stem}_{i}{ext}\""));
        let g = janus_zeit::warp::Grid { wkt: "WKT \"x\"".into(), gt: [1.0, 30.0, 0.0, 2.0, 0.0, -30.0], w: 3, h: 4 };
        let o =
            RasterizeOptions { layer: String::new(), burn: Burn::Attribute("cls".into()), all_touched: true, grid: g };
        let s = rasterize("v.gpkg", &o, "v raster.tif", run);
        assert!(s.contains("geotransform = (1.0, 30.0, 0.0, 2.0, 0.0, -30.0)"), "{s}");
        assert!(s.contains(r#"wkt = "WKT \"x\"""#));
        assert!(s.contains("all_touched = True") && s.contains("mode = \"attribute\""));
    }
}
