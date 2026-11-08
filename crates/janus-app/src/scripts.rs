//! The Python of Janus' own tools (rerun.py beside a run's provenance, as a Zeit run's): the same operation on the
//! same inputs, as short as Zeit lets it be: `load_raster` reads, warps, scales and masks, `save_raster` writes,
//! `rasterize`, `polygonize`, `zonal_stats` and `zonal_areas` do the vector work, `regularize_time_series`, `nd` and
//! `build_vrt` the rest. Only the numpy of the calculator's expression and what Zeit cannot reproduce (a few of
//! gdalwarp's combinations) are written out. The scripts go in the run's folder's `python` subfolder (Janus' own are
//! left alone) and hold only the branches their run took. The Python history shows them (history.rs); the
//! self-test runs them and compares their outputs with Janus'.

use std::path::Path;

use janus_zeit::rasterize::{Burn, PALETTE, RasterizeOptions};

use crate::python::py_str;

/// What a script needs installed, for its first lines.
const NEEDS: &str = "Needs: pip install \"zeit-cdts>=3.2\".";

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

/// A Python list of items already written, as many on a line as fit in 110 columns.
fn wrap_items(v: &[String], open: &str, close: &str) -> String {
    let one = format!("{open}{}{close}", v.join(", "));
    if one.len() <= 100 {
        return one;
    }
    let mut o = format!("{open}\n");
    let mut line = String::new();
    for s in v {
        let item = format!("{s}, ");
        if !line.is_empty() && line.len() + item.len() > 108 {
            o += &format!("    {}\n", line.trim_end());
            line.clear();
        }
        line += &item;
    }
    if !line.is_empty() {
        o += &format!("    {}\n", line.trim_end());
    }
    o + close
}

/// A Python list of texts, as many on a line as fit in 110 columns.
fn py_wrapped(v: &[String]) -> String {
    wrap_items(&v.iter().map(|s| py_str(s)).collect::<Vec<_>>(), "[", "]")
}

fn py_list(v: &[String]) -> String {
    let mut o = String::from("[\n");
    for s in v {
        o += &format!("    {},\n", py_str(s));
    }
    o + "]"
}

/// A call, its arguments filled up to 110 columns and hanging under the first.
fn py_call(head: &str, args: &[String]) -> String {
    let indent = " ".repeat(head.len() + 1);
    let mut out = format!("{head}(");
    let mut line = out.len();
    for (i, a) in args.iter().enumerate() {
        let item = if i + 1 < args.len() { format!("{a}, ") } else { format!("{a})") };
        if i > 0 && line + item.trim_end().len() > 110 {
            out = out.trim_end().to_string() + "\n" + &indent;
            line = indent.len();
        }
        out += &item;
        line += item.len();
    }
    if args.is_empty() {
        out += ")";
    }
    out
}

/// A number as Python reads it back exactly.
fn num(v: f64) -> String {
    if v.is_nan() { "float(\"nan\")".into() } else { format!("{v:?}") }
}

/// A number as short as it can be written (a whole one with no decimals).
fn short(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 { format!("{v:.0}") } else { format!("{v:?}") }
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

/// gdalwarp's data type as numpy's, None where Zeit has no such type.
fn numpy_type(t: &str) -> Option<&'static str> {
    Some(match t {
        "Byte" => "uint8",
        "Int8" => "int8",
        "Int16" => "int16",
        "UInt16" => "uint16",
        "Int32" => "int32",
        "UInt32" => "uint32",
        "Float32" => "float32",
        "Float64" => "float64",
        _ => return None,
    })
}

/// gdalwarp's resampling method as Zeit's, None where Zeit has no such method.
fn resampling_name(r: &str) -> Option<&'static str> {
    Some(match r {
        "" | "near" => "nearest",
        "bilinear" => "bilinear",
        "cubic" => "cubic",
        "cubicspline" => "cubic_spline",
        "lanczos" => "lanczos",
        "average" => "average",
        "mode" => "mode",
        "min" => "min",
        "max" => "max",
        "med" => "med",
        "q1" => "q1",
        "q3" => "q3",
        "rms" => "rms",
        _ => return None,
    })
}

