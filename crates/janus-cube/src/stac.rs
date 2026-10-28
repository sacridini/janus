//! STAC catalogs (File > Search STAC catalog...): the items of a collection over an area and dates (STAC API 1.0,
//! POST /search through GDAL's libcurl, the program's), their assets as bands, their dates; stac_vrt.rs makes each
//! date's mosaic of the chosen bands on one grid a VRT, a series read in place like File > Open URL. Planetary
//! Computer's assets are read through /vsiaz/ with its SAS tokens, renewed while the program runs.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

use crate::QaRule;
use crate::dates::days_from_civil;

/// A collection offered for a catalog, with the bands it starts with.
pub struct Collection {
    pub id: &'static str,
    pub name: &'static str,
    /// assets ticked at first (the reflectance bands of a surface reflectance stack, the quality band last)
    pub bands: &'static [&'static str],
    pub help: &'static str,
}

pub struct Catalog {
    pub id: &'static str,
    pub name: &'static str,
    /// the STAC API's root (its /search and /collections)
    pub url: &'static str,
    pub help: &'static str,
    pub collections: &'static [Collection],
}

/// The catalogs listed (a custom URL is typed in the window).
pub const CATALOGS: [Catalog; 3] = [
    Catalog {
        id: "earth-search",
        name: "Earth Search (Element 84, AWS)",
        url: "https://earth-search.aws.element84.com/v1",
        help: "Element 84's catalog of public data on AWS: Sentinel-2 as cloud-optimized GeoTIFFs, read without an \
               account.",
        collections: &[Collection {
            id: "sentinel-2-c1-l2a",
            name: "Sentinel-2 L2A, Collection 1",
            bands: &["blue", "green", "red", "nir", "swir16", "swir22", "scl"],
            help: "Surface reflectance, 10 to 60 m, every 5 days (2015 on), reprocessed by ESA as Collection 1; \
                   the scene classification (SCL) masks clouds",
        }],
    },
    Catalog {
        id: "planetary-computer",
        name: "Planetary Computer (Microsoft)",
        url: "https://planetarycomputer.microsoft.com/api/stac/v1",
        help: "Microsoft's catalog: Sentinel-2, Landsat and Sentinel-1 as cloud-optimized GeoTIFFs on Azure, read \
               with short-lived tokens (SAS) that Janus asks for and renews, without an account.",
        collections: &[
            Collection {
                id: "sentinel-2-l2a",
                name: "Sentinel-2 L2A",
                bands: &["B02", "B03", "B04", "B08", "B11", "B12", "SCL"],
                help: "Surface reflectance, 10 to 60 m, every 5 days (2015 on); the scene classification (SCL) \
                       masks clouds",
            },
            Collection {
                id: "landsat-c2-l2",
                name: "Landsat Collection 2 Level-2",
                bands: &["blue", "green", "red", "nir08", "swir16", "swir22", "qa_pixel"],
                help: "Surface reflectance of Landsat 4 to 9, 30 m, every 8 to 16 days (1982 on); QA_PIXEL masks \
                       clouds",
            },
            Collection {
                id: "sentinel-1-rtc",
                name: "Sentinel-1 RTC (radar)",
                bands: &["vv", "vh"],
                help: "Radar backscatter (gamma0, linear) in VV and VH, corrected for the terrain, 10 m, every 6 \
                       to 12 days (2014 on): through the clouds. Shown in dB; keep one orbit",
            },
        ],
    },
    Catalog {
        id: "bdc",
        name: "Brazil Data Cube (INPE)",
        url: "https://data.inpe.br/bdc/stac/v1",
        help: "INPE's catalog: data cubes of Brazil, composites of 16 days on a regular grid (and the scenes they \
               are made of), read without an account.",
        collections: &[
            Collection {
                id: "S2-16D-2",
                name: "Sentinel-2, 16-day cube",
                bands: &["B02", "B03", "B04", "B08", "B11", "B12", "SCL"],
                help: "Sentinel-2 surface reflectance, 10 m, the best observation of every 16 days (2017 on), \
                       with NDVI and EVI",
            },
            Collection {
                id: "LANDSAT-16D-1",
                name: "Landsat, 16-day cube",
                bands: &["blue", "green", "red", "nir08", "swir16", "swir22", "qa_pixel"],
                help: "Landsat surface reflectance, 30 m, the best observation of every 16 days, with NDVI and EVI",
            },
            Collection {
                id: "CBERS4-WFI-16D-2",
                name: "CBERS-4 WFI, 16-day cube",
                bands: &["BAND13", "BAND14", "BAND15", "BAND16"],
                help: "CBERS-4 WFI surface reflectance, 64 m, the best observation of every 16 days, with NDVI and \
                       EVI",
            },
        ],
    },
];

