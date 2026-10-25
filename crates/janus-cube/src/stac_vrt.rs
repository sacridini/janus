//! A STAC search as a series (stac.rs): each date's items mosaicked and the chosen bands stacked, on one grid, as a
//! VRT that points at the assets in place (nothing is downloaded until read; GDAL takes the overviews of the
//! cloud-optimized GeoTIFFs for a zoomed-out map). The grid is the CRS most items have, at the finest resolution of
//! the bands (or the one asked for), aligned to their pixels, over the area searched. Items in another CRS (a tile
//! of the next UTM zone) are warped onto it: a warped VRT made from a local stand-in of the same grid, its source
//! replaced, so making it needs no request either. The files: one VRT per date, named by it (the dates of the
//! series), warp/ for the warped items, footprints.geojson.

use std::collections::HashMap;
use std::ffi::{CString, c_char, c_int, c_void};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use gdal::Metadata;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};

use crate::QaRule;
use crate::stac::{Asset, BandChoice, Item, SAS_METADATA, asset_path, azure_blob, group_by_day};

/// The grid of one asset: its CRS ("EPSG:n" or WKT), geotransform, size, data type, nodata.
#[derive(Clone, Debug, PartialEq)]
pub struct Geom {
    pub crs: String,
    pub gt: [f64; 6],
    pub w: usize,
    pub h: usize,
    pub dtype: String,
    pub nodata: Option<f64>,
}

/// The series' grid.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    pub crs: String,
    pub gt: [f64; 6],
    pub w: usize,
    pub h: usize,
}

/// What to build: the items of the dates kept, the bands in their order, the area (longitude, latitude).
pub struct BuildSpec {
    pub items: Vec<Item>,
    pub bands: Vec<BandChoice>,
    pub bbox: [f64; 4],
    /// the cell size (None: the finest of the bands)
    pub res: Option<f64>,
    pub dir: PathBuf,
    /// Planetary Computer: assets through /vsiaz/, signed
    pub planetary: bool,
}

/// What was built: the dates' VRTs in order, the grid, how many items were warped.
#[derive(Debug)]
pub struct Built {
    pub files: Vec<String>,
    pub grid: Grid,
    pub warped: usize,
}

/// The largest side of a series' grid (cells).
pub const MAX_SIDE: usize = 60000;

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// A number as GDAL reads it back exactly.
fn num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 { format!("{}", v as i64) } else { format!("{v:?}") }
}

/// The grid an item's asset has according to STAC (its own proj:shape and proj:transform, else the item's when
/// the asset is of the item's resolution), without its data type.
fn stac_geom(it: &Item, a: &Asset) -> Option<(String, [f64; 6], usize, usize)> {
    let crs = it.crs.clone()?;
    if let (Some(gt), Some(s)) = (a.gt, a.shape) {
        return Some((crs, gt, s[1], s[0]));
    }
    let (gt, s) = (it.gt?, it.shape?);
    if a.resolution.is_some_and(|r| (r - gt[1].abs()).abs() > 1e-6 * r) {
        return None;
    }
    Some((crs, gt, s[1], s[0]))
}

/// The grid of a file, read with GDAL (a request or two).
fn read_geom(path: &str) -> Result<Geom, String> {
    let ds = crate::open_raster(path).map_err(|e| format!("{path}: {e}"))?;
    let (w, h) = ds.raster_size();
    let gt = ds.geo_transform().map_err(|e| format!("{path}: no geotransform ({e})"))?;
    let srs = ds.spatial_ref().map_err(|e| format!("{path}: no CRS ({e})"))?;
    let crs = match (srs.auth_name(), srs.auth_code()) {
        (Some(a), Ok(c)) if a == "EPSG" => format!("EPSG:{c}"),
        _ => srs.to_wkt().map_err(|e| e.to_string())?,
    };
    let b = ds.rasterband(1).map_err(|e| format!("{path}: {e}"))?;
    Ok(Geom { crs, gt, w, h, dtype: b.band_type().name(), nodata: b.no_data_value() })
}

fn srs(def: &str) -> Result<SpatialRef, String> {
    let mut s = SpatialRef::from_definition(def).map_err(|e| format!("CRS not understood ({e})"))?;
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(s)
}

