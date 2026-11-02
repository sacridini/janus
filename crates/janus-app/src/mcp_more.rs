//! The MCP server's tools of STAC catalogs (their collections, a search, a series built from it: opened in place,
//! downloaded or composited, as the STAC window does) and of vector files (what a file holds, the series at each of
//! its features). A child of mcp.rs: its helpers and its Server.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
use gdal::vector::LayerAccess;
use janus_cube::stac::{self, BandChoice, CATALOGS, Item};
use janus_cube::stac_vrt::{self, BuildSpec};
use janus_ml::InputBand;
use janus_zeit::composite::{CompOptions, Method};
use serde_json::{Value, json};

use super::{Call, R, Server, out_dir, vector_features};

/// Items a search gets at most (the STAC window's).
const MAX_ITEMS: usize = 2000;

/// A catalog by its id (earth-search, planetary-computer, bdc) or the URL of a STAC API.
fn catalog_url(a: &Value) -> R<String> {
    let c = a["catalog"].as_str().unwrap_or("earth-search").trim();
    if let Some(k) = CATALOGS.iter().find(|k| k.id.eq_ignore_ascii_case(c)) {
        return Ok(k.url.to_string());
    }
    if c.starts_with("http://") || c.starts_with("https://") {
        return Ok(c.to_string());
    }
    Err(format!(
        "catalog: {} or the URL of a STAC API, not {c:?}",
        CATALOGS.iter().map(|k| k.id).collect::<Vec<_>>().join(", ")
    ))
}

/// The bands a collection starts with in the STAC window (its preset), if the catalog lists it.
fn preset(url: &str, collection: &str) -> Option<&'static [&'static str]> {
    CATALOGS
        .iter()
        .filter(|k| k.url == url)
        .flat_map(|k| k.collections.iter())
        .find(|c| c.id.eq_ignore_ascii_case(collection))
        .map(|c| c.bands)
}

/// A date "YYYY-MM-DD" (checked).
fn date_arg(a: &Value, key: &str) -> R<String> {
    let s = a[key].as_str().ok_or(format!("{key}: a date, YYYY-MM-DD"))?.trim();
    let ok = s.len() == 10
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && janus_cube::stac::parse_time(s).is_some();
    if !ok {
        return Err(format!("{key}: a date as YYYY-MM-DD, not {s:?}"));
    }
    Ok(s.to_string())
}

fn day_text(day: i64) -> String {
    janus_cube::dates::format_date(day as f64 * 86400.0, janus_cube::TimeGranularity::Day)
}

/// A search's items as the arguments ask (catalog, collection, bbox_lonlat, start, end, max_cloud, max_items).
fn search(a: &Value, c: &Call) -> R<(stac::Search, Vec<Item>)> {
    let url = catalog_url(a)?;
    let collection = a["collection"].as_str().ok_or("collection: the collection's id (stac_catalogs)")?.trim();
    let bbox: Vec<f64> =
        a["bbox_lonlat"].as_array().map(|v| v.iter().filter_map(Value::as_f64).collect()).unwrap_or_default();
    let [w, s, e, n] = bbox[..] else {
        return Err("bbox_lonlat: [lon_min, lat_min, lon_max, lat_max]".into());
    };
    if !(w < e && s < n && (-180.0..=180.0).contains(&w) && (-90.0..=90.0).contains(&s)) {
        return Err(format!("bbox_lonlat: [lon_min, lat_min, lon_max, lat_max], not {bbox:?}"));
    }
    let q = stac::Search {
        url,
        collection: collection.to_string(),
        bbox: [w, s, e, n],
        start: date_arg(a, "start")?,
        end: date_arg(a, "end")?,
        max_cloud: a["max_cloud"].as_f64().filter(|m| *m < 100.0 && stac::has_clouds(collection)),
        max_items: a["max_items"].as_u64().map_or(MAX_ITEMS, |m| (m as usize).clamp(1, MAX_ITEMS)),
    };
    c.progress(0.0, "searching");
    let items = stac::search(&q, &|n| c.progress(0.0, &format!("{n} items found")), &c.stop)?;
    if items.is_empty() {
        return Err(format!(
            "no item of {} over {:?} from {} to {}{}",
            q.collection,
            q.bbox,
            q.start,
            q.end,
            q.max_cloud.map_or(String::new(), |m| format!(" with at most {m}% clouds"))
        ));
    }
    Ok((q, items))
}