/// Planetary Computer's API (its assets need its tokens).
pub fn is_planetary(url: &str) -> bool {
    url.contains("planetarycomputer.microsoft.com")
}

/// One asset of an item: a file, and what STAC says of its band.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Asset {
    pub href: String,
    pub media: String,
    pub roles: Vec<String>,
    /// bands in the file (eo:bands or raster:bands; 0: not said)
    pub bands: usize,
    pub common_name: Option<String>,
    pub wavelength_nm: Option<f64>,
    pub scale: Option<f64>,
    pub offset: Option<f64>,
    pub nodata: Option<f64>,
    /// GDAL's name of its data type (from raster:bands)
    pub data_type: Option<String>,
    pub resolution: Option<f64>,
    /// its grid when it has one of its own (proj:shape as rows, columns; proj:transform as GDAL's geotransform)
    pub shape: Option<[usize; 2]>,
    pub gt: Option<[f64; 6]>,
}

impl Asset {
    /// A raster of one band (a cloud-optimized GeoTIFF, by its type or its name): what can be a band of a series.
    pub fn is_band(&self) -> bool {
        let href = self.href.split('?').next().unwrap_or("").to_ascii_lowercase();
        let tiff = self.media.contains("tiff") || href.ends_with(".tif") || href.ends_with(".tiff");
        let skip = ["thumbnail", "overview", "visual", "metadata"];
        tiff && self.bands <= 1 && !self.roles.iter().any(|r| skip.contains(&r.as_str()))
    }
}

/// An item: a scene (or a tile of a cube) at a date, its assets.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub collection: String,
    /// Unix seconds
    pub time: f64,
    pub cloud: Option<f64>,
    pub platform: String,
    /// a radar's pass: "ascending" or "descending" (sat:orbit_state), and its relative orbit (sat:relative_orbit)
    pub orbit_state: Option<String>,
    pub relative_orbit: Option<i64>,
    /// the tile it belongs to (MGRS, WRS-2 path/row, a cube's tile; else its id): items of a tile share a grid
    pub tile: String,
    /// its CRS: "EPSG:n" or WKT2 (None: read from its files)
    pub crs: Option<String>,
    pub shape: Option<[usize; 2]>,
    pub gt: Option<[f64; 6]>,
    /// the footprint (GeoJSON geometry, longitude and latitude)
    pub geometry: Value,
    pub assets: BTreeMap<String, Asset>,
}

impl Item {
    /// Days since 1970-01-01 of its date (UTC).
    pub fn day(&self) -> i64 {
        (self.time / 86400.0).floor() as i64
    }
}

/// GDAL's name of a STAC data type ("uint16" -> "UInt16").
fn gdal_type(t: &str) -> Option<String> {
    let n = match t {
        "uint8" => "Byte",
        "int8" => "Int8",
        "uint16" => "UInt16",
        "int16" => "Int16",
        "uint32" => "UInt32",
        "int32" => "Int32",
        "float32" => "Float32",
        "float64" => "Float64",
        _ => return None,
    };
    Some(n.into())
}

/// STAC's proj:transform (x = a col + b row + c, y = d col + e row + f) as a GDAL geotransform. Only a north-up
/// one is trusted: some of Planetary Computer's Sentinel-1 RTC items (from October 2024) give their files' grid
/// south-up and 224 km off, when the files are north-up; such an item's grid is read from its file.
fn geotransform(v: &Value) -> Option<[f64; 6]> {
    let t: Vec<f64> = v.as_array()?.iter().filter_map(Value::as_f64).collect();
    (t.len() >= 6 && t[0] > 0.0 && t[4] < 0.0).then(|| [t[2], t[0], t[1], t[5], t[3], t[4]])
}

fn shape(v: &Value) -> Option<[usize; 2]> {
    let s: Vec<usize> = v.as_array()?.iter().filter_map(|x| x.as_u64().map(|x| x as usize)).collect();
    (s.len() == 2 && s[0] > 0 && s[1] > 0).then(|| [s[0], s[1]])
}

/// The CRS of an item or asset: proj:code / proj:epsg, else proj:wkt2.
fn crs(o: &Value) -> Option<String> {
    if let Some(c) = o.get("proj:code").and_then(Value::as_str) {
        return Some(c.to_string());
    }
    if let Some(e) = o.get("proj:epsg").and_then(Value::as_i64) {
        return Some(format!("EPSG:{e}"));
    }
    o.get("proj:wkt2").and_then(Value::as_str).filter(|w| !w.is_empty()).map(str::to_string)
}

