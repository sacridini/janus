//! Esri World Imagery Wayback: the releases with distinct imagery at a point and their acquisition dates. Ported
//! from vv/highres.py of Validation Viewer by Dr. Matthias Baumann (Humboldt-Universität zu Berlin),
//! https://scm.cms.hu-berlin.de/baumamat/validationviewer, GPL-2.0-or-later, used under GPL-3.0-or-later.
//!
//! Wayback is talked to directly over HTTP (GDAL's libcurl, as the rest of the program: `janus_gdal::http_fetch`):
//! the tilemap endpoint says, for a tile, which release last changed it, so walking back from the newest release
//! visits only releases with distinct imagery (10 to 20 small requests). Each distinct release's acquisition date
//! comes from its metadata layer. A release is drawn as the mosaic of its 256-pixel tiles (JPEG, decoded by GDAL,
//! kept as files under the cache folder) warped, cubic, onto the grid of a square chip in the cube's CRS. At most
//! [`CONNECTIONS`] requests run at once. Nothing here touches the network until a function of it is called.

use std::ffi::CString;
use std::os::raw::c_char;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use gdal::raster::Buffer;
use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::{Dataset, DriverManager};
use serde_json::Value;

use crate::basemap::curl_ca_bundle;
use crate::reproject::Quiet;

/// What Esri asks to be shown with its imagery.
pub const ATTRIBUTION: &str = "Esri, Maxar, Earthstar Geographics, and the GIS User Community";

pub const CONFIG_URL: &str = "https://s3-us-west-2.amazonaws.com/config.maptiles.arcgis.com/waybackconfig.json";
const TILEMAP_URL: &str =
    "https://wayback.maptiles.arcgis.com/arcgis/rest/services/World_Imagery/MapServer/tilemap/{rel}/{z}/{row}/{col}";
/// the finest zoom level Wayback has
pub const MAX_Z: u32 = 19;
/// a chip's edge in pixels, at most
pub const MAX_PX: u32 = 1024;
/// concurrent requests, at most
pub const CONNECTIONS: usize = 4;
/// the Web Mercator sphere's radius (m)
const R_MERC: f64 = 6378137.0;
/// half the width of the Web Mercator world (m)
const E: f64 = 20037508.342789244;
/// a chip covering more tiles than this is refused
const MAX_TILES: usize = 144;
/// how long the list of releases is kept
const CONFIG_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// A Wayback release: a dated copy of World Imagery.
#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub num: u32,
    /// "YYYY-MM-DD" of the release (not of the imagery)
    pub date: String,
    /// tile URL template with {level}, {row} and {col}
    pub tile_url: String,
    /// the metadata MapServer's URL (its layers hold the acquisition footprints)
    pub meta_url: String,
}

/// An image of a point: the acquisition `date` ("YYYY-MM-DD"), seen in release `release`.
#[derive(Clone, Debug, PartialEq)]
pub struct WaybackImage {
    pub date: String,
    pub release: u32,
    pub release_date: String,
    /// source resolution (m), if the metadata has it
    pub res_m: Option<f64>,
    pub desc: String,
    pub zoom: u32,
}