/// The items of the pass asked for (`orbit`: "descending 83"), else of the pass with the most; all of them when
/// the items have no pass (an optical collection).
fn of_orbit(a: &Value, items: Vec<Item>) -> R<(Vec<Item>, Option<String>)> {
    let orbits = stac::orbits(&items);
    if orbits.is_empty() {
        return Ok((items, None));
    }
    let o = match a["orbit"].as_str() {
        Some(want) => orbits.iter().find(|o| o.label().eq_ignore_ascii_case(want.trim())).ok_or(format!(
            "orbit: one of {}",
            orbits.iter().map(|o| format!("{:?}", o.label())).collect::<Vec<_>>().join(", ")
        ))?,
        None => &orbits[0],
    };
    let label = o.label();
    Ok((o.items.iter().map(|&i| items[i].clone()).collect(), Some(label)))
}

/// The bands asked for (asset keys or labels, in their order), else the collection's first ones.
fn bands_of(a: &Value, url: &str, collection: &str, items: &[Item]) -> R<Vec<BandChoice>> {
    let choices = stac::band_choices(items);
    let keys: Vec<String> = match a["bands"].as_array() {
        Some(v) => v.iter().filter_map(|b| b.as_str().map(str::to_string)).collect(),
        None => stac::default_bands(&choices, preset(url, collection)),
    };
    let mut out = vec![];
    for k in &keys {
        let b = choices.iter().find(|c| c.key.eq_ignore_ascii_case(k) || c.label.eq_ignore_ascii_case(k)).ok_or_else(
            || {
                format!(
                    "bands: {k:?} is not a band of these items ({})",
                    choices.iter().map(|c| c.key.as_str()).collect::<Vec<_>>().join(", ")
                )
            },
        )?;
        out.push(b.clone());
    }
    if out.is_empty() {
        return Err("bands: none chosen".into());
    }
    Ok(out)
}