/// The area (longitude, latitude) in a CRS: its edges, densified (a projection curves them).
fn bbox_in(bbox: [f64; 4], crs: &str) -> Result<[f64; 4], String> {
    let ll = srs("EPSG:4326")?;
    let to = srs(crs)?;
    let ct = CoordTransform::new(&ll, &to).map_err(|e| e.to_string())?;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    let n = 16;
    for i in 0..=n {
        let f = i as f64 / n as f64;
        let lon = bbox[0] + f * (bbox[2] - bbox[0]);
        let lat = bbox[1] + f * (bbox[3] - bbox[1]);
        for (x, y) in [(lon, bbox[1]), (lon, bbox[3]), (bbox[0], lat), (bbox[2], lat)] {
            let (mut xs, mut ys, mut zs) = ([x], [y], [0.0]);
            if ct.transform_coords(&mut xs, &mut ys, &mut zs).is_ok() && xs[0].is_finite() && ys[0].is_finite() {
                b = [b[0].min(xs[0]), b[1].min(ys[0]), b[2].max(xs[0]), b[3].max(ys[0])];
            }
        }
    }
    if b[2] <= b[0] || b[3] <= b[1] {
        return Err("the area could not be placed in the items' CRS".into());
    }
    Ok(b)
}

/// The grid over `area` (in `crs`) of cells of `res`, aligned to the pixels of `like` (an asset in that CRS).
pub fn grid_over(area: [f64; 4], crs: &str, res: f64, like: &[f64; 6]) -> Result<Grid, String> {
    let (ox, oy) = (like[0], like[3]);
    let x0 = ox + ((area[0] - ox) / res).floor() * res;
    let y1 = oy - ((oy - area[3]) / res).floor() * res;
    let w = ((area[2] - x0) / res).ceil().max(1.0);
    let h = ((y1 - area[1]) / res).ceil().max(1.0);
    if w > MAX_SIDE as f64 || h > MAX_SIDE as f64 {
        return Err(format!(
            "the area is {w:.0} x {h:.0} cells of {res} m: zoom in, use an ROI or coarser cells (at most {MAX_SIDE} a \
             side)"
        ));
    }
    Ok(Grid { crs: crs.to_string(), gt: [x0, res, 0.0, y1, 0.0, -res], w: w as usize, h: h as usize })
}

/// Of a file's grid, the window that covers the series' grid, clipped to the file: (source x, y, w, h), and where
/// it goes (x, y, w, h). None if they do not meet.
pub fn source_window(g: &Geom, grid: &Grid) -> Option<([f64; 4], [f64; 4])> {
    let (rx, ry) = (g.gt[1], -g.gt[5]);
    if g.gt[2] != 0.0 || g.gt[4] != 0.0 || rx <= 0.0 || ry <= 0.0 {
        return None;
    }
    let res = grid.gt[1];
    let (sx, sy) = ((grid.gt[0] - g.gt[0]) / rx, (g.gt[3] - grid.gt[3]) / ry);
    let (sw, sh) = (grid.w as f64 * res / rx, grid.h as f64 * res / ry);
    let (cx0, cy0) = (sx.max(0.0), sy.max(0.0));
    let (cx1, cy1) = ((sx + sw).min(g.w as f64), (sy + sh).min(g.h as f64));
    if cx1 <= cx0 || cy1 <= cy0 {
        return None;
    }
    let (kx, ky) = (rx / res, ry / res);
    Some(([cx0, cy0, cx1 - cx0, cy1 - cy0], [(cx0 - sx) * kx, (cy0 - sy) * ky, (cx1 - cx0) * kx, (cy1 - cy0) * ky]))
}

/// A list of arguments for GDAL's C utilities.
struct Args {
    _owned: Vec<CString>,
    p: Vec<*mut c_char>,
}

impl Args {
    fn new(a: &[String]) -> Args {
        let owned: Vec<CString> = a.iter().map(|s| CString::new(s.as_str()).unwrap_or_default()).collect();
        let mut p: Vec<*mut c_char> = owned.iter().map(|c| c.as_ptr() as *mut c_char).collect();
        p.push(std::ptr::null_mut());
        Args { _owned: owned, p }
    }
}

