//! The Python of Janus' own tools (rerun.py beside a run's provenance, as a Zeit run's): the same operation on the
//! same inputs, written with Zeit (`load_raster` reads and warps, `save_raster` writes, `regularize_time_series`,
//! `mmu_sieve`...) and the little geopandas and rasterio Zeit does not cover, in the run's folder's `python`
//! subfolder (Janus' own are left alone). Each script holds only the branches its run took. The Python history
//! shows them (history.rs); the self-test runs them and compares their outputs with Janus'.

use std::path::Path;

use janus_zeit::rasterize::{Burn, RasterizeOptions};

use crate::python::py_str;

/// A template's `@name@` places filled.
fn fill(template: &str, values: &[(&str, String)]) -> String {
    let mut s = template.to_string();
    for (k, v) in values {
        s = s.replace(&format!("@{k}@"), v);
    }
    s
}

/// The Janus that ran, for the scripts' first line.
fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A Python list on one line.
fn py_inline(v: &[String]) -> String {
    format!("[{}]", v.iter().map(|s| py_str(s)).collect::<Vec<_>>().join(", "))
}

/// A Python list of texts, as many on a line as fit in 110 columns.
fn py_wrapped(v: &[String]) -> String {
    let mut o = String::from("[\n");
    let mut line = String::new();
    for s in v {
        let item = format!("{}, ", py_str(s));
        if !line.is_empty() && line.len() + item.len() > 108 {
            o += &format!("    {}\n", line.trim_end());
            line.clear();
        }
        line += &item;
    }
    if !line.is_empty() {
        o += &format!("    {}\n", line.trim_end());
    }
    o + "]"
}

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

/// The folder the script writes into, as a Python string: the run's `python` subfolder.
fn out_dir(run: &Path) -> String {
    py_str(&run.join("python").to_string_lossy())
}

fn py_bool(b: bool) -> &'static str {
    if b { "True" } else { "False" }
}

/// A Python dict of numbers to texts.
fn py_names(names: &[(i64, String)]) -> String {
    let items: Vec<String> = names.iter().map(|(k, n)| format!("{k}: {}", py_str(n))).collect();
    format!("{{{}}}", items.join(", "))
}

/// A CRS as short as it can be written: `EPSG:32723` when the WKT carries its authority, else the WKT.
fn crs_text(definition: &str) -> String {
    if let Ok(srs) = gdal::spatial_ref::SpatialRef::from_definition(definition)
        && let (Some(name), Ok(code)) = (srs.auth_name(), srs.auth_code())
        && name.eq_ignore_ascii_case("EPSG")
        && code > 0
    {
        return format!("EPSG:{code}");
    }
    definition.to_string()
}

/// A GDAL geotransform as the arguments of rasterio's `Affine`.
fn affine(gt: &[f64; 6]) -> String {
    format!("Affine({}, {}, {}, {}, {}, {})", num(gt[1]), num(gt[2]), num(gt[0]), num(gt[4]), num(gt[5]), num(gt[3]))
}