/// The arguments of `load_raster` that do what gdalwarp's options do, None where Zeit does not reproduce gdalwarp: an
/// extent with a cutline, an extent in another CRS than the result's, a new CRS with an extent or a cutline and no
/// pixel size, `-tap` with no pixel size, or a resampling or a type Zeit does not have. `files` are the sources, whose
/// CRS the extent is in when the result keeps it.
fn warp_arguments(opts: &janus_zeit::warp::WarpOptions, like: Option<&str>, files: &[String]) -> Option<String> {
    let mut kw: Vec<String> = vec![];
    let mut moves = false; // the pixels are laid again
    if opts.grid.is_some() {
        kw.push(format!("like={}", py_str(&crate::provenance::absolute(like?))));
        moves = true;
    } else {
        if let Some(s) = &opts.t_srs {
            kw.push(format!("crs={}", py_str(&crs_text(s))));
            moves = true;
        }
        if let Some((x, y)) = opts.pixel {
            kw.push(format!("res={}", if x == y { num(x) } else { format!("({}, {})", num(x), num(y)) }));
            if opts.tap {
                kw.push("tap=True".into());
            }
            moves = true;
        }
        if let Some((e, srs)) = &opts.extent {
            let result_crs = match &opts.t_srs {
                // the new CRS needs the pixel size to lay the cells from the extent
                Some(t) => opts.pixel.is_some().then(|| crs_text(t)),
                None => files.first().and_then(|f| gdal::Dataset::open(f).ok()).map(|ds| crs_text(&ds.projection())),
            };
            if opts.cutline.is_some() || result_crs? != crs_text(srs) {
                return None;
            }
            kw.push(format!("extent=({}, {}, {}, {})", num(e[0]), num(e[1]), num(e[2]), num(e[3])));
            moves = true;
        }
        if opts.cutline.is_some() && opts.t_srs.is_some() && opts.pixel.is_none() {
            return None;
        }
    }
    if let Some((file, layer)) = &opts.cutline {
        let file = py_str(&crate::provenance::absolute(file));
        kw.push(format!("cutline={}", if layer.is_empty() { file } else { format!("({file}, {})", py_str(layer)) }));
    }
    if moves {
        kw.push(format!("resampling={}", py_str(resampling_name(&opts.resampling)?)));
    }
    let dtype = match &opts.data_type {
        Some(t) => Some(numpy_type(t)?),
        None => None,
    };
    // the cells the source does not cover: the number asked, else as gdalwarp leaves them
    if let Some(v) = opts.dst_nodata {
        kw.push(format!("nodata={}", num(v)));
    } else if moves || opts.cutline.is_some() || dtype.is_some() {
        kw.push("nodata=\"source\"".into());
    }
    if let Some(t) = dtype {
        kw.push(format!("dtype={}", py_str(t)));
    }
    Some(kw.iter().map(|k| format!(", {k}")).collect())
}