fn last_error(what: &str) -> String {
    // SAFETY: GDAL's thread-local last error message.
    let m = unsafe { std::ffi::CStr::from_ptr(gdal_sys::CPLGetLastErrorMsg()) }.to_string_lossy().into_owned();
    if m.is_empty() { what.to_string() } else { format!("{what}: {m}") }
}

/// A warped VRT of a file of grid `g` onto `grid` (XML, its source to be filled in): made from a stand-in of the
/// same grid, without the file.
fn warp_template(g: &Geom, grid: &Grid, resampling: &str) -> Result<String, String> {
    let mut stub = format!(
        "<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\"><SRS>{}</SRS><GeoTransform>{}</GeoTransform>\
         <VRTRasterBand dataType=\"{}\" band=\"1\">",
        g.w,
        g.h,
        xml_escape(&g.crs),
        g.gt.map(num).join(","),
        g.dtype
    );
    if let Some(nd) = g.nodata {
        let _ = write!(stub, "<NoDataValue>{}</NoDataValue>", num(nd));
    }
    stub += "</VRTRasterBand></VRTDataset>";
    let src = gdal::Dataset::open(&stub).map_err(|e| format!("a grid's stand-in: {e}"))?;
    let mut a: Vec<String> = ["-of", "VRT", "-r", resampling].map(String::from).to_vec();
    a.extend(["-t_srs".into(), grid.crs.clone(), "-te".into()]);
    let (x1, y0) = (grid.gt[0] + grid.w as f64 * grid.gt[1], grid.gt[3] + grid.h as f64 * grid.gt[5]);
    a.extend([grid.gt[0], y0, x1, grid.gt[3]].map(num));
    a.extend(["-ts".into(), grid.w.to_string(), grid.h.to_string()]);
    if let Some(nd) = g.nodata {
        a.extend(["-srcnodata".into(), num(nd), "-dstnodata".into(), num(nd)]);
    }
    let args = Args::new(&a);
    let empty = CString::new("").unwrap();
    // SAFETY: GDAL's C utility on a live handle; the result is owned by the Dataset below.
    let out = unsafe {
        gdal_sys::CPLErrorReset();
        let wo = gdal_sys::GDALWarpAppOptionsNew(args.p.as_ptr() as *mut *mut c_char, std::ptr::null_mut());
        let mut srcs = [src.c_dataset()];
        let mut usage = 0;
        let h = gdal_sys::GDALWarp(empty.as_ptr(), std::ptr::null_mut(), 1, srcs.as_mut_ptr(), wo, &mut usage);
        gdal_sys::GDALWarpAppOptionsFree(wo);
        if h.is_null() {
            return Err(last_error("an item cannot be warped onto the series' grid"));
        }
        gdal::Dataset::from_c_dataset(h)
    };
    let xml = out.metadata_domain("xml:VRT").and_then(|v| v.into_iter().next()).ok_or("no warped VRT")?;
    let (a, b) = (xml.find("<SourceDataset").ok_or("no source")?, xml.find("</SourceDataset>").ok_or("no source")?);
    Ok(format!("{}{{SOURCE}}{}", &xml[..a], &xml[b + "</SourceDataset>".len()..]))
}

