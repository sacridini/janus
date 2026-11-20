//! The sun of each date of a series, for the tools that need its zenith and azimuth (the topographic correction): where
//! each date's value comes from and what a run reads. The sources, in the order `auto` tries them for a date:
//!
//! 1. the angle bands of the series (the roles `sza` and `saa`, HLS's and Landsat Collection 2's), per pixel;
//! 2. Sentinel-2 granule metadata (`MTD_TL.xml`, `granules::Angles`), per pixel;
//! 3. the date's STAC items (`STAC_SUN_ZENITH`, `STAC_SUN_AZIMUTH` in its VRT: `s2:mean_solar_*`, or
//!    `view:sun_elevation` and `view:sun_azimuth`);
//! 4. the Landsat MTL beside a local file (`SUN_ELEVATION`, `SUN_AZIMUTH`);
//! 5. the sun's position at the acquisition time over the window's centre (`zeit_core::solpos::sun_position`), where
//!    the time of day is known (the VRT's `STAC_DATETIME`, a time in the file's name, the MTL's centre time). A time is
//!    never assumed.
//!
//! Where none gives a sun the run stops and names the dates, unless `missing_sun` is `fixed`.

use std::path::Path;

use gdal::Metadata;
use janus_cube::CubeInfo;
use janus_cube::stac::{DATETIME_METADATA, SUN_AZIMUTH_METADATA, SUN_ZENITH_METADATA};
use serde::{Deserialize, Serialize};

use crate::pixel::Params;

/// What a date's files say of its sun: the zenith and azimuth of its STAC items and of its Landsat MTL (degrees), and
/// its acquisition time (ISO 8601, UTC).
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SunHint {
    #[serde(default)]
    pub stac: Option<[f64; 2]>,
    #[serde(default)]
    pub mtl: Option<[f64; 2]>,
    #[serde(default)]
    pub time: Option<String>,
}

/// Where a date's sun comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// the series' angle bands (per pixel)
    Bands,
    /// Sentinel-2's granule metadata (per pixel)
    Granules,
    /// one zenith and azimuth for the scene
    Scene { zenith: f64, azimuth: f64, from: &'static str },
    /// no source gave one
    Missing(String),
}

impl Source {
    /// The name the tables and the report give.
    pub fn name(&self) -> &str {
        match self {
            Source::Bands => "angle bands",
            Source::Granules => "Sentinel-2 granule metadata",
            Source::Scene { from, .. } => from,
            Source::Missing(_) => "none",
        }
    }
}

pub const FROM_STAC: &str = "STAC item";
pub const FROM_MTL: &str = "Landsat MTL";
pub const FROM_TIME: &str = "sun position at the acquisition time";
pub const FROM_TYPED: &str = "typed angles";

// ---------------------------------------------------------------------------------------------------------------
// What the files say
// ---------------------------------------------------------------------------------------------------------------

/// The value of `key` in a Landsat MTL, in its text form (`KEY = value`) or its XML (`<KEY>value</KEY>`).
fn mtl_value(text: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(at) = text[from..].find(key) {
        let start = from + at;
        let (before, after) = (&text[..start], &text[start + key.len()..]);
        from = start + key.len();
        // a whole word: not SUN_ELEVATION_X, not XSUN_ELEVATION
        if before.chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_')
            || after.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let rest = after.trim_start();
        let rest = rest.strip_prefix('=').or_else(|| rest.strip_prefix('>'))?;
        let value = rest.split(['\n', '\r', '<']).next()?.trim().trim_matches('"').trim();
        return (!value.is_empty()).then(|| value.to_string());
    }
    None
}