/// Unix seconds of an RFC 3339 time ("2024-07-28T14:35:43.951Z", "...+00:00").
pub fn parse_time(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let num = |a: usize, n: usize| -> Option<i64> { s.get(a..a + n)?.parse().ok() };
    let (y, m, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut t = days_from_civil(y as i32, m as u32, d as u32) as f64 * 86400.0;
    if b.len() >= 19 && matches!(b[10], b'T' | b't' | b' ') {
        t += (num(11, 2)? * 3600 + num(14, 2)? * 60 + num(17, 2)?) as f64;
        let mut i = 19;
        if b.get(i) == Some(&b'.') {
            let j = (i + 1..b.len()).find(|&k| !b[k].is_ascii_digit()).unwrap_or(b.len());
            t += format!("0.{}", &s[i + 1..j]).parse::<f64>().unwrap_or(0.0);
            i = j;
        }
        if let Some(&sign) = b.get(i).filter(|c| matches!(c, b'+' | b'-')) {
            let off = (num(i + 1, 2).unwrap_or(0) * 3600 + num(i + 4, 2).unwrap_or(0) * 60) as f64;
            t -= if sign == b'+' { off } else { -off };
        }
    }
    Some(t)
}

/// An item of a search's answer.
pub fn parse_item(f: &Value) -> Option<Item> {
    let p = f.get("properties")?;
    let time = p
        .get("datetime")
        .and_then(Value::as_str)
        .or_else(|| p.get("start_datetime").and_then(Value::as_str))
        .and_then(parse_time)?;
    let s = |k: &str| p.get(k).and_then(Value::as_str).map(str::to_string);
    let tile = s("grid:code")
        .or_else(|| s("s2:mgrs_tile"))
        .or_else(|| p.get("bdc:tiles").and_then(|t| t.get(0)).and_then(Value::as_str).map(str::to_string))
        .or_else(|| Some(format!("{}{}", s("landsat:wrs_path")?, s("landsat:wrs_row")?)))
        .or_else(|| s("tileId"))
        .unwrap_or_else(|| f.get("id").and_then(Value::as_str).unwrap_or("").to_string());
    let mut item = Item {
        id: f.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
        collection: f.get("collection").and_then(Value::as_str).unwrap_or("").to_string(),
        time,
        cloud: p.get("eo:cloud_cover").and_then(Value::as_f64),
        platform: s("platform").unwrap_or_default(),
        orbit_state: s("sat:orbit_state"),
        relative_orbit: p.get("sat:relative_orbit").and_then(Value::as_i64),
        tile,
        crs: crs(p),
        shape: p.get("proj:shape").and_then(shape),
        gt: p.get("proj:transform").and_then(geotransform),
        geometry: f.get("geometry").cloned().unwrap_or(Value::Null),
        assets: BTreeMap::new(),
    };
    // Sentinel-2 since processing baseline 04.00 (2022-01-25) adds 1000 to its reflectances (BOA_ADD_OFFSET); a
    // catalog that does not give the scale (Planetary Computer) gets ESA's
    let baseline = s("s2:processing_baseline").and_then(|b| b.parse::<f64>().ok());
    for (key, a) in f.get("assets").and_then(Value::as_object).into_iter().flatten() {
        let Some(href) = a.get("href").and_then(Value::as_str) else { continue };
        let eo = a.get("eo:bands").and_then(Value::as_array).or_else(|| a.get("bands").and_then(Value::as_array));
        let rb = a.get("raster:bands").and_then(Value::as_array);
        let eo0 = eo.and_then(|b| b.first());
        let rb0 = rb.and_then(|b| b.first());
        let f64_of = |o: Option<&Value>, k: &str| o.and_then(|o| o.get(k)).and_then(Value::as_f64);
        let wl = f64_of(eo0, "center_wavelength").or_else(|| f64_of(eo0, "eo:center_wavelength")).filter(|w| *w > 0.0);
        let mut asset = Asset {
            href: href.to_string(),
            media: a.get("type").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase(),
            roles: a
                .get("roles")
                .and_then(Value::as_array)
                .map(|r| r.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default(),
            bands: eo.map_or(0, Vec::len).max(rb.map_or(0, Vec::len)),
            common_name: eo0
                .and_then(|b| b.get("common_name").or_else(|| b.get("eo:common_name")))
                .and_then(Value::as_str)
                .map(str::to_string),
            // micrometres in STAC
            wavelength_nm: wl.map(|w| if w < 30.0 { w * 1000.0 } else { w }),
            scale: f64_of(rb0, "scale"),
            offset: f64_of(rb0, "offset"),
            nodata: rb0.and_then(|b| b.get("nodata")).and_then(|n| {
                n.as_f64().or_else(|| n.as_str().and_then(|s| if s == "nan" { Some(f64::NAN) } else { None }))
            }),
            data_type: rb0.and_then(|b| b.get("data_type")).and_then(Value::as_str).and_then(gdal_type),
            resolution: f64_of(rb0, "spatial_resolution").or_else(|| a.get("gsd").and_then(Value::as_f64)),
            shape: a.get("proj:shape").and_then(shape),
            gt: a.get("proj:transform").and_then(geotransform),
        };
        if asset.scale.is_none()
            && asset.wavelength_nm.is_some()
            && let Some(b) = baseline
        {
            asset.scale = Some(1e-4);
            asset.offset = Some(if b >= 4.0 { -0.1 } else { 0.0 });
            asset.nodata = asset.nodata.or(Some(0.0));
        }
        item.assets.insert(key.clone(), asset);
    }
    Some(item)
}

/// A search: a collection over an area (longitude, latitude) and dates (inclusive, "YYYY-MM-DD").
#[derive(Clone, Debug, PartialEq)]
pub struct Search {
    /// the API's root
    pub url: String,
    pub collection: String,
    pub bbox: [f64; 4],
    pub start: String,
    pub end: String,
    /// at most this % of clouds (eo:cloud_cover); None: all
    pub max_cloud: Option<f64>,
    /// at most this many items
    pub max_items: usize,
}

/// The JSON of a STAC request: GET (body None) or POST.
fn request(url: &str, body: Option<&Value>) -> Result<Value, String> {
    let post = body.map(Value::to_string);
    let bytes = janus_gdal::http_fetch(url, post.as_deref(), &["TIMEOUT=60", "MAX_RETRY=3", "RETRY_DELAY=2"])?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{url}: not JSON ({e})"))
}

/// The items of a search, oldest first, the page after page of the API (`progress`: items so far). Clouds are
/// filtered here, which works with every API (the query extension is not everywhere).
pub fn search(s: &Search, progress: &dyn Fn(usize), cancel: &AtomicBool) -> Result<Vec<Item>, String> {
    let root = s.url.trim_end_matches('/');
    let mut body = json!({
        "collections": [s.collection],
        "bbox": s.bbox,
        "datetime": format!("{}T00:00:00Z/{}T23:59:59Z", s.start, s.end),
        "limit": 100,
    });
    let mut url = format!("{root}/search");
    let mut post = true;
    let mut items: Vec<Item> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for _page in 0..1000 {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let page = request(&url, post.then_some(&body))?;
        if let Some(m) = page.get("code").and_then(Value::as_str) {
            let d = page.get("description").and_then(Value::as_str).unwrap_or("");
            return Err(format!("{m}: {d}"));
        }
        let feats = page.get("features").and_then(Value::as_array).ok_or("the answer has no features")?;
        for f in feats {
            let Some(it) = parse_item(f) else { continue };
            if s.max_cloud.is_some_and(|m| it.cloud.is_some_and(|c| c > m)) || !seen.insert(it.id.clone()) {
                continue;
            }
            items.push(it);
        }
        progress(items.len());
        if items.len() >= s.max_items || feats.is_empty() {
            break;
        }
        let next = page
            .get("links")
            .and_then(Value::as_array)
            .and_then(|l| l.iter().find(|l| l.get("rel").and_then(Value::as_str) == Some("next")));
        let Some(next) = next else { break };
        let Some(href) = next.get("href").and_then(Value::as_str) else { break };
        url = href.to_string();
        // a POST link carries its body (to merge with ours), else it is a GET of its URL
        post = next.get("method").and_then(Value::as_str).is_some_and(|m| m.eq_ignore_ascii_case("POST"));
        if post && let Some(b) = next.get("body").and_then(Value::as_object) {
            let merge = next.get("merge").and_then(Value::as_bool).unwrap_or(false);
            if !merge {
                body = json!({});
            }
            for (k, v) in b {
                body[k] = v.clone();
            }
        }
    }
    items.truncate(s.max_items);
    items.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.id.cmp(&b.id)));
    Ok(items)
}

