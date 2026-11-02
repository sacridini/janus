//! Landsat and Sentinel-2 on one radiometric scale (Tools > Data preparation > Harmonize sensors; Zeit's
//! bandpass_adjust): a series that mixes sensors has a step wherever the sensor changes, since the bands of
//! Sentinel-2's MSI are not those of Landsat 8's OLI, and CCDC or BFAST read the step as a break. The bandpass
//! adjustment of HLS (Harmonized Landsat Sentinel-2, Claverie et al. 2018) removes it: a line per band and Sentinel-2
//! unit taking MSI's reflectance to OLI's. OLI is the reference, as in HLS: Landsat 8 and 9 stay as they are. ETM+
//! (Landsat 7) and TM (Landsat 4-5) are taken to OLI with the lines of Roy et al. (2016) only when asked: they were
//! fitted on pre-collection data, and Collection 2 surface reflectance is generally used across sensors without them.
//!
//! One series: every band of every date, those with a line adjusted, the others copied. Several series on one grid
//! (a Sentinel-2 search and a Landsat one, say): each adjusted, joined in time, the reflectance bands they share
//! named by their role, in the units of the first. Each date's sensor from its scene (the STAC item a series' VRT
//! records, else the file's name) or given. Written as Speckle filter writes: a COG a date.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use gdal::Metadata;
use janus_cube::CubeInfo;
use janus_cube::dates::civil_from_days;

use crate::regularize::{read_date, to_cogs};

/// A date's sensor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Sensor {
    S2A,
    S2B,
    S2C,
    /// Sentinel-2 of unknown unit: adjusted as Sentinel-2A
    S2,
    /// Landsat 8 and 9
    Oli,
    /// Landsat 7
    Etm,
    /// Landsat 4 and 5
    Tm,
}

impl Sensor {
    pub const ALL: [Sensor; 7] =
        [Sensor::S2A, Sensor::S2B, Sensor::S2C, Sensor::S2, Sensor::Oli, Sensor::Etm, Sensor::Tm];

    pub fn label(self) -> &'static str {
        match self {
            Sensor::S2A => "Sentinel-2A",
            Sensor::S2B => "Sentinel-2B",
            Sensor::S2C => "Sentinel-2C",
            Sensor::S2 => "Sentinel-2 (unit unknown)",
            Sensor::Oli => "Landsat 8-9 (OLI)",
            Sensor::Etm => "Landsat 7 (ETM+)",
            Sensor::Tm => "Landsat 4-5 (TM)",
        }
    }

    pub fn sentinel2(self) -> bool {
        matches!(self, Sensor::S2A | Sensor::S2B | Sensor::S2C | Sensor::S2)
    }
}

/// The sensor a name says: a STAC platform ("sentinel-2a", "landsat-8"), Earth Engine's SPACECRAFT_ID
/// ("LANDSAT_7"), a scene id or file name ("S2B_MSIL2A_...", "LC08_L2SP_...", "LT05_..."). None: none known.
pub fn sensor_of(name: &str) -> Option<Sensor> {
    let low = name.to_ascii_lowercase();
    // the letters and digits only ("sentinel-2a" and "Sentinel_2A" alike), and the words
    let compact: String = low.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    let words: Vec<&str> = low.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let first = words.first().copied().unwrap_or("");
    let word = |w: &str| words.contains(&w);
    for (s, unit) in [(Sensor::S2A, "a"), (Sensor::S2B, "b"), (Sensor::S2C, "c")] {
        if compact.contains(&format!("sentinel2{unit}")) || first == format!("s2{unit}") {
            return Some(s);
        }
    }
    if compact.contains("sentinel2") || first == "s2" || word("msi") {
        return Some(Sensor::S2);
    }
    let landsat = |n: &[&str]| n.iter().any(|n| compact.contains(&format!("landsat{n}")));
    if landsat(&["09", "9", "08", "8"])
        || ["lc09", "lc9", "lc08", "lc8", "lo08", "lo09"].contains(&first)
        || word("oli")
    {
        return Some(Sensor::Oli);
    }
    if landsat(&["07", "7"]) || ["le07", "le7"].contains(&first) || word("etm") {
        return Some(Sensor::Etm);
    }
    if landsat(&["05", "5", "04", "4"]) || ["lt05", "lt5", "lt04", "lt4", "lm05", "lm04"].contains(&first) || word("tm")
    {
        return Some(Sensor::Tm);
    }
    None
}