/// The sun and the acquisition time a Landsat MTL (`..._MTL.txt` or `.xml`) gives: `[zenith, azimuth]` (the zenith is
/// 90 minus `SUN_ELEVATION`) and the time (`DATE_ACQUIRED` and `SCENE_CENTER_TIME`, ISO 8601 UTC).
pub fn parse_mtl(text: &str) -> Option<([f64; 2], Option<String>)> {
    let elevation: f64 = mtl_value(text, "SUN_ELEVATION")?.parse().ok()?;
    let azimuth: f64 = mtl_value(text, "SUN_AZIMUTH")?.parse().ok()?;
    if !elevation.is_finite() || !azimuth.is_finite() {
        return None;
    }
    let time = mtl_value(text, "DATE_ACQUIRED").zip(mtl_value(text, "SCENE_CENTER_TIME")).and_then(|(d, t)| {
        let t: String = t.chars().take(8).collect();
        let iso = format!("{d}T{t}Z");
        janus_cube::stac::parse_time(&iso).map(|_| iso)
    });
    Some(([90.0 - elevation, azimuth], time))
}

/// The MTL of a Landsat file: `<product id>_MTL.txt` (or `.xml`) in its folder, the product id being the start of the
/// file's name (`LC09_L2SP_224078_20230723_20230725_02_T1_SR_B4.TIF`).
fn mtl_beside(path: &str) -> Option<String> {
    let p = Path::new(path);
    let name = p.file_name()?.to_str()?;
    for entry in std::fs::read_dir(p.parent()?).ok()?.flatten() {
        let n = entry.file_name().to_string_lossy().into_owned();
        let lower = n.to_ascii_lowercase();
        let Some(stem) = lower.strip_suffix("_mtl.txt").or_else(|| lower.strip_suffix("_mtl.xml")) else { continue };
        if !stem.is_empty() && name.to_ascii_lowercase().starts_with(stem) {
            return std::fs::read_to_string(entry.path()).ok();
        }
    }
    None
}

/// A time in a file's name (Sentinel-2's `20230723T132229`), as ISO 8601 UTC.
fn time_in_name(name: &str) -> Option<String> {
    let b = name.as_bytes();
    (0..b.len().saturating_sub(14)).find_map(|i| {
        let w = &b[i..i + 15];
        let digits = |r: std::ops::Range<usize>| w[r].iter().all(u8::is_ascii_digit);
        if !(digits(0..8) && w[8] == b'T' && digits(9..15)) {
            return None;
        }
        let s = std::str::from_utf8(w).ok()?;
        let iso = format!("{}-{}-{}T{}:{}:{}Z", &s[0..4], &s[4..6], &s[6..8], &s[9..11], &s[11..13], &s[13..15]);
        janus_cube::stac::parse_time(&iso).map(|_| iso)
    })
}