/// Reproject, resample, clip (janus_zeit::warp::warp_files): Zeit's `load_raster` warps each file as gdalwarp does
/// and `save_raster` writes it under its own name; gdal.Warp itself with gdalwarp's options for what Zeit cannot do.
/// `like` is the file of the layer whose grid is the target.
pub fn warp(files: &[String], opts: &janus_zeit::warp::WarpOptions, like: Option<&str>, run: &Path) -> String {
    let Some(kw) = warp_arguments(opts, like, files) else {
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
        r#""""Reproject, resample, clip as Janus @ver@ ran it: Zeit's load_raster warps each file as gdalwarp does and save_raster writes it@note@. @needs@"""
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
            ("needs", NEEDS.into()),
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
written under its own name (Zeit's load_raster does not reproduce these options together). Needs GDAL's Python bindings (osgeo)."""
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

/// Whether a raster file is on the grid (size and geotransform).
fn same_grid(path: &str, g: &janus_zeit::warp::Grid) -> bool {
    let Ok(ds) = gdal::Dataset::open(path) else { return false };
    let gt = ds.geo_transform().unwrap_or_default();
    ds.raster_size() == (g.w, g.h)
        && gt.iter().zip(&g.gt).all(|(a, b)| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0))
}

/// The classes of a text attribute (its sorted values on the features that have a geometry), None for a number.
fn text_classes(vector: &str, layer: &str, attribute: &str) -> Option<Vec<String>> {
    use gdal::vector::LayerAccess;
    use gdal_sys::OGRFieldType::{OFTInteger, OFTInteger64, OFTReal};
    let ds = gdal::Dataset::open(vector).ok()?;
    let mut l = if layer.is_empty() { ds.layer(0) } else { ds.layer_by_name(layer) }.ok()?;
    let t = l.defn().fields().find(|f| f.name() == attribute)?.field_type();
    if t == OFTInteger || t == OFTInteger64 || t == OFTReal {
        return None;
    }
    let mut names: Vec<String> = vec![];
    for f in l.features() {
        let i = f.field_index(attribute).ok()?;
        if f.geometry().is_some()
            && let Some(Some(v)) = f.field_as_string(i).ok()
        {
            names.push(v);
        }
    }
    names.sort();
    names.dedup();
    Some(names)
}

/// Rasterize on the series' grid (janus_zeit::rasterize): `zeit.rasterize` burns the features in reading order, in
/// the grid's CRS (a later one over an earlier one), with the value Janus burns: 1, the feature's number, or an
/// attribute's (a number as it is, a text as a class numbered from 1 in sorted order, its names and Tableau 20
/// colours written as Janus writes them). `like` is a file on the grid; None when it is not (no script then).
pub fn rasterize(vector: &str, opts: &RasterizeOptions, like: &str, out_name: &str, run: &Path) -> Option<String> {
    if !same_grid(like, &opts.grid) {
        return None;
    }
    let mut args = vec![py_str(&crate::provenance::absolute(vector)), py_str(&crate::provenance::absolute(like))];
    let (mut classes, mut what) = (vec![], "1");
    match &opts.burn {
        Burn::One => {}
        Burn::Index => {
            args.push("burn=\"id\"".into());
            what = "the feature's number (1, 2... counting every feature read)";
        }
        Burn::Attribute(a) => {
            args.push(format!("burn={}", py_str(a)));
            what = "the attribute's value (a text as a class)";
            classes = text_classes(vector, &opts.layer, a).unwrap_or_default();
        }
    }
    if !opts.layer.is_empty() {
        args.push(format!("layer={}", py_str(&opts.layer)));
    }
    if opts.all_touched {
        args.push("all_touched=True".into());
    }
    let burn = py_call("burned = zeit.rasterize", &args);
    let out = py_str(&run.join("python").join(out_name).to_string_lossy());
    let mut s = format!(
        "\"\"\"Rasterize as Janus {} ran it: the layer's features, in the grid's CRS, burned on the series' grid (a later one over an earlier one) with {what}. {NEEDS}\"\"\"\nimport zeit\n\n",
        version()
    );
    if classes.is_empty() {
        s += &format!("{burn}\nprint(zeit.save_raster(burned, {out}))\n");
        return Some(s);
    }
    let names: Vec<String> = classes.iter().enumerate().map(|(k, n)| format!("{}: {}", k + 1, py_str(n))).collect();
    s +=
        &format!("classes = {}  # the text's classes, numbered from 1 in sorted order\n", wrap_items(&names, "{", "}"));
    // Janus colours up to 255 classes (a byte map) with Tableau 20, cycling
    let colors = classes.len() < 255;
    if colors {
        let hex: Vec<String> = (0..classes.len())
            .map(|k| {
                let [r, g, b] = PALETTE[k % PALETTE.len()];
                format!("{}: \"#{r:02x}{g:02x}{b:02x}\"", k + 1)
            })
            .collect();
        s += &format!("colors = {}  # Tableau 20\n", wrap_items(&hex, "{", "}"));
    }
    s += &format!(
        "{burn}\nprint(zeit.save_raster(burned, {out}, categories=classes{}))\n",
        if colors { ", colors=colors" } else { "" }
    );
    Some(s)
}

/// Polygonize (janus_zeit::polygonize): `zeit.polygonize`, the patches through GDAL's polygonizer as Janus, measured
/// on the CRS's ellipsoid as GDAL measures them, into a GeoPackage.
pub fn polygonize(src: &str, opts: &janus_zeit::polygonize::PolyOptions, out_name: &str, run: &Path) -> String {
    // a file of several bands needs the band, whichever it is
    let bands = |path: &str| gdal::Dataset::open(path).map_or(1, |d| d.raster_count());
    let mut args = vec![py_str(&crate::provenance::absolute(src))];
    if opts.band > 1 || bands(src) > 1 {
        args.push(format!("band={}", opts.band.max(1)));
    }
    if opts.eight {
        args.push("connectivity=8".into());
    }
    if opts.min_area_ha > 0.0 {
        args.push(format!("min_area_ha={}", num(opts.min_area_ha)));
    }
    if opts.simplify_m > 0.0 {
        args.push(format!("simplify={}", num(opts.simplify_m)));
    }
    if !opts.classes.is_empty() {
        args.push(format!("classes={}", py_names(&opts.classes)));
    }
    if opts.years {
        args.push("years=True".into());
    }
    if let Some((name, path, band)) = &opts.mean_of {
        let col: String =
            name.chars().map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '_' }).collect();
        args.push(format!("mean_of={}", py_str(&crate::provenance::absolute(path))));
        if *band > 1 || bands(path) > 1 {
            args.push(format!("mean_band={}", (*band).max(1)));
        }
        args.push(format!("mean_name={}", py_str(&format!("mean_{}", col.trim_matches('_')))));
    }
    let layer = if opts.layer.is_empty() { "polygons" } else { &opts.layer };
    fill(
        r#""""Polygonize as Janus @ver@ ran it: the map's patches (pixels of one value connected @conn@-wise) as polygons in a GeoPackage, each with its value@what@, area (ha) and perimeter (m) on the ellipsoid and pixels. @needs@"""
from pathlib import Path

import zeit

@call@
out = Path(@out@)
out.parent.mkdir(parents=True, exist_ok=True)
patches.to_file(out, layer=@layer@, driver="GPKG")
"#,
        &[
            ("ver", version().into()),
            ("conn", if opts.eight { "8" } else { "4" }.into()),
            (
                "what",
                format!(
                    "{}{}{}",
                    if opts.classes.is_empty() { "" } else { ", class" },
                    if opts.years { ", date" } else { "" },
                    opts.mean_of.as_ref().map_or(String::new(), |m| format!(", the mean of {}", m.0))
                ),
            ),
            ("needs", NEEDS.into()),
            ("call", py_call("patches = zeit.polygonize", &args)),
            ("out", py_str(&run.join("python").join(out_name).to_string_lossy())),
            ("layer", py_str(layer)),
        ],
    )
}

/// Zonal statistics over time (janus_zeit::zonal): `zeit.zonal_stats` or `zeit.zonal_areas`, each feature burned
/// alone on the grid (its pixels, whose centre is inside or every one touched), each map's values in it:
/// statistics, or each class's (year's) pixels and true area, and the transitions; the same CSVs.
#[allow(clippy::too_many_arguments)]
pub fn zonal(
    vector: &str,
    sources: &[janus_zeit::areas::AreaSource],
    opts: &janus_zeit::zonal::ZonalOptions,
    stat: usize,
    names: &[(i64, String)],
    run: &Path,
) -> String {
    use janus_zeit::zonal::ZonalKind;
    const STATS: [&str; 10] = ["n", "mean", "std", "min", "p10", "p25", "median", "p75", "p90", "max"];
    let kind = opts.kind;
    let classes = kind == ZonalKind::Classes;
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
    let mut args = vec![py_str(&crate::provenance::absolute(vector)), "maps".to_string()];
    if kind == ZonalKind::Years {
        args.push("kind=\"years\"".into());
    }
    if classes && !names.is_empty() {
        args.push(format!("names={}", py_names(&names)));
    }
    if let Some((a, b)) = transitions {
        args.push(format!("transitions=({a}, {b})"));
    }
    if !opts.layer.is_empty() {
        args.push(format!("layer={}", py_str(&opts.layer)));
    }
    if !opts.id_field.is_empty() {
        args.push(format!("id_field={}", py_str(&opts.id_field)));
    }
    if opts.all_touched {
        args.push("all_touched=True".into());
    }
    if kind == ZonalKind::Continuous && STATS[stat.min(9)] != "mean" {
        args.push(format!("stat={}", py_str(STATS[stat.min(9)])));
    }
    if let Some(b) = opts.window {
        args.push(format!("window=({}, {}, {}, {})", b[0], b[1], b[2], b[3]));
    }
    let mut s = format!(
        "\"\"\"Zonal statistics over time as Janus {} ran them: each feature of the layer is a zone, its pixels on the maps' grid those whose centre is inside it{} (a pixel in several features counts in each); {what}. {NEEDS}\"\"\"\nimport zeit\n\nmaps = [  # (file, band, label)\n",
        version(),
        if opts.all_touched { " (or that it touches)" } else { "" }
    );
    for m in sources {
        s += &format!(
            "    ({}, {}, {}),\n",
            py_str(&crate::provenance::absolute(&m.path)),
            m.band.max(1),
            py_str(&m.label)
        );
    }
    s += "]\n";
    let function = if kind == ZonalKind::Continuous { "zeit.zonal_stats" } else { "zeit.zonal_areas" };
    s += &py_call(&format!("tables = {function}"), &args);
    s += &format!("\ntables.to_csv({})\n", out_dir(run));
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

/// Zeit's name for a quality band's rule.
fn qa_rule_name(rule: janus_cube::QaRule) -> &'static str {
    use janus_cube::QaRule;
    match rule {
        QaRule::Fmask => "fmask",
        QaRule::LandsatC2 => "landsat_c2",
        QaRule::Scl => "scl",
        QaRule::HlsFmask => "hls_fmask",
        _ => "nonzero",
    }
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

/// The name a band of the files goes by in the cube: the file's, else `band N`, never one twice.
fn band_name(info: &janus_cube::CubeInfo, band: i32, used: &[String]) -> String {
    let own = info.band_names.get((band - 1).max(0) as usize).filter(|n| !n.is_empty());
    let name = own.cloned().unwrap_or_else(|| format!("band {band}"));
    if used.contains(&name) { format!("{name} ({band})") } else { name }
}

/// Regularize series (janus_zeit::regularize): the series read as Janus reads it over the window (`load_raster` of
/// its files on their dates: the bands by role, the scale and offset, the quality band), each period's median,
/// medoid or clearest observation, the gaps filled, all by `zeit.regularize_time_series`, a GeoTIFF per period.
/// None for a series whose dates are not each a file or each a band of one file (no script then).
pub fn regularize(
    info: &janus_cube::CubeInfo,
    opts: &janus_zeit::regularize::RegOptions,
    run: &Path,
) -> Option<String> {
    use janus_zeit::regularize::{Composite, Fill, Step};
    let first = info.layers.first()?;
    if !info.time_is_date
        || info.multidim
        || info.spectral
        || info.stack
        || info.layers.iter().any(|l| l.band_base != 0)
    {
        return None;
    }
    let sel = info.sel;
    let qa_on = sel.qa_band > 0 && sel.qa_rule != janus_cube::QaRule::None;
    let n_bands = info.bands_per_date.max(1);
    let multi = opts.all_bands && n_bands > 1;
    let expr = info.expr.as_ref().filter(|_| !opts.all_bands);
    let nd = sel.nd_band > 0 && !opts.all_bands && expr.is_none();
    // one file with a band a date, or a file a date and the same band of each
    let stack = info.t() > 1 && info.layers.iter().all(|l| l.path == first.path);
    if (stack && (info.expr.is_some() || multi)) || (!stack && info.layers.iter().any(|l| l.band != first.band)) {
        return None;
    }
    let paths: Vec<String> = info.layers.iter().map(|l| crate::provenance::absolute(&l.path)).collect();
    // the bands read, by role where there are several
    let mut used: Vec<String> = vec![];
    let mut roles = |bands: &[i32]| -> Vec<(String, i32)> {
        bands
            .iter()
            .map(|&b| {
                let n = band_name(info, b, &used);
                used.push(n.clone());
                (n, b)
            })
            .collect()
    };
    let wanted: Vec<i32> = if let Some(e) = expr {
        e.bands.clone()
    } else if multi {
        (1..=n_bands as i32).collect()
    } else if nd {
        vec![first.band, sel.nd_band]
    } else {
        vec![]
    };
    let roles = roles(&wanted);
    let mut load: Vec<String> = vec![];
    if stack {
        // every band a date, unless the file holds others
        let all: Vec<i32> = info.layers.iter().map(|l| l.band).collect();
        let count = gdal::Dataset::open(&first.path).map_or(0, |d| d.raster_count() as i32);
        if all != (1..=count).collect::<Vec<_>>() {
            load.push(format!("band=[{}]", all.iter().map(i32::to_string).collect::<Vec<_>>().join(", ")));
        }
    } else if !roles.is_empty() {
        let items: Vec<String> = roles.iter().map(|(n, b)| format!("{}: {b}", py_str(n))).collect();
        load.push(format!("bands={{{}}}", items.join(", ")));
    } else if n_bands > 1 {
        load.push(format!("band={}", first.band));
    }
    // the bands read, to know whether any has a scale or offset
    let mut read_bands: Vec<(String, i32)> = vec![];
    for l in &info.layers {
        let bands = if stack || roles.is_empty() { vec![l.band] } else { roles.iter().map(|r| r.1).collect() };
        read_bands.extend(bands.into_iter().map(|b| (l.path.clone(), b)));
    }
    if qa_on && !stack && expr.is_none() {
        load.push(format!("qa={}", sel.qa_band));
        load.push(format!("qa_rule={}", py_str(qa_rule_name(sel.qa_rule))));
    }
    if scaled(&read_bands) {
        load.push("scale=True".into());
    }
    let [x0, y0, x1, y1] = opts.window.unwrap_or([0, 0, info.width, info.height]);
    if opts.window.is_some() {
        load.push(format!("window=({x0}, {y0}, {x1}, {y1})"));
    }
    load.extend(["masked=True".to_string(), "chunks=\"auto\"".to_string()]);
    let source = if stack { py_str(&paths[0]) } else { "files".to_string() };
    let days: Vec<String> = info.layers.iter().map(|l| day_text((l.time / 86400.0).floor() as i64)).collect();
    let (freq, step_text) = match opts.step {
        Step::Days(n) => (format!("{n}D"), format!("every {n} days from the first date")),
        Step::Month => ("MS".to_string(), "calendar months".to_string()),
        Step::Year => ("YS".to_string(), "calendar years".to_string()),
    };
    let (method, method_text) = match opts.composite {
        Composite::Median => (None, "median"),
        Composite::Medoid => (Some("medoid"), "medoid"),
        Composite::Clearest => (Some("clearest"), "clearest observation"),
    };
    let mut reg = vec!["cube".to_string(), py_str(&freq)];
    reg.extend(method.map(|m| format!("method={}", py_str(m))));
    let fill_text = match opts.fill {
        Fill::None => "",
        Fill::Linear => ", the gaps filled linearly in time",
        Fill::Whittaker(_) => ", the gaps filled by a Whittaker curve",
    };
    match opts.fill {
        Fill::None => {}
        Fill::Linear | Fill::Whittaker(_) => {
            reg.push(format!(
                "fill={}",
                py_str(if matches!(opts.fill, Fill::Linear) { "linear" } else { "whittaker" })
            ));
            if opts.max_gap_days > 0.0 {
                reg.push(format!("max_gap={}", short(opts.max_gap_days)));
            }
            if let Fill::Whittaker(lambda) = opts.fill
                && lambda != 10.0
            {
                reg.push(format!("lmbda={}", short(lambda)));
            }
        }
    }
    let names: Vec<String> = if multi { vec![] } else { vec![info.selection_text()] };
    let band_arg = if names.first().is_none_or(|n| n.is_empty()) {
        String::new()
    } else {
        format!(", band_names={}", py_inline(&names))
    };
    let description = info.description.replace('\n', " ")
        + &if info.selection_text().is_empty() { String::new() } else { format!(": {}", info.selection_text()) };
    let mut s = format!(
        "\"\"\"Regularize series as Janus {} ran it: Zeit's regularize_time_series makes the {method_text} of each period ({step_text}){fill_text}, a GeoTIFF per period. {NEEDS}\"\"\"\nfrom pathlib import Path\n\n",
        version()
    );
    if expr.is_some() {
        s += "import numpy as np\n";
    }
    s += &format!("import zeit\n\n# {description}\n");
    if !stack {
        s += &format!("files = {}\n", py_list(&paths));
    }
    s += &format!("dates = {}\n", py_wrapped(&days));
    // an expression of the date's variables (year, day of the year...) is given them for each date
    let dated = expr.is_some_and(|e| e.program.uses_date());
    if let Some(e) = expr {
        let mut f = numpy_function(&e.program);
        if !dated {
            f = f.replacen("def expression(b, year, month, day, doy, decyear, t):", "def expression(b):", 1);
        }
        s += &format!("\n\n{}\n\n\n", f.trim_end());
    }
    let mut load_args = vec![source, "dates=dates".to_string()];
    load_args.extend(load);
    s += &py_call("cube = zeit.load_raster", &load_args);
    s += "\n";
    if nd {
        s += &format!("cube = zeit.normalized_difference(cube, {}, {})\n", py_str(&roles[0].0), py_str(&roles[1].0));
    }
    if expr.is_some() {
        // the expression date by date, its variables then the quality band's mask
        if dated {
            let vars: Vec<String> = (0..info.t())
                .map(|t| {
                    let v: Vec<String> =
                        janus_cube::calc::plan::date_vars(info, t).0.iter().map(|&x| num(f64::from(x))).collect();
                    format!("({})", v.join(", "))
                })
                .collect();
            s += &format!(
                "when = {}  # each date's year, month, day, day of the year, decimal year and place\n",
                wrap_items(&vars, "[", "]")
            );
        }
        s += &format!(
            "data, shape = cube.values, cube.shape[-2:]\nwith np.errstate(all=\"ignore\"):\n    values = np.stack(\n        [np.broadcast_to(expression(list(data[i].astype(\"float64\")){}), shape) for i in range(len(data))]\n    )\ncube = cube.isel(band=0, drop=True).copy(data=values.astype(\"float32\"))\n",
            if dated { ", *when[i]" } else { "" }
        );
        if qa_on {
            let mut qa = vec!["files".to_string(), "dates=dates".to_string(), format!("band={}", sel.qa_band)];
            if opts.window.is_some() {
                qa.push(format!("window=({x0}, {y0}, {x1}, {y1})"));
            }
            qa.push("chunks=\"auto\"".into());
            s += &format!(
                "usable = zeit.qa_mask({}, {})  # the quality band's mask, after the expression\ncube = cube.where(usable)\n",
                py_call("zeit.load_raster", &qa),
                py_str(qa_rule_name(sel.qa_rule))
            );
        }
    }
    s += &format!("reg = {}.load()\n", py_call("zeit.regularize_time_series", &reg));
    s += &format!("stem, out = {}, Path({})\n", py_str(&opts.stem), out_dir(run));
    s += &format!(
        "for layer in reg:\n    print(zeit.save_raster(layer.astype(\"float32\"), out / f\"{{stem}}_{{layer.time.dt.strftime('%Y%m%d').item()}}.tif\"{band_arg}))\n"
    );
    Some(s)
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
        r#""""The raster calculator as Janus @ver@ ran it: each date's inputs read as its VRTs read them (the bands' no data as NaN@scale_doc@), the expression in numpy, a GeoTIFF per date with Zeit's save_raster (Janus writes a VRT per date, or a COG). numpy computes in float64 and Janus in float32: the results agree to about 1e-6. @needs@"""
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
    return zeit.load_raster(path, band=band, masked=not raw@scale_arg@).values.astype("float32")


for stem, when, sources in dates:
    with np.errstate(all="ignore"):
        result = expression([read(s).astype("float64") for s in sources], *when)
    print(zeit.save_raster(np.broadcast_to(result, shape).astype("float32"), out / f"{stem}.tif", like=like))
"#,
        &[
            ("ver", version().into()),
            ("needs", NEEDS.into()),
            ("quoted", quoted),
            ("inputs", inputs.join("\n")),
            ("function", numpy_function(&plan.program)),
            ("like", py_str(&crate::provenance::absolute(&grid.layers.first()?.path))),
            ("h", grid.height.to_string()),
            ("w", grid.width.to_string()),
            ("out", out_dir(run)),
            ("dates", dates),
            ("scale_doc", if scale { ", their scale and offset applied".into() } else { String::new() }),
            ("scale_arg", if scale { ", scale=not raw".into() } else { String::new() }),
        ],
    ))
}