/// The composite's options for the bands and items (the STAC window's composite_options): the quality band apart,
/// each date's sensor and cover scores.
fn composite_options(a: &Value, bands: &[BandChoice], items: &[Item], threads: usize) -> R<CompOptions> {
    let o = &a["composite"];
    let m = o["method"].as_str().unwrap_or("bap");
    let method = *Method::ALL
        .iter()
        .find(|x| x.id() == m)
        .ok_or(format!("composite.method: bap, greenest, medoid or median, not {m:?}"))?;
    let (mut idx, mut names) = (vec![], vec![]);
    let (mut qa_band, mut qa_rule) = (0, janus_cube::QaRule::None);
    for (i, b) in bands.iter().enumerate() {
        if b.quality != janus_cube::QaRule::None {
            if qa_band == 0 {
                (qa_band, qa_rule) = (i as i32 + 1, b.quality);
            }
        } else {
            idx.push(i as i32 + 1);
            names.push(b.label.clone());
        }
    }
    let find = |p: &dyn Fn(&str) -> bool| bands.iter().position(|b| p(&b.label)).map_or(0, |i| i as i32 + 1);
    let red = find(&|l| l == "Red" || l.starts_with("Red ("));
    let nir = find(&|l| l == "NIR" || l.starts_with("NIR ("));
    let groups = stac::group_by_day(items);
    let sensor = groups
        .iter()
        .map(|g| {
            let v: Vec<f64> =
                g.items.iter().map(|&i| janus_zeit::composite::sensor_score(&items[i].platform, g.day)).collect();
            v.iter().sum::<f64>() / v.len().max(1) as f64
        })
        .collect();
    let cover = groups.iter().map(|g| g.cloud.map_or(1.0, |c| 1.0 - c.clamp(0.0, 100.0) / 100.0)).collect();
    // the target day: given ("MM-DD"), else the middle of the dates within a year, else July 2nd
    let target = match o["target"].as_str() {
        Some(t) => {
            let (mm, dd) = t.split_once('-').ok_or(format!("composite.target: MM-DD, not {t:?}"))?;
            let (mm, dd): (u32, u32) = (
                mm.parse().map_err(|_| "composite.target: MM-DD")?,
                dd.parse().map_err(|_| "composite.target: MM-DD")?,
            );
            (janus_cube::dates::days_from_civil(2001, mm, dd) - janus_cube::dates::days_from_civil(2001, 1, 1) + 1)
                as f64
        }
        None => {
            let (a, b) = (groups.first().map_or(0, |g| g.day), groups.last().map_or(0, |g| g.day));
            if b - a > 365 {
                183.0
            } else {
                let mid = (a + b) / 2;
                let (y, _, _) = janus_cube::dates::civil_from_days(mid);
                (mid - janus_cube::dates::days_from_civil(y, 1, 1) + 1) as f64
            }
        }
    };
    if method == Method::Greenest && (red == 0 || nir == 0) {
        return Err("composite.method greenest needs the red and NIR bands among the bands".into());
    }
    Ok(CompOptions {
        method,
        step: o["yearly"].as_bool().unwrap_or(false).then_some(janus_zeit::regularize::Step::Year),
        bands: idx,
        names,
        qa_band,
        qa_rule,
        red,
        nir,
        bap: janus_zeit::composite::BapOptions {
            target_doy: target,
            doy_sigma: o["doy_sigma"].as_f64().unwrap_or(30.0),
            cloud_min: 30.0,
            cloud_max: 1500.0,
            w_doy: 0.3,
            w_cloud: 0.5,
            w_sensor: 0.1,
            w_cover: 0.1,
        },
        sensor,
        cover,
        info: o["info"].as_bool().unwrap_or(true),
        window: None,
        block: 1024,
        threads,
        stem: "composite".into(),
    })
}

/// A footprint-free summary of the items: by day (their count and cloud), the platforms.
fn by_day(items: &[Item]) -> Vec<Value> {
    stac::group_by_day(items)
        .iter()
        .map(|g| {
            json!({
                "date": day_text(g.day),
                "items": g.items.len(),
                "cloud": g.cloud.map(|c| (c * 10.0).round() / 10.0),
            })
        })
        .collect()
}