/// A chip as RGB bytes, rows from the top, 3 bytes a pixel.
#[derive(Clone, Debug, PartialEq)]
pub struct RgbImage {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

impl RgbImage {
    /// The image as a PNG file (through GDAL).
    pub fn write_png(&self, path: &Path) -> Result<(), String> {
        crate::vectors::register();
        let mem = DriverManager::get_driver_by_name("MEM").map_err(|e| e.to_string())?;
        let png = DriverManager::get_driver_by_name("PNG").map_err(|e| e.to_string())?;
        let (w, h) = (self.w as usize, self.h as usize);
        let ds = mem.create_with_band_type::<u8, _>("", w, h, 3).map_err(|e| e.to_string())?;
        for b in 0..3 {
            let plane: Vec<u8> = self.data.iter().skip(b).step_by(3).copied().collect();
            let mut buf = Buffer::new((w, h), plane);
            ds.rasterband(b + 1)
                .and_then(|mut band| band.write((0, 0), (w, h), &mut buf))
                .map_err(|e| e.to_string())?;
        }
        ds.create_copy(&png, path, &Default::default()).map(|_| ()).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------------------
// geometry
// ---------------------------------------------------------------------------------------------------------

/// The lowest zoom whose ground resolution gives about MAX_PX pixels over the chip (12 to `zmax`).
pub fn zoom_for(size_m: f64, lat: f64, zmax: u32) -> u32 {
    let need = 2.0 * std::f64::consts::PI * R_MERC * lat.to_radians().cos() / 256.0 * f64::from(MAX_PX) / size_m;
    let z = need.log2().ceil();
    z.clamp(12.0, f64::from(zmax.max(12))) as u32
}

/// The chip's edge in pixels at zoom `z`: its ground size over the ground size of a pixel (128 to MAX_PX).
pub fn out_pixels(size_m: f64, lat: f64, z: u32) -> u32 {
    let ground = 2.0 * std::f64::consts::PI * R_MERC * lat.to_radians().cos() / 256.0 / 2f64.powi(z as i32);
    (size_m / ground).round().clamp(128.0, f64::from(MAX_PX)) as u32
}

/// Tile coordinates (fractional) of a point at zoom `z`.
pub fn tile_xy(lon: f64, lat: f64, z: u32) -> (f64, f64) {
    let n = 2f64.powi(z as i32);
    let x = (lon + 180.0) / 360.0 * n;
    let y = (1.0 - lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n;
    (x, y)
}

/// Web Mercator bounds (x0, y0, x1, y1) of the block of tiles [tx0..=tx1] x [ty0..=ty1] at zoom `z`.
pub fn merc_bounds(tx0: i64, ty0: i64, tx1: i64, ty1: i64, z: u32) -> (f64, f64, f64, f64) {
    let span = 2.0 * E;
    let t = span / 2f64.powi(z as i32);
    (-E + tx0 as f64 * t, E - (ty1 + 1) as f64 * t, -E + (tx1 + 1) as f64 * t, E - ty0 as f64 * t)
}

// ---------------------------------------------------------------------------------------------------------
// the releases and the chain of tilemaps (pure parts)
// ---------------------------------------------------------------------------------------------------------

/// "YYYY-MM-DD" found in a text (the first date), if any.
fn find_date(s: &str) -> Option<String> {
    let b = s.as_bytes();
    (0..b.len().saturating_sub(9)).find_map(|i| {
        let w = &b[i..i + 10];
        let ok = w.iter().enumerate().all(|(k, c)| if k == 4 || k == 7 { *c == b'-' } else { c.is_ascii_digit() });
        ok.then(|| s[i..i + 10].to_string())
    })
}

/// The releases of waybackconfig.json, newest first.
pub fn parse_config(json: &str) -> Result<Vec<Release>, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| format!("Wayback's configuration: {e}"))?;
    let obj = v.as_object().ok_or("Wayback's configuration is not an object")?;
    let mut rel = Vec::new();
    for (k, r) in obj {
        let (Ok(num), Some(url), Some(meta)) =
            (k.parse::<u32>(), r["itemURL"].as_str(), r["metadataLayerUrl"].as_str())
        else {
            continue;
        };
        let date = r["itemTitle"].as_str().and_then(find_date).unwrap_or_else(|| "0000".into());
        rel.push(Release { num, date, tile_url: url.into(), meta_url: meta.into() });
    }
    if rel.is_empty() {
        return Err("Wayback's configuration lists no release".into());
    }
    rel.sort_by(|a, b| b.date.cmp(&a.date).then(b.num.cmp(&a.num)));
    Ok(rel)
}

/// The releases with distinct imagery at a tile: from the newest release, a tilemap answer says (`select`) which
/// release last changed the tile; the walk goes on from the release after that one until there is no data.
/// `tilemap(num)` is the answer's text of release `num` (None: the request failed, which ends the walk).
pub fn select_chain(rel: &[Release], mut tilemap: impl FnMut(u32) -> Option<String>) -> Vec<Release> {
    let index = |num: u32| rel.iter().position(|r| r.num == num);
    let (mut found, mut i) = (Vec::new(), 0);
    while i < rel.len() {
        let Some(text) = tilemap(rel[i].num) else { break };
        let Ok(j) = serde_json::from_str::<Value>(&text) else { break };
        if j["data"].as_array().and_then(|d| d.first()).and_then(Value::as_i64) != Some(1) {
            break;
        }
        let num = j["select"].as_array().and_then(|s| s.first()).map_or(Some(rel[i].num), |s| {
            s.as_u64().or_else(|| s.as_str().and_then(|t| t.parse().ok())).map(|n| n as u32)
        });
        // a release before the one asked would loop for ever
        let Some(k) = num.and_then(index).filter(|&k| k >= i) else { break };
        found.push(rel[k].clone());
        i = k + 1;
    }
    found
}

/// The metadata layers to ask, the one closest to 23 - z first (layer 0 is the finest scale).
pub fn meta_layers(z: u32) -> Vec<i32> {
    let want = 23 - z as i32;
    let mut l: Vec<i32> = (0..8).collect();
    l.sort_by_key(|&k| (k - want).abs()); // stable
    l
}

/// Days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "YYYY-MM-DD" (UTC) of a time in milliseconds since the epoch.
fn date_of_ms(ms: i64) -> String {
    let (y, m, d) = civil_from_days(ms.div_euclid(86_400_000));
    format!("{y:04}-{m:02}-{d:02}")
}

/// What a metadata layer's answer says about the point: (acquisition date, resolution, description), None if the
/// layer has no feature there.
pub fn parse_meta(json: &str) -> Option<(String, Option<f64>, String)> {
    let v: Value = serde_json::from_str(json).ok()?;
    let a = v["features"].as_array()?.first()?["attributes"].as_object()?;
    let ms = a.get("SRC_DATE2")?.as_f64()?;
    let text = |k: &str| match a.get(k) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => String::new(),
        Some(o) => o.to_string(),
    };
    let desc = format!("{} {}", text("NICE_NAME"), text("SRC_DESC")).trim().to_string();
    Some((date_of_ms(ms as i64), a.get("SRC_RES").and_then(Value::as_f64), desc))
}

/// The image of a release at the point: the first layer (closest to 23 - z first) with a feature gives the
/// acquisition date; `query(layer)` is that layer's answer (None: the request failed, next layer). Without any,
/// the release's own date.
pub fn describe(r: &Release, z: u32, mut query: impl FnMut(i32) -> Option<String>) -> WaybackImage {
    for l in meta_layers(z) {
        if let Some((date, res_m, desc)) = query(l).as_deref().and_then(parse_meta) {
            return WaybackImage { date, release: r.num, release_date: r.date.clone(), res_m, desc, zoom: z };
        }
    }
    WaybackImage {
        date: r.date.clone(),
        release: r.num,
        release_date: r.date.clone(),
        res_m: None,
        desc: "release date (no metadata)".into(),
        zoom: z,
    }
}

/// One image per acquisition date, the newest release's (`items` newest release first), by date.
pub fn dedup_by_date(items: Vec<WaybackImage>) -> Vec<WaybackImage> {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<WaybackImage> = items.into_iter().filter(|i| seen.insert(i.date.clone())).collect();
    out.sort_by(|a, b| a.date.cmp(&b.date));
    out
}

/// One image per year (`images` by date): the latest inside the `months` (1 to 12), else the latest of the year;
/// (year, index in `images`, how many others the year has).
pub fn pick_per_year(images: &[WaybackImage], months: &[u32]) -> Vec<(i32, usize, usize)> {
    let mut years: Vec<(i32, Vec<usize>)> = Vec::new();
    for (i, im) in images.iter().enumerate() {
        let Some(y) = im.date.get(0..4).and_then(|s| s.parse::<i32>().ok()) else { continue };
        match years.iter_mut().find(|(yy, _)| *yy == y) {
            Some((_, v)) => v.push(i),
            None => years.push((y, vec![i])),
        }
    }
    years.sort_by_key(|(y, _)| *y);
    years
        .into_iter()
        .map(|(y, v)| {
            let month = |i: usize| images[i].date.get(5..7).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
            let chosen = v.iter().rev().find(|&&i| months.contains(&month(i))).or(v.last()).copied().unwrap();
            (y, chosen, v.len() - 1)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------
// the network
// ---------------------------------------------------------------------------------------------------------

fn http_get(url: &str) -> Result<Vec<u8>, String> {
    // per thread: certificates for curl, a dead connection fails fast
    let ca = curl_ca_bundle();
    if !ca.is_empty() {
        let _ = gdal::config::set_thread_local_config_option("CURL_CA_BUNDLE", &ca);
    }
    janus_gdal::http_fetch(
        url,
        None,
        &["TIMEOUT=30", "CONNECTTIMEOUT=10", "MAX_RETRY=3", "RETRY_DELAY=1", "USERAGENT=Janus"],
    )
}

/// `f` over `items` on at most CONNECTIONS threads, results in order.
fn par_map<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let out: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..CONNECTIONS.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(it) = items.get(i) else { break };
                    let r = f(it);
                    out.lock().unwrap().push((i, r));
                }
            });
        }
    });
    let mut v = out.into_inner().unwrap();
    v.sort_by_key(|(i, _)| *i);
    v.into_iter().map(|(_, r)| r).collect()
}