/// A collection of a catalog: its id and title.
pub fn collections(url: &str) -> Result<Vec<(String, String)>, String> {
    let root = url.trim_end_matches('/');
    let v = request(&format!("{root}/collections?limit=1000"), None)?;
    let list = v.get("collections").and_then(Value::as_array).ok_or("the answer lists no collections")?;
    let mut out: Vec<(String, String)> = list
        .iter()
        .filter_map(|c| {
            let id = c.get("id")?.as_str()?.to_string();
            let title = c.get("title").and_then(Value::as_str).unwrap_or("").to_string();
            Some((id, title))
        })
        .collect();
    out.sort_by_key(|a| a.0.to_lowercase());
    Ok(out)
}

/// A band a series can have: an asset key, what to call it, its wavelength, whether it masks clouds.
#[derive(Clone, Debug, PartialEq)]
pub struct BandChoice {
    pub key: String,
    /// "Red (B04)": what the band roles and the Display panel go by
    pub label: String,
    pub wavelength_nm: Option<f64>,
    pub quality: QaRule,
}

/// A quality band recognised by its key: Sentinel-2's scene classification, Landsat C2's QA_PIXEL.
pub fn quality_rule(key: &str) -> QaRule {
    match key.to_ascii_lowercase().as_str() {
        "scl" => QaRule::Scl,
        "qa_pixel" => QaRule::LandsatC2,
        _ => QaRule::None,
    }
}