/// What each date's file says of its sun (for the dates of a stack, nothing).
pub fn of_series(info: &CubeInfo) -> Vec<SunHint> {
    (0..info.t())
        .map(|t| {
            let mut hint = SunHint::default();
            let Some(layer) = info.layers.get(t) else { return hint };
            if info.stack {
                return hint;
            }
            if let Ok(ds) = gdal::Dataset::open(&layer.path) {
                let get = |k: &str| ds.metadata_item(k, "").and_then(|v| v.trim().parse::<f64>().ok());
                hint.stac = get(SUN_ZENITH_METADATA).zip(get(SUN_AZIMUTH_METADATA)).map(|(z, a)| [z, a]);
                hint.time = ds.metadata_item(DATETIME_METADATA, "").filter(|t| !t.is_empty());
            }
            if !janus_cube::is_remote_path(&layer.path) {
                if let Some((sun, time)) = mtl_beside(&layer.path).and_then(|t| parse_mtl(&t)) {
                    hint.mtl = Some(sun);
                    hint.time = hint.time.or(time);
                }
                let name = Path::new(&layer.path).file_name().and_then(|n| n.to_str()).unwrap_or("");
                hint.time = hint.time.or_else(|| time_in_name(name));
            }
            hint
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The choice
// ---------------------------------------------------------------------------------------------------------------

/// The sun at `time` (ISO 8601 UTC) over a place (`lat`, `lon` in degrees): `[zenith, azimuth]`.
pub fn at_time(time: &str, lat: f64, lon: f64) -> Option<[f64; 2]> {
    let secs = janus_cube::stac::parse_time(time)?;
    let day = (secs / 86400.0).floor() as i64;
    let rest = (secs - day as f64 * 86400.0).round() as i32;
    let (y, m, d) = crate::numeric::civil(day);
    let (zenith, azimuth) = zeit_core::solpos::sun_position(
        lat,
        lon,
        y as i32,
        m as i32,
        d as i32,
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        0.0,
    );
    Some([zenith, azimuth])
}

/// What the run knows of the series to choose each date's sun.
pub struct Known<'a> {
    pub hints: &'a [SunHint],
    /// whether the angle bands `sza` and `saa` are mapped
    pub bands: bool,
    /// each date's Sentinel-2 granule metadata (empty where a date has none)
    pub granules: &'a [Vec<String>],
    /// the window's centre, latitude and longitude, for the sun's position
    pub centre: Option<(f64, f64)>,
}

/// Each date's sun by the parameter `sun` (and `missing_sun`, `sun_zenith`, `sun_azimuth`): a source for each of the
/// `n` dates, `Missing` (with why) where none gives one.
pub fn plan(p: &Params, known: &Known, n: usize) -> Vec<Source> {
    let typed = Source::Scene { zenith: p.f64("sun_zenith"), azimuth: p.f64("sun_azimuth"), from: FROM_TYPED };
    let mode = p.str("sun");
    (0..n)
        .map(|t| {
            let hint = known.hints.get(t).cloned().unwrap_or_default();
            let has_granules = known.granules.get(t).is_some_and(|g| !g.is_empty());
            let stac = || hint.stac.map(|[zenith, azimuth]| Source::Scene { zenith, azimuth, from: FROM_STAC });
            let mtl = || hint.mtl.map(|[zenith, azimuth]| Source::Scene { zenith, azimuth, from: FROM_MTL });
            let timed = || {
                let (lat, lon) = known.centre?;
                let [zenith, azimuth] = at_time(hint.time.as_deref()?, lat, lon)?;
                Some(Source::Scene { zenith, azimuth, from: FROM_TIME })
            };
            let found = match mode {
                "fixed" => Some(typed.clone()),
                "bands" => known.bands.then_some(Source::Bands),
                "sentinel2" => has_granules.then_some(Source::Granules),
                "stac" => stac().or_else(mtl).or_else(timed),
                _ => (known.bands.then_some(Source::Bands))
                    .or_else(|| has_granules.then_some(Source::Granules))
                    .or_else(stac)
                    .or_else(mtl)
                    .or_else(timed),
            };
            match found {
                Some(s) => s,
                None if p.str("missing_sun") == "fixed" => typed.clone(),
                None => Source::Missing(
                    match mode {
                        "bands" => "the bands sza and saa are not mapped",
                        "sentinel2" => "no Sentinel-2 granule metadata",
                        "stac" => "no STAC item, Landsat MTL or acquisition time",
                        _ => "no angle bands, granule metadata, STAC item, Landsat MTL or acquisition time",
                    }
                    .to_string(),
                ),
            }
        })
        .collect()
}

/// The error of a plan with dates left without a sun, naming them (the first five).
pub fn missing_error(plan: &[Source], labels: &[String]) -> Option<String> {
    let missing: Vec<String> = plan
        .iter()
        .enumerate()
        .filter(|(_, s)| matches!(s, Source::Missing(_)))
        .map(|(t, _)| labels.get(t).cloned().unwrap_or_else(|| format!("date {}", t + 1)))
        .collect();
    let why = plan.iter().find_map(|s| if let Source::Missing(w) = s { Some(w.clone()) } else { None })?;
    let shown: Vec<&str> = missing.iter().take(5).map(String::as_str).collect();
    let more =
        if missing.len() > shown.len() { format!(" and {} more", missing.len() - shown.len()) } else { String::new() };
    Some(format!(
        "ValueError: no sun for {}{more} ({why}): map the series' sza and saa bands, give the dates' STAC items or \
         Landsat MTL, or choose 'Use the typed angles' for a date without a sun",
        shown.join(", ")
    ))
}

/// The window's centre as latitude and longitude, from a north-up grid (`gt`) in the CRS `wkt`.
pub fn centre_lat_lon(wkt: &str, gt: &[f64; 6], window: [usize; 4]) -> Option<(f64, f64)> {
    use gdal::spatial_ref::{AxisMappingStrategy, CoordTransform, SpatialRef};
    let x = gt[0] + (window[0] + window[2]) as f64 / 2.0 * gt[1];
    let y = gt[3] + (window[1] + window[3]) as f64 / 2.0 * gt[5];
    let mut from = SpatialRef::from_wkt(wkt).ok()?;
    from.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let mut to = SpatialRef::from_epsg(4326).ok()?;
    to.set_axis_mapping_strategy(AxisMappingStrategy::TraditionalGisOrder);
    let t = CoordTransform::new(&from, &to).ok()?;
    let (mut xs, mut ys, mut zs) = ([x], [y], [0.0]);
    t.transform_coords(&mut xs, &mut ys, &mut zs).ok()?;
    (xs[0].is_finite() && ys[0].is_finite()).then_some((ys[0], xs[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn given(v: &[(&str, serde_json::Value)]) -> Params {
        Params::with_defaults("topocorr", &v.iter().map(|(k, x)| (k.to_string(), x.clone())).collect()).unwrap()
    }

    #[test]
    fn a_landsat_mtl_gives_the_zenith_and_the_time() {
        let text = "GROUP = LANDSAT_METADATA_FILE\n  GROUP = IMAGE_ATTRIBUTES\n    SUN_AZIMUTH = 56.38\n    SUN_ELEVATION = 30.16\n    SUN_ELEVATION_X = 1.0\n  END_GROUP = IMAGE_ATTRIBUTES\n  DATE_ACQUIRED = 2023-07-23\n  SCENE_CENTER_TIME = \"13:22:29.1234560Z\"\nEND_GROUP";
        let ([zenith, azimuth], time) = parse_mtl(text).unwrap();
        assert!((zenith - 59.84).abs() < 1e-9 && (azimuth - 56.38).abs() < 1e-9);
        assert_eq!(time.as_deref(), Some("2023-07-23T13:22:29Z"));
        // the XML form
        let xml = "<LANDSAT_METADATA_FILE><IMAGE_ATTRIBUTES><SUN_AZIMUTH>141.2</SUN_AZIMUTH><SUN_ELEVATION>38.5</SUN_ELEVATION></IMAGE_ATTRIBUTES></LANDSAT_METADATA_FILE>";
        let ([zenith, azimuth], time) = parse_mtl(xml).unwrap();
        assert!((zenith - 51.5).abs() < 1e-9 && (azimuth - 141.2).abs() < 1e-9 && time.is_none());
        assert!(parse_mtl("SUN_AZIMUTH = 1.0").is_none());
    }

    #[test]
    fn a_time_in_a_sentinel2_name_is_read() {
        assert_eq!(
            time_in_name("S2B_MSIL2A_20230723T132229_N0509_R038_T23LKC.tif").as_deref(),
            Some("2023-07-23T13:22:29Z")
        );
        assert_eq!(time_in_name("20230723.tif"), None);
    }

    /// 359 and 1 degrees are 0 on average, not 180.
    #[test]
    fn the_azimuth_mean_goes_round_the_circle() {
        let m = janus_cube::stac::circular_mean_deg(&[359.0, 1.0]).unwrap();
        assert!(m < 1e-9 || 360.0 - m < 1e-9, "{m}");
        assert!((janus_cube::stac::circular_mean_deg(&[90.0, 100.0]).unwrap() - 95.0).abs() < 1e-9);
    }

    /// The sun at a known instant: Rome at the June solstice, noon UTC: the sun is a little west of south, near its
    /// highest (zenith about 21 degrees by hand: cos z = sin lat sin dec + cos lat cos dec cos h, h = 12.2 degrees).
    #[test]
    fn the_sun_position_at_a_known_instant() {
        let [zenith, azimuth] = at_time("2023-06-21T12:00:00Z", 41.9, 12.5).unwrap();
        assert!((zenith - 21.1).abs() < 0.3, "{zenith}");
        assert!((150.0..230.0).contains(&azimuth), "{azimuth}");
        // the equator at the March equinox's noon: the zenith is near 0 (the longitude 0)
        let [zenith, _] = at_time("2023-03-20T12:00:00Z", 0.0, 0.0).unwrap();
        assert!(zenith < 3.0, "{zenith}");
    }

    /// bands > granules > STAC > MTL > the sun's position > an error naming the date.
    #[test]
    fn the_sources_are_tried_in_order() {
        let both =
            SunHint { stac: Some([40.0, 130.0]), mtl: Some([41.0, 131.0]), time: Some("2023-07-23T13:22:29Z".into()) };
        let mtl_only = SunHint { stac: None, mtl: Some([41.0, 131.0]), time: Some("2023-07-23T13:22:29Z".into()) };
        let time_only = SunHint { time: Some("2023-07-23T13:22:29Z".into()), ..Default::default() };
        let hints = vec![both.clone(), mtl_only, time_only, SunHint::default(), both];
        let granules: Vec<Vec<String>> = vec![vec![], vec![], vec![], vec![], vec!["a/MTD_TL.xml".into()]];
        let known = Known { hints: &hints, bands: false, granules: &granules, centre: Some((-22.0, -46.0)) };
        let plan = plan(&given(&[]), &known, 5);
        let from: Vec<&str> = plan.iter().map(Source::name).collect();
        assert_eq!(from[0], FROM_STAC);
        assert_eq!(from[1], FROM_MTL);
        assert_eq!(from[2], FROM_TIME);
        assert!(matches!(plan[3], Source::Missing(_)));
        assert_eq!(plan[4], Source::Granules);
        // the angle bands win over everything
        let known = Known { bands: true, ..known };
        assert!(super::plan(&given(&[]), &known, 5).iter().all(|s| *s == Source::Bands));
        // the error names the date
        let error = missing_error(
            &super::plan(&given(&[("sun", "stac".into())]), &Known { bands: false, ..known }, 5),
            &["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
        );
        assert!(error.unwrap().contains("for d "), "names the date");
        // missing_sun = fixed: the typed angles
        let known = Known { bands: false, ..known };
        let p = given(&[("missing_sun", "fixed".into()), ("sun_zenith", 33.0.into()), ("sun_azimuth", 100.0.into())]);
        assert_eq!(super::plan(&p, &known, 5)[3], Source::Scene { zenith: 33.0, azimuth: 100.0, from: FROM_TYPED });
        // fixed: the typed angles for all
        let p = given(&[("sun", "fixed".into())]);
        assert!(super::plan(&p, &known, 5).iter().all(|s| s.name() == FROM_TYPED));
    }

    #[test]
    fn a_local_landsat_file_finds_its_mtl_beside_it() {
        let dir = std::env::temp_dir().join("janus-zeit-test-sun-mtl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let product = "LC09_L2SP_224078_20230723_20230725_02_T1";
        std::fs::write(dir.join(format!("{product}_MTL.txt")), "SUN_AZIMUTH = 56.38\nSUN_ELEVATION = 30.16\n").unwrap();
        let text = mtl_beside(&dir.join(format!("{product}_SR_B4.TIF")).to_string_lossy()).unwrap();
        assert!(parse_mtl(&text).is_some());
        assert!(mtl_beside(&dir.join("other_SR_B4.TIF").to_string_lossy()).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