/// The releases, newest first. The list is kept in `cache_dir` for 24 hours.
pub fn releases(cache_dir: &Path) -> Result<Vec<Release>, String> {
    let path = cache_dir.join("_wayback_config.json");
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .is_ok_and(|t| SystemTime::now().duration_since(t).is_ok_and(|age| age < CONFIG_MAX_AGE));
    if fresh
        && let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(r) = parse_config(&text)
    {
        return Ok(r);
    }
    let body = http_get(CONFIG_URL).map_err(|e| format!("Wayback's list of releases: {e}"))?;
    let text = String::from_utf8_lossy(&body).into_owned();
    let r = parse_config(&text)?;
    let _ = std::fs::create_dir_all(cache_dir);
    let _ = std::fs::write(&path, text);
    Ok(r)
}

/// The images of a point at zoom `z` (the zoom of the tilemap to follow: `zoom_for`), by acquisition date, one
/// per date (the newest release that has it).
pub fn list(lon: f64, lat: f64, z: u32, cache_dir: &Path) -> Result<Vec<WaybackImage>, String> {
    let rel = releases(cache_dir)?;
    let (x, y) = tile_xy(lon, lat, z);
    let url = |num: u32| {
        TILEMAP_URL
            .replace("{rel}", &num.to_string())
            .replace("{z}", &z.to_string())
            .replace("{row}", &(y as i64).to_string())
            .replace("{col}", &(x as i64).to_string())
    };
    let found = select_chain(&rel, |num| http_get(&url(num)).ok().map(|b| String::from_utf8_lossy(&b).into_owned()));
    let items = par_map(&found, |r| {
        describe(r, z, |layer| {
            let q = format!(
                "{}/{layer}/query?geometry={lon},{lat}&geometryType=esriGeometryPoint&inSR=4326\
                 &spatialRel=esriSpatialRelIntersects&outFields=SRC_DATE2,SRC_RES,SRC_DESC,NICE_NAME\
                 &returnGeometry=false&f=json",
                r.meta_url
            );
            http_get(&q).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
        })
    });
    Ok(dedup_by_date(items))
}