/// Whether two CRS definitions are the same CRS.
fn same_crs(a: &str, b: &str, cache: &mut HashMap<(String, String), bool>) -> bool {
    if a == b {
        return true;
    }
    *cache.entry((a.to_string(), b.to_string())).or_insert_with(|| match (srs(a), srs(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    })
}

/// Opens what STAC does not describe, `threads` at a time: one asset per tile and band (a tile keeps its grid),
/// or per band for its data type alone.
fn read_geoms(
    paths: Vec<(String, String)>,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> HashMap<String, Result<Geom, String>> {
    let mut out = HashMap::new();
    let n = paths.len();
    for chunk in paths.chunks(8) {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let got: Vec<(String, Result<Geom, String>)> = std::thread::scope(|s| {
            let hs: Vec<_> = chunk.iter().map(|(k, p)| (k.clone(), s.spawn(move || read_geom(p)))).collect();
            hs.into_iter().map(|(k, h)| (k, h.join().unwrap_or_else(|_| Err("panicked".into())))).collect()
        });
        out.extend(got);
        progress(0.5 * out.len() as f64 / n.max(1) as f64, &format!("reading the grids of {} files", n));
    }
    out
}

/// Writes the series: a VRT per date (oldest first) into `spec.dir`, warped items into its warp/ folder.
pub fn build(spec: &BuildSpec, progress: &dyn Fn(f64, &str), cancel: &AtomicBool) -> Result<Built, String> {
    if spec.items.is_empty() || spec.bands.is_empty() {
        return Err("no items or no bands".into());
    }
    std::fs::create_dir_all(&spec.dir).map_err(|e| format!("{}: {e}", spec.dir.display()))?;
    let path_of = |a: &Asset| asset_path(&a.href, spec.planetary);
    if spec.planetary {
        let mut signed = std::collections::HashSet::new();
        for it in &spec.items {
            for b in &spec.bands {
                if let Some((acc, cont, _)) = it.assets.get(&b.key).and_then(|a| azure_blob(&a.href))
                    && signed.insert((acc.clone(), cont.clone()))
                {
                    crate::stac::sign_planetary(&it.collection, &acc, &cont)?;
                }
            }
        }
    }
    // the grids: STAC's where it has them; what it lacks read from one file per tile and band
    let mut want: Vec<(String, String)> = Vec::new();
    let mut wanted = std::collections::HashSet::new();
    for it in &spec.items {
        for b in &spec.bands {
            let Some(a) = it.assets.get(&b.key) else { continue };
            let key = match stac_geom(it, a) {
                Some(_) if a.data_type.is_some() => continue,
                Some(_) => format!("type {}", b.key),
                None => format!("tile {} {}", it.tile, b.key),
            };
            if wanted.insert(key.clone()) {
                want.push((key, path_of(a)));
            }
        }
    }
    let read = read_geoms(want, progress, cancel);
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let geom = |it: &Item, key: &str| -> Result<Geom, String> {
        let a = &it.assets[key];
        match stac_geom(it, a) {
            Some((crs, gt, w, h)) => {
                let (dtype, nd) = match &a.data_type {
                    Some(t) => (t.clone(), None),
                    None => {
                        let g = read.get(&format!("type {key}")).ok_or("no data type")?.clone()?;
                        (g.dtype, g.nodata)
                    }
                };
                Ok(Geom { crs, gt, w, h, dtype, nodata: a.nodata.or(nd) })
            }
            None => {
                let mut g = read.get(&format!("tile {} {key}", it.tile)).ok_or("no grid")?.clone()?;
                g.nodata = a.nodata.or(g.nodata);
                Ok(g)
            }
        }
    };
    // the CRS of most items, the finest band in it, aligned to its pixels
    let mut count: HashMap<String, usize> = HashMap::new();
    let mut first: HashMap<String, [f64; 6]> = HashMap::new();
    let mut finest: HashMap<String, f64> = HashMap::new();
    let mut geoms: Vec<Vec<Option<Geom>>> = Vec::with_capacity(spec.items.len());
    let mut problems: Vec<String> = Vec::new();
    for it in &spec.items {
        let mut row = Vec::with_capacity(spec.bands.len());
        for b in &spec.bands {
            let g = if it.assets.contains_key(&b.key) {
                match geom(it, &b.key) {
                    Ok(g) => Some(g),
                    Err(e) => {
                        if problems.len() < 5 {
                            problems.push(format!("{} {}: {e}", it.id, b.key));
                        }
                        None
                    }
                }
            } else {
                None
            };
            if let Some(g) = &g {
                first.entry(g.crs.clone()).or_insert(g.gt);
                let f = finest.entry(g.crs.clone()).or_insert(f64::INFINITY);
                *f = f.min(g.gt[1].abs());
            }
            row.push(g);
        }
        if let Some(g) = row.iter().flatten().next() {
            *count.entry(g.crs.clone()).or_default() += 1;
        }
        geoms.push(row);
    }
    let crs = count.iter().max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0))).map(|(c, _)| c.clone()).ok_or_else(|| {
        format!("the items' grids could not be read{}", problems.first().map_or(String::new(), |p| format!(": {p}")))
    })?;
    let res = spec.res.filter(|r| *r > 0.0).unwrap_or(finest[&crs]);
    let grid = grid_over(bbox_in(spec.bbox, &crs)?, &crs, res, &first[&crs])?;
    // each date's VRT
    let groups = group_by_day(&spec.items);
    let warp_dir = spec.dir.join("warp");
    let mut templates: HashMap<String, String> = HashMap::new();
    let mut same: HashMap<(String, String), bool> = HashMap::new();
    let mut files = Vec::with_capacity(groups.len());
    let mut warped = 0usize;
    let mut sas: Vec<String> = Vec::new();
    for (gi, g) in groups.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let date = crate::dates::format_date(g.day as f64 * 86400.0, crate::TimeGranularity::Day);
        // the clearest item drawn last (on top)
        let mut order = g.items.clone();
        order.sort_by(|&a, &b| {
            let c = |i: usize| spec.items[i].cloud.unwrap_or(0.0);
            c(b).total_cmp(&c(a))
        });
        let mut x = format!(
            "<VRTDataset rasterXSize=\"{}\" rasterYSize=\"{}\">\n  <SRS>{}</SRS>\n  <GeoTransform>{}</GeoTransform>\n",
            grid.w,
            grid.h,
            xml_escape(&grid.crs),
            grid.gt.map(num).join(", ")
        );
        let ids: Vec<&str> = order.iter().map(|&i| spec.items[i].id.as_str()).collect();
        let _ = writeln!(
            x,
            "  <Metadata>\n    <MDI key=\"STAC_COLLECTION\">{}</MDI>\n    <MDI key=\"STAC_ITEMS\">{}</MDI>",
            xml_escape(&spec.items[g.items[0]].collection),
            xml_escape(&ids.join(" "))
        );
        if spec.planetary {
            let mut lines: Vec<String> = Vec::new();
            for &i in &order {
                let it = &spec.items[i];
                for b in &spec.bands {
                    if let Some((acc, cont, _)) = it.assets.get(&b.key).and_then(|a| azure_blob(&a.href)) {
                        let l = format!("{} {acc} {cont}", it.collection);
                        if !lines.contains(&l) {
                            lines.push(l);
                        }
                    }
                }
            }
            for l in &lines {
                if !sas.contains(l) {
                    sas.push(l.clone());
                }
            }
            let _ = writeln!(x, "    <MDI key=\"{SAS_METADATA}\">{}</MDI>", xml_escape(&lines.join("\n")));
        }
        x += "  </Metadata>\n";
        for (bi, b) in spec.bands.iter().enumerate() {
            let have: Vec<(usize, &Geom)> =
                order.iter().filter_map(|&i| geoms[i][bi].as_ref().map(|g| (i, g))).collect();
            let Some(&(i0, g0)) = have.first() else {
                return Err(format!("{date}: no item has the band {}", b.label));
            };
            let a0 = &spec.items[i0].assets[&b.key];
            let _ = writeln!(x, "  <VRTRasterBand dataType=\"{}\" band=\"{}\">", g0.dtype, bi + 1);
            let _ = writeln!(x, "    <Description>{}</Description>", xml_escape(&b.label));
            if let Some(nm) = b.wavelength_nm {
                let _ = writeln!(
                    x,
                    "    <Metadata>\n      <MDI key=\"wavelength\">{}</MDI>\n      \
                     <MDI key=\"wavelength_units\">nm</MDI>\n    </Metadata>",
                    num(nm)
                );
            }
            if let Some(nd) = g0.nodata.filter(|v| !v.is_nan()) {
                let _ = writeln!(x, "    <NoDataValue>{}</NoDataValue>", num(nd));
            }
            if let Some(s) = a0.scale {
                let _ = writeln!(
                    x,
                    "    <Offset>{}</Offset>\n    <Scale>{}</Scale>",
                    num(a0.offset.unwrap_or(0.0)),
                    num(s)
                );
            }
            for (i, gm) in have {
                let it = &spec.items[i];
                let path = path_of(&it.assets[&b.key]);
                // coarser cells average the reflectances; codes (quality) are never averaged
                let coarser = grid.gt[1] > 1.5 * gm.gt[1].abs();
                let resampling = if coarser && b.quality == QaRule::None { "average" } else { "nearest" };
                let nodata = gm.nodata.filter(|v| !v.is_nan()).map(|v| format!("<NODATA>{}</NODATA>", num(v)));
                if same_crs(&gm.crs, &grid.crs, &mut same) {
                    let Some((s, d)) = source_window(gm, &grid) else { continue };
                    let _ = writeln!(
                        x,
                        "    <ComplexSource resampling=\"{resampling}\">\n      \
                         <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\n      <SourceBand>1</SourceBand>\n      \
                         <SrcRect xOff=\"{}\" yOff=\"{}\" xSize=\"{}\" ySize=\"{}\"/>\n      \
                         <DstRect xOff=\"{}\" yOff=\"{}\" xSize=\"{}\" ySize=\"{}\"/>\n      {}\n    </ComplexSource>",
                        xml_escape(&path),
                        num(s[0]),
                        num(s[1]),
                        num(s[2]),
                        num(s[3]),
                        num(d[0]),
                        num(d[1]),
                        num(d[2]),
                        num(d[3]),
                        nodata.unwrap_or_default()
                    );
                } else {
                    let r = if resampling == "average" { "average" } else { "near" };
                    let tkey = format!("{:?} {r}", gm);
                    if !templates.contains_key(&tkey) {
                        templates.insert(tkey.clone(), warp_template(gm, &grid, r)?);
                    }
                    std::fs::create_dir_all(&warp_dir).map_err(|e| format!("{}: {e}", warp_dir.display()))?;
                    let name = format!("{date}_{}_{}.vrt", sanitize(&it.id), sanitize(&b.key));
                    let src = format!("<SourceDataset relativeToVRT=\"0\">{}</SourceDataset>", xml_escape(&path));
                    std::fs::write(warp_dir.join(&name), templates[&tkey].replace("{SOURCE}", &src))
                        .map_err(|e| format!("{name}: {e}"))?;
                    warped += 1;
                    let _ = writeln!(
                        x,
                        "    <ComplexSource>\n      <SourceFilename relativeToVRT=\"1\">warp/{name}</SourceFilename>\n      \
                         <SourceBand>1</SourceBand>\n      <SrcRect xOff=\"0\" yOff=\"0\" xSize=\"{w}\" ySize=\"{h}\"/>\n      \
                         <DstRect xOff=\"0\" yOff=\"0\" xSize=\"{w}\" ySize=\"{h}\"/>\n      {}\n    </ComplexSource>",
                        nodata.unwrap_or_default(),
                        w = grid.w,
                        h = grid.h
                    );
                }
            }
            x += "  </VRTRasterBand>\n";
        }
        x += "</VRTDataset>\n";
        let f = spec.dir.join(format!("{date}.vrt"));
        std::fs::write(&f, x).map_err(|e| format!("{}: {e}", f.display()))?;
        files.push(f.to_string_lossy().into_owned());
        progress(0.5 + 0.5 * (gi + 1) as f64 / groups.len() as f64, &format!("{} of {} dates", gi + 1, groups.len()));
    }
    let _ = std::fs::write(spec.dir.join("footprints.geojson"), crate::stac::footprints_geojson(&spec.items));
    Ok(Built { files, grid, warped })
}