/// Build series (janus_zeit::series_io::build): a VRT per date through `zeit.build_vrt` with the same grid (the
/// files' extent at their finest pixel), the bands named; written as COGs when Janus wrote COGs.
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
    let (what, dates_what) = if tiled {
        (
            "each band's file, or its tiles mosaicked, the bands stacked",
            "each band's files (one, or the tiles mosaicked)",
        )
    } else if plan.multiband {
        ("the date's files mosaicked as they are, their bands kept", "the files of its tiles")
    } else {
        ("each band's file, the bands stacked", "each band's file")
    };
    let (rx, ry) = (g.gt[1], -g.gt[5]);
    let named = plan.bands.len() > 3;
    let mut build = vec![
        "files".to_string(),
        if cog {
            "out / \".part\" / f\"{name}.vrt\"".to_string()
        } else {
            "out / f\"{stem}_{label.replace('-', '')}.vrt\"".to_string()
        },
    ];
    if !tiled && !plan.multiband {
        build.push("separate=True".into());
    }
    build.push(format!("extent=({}, {}, {}, {})", num(x0), num(y0), num(x1), num(y1)));
    build.push(format!("res={}", if rx == ry { num(rx) } else { format!("({}, {})", num(rx), num(ry)) }));
    build.push(if named { "band_names=band_names".into() } else { format!("band_names={}", py_inline(&plan.bands)) });
    if !wl.is_empty() {
        build.push("wavelengths=wavelengths".into());
    }
    // the call inside the loop: indented under it
    let call = if cog {
        format!("vrt = {}", py_call("vrt = zeit.build_vrt", &build).trim_start_matches("vrt = "))
    } else {
        format!("{})", py_call("print(zeit.build_vrt", &build))
    };
    let call = format!("    {}", call.replace('\n', "\n    "));
    let band_names = if named { format!("band_names = {}\n", py_wrapped(&plan.bands)) } else { String::new() };
    let wavelengths = if wl.is_empty() {
        String::new()
    } else {
        format!("wavelengths = {{{}}}  # nm, of the Sentinel-2 bands named\n", wl.join(", "))
    };
    fill(
        r#""""Build series as Janus @ver@ ran it: a VRT per date (zeit.build_vrt: @what@), on the grid of the files' extent at their finest pixel@cogs@. @needs@"""
from pathlib import Path
@rasterio@
import zeit

@band_names@@wavelengths@# Each date: its label and @dates_what@
dates = @dates@
stem, out = @stem@, Path(@out@)
for label, files in dates:
@name@@call@
@write@"#,
        &[
            ("ver", version().into()),
            ("what", what.into()),
            ("needs", NEEDS.into()),
            ("cogs", if cog { ", then written as cloud-optimized GeoTIFFs".into() } else { String::new() }),
            ("rasterio", if cog { "\nimport rasterio.shutil".into() } else { String::new() }),
            ("band_names", band_names),
            ("wavelengths", wavelengths),
            ("dates_what", dates_what.into()),
            ("dates", dates),
            ("stem", py_str(stem)),
            ("out", out_dir(run)),
            ("name", if cog { "    name = f\"{stem}_{label.replace('-', '')}\"\n".into() } else { String::new() }),
            ("call", call),
            (
                "write",
                if cog {
                    "    rasterio.shutil.copy(vrt, out / f\"{name}.tif\", driver=\"COG\", compress=\"deflate\")\n    print(out / f\"{name}.tif\")\n"
                        .into()
                } else {
                    String::new()
                },
            ),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use janus_zeit::warp::{Format, Grid, WarpOptions};

    #[test]
    fn scripts_are_python() {
        let run = Path::new("C:/runs/warp_1");
        let o = WarpOptions {
            t_srs: Some("EPSG:4326".into()),
            resampling: "bilinear".into(),
            pixel: Some((0.001, 0.001)),
            ..Default::default()
        };
        let s = warp(&["a\\b c.tif".into(), "d.tif".into()], &o, None, run);
        assert!(s.contains(r#""a\\b c.tif""#), "{s}");
        assert!(
            s.contains("zeit.load_raster(f, crs=\"EPSG:4326\", res=0.001, resampling=\"bilinear\", nodata=\"source\")"),
            "{s}"
        );
        assert!(!s.contains("osgeo") && s.contains("zeit-cdts>=3.2"), "{s}");
        // what Zeit does not reproduce stays gdalwarp's: a new CRS with a cutline and no pixel size
        let o = WarpOptions {
            t_srs: Some("EPSG:4326".into()),
            cutline: Some(("v.gpkg".into(), String::new())),
            ..Default::default()
        };
        assert!(warp(&["a.tif".into()], &o, None, run).contains("gdal.Warp(str(path), f, options=options)"));
    }

    #[test]
    fn rasterize_calls_zeit() {
        let run = Path::new("C:/runs/r");
        let d = std::env::temp_dir().join(format!("janus_scripts_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let like = d.join("grid.tif");
        let g = Grid { wkt: String::new(), gt: [1.0, 30.0, 0.0, 2.0, 0.0, -30.0], w: 3, h: 4 };
        let mut ds = gdal::DriverManager::get_driver_by_name("GTiff")
            .unwrap()
            .create_with_band_type::<u8, _>(&like, 3, 4, 1)
            .unwrap();
        ds.set_geo_transform(&g.gt).unwrap();
        drop(ds);
        let like = like.to_string_lossy().into_owned();
        let o = RasterizeOptions {
            layer: "l".into(),
            burn: Burn::Attribute("cls".into()),
            all_touched: true,
            grid: g.clone(),
        };
        let s = rasterize("v.gpkg", &o, &like, "v raster.tif", run).unwrap();
        // the call wraps at 110 columns, and where depends on the length of the temporary folder's path
        let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(flat.contains("burn=\"cls\", layer=\"l\", all_touched=True"), "{s}");
        // a grid the file is not on has no script
        let other = Grid { w: 5, ..g };
        let o = RasterizeOptions { grid: other, ..o };
        assert!(rasterize("v.gpkg", &o, &like, "v.tif", run).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn vector_scripts_call_zeit() {
        use janus_zeit::areas::AreaSource;
        use janus_zeit::zonal::{ZonalKind, ZonalOptions};
        let run = Path::new("C:/runs/p");
        let o = janus_zeit::polygonize::PolyOptions {
            band: 1,
            eight: true,
            min_area_ha: 2.0,
            simplify_m: 10.0,
            classes: vec![(1, "A b".into())],
            years: true,
            mean_of: Some(("NIR map".into(), "m.tif".into(), 1)),
            layer: String::new(),
        };
        let s = polygonize("a.tif", &o, "p.gpkg", run);
        for want in [
            "connectivity=8",
            "min_area_ha=2.0",
            "simplify=10.0",
            "classes={1: \"A b\"}",
            "years=True",
            "mean_of=\"m.tif\"",
            "mean_name=\"mean_nir_map\"",
            "layer=\"polygons\"",
        ] {
            assert!(s.contains(want), "{want}: {s}");
        }
        assert!(!s.contains("osgeo") && !s.contains("rasterio"), "{s}");
        let maps = vec![AreaSource { path: "a.tif".into(), band: 2, label: "2000".into() }];
        let o = ZonalOptions { kind: ZonalKind::Continuous, window: Some([1, 2, 3, 4]), ..Default::default() };
        let s = zonal("z.gpkg", &maps, &o, 6, &[], run);
        assert!(s.contains("(\"a.tif\", 2, \"2000\")"), "{s}");
        assert!(s.contains("zeit.zonal_stats(\"z.gpkg\", maps, stat=\"median\", window=(1, 2, 3, 4))"), "{s}");
        let o = ZonalOptions { kind: ZonalKind::Years, all_touched: true, ..Default::default() };
        let s = zonal("z.gpkg", &maps, &o, 1, &[], run);
        assert!(s.contains("zeit.zonal_areas(\"z.gpkg\", maps, kind=\"years\", all_touched=True)"), "{s}");
        assert!(s.contains("tables.to_csv("), "{s}");
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
        let args = |o: &WarpOptions| warp_arguments(o, None, &[]);
        let o = WarpOptions { t_srs: Some("EPSG:32722".into()), resampling: "near".into(), ..Default::default() };
        assert_eq!(args(&o).unwrap(), ", crs=\"EPSG:32722\", resampling=\"nearest\", nodata=\"source\"");
        // the pixels aligned, a no data value and a type of the output
        let o = WarpOptions {
            pixel: Some((30.0, 30.0)),
            tap: true,
            dst_nodata: Some(0.0),
            data_type: Some("Byte".into()),
            resampling: "bilinear".into(),
            ..Default::default()
        };
        assert_eq!(args(&o).unwrap(), ", res=30.0, tap=True, resampling=\"bilinear\", nodata=0.0, dtype=\"uint8\"");
        // an extent in the result's CRS (the files', here a new CRS beside it needs the pixel size)
        let e = Some(([1.0, 2.0, 3.0, 4.0], "EPSG:32722".to_string()));
        let o = WarpOptions {
            t_srs: Some("EPSG:32722".into()),
            pixel: Some((30.0, 30.0)),
            extent: e.clone(),
            resampling: "near".into(),
            ..Default::default()
        };
        assert!(args(&o).unwrap().contains(", extent=(1.0, 2.0, 3.0, 4.0)"));
        for o in [
            // no pixel size with a new CRS, an extent in another CRS, an extent with a cutline
            WarpOptions { t_srs: Some("EPSG:32722".into()), extent: e.clone(), ..Default::default() },
            WarpOptions {
                t_srs: Some("EPSG:4326".into()),
                pixel: Some((1.0, 1.0)),
                extent: e.clone(),
                ..Default::default()
            },
            WarpOptions {
                t_srs: Some("EPSG:32722".into()),
                pixel: Some((30.0, 30.0)),
                extent: e,
                cutline: Some(("v.gpkg".into(), String::new())),
                ..Default::default()
            },
            WarpOptions {
                t_srs: Some("EPSG:4326".into()),
                cutline: Some(("v.gpkg".into(), String::new())),
                ..Default::default()
            },
        ] {
            assert!(args(&o).is_none());
        }
        // a cutline alone, with its layer
        let o = WarpOptions { cutline: Some(("v.gpkg".into(), "l".into())), ..Default::default() };
        assert_eq!(args(&o).unwrap(), ", cutline=(\"v.gpkg\", \"l\"), nodata=\"source\"");
        // a layer's grid is a file for `like=`
        let g = Grid { wkt: String::new(), gt: [0.0, 1.0, 0.0, 0.0, 0.0, -1.0], w: 1, h: 1 };
        let o =
            WarpOptions { grid: Some(g), resampling: "bilinear".into(), format: Format::GTiff, ..Default::default() };
        assert!(args(&o).is_none());
        assert_eq!(
            warp_arguments(&o, Some("l.tif"), &[]).unwrap(),
            ", like=\"l.tif\", resampling=\"bilinear\", nodata=\"source\""
        );
    }
}