/// What a band is called (the names janus_cube::roles knows), from its common name.
fn role_label(common: &str) -> Option<&'static str> {
    Some(match common {
        "coastal" => "Coastal aerosol",
        "blue" => "Blue",
        "green" => "Green",
        "red" => "Red",
        "yellow" => "Yellow",
        "pan" => "Panchromatic",
        "rededge" => "Red edge",
        "nir" => "NIR",
        "nir08" => "NIR (narrow)",
        "nir09" => "Water vapour",
        "cirrus" => "Cirrus",
        "swir16" => "SWIR 1",
        "swir22" => "SWIR 2",
        "lwir" | "lwir11" => "Thermal",
        "lwir12" => "Thermal 2",
        _ => return None,
    })
}

/// The bands of a search's items (single-band rasters), those with a wavelength first in its order.
pub fn band_choices(items: &[Item]) -> Vec<BandChoice> {
    let mut seen: BTreeMap<String, BandChoice> = BTreeMap::new();
    for it in items.iter().take(50) {
        for (key, a) in &it.assets {
            if seen.contains_key(key) || !a.is_band() {
                continue;
            }
            let quality = quality_rule(key);
            let common = a.common_name.as_deref().map(str::to_ascii_lowercase);
            let label = match common.as_deref().and_then(role_label) {
                // the reflectance bands: by their role (a red edge of Sentinel-2's three by its key)
                Some(r) if quality == QaRule::None => {
                    if key.eq_ignore_ascii_case(common.as_deref().unwrap_or("")) {
                        r.to_string()
                    } else {
                        format!("{r} ({key})")
                    }
                }
                _ if quality == QaRule::Scl => "SCL (scene classification)".into(),
                // a radar's polarizations
                _ if ["vv", "vh", "hh", "hv"].contains(&key.to_ascii_lowercase().as_str()) => key.to_uppercase(),
                _ if quality == QaRule::LandsatC2 => "QA_PIXEL".into(),
                _ => key.clone(),
            };
            seen.insert(
                key.clone(),
                BandChoice {
                    key: key.clone(),
                    label,
                    wavelength_nm: a.wavelength_nm.filter(|_| quality == QaRule::None),
                    quality,
                },
            );
        }
    }
    let mut v: Vec<BandChoice> = seen.into_values().collect();
    v.sort_by(|a, b| match (a.wavelength_nm, b.wavelength_nm) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        _ => crate::inputs::natural_cmp(&a.key, &b.key),
    });
    v
}

/// The bands ticked at first: the collection's, else blue, green, red, NIR, SWIR 1, SWIR 2 and a quality band.
pub fn default_bands(choices: &[BandChoice], preset: Option<&[&str]>) -> Vec<String> {
    if let Some(p) = preset {
        let v: Vec<String> = p.iter().filter(|k| choices.iter().any(|c| c.key == **k)).map(|k| k.to_string()).collect();
        if !v.is_empty() {
            return v;
        }
    }
    let mut v = Vec::new();
    for want in ["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2"] {
        if let Some(c) = choices.iter().find(|c| c.label == want || c.label.starts_with(&format!("{want} ("))) {
            v.push(c.key.clone());
        }
    }
    if !v.iter().any(|k| choices.iter().any(|c| &c.key == k && c.label.starts_with("NIR")))
        && let Some(c) = choices.iter().find(|c| c.label.starts_with("NIR"))
    {
        v.push(c.key.clone());
    }
    if let Some(c) = choices.iter().find(|c| c.quality != QaRule::None) {
        v.push(c.key.clone());
    }
    v
}

/// Items of the same day (UTC), mosaicked into one date of the series.
#[derive(Clone, Debug, PartialEq)]
pub struct DateGroup {
    pub day: i64,
    /// indices into the items
    pub items: Vec<usize>,
    /// the mean cloud cover of its items (%)
    pub cloud: Option<f64>,
}

pub fn group_by_day(items: &[Item]) -> Vec<DateGroup> {
    group_by_day_of(items, &(0..items.len()).collect::<Vec<_>>())
}

/// The days of some of the items (`which`, indices into them).
pub fn group_by_day_of(items: &[Item], which: &[usize]) -> Vec<DateGroup> {
    let mut by: BTreeMap<i64, Vec<usize>> = BTreeMap::new();
    for &i in which {
        by.entry(items[i].day()).or_default().push(i);
    }
    by.into_iter()
        .map(|(day, idx)| {
            let c: Vec<f64> = idx.iter().filter_map(|&i| items[i].cloud).collect();
            let cloud = (!c.is_empty()).then(|| c.iter().sum::<f64>() / c.len() as f64);
            DateGroup { day, items: idx, cloud }
        })
        .collect()
}