// ---------------------------------------------------------------------------------------------------------
// tiles and the chip
// ---------------------------------------------------------------------------------------------------------

fn tile_path(cache_dir: &Path, rel: u32, z: u32, x: i64, y: i64) -> PathBuf {
    cache_dir.join("wayback_tiles").join(rel.to_string()).join(z.to_string()).join(format!("{x}_{y}.jpg"))
}

fn last_error(what: &str) -> String {
    // SAFETY: GDAL returns a string it owns, valid until its next error on this thread; copied at once.
    let msg = unsafe {
        let p = gdal_sys::CPLGetLastErrorMsg();
        if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
    };
    if msg.is_empty() { what.to_string() } else { format!("{what}: {msg}") }
}

/// A tile's pixels (256 x 256 x 3, three planes) from its encoded bytes.
fn decode_tile(bytes: Vec<u8>) -> Option<Vec<u8>> {
    static N: AtomicUsize = AtomicUsize::new(0);
    let name = format!("/vsimem/janus-wayback-{}.img", N.fetch_add(1, Ordering::Relaxed));
    gdal::vsi::create_mem_file(&name, bytes).ok()?;
    let r = (|| {
        let _q = Quiet::new();
        let ds = Dataset::open(&name).ok()?;
        let (w, h) = ds.raster_size();
        if (w, h) != (256, 256) || ds.raster_count() == 0 {
            return None;
        }
        let nb = ds.raster_count();
        let mut out = vec![0u8; 3 * 256 * 256];
        for b in 0..3 {
            let band = ds.rasterband(b.min(nb - 1) + 1).ok()?;
            band.read_into_slice::<u8>((0, 0), (256, 256), (256, 256), &mut out[b * 65536..(b + 1) * 65536], None)
                .ok()?;
        }
        Some(out)
    })();
    let _ = gdal::vsi::unlink_mem_file(&name);
    r
}

/// A tile of release `r`, from the cache folder or the server (and kept); None where Wayback has none.
fn get_tile(r: &Release, cache_dir: &Path, z: u32, x: i64, y: i64) -> Option<Vec<u8>> {
    let path = tile_path(cache_dir, r.num, z, x, y);
    if let Ok(b) = std::fs::read(&path)
        && let Some(t) = decode_tile(b)
    {
        return Some(t);
    }
    let url =
        r.tile_url.replace("{level}", &z.to_string()).replace("{row}", &y.to_string()).replace("{col}", &x.to_string());
    let body = http_get(&url).ok().filter(|b| !b.is_empty())?;
    let t = decode_tile(body.clone())?;
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let part = path.with_extension("part");
    if std::fs::write(&part, &body).is_ok() {
        let _ = std::fs::rename(&part, &path);
    }
    Some(t)
}

fn srs_of(def: &str) -> Result<SpatialRef, String> {
    let mut s = SpatialRef::from_definition(def).map_err(|e| format!("the CRS is not understood ({e})"))?;
    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    Ok(s)
}