/// A date (days since 1970) as YYYY-MM-DD.
fn day_text(days: i64) -> String {
    let (y, m, d) = janus_cube::dates::civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The script written beside a run (rerun.py) and named its Python in the provenance.
pub fn attach(folder: &Path, p: &mut crate::provenance::Provenance, code: String) {
    if std::fs::write(folder.join(crate::provenance::SCRIPT), code).is_ok() {
        p.python = crate::provenance::SCRIPT.into();
    }
}

/// The arguments of `load_raster` that do what gdalwarp's options do, None where Zeit cannot: pixels aligned to
/// multiples of the size, a no data value or a data type of the output, a cutline (cropped to its extent, off the
/// source's pixels), an extent together with another CRS or pixel size.
fn warp_arguments(opts: &janus_zeit::warp::WarpOptions, like: Option<&str>) -> Option<String> {
    if opts.tap || opts.dst_nodata.is_some() || opts.data_type.is_some() || opts.cutline.is_some() {
        return None;
    }
    let mut kw = String::new();
    let mut warps = false;
    if opts.grid.is_some() {
        kw += &format!(", like={}", py_str(like?));
        warps = true;
    } else {
        if let Some(s) = &opts.t_srs {
            kw += &format!(", crs={}", py_str(&crs_text(s)));
            warps = true;
        }
        if let Some((x, y)) = opts.pixel {
            kw += &format!(", res={}", if x == y { num(x) } else { format!("({}, {})", num(x), num(y)) });
            warps = true;
        }
        if let Some((e, _)) = &opts.extent {
            if warps {
                return None;
            }
            kw += &format!(", clip=({}, {}, {}, {})", num(e[0]), num(e[1]), num(e[2]), num(e[3]));
        }
    }
    if warps {
        let method = if opts.resampling.is_empty() || opts.resampling == "near" { "nearest" } else { &opts.resampling };
        kw += &format!(", resampling={}", py_str(method));
    }
    Some(kw)
}

/// Reproject, resample, clip (janus_zeit::warp::warp_files): Zeit's `load_raster` warps each file as gdalwarp does
/// and `save_raster` writes it under its own name; gdal.Warp itself with gdalwarp's options for what Zeit cannot do.
/// `like` is the file of the layer whose grid is the target.
pub fn warp(files: &[String], opts: &janus_zeit::warp::WarpOptions, like: Option<&str>, run: &Path) -> String {
    let Some(kw) = warp_arguments(opts, like) else {
        return warp_gdal(files, &opts.args().unwrap_or_default(), opts.format.extension(), run);
    };
    // the files' names; two of one name, from different folders: the second numbered
    let mut names: Vec<String> = vec![];
    for (i, f) in files.iter().enumerate() {
        let stem = Path::new(f).file_stem().map_or(format!("map_{i}"), |s| s.to_string_lossy().into_owned());
        let mut name = format!("{stem}.tif");
        if names.contains(&name) {
            name = format!("{stem}_{i}.tif");
        }
        names.push(name);
    }
    let plain = files
        .iter()
        .zip(&names)
        .all(|(f, n)| Path::new(f).file_stem().is_some_and(|s| format!("{}.tif", s.to_string_lossy()) == *n));
    let (list, head, target) = if plain {
        (py_list(files), "f in files", "out / (Path(f).stem + \".tif\")")
    } else {
        let pairs: Vec<String> =
            files.iter().zip(&names).map(|(f, n)| format!("    ({}, {}),\n", py_str(f), py_str(n))).collect();
        (format!("[  # (file, the name written)\n{}]", pairs.concat()), "f, name in files", "out / name")
    };
    let cog = opts.format == janus_zeit::warp::Format::Cog;
    fill(
        r#""""Reproject, resample, clip as Janus @ver@ ran it: Zeit's load_raster warps each file as gdalwarp does and save_raster writes it@note@."""
from pathlib import Path

import zeit

files = @files@
out = Path(@out@)
for @head@:
    print(zeit.save_raster(zeit.load_raster(f@kw@), @target@@driver@))
"#,
        &[
            ("ver", version().into()),
            (
                "note",
                if opts.format == janus_zeit::warp::Format::Vrt {
                    " (Janus wrote warped VRTs, virtual; here the pixels are written)".into()
                } else {
                    String::new()
                },
            ),
            ("files", list),
            ("out", out_dir(run)),
            ("head", head.into()),
            ("kw", kw),
            ("target", target.into()),
            ("driver", if cog { ", driver=\"COG\"".into() } else { String::new() }),
        ],
    )
}

/// gdal.Warp with gdalwarp's options, file by file, each under its own name.
fn warp_gdal(files: &[String], args: &[String], ext: &str, run: &Path) -> String {
    fill(
        r#""""Reproject, resample, clip as Janus @ver@ ran it: GDAL's gdalwarp (gdal.Warp) with the same options on each file,
written under its own name (Zeit's load_raster cannot do what these options ask). Needs GDAL's Python bindings (osgeo)."""
from pathlib import Path

from osgeo import gdal

gdal.UseExceptions()

files = @files@
# gdalwarp's options, as Janus gave them
options = @options@
out = Path(@out@)
out.mkdir(parents=True, exist_ok=True)
written = []
for i, f in enumerate(files):
    path = out / (Path(f).stem + @ext@)
    if path in written:  # two files of one name, from different folders
        path = out / f"{Path(f).stem}_{i}{@ext@}"
    written.append(path)
    gdal.Warp(str(path), f, options=options)
    print(path)
"#,
        &[
            ("ver", version().into()),
            ("files", py_list(files)),
            ("options", py_wrapped(args)),
            ("out", out_dir(run)),
            ("ext", py_str(&format!(".{ext}"))),
        ],
    )
}

/// What kind of values an attribute holds.
#[derive(Clone, Copy, PartialEq)]
enum Field {
    Real,
    Integer,
    Text,
}

/// The type of an attribute of a layer (real when it cannot be told).
fn field_kind(vector: &str, layer: &str, attribute: &str) -> Field {
    use gdal::vector::LayerAccess;
    use gdal_sys::OGRFieldType::{OFTInteger, OFTInteger64, OFTReal};
    let Ok(ds) = gdal::Dataset::open(vector) else { return Field::Real };
    let Ok(l) = (if layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(layer) }) else { return Field::Real };
    match l.defn().fields().find(|f| f.name() == attribute).map(|f| f.field_type()) {
        Some(t) if t == OFTInteger || t == OFTInteger64 => Field::Integer,
        Some(t) if t != OFTReal => Field::Text,
        _ => Field::Real,
    }
}

/// Rasterize on the series' grid (janus_zeit::rasterize): the features in reading order, in the grid's CRS, burned
/// by GDAL (a later one over an earlier one, rasterio's `rasterize`) with the value Janus burns: 1, the feature's
/// number, or an attribute's (a number as it is, a text as a class numbered from 1 in sorted order).
pub fn rasterize(vector: &str, opts: &RasterizeOptions, out_name: &str, run: &Path) -> String {
    let g = &opts.grid;
    let crs = crs_text(&g.wkt);
    let attribute = match &opts.burn {
        Burn::Attribute(a) => py_str(a),
        _ => String::new(),
    };
    // the features kept, the value each burns, the output's type
    let has = "zones.geometry.notna()";
    let (what, steps, shapes, dtype, fill_value, nodata) = match &opts.burn {
        Burn::One => (
            "1",
            format!("zones = zones[{has}]\n"),
            "zones.geometry".to_string(),
            "\"uint8\"".to_string(),
            String::new(),
            String::new(),
        ),
        Burn::Index => (
            "the feature's number (1, 2... counting every feature read)",
            format!("zones[\"burn\"] = range(1, len(zones) + 1)  # the feature's number\nzones = zones[{has}]\n"),
            "zip(zones.geometry, zones.burn)".to_string(),
            "\"int32\"".to_string(),
            String::new(),
            ", nodata=0".to_string(),
        ),
        Burn::Attribute(_) => {
            let kept = format!("zones = zones[{has} & zones[{attribute}].notna()]\n");
            let value = format!("zip(zones.geometry, zones[{attribute}])");
            match field_kind(
                vector,
                &opts.layer,
                match &opts.burn {
                    Burn::Attribute(a) => a,
                    _ => "",
                },
            ) {
                Field::Real => (
                    "the attribute's value",
                    kept,
                    value,
                    "\"float64\"".to_string(),
                    "fill=float(\"nan\"), ".to_string(),
                    ", nodata=float(\"nan\")".to_string(),
                ),
                Field::Integer => (
                    "the attribute's value",
                    kept,
                    value,
                    "\"int32\"".to_string(),
                    "fill=-2**31, ".to_string(),
                    ", nodata=-2**31".to_string(),
                ),
                Field::Text => (
                    "the attribute's text as a class (numbered from 1 in sorted order)",
                    format!(
                        "{kept}classes = sorted(zones[{attribute}].unique())  # the text's classes\n\
                         dtype = \"uint8\" if len(classes) < 255 else \"int32\"\n"
                    ),
                    format!("zip(zones.geometry, zones[{attribute}].map({{c: i for i, c in enumerate(classes, 1)}}))"),
                    "dtype".to_string(),
                    String::new(),
                    ", nodata=0".to_string(),
                ),
            }
        }
    };
    let text = matches!(&opts.burn, Burn::Attribute(_)) && steps.contains("classes = ");
    fill(
        r#""""Rasterize as Janus @ver@ ran it: the layer's features, in the grid's CRS, burned on the series' grid (a later one over an earlier one) with @what@."""
import geopandas as gpd
import zeit
from rasterio.features import rasterize
from rasterio.transform import Affine

crs, transform = @crs@, @affine@  # the series' grid
zones = gpd.read_file(@vector@, layer=@layer@).to_crs(crs)
@steps@burned = rasterize(@shapes@, (@h@, @w@), transform=transform, @fill@dtype=@dtype@@touched@)
print(zeit.save_raster(burned, @out@, crs=crs, transform=transform@nodata@))
@classes@"#,
        &[
            ("ver", version().into()),
            ("what", what.into()),
            ("crs", py_str(&crs)),
            ("affine", affine(&g.gt)),
            ("vector", py_str(vector)),
            ("layer", if opts.layer.is_empty() { "None".into() } else { py_str(&opts.layer) }),
            ("steps", steps),
            ("shapes", shapes),
            ("h", g.h.to_string()),
            ("w", g.w.to_string()),
            ("fill", fill_value),
            ("touched", if opts.all_touched { ", all_touched=True".into() } else { String::new() }),
            ("dtype", dtype),
            ("out", py_str(&run.join("python").join(out_name).to_string_lossy())),
            ("nodata", nodata),
            ("classes", if text { "print(dict(enumerate(classes, 1)))\n".into() } else { String::new() }),
        ],
    )
}

/// Polygonize (janus_zeit::polygonize): the patches through GDAL's polygonizer (rasterio's `shapes`) as Janus,
/// measured on the CRS's ellipsoid as GDAL measures them, into a GeoPackage.
pub fn polygonize(src: &str, opts: &janus_zeit::polygonize::PolyOptions, out_name: &str, run: &Path) -> String {
    let conn = if opts.eight { 8 } else { 4 };
    // the map's CRS and its pixel, for the minimum area in pixels and the simplification's units
    let (mut crs, mut geographic, mut pixel) = (false, false, 0.0);
    if let Ok(ds) = gdal::Dataset::open(src) {
        let wkt = ds.projection();
        crs = !wkt.is_empty();
        geographic = gdal::spatial_ref::SpatialRef::from_wkt(&wkt).is_ok_and(|s| s.is_geographic());
        let (w, h) = ds.raster_size();
        let gt = ds.geo_transform().unwrap_or([0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
        pixel = janus_zeit::polygonize::pixel_area(&gt, &wkt, w, h);
    }
    let min_pixels =
        if opts.min_area_ha > 0.0 && pixel > 0.0 { (opts.min_area_ha * 1e4 / pixel).ceil() as usize } else { 0 };
    let sieve = min_pixels >= 2;
    let mut cols = vec!["\"value\"".to_string()];
    let mut body = String::new();
    if sieve {
        body += &format!(
            "raster = zeit.mmu_sieve(raster, {min_pixels}, connectivity={conn})  # the patches under {} ha ({min_pixels} pixels of {pixel:.2} m2) merged into the neighbour they share the longest border with\n",
            opts.min_area_ha
        );
    }
    body += "transform = raster.rio.transform()\n";
    body += &format!(
        "found = list(shapes(raster.values.astype(\"float32\"), mask=np.isfinite(raster.values), connectivity={conn}, transform=transform))\n\
         patches = gpd.GeoDataFrame({{\"value\": [v for _, v in found]}}, geometry=[shape(g) for g, _ in found], crs=raster.rio.crs)\n\
         patches[\"pixels\"] = (patches.geometry.values.area / abs(transform.a * transform.e)).round().astype(int)  # the patch's pixels\n"
    );
    let mut mean_column = None;
    if let Some((name, path, band)) = &opts.mean_of {
        let col: String =
            name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        let col = format!("mean_{}", col.trim_matches('_'));
        body += &format!(
            "other = zeit.load_raster({}, band={band}, like=raster, masked=True)  # the other map, on this grid\n\
             ids = rasterize(zip(patches.geometry, range(1, len(patches) + 1)), raster.shape, transform=transform, dtype=\"int32\")\n\
             patches[{}] = pd.Series(other.values.ravel().astype(\"float64\")).groupby(ids.ravel()).mean().reindex(range(1, len(patches) + 1)).values\n",
            py_str(&crate::provenance::absolute(path)),
            py_str(&col)
        );
        mean_column = Some(col);
    }
    if sieve {
        body += &format!(
            "patches = patches[patches.pixels >= {min_pixels}]  # the patches left under it had no neighbour to join\n"
        );
    }
    if opts.simplify_m > 0.0 {
        let tolerance = if geographic { opts.simplify_m / 111_320.0 } else { opts.simplify_m };
        body += &format!("patches.geometry = patches.simplify({}, preserve_topology=True)\n", num(tolerance));
    }
    if !opts.classes.is_empty() {
        body += &format!("patches[\"class\"] = patches.value.map({})\n", py_names(&opts.classes));
        cols.push("\"class\"".into());
    }
    if opts.years {
        body += "patches[\"date\"] = patches.value.map(date)\n";
        cols.push("\"date\"".into());
    }
    if crs {
        body += "geod, lonlat = patches.crs.get_geod(), patches.to_crs(patches.crs.geodetic_crs).geometry  # measured on the ellipsoid\n\
                 patches[\"area_ha\"] = [abs(geod.geometry_area_perimeter(g)[0]) / 1e4 for g in lonlat]\n\
                 patches[\"perimeter_m\"] = [geod.geometry_length(g.boundary) for g in lonlat]\n";
    } else {
        body += "patches[\"area_ha\"], patches[\"perimeter_m\"] = patches.geometry.values.area / 1e4, patches.length\n";
    }
    cols.extend(["\"area_ha\"".into(), "\"perimeter_m\"".into(), "\"pixels\"".into()]);
    cols.extend(mean_column.map(|c| py_str(&c)));
    cols.push("\"geometry\"".into());
    let layer = if opts.layer.is_empty() { "polygons" } else { &opts.layer };
    fill(
        r#""""Polygonize as Janus @ver@ ran it: the map's patches (pixels of one value connected @conn@-wise) as polygons in a GeoPackage, each with its value@what@, area (ha) and perimeter (m) on the ellipsoid and pixels."""
@stdlib@from pathlib import Path

import geopandas as gpd
import numpy as np
@pandas@import zeit
from rasterio.features import @features@
from shapely.geometry import shape
@date@
raster = zeit.load_raster(@src@, band=@band@, masked=True)  # no data as NaN
@body@out = Path(@out@)
out.parent.mkdir(parents=True, exist_ok=True)
patches[[@cols@]].to_file(out, layer=@layer@, driver="GPKG")
"#,
        &[
            ("ver", version().into()),
            ("conn", conn.to_string()),
            (
                "what",
                format!(
                    "{}{}{}",
                    if opts.classes.is_empty() { "" } else { ", class" },
                    if opts.years { ", date" } else { "" },
                    opts.mean_of.as_ref().map_or(String::new(), |m| format!(", the mean of {}", m.0))
                ),
            ),
            ("stdlib", if opts.years { "import calendar\nimport datetime\n".into() } else { String::new() }),
            ("pandas", if opts.mean_of.is_some() { "import pandas as pd\n".into() } else { String::new() }),
            ("features", if opts.mean_of.is_some() { "rasterize, shapes".into() } else { "shapes".into() }),
            (
                "date",
                if opts.years {
                    "\n\ndef date(v):\n    \"\"\"A decimal year as a date (YYYY-MM-DD), a whole year as itself.\"\"\"\n    year = int(v)\n    \
                     days = 366 if calendar.isleap(year) else 365\n    \
                     return str(year) if v == year else (datetime.date(year, 1, 1) + datetime.timedelta(days=int((v - year) * days))).isoformat()\n"
                        .into()
                } else {
                    String::new()
                },
            ),
            ("src", py_str(&crate::provenance::absolute(src))),
            ("band", opts.band.max(1).to_string()),
            ("body", body),
            ("out", py_str(&run.join("python").join(out_name).to_string_lossy())),
            ("cols", cols.join(", ")),
            ("layer", py_str(layer)),
        ],
    )
}

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
    use janus_zeit::areas::{AreaKind, PixelAreas};
    use janus_zeit::zonal::ZonalKind;
    const STATS: [&str; 10] = ["n", "mean", "std", "min", "p10", "p25", "median", "p75", "p90", "max"];
    let kind = opts.kind;
    let values = kind == ZonalKind::Continuous;
    let classes = kind == ZonalKind::Classes;
    let (w, h) = opts.window.map_or((grid.w, grid.h), |b| (b[2] - b[0], b[3] - b[1]));
    let areas =
        PixelAreas::new(&grid.gt, &grid.wkt, opts.window.map_or(0, |b| b[0]), opts.window.map_or(0, |b| b[1]), w, h);
    let geodesic = matches!(areas.kind, AreaKind::Geographic | AreaKind::Projected);
    let transitions = opts.transitions.filter(|&(a, b)| classes && a.max(b) < sources.len());
    let mut names = names.to_vec();
    names.sort();
    let what = match kind {
        ZonalKind::Continuous => {
            "through each map the number of values, mean, standard deviation, least, percentiles 10, 25, 50, 75 and 90 (linear) and largest of the values"
        }
        ZonalKind::Classes => {
            "through each map each class's pixels and area (each pixel's true area on the ellipsoid) and the transitions between two maps"
        }
        ZonalKind::Years => "through each map each year's pixels and area (each pixel's true area on the ellipsoid)",
    };
    let mut s = String::new();
    s += &format!(
        "\"\"\"Zonal statistics over time as Janus {} ran them: each feature of the layer is a zone, its pixels on the maps' grid those\n\
         whose centre is inside it{} (a pixel in several features counts in each); {what}.\"\"\"\n\
         from pathlib import Path\n\nimport geopandas as gpd\nimport numpy as np\nimport pandas as pd\nimport zeit\n\
         from rasterio.features import geometry_mask\nfrom rasterio.transform import Affine\n",
        version(),
        if opts.all_touched { " (or that it touches)" } else { "" }
    );
    if !values && geodesic {
        s += "from pyproj import Transformer\nfrom scipy.interpolate import RegularGridInterpolator\n";
    }
    s += &format!("\nvector = {}\nmaps = [  # (file, band, label)\n", py_str(&crate::provenance::absolute(vector)));
    for m in sources {
        s += &format!(
            "    ({}, {}, {}),\n",
            py_str(&crate::provenance::absolute(&m.path)),
            m.band.max(1),
            py_str(&m.label)
        );
    }
    s += "]\n";
    if classes && !names.is_empty() {
        s += &format!("names = {}  # class names by value\n", py_names(&names));
    }
    s += &format!("out = Path({})\n\n", out_dir(run));
    let to_crs = if grid.wkt.is_empty() { String::new() } else { format!(".to_crs({})", py_str(&crs_text(&grid.wkt))) };
    s += &format!(
        "zones = gpd.read_file(vector, layer={}, fid_as_index=True){to_crs}\n",
        if opts.layer.is_empty() { "None".into() } else { py_str(&opts.layer) }
    );
    s += &if opts.id_field.is_empty() {
        "ids = zones.index.astype(str).values  # the zones named by their FIDs\n".to_string()
    } else {
        format!("ids = zones[{}].fillna(\"\").astype(str).values\n", py_str(&opts.id_field))
    };
    let window = opts
        .window
        .map_or(String::new(), |b| format!(".isel(y=slice({}, {}), x=slice({}, {}))", b[1], b[3], b[0], b[2]));
    s += &format!(
        "rasters = [zeit.load_raster(path, band=band, masked=True){window} for path, band, _ in maps]\n\
         t, (H, W) = rasters[0].rio.transform(recalc=True), rasters[0].shape\n\n\n\
         def burn(g):\n    \
         \"\"\"A zone's extent on the grid (a cell around) and its cells' positions, the zone burned alone on the extent.\"\"\"\n    \
         if g is None or g.is_empty:\n        return None, np.zeros(0, int)\n    \
         (c0, r0), (c1, r1) = ~t * (g.bounds[0], g.bounds[3]), ~t * (g.bounds[2], g.bounds[1])\n    \
         c0, r0 = max(int(np.floor(c0)) - 1, 0), max(int(np.floor(r0)) - 1, 0)\n    \
         c1, r1 = min(int(np.ceil(c1)) + 1, W), min(int(np.ceil(r1)) + 1, H)\n    \
         if c1 <= c0 or r1 <= r0:\n        return None, np.zeros(0, int)\n    \
         inside = geometry_mask([g], (r1 - r0, c1 - c0), t * Affine.translation(c0, r0), {}invert=True)\n    \
         rows, cols = np.nonzero(inside)\n    \
         return (c0, r0, c1, r1), (rows + r0) * W + cols + c0\n\n\n\
         burned = [burn(g) for g in zones.geometry]\n\
         cells = [c for _, c in burned]\n\
         zone, flat = np.repeat(np.arange(len(zones)), [len(c) for c in cells]), np.concatenate(cells)  # each cell's zone and place\n",
        if opts.all_touched { "all_touched=True, " } else { "" }
    );
    if !values {
        if geodesic {
            s += "c0, r0, c1, r1 = (f([e[i] for e, _ in burned if e]) for f, i in ((min, 0), (min, 1), (max, 2), (max, 3)))  # the zones' extent\n\
                  step = max(-(-max(c1 - c0, r1 - r0) // 128), 1)  # each cell's area (m2) on the ellipsoid: at most 129 x 129 samples across it, interpolated\n\
                  xs, ys = (np.unique([*range(0, n, step), n - 1]) for n in (c1 - c0, r1 - r0))\n\
                  nodes = np.broadcast_arrays(c0 + xs[None, :], r0 + ys[:, None])\n\
                  corners = [t * (nodes[0] + dx, nodes[1] + dy) for dx, dy in ((0, 0), (1, 0), (1, 1), (0, 1))]\n\
                  lon, lat = Transformer.from_crs(zones.crs, zones.crs.geodetic_crs, always_xy=True).transform(\n    \
                  np.stack([x for x, _ in corners], -1), np.stack([y for _, y in corners], -1))\n\
                  geod = zones.crs.get_geod()\n\
                  samples = np.array([[abs(geod.polygon_area_perimeter(lon[j, i], lat[j, i])[0]) for i in range(len(xs))] for j in range(len(ys))])\n\
                  area = RegularGridInterpolator((ys, xs), samples)(np.column_stack([flat // W - r0, flat % W - c0]))\n";
        } else {
            s += &format!(
                "area = np.full(len(flat), {})  # m2 of a cell: {}\n",
                num(areas.nominal),
                if areas.kind == AreaKind::EqualArea {
                    "every cell of an equal-area projection"
                } else {
                    "the grid units squared, with no CRS"
                }
            );
        }
    }
    s += "\ncolumn = lambda s: \"\".join(ch.lower() if ch.isalnum() else \"_\" for ch in s).strip(\"_\")\n\
          long, wide = [], pd.DataFrame({\"id\": ids, \"fid\": zones.index, \"pixels\": [len(c) for c in cells]})\n";
    if values {
        let stat = STATS[stat.min(9)];
        s += &format!(
            "\nfor (_, _, label), raster in zip(maps, rasters):\n    \
             x = pd.Series(raster.values.ravel()[flat].astype(\"float32\").astype(\"float64\")).groupby(zone)  # the values of each zone\n    \
             s = x.agg(n=\"count\", mean=\"mean\", std=lambda v: v.std(ddof=0), min=\"min\", p10=lambda v: v.quantile(0.1),\n              \
             p25=lambda v: v.quantile(0.25), median=\"median\", p75=lambda v: v.quantile(0.75), p90=lambda v: v.quantile(0.9),\n              \
             max=\"max\").reindex(range(len(zones)))\n    \
             s[\"n\"] = s[\"n\"].fillna(0).astype(int)\n    \
             long.append(pd.concat([pd.DataFrame({{\"id\": ids, \"fid\": zones.index, \"date\": label}}), s.reset_index(drop=True)], axis=1))\n    \
             wide[f\"{stat}_{{column(label)}}\"] = s[\"{stat}\"].values\n"
        );
    } else {
        let years = kind == ZonalKind::Years;
        let key = if years { "np.floor(v)" } else { "np.where(v >= 0, np.floor(v + 0.5), np.ceil(v - 0.5))" };
        let name = if years { "str(v)" } else { "names.get(v, str(v))" };
        s += &format!(
            "\nkey = lambda v: {key}.astype(np.int64)  # {}\n\n\n\
             def count(*maps):\n    \
             \"\"\"The pixels and the area (m2) of each zone's cells by the keys of the maps' values (the cells with a value in each).\"\"\"\n    \
             x = [m.values.ravel()[flat] for m in maps]\n    \
             ok = np.all([np.isfinite(v) for v in x], axis=0)\n    \
             frame = pd.DataFrame({{\"zone\": zone[ok], **{{i: key(v[ok]) for i, v in enumerate(x)}}, \"area_m2\": area[ok]}})\n    \
             return frame.groupby([\"zone\", *range(len(x))]).area_m2.agg([\"count\", \"sum\"]).set_axis([\"pixels\", \"area_m2\"], axis=1).reset_index()\n\n\n\
             counts = [count(r) for r in rasters]\n\
             values = sorted({{v for c in counts for v in c[0]}})\n\
             for (_, _, label), c in zip(maps, counts):\n    \
             c = c.rename(columns={{0: \"value\"}})\n    \
             c.insert(0, \"id\", ids[c.zone])\n    c.insert(1, \"fid\", zones.index[c.zone])\n    c.insert(2, \"date\", label)\n    \
             c.insert(5, \"{}\", c.value.map(lambda v: {name}))\n    \
             c[\"area_ha\"], c[\"percent\"] = c.area_m2 / 1e4, 100 * c.area_m2 / c.groupby(\"zone\").area_m2.transform(\"sum\")\n    \
             long.append(c.drop(columns=\"zone\").round({{\"area_m2\": 3, \"area_ha\": 6, \"percent\": 6}}))\n    \
             ha = c.pivot_table(index=\"zone\", columns=\"value\", values=\"area_ha\", aggfunc=\"sum\").reindex(index=range(len(zones)), columns=values).fillna(0)\n    \
             ha.columns = [f\"ha_{{{}}}\" + ({}) for v in values]\n    \
             wide = wide.join(ha.set_index(wide.index))\n",
            if years { "the year" } else { "the class: the value rounded, halves away from zero" },
            if years { "year" } else { "class" },
            if years { "str(v)" } else { "column(names.get(v, str(v)))" },
            if sources.len() == 1 { "\"\"".to_string() } else { "\"_\" + column(label)".to_string() },
        );
        if let Some((a, b)) = transitions {
            s += &format!(
                "\nmove = count(rasters[{a}], rasters[{b}]).rename(columns={{0: \"from\", 1: \"to\"}})  # the transitions from {} to {}\n\
                 move.insert(0, \"id\", ids[move.zone])\nmove.insert(1, \"fid\", zones.index[move.zone])\n\
                 move.insert(4, \"from_class\", move[\"from\"].map(lambda v: names.get(v, str(v))))\n\
                 move.insert(6, \"to_class\", move[\"to\"].map(lambda v: names.get(v, str(v))))\n\
                 move[\"area_ha\"] = move.area_m2 / 1e4\n",
                sources[a].label, sources[b].label
            );
        }
    }
    s += "\nout.mkdir(parents=True, exist_ok=True)\n\
          pd.concat(long).to_csv(out / \"zonal_long.csv\", index=False)\n\
          wide.to_csv(out / \"zonal_wide.csv\", index=False)\n";
    if transitions.is_some() {
        s += "move.drop(columns=\"zone\").round({\"area_m2\": 3, \"area_ha\": 6}).to_csv(out / \"transitions.csv\", index=False)\n";
    }
    s
}

/// The helpers of the expression's numpy (`janus_cube::calc::python::NUMPY_PRELUDE`) that `code` calls, with those
/// they call.
fn prelude_for(code: &str) -> String {
    let mut blocks: Vec<(String, String)> = vec![];
    for line in janus_cube::calc::python::NUMPY_PRELUDE.lines() {
        if let Some(rest) = line.strip_prefix("def ") {
            blocks.push((rest.split('(').next().unwrap_or("").to_string(), String::new()));
        }
        if let Some(b) = blocks.last_mut() {
            b.1 += line;
            b.1 += "\n";
        }
    }
    let mut kept = vec![false; blocks.len()];
    loop {
        let mut changed = false;
        for i in 0..blocks.len() {
            let call = format!("{}(", blocks[i].0);
            let used = code.contains(&call)
                || blocks.iter().enumerate().any(|(j, b)| kept[j] && j != i && b.1.contains(&call));
            if used && !kept[i] {
                kept[i] = true;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut out = String::new();
    for (i, b) in blocks.iter().enumerate() {
        if kept[i] {
            out += "\n\n";
            out += b.1.trim_end();
        }
    }
    out
}

/// The expression of a compiled program as a Python function `expression(b, year, month, day, doy, decyear, t)`
/// with the helpers it needs before it.
fn numpy_function(program: &janus_cube::calc::program::Program) -> String {
    let (stmts, r) = janus_cube::calc::python::numpy(program, &|k| format!("b[{k}]"), "    ");
    let body = format!("{stmts}    return _out({r})\n");
    let prelude = prelude_for(&body);
    let prelude = prelude.trim_start_matches('\n');
    let gap = if prelude.is_empty() { "" } else { "\n\n\n" };
    format!(
        "{prelude}{gap}def expression(b, year, month, day, doy, decyear, t):\n    \"\"\"The expression over its inputs b[0], b[1]...\"\"\"\n{body}"
    )
}

/// The rule of a quality band as Python: a function of the band's codes (a DataArray, no data as NaN).
fn usable_function(rule: janus_cube::QaRule) -> String {
    use janus_cube::QaRule;
    let (what, test) = match rule {
        QaRule::Fmask => ("Fmask: 0 clear land, 1 water", "(c == 0) | (c == 1)"),
        QaRule::LandsatC2 => {
            ("Landsat C2 QA_PIXEL: bits 0 fill, 1 dilated cloud, 3 cloud, 4 shadow, 5 snow out", "(c & 0x3B) == 0")
        }
        QaRule::Scl => ("Sentinel-2 SCL: 4 vegetation, 5 bare soil, 6 water", "(c >= 4) & (c <= 6)"),
        QaRule::HlsFmask => {
            ("HLS Fmask: bits 1 cloud, 2 adjacent, 3 shadow, 4 snow out; 255 fill", "(c != 255) & ((c & 0x1E) == 0)")
        }
        _ => ("a mask: 0 invalid", "c != 0"),
    };
    format!(
        "def usable(q):\n    \"\"\"The observations the quality band keeps ({what}).\"\"\"\n    \
         c = q.fillna(0).astype(\"int64\")\n    return q.notnull() & (q >= 0) & ({test})\n"
    )
}

/// Whether any of these bands (file, band) has a scale or offset (true when a file cannot be read).
fn scaled(bands: &[(String, i32)]) -> bool {
    let mut seen = std::collections::HashSet::new();
    bands.iter().filter(|b| seen.insert((*b).clone())).any(|(path, band)| match gdal::Dataset::open(path) {
        Ok(ds) => ds
            .rasterband(*band as usize)
            .is_ok_and(|b| b.scale().is_some_and(|s| s != 1.0) || b.offset().is_some_and(|o| o != 0.0)),
        Err(_) => true,
    })
}

/// Regularize series (janus_zeit::regularize): the series read as Janus reads it over the window, each period's
/// median, medoid or clearest observation (Zeit's `regularize_time_series`), the gaps filled, a GeoTIFF per period.
pub fn regularize(info: &janus_cube::CubeInfo, opts: &janus_zeit::regularize::RegOptions, run: &Path) -> String {
    use janus_zeit::regularize::{Composite, Fill, Step};
    let [x0, y0, x1, y1] = opts.window.unwrap_or([0, 0, info.width, info.height]);
    let sel = info.sel;
    let qa_on = sel.qa_band > 0 && sel.qa_rule != janus_cube::QaRule::None;
    let n_bands = info.bands_per_date.max(1);
    let multi = opts.all_bands && n_bands > 1;
    let expr = info.expr.as_ref().filter(|_| !opts.all_bands);
    let qa = |base: i32| if qa_on { (sel.qa_band + base).to_string() } else { "None".into() };
    let names: Vec<String> = if multi {
        (0..n_bands).map(|b| info.band_names.get(b).cloned().unwrap_or_else(|| format!("band {}", b + 1))).collect()
    } else {
        vec![info.selection_text()]
    };
    // each date: its day, its file and what it reads there
    let mut layers = String::new();
    for (t, l) in info.layers.iter().enumerate() {
        let day = day_text((l.time / 86400.0).floor() as i64);
        let path = py_str(&crate::provenance::absolute(&l.path));
        let rest = if let Some(e) = expr {
            let bands: Vec<String> = e.bands.iter().map(|b| (b + l.band_base).to_string()).collect();
            let vars: Vec<String> =
                janus_cube::calc::plan::date_vars(info, t).0.iter().map(|&x| num(f64::from(x))).collect();
            format!("[{}], {}, ({})", bands.join(", "), qa(l.band_base), vars.join(", "))
        } else if opts.all_bands {
            let first = l.band_base + 1;
            let bands = if multi {
                format!("[{}]", (first..first + n_bands as i32).map(|b| b.to_string()).collect::<Vec<_>>().join(", "))
            } else {
                first.to_string()
            };
            format!("{bands}, {}", qa(l.band_base))
        } else {
            let nd = if sel.nd_band > 0 { (sel.nd_band + l.band_base).to_string() } else { "None".into() };
            let mut r = format!("{}", l.band);
            if sel.nd_band > 0 {
                r += &format!(", {nd}");
            }
            if qa_on {
                r += &format!(", {}", qa(l.band_base));
            }
            r
        };
        layers += &format!("    ({}, {path}, {rest}),\n", py_str(&day));
    }
    let fields = if expr.is_some() {
        "the bands its expression reads, quality band, its variables"
    } else if multi {
        "bands"
    } else {
        "band"
    };
    let (fields, args) = if expr.is_some() {
        (fields.to_string(), "path, bands, qa, when".to_string())
    } else if multi {
        (format!("{fields}, quality band"), "path, bands, qa".to_string())
    } else {
        let mut f = String::from("band");
        let mut a = String::from("path, band");
        if sel.nd_band > 0 && !opts.all_bands {
            f += ", band B of a normalized difference (A - B) / (A + B)";
            a += ", nd";
        }
        if qa_on {
            f += ", quality band";
            a += ", qa";
        }
        (f, a)
    };
    // the bands read, to know whether any has a scale or offset
    let mut read_bands: Vec<(String, i32)> = vec![];
    for l in &info.layers {
        let mut bands = vec![l.band];
        if let Some(e) = expr {
            bands = e.bands.iter().map(|b| b + l.band_base).collect();
        } else if opts.all_bands {
            bands = (l.band_base + 1..=l.band_base + n_bands as i32).collect();
        } else if sel.nd_band > 0 {
            bands.push(sel.nd_band + l.band_base);
        }
        read_bands.extend(bands.into_iter().map(|b| (l.path.clone(), b)));
    }
    let simple = expr.is_none() && !multi && !qa_on && (sel.nd_band == 0 || opts.all_bands);
    let mut helpers = String::new();
    if qa_on {
        helpers += &usable_function(sel.qa_rule);
        helpers += "\n\n\n";
    }
    let scale = scaled(&read_bands);
    helpers += &format!(
        "def read(path, band):\n    \"\"\"A band over the window, its no data NaN{}.\"\"\"\n    \
         v = zeit.load_raster(path, band=band, masked=True, chunks=\"auto\").isel(window)\n    \
         {}.drop_vars(\"band\", errors=\"ignore\")\n",
        if scale { ", its scale and offset applied" } else { "" },
        if scale {
            "return (v * v.attrs.get(\"scale_factor\", 1) + v.attrs.get(\"add_offset\", 0))"
        } else {
            "return v"
        }
    );
    let mask = if qa_on {
        "    if qa:\n        v = v.where(usable(zeit.load_raster(path, band=qa, masked=True, chunks=\"auto\").isel(window)))\n"
    } else {
        ""
    };
    let masked = if qa_on { ", masked by the quality band" } else { "" };
    if !simple {
        helpers += &format!("\n\ndef date({args}):\n    \"\"\"A date as Janus reads it{masked}.\"\"\"\n");
        if expr.is_some() {
            helpers += "    b = [read(path, k) for k in bands]\n    with np.errstate(all=\"ignore\"):\n        \
                        v = b[0].copy(data=np.broadcast_to(expression([x.values.astype(\"float64\") for x in b], *when), b[0].shape).astype(\"float32\"))\n";
        } else if multi {
            helpers += "    v = read(path, bands).assign_coords(band=band_names)\n";
        } else {
            helpers += "    v = read(path, band)\n";
            if sel.nd_band > 0 && !opts.all_bands {
                helpers += "    b = read(path, nd)\n    v = ((v - b) / (v + b)).where(v + b != 0)\n";
            }
        }
        helpers += mask;
        helpers += "    return v\n";
    }
    let date_fn = if simple { "read" } else { "date" };
    let expression_code = expr.map_or(String::new(), |e| format!("{}\n\n\n", numpy_function(&e.program)));
    let (freq, step_text) = match opts.step {
        Step::Days(n) => (format!("{n}D"), format!("every {n} days from the first date")),
        Step::Month => ("MS".to_string(), "calendar months".to_string()),
        Step::Year => ("YS".to_string(), "calendar years".to_string()),
    };
    let method = match opts.composite {
        Composite::Median => "median",
        Composite::Medoid => "medoid",
        Composite::Clearest => "clearest",
    };
    let composite = if opts.composite == Composite::Clearest {
        format!(
            "cube = cube.load()
share = (cube.notnull().all(\"band\") if \"band\" in cube.dims else cube.notnull()).mean((\"y\", \"x\"))  # each date's share of pixels with a value\n\n\n\
             def clearest(group):\n    \
             \"\"\"The observation of the period's clearest date that has every band, pixel by pixel.\"\"\"\n    \
             group = group.isel(time=np.argsort(-share.sel(time=group.time).values, kind=\"stable\"))\n    \
             have = group.notnull().all(\"band\") if \"band\" in group.dims else group.notnull()\n    \
             return group.isel(time=have.argmax(\"time\")).where(have.any(\"time\"))\n\n\n\
             reg = cube.resample(time=\"{freq}\").map(clearest).load()\n"
        )
    } else {
        format!("reg = zeit.regularize_time_series(cube, \"{freq}\", method=\"{method}\").load()\n")
    };
    // the gaps between values, up to the largest (0: any)
    // the gaps between values filled linearly in time, up to the largest (0: any), into `target`
    let linear = |target: &str| {
        if opts.max_gap_days > 0.0 {
            format!(
                "valid = reg.notnull().values\n\
                 days = ((reg.time - reg.time[0]) / pd.Timedelta(days=1)).values.reshape((-1,) + (1,) * (reg.ndim - 1))\n\
                 before = np.maximum.accumulate(np.where(valid, days, -np.inf), axis=0)  # the last day with a value up to each period\n\
                 after = np.minimum.accumulate(np.where(valid, days, np.inf)[::-1], axis=0)[::-1]  # and the next one\n\
                 {target} = reg.interpolate_na(\"time\", method=\"linear\").where(reg.copy(data=valid | (after - before <= {})))  # the gaps up to {} days\n",
                num(opts.max_gap_days),
                opts.max_gap_days
            )
        } else {
            format!("{target} = reg.interpolate_na(\"time\", method=\"linear\")  # the gaps between values\n")
        }
    };
    let (fill_text, fill_code) = match opts.fill {
        Fill::None => (String::new(), String::new()),
        Fill::Linear => (", the gaps filled linearly in time".to_string(), linear("reg")),
        Fill::Whittaker(lambda) => (
            ", the gaps filled by a Whittaker curve".to_string(),
            format!(
                "{}reg = reg.where(reg.notnull() | gaps.isnull(), zeit.smooth(reg, lmbda={}))  # the gaps filled by the curve, the values kept\n",
                linear("gaps"),
                num(lambda)
            ),
        ),
    };
    let numpy_needed =
        expr.is_some() || opts.composite == Composite::Clearest || (opts.fill != Fill::None && opts.max_gap_days > 0.0);
    let band_arg = if multi {
        ", band_names=band_names".to_string()
    } else if names[0].is_empty() {
        String::new()
    } else {
        format!(", band_names={}", py_inline(&names))
    };
    fill(
        r#""""Regularize series as Janus @ver@ ran it: Zeit's regularize_time_series makes the @method@ of each period (@step@)@fill@, a GeoTIFF per period."""
from pathlib import Path

@numpy@import pandas as pd
import xarray as xr
import zeit

# @description@
layers = [  # (date, file, @fields@)
@layers@]
window = dict(y=slice(@y0@, @y1@), x=slice(@x0@, @x1@))  # rows and columns of the files' pixels
@band_names@stem, out = @stem@, Path(@out@)


@expression@@helpers@

cube = xr.concat([@date_fn@(*layer[1:]) for layer in layers], dim="time").assign_coords(time=pd.to_datetime([layer[0] for layer in layers]))
@composite@@fill_code@for time in reg.time.values:
    print(zeit.save_raster(reg.sel(time=time).astype("float32"), out / f"{stem}_{pd.Timestamp(time):%Y%m%d}.tif"@band_arg@))
"#,
        &[
            ("ver", version().into()),
            (
                "method",
                if opts.composite == Composite::Clearest { "clearest observation".into() } else { method.to_string() },
            ),
            ("step", step_text),
            ("fill", fill_text),
            ("numpy", if numpy_needed { "import numpy as np\n".into() } else { String::new() }),
            (
                "description",
                info.description.replace('\n', " ")
                    + &if info.selection_text().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", info.selection_text())
                    },
            ),
            ("fields", fields),
            ("layers", layers),
            ("y0", y0.to_string()),
            ("y1", y1.to_string()),
            ("x0", x0.to_string()),
            ("x1", x1.to_string()),
            (
                "band_names",
                if multi { format!("band_names = {}  # the bands' names\n", py_inline(&names)) } else { String::new() },
            ),
            ("stem", py_str(&opts.stem)),
            ("out", out_dir(run)),
            ("expression", expression_code),
            ("helpers", helpers),
            ("date_fn", date_fn.into()),
            ("composite", composite),
            ("fill_code", fill_code),
            ("band_arg", band_arg),
        ],
    )
}

/// The raster calculator (janus_cube::calc::plan) in its simple cases: no reduction over time, every layer on the
/// result's grid. Each date's inputs as its VRT reads them, the expression as numpy, a GeoTIFF per date. None
/// otherwise (the run then kept as data).
pub fn calculator(
    text: &str,
    plan: &janus_cube::calc::plan::Plan,
    layers: &[janus_cube::calc::plan::CalcLayer],
    _opts: &janus_cube::calc::plan::Options,
    run: &Path,
) -> Option<String> {
    use janus_cube::calc::plan::{Key, Placement, date_stem, date_vars, match_date, placement};
    if !plan.stages.is_empty() || !plan.periods.is_empty() {
        return None;
    }
    let grid = &layers.get(plan.grid)?.info;
    grid.geo_transform?;
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
                    match rinfo.as_ref().and_then(|_| match_date(&layers[*layer].info, *time, t, dated, _opts.matching))
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
    let mut read_bands = vec![];
    for key in &plan.inputs {
        if let Key::Band { layer, band, raw: false, .. } = key {
            for l in &layers[*layer].info.layers {
                read_bands.push((l.path.clone(), if *band <= 0 { l.band } else { band + l.band_base }));
            }
        }
    }
    let scale = scaled(&read_bands);
    let quoted: String = text.lines().map(|l| format!("#   {l}\n")).collect();
    Some(fill(
        r#""""The raster calculator as Janus @ver@ ran it: each date's inputs read as its VRTs read them (the bands' no data as NaN, their scale and offset applied), the expression in numpy, a GeoTIFF per date with Zeit's save_raster (Janus writes a VRT per date, or a COG). numpy computes in float64 and Janus in float32: the results agree to about 1e-6."""
from pathlib import Path

import numpy as np
import zeit

# The text:
@quoted@# Its inputs:
@inputs@


@function@

like, shape = @like@, (@h@, @w@)  # the result's grid
out = Path(@out@)
# Each date: its file's name, its year, month, day, day of the year, decimal year and place, and each input's
# (file, band, quality codes as they are) or None (no date matched)
dates = @dates@


def read(source):
    """An input at a date as the VRT gives it: no data as NaN@scale_doc@ (quality codes as they are), as float32."""
    if source is None:
        return np.full(shape, np.nan, dtype="float32")
    path, band, raw = source
    a = zeit.load_raster(path, band=band, masked=not raw)
    return @value@.values.astype("float32")


for stem, when, sources in dates:
    with np.errstate(all="ignore"):
        result = expression([read(s).astype("float64") for s in sources], *when)
    print(zeit.save_raster(np.broadcast_to(result, shape).astype("float32"), out / f"{stem}.tif", like=like))
"#,
        &[
            ("ver", version().into()),
            ("quoted", quoted),
            ("inputs", inputs.join("\n")),
            ("function", numpy_function(&plan.program)),
            ("like", py_str(&crate::provenance::absolute(&grid.layers.first()?.path))),
            ("h", grid.height.to_string()),
            ("w", grid.width.to_string()),
            ("out", out_dir(run)),
            ("dates", dates),
            (
                "value",
                if scale {
                    "(a if raw else a * a.attrs.get(\"scale_factor\", 1) + a.attrs.get(\"add_offset\", 0))".into()
                } else {
                    "a".into()
                },
            ),
            ("scale_doc", if scale { ", the scale and offset applied".into() } else { String::new() }),
        ],
    ))
}

/// Build series (janus_zeit::series_io::build): a VRT per date through gdal.BuildVRT (Zeit writes no VRT) with the
/// same options, the bands named; written as COGs when Janus wrote COGs.
pub fn build_series(
    plan: &janus_zeit::series_io::BuildPlan,
    stem: &str,
    format: janus_zeit::series_io::BuildFormat,
    run: &Path,
) -> String {
    let Some(g) = &plan.grid else { return String::new() };
    let (x0, y1) = (g.gt[0], g.gt[3]);
    let (x1, y0) = (x0 + g.w as f64 * g.gt[1], y1 + g.h as f64 * g.gt[5]);
    let cog = format == janus_zeit::series_io::BuildFormat::Cog;
    // a band a file, or the files of several bands mosaicked, or tiles to mosaic within each band
    let tiled = !plan.multiband && plan.dates.iter().any(|d| d.bands.iter().any(|b| b.len() > 1));
    let abs =
        |files: &[String]| files.iter().map(|f| py_str(&crate::provenance::absolute(f))).collect::<Vec<_>>().join(", ");
    let mut dates = String::from("[\n");
    for d in &plan.dates {
        let files = if tiled {
            format!("[{}]", d.bands.iter().map(|b| format!("[{}]", abs(b))).collect::<Vec<_>>().join(", "))
        } else if plan.multiband {
            format!("[{}]", abs(d.bands.first().map_or(&[][..], |b| &b[..])))
        } else {
            format!(
                "[{}]",
                d.bands
                    .iter()
                    .filter_map(|b| b.first())
                    .map(|f| py_str(&crate::provenance::absolute(f)))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        dates += &format!("    ({}, {files}),\n", py_str(&d.label));
    }
    dates += "]";
    let wl: Vec<String> = plan
        .bands
        .iter()
        .filter_map(|b| janus_zeit::series_io::sentinel2_nm(b).map(|nm| format!("{}: {}", py_str(b), num(nm))))
        .collect();
    let (what, loop_text) = if tiled {
        (
            "each band's file, or its tiles mosaicked, the bands stacked",
            "    (out / \".bands\").mkdir(exist_ok=True)
    sources = []\n    for b, files in enumerate(files_of_bands):\n        if len(files) == 1:\n            sources.append(files[0])\n        else:\n            \
             mosaic = out / \".bands\" / f\"{name}_b{b + 1}.vrt\"\n            gdal.BuildVRT(str(mosaic), files, **grid)\n            sources.append(str(mosaic))\n    \
             gdal.BuildVRT(str(vrt), sources, separate=True, **grid)\n",
        )
    } else if plan.multiband {
        ("the date's files mosaicked as they are, their bands kept", "    gdal.BuildVRT(str(vrt), files, **grid)\n")
    } else {
        ("each band's file, the bands stacked", "    gdal.BuildVRT(str(vrt), files, separate=True, **grid)\n")
    };
    let head = if tiled { "label, files_of_bands in dates" } else { "label, files in dates" };
    fill(
        r#""""Build series as Janus @ver@ ran it: a VRT per date (gdal.BuildVRT: @what@), on the grid of the files' extent at their finest pixel@cogs@. Zeit writes no VRT: needs GDAL's Python bindings (osgeo)."""
from pathlib import Path

from osgeo import gdal

gdal.UseExceptions()

grid = dict(outputBounds=(@x0@, @y0@, @x1@, @y1@), xRes=@rx@, yRes=@ry@, resampleAlg="nearest")  # west, south, east, north; the pixel
band_names = @band_names@
@wavelengths@# Each date: its label and @dates_what@
dates = @dates@
stem, out = @stem@, Path(@out@)
@vrt_dir@.mkdir(parents=True, exist_ok=True)
for @head@:
    name = f"{stem}_{label.replace('-', '')}"
    vrt = @vdir@ / f"{name}.vrt"
@loop@    ds = gdal.Open(str(vrt), gdal.GA_Update)
    for b, n in enumerate(band_names[: ds.RasterCount], 1):
        ds.GetRasterBand(b).SetDescription(n)
@wavelength_set@    ds = None
@write@"#,
        &[
            ("ver", version().into()),
            ("what", what.into()),
            ("cogs", if cog { ", then written as cloud-optimized GeoTIFFs".into() } else { String::new() }),
            ("x0", num(x0)),
            ("y0", num(y0)),
            ("x1", num(x1)),
            ("y1", num(y1)),
            ("rx", num(g.gt[1])),
            ("ry", num(-g.gt[5])),
            ("band_names", py_list(&plan.bands)),
            (
                "wavelengths",
                if wl.is_empty() {
                    String::new()
                } else {
                    format!("wavelengths = {{{}}}  # nm, of the Sentinel-2 bands named\n", wl.join(", "))
                },
            ),
            (
                "dates_what",
                if tiled {
                    "each band's files (one, or the tiles mosaicked)".into()
                } else if plan.multiband {
                    "the files of its tiles".into()
                } else {
                    "each band's file".into()
                },
            ),
            ("dates", dates),
            ("stem", py_str(stem)),
            ("out", out_dir(run)),
            (
                "vrt_dir",
                if cog {
                    "vrt_dir = out / \".part\"
vrt_dir"
                        .into()
                } else {
                    "out".into()
                },
            ),
            ("vdir", if cog { "vrt_dir" } else { "out" }.into()),
            ("head", head.into()),
            ("loop", loop_text.into()),
            (
                "wavelength_set",
                if wl.is_empty() {
                    String::new()
                } else {
                    "        if n in wavelengths:\n            band = ds.GetRasterBand(b)\n            band.SetMetadataItem(\"wavelength\", str(wavelengths[n]))\n            \
                     band.SetMetadataItem(\"wavelength_units\", \"nm\")\n"
                        .into()
                },
            ),
            (
                "write",
                if cog {
                    "    out_file = out / f\"{name}.tif\"\n    gdal.Translate(str(out_file), str(vrt), format=\"COG\", creationOptions=[\"COMPRESS=DEFLATE\"])\n    print(out_file)\n".into()
                } else {
                    "    print(vrt)\n".into()
                },
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_python() {
        let run = Path::new("C:/runs/warp_1");
        let o = janus_zeit::warp::WarpOptions {
            t_srs: Some("EPSG:4326".into()),
            resampling: "bilinear".into(),
            pixel: Some((0.001, 0.001)),
            ..Default::default()
        };
        let s = warp(&["a\\b c.tif".into(), "d.tif".into()], &o, None, run);
        assert!(s.contains(r#""a\\b c.tif""#), "{s}");
        assert!(s.contains("zeit.load_raster(f, crs=\"EPSG:4326\", res=0.001, resampling=\"bilinear\")"), "{s}");
        assert!(!s.contains("osgeo"), "{s}");
        // what Zeit cannot do stays gdalwarp's
        let o = janus_zeit::warp::WarpOptions { tap: true, pixel: Some((30.0, 30.0)), ..Default::default() };
        assert!(warp(&["a.tif".into()], &o, None, run).contains("gdal.Warp(str(path), f, options=options)"));
        let g = janus_zeit::warp::Grid { wkt: "WKT \"x\"".into(), gt: [1.0, 30.0, 0.0, 2.0, 0.0, -30.0], w: 3, h: 4 };
        let o =
            RasterizeOptions { layer: String::new(), burn: Burn::Attribute("cls".into()), all_touched: true, grid: g };
        let s = rasterize("v.gpkg", &o, "v raster.tif", run);
        assert!(s.contains("Affine(30.0, 0.0, 1.0, 0.0, -30.0, 2.0)"), "{s}");
        assert!(s.contains(r#"crs, transform = "WKT \"x\"","#), "{s}");
        assert!(s.contains("all_touched=True") && s.contains("zones[\"cls\"]"), "{s}");
    }

    #[test]
    fn the_prelude_holds_what_the_expression_calls() {
        let p = prelude_for(
            "    return _out(_and(_cmp(b[0], 1.0, b[0] > 1.0), b[1]))
",
        );
        assert!(p.contains("def _and") && p.contains("def _truth") && p.contains("def _cmp") && p.contains("def _f"));
        assert!(p.contains("def _out") && !p.contains("def _bit") && !p.contains("def _qa"), "{p}");
    }

    #[test]
    fn warp_arguments_follow_gdalwarps_options() {
        use janus_zeit::warp::{Format, Grid, WarpOptions};
        let o = WarpOptions { t_srs: Some("EPSG:32722".into()), resampling: "near".into(), ..Default::default() };
        assert_eq!(warp_arguments(&o, None).unwrap(), ", crs=\"EPSG:32722\", resampling=\"nearest\"");
        // a window alone is a clip; with a CRS or a pixel size, or what Zeit cannot do, gdalwarp's own
        let window = Some(([1.0, 2.0, 3.0, 4.0], "x".to_string()));
        let o = WarpOptions { extent: window.clone(), resampling: "near".into(), ..Default::default() };
        assert_eq!(warp_arguments(&o, None).unwrap(), ", clip=(1.0, 2.0, 3.0, 4.0)");
        let o = WarpOptions { extent: window, pixel: Some((30.0, 30.0)), ..Default::default() };
        assert!(warp_arguments(&o, None).is_none());
        for o in [
            WarpOptions { tap: true, ..Default::default() },
            WarpOptions { dst_nodata: Some(0.0), ..Default::default() },
            WarpOptions { data_type: Some("Byte".into()), ..Default::default() },
            WarpOptions { cutline: Some(("v.gpkg".into(), String::new())), ..Default::default() },
        ] {
            assert!(warp_arguments(&o, None).is_none());
        }
        // a layer's grid is a file for `like=`
        let g = Grid { wkt: String::new(), gt: [0.0, 1.0, 0.0, 0.0, 0.0, -1.0], w: 1, h: 1 };
        let o =
            WarpOptions { grid: Some(g), resampling: "bilinear".into(), format: Format::GTiff, ..Default::default() };
        assert!(warp_arguments(&o, None).is_none());
        assert_eq!(warp_arguments(&o, Some("l.tif")).unwrap(), ", like=\"l.tif\", resampling=\"bilinear\"");
    }
}
