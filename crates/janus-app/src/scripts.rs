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

/// The script written beside a run (rerun.py) and named its Python in the provenance.
pub fn attach(folder: &Path, p: &mut crate::provenance::Provenance, code: String) {
    if std::fs::write(folder.join(crate::provenance::SCRIPT), code).is_ok() {
        p.python = crate::provenance::SCRIPT.into();
    }
}

/// A Python dict of numbers to texts.
fn py_names(names: &[(i64, String)]) -> String {
    let items: Vec<String> = names.iter().map(|(k, n)| format!("{k}: {}", py_str(n))).collect();
    format!("{{{}}}", items.join(", "))
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// Polygonize (janus_zeit::polygonize): the patches through GDAL's polygonizer as Janus, measured on the CRS's
/// ellipsoid as GDAL measures them, into a GeoPackage.
pub fn polygonize(src: &str, opts: &janus_zeit::polygonize::PolyOptions, out_name: &str, run: &Path) -> String {
    let sieve = opts.min_area_ha > 0.0;
    let classes: Vec<(i64, String)> = opts.classes.clone();
    let mean_of = opts.mean_of.as_ref().map_or("None".to_string(), |(name, path, band)| {
        format!("({}, {}, {band})", py_str(name), py_str(&crate::provenance::absolute(path)))
    });
    format!(
        r#""""Polygonize as Janus ran it: the map's patches (pixels of one value connected 4- or 8-wise) as polygons in a
GeoPackage, each with its value, class, date (a map of years), area (ha) and perimeter (m) on the CRS's ellipsoid,
pixels and the mean of another map; the patches under a minimum area first merged into the neighbour they share
the longest border with (zeit.mmu_sieve, as Janus), those with none left out; the outlines simplified.
Needs numpy, rasterio, shapely, pyproj and geopandas{zeit_needs}."""
import datetime
import math
import os

import geopandas as gpd
import numpy as np
import rasterio
import rasterio.features
from pyproj import CRS, Transformer
from shapely.geometry import shape
{zeit_import}
src = {src}
band = {band}
connectivity = {conn}
min_area_ha = {min_area}
simplify_m = {simplify}
classes = {classes}  # class names by value
years = {years}  # the values are (decimal) years: a date column
mean_of = {mean_of}  # (name, file, band) of a map of the same grid
layer = {layer}
out = os.path.join({out}, {name})
{body}"#,
        zeit_needs = if sieve { " (zeit for the minimum area)" } else { "" },
        zeit_import = if sieve { "import zeit\n" } else { "" },
        src = py_str(&crate::provenance::absolute(src)),
        band = opts.band.max(1),
        conn = if opts.eight { 8 } else { 4 },
        min_area = num(opts.min_area_ha),
        simplify = num(opts.simplify_m),
        classes = py_names(&classes),
        years = py_bool(opts.years),
        layer = py_str(if opts.layer.is_empty() { "polygons" } else { &opts.layer }),
        out = out_dir(run),
        name = py_str(out_name),
        body = POLYGONIZE_BODY,
    )
}

const POLYGONIZE_BODY: &str = r##"
with rasterio.open(src) as ds:
    values = ds.read(band).astype("float64")
    nodata = ds.nodatavals[band - 1]
    transform, (h, w) = ds.transform, ds.shape
    crs = CRS.from_wkt(ds.crs.to_wkt()) if ds.crs else None
if nodata is not None and not np.isnan(nodata):
    values[values == nodata] = np.nan

# measures on the CRS's ellipsoid (Karney's geodesics, as GDAL's OGR_G_GeodesicArea), the rings in its longitudes
# and latitudes; without a CRS, on the plane
geod = crs.get_geod() if crs is not None else None
to_lonlat = None
if geod is not None and not crs.is_geographic:
    to_lonlat = Transformer.from_crs(crs, crs.geodetic_crs, always_xy=True)


def ring(coords):
    """A ring's area (m2) and perimeter (m) on the ellipsoid; its points in the map's CRS, closed."""
    xy = np.asarray(coords, dtype="float64")[:-1]
    x, y = xy[:, 0], xy[:, 1]
    if to_lonlat is not None:
        x, y = to_lonlat.transform(x, y)
    area, perimeter = geod.polygon_area_perimeter(x, y)
    return abs(area), perimeter


def measure(g):
    """A polygon's area (m2, its holes out) and perimeter (m, its holes' too)."""
    if geod is None:
        return g.area, g.boundary.length
    area, perimeter = ring(g.exterior.coords)
    for r in g.interiors:
        a, p = ring(r.coords)
        area, perimeter = area - a, perimeter + p
    return area, perimeter


def pixel_area():
    """The area (m2) of one pixel at the map's centre."""
    planar = abs(transform.a * transform.e - transform.b * transform.d)
    if geod is None:
        return planar
    cx, cy = w // 2, h // 2
    a, _ = ring([transform * (cx + dx, cy + dy) for dx, dy in ((0, 0), (1, 0), (1, 1), (0, 1), (0, 0))])
    return a if math.isfinite(a) and a > 0 else planar


def date_text(v):
    """A decimal year as a date (YYYY-MM-DD), a whole year as itself."""
    if v == int(v):
        return "%d" % v
    y = math.floor(v)
    start = datetime.date(y, 1, 1)
    days = (datetime.date(y + 1, 1, 1) - start).days
    return (start + datetime.timedelta(days=math.floor((v - y) * days))).isoformat()


min_pixels = math.ceil(min_area_ha * 1e4 / pixel_area()) if min_area_ha > 0 else 0
if min_pixels >= 2:
    # the patches under min_pixels merged into the neighbour they share the longest border with
    values = np.asarray(zeit.mmu_sieve(values, min_pixels, connectivity=connectivity), dtype="float64")

# the patches: each value's pixels connected 4- or 8-wise, as polygons (GDAL's polygonizer, as Janus)
valid = np.isfinite(values)
kinds, code = np.unique(values[valid], return_inverse=True)
codes = np.zeros(values.shape, dtype="int32")
codes[valid] = code.ravel() + 1
found = list(rasterio.features.shapes(codes, mask=valid, connectivity=connectivity, transform=transform))
polygons = [shape(g) for g, _ in found]
value = [float(kinds[int(c) - 1]) for _, c in found]
n = len(polygons)
# each pixel's patch (its polygon burned back), for the pixels and the other map's mean
patch = np.zeros((h, w), dtype="int32")
if n:
    patch = rasterio.features.rasterize(((g, i + 1) for i, g in enumerate(polygons)), out_shape=(h, w),
                                        transform=transform, fill=0, dtype="int32")
pixels = np.bincount(patch.ravel(), minlength=n + 1)[1:]
means, mean_column = np.full(n, np.nan), None
if mean_of is not None:
    name, path, b = mean_of
    with rasterio.open(path) as ds:
        other = ds.read(b).astype("float64")
        nd = ds.nodatavals[b - 1]
    if other.shape != values.shape:
        raise SystemExit(f"{path} is {other.shape[1]} x {other.shape[0]}, the map {w} x {h}: not the same grid")
    if nd is not None and not np.isnan(nd):
        other[other == nd] = np.nan
    ok = (patch > 0) & np.isfinite(other)
    sums = np.bincount(patch[ok], weights=other[ok], minlength=n + 1)[1:]
    counts = np.bincount(patch[ok], minlength=n + 1)[1:]
    with np.errstate(invalid="ignore", divide="ignore"):
        means = np.where(counts > 0, sums / np.maximum(counts, 1), np.nan)
    mean_column = "mean_" + "".join(ch.lower() if ch.isalnum() else "_" for ch in name).strip("_")

tolerance = simplify_m / 111320.0 if crs is not None and crs.is_geographic else simplify_m
rows, geometries, dropped = [], [], 0
for i, g in enumerate(polygons):
    if min_pixels >= 2 and pixels[i] < min_pixels:
        dropped += 1  # under the minimum area with no neighbour to join
        continue
    if tolerance > 0:
        g = g.simplify(tolerance, preserve_topology=True)
    area, perimeter = measure(g)
    v = value[i]
    row = {"value": v}
    if classes:
        row["class"] = classes.get(int(round(v))) if v == int(v) else None
    if years:
        row["date"] = date_text(v)
    row.update(area_ha=area / 1e4, perimeter_m=perimeter, pixels=int(pixels[i]))
    if mean_column:
        row[mean_column] = None if np.isnan(means[i]) else float(means[i])
    rows.append(row)
    geometries.append(g)

os.makedirs(os.path.dirname(out), exist_ok=True)
frame = gpd.GeoDataFrame(rows, geometry=geometries, crs=crs.to_wkt() if crs is not None else None)
frame.to_file(out, layer=layer, driver="GPKG")
print(out, len(rows), "polygons,", dropped, "left out")
"##;

/// Zonal statistics over time (janus_zeit::zonal): each feature burned alone on the grid (its pixels, whose centre
/// is inside or every one touched), each map's values in it: statistics, or each class's (year's) pixels and true
/// area, and the transitions; the same CSVs.
#[allow(clippy::too_many_arguments)]
pub fn zonal(
    vector: &str,
    grid: &janus_zeit::warp::Grid,
    sources: &[janus_zeit::areas::AreaSource],
    opts: &janus_zeit::zonal::ZonalOptions,
    stat: usize,
    names: &[(i64, String)],
    run: &Path,
) -> String {
    use janus_zeit::zonal::ZonalKind;
    let kind = match opts.kind {
        ZonalKind::Continuous => "values",
        ZonalKind::Classes => "classes",
        ZonalKind::Years => "years",
    };
    let maps: Vec<String> = sources
        .iter()
        .map(|s| {
            format!(
                "    ({}, {}, {}),\n",
                py_str(&crate::provenance::absolute(&s.path)),
                s.band.max(1),
                py_str(&s.label)
            )
        })
        .collect();
    let gt: Vec<String> = grid.gt.iter().map(|&v| num(v)).collect();
    let mut names = names.to_vec();
    names.sort();
    format!(
        r#""""Zonal statistics over time as Janus ran them: each feature of the layer is a zone, its pixels on the maps'
grid those whose centre is inside it (or every pixel it touches), each feature burned alone (a pixel in several
features counts in each); through each map: the number of values, mean, standard deviation, least, percentiles
10, 25, 50, 75 and 90 (numpy's, linear) and largest of the values; or each class's (each year's) pixels and area
(each pixel's true area on the CRS's ellipsoid, as Janus computes it) and the transitions between two maps.
Writes zonal_long.csv, zonal_wide.csv and transitions.csv as Janus. Needs GDAL's Python bindings (osgeo; 3.9 or
later for the geodesic areas) and numpy."""
import bisect
import math
import os

import numpy as np
from osgeo import gdal, ogr, osr

gdal.UseExceptions()

vector = {vector}
layer_name = {layer}  # "" : the first layer
id_field = {id_field}  # "" : the zones named by their FIDs
all_touched = {all_touched}
kind = {kind}  # "values", "classes" or "years"
window = {window}  # (x0, y0, x1, y1) of the grid, or None: all of it
transitions = {transitions}  # (from, to): indexes in maps, or None
stat = {stat}  # the statistic of zonal_wide.csv (values), its index in STATS
# the maps' grid: its CRS, geotransform and size
wkt = {wkt}
geotransform = ({gt})
width, height = {w}, {h}
# each map: (file, band, label)
maps = [
{maps}]
names = {names}  # class names by value
out_dir = {out}
{body}"#,
        vector = py_str(&crate::provenance::absolute(vector)),
        layer = py_str(&opts.layer),
        id_field = py_str(&opts.id_field),
        all_touched = py_bool(opts.all_touched),
        kind = py_str(kind),
        window = opts.window.map_or("None".into(), |w| format!("({}, {}, {}, {})", w[0], w[1], w[2], w[3])),
        transitions = opts.transitions.map_or("None".into(), |(a, b)| format!("({a}, {b})")),
        stat = stat.min(9),
        wkt = py_str(&grid.wkt),
        gt = gt.join(", "),
        w = grid.w,
        h = grid.h,
        maps = maps.concat(),
        names = py_names(&names),
        out = out_dir(run),
        body = ZONAL_BODY,
    )
}

const ZONAL_BODY: &str = r##"
STATS = ["n", "mean", "std", "min", "p10", "p25", "median", "p75", "p90", "max"]
# the projection methods whose map area is the ellipsoid's
EQUAL_AREA = ("equal_area", "albers", "mollweide", "sinusoidal", "eckert_iv", "eckert_vi", "homolosine",
              "equal_earth", "quartic_authalic")

grid_srs = None
if wkt:
    grid_srs = osr.SpatialReference()
    grid_srs.ImportFromWkt(wkt)
    grid_srs.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
src = ogr.Open(vector)
layer = src.GetLayerByName(layer_name) if layer_name else src.GetLayer(0)
to_grid = None
if grid_srs is not None and layer.GetSpatialRef() is not None:
    layer_srs = layer.GetSpatialRef().Clone()
    layer_srs.SetAxisMappingStrategy(osr.OAMS_TRADITIONAL_GIS_ORDER)
    to_grid = osr.CoordinateTransformation(layer_srs, grid_srs)

# the zones: the features in reading order, in the grid's CRS
fids, ids, geometries = [], [], []
for f in layer:
    fid = f.GetFID()
    fids.append(fid)
    if id_field:
        ids.append(f.GetFieldAsString(id_field) if f.IsFieldSetAndNotNull(id_field) else "")
    else:
        ids.append(str(fid))
    g = f.GetGeometryRef()
    if g is None or g.IsEmpty():
        geometries.append(None)
        continue
    g = g.Clone()
    if to_grid is not None:
        g.Transform(to_grid)
    geometries.append(g)
zones = len(fids)

# each feature's extent on the grid (a pixel around), within the window; the window read: all of them
wx0, wy0, wx1, wy1 = window or (0, 0, width, height)
gt = geotransform
det = gt[1] * gt[5] - gt[2] * gt[4]


def pixel(x, y):
    dx, dy = x - gt[0], y - gt[3]
    return (gt[5] * dx - gt[2] * dy) / det, (-gt[4] * dx + gt[1] * dy) / det


def clamp(v, lo, hi):
    return int(min(max(v, lo), hi))


def extent(g):
    minx, maxx, miny, maxy = g.GetEnvelope()
    corners = [pixel(minx, miny), pixel(minx, maxy), pixel(maxx, miny), pixel(maxx, maxy)]
    c0, r0 = min(c for c, _ in corners), min(r for _, r in corners)
    c1, r1 = max(c for c, _ in corners), max(r for _, r in corners)
    e = (clamp(math.floor(c0) - 1, wx0, wx1), clamp(math.floor(r0) - 1, wy0, wy1),
         clamp(math.ceil(c1) + 1, wx0, wx1), clamp(math.ceil(r1) + 1, wy0, wy1))
    return e if e[2] > e[0] and e[3] > e[1] else None


extents = [extent(g) if g is not None else None for g in geometries]
on = [e for e in extents if e]
if not on:
    raise SystemExit("no feature on the maps' grid (within the window)")
x0, y0 = min(e[0] for e in on), min(e[1] for e in on)
x1, y1 = max(e[2] for e in on), max(e[3] for e in on)
w, h = x1 - x0, y1 - y0

# each zone's pixels (their places in the window), each feature burned alone on its extent
memory = (ogr.GetDriverByName("MEM") or ogr.GetDriverByName("Memory")).CreateDataSource("")
pixels_of = []
for z in range(zones):
    if extents[z] is None:
        pixels_of.append(np.zeros(0, dtype=np.int64))
        continue
    ex0, ey0, ex1, ey1 = extents[z]
    ds = gdal.GetDriverByName("MEM").Create("", ex1 - ex0, ey1 - ey0, 1, gdal.GDT_Byte)
    ds.SetGeoTransform((gt[0] + ex0 * gt[1] + ey0 * gt[2], gt[1], gt[2],
                        gt[3] + ex0 * gt[4] + ey0 * gt[5], gt[4], gt[5]))
    if wkt:
        ds.SetProjection(wkt)
    one = memory.CreateLayer("zone%d" % z, grid_srs, ogr.wkbUnknown)
    f = ogr.Feature(one.GetLayerDefn())
    f.SetGeometry(geometries[z])
    one.CreateFeature(f)
    gdal.RasterizeLayer(ds, [1], one, burn_values=[1], options=["ALL_TOUCHED=TRUE"] if all_touched else [])
    rr, cc = np.nonzero(ds.GetRasterBand(1).ReadAsArray() == 1)
    pixels_of.append((rr + ey0 - y0) * w + (cc + ex0 - x0))
pixels = [len(p) for p in pixels_of]


def nodes(n, step):
    v = list(range(0, n, max(step, 1)))
    if v[-1] != n - 1:
        v.append(n - 1)
    return v


def between(ns, x):
    """The sample before x and the weight of the one after it."""
    if len(ns) < 2:
        return 0, 0.0
    i = min(max(bisect.bisect_right(ns, x) - 1, 0), len(ns) - 2)
    return i, (x - ns[i]) / (ns[i + 1] - ns[i])


def pixel_areas():
    """Each pixel's area (m2) over the window: in an equal-area projection the geotransform's; else geodesic on the
    CRS's ellipsoid at up to 129 x 129 pixels across it, interpolated between them (as Janus)."""
    flat = abs(det)
    if grid_srs is None:
        return np.full((h, w), flat)
    if not grid_srs.IsGeographic():
        units = grid_srs.GetLinearUnits()
        method = (grid_srs.GetAttrValue("PROJECTION") or "").lower()
        if any(m in method for m in EQUAL_AREA):
            return np.full((h, w), flat * units * units)
    step = max(-(-max(w, h) // 128), 1)
    xs, ys = nodes(w, step), nodes(h, step)
    samples = np.zeros((len(ys), len(xs)))
    for j, y in enumerate(ys):
        for i, x in enumerate(xs):
            r = ogr.Geometry(ogr.wkbLinearRing)
            for dx, dy in ((0, 0), (1, 0), (1, 1), (0, 1), (0, 0)):
                c, rr = x0 + x + dx, y0 + y + dy
                r.AddPoint_2D(gt[0] + c * gt[1] + rr * gt[2], gt[3] + c * gt[4] + rr * gt[5])
            poly = ogr.Geometry(ogr.wkbPolygon)
            poly.AddGeometry(r)
            poly.AssignSpatialReference(grid_srs)
            samples[j, i] = abs(poly.GeodesicArea())
    out = np.zeros((h, w))
    at = [between(xs, x) for x in range(w)]
    ix = np.array([a[0] for a in at])
    tx = np.array([a[1] for a in at])
    nx = np.minimum(ix + 1, len(xs) - 1)
    for y in range(h):
        j, ty = between(ys, y)
        a = samples[j]
        col = a + (samples[min(j + 1, len(ys) - 1)] - a) * ty if ty > 0 else a
        out[y] = np.where(tx > 0, col[ix] + (col[nx] - col[ix]) * tx, col[ix])
    return out


def statistics(v):
    """A zone's statistics: its values as float32, sorted, summed in order (as Janus)."""
    s = [float(len(v))] + [math.nan] * 9
    if len(v) == 0:
        return s
    v = np.sort(v.astype(np.float32)).astype(np.float64)
    n = float(len(v))
    mean = np.cumsum(v)[-1] / n
    var = np.cumsum((v - mean) * (v - mean))[-1] / n
    s[1], s[2], s[3], s[9] = mean, math.sqrt(var), v[0], v[-1]
    for k, q in enumerate((0.1, 0.25, 0.5, 0.75, 0.9)):
        pos = q * (len(v) - 1)
        lo, hi = math.floor(pos), math.ceil(pos)
        s[4 + k] = v[lo] + (v[hi] - v[lo]) * (pos - lo)
    return [float(x) for x in s]


def read(path, band):
    """A map over the window as float64, its no data NaN."""
    ds = gdal.Open(path)
    if (ds.RasterXSize, ds.RasterYSize) != (width, height):
        raise SystemExit(f"{path} is not on the grid ({width} x {height})")
    b = ds.GetRasterBand(band)
    a = b.ReadAsArray(x0, y0, w, h).astype("float64").ravel()
    nd = b.GetNoDataValue()
    if nd is not None and not math.isnan(nd):
        a[a == nd] = np.nan
    return a


def key(v):
    """A class (the value rounded, halves away from zero) or a year (its whole part)."""
    if kind == "years":
        return np.floor(v).astype(np.int64)
    return np.where(v >= 0, np.floor(v + 0.5), np.ceil(v - 0.5)).astype(np.int64)


labels = [m[2] for m in maps]
areas = pixel_areas().ravel() if kind != "values" else None
stats, counts = [], []
for path, band, label in maps:
    v = read(path, band)
    if kind == "values":
        stats.append([statistics(v[p][np.isfinite(v[p])]) for p in pixels_of])
        continue
    c = []
    for z, p in enumerate(pixels_of):
        x = v[p]
        ok = np.isfinite(x)
        k, a = key(x[ok]), areas[p][ok]
        for value in np.unique(k):
            sel = k == value
            c.append((z, int(value), int(sel.sum()), float(a[sel].sum())))
    counts.append(c)
moves = []
if kind == "classes" and transitions and max(transitions) < len(maps):
    first, last = (read(maps[i][0], maps[i][1]) for i in transitions)
    for z, p in enumerate(pixels_of):
        xa, xb = first[p], last[p]
        ok = np.isfinite(xa) & np.isfinite(xb)
        ka, kb, a = key(xa[ok]), key(xb[ok]), areas[p][ok]
        for fa, tb in (np.unique(np.stack([ka, kb], axis=1), axis=0) if ka.size else []):
            sel = (ka == fa) & (kb == tb)
            moves.append((z, int(fa), int(tb), int(sel.sum()), float(a[sel].sum())))
values = sorted({c[1] for cs in counts for c in cs})


def fmt(x):
    """A number as Janus writes it (empty without a value)."""
    if math.isnan(x):
        return ""
    s = repr(float(x))
    if "e" in s:
        s = np.format_float_positional(float(x), trim="-")
    return s[:-2] if s.endswith(".0") else s


def column(s):
    return "".join(ch.lower() if ch.isalnum() else "_" for ch in s).strip("_")


def q(s):
    return '"' + s.replace('"', "'") + '"'


def name_of(v):
    return names.get(v, str(v))


def write(name, lines):
    with open(os.path.join(out_dir, name), "w", encoding="utf-8", newline="\n") as f:
        f.write("".join(line + "\n" for line in lines))
    print(os.path.join(out_dir, name))


os.makedirs(out_dir, exist_ok=True)
long = []
if kind == "values":
    long.append("id,fid,date," + ",".join(STATS))
    for s, label in enumerate(labels):
        for z in range(zones):
            long.append(f"{q(ids[z])},{fids[z]},{label}," + ",".join(fmt(x) for x in stats[s][z]))
else:
    long.append(f"id,fid,date,value,{'year' if kind == 'years' else 'class'},pixels,area_m2,area_ha,percent")
    for s, label in enumerate(labels):
        total = {}
        for z, v, p, a in counts[s]:
            total[z] = total.get(z, 0.0) + a
        for z, v, p, a in counts[s]:
            share = 100 * a / total[z] if total[z] > 0 else 0.0
            long.append(f"{q(ids[z])},{fids[z]},{label},{v},{q(name_of(v))},{p},{a:.3f},{a / 1e4:.6f},{share:.6f}")
write("zonal_long.csv", long)

if kind == "values":
    columns = [f"{STATS[stat]}_{column(label)}" for label in labels]
    rows = [[stats[s][z][stat] for s in range(len(labels))] for z in range(zones)]
else:
    columns, rows = [], [[] for _ in range(zones)]
    for s, label in enumerate(labels):
        for v in values:
            name = str(v) if kind == "years" else column(name_of(v))
            columns.append(f"ha_{name}" if len(labels) == 1 else f"ha_{name}_{column(label)}")
            for row in rows:
                row.append(0.0)
            for z, cv, _, a in counts[s]:
                if cv == v:
                    rows[z][-1] = a / 1e4
write("zonal_wide.csv", ["id,fid,pixels," + ",".join(columns)]
      + [f"{q(ids[z])},{fids[z]},{pixels[z]}," + ",".join(fmt(x) for x in rows[z]) for z in range(zones)])
if moves:
    write("transitions.csv", ["id,fid,from,from_class,to,to_class,pixels,area_m2,area_ha"]
          + [f"{q(ids[z])},{fids[z]},{a},{q(name_of(a))},{b},{q(name_of(b))},{p},{m2:.3f},{m2 / 1e4:.6f}"
             for z, a, b, p, m2 in moves])
"##;

/// Regularize series (janus_zeit::regularize): the series read as Janus reads it over the window, each period's
/// median, medoid or clearest observation, the gaps filled, a GeoTIFF per period.
pub fn regularize(info: &janus_cube::CubeInfo, opts: &janus_zeit::regularize::RegOptions, run: &Path) -> String {
    use janus_zeit::regularize::{Composite, Fill, Step, periods};
    let times: Vec<f64> = info.layers.iter().map(|l| l.time).collect();
    let ps = periods(&times, opts.step);
    let [x0, y0, x1, y1] = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let ymd = |d: i64| {
        let (y, m, dd) = janus_cube::dates::civil_from_days(d);
        format!("{y:04}{m:02}{dd:02}")
    };
    let mut periods_py = String::from("[\n");
    for p in &ps {
        let ts: Vec<String> = p.dates.iter().map(|t| t.to_string()).collect();
        periods_py += &format!("    ({}, {}, [{}]),\n", py_str(&ymd(p.start)), p.start, ts.join(", "));
    }
    periods_py += "]";
    let names: Vec<String> = if opts.all_bands {
        (0..info.bands_per_date.max(1))
            .map(|b| info.band_names.get(b).cloned().unwrap_or_else(|| format!("band {}", b + 1)))
            .collect()
    } else {
        vec![info.selection_text()]
    };
    let bases: Vec<String> = info.layers.iter().map(|l| l.band_base.to_string()).collect();
    let (fill, lambda) = match opts.fill {
        Fill::None => ("none", 0.0),
        Fill::Linear => ("linear", 0.0),
        Fill::Whittaker(l) => ("whittaker", l),
    };
    let step = match opts.step {
        Step::Days(n) => format!("every {n} days from the first date"),
        Step::Month => "calendar months".into(),
        Step::Year => "calendar years".into(),
    };
    format!(
        r#""""Regularize series as Janus ran it: the series read from its files over the window as Janus reads it, a
composite per period ({step}): each band's median, the medoid (the observation nearest the bands' medians) or the
clearest date's observation; the empty periods filled (linear, or by a Whittaker curve) up to a gap; a GeoTIFF per
period. The whole window is read into memory. Needs numpy and rasterio."""
import os
import warnings

import numpy as np
import rasterio
from rasterio.enums import Resampling
from rasterio.windows import Window

warnings.simplefilter("ignore", RuntimeWarning)  # medians of periods without a value
{series}
# The window: columns {x0}..{x1}, rows {y0}..{y1} of the files' pixels.
window = Window({x0}, {y0}, {w}, {h})
all_bands = {all_bands}  # every band of the dates, else the quantity shown (with its quality mask)
n_bands = {n_bands}
bases = [{bases}]  # each date's first band in its file, less 1
qa_at = {qa_at}  # the quality band's place in a layer's entry
band_names = {band_names}
composite = {composite}  # "median", "medoid" or "clearest"
fill_method = {fill}  # "none", "linear" or "whittaker"
lam = {lambda}  # the Whittaker curve's lambda
max_gap_days = {max_gap}  # a gap is filled when its values around are at most this many days apart (0: any)
# Each period: (its first day, as days since 1970-01-01, the dates in it), as pandas' resample gives them
periods = {periods}
x = np.array([p[1] for p in periods], dtype="float64")
stem = {stem}
out_dir = {out}
{body}"#,
        series = crate::python::series_code(info),
        w = x1 - x0,
        h = y1 - y0,
        all_bands = py_bool(opts.all_bands),
        n_bands = info.bands_per_date.max(1),
        bases = bases.join(", "),
        qa_at = if info.expr.is_some() { 2 } else { 3 },
        band_names = py_list(&names),
        composite = py_str(match opts.composite {
            Composite::Median => "median",
            Composite::Medoid => "medoid",
            Composite::Clearest => "clearest",
        }),
        fill = py_str(fill),
        lambda = num(lambda),
        max_gap = num(opts.max_gap_days),
        periods = periods_py,
        stem = py_str(&opts.stem),
        out = out_dir(run),
        body = REGULARIZE_BODY,
    )
}

const REGULARIZE_BODY: &str = r##"

def date_values(t):
    """Date t over the window as Janus reads it, as float32: (bands, y, x)."""
    layer = layers[t]
    if not all_bands:
        return shown(layer, window)[None].astype("float32")
    v = np.stack([read(layer[0], bases[t] + b + 1, window) for b in range(n_bands)])
    if layer[qa_at]:
        v[:, ~usable(read(layer[0], layer[qa_at], window, raw=True), qa_rule)] = np.nan
    return v.astype("float32")


def whittaker(y):
    """The Whittaker curve (zeit's: Eilers 2003, divided differences for uneven x, scaled by the median step) of each
    column of y (periods, pixels) at x, its no data weighing 0: the same operations in the same order as Janus'."""
    n = y.shape[0]
    ok = np.isfinite(y)
    if n < 3 or lam <= 0:
        return np.where(ok, y, np.nan)
    wy = np.where(ok, y, 0.0)
    steps = np.diff(x)
    h0 = np.sort(steps)[len(steps) // 2]
    h0 = h0 if h0 > 0 else 1.0
    a0, a1, a2 = ok.astype("float64"), np.zeros(n), np.zeros(n)
    for k in range(n - 2):
        h1, h2 = steps[k] / h0, steps[k + 1] / h0
        c = (2.0 / (h1 * (h1 + h2)), -2.0 / (h1 * h2), 2.0 / (h2 * (h1 + h2)))
        for p in range(3):
            a0[k + p] += lam * c[p] * c[p]
            if p < 2:
                a1[k + p] += lam * c[p] * c[p + 1]
        a2[k] += lam * c[0] * c[2]
    d, l1, l2 = np.zeros(y.shape), np.zeros(y.shape), np.zeros(y.shape)
    bad = np.zeros(y.shape[1:], dtype=bool)
    for i in range(n):
        if i >= 2:
            l2[i] = a2[i - 2] / d[i - 2]
        if i >= 1:
            l1[i] = (a1[i - 1] - (l2[i] * l1[i - 1] if i >= 2 else 0.0)) / d[i - 1]
        s = a0[i] - l1[i] * l1[i] - l2[i] * l2[i]
        bad |= ~(s > 0)
        d[i] = np.sqrt(np.where(s > 0, s, 1.0))
    u = np.zeros(y.shape)
    for i in range(n):
        s = wy[i]
        if i >= 1:
            s = s - l1[i] * u[i - 1]
        if i >= 2:
            s = s - l2[i] * u[i - 2]
        u[i] = s / d[i]
    z = np.zeros(y.shape)
    for i in reversed(range(n)):
        s = u[i]
        if i + 1 < n:
            s = s - l1[i + 1] * z[i + 1]
        if i + 2 < n:
            s = s - l2[i + 2] * z[i + 2]
        z[i] = s / d[i]
    z[:, bad | (ok.sum(axis=0) < 2)] = np.nan
    return z


def fill(y):
    """The gaps of each pixel's regular series (periods, pixels) filled where the values around them are at most
    max_gap_days apart (0: any): linear in time, or the Whittaker curve; the values kept, the ends left empty."""
    if fill_method == "none":
        return y
    n = y.shape[0]
    ok = np.isfinite(y)
    idx = np.arange(n)[:, None]
    before = np.maximum.accumulate(np.where(ok, idx, -1), axis=0)
    after = np.minimum.accumulate(np.where(ok, idx, n)[::-1], axis=0)[::-1]
    gap = ~ok & (before >= 0) & (after < n)
    a, b = np.clip(before, 0, n - 1), np.clip(after, 0, n - 1)
    xa, xb = x[a], x[b]
    if max_gap_days > 0:
        gap &= ~(xb - xa > max_gap_days)
    out = y.copy()
    cols = np.nonzero(gap.any(axis=0))[0]
    if cols.size == 0:
        return out
    if fill_method == "linear":
        ya, yb = np.take_along_axis(y, a, axis=0), np.take_along_axis(y, b, axis=0)
        with np.errstate(all="ignore"):
            v = ya + (yb - ya) * (x[:, None] - xa) / (xb - xa)
        out[gap] = v[gap]
    else:
        z = whittaker(y[:, cols])
        sub = out[:, cols]
        sub[gap[:, cols]] = z[gap[:, cols]]
        out[:, cols] = sub
    return out


with np.errstate(all="ignore"):
    values = np.stack([date_values(t) for t in range(len(layers))])  # (time, band, y, x)
bands, h, w = values.shape[1], values.shape[2], values.shape[3]
rank = []
if composite == "clearest":
    # the dates clearest first: the share of their pixels with a value, read at a reduced size as Janus (where two
    # dates' shares are close Janus' reading may rank them otherwise)
    s = min(256 / max(w, h), 1.0)
    size = (max(int(h * s), 1), max(int(w * s), 1))
    with np.errstate(all="ignore"):
        share = [np.isfinite(shown(layer, window, size)).mean() for layer in layers]
    rank = sorted(range(len(layers)), key=lambda t: (-share[t], t))

out = np.full((len(periods), bands, h, w), np.nan)
for k, (start, day, ts) in enumerate(periods):
    if not ts:
        continue
    obs = values[ts].astype("float64")  # (dates, band, y, x)
    if composite == "median":
        out[k] = np.nanmedian(obs, axis=0)
    elif composite == "medoid":
        # each band's median, then the complete observation nearest them (the first of equal ones)
        med = np.nanmedian(obs, axis=0)
        d = np.zeros((obs.shape[0], h, w))
        for b in range(bands):
            d = d + (obs[:, b] - med[b]) * (obs[:, b] - med[b])
        d[~np.all(np.isfinite(obs), axis=1)] = np.inf
        best = np.argmin(d, axis=0)
        pick = np.take_along_axis(obs, best[None, None], axis=0)[0]
        out[k] = np.where(np.isfinite(np.min(d, axis=0)), pick, np.nan)
    else:
        # the observation of the clearest date that has one (every band)
        done = np.zeros((h, w), dtype=bool)
        for t in [t for t in rank if t in ts]:
            o = values[t].astype("float64")
            here = np.all(np.isfinite(o), axis=0) & ~done
            out[k][:, here] = o[:, here]
            done |= here
for b in range(bands):
    out[:, b] = fill(out[:, b].reshape(len(periods), -1)).reshape(len(periods), h, w)

os.makedirs(out_dir, exist_ok=True)
with rasterio.open(layers[0][0]) as src:
    crs, transform = src.crs, src.window_transform(window)
for k, (start, day, ts) in enumerate(periods):
    path = os.path.join(out_dir, f"{stem}_{start}.tif")
    with rasterio.open(path, "w", driver="GTiff", width=w, height=h, count=bands, dtype="float32", crs=crs,
                       transform=transform, nodata=float("nan"), tiled=True, compress="deflate") as dst:
        dst.write(out[k].astype("float32"))
        for b, name in enumerate(band_names):
            dst.set_band_description(b + 1, name)
    print(path)
"##;

/// The raster calculator (janus_cube::calc::plan) in its simple cases: no reduction over time, every layer on the
/// result's grid. Each date's inputs as its VRT reads them, the expression as numpy, a GeoTIFF per date. None
/// otherwise (the run then kept as data).
pub fn calculator(
    text: &str,
    plan: &janus_cube::calc::plan::Plan,
    layers: &[janus_cube::calc::plan::CalcLayer],
    opts: &janus_cube::calc::plan::Options,
    run: &Path,
) -> Option<String> {
    use janus_cube::calc::plan::{Key, Placement, date_stem, date_vars, match_date, placement};
    if !plan.stages.is_empty() || !plan.periods.is_empty() {
        return None;
    }
    let grid = &layers.get(plan.grid)?.info;
    let gt = grid.geo_transform?;
    for &l in &plan.used {
        if placement(&layers.get(l)?.info, grid).ok()? != Placement::Same {
            return None;
        }
    }
    let source = |layer: usize, band: i32, raw: bool, t: usize| {
        let l = &layers[layer].info.layers[t];
        let file_band = if band <= 0 { l.band } else { band + l.band_base };
        format!("({}, {file_band}, {})", py_str(&crate::provenance::absolute(&l.path)), py_bool(raw))
    };
    let rinfo = plan.series.map(|r| plan.series_info(layers, r));
    let mut dates = String::from("[\n");
    for (t, (time, _)) in plan.dates(layers).iter().enumerate() {
        let mut srcs = vec![];
        for k in &plan.inputs {
            srcs.push(match k {
                Key::Band { layer, band, raw, fixed: None } if layers[*layer].info.t() > 1 => {
                    let dated = rinfo.as_ref().is_some_and(|r| r.time_is_date);
                    match rinfo.as_ref().and_then(|_| match_date(&layers[*layer].info, *time, t, dated, opts.matching))
                    {
                        Some(d) => source(*layer, *band, *raw, d),
                        None if rinfo.is_none() => source(*layer, *band, *raw, 0),
                        None => "None".into(),
                    }
                }
                Key::Band { layer, band, raw, fixed } => source(*layer, *band, *raw, fixed.unwrap_or(0)),
                Key::Stage(_) => return None,
            });
        }
        let vars = rinfo.as_ref().map_or_else(janus_cube::calc::DateVars::default, |r| date_vars(r, t));
        let v: Vec<String> = vars.0.iter().map(|&x| num(f64::from(x))).collect();
        let stem = rinfo.as_ref().map_or_else(|| "result".to_string(), |r| date_stem(r, t));
        dates += &format!("    ({}, ({}), [{}]),\n", py_str(&stem), v.join(", "), srcs.join(", "));
    }
    dates += "]";
    let inputs: Vec<String> = plan
        .inputs
        .iter()
        .enumerate()
        .map(|(k, key)| match key {
            Key::Band { layer, band, raw, fixed } => format!(
                "#   b[{k}]: \"{}\" ({}), {}{}{}",
                layers[*layer].name,
                layers[*layer].alias,
                if *band <= 0 { "the band it shows".to_string() } else { format!("band {band} of each date") },
                if *raw { ", quality codes as they are" } else { "" },
                fixed.map_or(String::new(), |t| format!(", its image {}", t + 1))
            ),
            Key::Stage(_) => String::new(),
        })
        .collect();
    let (stmts, r) = janus_cube::calc::python::numpy(&plan.program, &|k| format!("b[{k}]"), "    ");
    let gts: Vec<String> = gt.iter().map(|&v| num(v)).collect();
    let quoted: String = text.lines().map(|l| format!("#   {l}\n")).collect();
    Some(format!(
        r#""""The raster calculator as Janus ran it: each date's inputs read as its VRTs read them (the bands' no data as
NaN, their scale and offset applied), the expression in numpy, a GeoTIFF per date (Janus writes a VRT per date, or a
COG). numpy computes in float64 and Janus in float32: the results agree to about 1e-6. Needs numpy and rasterio."""
import os

import numpy as np
import rasterio
from rasterio.transform import Affine

# The text:
{quoted}# Its inputs:
{inputs}
# The result's grid: its CRS, geotransform and size
wkt = {wkt}
geotransform = ({gts_text})
width, height = {w}, {h}
# Each date: its file's name, its year, month, day, day of the year, decimal year and place, and each input's
# (file, band, quality codes as they are) or None (no date matched)
dates = {dates}
out_dir = {out}
{prelude}

def expression(b, year, month, day, doy, decyear, t):
    """The expression over its inputs b[0], b[1]..."""
{stmts}    return _out({r})
{body}"#,
        inputs = inputs.join("\n"),
        gts_text = gts.join(", "),
        wkt = py_str(&grid.crs_wkt),
        w = grid.width,
        h = grid.height,
        out = out_dir(run),
        prelude = janus_cube::calc::python::NUMPY_PRELUDE,
        body = CALC_BODY,
    ))
}

const CALC_BODY: &str = r##"

def read(source):
    """An input at a date as the VRT gives it: the band's no data as NaN and its scale and offset applied (quality
    codes as they are), as float32."""
    if source is None:
        return np.full((height, width), np.nan, dtype="float32")
    path, band, raw = source
    with rasterio.open(path) as src:
        a = src.read(band).astype("float64")
        if not raw:
            nd = src.nodatavals[band - 1]
            bad = (a == nd) if nd is not None and not np.isnan(nd) else np.zeros(a.shape, dtype=bool)
            a = a * src.scales[band - 1] + src.offsets[band - 1]
            a[bad] = np.nan
    return a.astype("float32")


os.makedirs(out_dir, exist_ok=True)
transform = Affine.from_gdal(*geotransform)
for stem, when, sources in dates:
    b = [read(s).astype("float64") for s in sources]
    with np.errstate(all="ignore"):
        r = np.broadcast_to(expression(b, *when), (height, width)).astype("float32")
    path = os.path.join(out_dir, stem + ".tif")
    with rasterio.open(path, "w", driver="GTiff", width=width, height=height, count=1, dtype="float32",
                       crs=wkt or None, transform=transform, nodata=float("nan"), tiled=True, compress="deflate") as dst:
        dst.write(r, 1)
    print(path)
"##;

/// Build series (janus_zeit::series_io::build): a VRT per date through gdal.BuildVRT with the same options, the
/// bands named; written as COGs when Janus wrote COGs.
pub fn build_series(
    plan: &janus_zeit::series_io::BuildPlan,
    stem: &str,
    format: janus_zeit::series_io::BuildFormat,
    run: &Path,
) -> String {
    let Some(g) = &plan.grid else { return String::new() };
    let (x0, y1) = (g.gt[0], g.gt[3]);
    let (x1, y0) = (x0 + g.w as f64 * g.gt[1], y1 + g.h as f64 * g.gt[5]);
    let mut dates = String::from("[\n");
    for d in &plan.dates {
        let bands: Vec<String> = d
            .bands
            .iter()
            .map(|files| {
                let f: Vec<String> = files.iter().map(|f| py_str(&crate::provenance::absolute(f))).collect();
                format!("[{}]", f.join(", "))
            })
            .collect();
        dates += &format!("    ({}, [{}]),\n", py_str(&d.label), bands.join(", "));
    }
    dates += "]";
    let wl: Vec<String> = plan
        .bands
        .iter()
        .filter_map(|b| janus_zeit::series_io::sentinel2_nm(b).map(|nm| format!("{}: {}", py_str(b), num(nm))))
        .collect();
    let cog = format == janus_zeit::series_io::BuildFormat::Cog;
    format!(
        r#""""Build series as Janus ran it: a VRT per date (gdal.BuildVRT: each band's file, or its tiles mosaicked, the
bands stacked; files of several bands mosaicked as they are), on the grid of the files' extent at their finest
pixel{cogs}. Needs GDAL's Python bindings (osgeo)."""
import os

from osgeo import gdal

gdal.UseExceptions()

bounds = ({x0}, {y0}, {x1}, {y1})  # the grid's extent: west, south, east, north
resolution = ({rx}, {ry})
multiband = {multiband}  # files of several bands each
band_names = {band_names}
wavelengths = {{{wl}}}  # nm, of the Sentinel-2 bands named
# Each date: its label and each band's files (one, or the tiles mosaicked); files of several bands: one entry
dates = {dates}
stem = {stem}
cog = {cog}
out_dir = {out}

common = dict(outputBounds=bounds, xRes=resolution[0], yRes=resolution[1], resampleAlg="nearest")
vrt_dir = os.path.join(out_dir, ".part") if cog else out_dir
bands_dir = os.path.join(out_dir, ".bands")
os.makedirs(vrt_dir, exist_ok=True)
for label, bands in dates:
    name = stem + "_" + label.replace("-", "")
    vrt = os.path.join(vrt_dir, name + ".vrt")
    if multiband:
        gdal.BuildVRT(vrt, bands[0], **common)
    else:
        sources = []
        for b, files in enumerate(bands):
            if len(files) == 1:
                sources.append(files[0])
            else:
                os.makedirs(bands_dir, exist_ok=True)
                mosaic = os.path.join(bands_dir, "%s_b%d.vrt" % (name, b + 1))
                gdal.BuildVRT(mosaic, files, **common)
                sources.append(mosaic)
        gdal.BuildVRT(vrt, sources, separate=True, **common)
    ds = gdal.Open(vrt, gdal.GA_Update)
    for b, n in enumerate(band_names[:ds.RasterCount]):
        band = ds.GetRasterBand(b + 1)
        band.SetDescription(n)
        if n in wavelengths:
            band.SetMetadataItem("wavelength", str(wavelengths[n]))
            band.SetMetadataItem("wavelength_units", "nm")
    ds = None
    if cog:
        out = os.path.join(out_dir, name + ".tif")
        gdal.Translate(out, vrt, format="COG", creationOptions=["COMPRESS=DEFLATE"])
        print(out)
    else:
        print(vrt)
"#,
        cogs = if cog { ", then written as cloud-optimized GeoTIFFs" } else { "" },
        rx = num(g.gt[1]),
        ry = num(-g.gt[5]),
        x0 = num(x0),
        y0 = num(y0),
        x1 = num(x1),
        y1 = num(y1),
        multiband = py_bool(plan.multiband),
        band_names = py_list(&plan.bands),
        wl = wl.join(", "),
        stem = py_str(stem),
        cog = py_bool(cog),
        out = out_dir(run),
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