/// Where a date's sensor was read.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    Sensor(Sensor),
    /// a product of HLS: its Sentinel-2 dates already adjusted
    Hls,
    Unknown,
}

/// The sensor of date `t`: from the STAC items its VRT records (a series built from a search), else its file's name.
pub fn date_sensor(info: &CubeInfo, t: usize) -> Found {
    let Some(layer) = info.layers.get(t) else { return Found::Unknown };
    let mut names: Vec<String> = Vec::new();
    if let Ok(ds) = gdal::Dataset::open(&layer.path) {
        let collection = ds.metadata_item("STAC_COLLECTION", "").unwrap_or_default();
        if collection.to_ascii_lowercase().starts_with("hls") {
            return Found::Hls;
        }
        names.extend(ds.metadata_item("STAC_PLATFORM", ""));
        // the first item's id (the items of a day are of one sensor, nearly always)
        names.extend(ds.metadata_item("STAC_ITEMS", "").and_then(|s| s.split_whitespace().next().map(String::from)));
    }
    let file = Path::new(&layer.path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    names.push(file.clone());
    if file.to_ascii_uppercase().starts_with("HLS.") || names.iter().any(|n| n.to_ascii_uppercase().starts_with("HLS."))
    {
        return Found::Hls;
    }
    names.iter().find_map(|n| sensor_of(n)).map_or(Found::Unknown, Found::Sensor)
}

/// The bands HLS adjusts, in this order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    Coastal,
    Blue,
    Green,
    Red,
    /// MSI's narrow NIR, B8A (OLI's band 5)
    Nir,
    Swir1,
    Swir2,
}

impl Role {
    pub const ALL: [Role; 7] = [Role::Coastal, Role::Blue, Role::Green, Role::Red, Role::Nir, Role::Swir1, Role::Swir2];

    /// The band's name in a joined series (names janus_cube::roles knows again).
    pub fn label(self) -> &'static str {
        match self {
            Role::Coastal => "Coastal aerosol",
            Role::Blue => "Blue",
            Role::Green => "Green",
            Role::Red => "Red",
            Role::Nir => "NIR (narrow)",
            Role::Swir1 => "SWIR 1",
            Role::Swir2 => "SWIR 2",
        }
    }
}

/// HLS's bandpass adjustment, OLI = slope × MSI + intercept (reflectance 0-1), by Sentinel-2 unit: HLS v2.0 User
/// Guide, table 5 (S2A and S2B also the v1.4 values of Claverie et al. 2018); Zeit's zeit._bandpass.HLS.
const HLS_S2A: [(f64, f64); 7] = [
    (0.9959, -0.0002),
    (0.9778, -0.0040),
    (1.0053, -0.0009),
    (0.9765, 0.0009),
    (0.9983, -0.0001),
    (0.9987, -0.0011),
    (1.0030, -0.0012),
];
const HLS_S2B: [(f64, f64); 7] = [
    (0.9959, -0.0002),
    (0.9778, -0.0040),
    (1.0075, -0.0008),
    (0.9761, 0.0010),
    (0.9966, 0.0000),
    (1.0000, -0.0003),
    (0.9867, 0.0004),
];
const HLS_S2C: [(f64, f64); 7] = [
    (1.0030, 0.0000),
    (0.9851, -0.0027),
    (1.0038, -0.0009),
    (0.9718, 0.0011),
    (0.9995, -0.0003),
    (0.9994, -0.0007),
    (0.9910, 0.0004),
];

/// ETM+ (and TM, as Earth Engine's harmonization tutorial treats it) to OLI: Roy et al. (2016), table 2, surface
/// reflectance, OLI = intercept + slope × ETM+; no line for the coastal band.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EtmLines {
    /// reduced major axis
    Rma,
    /// ordinary least squares
    Ols,
}

const ROY_RMA: [(f64, f64); 6] =
    [(0.9785, -0.0095), (0.9542, -0.0016), (0.9825, -0.0022), (1.0073, -0.0021), (1.0171, -0.0030), (0.9949, 0.0029)];
const ROY_OLS: [(f64, f64); 6] =
    [(0.8474, 0.0003), (0.8483, 0.0088), (0.9047, 0.0061), (0.8462, 0.0412), (0.8937, 0.0254), (0.9071, 0.0172)];