/// A name safe in a file name.
pub fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' }).collect()
}

struct Progress<'a> {
    f: &'a dyn Fn(f64),
    cancel: &'a AtomicBool,
}

unsafe extern "C" fn on_progress(done: f64, _msg: *const c_char, data: *mut c_void) -> c_int {
    // SAFETY: `data` is the Progress given with the callback, alive during the call that uses it.
    let p = unsafe { &*(data as *const Progress) };
    (p.f)(done);
    c_int::from(!p.cancel.load(Ordering::Relaxed))
}

/// The series' VRTs written as cloud-optimized GeoTIFFs (DEFLATE, overviews) into `out`, under a temporary name
/// renamed when complete: everything read once (the download). The files, in order.
pub fn download(
    files: &[String],
    out: &Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut written = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let stem = Path::new(f).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let dst = out.join(format!("{stem}.tif"));
        let part = out.join(format!("{stem}.tif.part"));
        let src = crate::open_raster(f).map_err(|e| format!("{f}: {e}"))?;
        let mut a: Vec<String> = [
            "-of",
            "COG",
            "-co",
            "COMPRESS=DEFLATE",
            "-co",
            "PREDICTOR=2",
            "-co",
            "BIGTIFF=IF_SAFER",
            "-co",
            "NUM_THREADS=ALL_CPUS",
            "-co",
            "OVERVIEW_RESAMPLING=NEAREST",
        ]
        .map(String::from)
        .to_vec();
        // a GeoTIFF has one data type: the one that holds every band's (reflectances and a byte of SCL codes)
        let types: Vec<gdal_sys::GDALDataType::Type> =
            (1..=src.raster_count()).filter_map(|b| src.rasterband(b).ok()).map(|b| b.band_type() as _).collect();
        if types.windows(2).any(|w| w[0] != w[1]) {
            // SAFETY: GDAL's pure functions on data type values.
            let name = unsafe {
                let t = types.iter().fold(types[0], |u, &t| gdal_sys::GDALDataTypeUnion(u, t));
                std::ffi::CStr::from_ptr(gdal_sys::GDALGetDataTypeName(t)).to_string_lossy().into_owned()
            };
            a.extend(["-ot".into(), name]);
        }
        let args = Args::new(&a);
        let n = files.len() as f64;
        let msg = format!("{} of {} dates: {stem}", i + 1, files.len());
        let report = |d: f64| progress((i as f64 + d) / n, &msg);
        let p = Progress { f: &report, cancel };
        let c_dst = CString::new(part.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;
        // SAFETY: GDAL's C utility on a live handle; the progress data outlives the call.
        let ok = unsafe {
            gdal_sys::CPLErrorReset();
            let to = gdal_sys::GDALTranslateOptionsNew(args.p.as_ptr() as *mut *mut c_char, std::ptr::null_mut());
            gdal_sys::GDALTranslateOptionsSetProgress(to, Some(on_progress), &p as *const Progress as *mut c_void);
            let h = gdal_sys::GDALTranslate(c_dst.as_ptr(), src.c_dataset(), to, std::ptr::null_mut());
            gdal_sys::GDALTranslateOptionsFree(to);
            if !h.is_null() {
                gdal_sys::GDALClose(h);
            }
            !h.is_null()
        };
        if !ok || cancel.load(Ordering::Relaxed) {
            let _ = std::fs::remove_file(&part);
            return Err(if cancel.load(Ordering::Relaxed) { "cancelled".into() } else { last_error(&stem) });
        }
        std::fs::rename(&part, &dst).map_err(|e| format!("{}: {e}", dst.display()))?;
        written.push(dst.to_string_lossy().into_owned());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_and_windows() {
        // a 20 m band of an MGRS tile, the grid at 10 m over part of it
        let like = [399960.0, 10.0, 0.0, 8900020.0, 0.0, -10.0];
        let g = grid_over([420003.0, 8870001.0, 430007.0, 8880009.0], "EPSG:32720", 10.0, &like).unwrap();
        assert_eq!(g.gt, [420000.0, 10.0, 0.0, 8880010.0, 0.0, -10.0]);
        assert_eq!((g.w, g.h), (1001, 1001));
        let b20 = Geom {
            crs: "EPSG:32720".into(),
            gt: [399960.0, 20.0, 0.0, 8900020.0, 0.0, -20.0],
            w: 5490,
            h: 5490,
            dtype: "UInt16".into(),
            nodata: Some(0.0),
        };
        let (s, d) = source_window(&b20, &g).unwrap();
        assert_eq!(s, [1002.0, 1000.5, 500.5, 500.5]);
        assert_eq!(d, [0.0, 0.0, 1001.0, 1001.0]);
        // a tile reaching only the grid's right half
        let right = Geom { gt: [425000.0, 10.0, 0.0, 8900020.0, 0.0, -10.0], w: 100, ..b20.clone() };
        let (s, d) = source_window(&right, &g).unwrap();
        assert_eq!((s[0], s[2]), (0.0, 100.0));
        assert_eq!((d[0], d[2]), (500.0, 100.0));
        let away = Geom { gt: [0.0, 10.0, 0.0, 0.0, 0.0, -10.0], ..b20 };
        assert!(source_window(&away, &g).is_none());
        assert!(grid_over([0.0, 0.0, 1e7, 1e7], "EPSG:32720", 10.0, &like).is_err());
    }
}