/// A radar's pass over the area: its direction and relative orbit, the items of it. Scenes of one pass see the
/// ground from the same side and angle; mixing passes makes false steps in a series.
#[derive(Clone, Debug, PartialEq)]
pub struct Orbit {
    pub state: String,
    pub relative: Option<i64>,
    pub items: Vec<usize>,
}

impl Orbit {
    /// "descending 83"
    pub fn label(&self) -> String {
        match self.relative {
            Some(r) => format!("{} {r}", self.state),
            None => self.state.clone(),
        }
    }
}

/// The passes of the items that have one (sat:orbit_state), the most items first; empty when none has.
pub fn orbits(items: &[Item]) -> Vec<Orbit> {
    let mut by: BTreeMap<(String, Option<i64>), Vec<usize>> = BTreeMap::new();
    for (i, it) in items.iter().enumerate() {
        if let Some(st) = &it.orbit_state {
            by.entry((st.clone(), it.relative_orbit)).or_default().push(i);
        }
    }
    let mut v: Vec<Orbit> = by.into_iter().map(|((state, relative), items)| Orbit { state, relative, items }).collect();
    v.sort_by(|a, b| b.items.len().cmp(&a.items.len()).then(a.label().cmp(&b.label())));
    v
}

/// What a collection's series is shown as when it opens (an index of the library): a radar's VV in dB.
pub fn display_index(collection: &str) -> Option<&'static str> {
    collection.to_ascii_lowercase().starts_with("sentinel-1").then_some("VVdB")
}

/// Whether a collection's scenes have a cloud cover to filter by (a radar's do not).
pub fn has_clouds(collection: &str) -> bool {
    !collection.to_ascii_lowercase().starts_with("sentinel-1")
}

/// A GeoJSON FeatureCollection of the items' footprints (id, date, clouds), for a vector layer.
pub fn footprints_geojson(items: &[Item]) -> String {
    let feats: Vec<Value> = items
        .iter()
        .filter(|it| !it.geometry.is_null())
        .map(|it| {
            json!({
                "type": "Feature",
                "geometry": it.geometry,
                "properties": {
                    "id": it.id,
                    "date": crate::dates::format_date(it.time, crate::TimeGranularity::Day),
                    "cloud_cover": it.cloud,
                    "tile": it.tile,
                },
            })
        })
        .collect();
    json!({"type": "FeatureCollection", "features": feats}).to_string()
}

// Planetary Computer: each collection's container is read through /vsiaz/ with a SAS token of its API, set as the
// container's options (VSISetPathSpecificOption) and renewed before it expires. The VRTs keep the paths without the
// token (they open again in later runs: see sign_vrt_inputs).

struct Sas {
    collection: String,
    account: String,
    container: String,
    /// Unix seconds
    expiry: f64,
}

static SAS: Mutex<Vec<Sas>> = Mutex::new(Vec::new());
static RENEWER: OnceLock<()> = OnceLock::new();

fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// The account and container of an Azure Blob URL (https://account.blob.core.windows.net/container/...).
pub fn azure_blob(href: &str) -> Option<(String, String, String)> {
    let rest = href.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let account = host.strip_suffix(".blob.core.windows.net")?;
    let (container, key) = path.split_once('/')?;
    Some((account.to_string(), container.to_string(), key.split('?').next().unwrap_or(key).to_string()))
}

fn fetch_token(collection: &str) -> Result<(String, f64), String> {
    let url = format!("https://planetarycomputer.microsoft.com/api/sas/v1/token/{collection}");
    let v = request(&url, None)?;
    let token = v.get("token").and_then(Value::as_str).ok_or("Planetary Computer gave no token")?;
    let expiry = v.get("msft:expiry").and_then(Value::as_str).and_then(parse_time).unwrap_or(now() + 1800.0);
    Ok((token.to_string(), expiry))
}

fn apply_token(account: &str, container: &str, token: &str) {
    let prefix = format!("/vsiaz/{container}/");
    janus_gdal::set_path_option(&prefix, "AZURE_STORAGE_ACCOUNT", Some(account));
    janus_gdal::set_path_option(&prefix, "AZURE_STORAGE_SAS_TOKEN", Some(token));
    janus_gdal::set_path_option(&prefix, "AZURE_NO_SIGN_REQUEST", Some("NO"));
}