impl Server {
    /// The catalogs Janus knows and their collections, or the collections of one catalog's API.
    pub(super) fn stac_catalogs(&self, a: &Value) -> R<Value> {
        if a.get("catalog").and_then(Value::as_str).is_none() {
            return Ok(json!({
                "catalogs": CATALOGS.iter().map(|k| json!({
                    "id": k.id,
                    "name": k.name,
                    "url": k.url,
                    "about": k.help,
                    "collections": k.collections.iter().map(|c| json!({
                        "id": c.id, "name": c.name, "bands": c.bands, "about": c.help,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "note": "any other STAC API by its URL as catalog; stac_catalogs with catalog lists all its collections",
            }));
        }
        let url = catalog_url(a)?;
        let list = stac::collections(&url)?;
        let filter = a["filter"].as_str().unwrap_or("").to_lowercase();
        let shown: Vec<Value> = list
            .iter()
            .filter(|(id, t)| {
                filter.is_empty() || id.to_lowercase().contains(&filter) || t.to_lowercase().contains(&filter)
            })
            .take(500)
            .map(|(id, t)| json!({ "id": id, "title": t }))
            .collect();
        Ok(json!({ "catalog": url, "collections": shown, "total": list.len() }))
    }

    /// A search: its dates (items per day, their clouds), the bands the items have, the radar passes.
    pub(super) fn stac_search(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let (q, items) = search(a, c)?;
        let choices = stac::band_choices(&items);
        let on = stac::default_bands(&choices, preset(&q.url, &q.collection));
        let mut platforms: std::collections::BTreeMap<String, usize> = Default::default();
        for it in &items {
            *platforms.entry(if it.platform.is_empty() { "?".into() } else { it.platform.clone() }).or_default() += 1;
        }
        let days = by_day(&items);
        let mut answer = json!({
            "items": items.len(),
            "dates": days.len(),
            "first": items.first().map(|i| day_text(i.day())),
            "last": items.last().map(|i| day_text(i.day())),
            "by_date": days.iter().take(400).collect::<Vec<_>>(),
            "bands": choices.iter().map(|b| json!({
                "key": b.key,
                "label": b.label,
                "wavelength_nm": b.wavelength_nm,
                "masks_clouds": b.quality != janus_cube::QaRule::None,
                "default": on.contains(&b.key),
            })).collect::<Vec<_>>(),
            "platforms": platforms,
            "orbits": stac::orbits(&items).iter().map(|o| json!({ "orbit": o.label(), "items": o.items.len() })).collect::<Vec<_>>(),
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
            "next": "stac_series with the same search builds the series (open in place, download or composite)",
        });
        if items.len() >= q.max_items {
            answer["note"] = json!(format!("{} items at most were taken: narrow the dates or the area", q.max_items));
        }
        if a["footprints"].as_bool() == Some(true) {
            let dir = out_dir(a, "stac_search")?;
            let f = dir.join("footprints.geojson");
            std::fs::write(&f, stac::footprints_geojson(&items)).map_err(|e| format!("{}: {e}", f.display()))?;
            answer["footprints"] = json!(f);
        }
        Ok(answer)
    }

    /// A series from a search: VRTs reading the catalog in place, downloaded as COGs, or composited (the best
    /// available pixel, the greenest, the medoid, the median), one date or period a file.
    pub(super) fn stac_series(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let (q, items) = search(a, c)?;
        let (items, orbit) = of_orbit(a, items)?;
        let bands = bands_of(a, &q.url, &q.collection, &items)?;
        let mode = a["mode"].as_str().unwrap_or("open");
        if !["open", "download", "composite"].contains(&mode) {
            return Err(format!("mode: open, download or composite, not {mode:?}"));
        }
        let comp = (mode == "composite").then(|| composite_options(a, &bands, &items, self.threads)).transpose()?;
        let dir = out_dir(a, &format!("stac_{}", stac_vrt::sanitize(&q.collection)))?;
        let spec = BuildSpec {
            items,
            bands: bands.clone(),
            bbox: q.bbox,
            res: a["resolution"].as_f64().filter(|r| *r > 0.0),
            dir: if mode == "open" { dir.clone() } else { dir.join("vrt") },
            planetary: stac::is_planetary(&q.url),
        };
        let split = if mode == "open" { 1.0 } else { 0.1 };
        let built = stac_vrt::build(&spec, &|f, m| c.progress(f * split, m), &c.stop)?;
        let rest = |f: f64, m: &str| c.progress(split + (1.0 - split) * f, m);
        let (files, periods) = match (&comp, mode) {
            (Some(o), _) => {
                let info = janus_cube::open_cube(&built.files, &janus_cube::BandSelection::default())?;
                let r = janus_zeit::composite::composite(&Arc::new(info), o, &dir, &rest, &c.stop)?;
                (r.files, Some(r.periods.len()))
            }
            (None, "download") => (stac_vrt::download(&built.files, &dir, &rest, &c.stop)?, None),
            _ => (built.files.clone(), None),
        };
        let g = &built.grid;
        // the files written, not the folder: its vrt/ and info/ beside them
        let series = match (&comp, mode) {
            (Some(o), _) => json!(dir.join(format!("{}_*.tif", o.stem))),
            (None, "download") => json!(dir.join("*.tif")),
            _ => json!(dir),
        };
        let mut answer = json!({
            "series": series,
            "mode": mode,
            "dates": files.len(),
            "first_file": files.first(),
            "last_file": files.last(),
            "bands": bands.iter().enumerate().map(|(i, b)| json!({ "band": i + 1, "key": b.key, "label": b.label, "masks_clouds": b.quality != janus_cube::QaRule::None })).collect::<Vec<_>>(),
            "grid": { "width": g.w, "height": g.h, "pixel": g.gt[1], "crs": if g.crs.len() > 60 { "WKT (see describe_series)".to_string() } else { g.crs.clone() } },
            "items_warped": built.warped,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        });
        if let Some(o) = &orbit {
            answer["orbit"] = json!(o);
        }
        if let Some(n) = periods {
            let o = comp.as_ref().unwrap();
            answer["composite"] = json!({
                "method": o.method.label(),
                "periods": n,
                "bands": o.names,
                "info_files": "beside them (year, day of year, score, clear observations of each pixel)",
            });
            answer["dates"] = json!(files.len());
            if n == 1 && o.names.len() > 1 {
                answer["composite"]["one_period"] = json!(
                    "one file whose bands are the composite's bands, not dates: give the series with its band \
                     (or expression) to read it as one date, or ask for yearly periods over several years"
                );
            }
        }
        if let Some(ix) = stac::display_index(&q.collection) {
            answer["expression"] = json!(ix);
        }
        answer["note"] = json!(match mode {
            "open" =>
                "VRTs reading the catalog in place (each read goes to the network): use download for a series kept on disk",
            "download" => "a cloud-optimized GeoTIFF per date, every band",
            _ => "a cloud-optimized GeoTIFF per period of the composite",
        });
        Ok(answer)
    }

    /// What a vector file holds: its layers, their geometry, features, CRS, extent and fields, a few features.
    pub(super) fn describe_vector(&self, a: &Value) -> R<Value> {
        let path = a["path"].as_str().ok_or("path: a vector file (GeoPackage, Shapefile, GeoJSON, KML, CSV...)")?;
        let ds = gdal::Dataset::open(path).map_err(|e| format!("{path}: {e}"))?;
        let mut wgs = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
        wgs.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
        let n_show = a["features"].as_u64().unwrap_or(3).min(50) as usize;
        let mut layers = vec![];
        for mut l in ds.layers() {
            let geom = l
                .defn()
                .geom_fields()
                .next()
                .map(|g| gdal::vector::geometry_type_to_name(g.field_type()))
                .unwrap_or_else(|| "none".into());
            let fields: Vec<Value> = l
                .defn()
                .fields()
                .map(|f| json!({ "name": f.name(), "type": gdal::vector::field_type_to_name(f.field_type()) }))
                .collect();
            let srs = l.spatial_ref();
            let extent = l.try_get_extent().ok().flatten();
            let lonlat = match (&srs, &extent) {
                (Some(s), Some(e)) => {
                    let mut s = s.clone();
                    s.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
                    CoordTransform::new(&s, &wgs)
                        .ok()
                        .and_then(|ct| ct.transform_bounds(&[e.MinX, e.MinY, e.MaxX, e.MaxY], 21).ok())
                        .map(|b| json!(b))
                }
                _ => None,
            };
            let names: Vec<String> = l.defn().fields().map(|f| f.name()).collect();
            let mut shown = vec![];
            for f in l.features().take(n_show) {
                let mut o = serde_json::Map::new();
                for (i, n) in names.iter().enumerate() {
                    o.insert(n.clone(), f.field_as_string(i).ok().flatten().map_or(Value::Null, Value::String));
                }
                shown.push(json!({ "fid": f.fid(), "attributes": o }));
            }
            layers.push(json!({
                "layer": l.name(),
                "geometry": geom,
                "features": l.feature_count(),
                "crs": srs.as_ref().and_then(|s| s.name()),
                "epsg": srs.as_ref().and_then(|s| s.auth_code().ok()),
                "extent": extent.map(|e| [e.MinX, e.MinY, e.MaxX, e.MaxY]),
                "extent_lonlat": lonlat,
                "fields": fields,
                "first_features": shown,
            }));
        }
        Ok(json!({ "path": path, "layers": layers }))
    }

    /// The series at each feature of a vector file: a point's pixel, a polygon's mean of its pixels inside (up to
    /// max_pixels spread over it); a CSV (feature, id, date, value, pixels) and a summary.
    pub(super) fn vector_series(&self, a: &Value, c: &Call) -> R<Value> {
        let t0 = Instant::now();
        let info = self.series(&a["series"])?;
        let vector = a["vector"].as_str().ok_or("vector: a vector file of points or polygons")?;
        let max = a["max_pixels"].as_u64().unwrap_or(500).clamp(1, 100_000) as usize;
        c.progress(0.0, "placing the features");
        let feats = vector_features(&info, vector, a["layer"].as_str(), a["id_field"].as_str(), max)?;
        if feats.is_empty() {
            return Err(format!("no feature of {vector} falls on the series"));
        }
        let pixels: Vec<(usize, usize)> = feats.iter().flat_map(|f| f.1.iter().copied()).collect();
        c.progress(0.1, &format!("reading {} pixels of {} features", pixels.len(), feats.len()));
        let shown = InputBand::shown(&info);
        let raw = janus_ml::data::read_points(&info, std::slice::from_ref(&shown), &pixels);
        let t_n = info.t();
        let dir = out_dir(a, "vector_series")?;
        let csv = dir.join(format!(
            "{}_series.csv",
            Path::new(vector).file_stem().map_or("features".into(), |s| s.to_string_lossy().into_owned())
        ));
        let mut text = String::from("feature,id,date,value,pixels\n");
        let mut summary = vec![];
        let mut at = 0;
        for (k, (id, px)) in feats.iter().enumerate() {
            let mut means = vec![f64::NAN; t_n];
            let mut counts = vec![0usize; t_n];
            for j in 0..px.len() {
                for (t, &v) in raw.pixel(at + j).iter().take(t_n).enumerate() {
                    if v.is_finite() {
                        means[t] = if counts[t] == 0 { 0.0 } else { means[t] } + f64::from(v);
                        counts[t] += 1;
                    }
                }
            }
            at += px.len();
            for t in 0..t_n {
                if counts[t] > 0 {
                    means[t] /= counts[t] as f64;
                }
                let v =
                    if means[t].is_finite() { format!("{}", (means[t] * 1e6).round() / 1e6) } else { String::new() };
                let id = id.replace('"', "\"\"");
                text += &format!("{},\"{id}\",{},{v},{}\n", k + 1, info.layers[t].label, counts[t]);
            }
            if summary.len() < 50 {
                let ok: Vec<f64> = means.iter().copied().filter(|v| v.is_finite()).collect();
                summary.push(json!({
                    "feature": k + 1,
                    "id": id,
                    "pixels": px.len(),
                    "dates_with_a_value": ok.len(),
                    "mean": (!ok.is_empty()).then(|| (ok.iter().sum::<f64>() / ok.len() as f64 * 1e4).round() / 1e4),
                }));
            }
        }
        std::fs::write(&csv, text).map_err(|e| format!("{}: {e}", csv.display()))?;
        Ok(json!({
            "csv": csv,
            "features": feats.len(),
            "dates": t_n,
            "value": shown.name,
            "first_features": summary,
            "seconds": (t0.elapsed().as_secs_f64() * 10.0).round() / 10.0,
        }))
    }
}