/// The line (slope, intercept in reflectance) that takes `role` of `sensor` to OLI; None: left as it is.
pub fn line(sensor: Sensor, role: Role, etm: Option<EtmLines>) -> Option<(f64, f64)> {
    let k = Role::ALL.iter().position(|&r| r == role)?;
    match sensor {
        Sensor::S2A | Sensor::S2 => Some(HLS_S2A[k]),
        Sensor::S2B => Some(HLS_S2B[k]),
        Sensor::S2C => Some(HLS_S2C[k]),
        Sensor::Etm | Sensor::Tm => {
            let table = match etm? {
                EtmLines::Rma => &ROY_RMA,
                EtmLines::Ols => &ROY_OLS,
            };
            (k > 0).then(|| table[k - 1])
        }
        Sensor::Oli => None,
    }
}

/// A series' bands by role (0-based band of a date), and whether its NIR is MSI's broad one (B08), which HLS has no
/// line for: names as Janus' STAC series and Zeit's give them ("Red (B04)", "NIR (narrow) (B8A)", "nir08", "SR_B4"),
/// or an unnamed stack of 6 taken as blue to SWIR 2.
pub fn band_roles(info: &CubeInfo) -> (Vec<(Role, usize)>, bool) {
    let found = janus_cube::roles::guess_band_roles(info);
    let mut out: Vec<(Role, usize)> = Vec::new();
    for (role, key) in [
        (Role::Blue, "blue"),
        (Role::Green, "green"),
        (Role::Red, "red"),
        (Role::Nir, "nir"),
        (Role::Swir1, "swir1"),
        (Role::Swir2, "swir2"),
    ] {
        if let Some(&b) = found.get(key) {
            out.push((role, b as usize - 1));
        }
    }
    if let Some(b) = info.band_names.iter().position(|n| n.to_ascii_lowercase().contains("coastal"))
        && !out.iter().any(|&(_, x)| x == b)
    {
        out.insert(0, (Role::Coastal, b));
    }
    let broad = out.iter().find(|r| r.0 == Role::Nir).is_some_and(|&(_, b)| {
        let n = info.band_names.get(b).map(|s| s.to_ascii_lowercase()).unwrap_or_default();
        let narrow = n.contains("narrow") || n.contains("b8a") || n.contains("nir08");
        !narrow && (n.trim() == "nir" || n.contains("b08") || n.contains("(b8)") || n.trim() == "b8")
    });
    (out, broad)
}

/// A series to harmonize: its dates' sensors, and what its values are reflectance times (1, or 10000 for scaled
/// integers).
#[derive(Clone)]
pub struct Source {
    pub info: Arc<CubeInfo>,
    pub sensors: Vec<Sensor>,
    pub factor: f64,
}

/// The sensors of a series' dates, each read from its scene (`date_sensor`); `given` names every date's instead.
/// Refused: an HLS product (its Sentinel-2 dates already adjusted), dates of no sensor known (named).
pub fn sensors(info: &CubeInfo, given: Option<Sensor>) -> Result<Vec<Sensor>, String> {
    let found: Vec<Found> = (0..info.t()).map(|t| date_sensor(info, t)).collect();
    if found.contains(&Found::Hls) {
        return Err("the series is HLS, whose Sentinel-2 dates are already adjusted to OLI".into());
    }
    if let Some(s) = given {
        return Ok(vec![s; info.t()]);
    }
    let unknown: Vec<usize> = (0..info.t()).filter(|&t| found[t] == Found::Unknown).collect();
    if let Some(&t) = unknown.first() {
        return Err(format!(
            "{} of {} dates have no sensor in their scene or file name (the first: {}); say which it is",
            unknown.len(),
            info.t(),
            info.layers[t].label
        ));
    }
    Ok(found
        .into_iter()
        .map(|f| match f {
            Found::Sensor(s) => s,
            _ => unreachable!(),
        })
        .collect())
}

/// A series ready to harmonize: its dates' sensors (`sensors`) and its values' factor (`guess_factor`).
pub fn source(info: &Arc<CubeInfo>, given: Option<Sensor>) -> Result<Source, String> {
    Ok(Source { info: info.clone(), sensors: sensors(info, given)?, factor: guess_factor(info) })
}