/// A collection's container readable through /vsiaz/ (a token asked for unless one is still valid for 10 min).
pub fn sign_planetary(collection: &str, account: &str, container: &str) -> Result<(), String> {
    {
        let sas = SAS.lock().unwrap();
        if sas.iter().any(|s| s.account == account && s.container == container && s.expiry - now() > 600.0) {
            return Ok(());
        }
    }
    let (token, expiry) = fetch_token(collection)?;
    apply_token(account, container, &token);
    let mut sas = SAS.lock().unwrap();
    sas.retain(|s| !(s.account == account && s.container == container));
    sas.push(Sas { collection: collection.into(), account: account.into(), container: container.into(), expiry });
    drop(sas);
    RENEWER.get_or_init(|| {
        let _ = std::thread::Builder::new().name("stac tokens".into()).spawn(|| {
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
                let due: Vec<(String, String, String)> = SAS
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|s| s.expiry - now() < 600.0)
                    .map(|s| (s.collection.clone(), s.account.clone(), s.container.clone()))
                    .collect();
                for (c, a, k) in due {
                    let _ = sign_planetary(&c, &a, &k);
                }
            }
        });
    });
    Ok(())
}

/// The metadata item of a VRT that names what to sign before reading it: "collection account container", one per
/// line.
pub const SAS_METADATA: &str = "JANUS_PLANETARY_SAS";

/// VRTs written from Planetary Computer: their containers signed before they are opened (a later run, a reopened
/// folder). Other inputs are left alone; a VRT is read only up to 1 MB to find the item.
pub fn sign_vrt_inputs(files: &[String]) {
    for f in files {
        if !f.to_ascii_lowercase().ends_with(".vrt") || crate::is_remote_path(f) {
            continue;
        }
        let Ok(mut file) = std::fs::File::open(f) else { continue };
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::Read::take(&mut file, 1 << 20), &mut text);
        let key = format!("<MDI key=\"{SAS_METADATA}\">");
        let Some(at) = text.find(&key) else { continue };
        let rest = &text[at + key.len()..];
        let val = &rest[..rest.find("</MDI>").unwrap_or(0)];
        for line in val.lines() {
            let w: Vec<&str> = line.split_whitespace().collect();
            if w.len() == 3 {
                let _ = sign_planetary(w[0], w[1], w[2]);
            }
        }
    }
}