/// The chip of release `r` at zoom `z`: a square of side `size_m` centred on `center` (x, y) in `crs` (WKT
/// or "EPSG:n"), `out_px` pixels a side, from the tiles covering it warped (cubic) onto its grid. Pixels without
/// a tile are mid grey. Tiles are kept under `cache_dir`.
pub fn render(
    r: &Release,
    z: u32,
    crs: &str,
    center: (f64, f64),
    size_m: f64,
    out_px: u32,
    cache_dir: &Path,
) -> Result<RgbImage, String> {
    crate::vectors::register();
    let (cx, cy) = center;
    if size_m.is_nan() || size_m <= 0.0 || out_px == 0 || z > MAX_Z + 3 {
        return Err("the chip's size or the zoom is not valid".into());
    }
    let dst = srs_of(crs)?;
    let merc = srs_of("EPSG:3857")?;
    let (x0, y0, x1, y1) = (cx - size_m / 2.0, cy - size_m / 2.0, cx + size_m / 2.0, cy + size_m / 2.0);
    // the chip's edge in Web Mercator, to know which tiles cover it
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for i in 0..=8 {
        let f = f64::from(i) / 8.0;
        for (px, py) in [(x0 + f * size_m, y0), (x0 + f * size_m, y1), (x0, y0 + f * size_m), (x1, y0 + f * size_m)] {
            xs.push(px);
            ys.push(py);
        }
    }
    let mut zs = vec![0.0; xs.len()];
    {
        let _q = Quiet::new();
        CoordTransform::new(&dst, &merc)
            .and_then(|ct| ct.transform_coords(&mut xs, &mut ys, &mut zs))
            .map_err(|e| format!("the chip cannot be placed on Web Mercator: {e}"))?;
    }
    if xs.iter().chain(&ys).any(|v| !v.is_finite()) {
        return Err("the chip is outside Web Mercator".into());
    }
    let s = 2.0 * E / 2f64.powi(z as i32);
    let n = (1i64 << z) - 1;
    let range = |lo: f64, hi: f64, flip: bool| {
        let (a, b) = if flip { ((E - hi) / s, (E - lo) / s) } else { ((lo + E) / s, (hi + E) / s) };
        ((a.floor() as i64).clamp(0, n), ((b - 1e-9).floor() as i64).clamp(0, n))
    };
    let fold = |v: &[f64]| v.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &t| (a.0.min(t), a.1.max(t)));
    let ((mx0, mx1), (my0, my1)) = (fold(&xs), fold(&ys));
    let (tx0, tx1) = range(mx0, mx1, false);
    let (ty0, ty1) = range(my0, my1, true);
    let (nx, ny) = ((tx1 - tx0 + 1) as usize, (ty1 - ty0 + 1) as usize);
    if nx * ny > MAX_TILES {
        return Err(format!("the chip needs {} tiles at zoom {z}; the most is {MAX_TILES}", nx * ny));
    }

    let wanted: Vec<(i64, i64)> = (ty0..=ty1).flat_map(|ty| (tx0..=tx1).map(move |tx| (tx, ty))).collect();
    let tiles = par_map(&wanted, |&(tx, ty)| get_tile(r, cache_dir, z, tx, ty));
    if tiles.iter().all(Option::is_none) {
        return Err(format!("Wayback has no tile for release {} at this place", r.num));
    }
    let (mw, mh) = (nx * 256, ny * 256);
    let mem = DriverManager::get_driver_by_name("MEM").map_err(|e| e.to_string())?;
    let mut src = mem.create_with_band_type::<u8, _>("", mw, mh, 3).map_err(|e| e.to_string())?;
    let mut planes = vec![vec![128u8; mw * mh]; 3];
    for (&(tx, ty), t) in wanted.iter().zip(&tiles) {
        let Some(t) = t else { continue };
        let (ox, oy) = ((tx - tx0) as usize * 256, (ty - ty0) as usize * 256);
        for (b, plane) in planes.iter_mut().enumerate() {
            for row in 0..256 {
                let d = (oy + row) * mw + ox;
                plane[d..d + 256].copy_from_slice(&t[b * 65536 + row * 256..b * 65536 + (row + 1) * 256]);
            }
        }
    }
    for (b, plane) in planes.into_iter().enumerate() {
        let mut buf = Buffer::new((mw, mh), plane);
        src.rasterband(b + 1).and_then(|mut band| band.write((0, 0), (mw, mh), &mut buf)).map_err(|e| e.to_string())?;
    }
    let (bx0, _, _, by1) = merc_bounds(tx0, ty0, tx1, ty1, z);
    src.set_geo_transform(&[bx0, s / 256.0, 0.0, by1, 0.0, -s / 256.0]).map_err(|e| e.to_string())?;
    src.set_spatial_ref(&merc).map_err(|e| e.to_string())?;

    // GDAL's warp, cubic, onto the chip's grid
    let args: Vec<CString> = [
        "-of".to_string(),
        "MEM".into(),
        "-r".into(),
        "cubic".into(),
        "-t_srs".into(),
        crs.into(),
        "-te".into(),
        format!("{x0:.9}"),
        format!("{y0:.9}"),
        format!("{x1:.9}"),
        format!("{y1:.9}"),
        "-ts".into(),
        out_px.to_string(),
        out_px.to_string(),
    ]
    .iter()
    .map(|a| CString::new(a.as_str()).unwrap_or_default())
    .collect();
    let mut p: Vec<*mut c_char> = args.iter().map(|c| c.as_ptr() as *mut c_char).collect();
    p.push(std::ptr::null_mut());
    let empty = CString::new("").unwrap();
    // SAFETY: GDAL's warp utility on a live dataset; the result is owned by the Dataset made of it.
    let warped = unsafe {
        gdal_sys::CPLErrorReset();
        let wo = gdal_sys::GDALWarpAppOptionsNew(p.as_mut_ptr(), std::ptr::null_mut());
        let mut srcs = [src.c_dataset()];
        let mut usage = 0;
        let h = gdal_sys::GDALWarp(empty.as_ptr(), std::ptr::null_mut(), 1, srcs.as_mut_ptr(), wo, &mut usage);
        gdal_sys::GDALWarpAppOptionsFree(wo);
        if h.is_null() {
            return Err(last_error("the tiles could not be warped onto the chip"));
        }
        Dataset::from_c_dataset(h)
    };
    let px = out_px as usize;
    let mut data = vec![0u8; px * px * 3];
    let mut plane = vec![0u8; px * px];
    for b in 0..3 {
        let band = warped.rasterband(b + 1).map_err(|e| e.to_string())?;
        band.read_into_slice::<u8>((0, 0), (px, px), (px, px), &mut plane, None).map_err(|e| e.to_string())?;
        for (i, v) in plane.iter().enumerate() {
            data[i * 3 + b] = *v;
        }
    }
    Ok(RgbImage { w: out_px, h: out_px, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// waybackconfig.json as the server has it (3 releases; the third's title has no date).
    const CONFIG: &str = r#"{
      "13161": {"itemID": "a", "itemTitle": "World Imagery (Wayback 2023-06-29)",
        "itemURL": "https://wayback.maptiles.arcgis.com/arcgis/rest/services/World_Imagery/WMTS/1.0.0/default028mm/MapServer/tile/13161/{level}/{row}/{col}",
        "metadataLayerUrl": "https://metadata.maptiles.arcgis.com/arcgis/rest/services/World_Imagery_Metadata_2023_r06/MapServer",
        "layerIdentifier": "WB_2023_R06"},
      "10": {"itemID": "b", "itemTitle": "World Imagery (Wayback 2014-02-20)",
        "itemURL": "https://wayback.maptiles.arcgis.com/arcgis/rest/services/World_Imagery/WMTS/1.0.0/default028mm/MapServer/tile/10/{level}/{row}/{col}",
        "metadataLayerUrl": "https://metadata.maptiles.arcgis.com/arcgis/rest/services/World_Imagery_Metadata_2014_r01/MapServer"},
      "5": {"itemID": "c", "itemTitle": "World Imagery (Wayback)", "itemURL": "u5/{level}/{row}/{col}", "metadataLayerUrl": "m5"}
    }"#;

    fn img(date: &str, release: u32) -> WaybackImage {
        WaybackImage {
            date: date.into(),
            release,
            release_date: String::new(),
            res_m: None,
            desc: String::new(),
            zoom: 19,
        }
    }

    #[test]
    fn config_newest_first() {
        let r = parse_config(CONFIG).unwrap();
        assert_eq!(r.iter().map(|r| r.num).collect::<Vec<_>>(), [13161, 10, 5]);
        assert_eq!(r[0].date, "2023-06-29");
        assert_eq!(r[1].date, "2014-02-20");
        assert_eq!(r[2].date, "0000");
        assert!(r[1].tile_url.ends_with("/10/{level}/{row}/{col}"));
        assert!(r[0].meta_url.ends_with("Metadata_2023_r06/MapServer"));
        assert!(parse_config("{}").is_err());
        assert!(parse_config("[1]").is_err());
    }

    #[test]
    fn chain_follows_select_and_stops() {
        let rel = parse_config(CONFIG).unwrap();
        // newest release: the tile last changed in release 10, whose own tilemap says it changed in 10 as well;
        // then release 5 has no data
        let mut asked = Vec::new();
        let found = select_chain(&rel, |num| {
            asked.push(num);
            Some(match num {
                13161 => r#"{"data":[1],"select":["10"],"valid":true}"#.into(),
                10 => r#"{"data":[1],"select":[10]}"#.into(),
                _ => r#"{"data":[0]}"#.into(),
            })
        });
        assert_eq!(found.iter().map(|r| r.num).collect::<Vec<_>>(), [10]);
        // 13161 -> select 10 (index 1) -> next asked is release 5 (index 2), not 10 again
        assert_eq!(asked, [13161, 5]);

        // no select: the release itself; a failed request, an unknown release and a missing tile end the walk
        let found = select_chain(&rel, |_| Some(r#"{"data":[1]}"#.into()));
        assert_eq!(found.iter().map(|r| r.num).collect::<Vec<_>>(), [13161, 10, 5]);
        // a select that points back is not followed
        assert!(select_chain(&rel, |_| Some(r#"{"data":[1],"select":[13161]}"#.into())).len() <= 2);
        assert!(select_chain(&rel, |_| None).is_empty());
        assert!(select_chain(&rel, |_| Some(r#"{"data":[1],"select":[99]}"#.into())).is_empty());
        assert!(select_chain(&rel, |_| Some("not json".into())).is_empty());
    }

    #[test]
    fn meta_layer_order_and_parse() {
        assert_eq!(meta_layers(19), [4, 3, 5, 2, 6, 1, 7, 0]);
        assert_eq!(meta_layers(15), [7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(meta_layers(12), [7, 6, 5, 4, 3, 2, 1, 0]);
        // 2018-09-02 11:00 UTC
        let ans = r#"{"features":[{"attributes":{"SRC_DATE2":1535886000000,"SRC_RES":0.3,
            "SRC_DESC":"Vexcel","NICE_NAME":"Image © 2018 Vexcel"}}]}"#;
        let (d, res, desc) = parse_meta(ans).unwrap();
        assert_eq!(d, "2018-09-02");
        assert_eq!(res, Some(0.3));
        assert_eq!(desc, "Image \u{a9} 2018 Vexcel Vexcel");
        assert_eq!(parse_meta(r#"{"features":[]}"#), None);
        assert_eq!(parse_meta(r#"{"features":[{"attributes":{"SRC_DATE2":null}}]}"#), None);
        assert_eq!(date_of_ms(0), "1970-01-01");
        assert_eq!(date_of_ms(-86_400_000), "1969-12-31");
        assert_eq!(date_of_ms(951_782_400_000), "2000-02-29");
    }

    #[test]
    fn describe_takes_the_first_layer_with_a_feature() {
        let rel = parse_config(CONFIG).unwrap();
        let mut asked = Vec::new();
        let hit = r#"{"features":[{"attributes":{"SRC_DATE2":1535886000000,"SRC_RES":null,"SRC_DESC":null,"NICE_NAME":"X"}}]}"#;
        let i = describe(&rel[1], 19, |l| {
            asked.push(l);
            match l {
                4 => None,                              // the request failed
                3 => Some(r#"{"features":[]}"#.into()), // no feature
                _ => Some(hit.into()),
            }
        });
        assert_eq!(asked, [4, 3, 5]);
        assert_eq!(
            (i.date.as_str(), i.release, i.release_date.as_str(), i.res_m, i.desc.as_str()),
            ("2018-09-02", 10, "2014-02-20", None, "X")
        );
        let none = describe(&rel[1], 19, |_| None);
        assert_eq!((none.date.as_str(), none.res_m), ("2014-02-20", None));
        assert!(none.desc.contains("no metadata"));
    }

    #[test]
    fn dates_keep_the_newest_release() {
        // newest release first: the second image of a date (an older release) is dropped
        let out = dedup_by_date(vec![
            img("2020-05-01", 30),
            img("2016-07-01", 20),
            img("2020-05-01", 10),
            img("2012-01-02", 5),
        ]);
        assert_eq!(
            out.iter().map(|i| (i.date.as_str(), i.release)).collect::<Vec<_>>(),
            [("2012-01-02", 5), ("2016-07-01", 20), ("2020-05-01", 30)]
        );
    }

    #[test]
    fn zoom_and_pixels_match_the_python() {
        // lat 52.4558, 150 m: 2 pi R cos(lat) / 256 * 1024 / 150 = 6.51e5, log2 = 19.3 -> 20, capped at Wayback's 19
        assert_eq!(zoom_for(150.0, 52.4558, MAX_Z), 19);
        assert_eq!(zoom_for(150.0, 52.4558, 22), 20);
        // a huge chip: the floor is 12; a tiny one: the ceiling
        assert_eq!(zoom_for(1.0e6, 52.4558, 19), 12);
        // ground pixel at z 19: 95393.1 / 524288 = 0.18195 m -> 150 / 0.18195 = 824.4
        assert_eq!(out_pixels(150.0, 52.4558, 19), 824);
        // z 20: 1648.8, capped at 1024; a 5 m chip at z 12: 128 at least
        assert_eq!(out_pixels(150.0, 52.4558, 20), 1024);
        assert_eq!(out_pixels(5.0, 52.4558, 12), 128);
        // the equator at z 0: one tile is the world's 40075016.7 m over 256 px
        assert_eq!(out_pixels(40075016.7 / 256.0 * 300.0, 0.0, 0), 300);
        let (x, y) = tile_xy(0.0, 0.0, 1);
        assert!((x - 1.0).abs() < 1e-12 && (y - 1.0).abs() < 1e-12);
        let b = merc_bounds(0, 0, 0, 0, 0);
        assert!((b.0 + E).abs() < 1e-6 && (b.3 - E).abs() < 1e-6 && (b.2 - E).abs() < 1e-6 && (b.1 + E).abs() < 1e-6);
    }

    #[test]
    fn one_image_per_year() {
        let ims = [
            img("2015-03-01", 1),
            img("2015-07-10", 2), // in season
            img("2015-12-01", 3),
            img("2016-01-05", 4), // only one of 2016, out of season
            img("2018-06-30", 5),
            img("2018-08-01", 6), // both in season: the later
        ];
        let p = pick_per_year(&ims, &[6, 7, 8]);
        assert_eq!(p, [(2015, 1, 2), (2016, 3, 0), (2018, 5, 1)]);
        // nothing in season: the last of each year
        let q = pick_per_year(&ims, &[11]);
        assert_eq!(q, [(2015, 2, 2), (2016, 3, 0), (2018, 5, 1)]);
        assert!(pick_per_year(&[], &[6]).is_empty());
    }

    /// A chip from tiles already in the cache folder (no network): each tile one colour by its place; the chip's
    /// centre lies in tile (x, y), which is where its centre pixel's colour comes from.
    #[test]
    fn render_from_cached_tiles() {
        crate::vectors::register();
        let dir = std::env::temp_dir().join("janus-geo-test-wayback");
        let _ = std::fs::remove_dir_all(&dir);
        let rel = Release {
            num: 7,
            date: "2020-01-01".into(),
            tile_url: "http://127.0.0.1:9/{level}/{row}/{col}".into(),
            meta_url: String::new(),
        };
        let (lon, lat, z) = (13.64525, 52.4558, 17u32);
        let (fx, fy) = tile_xy(lon, lat, z);
        let (cx, cy) = (fx as i64, fy as i64);
        let mem = DriverManager::get_driver_by_name("MEM").unwrap();
        let jpg = DriverManager::get_driver_by_name("JPEG").unwrap();
        let colour = |x: i64, y: i64| [(x.rem_euclid(4) * 60 + 30) as u8, (y.rem_euclid(4) * 60 + 30) as u8, 90u8];
        for ty in cy - 2..=cy + 2 {
            for tx in cx - 2..=cx + 2 {
                let ds = mem.create_with_band_type::<u8, _>("", 256, 256, 3).unwrap();
                for b in 0..3 {
                    let mut buf = Buffer::new((256, 256), vec![colour(tx, ty)[b]; 256 * 256]);
                    ds.rasterband(b + 1).unwrap().write((0, 0), (256, 256), &mut buf).unwrap();
                }
                let f = tile_path(&dir, 7, z, tx, ty);
                std::fs::create_dir_all(f.parent().unwrap()).unwrap();
                ds.create_copy(&jpg, &f, &Default::default()).unwrap();
            }
        }
        // the chip's centre in UTM 33N, 150 m, in the CRS of the cube
        let mut wgs = SpatialRef::from_epsg(4326).unwrap();
        wgs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let utm = srs_of("EPSG:32633").unwrap();
        let (mut xs, mut ys, mut zs) = ([lon], [lat], [0.0]);
        CoordTransform::new(&wgs, &utm).unwrap().transform_coords(&mut xs, &mut ys, &mut zs).unwrap();
        let im = render(&rel, z, "EPSG:32633", (xs[0], ys[0]), 150.0, 200, &dir).unwrap();
        assert_eq!((im.w, im.h, im.data.len()), (200, 200, 200 * 200 * 3));
        let at = |x: usize, y: usize| &im.data[(y * 200 + x) * 3..(y * 200 + x) * 3 + 3];
        // 150 m at z 17 (about 0.8 m a pixel) is under one tile: the centre is in the tile of the point
        let want = colour(cx, cy);
        for (g, w) in at(100, 100).iter().zip(want) {
            assert!(g.abs_diff(w) < 12, "centre {:?} vs {:?}", at(100, 100), want);
        }
        assert!(im.data.iter().any(|&v| v != 0));
        // a chip written as PNG
        let png = dir.join("chip.png");
        im.write_png(&png).unwrap();
        assert!(png.metadata().unwrap().len() > 100);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