/// What a series' values are reflectance times, as Zeit's scale="auto": 10000 if they go above 2 (the 99th
/// percentile of a window of 64 x 64 pixels in the middle of three dates, its reflectance bands), else 1.
pub fn guess_factor(info: &Arc<CubeInfo>) -> f64 {
    let (roles, _) = band_roles(info);
    let (w, h) = (info.width.min(64), info.height.min(64));
    let (x0, y0) = ((info.width - w) / 2, (info.height - h) / 2);
    let n = w * h;
    let bands = info.bands_per_date.max(1);
    let t_n = info.t();
    let mut values: Vec<f32> = Vec::new();
    let mut dates: Vec<usize> = [0, t_n / 2, t_n.saturating_sub(1)].into_iter().filter(|&t| t < t_n).collect();
    dates.dedup();
    for t in dates {
        let mut buf = vec![f32::NAN; bands * n];
        if read_date(info, t, true, (x0, y0, w, h), &mut buf) {
            for &(_, b) in &roles {
                values.extend(buf[b * n..(b + 1) * n].iter().filter(|v| v.is_finite()).map(|v| v.abs()));
            }
        }
    }
    if values.is_empty() {
        return if info.data_type.contains("Int") || info.data_type.contains("Byte") { 10000.0 } else { 1.0 };
    }
    let k = ((values.len() - 1) as f64 * 0.99).round() as usize;
    let (_, &mut p99, _) = values.select_nth_unstable_by(k, f32::total_cmp);
    if p99 > 2.0 { 10000.0 } else { 1.0 }
}

#[derive(Clone, Debug)]
pub struct HarmonizeOptions {
    /// Landsat 7 and 4-5: None left as they are, else Roy et al.'s lines
    pub etm: Option<EtmLines>,
    /// the window (x0, y0, x1, y1) in the series' pixels (None: all of it)
    pub window: Option<[usize; 4]>,
    /// the files' names: `<stem>_<YYYYMMDD>.tif`
    pub stem: String,
    pub threads: usize,
}

/// What `harmonize` wrote.
#[derive(Debug)]
pub struct Harmonized {
    pub files: Vec<String>,
    /// the bands written, by name
    pub bands: Vec<String>,
    /// how many dates each sensor had, and what was done
    pub notes: Vec<String>,
}

/// An output band: where it is read in a date of each source, and its role.
struct OutBand {
    name: String,
    role: Option<Role>,
    /// the band (0-based) in each source
    from: Vec<usize>,
}

/// The day's text, YYYYMMDD.
fn ymd(time: f64) -> String {
    let (y, m, d) = civil_from_days((time / 86400.0).floor() as i64);
    format!("{y:04}{m:02}{d:02}")
}

/// Whether two series have the same grid: size, geotransform and CRS.
pub fn same_grid(a: &CubeInfo, b: &CubeInfo) -> bool {
    let gt = match (a.geo_transform, b.geo_transform) {
        (Some(x), Some(y)) => x.iter().zip(&y).all(|(p, q)| (p - q).abs() <= 1e-9 * p.abs().max(q.abs()).max(1.0)),
        (None, None) => true,
        _ => false,
    };
    let crs = if !a.crs_authority.is_empty() || !b.crs_authority.is_empty() {
        a.crs_authority == b.crs_authority
    } else {
        a.crs_wkt == b.crs_wkt
    };
    a.width == b.width && a.height == b.height && gt && crs
}