/// What GDAL opens for an asset: Planetary Computer's through /vsiaz/ (signed), others as File > Open URL does.
pub fn asset_path(href: &str, planetary: bool) -> String {
    if planetary && let Some((_, container, key)) = azure_blob(href) {
        return format!("/vsiaz/{container}/{key}");
    }
    crate::gdal_path(href)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature() -> Value {
        json!({
            "type": "Feature",
            "id": "S2A_T20LMP_20240728T142957_L2A",
            "collection": "sentinel-2-c1-l2a",
            "geometry": {"type": "Polygon", "coordinates": [[[-63.9, -10.0], [-62.9, -10.0], [-62.9, -9.0], [-63.9, -10.0]]]},
            "properties": {
                "datetime": "2024-07-28T14:35:43.951000Z",
                "platform": "sentinel-2a",
                "eo:cloud_cover": 0.14,
                "proj:epsg": 32720,
                "grid:code": "MGRS-20LMP",
            },
            "assets": {
                "red": {
                    "href": "https://e84.s3.amazonaws.com/20/L/MP/B04.tif",
                    "type": "image/tiff; application=geotiff; profile=cloud-optimized",
                    "roles": ["data", "reflectance"],
                    "proj:shape": [10980, 10980],
                    "proj:transform": [10, 0, 399960, 0, -10, 8900020],
                    "eo:bands": [{"name": "B04", "common_name": "red", "center_wavelength": 0.665}],
                    "raster:bands": [{"nodata": 0, "data_type": "uint16", "scale": 0.0001, "offset": -0.1}],
                },
                "scl": {
                    "href": "https://e84.s3.amazonaws.com/20/L/MP/SCL.tif",
                    "type": "image/tiff; application=geotiff",
                    "roles": ["data"],
                    "proj:shape": [5490, 5490],
                    "proj:transform": [20, 0, 399960, 0, -20, 8900020],
                    "raster:bands": [{"nodata": 0, "data_type": "uint8"}],
                },
                "visual": {
                    "href": "https://e84.s3.amazonaws.com/20/L/MP/TCI.tif",
                    "type": "image/tiff; application=geotiff",
                    "roles": ["visual"],
                    "eo:bands": [{"common_name": "red"}, {"common_name": "green"}, {"common_name": "blue"}],
                },
                "thumbnail": {"href": "https://e84.s3.amazonaws.com/t.jpg", "type": "image/jpeg", "roles": ["thumbnail"]},
            },
        })
    }

    #[test]
    fn times() {
        assert_eq!(parse_time("1970-01-02T00:00:00Z"), Some(86400.0));
        assert_eq!(parse_time("1970-01-01T01:00:00.5+01:00"), Some(0.5));
        assert_eq!(parse_time("2024-07-27T00:00:00.000000Z"), parse_time("2024-07-27"));
        assert!(parse_time("July").is_none());
    }

    #[test]
    fn items_and_bands() {
        let it = parse_item(&feature()).unwrap();
        assert_eq!((it.tile.as_str(), it.crs.as_deref(), it.cloud), ("MGRS-20LMP", Some("EPSG:32720"), Some(0.14)));
        let red = &it.assets["red"];
        assert_eq!(red.gt, Some([399960.0, 10.0, 0.0, 8900020.0, 0.0, -10.0]));
        assert_eq!(red.shape, Some([10980, 10980]));
        assert_eq!((red.scale, red.offset, red.nodata), (Some(1e-4), Some(-0.1), Some(0.0)));
        assert_eq!(red.data_type.as_deref(), Some("UInt16"));
        assert!((red.wavelength_nm.unwrap() - 665.0).abs() < 1e-9);
        let c = band_choices(&[it]);
        // the visual composite and the thumbnail are not bands; reflectance first
        assert_eq!(c.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(), ["red", "scl"]);
        assert_eq!((c[0].label.as_str(), c[1].label.as_str()), ("Red", "SCL (scene classification)"));
        assert_eq!(c[1].quality, QaRule::Scl);
        assert_eq!(default_bands(&c, None), ["red", "scl"]);
        assert_eq!(default_bands(&c, Some(&["B04", "scl"])), ["scl"]);
    }

    #[test]
    fn sentinel2_offset_by_baseline() {
        // Planetary Computer: no raster:bands; the baseline says whether 1000 was added
        let mut f = feature();
        f["properties"]["s2:processing_baseline"] = json!("05.11");
        f["assets"]["red"].as_object_mut().unwrap().remove("raster:bands");
        let a = &parse_item(&f).unwrap().assets["red"];
        assert_eq!((a.scale, a.offset), (Some(1e-4), Some(-0.1)));
        f["properties"]["s2:processing_baseline"] = json!("03.01");
        let a = &parse_item(&f).unwrap().assets["red"];
        assert_eq!((a.scale, a.offset), (Some(1e-4), Some(0.0)));
    }

    #[test]
    fn days_and_blobs() {
        let mut a = parse_item(&feature()).unwrap();
        let mut b = a.clone();
        b.time += 3600.0;
        b.cloud = Some(10.14);
        a.id = "a".into();
        let mut c = a.clone();
        c.time += 86400.0 * 5.0;
        let g = group_by_day(&[a, b, c]);
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].items, [0, 1]);
        assert!((g[0].cloud.unwrap() - 5.14).abs() < 1e-9);
        assert_eq!(
            azure_blob("https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/20/L/B04.tif?x=1"),
            Some(("sentinel2l2a01".into(), "sentinel2-l2".into(), "20/L/B04.tif".into()))
        );
        assert_eq!(
            asset_path("https://sentinel2l2a01.blob.core.windows.net/sentinel2-l2/20/B04.tif", true),
            "/vsiaz/sentinel2-l2/20/B04.tif"
        );
        assert_eq!(asset_path("https://h/x/B04.tif", false), "/vsicurl/https://h/x/B04.tif");
    }

    /// Sentinel-1 RTC: its passes read, the most frequent first; the polarizations named as such; shown in dB.
    #[test]
    fn radar_passes() {
        let mut f = feature();
        f["properties"]["sat:orbit_state"] = json!("descending");
        f["properties"]["sat:relative_orbit"] = json!(83);
        let it = parse_item(&f).unwrap();
        assert_eq!((it.orbit_state.as_deref(), it.relative_orbit), (Some("descending"), Some(83)));
        let pass = |st: &str, r: i64| Item { orbit_state: Some(st.into()), relative_orbit: Some(r), ..it.clone() };
        let items =
            vec![pass("ascending", 10), pass("descending", 83), pass("descending", 83), Item { ..Default::default() }];
        let o = orbits(&items);
        assert_eq!(o.len(), 2);
        assert_eq!((o[0].label(), o[0].items.clone()), ("descending 83".to_string(), vec![1, 2]));
        assert_eq!(group_by_day_of(&items, &o[1].items).len(), 1);
        assert!(orbits(&items[3..]).is_empty());
        assert_eq!(display_index("sentinel-1-rtc"), Some("VVdB"));
        // a south-up proj:transform is not trusted (the file's grid is read instead)
        assert_eq!(geotransform(&json!([10.0, 0.0, 339780.0, 0.0, -10.0, 9016350.0])).map(|g| g[3]), Some(9016350.0));
        assert!(geotransform(&json!([10.0, 0.0, 339779.26, 0.0, 10.0, 8792067.35, 0.0, 0.0, 1.0])).is_none());
        assert!(!has_clouds("sentinel-1-rtc") && has_clouds("sentinel-2-l2a"));
        assert!(crate::calc::indices::find("VVdB").is_some_and(|i| i.formula == "10.0 * log10(VV)"));
    }
}