/// Harmonizes `sources` (one series, or several on one grid joined in time) into `out_dir`: a COG a date
/// (`progress` from 0 to 1 with a message).
pub fn harmonize(
    sources: &[Source],
    opts: &HarmonizeOptions,
    out_dir: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Harmonized, String> {
    let first = sources.first().ok_or("no series")?;
    for s in sources {
        if !s.info.time_is_date {
            return Err("a series without dates".into());
        }
        if s.sensors.len() != s.info.t() {
            return Err(format!("{} sensors for {} dates", s.sensors.len(), s.info.t()));
        }
        if !same_grid(&first.info, &s.info) {
            return Err("the series are not on one grid: reproject one onto the other's \
                        (Tools > Data preparation > Reproject, resample, clip, to a layer's grid)"
                .into());
        }
    }
    let roles: Vec<(Vec<(Role, usize)>, bool)> = sources.iter().map(|s| band_roles(&s.info)).collect();
    let mut notes: Vec<String> = Vec::new();
    // the bands written
    let bands: Vec<OutBand> = if sources.len() == 1 {
        let info = &first.info;
        (0..info.bands_per_date.max(1))
            .map(|b| OutBand {
                name: info.band_names.get(b).cloned().unwrap_or_else(|| format!("band {}", b + 1)),
                role: roles[0].0.iter().find(|r| r.1 == b).map(|r| r.0),
                from: vec![b],
            })
            .collect()
    } else {
        let common: Vec<Role> =
            Role::ALL.into_iter().filter(|r| roles.iter().all(|(found, _)| found.iter().any(|f| f.0 == *r))).collect();
        if common.is_empty() {
            return Err("the series share no reflectance band (blue, green, red, NIR, SWIR 1, SWIR 2)".into());
        }
        let any_broad = roles.iter().any(|r| r.1);
        common
            .into_iter()
            .map(|r| OutBand {
                name: if r == Role::Nir && any_broad { "NIR".into() } else { r.label().into() },
                role: Some(r),
                from: roles.iter().map(|(found, _)| found.iter().find(|f| f.0 == r).unwrap().1).collect(),
            })
            .collect()
    };
    if bands.iter().all(|b| b.role.is_none()) {
        return Err(format!(
            "no band of the series is one HLS adjusts (its bands: {}); name them blue, green, red, NIR, SWIR 1, \
             SWIR 2",
            first.info.band_names.join(", ")
        ));
    }
    // the dates, of every source, in time
    let mut dates: Vec<(usize, usize)> =
        sources.iter().enumerate().flat_map(|(k, s)| (0..s.info.t()).map(move |t| (k, t))).collect();
    dates.sort_by(|a, b| sources[a.0].info.layers[a.1].time.total_cmp(&sources[b.0].info.layers[b.1].time));
    // each date's lines: slope and intercept (in the output's units) of each output band
    let out_factor = first.factor;
    let mut counts: std::collections::BTreeMap<&'static str, usize> = Default::default();
    let mut broad_skipped = false;
    let lines: Vec<Vec<(f32, f32)>> = dates
        .iter()
        .map(|&(k, t)| {
            let s = &sources[k];
            let sensor = s.sensors[t];
            *counts.entry(sensor.label()).or_default() += 1;
            let rescale = out_factor / s.factor;
            bands
                .iter()
                .map(|b| {
                    let l = b.role.and_then(|r| {
                        if r == Role::Nir && sensor.sentinel2() && roles[k].1 {
                            broad_skipped = true;
                            return None;
                        }
                        line(sensor, r, opts.etm)
                    });
                    let (a, c) = l.unwrap_or((1.0, 0.0));
                    // (a v / f + c) f' = a f'/f v + c f'
                    ((a * rescale) as f32, (c * out_factor) as f32)
                })
                .collect()
        })
        .collect();
    notes.push(format!("dates: {}", counts.iter().map(|(s, n)| format!("{s} {n}")).collect::<Vec<_>>().join(", ")));
    if dates.iter().any(|&(k, t)| sources[k].sensors[t].sentinel2()) {
        notes.push("Sentinel-2 -> OLI: HLS v2.0 bandpass adjustment (Claverie et al. 2018)".into());
    }
    if dates.iter().any(|&(k, t)| sources[k].sensors[t] == Sensor::S2) {
        notes.push("Sentinel-2 dates of unknown unit (A, B or C) adjusted as Sentinel-2A".into());
    }
    if broad_skipped {
        notes.push(
            "the Sentinel-2 NIR is the broad one (B08), which HLS has no line for: left as it is (search with B8A, \
             the narrow NIR, to adjust it)"
                .into(),
        );
    }
    if dates.iter().any(|&(k, t)| matches!(sources[k].sensors[t], Sensor::Etm | Sensor::Tm)) {
        notes.push(match opts.etm {
            Some(EtmLines::Rma) => "ETM+/TM -> OLI: Roy et al. (2016), reduced major axis".into(),
            Some(EtmLines::Ols) => "ETM+/TM -> OLI: Roy et al. (2016), least squares".into(),
            None => "Landsat 7 and 4-5 left as they are (Collection 2 is used across sensors as it is)".into(),
        });
    }
    if sources.iter().any(|s| s.factor != out_factor) {
        notes.push(format!("values in the units of the first series (reflectance x {out_factor})"));
    }

    let win = opts.window.unwrap_or([0, 0, first.info.width, first.info.height]);
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    if w == 0 || h == 0 {
        return Err("an empty window".into());
    }
    let part = out_dir.join(".part");
    std::fs::create_dir_all(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let gt = first.info.geo_transform.map(|g| {
        let (x0, y0) = (win[0] as f64, win[1] as f64);
        [g[0] + x0 * g[1] + y0 * g[2], g[1], g[2], g[3] + x0 * g[4] + y0 * g[5], g[4], g[5]]
    });
    // the files' names (two dates of one day keep both)
    let mut temps: Vec<PathBuf> = Vec::with_capacity(dates.len());
    for &(k, t) in &dates {
        let day = ymd(sources[k].info.layers[t].time);
        let mut f = part.join(format!("{}_{day}.tif", opts.stem));
        let mut n = 2;
        while temps.contains(&f) {
            f = part.join(format!("{}_{day}_{n}.tif", opts.stem));
            n += 1;
        }
        temps.push(f);
    }
    // each date on a thread: read and written in strips of rows
    let nb = bands.len();
    let rows = ((256usize << 20)
        / (w * 4 * (nb + sources.iter().map(|s| s.info.bands_per_date.max(1)).max().unwrap_or(1))).max(1))
    .clamp(1, 1024)
    .min(h);
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let failed: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let crs = &first.info.crs_wkt;
    std::thread::scope(|s| {
        for _ in 0..opts.threads.max(1).min(dates.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= dates.len() || cancel.load(Ordering::Relaxed) || failed.lock().unwrap().is_some() {
                        break;
                    }
                    let (k, t) = dates[i];
                    let r = write_date(
                        &sources[k].info,
                        t,
                        k,
                        &bands,
                        &lines[i],
                        (win, w, h, rows),
                        gt,
                        crs,
                        &temps[i],
                        cancel,
                    );
                    if let Err(e) = r {
                        failed.lock().unwrap().get_or_insert(e);
                        break;
                    }
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    progress(0.8 * d as f64 / dates.len() as f64, &format!("{d} of {} dates", dates.len()));
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    let files = to_cogs(&temps, out_dir, opts.threads, &|f| progress(0.8 + 0.2 * f, "writing the COGs"), cancel)?;
    let _ = std::fs::remove_dir_all(&part);
    Ok(Harmonized { files, bands: bands.iter().map(|b| b.name.clone()).collect(), notes })
}

/// Date `t` of a source through its lines into the GeoTIFF `path`, strip by strip.
#[allow(clippy::too_many_arguments)]
fn write_date(
    info: &Arc<CubeInfo>,
    t: usize,
    k: usize,
    bands: &[OutBand],
    lines: &[(f32, f32)],
    (win, w, h, rows): ([usize; 4], usize, usize, usize),
    gt: Option<[f64; 6]>,
    crs: &str,
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
    // uncompressed: read once more by the COGs' writing, which compresses
    let co = gdal::raster::RasterCreationOptions::from_iter(["TILED=YES", "BIGTIFF=IF_SAFER", "SPARSE_OK=TRUE"]);
    let mut ds = drv
        .create_with_band_type_with_options::<f32, _>(path, w, h, bands.len(), &co)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(gt) = gt {
        ds.set_geo_transform(&gt).map_err(|e| e.to_string())?;
    }
    if !crs.is_empty() {
        ds.set_projection(crs).map_err(|e| e.to_string())?;
    }
    for (b, ob) in bands.iter().enumerate() {
        let mut rb = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
        rb.set_no_data_value(Some(f64::NAN)).map_err(|e| e.to_string())?;
        rb.set_description(&ob.name).map_err(|e| e.to_string())?;
    }
    let src_bands = info.bands_per_date.max(1);
    for y0 in (0..h).step_by(rows) {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let r = rows.min(h - y0);
        let n = w * r;
        let mut values = vec![f32::NAN; src_bands * n];
        if !read_date(info, t, true, (win[0], win[1] + y0, w, r), &mut values) {
            values.fill(f32::NAN);
        }
        for (b, ob) in bands.iter().enumerate() {
            let src = ob.from[k.min(ob.from.len() - 1)];
            let (a, c) = lines[b];
            let out: Vec<f32> = values[src * n..(src + 1) * n]
                .iter()
                .map(|&v| if v.is_finite() { a * v + c } else { f32::NAN })
                .collect();
            let mut rb = ds.rasterband(b + 1).map_err(|e| e.to_string())?;
            let mut buf = gdal::raster::Buffer::new((w, r), out);
            rb.write((0, y0 as isize), (w, r), &mut buf).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensors_from_names() {
        for (name, s) in [
            ("sentinel-2a", Sensor::S2A),
            ("Sentinel_2B", Sensor::S2B),
            ("S2C_MSIL2A_20250101T133211_N0511_R081_T22LGQ", Sensor::S2C),
            ("S2B_23KPQ_20200701_0_L2A", Sensor::S2B),
            ("sentinel-2", Sensor::S2),
            ("LC08_L2SP_221071_20200101_20200823_02_T1", Sensor::Oli),
            ("LC09_L2SP_221071_20220101_20220123_02_T1_SR.tif", Sensor::Oli),
            ("landsat-9", Sensor::Oli),
            ("LANDSAT_8", Sensor::Oli),
            ("LANDSAT_7", Sensor::Etm),
            ("LE07_L2SP_221071_20010101", Sensor::Etm),
            ("landsat-5", Sensor::Tm),
            ("LT05_L2SP_221071_19950101", Sensor::Tm),
        ] {
            assert_eq!(sensor_of(name), Some(s), "{name}");
        }
        for name in ["ndvi_20200101.tif", "s1_20230101.tif", "S1A_IW_GRDH", "mosaic_2020"] {
            assert_eq!(sensor_of(name), None, "{name}");
        }
    }

    #[test]
    fn lines_as_zeit() {
        assert_eq!(line(Sensor::S2A, Role::Red, None), Some((0.9765, 0.0009)));
        assert_eq!(line(Sensor::S2, Role::Blue, None), line(Sensor::S2A, Role::Blue, None));
        assert_eq!(line(Sensor::S2B, Role::Swir2, None), Some((0.9867, 0.0004)));
        assert_eq!(line(Sensor::S2C, Role::Coastal, None), Some((1.0030, 0.0)));
        assert_eq!(line(Sensor::Oli, Role::Red, Some(EtmLines::Rma)), None);
        assert_eq!(line(Sensor::Etm, Role::Red, None), None);
        assert_eq!(line(Sensor::Etm, Role::Nir, Some(EtmLines::Rma)), Some((1.0073, -0.0021)));
        assert_eq!(line(Sensor::Tm, Role::Blue, Some(EtmLines::Ols)), Some((0.8474, 0.0003)));
        assert_eq!(line(Sensor::Etm, Role::Coastal, Some(EtmLines::Ols)), None);
    }

    fn named(names: &[&str]) -> CubeInfo {
        CubeInfo {
            band_names: names.iter().map(|s| s.to_string()).collect(),
            bands_per_date: names.len(),
            ..Default::default()
        }
    }

    #[test]
    fn roles_and_the_broad_nir() {
        // Planetary Computer's Sentinel-2 preset: B08, the broad NIR
        let (r, broad) = band_roles(&named(&[
            "Blue (B02)",
            "Green (B03)",
            "Red (B04)",
            "NIR (B08)",
            "SWIR 1 (B11)",
            "SWIR 2 (B12)",
            "SCL (scene classification)",
        ]));
        assert_eq!(r.len(), 6);
        assert!(broad);
        assert_eq!(r.iter().find(|x| x.0 == Role::Swir2).unwrap().1, 5);
        // Earth Search's: "nir" is B08
        assert!(band_roles(&named(&["Blue", "Green", "Red", "NIR", "SWIR 1", "SWIR 2"])).1);
        // B8A, and Landsat's
        assert!(!band_roles(&named(&["Blue (B02)", "Red (B04)", "NIR (narrow) (B8A)"])).1);
        let (r, broad) = band_roles(&named(&[
            "Coastal aerosol",
            "Blue",
            "Green",
            "Red",
            "NIR (narrow)",
            "SWIR 1",
            "SWIR 2",
            "QA_PIXEL",
        ]));
        assert!(!broad);
        assert_eq!(r[0], (Role::Coastal, 0));
        assert_eq!(r.len(), 7);
    }

    /// A Sentinel-2A series (reflectance x 10000) joined with a Landsat 8 one: the Sentinel-2 dates through HLS's
    /// lines, Landsat's as they were, in time, the bands by role; then the Sentinel-2 series alone, every band kept.
    #[test]
    fn harmonizes_and_joins() {
        let dir = std::env::temp_dir().join(format!("janus_harmonize_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wkt = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap().to_wkt().unwrap();
        let (w, h) = (40usize, 30usize);
        let mk = |sub: &str, name: &str, bands: &[(&str, f32)]| -> String {
            let d = dir.join(sub);
            std::fs::create_dir_all(&d).unwrap();
            let p = d.join(name);
            let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
            let mut ds = drv.create_with_band_type::<f32, _>(&p, w, h, bands.len()).unwrap();
            ds.set_geo_transform(&[600000.0, 30.0, 0.0, 8000000.0, 0.0, -30.0]).unwrap();
            ds.set_projection(&wkt).unwrap();
            for (b, (n, v)) in bands.iter().enumerate() {
                let mut rb = ds.rasterband(b + 1).unwrap();
                rb.set_description(n).unwrap();
                let mut vals = vec![*v; w * h];
                vals[0] = f32::NAN;
                let mut buf = gdal::raster::Buffer::new((w, h), vals);
                rb.write((0, 0), (w, h), &mut buf).unwrap();
            }
            p.to_string_lossy().into_owned()
        };
        let s2b =
            [("Blue (B02)", 800.0), ("Red (B04)", 1200.0), ("NIR (narrow) (B8A)", 3000.0), ("Red edge (B05)", 1500.0)];
        let s2: Vec<String> =
            ["S2A_MSIL2A_20230110.tif", "S2A_MSIL2A_20230130.tif"].iter().map(|n| mk("s2", n, &s2b)).collect();
        let l8b = [("Blue", 0.08), ("Green", 0.09), ("Red", 0.12), ("NIR (narrow)", 0.30)];
        let l8: Vec<String> =
            ["LC08_L2SP_20230105.tif", "LC08_L2SP_20230121.tif"].iter().map(|n| mk("l8", n, &l8b)).collect();
        let s2i = Arc::new(janus_cube::open_cube(&s2, &Default::default()).unwrap());
        let l8i = Arc::new(janus_cube::open_cube(&l8, &Default::default()).unwrap());
        assert_eq!(date_sensor(&s2i, 0), Found::Sensor(Sensor::S2A));
        assert_eq!(date_sensor(&l8i, 1), Found::Sensor(Sensor::Oli));
        assert_eq!((guess_factor(&s2i), guess_factor(&l8i)), (10000.0, 1.0));
        let src = |info: &Arc<CubeInfo>, s: Sensor, f: f64| Source {
            info: info.clone(),
            sensors: vec![s; info.t()],
            factor: f,
        };
        let none = AtomicBool::new(false);
        let opts = HarmonizeOptions { etm: None, window: None, stem: "h".into(), threads: 2 };
        let read = |p: &str, b: usize| -> (f32, f32) {
            let d = gdal::Dataset::open(p).unwrap();
            let v = d.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap();
            (v.data()[0], v.data()[5])
        };
        // joined: the Landsat series first, its units (reflectance) for all
        let out = harmonize(
            &[src(&l8i, Sensor::Oli, 1.0), src(&s2i, Sensor::S2A, 10000.0)],
            &opts,
            &dir.join("joined"),
            &|_, _| {},
            &none,
        )
        .unwrap();
        assert_eq!(out.bands, ["Blue", "Red", "NIR (narrow)"]);
        assert_eq!(out.files.len(), 4);
        let days: Vec<&str> = out.files.iter().map(|f| &f[f.len() - 12..f.len() - 4]).collect();
        assert_eq!(days, ["20230105", "20230110", "20230121", "20230130"]);
        let (nan, red) = read(&out.files[1], 2);
        assert!(nan.is_nan());
        assert!((red - (0.9765 * 0.12 + 0.0009)).abs() < 1e-6, "{red}");
        let (_, nir) = read(&out.files[1], 3);
        assert!((nir - (0.9983 * 0.30 - 0.0001)).abs() < 1e-6, "{nir}");
        assert!((read(&out.files[0], 2).1 - 0.12).abs() < 1e-7, "Landsat 8 as it was");
        let d = gdal::Dataset::open(&out.files[0]).unwrap();
        assert_eq!(d.rasterband(3).unwrap().description().unwrap(), "NIR (narrow)");
        // Sentinel-2 alone, in its units: every band, the red edge copied
        let out = harmonize(&[src(&s2i, Sensor::S2B, 10000.0)], &opts, &dir.join("one"), &|_, _| {}, &none).unwrap();
        assert_eq!(out.bands.len(), 4);
        let red = read(&out.files[0], 2).1;
        assert!((red - (0.9761 * 1200.0 + 0.0010 * 10000.0)).abs() < 1e-3, "{red}");
        assert_eq!(read(&out.files[0], 4).1, 1500.0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
