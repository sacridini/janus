//! Analysis-ready data (IDEIAS.md, phase 57): Tools > Analysis-ready data (FORCE). FORCE's Level 2 is a chain: the
//! reflectance of each date brought to nadir view (NBAR), the sensors put on one spectral response (the adjustment
//! to Sentinel-2A), and from that harmonized series a product (Tasseled Cap) or a sharper one (ImproPhe). This window
//! only orchestrates the four tools that exist (`chain::run_step`, with the parameters their own windows edit): no
//! computation of its own. The chain is one task (Tools > Tasks) in a folder of its own with a subfolder per step.
//!
//! What this chain has that the Time series analysis' has not: NBAR and the spectral adjustment write **one series
//! per band**, and the step after them reads **one series of several bands a date**. Between the steps `assemble`
//! makes the series of the bands one VRT a date (band k the k-th series' band of the date, its description the role
//! it plays, the date's quality band of the raw series as it is, the date's STAC metadata and file name, so that the
//! sensors and granules of each date are found as they are in the raw series): the harmonized series, which is also
//! an output of the chain that the user opens and uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use egui::{ComboBox, RichText, Ui};
use gdal::Metadata;
use janus_cube::{BandSelection, CubeInfo, QaRule};
use janus_zeit::job::JobResult;
use janus_zeit::manifest::{Tool, manifest};
use serde_json::{Value, json};

use crate::app::State;
use crate::chain::{Chain, ChainInput, ChainStep, JobStart, StepRun, cancelled, run_step, spawn_job};
use crate::provenance::{OutputRecord, Provenance, SCRIPT, SeriesRecord};
use crate::tsa::Advice;
use crate::zeit::{JobState, Preview, applicability};

/// The id of the chain's task in Tools > Tasks (not a tool of the manifest).
pub(crate) const JOB_TOOL: &str = "ard";
/// The id of the preview's task (the chain on the ROI, its folder removed with the preview).
pub(crate) const PREVIEW_JOB: &str = "ard-preview";

/// The steps in the order of the chain: the tool, the window's name for it and what it does.
pub(crate) const STEPS: [(&str, &str, &str); 4] = [
    (
        "nbar",
        "NBAR (c-factor)",
        "Each band brought to nadir view and a normalized sun, by the angles of each date: the series of every band.",
    ),
    (
        "spectral_adjust",
        "Spectral adjustment to Sentinel-2A",
        "Landsat or MODIS reflectance on the spectral response of Sentinel-2A (ten bands): with NBAR, the harmonized \
         series.",
    ),
    (
        "tasseled_cap",
        "Tasseled Cap",
        "Brightness, greenness and wetness of the harmonized series' six reflective bands: a series each.",
    ),
    (
        "improphe",
        "ImproPhe",
        "Sharpens another open series (picked below) to the pixels of the harmonized series, the fine one.",
    ),
];
const SCOPES: [&str; 3] = ["Whole image", "Visible area", "ROI"];

/// The roles Tasseled Cap and the spectral adjustment need.
const SIX: [&str; 6] = ["blue", "green", "red", "nir", "swir1", "swir2"];
/// The angle roles NBAR reads from bands.
const ANGLES: [&str; 4] = ["sza", "saa", "vza", "vaa"];
/// The roles NBAR has parameters for, in the order of its outputs (zeit_core::brdf::ROLES).
const NBAR_ROLES: [&str; 11] =
    ["coastal", "blue", "green", "red", "rededge1", "rededge2", "rededge3", "nir", "nir08", "swir1", "swir2"];
/// What the spectral adjustment writes: its output's id and the description of that band in the harmonized series.
/// Its NIR is the narrow one (B8A, the NIR of FORCE's indices and Tasseled Cap, as OLI's band 5), which the role `nir`
/// maps to; the broad one (B8) follows it under a name that no role takes.
const HARMONIZED: [(&str, &str); 10] = [
    ("blue", "blue"),
    ("green", "green"),
    ("red", "red"),
    ("rededge1", "rededge1"),
    ("rededge2", "rededge2"),
    ("rededge3", "rededge3"),
    ("nir08", "nir"),
    ("nir", "nir broad"),
    ("swir16", "swir1"),
    ("swir22", "swir2"),
];
/// The metadata of a date's file the tools read per date: its sensor (STAC platform and items), the Sentinel-2
/// granule metadata and what signs a Planetary Computer file.
const DATE_METADATA: [&str; 5] = [
    "STAC_PLATFORM",
    "STAC_ITEMS",
    "STAC_COLLECTION",
    janus_cube::stac::GRANULE_METADATA,
    janus_cube::stac::SAS_METADATA,
];

/// The window's state: which steps are on, the tab, the scope of the run.
pub struct ArdUi {
    pub open: bool,
    pub tab: usize,
    /// each step of STEPS on or off
    pub on: [bool; 4],
    /// 0 whole image, 1 visible area, 2 ROI
    pub scope: usize,
    /// the series the last chain wrote (name, file), filled when it ends
    pub series_out: Arc<Mutex<Vec<(String, String)>>>,
    /// the chain's preview on the ROI, while there is one
    pub preview: Option<ArdPreview>,
    /// when Copy Python last copied (for its "Copied")
    pub copied: Option<Instant>,
    /// the folders of previews turned off while their harmonized series was still being opened: the layer is removed
    /// when it arrives (`ard_pump_preview`)
    pub orphans: Vec<String>,
}

/// The chain run on the ROI into a folder of its own, and whether the curtain of before and after is on it yet.
pub struct ArdPreview {
    /// the preview's folder (the zeit preview's `dir` too)
    pub dir: String,
    /// the series the chain wrote (name, file), filled when it ends
    pub written: Arc<Mutex<Vec<(String, String)>>>,
    /// the curtain was put on the harmonized series
    pub curtain: bool,
}

/// The name of the series the chain leaves, as its output is called.
pub(crate) const HARMONIZED_NAME: &str = "Harmonized series";

impl Default for ArdUi {
    fn default() -> ArdUi {
        // NBAR and the spectral adjustment: the harmonized series
        ArdUi {
            open: false,
            tab: 0,
            on: [true, true, false, false],
            scope: 0,
            series_out: Arc::default(),
            preview: None,
            copied: None,
            orphans: vec![],
        }
    }
}

/// What the chain starts from, and the coarse series ImproPhe sharpens (the one its form picks: its name and series).
pub(crate) struct ArdInput {
    pub chain: ChainInput,
    pub coarse: Option<(String, Arc<CubeInfo>)>,
}

// ---------------------------------------------------------------------------------------------------------------
// What a step can take
// ---------------------------------------------------------------------------------------------------------------

fn missing<'a>(roles: &BTreeMap<String, i32>, wanted: &[&'a str]) -> Vec<&'a str> {
    wanted.iter().copied().filter(|r| !roles.contains_key(*r)).collect()
}

/// Why a step cannot run on a series with these roles, if it cannot: the tool's own rule, the bands it needs mapped,
/// and for ImproPhe the coarse series.
pub(crate) fn problem(
    tool: &Tool,
    info: &CubeInfo,
    roles: &BTreeMap<String, i32>,
    params: &BTreeMap<String, Value>,
    coarse: Option<&CubeInfo>,
) -> Option<String> {
    if let Some(why) = applicability(tool, info) {
        return Some(why);
    }
    match tool.id.as_str() {
        "nbar" => {
            // the angles of Sentinel-2's metadata are not bands
            let absent = missing(roles, &ANGLES);
            if params.get("angles").and_then(Value::as_str) != Some("sentinel2") && !absent.is_empty() {
                return Some(format!("map the angle bands first: {}", absent.join(", ")));
            }
            if NBAR_ROLES.iter().all(|r| !roles.contains_key(*r)) {
                return Some(format!("map at least one band to normalize ({})", NBAR_ROLES.join(", ")));
            }
        }
        "spectral_adjust" | "tasseled_cap" => {
            let absent = missing(roles, &SIX);
            if !absent.is_empty() {
                return Some(format!("choose the band of: {}", absent.join(", ")));
            }
        }
        "improphe" => match coarse {
            None => return Some("choose the coarse series in its form (another open series)".into()),
            Some(c) if !crate::layers::same_crs(info, c) => {
                return Some("the coarse series is in another CRS than the series".into());
            }
            Some(_) => {}
        },
        _ => {}
    }
    None
}

/// The bands (their descriptions in the harmonized series) a step of the chain writes as series, if it is one of the
/// two that do, from the roles it reads.
fn written_bands(id: &str, roles: &BTreeMap<String, i32>) -> Option<Vec<String>> {
    match id {
        "nbar" => Some(NBAR_ROLES.iter().filter(|r| roles.contains_key(**r)).map(|r| r.to_string()).collect()),
        "spectral_adjust" => Some(HARMONIZED.iter().map(|h| h.1.to_string()).collect()),
        _ => None,
    }
}

/// The name of the quality band in the harmonized series: the raw series' band's own (which the Open dialog
/// recognises, "QA_PIXEL"), unless it reads as a band role.
fn qa_name(raw: &CubeInfo) -> String {
    let name = raw.band_names.get(raw.sel.qa_band.max(1) as usize - 1).cloned().unwrap_or_default();
    let low = name.to_lowercase();
    if name.trim().is_empty() || ["blue", "green", "red", "nir", "swir", "thermal"].iter().any(|r| low.contains(r)) {
        "QA".into()
    } else {
        name
    }
}

/// Whether the raw series' quality band goes to the harmonized one.
fn keeps_quality(raw: &CubeInfo) -> bool {
    raw.sel.qa_band > 0 && raw.sel.qa_rule != QaRule::None
}

/// The selection of the harmonized series: its quality band (the one after the bands) with the raw series' rule.
fn harmonized_selection(raw: &CubeInfo, bands: usize) -> BandSelection {
    let qa = keeps_quality(raw);
    BandSelection {
        band: 1,
        qa_band: if qa { bands as i32 + 1 } else { 0 },
        qa_rule: if qa { raw.sel.qa_rule } else { QaRule::None },
        chosen: true,
        ..Default::default()
    }
}

fn roles_of(info: &CubeInfo) -> BTreeMap<String, i32> {
    crate::zeit::guess_roles(info)
}

/// The series a step would leave, judged without running it: the raw series with the bands `descs` (and its quality
/// band), its roles as the harmonized series' band names give them.
fn virtual_series(raw: &CubeInfo, descs: &[String]) -> (CubeInfo, BTreeMap<String, i32>) {
    let mut info = raw.clone();
    info.band_names = descs.to_vec();
    if keeps_quality(raw) {
        info.band_names.push(qa_name(raw));
    }
    info.bands_per_date = info.band_names.len();
    info.sel = harmonized_selection(raw, descs.len());
    info.expr = None;
    let roles = roles_of(&info);
    (info, roles)
}

/// The steps the series cannot take, with why, judged before the chain runs: each step on the series the ones before
/// it would leave (those that cannot run leave it as it was).
pub(crate) fn left_out(
    steps: &[ChainStep],
    info: &CubeInfo,
    roles: &BTreeMap<String, i32>,
    coarse: Option<&CubeInfo>,
) -> Vec<(String, String)> {
    let mut current = (info.clone(), roles.clone());
    let mut out = vec![];
    for s in steps {
        if let Some(why) = problem(s.tool, &current.0, &current.1, &s.params, coarse) {
            out.push((s.tool.id.clone(), why));
        } else if let Some(descs) = written_bands(&s.tool.id, &current.1) {
            current = virtual_series(info, &descs);
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// What to look at
// ---------------------------------------------------------------------------------------------------------------

/// How many dates are named in a line of advice before "and N more".
const NAMED_DATES: usize = 5;
/// Reflectance as 0-1 reaches this at most (clouds and bright sand a little over 1); a band shown beyond it is
/// digital numbers or another scale.
const UNIT_REFLECTANCE_MAX: f32 = 2.0;
/// Reflectance x 10000 reaches this at most.
const SCALED_REFLECTANCE_MAX: f32 = 10000.0;

/// What the advice reads of each date's file (opened to find it, so read once per series: `date_facts`). A list that
/// is empty was not read (no step on needs it).
#[derive(Clone, Debug, Default)]
pub(crate) struct DateFacts {
    /// each date's sensor in FORCE's names, "" where none is known
    pub sensors: Vec<String>,
    /// whether each date has Sentinel-2 granule metadata
    pub granules: Vec<bool>,
    /// whether each date is of HLS (named HLS.L30 / HLS.S30, or of a STAC collection hls*)
    pub hls: Vec<bool>,
}

/// What `advice` looks at besides the steps.
pub(crate) struct Look<'a> {
    pub info: &'a CubeInfo,
    pub roles: &'a BTreeMap<String, i32>,
    /// the coarse series ImproPhe's form picks
    pub coarse: Option<&'a CubeInfo>,
    pub facts: &'a DateFacts,
    /// the upper end of the stretch of the band shown, when it is a reflectance band shown as it is (the 98th
    /// percentile of the values the session has sampled)
    pub shown_hi: Option<f32>,
}

fn str_param<'a>(steps: &'a [ChainStep], tool: &str, key: &str) -> Option<&'a str> {
    steps.iter().find(|s| s.tool.id == tool).and_then(|s| s.params.get(key)).and_then(Value::as_str)
}

fn is_on(steps: &[ChainStep], tool: &str) -> bool {
    steps.iter().any(|s| s.tool.id == tool)
}

/// The steps that read each date's sensor with the parameters they have: the spectral adjustment with its sensor
/// "auto", Tasseled Cap with the coefficients of each sensor.
fn sensor_readers(steps: &[ChainStep]) -> Vec<&'static str> {
    let mut v = vec![];
    if is_on(steps, "spectral_adjust") && str_param(steps, "spectral_adjust", "sensor").unwrap_or("auto") == "auto" {
        v.push("spectral_adjust");
    }
    if is_on(steps, "tasseled_cap") && str_param(steps, "tasseled_cap", "coefficients") == Some("sensor") {
        v.push("tasseled_cap");
    }
    v
}

/// Whether NBAR reads the angles from Sentinel-2's granule metadata.
fn nbar_from_granules(steps: &[ChainStep]) -> bool {
    is_on(steps, "nbar") && str_param(steps, "nbar", "angles") == Some("sentinel2")
}

/// Which dates of a series are of HLS, by their files' names and STAC collection (`harmonize::date_sensor`), read
/// once per series.
fn hls_dates(info: &CubeInfo) -> Vec<bool> {
    use std::collections::HashMap;
    use std::sync::LazyLock;
    static CACHE: LazyLock<Mutex<HashMap<u64, Vec<bool>>>> = LazyLock::new(Default::default);
    if let Some(h) = CACHE.lock().ok().and_then(|c| c.get(&info.id).filter(|h| h.len() == info.t()).cloned()) {
        return h;
    }
    let found: Vec<bool> = (0..info.t())
        .map(|t| janus_zeit::harmonize::date_sensor(info, t) == janus_zeit::harmonize::Found::Hls)
        .collect();
    if let Ok(mut c) = CACHE.lock() {
        c.insert(info.id, found.clone());
    }
    found
}

/// What the steps that are on need of each date's file, read (once per series, then from the caches).
pub(crate) fn date_facts(steps: &[ChainStep], info: &CubeInfo) -> DateFacts {
    let mut facts = DateFacts::default();
    if !sensor_readers(steps).is_empty() {
        facts.sensors = crate::zeit::sensors_for("tasseled_cap", info);
    }
    if nbar_from_granules(steps) {
        facts.granules = crate::zeit::granules_for(info).iter().map(|g| !g.is_empty()).collect();
    }
    if is_on(steps, "nbar") {
        facts.hls = hls_dates(info);
    }
    facts
}

/// A date as a line of advice names it: its day, else its number.
fn date_text(info: &CubeInfo, t: usize) -> String {
    if info.time_is_date && t < info.t() {
        let (y, m, d) = janus_zeit::numeric::civil(info.unix_day(t));
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!("date {}", t + 1)
    }
}

/// Up to NAMED_DATES dates, and how many more.
fn name_dates(info: &CubeInfo, dates: &[usize]) -> String {
    let named: Vec<String> = dates.iter().take(NAMED_DATES).map(|&t| date_text(info, t)).collect();
    match dates.len().saturating_sub(NAMED_DATES) {
        0 => named.join(", "),
        more => format!("{} and {more} more", named.join(", ")),
    }
}

fn step_label(id: &str) -> &str {
    STEPS.iter().find(|s| s.0 == id).map_or(id, |s| s.1)
}

/// What to look at in the series and the steps that are on, from the dates' metadata and the steps' parameters (no
/// pixel is read; the stretch of the band shown, which the session has, tells of digital numbers). `level` 0 is
/// information, 1 something to look at, 2 something that stops a step: a step the series cannot take (`left_out`: the
/// bands unmapped, ImproPhe without a coarse series or in another CRS), dates without a sensor where a step reads it,
/// Sentinel-2 dates in the adjustment (the tool stops at the first), NBAR from granule metadata that dates do not
/// have, or no step on. At 1: HLS dates under NBAR (HLS v2 is already normalised) and a band shown whose values do
/// not fit the units a step is told (reflectance 0-1 beyond 2, x 10000 beyond 10000).
pub(crate) fn advice(steps: &[ChainStep], look: &Look) -> Vec<Advice> {
    let mut out = vec![];
    if steps.is_empty() {
        out.push(Advice { level: 2, text: "No step is on.".into() });
        return out;
    }
    let info = look.info;
    let n = info.t();
    let mut add = |level: u8, text: String| out.push(Advice { level, text });
    for (id, why) in left_out(steps, info, look.roles, look.coarse) {
        add(2, format!("{}: {why}.", step_label(&id)));
    }
    // each date's sensor, where a step reads it
    let readers = sensor_readers(steps);
    let sensors = &look.facts.sensors;
    if !readers.is_empty() && sensors.len() == n {
        let who: Vec<String> = readers
            .iter()
            .map(|id| match *id {
                "tasseled_cap" => format!("{} (coefficients by sensor)", step_label(id)),
                _ => format!("{} (sensor: auto)", step_label(id)),
            })
            .collect();
        let none: Vec<usize> = (0..n).filter(|&t| sensors[t].is_empty()).collect();
        if !none.is_empty() {
            add(
                2,
                format!(
                    "{} of {n} dates have no sensor ({}): {} {} it, read from the file name or the STAC platform. \
                     Name the sensor in the step's form, or the files for it.",
                    none.len(),
                    name_dates(info, &none),
                    who.join(" and "),
                    if who.len() == 1 { "needs" } else { "need" }
                ),
            );
        }
        // the adjustment is to Sentinel-2A: its tool stops at a Sentinel-2 date when it reads each date's sensor
        let s2: Vec<usize> = (0..n).filter(|&t| sensors[t] == "SEN2").collect();
        if readers.contains(&"spectral_adjust") && !s2.is_empty() {
            add(
                2,
                format!(
                    "{} of {n} dates are Sentinel-2 ({}): the spectral adjustment takes Landsat or MODIS to \
                     Sentinel-2A and stops at a Sentinel-2 date. Turn it off for a Sentinel-2 series.",
                    s2.len(),
                    name_dates(info, &s2)
                ),
            );
        }
    }
    // NBAR's angles from Sentinel-2's granule metadata
    if nbar_from_granules(steps) && look.facts.granules.len() == n {
        let without: Vec<usize> = (0..n).filter(|&t| !look.facts.granules[t]).collect();
        if !without.is_empty() {
            add(
                2,
                format!(
                    "NBAR reads the angles from Sentinel-2 granule metadata, which {} of {n} dates lack ({}): the run \
                     stops at the first. Take the angles from bands instead, or use a series made from a Sentinel-2 \
                     search or product.",
                    without.len(),
                    name_dates(info, &without)
                ),
            );
        }
    }
    // HLS is BRDF-normalised already
    let hls: Vec<usize> = look.facts.hls.iter().enumerate().filter(|(_, h)| **h).map(|(t, _)| t).collect();
    if is_on(steps, "nbar") && !hls.is_empty() {
        add(
            1,
            format!(
                "{} of {n} dates are HLS ({}), already normalised to nadir view (HLS v2): NBAR would correct them \
                 again.",
                hls.len(),
                name_dates(info, &hls)
            ),
        );
    }
    // digital numbers where a step is told reflectance
    if let Some(hi) = look.shown_hi {
        for id in ["spectral_adjust", "tasseled_cap"] {
            let (limit, what) = match str_param(steps, id, "units") {
                Some("unit") => (UNIT_REFLECTANCE_MAX, "reflectance 0-1"),
                Some("x10000") => (SCALED_REFLECTANCE_MAX, "reflectance x 10000"),
                _ => continue,
            };
            if hi > limit {
                add(
                    1,
                    format!(
                        "The band shown reaches {hi:.0}, beyond {what}, which the units of {} say: digital numbers? \
                         Set its units (Landsat Collection 2 digital numbers, or auto).",
                        step_label(id)
                    ),
                );
            }
        }
    }
    if info.sel.qa_band == 0 {
        add(0, "The series has no quality band: the steps do not screen clouds.".into());
    }
    out
}

// ---------------------------------------------------------------------------------------------------------------
// The series of the bands as a series of several bands a date
// ---------------------------------------------------------------------------------------------------------------

/// A series a step wrote for one band: what the band is called in the harmonized series and the GeoTIFF (a band for
/// each date).
pub(crate) struct BandSeries {
    pub description: String,
    pub file: String,
}

/// The series of bands a step's outputs are: NBAR's `nbar_<role>` (not its c-factors) and the adjustment's ten.
fn band_series(id: &str, outputs: &[Value]) -> Vec<BandSeries> {
    let path = |o: &Value| o["path"].as_str().unwrap_or("").to_string();
    match id {
        "nbar" => outputs
            .iter()
            .filter_map(|o| {
                Some(BandSeries { description: o["id"].as_str()?.strip_prefix("nbar_")?.into(), file: path(o) })
            })
            .collect(),
        "spectral_adjust" => HARMONIZED
            .iter()
            .filter_map(|(out, description)| {
                let o = outputs.iter().find(|o| o["id"].as_str() == Some(out))?;
                Some(BandSeries { description: description.to_string(), file: path(o) })
            })
            .collect(),
        _ => vec![],
    }
}

/// The harmonized series: its dates (the raw series' dates, in its VRTs) and what its bands are.
pub(crate) struct Assembled {
    pub info: Arc<CubeInfo>,
    /// the roles of its bands, as their descriptions give them
    pub roles: BTreeMap<String, i32>,
    /// the folder of its VRTs
    pub dir: PathBuf,
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

/// The name of each date's VRT: the raw date's file name, so that the sensor and the date in it are found again;
/// where that does not give the date (the raw series dated it by a folder, a band, a tag) or is the same for every
/// date (a file of all of them), the date is added to it.
fn date_names(raw: &CubeInfo) -> Vec<String> {
    let stems: Vec<String> = raw.layers.iter().map(|l| janus_cube::display_name(&l.path)).collect();
    let mut distinct = stems.clone();
    distinct.sort();
    distinct.dedup();
    let unique = distinct.len() == stems.len();
    (0..raw.t())
        .map(|t| {
            if !raw.time_is_date {
                return format!("{:04}_{}", t + 1, stems[t]);
            }
            let (y, m, d) = janus_zeit::numeric::civil(raw.unix_day(t));
            let iso = format!("{y:04}-{m:02}-{d:02}");
            let dated = |n: &str| janus_cube::dates::parse_date(n).map(|d| d.0) == Some(raw.layers[t].time);
            let name = if unique { stems[t].clone() } else { format!("{}_{iso}", stems[t]) };
            if dated(&name) { name } else { format!("{iso}_{}", stems[t]) }
        })
        .collect()
}

/// The series of `bands` (each a GeoTIFF of a band per date, on `window` of the raw series) as a series of several
/// bands a date, one VRT a date in `dir`: band k the k-th series' band of the date, described by its role, then the
/// quality band of the raw series (its raw values, over the window) when the raw series has one, which the step after
/// reads as the raw series' mask. The date's file name and the STAC metadata of its file (and, for a Sentinel-2 file
/// whose granule metadata sits beside it or in its product, where that is) go with it, so `sensors::of_series` and
/// `granules::of_series` answer for each date as they do for the raw series. The angle bands, which NBAR consumed, are
/// not carried. The series is opened with that selection.
pub(crate) fn assemble(
    raw: &CubeInfo,
    window: [usize; 4],
    bands: &[BandSeries],
    dir: &Path,
) -> Result<Assembled, String> {
    let n = raw.t();
    let first = bands.first().ok_or("no band to assemble")?;
    let (w, h) = (window[2] - window[0], window[3] - window[1]);
    let read = gdal::Dataset::open(&first.file).map_err(|e| format!("{}: {e}", first.file))?;
    let (gt, srs) = (read.geo_transform().ok(), read.projection());
    for b in bands {
        let ds = gdal::Dataset::open(&b.file).map_err(|e| format!("{}: {e}", b.file))?;
        if ds.raster_size() != (w, h) || ds.raster_count() != n {
            let (bw, bh) = ds.raster_size();
            return Err(format!(
                "{}: {} dates of {bw} x {bh} px, expected {n} of {w} x {h}",
                b.file,
                ds.raster_count()
            ));
        }
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let (names, granules) = (date_names(raw), janus_zeit::granules::of_series(raw));
    let qa = keeps_quality(raw);
    let full = window == [0, 0, raw.width, raw.height];
    let mut files = vec![];
    for t in 0..n {
        let layer = &raw.layers[t];
        let mut x = format!("<VRTDataset rasterXSize=\"{w}\" rasterYSize=\"{h}\">\n");
        if !srs.is_empty() {
            x += &format!("  <SRS>{}</SRS>\n", xml_escape(&srs));
        }
        if let Some(g) = gt {
            x +=
                &format!("  <GeoTransform>{}, {}, {}, {}, {}, {}</GeoTransform>\n", g[0], g[1], g[2], g[3], g[4], g[5]);
        }
        // the date's own metadata; the granule metadata of a local file where it is found for the raw series
        let file = gdal::Dataset::open(&layer.path).ok();
        let mut meta: Vec<(&str, String)> = DATE_METADATA
            .iter()
            .filter_map(|k| Some((*k, file.as_ref()?.metadata_item(k, "")?)))
            .filter(|(_, v)| !v.is_empty())
            .collect();
        if !meta.iter().any(|m| m.0 == janus_cube::stac::GRANULE_METADATA)
            && !granules[t].is_empty()
            && granules[t].iter().all(|g| !g.contains(char::is_whitespace))
        {
            meta.push((janus_cube::stac::GRANULE_METADATA, granules[t].join(" ")));
        }
        if !meta.is_empty() {
            x += "  <Metadata>\n";
            for (k, v) in &meta {
                x += &format!("    <MDI key=\"{k}\">{}</MDI>\n", xml_escape(v));
            }
            x += "  </Metadata>\n";
        }
        for (k, b) in bands.iter().enumerate() {
            x += &format!("  <VRTRasterBand dataType=\"Float32\" band=\"{}\">\n", k + 1);
            x += &format!(
                "    <Description>{}</Description>\n    <NoDataValue>nan</NoDataValue>\n",
                xml_escape(&b.description)
            );
            x += &format!(
                "    <SimpleSource>\n      <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\n      \
                 <SourceBand>{}</SourceBand>\n    </SimpleSource>\n  </VRTRasterBand>\n",
                xml_escape(&b.file),
                t + 1
            );
        }
        if qa {
            // the raw series' band of the date, the codes as they are, cut to the window the chain ran on
            let rects = if full {
                String::new()
            } else {
                format!(
                    "      <SrcRect xOff=\"{}\" yOff=\"{}\" xSize=\"{w}\" ySize=\"{h}\"/>\n      \
                     <DstRect xOff=\"0\" yOff=\"0\" xSize=\"{w}\" ySize=\"{h}\"/>\n",
                    window[0], window[1]
                )
            };
            x += &format!("  <VRTRasterBand dataType=\"Float32\" band=\"{}\">\n", bands.len() + 1);
            x += &format!("    <Description>{}</Description>\n", xml_escape(&qa_name(raw)));
            x += &format!(
                "    <SimpleSource>\n      <SourceFilename relativeToVRT=\"0\">{}</SourceFilename>\n      \
                 <SourceBand>{}</SourceBand>\n{rects}    </SimpleSource>\n  </VRTRasterBand>\n",
                xml_escape(&layer.path),
                raw.sel.qa_band + layer.band_base
            );
        }
        x += "</VRTDataset>\n";
        let path = dir.join(format!("{}.vrt", names[t]));
        std::fs::write(&path, x).map_err(|e| format!("{}: {e}", path.display()))?;
        files.push(path.to_string_lossy().into_owned());
    }
    let info = janus_cube::open_cube(&files, &harmonized_selection(raw, bands.len()))
        .map_err(|e| format!("the harmonized series could not be opened: {e}"))?;
    if info.t() != n || info.time_is_date != raw.time_is_date {
        return Err(format!("the harmonized series has {} dates, dated or not as the raw one's {n} are not", info.t()));
    }
    let roles = roles_of(&info);
    Ok(Assembled { info: Arc::new(info), roles, dir: dir.to_path_buf() })
}

// ---------------------------------------------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------------------------------------------

/// The chain on the series: each step's tool through `run_step` into a subfolder of `root` (01_nbar...), the series
/// NBAR and the spectral adjustment write (a series for each band) assembled into the harmonized series that the
/// steps after them read (in `NN_tool/series`). A step the series cannot take (`problem`) is left out and reported.
/// The outputs of those two steps are series kept from the layers (`no_layer`); the harmonized series is an output.
/// Outputs on the window of the input, all of them. `progress` is of the whole chain.
pub(crate) fn run_chain(
    steps: &[ChainStep],
    input: &ArdInput,
    root: &Path,
    progress: &(dyn Fn(f64, &str) + Sync),
    cancel: &AtomicBool,
) -> Result<Chain, String> {
    let base = &input.chain;
    if steps.is_empty() {
        return Err("no step is on".into());
    }
    std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let n = steps.len();
    // the harmonized series once a step wrote it, else the raw one
    let mut current: Option<Assembled> = None;
    let mut outputs: Vec<Value> = vec![];
    let mut skipped: Vec<(String, String)> = vec![];
    // the output of the latest harmonized series: the only one that is a layer
    let mut harmonized: Option<usize> = None;
    for (k, step) in steps.iter().enumerate() {
        if cancelled(cancel) {
            return Err("cancelled".into());
        }
        let tool = step.tool;
        let (info, roles, window, source) = match &current {
            Some(c) => (c.info.clone(), c.roles.clone(), None, Some(c.dir.to_string_lossy().into_owned())),
            None => (base.info.clone(), base.roles.clone(), Some(base.window), None),
        };
        let coarse = input.coarse.as_ref();
        if let Some(why) = problem(tool, &info, &roles, &step.params, coarse.map(|c| &*c.1)) {
            skipped.push((tool.id.clone(), why));
            continue;
        }
        let (scope, series) = match &source {
            Some(dir) => (
                format!("the harmonized series ({} x {} px)", info.width, info.height),
                SeriesRecord::of(&info, "harmonized series", std::slice::from_ref(dir)),
            ),
            None => (base.scope.clone(), SeriesRecord::of(&info, &base.name, &base.inputs)),
        };
        let step_progress = |f: f64, m: &str| {
            progress((k as f64 + f) / n as f64, &format!("step {}/{n}: {}: {m}", k + 1, tool.name));
        };
        let run = StepRun {
            tool,
            params: &step.params,
            info: &info,
            roles: &roles,
            window,
            map_t: base.map_t.min(info.t().saturating_sub(1)),
            threads: base.threads,
            other: coarse.filter(|_| tool.id == "improphe").map(|c| (c.0.as_str(), &*c.1)),
            scope,
            scope_index: base.scope_index,
            series,
            reads: source.clone().unwrap_or_else(|| "the series".into()),
            task: "Analysis-ready data",
            root,
            k,
            n,
        };
        let res = run_step(&run, &step_progress, cancel)?;
        let bands = band_series(&tool.id, &res.outputs);
        let writes_bands = written_bands(&tool.id, &roles).is_some();
        // each output named by its step, the layers it becomes being of different tools
        for mut o in res.outputs {
            let name = o["name"].as_str().or(o["id"].as_str()).unwrap_or("").to_string();
            o["name"] = json!(format!("{}: {name}", tool.name));
            if writes_bands {
                o["no_layer"] = json!(true);
            }
            outputs.push(o);
        }
        if writes_bands {
            progress((k as f64 + 1.0) / n as f64, &format!("step {}/{n}: assembling the harmonized series", k + 1));
            let dir = root.join(format!("{:02}_{}", k + 1, tool.id)).join("series");
            let assembled = assemble(&base.info, base.window, &bands, &dir)?;
            // the step's provenance lists the series it made too
            let folder = dir.parent().unwrap_or(root);
            if let Ok(mut p) = Provenance::read(folder) {
                p.outputs.push(OutputRecord { name: HARMONIZED_NAME.into(), path: dir.to_string_lossy().into() });
                p.write(folder);
            }
            if let Some(prev) = harmonized {
                outputs[prev]["no_layer"] = json!(true);
            }
            harmonized = Some(outputs.len());
            outputs.push(json!({"id": "harmonized", "name": HARMONIZED_NAME, "path": dir.to_string_lossy(),
                                "series": true, "unit": "as the input"}));
            current = Some(assembled);
        }
    }
    if outputs.is_empty() {
        let why: Vec<String> = skipped.iter().map(|(t, w)| format!("{t}: {w}")).collect();
        return Err(format!("no step could run: {}", why.join("; ")));
    }
    Ok(Chain { result: JobResult { outputs, window: base.window }, skipped })
}

/// The script's variable that holds the harmonized series once a step has made it: a DataArray (time, band, y, x) whose
/// bands are named by role, which is what `assemble` writes as VRTs in the chain.
const HARMONIZED_VAR: &str = "harmonized";

/// The line that makes `harmonized` out of the result `var` of a step that writes a series for each band (what
/// `assemble` does with the files): NBAR's bands are named by role already; the adjustment's ten are renamed as
/// `HARMONIZED` has them.
fn harmonized_line(id: &str, var: &str) -> Option<String> {
    match id {
        "nbar" => Some(format!("{HARMONIZED_VAR} = {var}  # the harmonized series: NBAR's bands, named by role")),
        "spectral_adjust" => {
            let list = |v: Vec<&str>| v.iter().map(|s| format!("\"{s}\"")).collect::<Vec<_>>().join(", ");
            Some(format!(
                "{HARMONIZED_VAR} = {var}.sel(band=[{}]).assign_coords(band=[{}])  # the harmonized series: Zeit's \
                 bands named by role",
                list(HARMONIZED.iter().map(|h| h.0).collect()),
                list(HARMONIZED.iter().map(|h| h.1).collect())
            ))
        }
        _ => None,
    }
}

/// The chain as one Python script, what `run_chain` does in Zeit: the series read once from its files over the window,
/// each step's call (`janus_zeit::python::calls`, those of each tool's own rerun.py, with the inputs that
/// `python::input_lines` makes) with its results numbered along (`result1`, `result2`...), and every result written as
/// a GeoTIFF in `out_dir` under its step's name. What `assemble` does between the steps (the per-band series made one
/// series of several bands a date) is in Python a DataArray `harmonized` of NBAR's result, or of the adjustment's with
/// its bands renamed by role; the steps after read the bands from it and the quality band and dates from the raw
/// series, as the chain's VRTs carry them. `skipped` are the steps the series cannot take (`left_out`).
pub(crate) fn chain_code(
    steps: &[ChainStep],
    skipped: &[(String, String)],
    input: &ArdInput,
    out_dir: &str,
) -> Result<String, String> {
    use std::fmt::Write as _;
    let base = &input.chain;
    let info = &base.info;
    let mut o = crate::python::script_head(info, "the Analysis-ready data chain", false, base.window);
    let (mut results, mut harmonized, mut names) = (0usize, false, vec![]);
    for (k, step) in steps.iter().enumerate() {
        let tool = step.tool;
        if let Some((_, why)) = skipped.iter().find(|(t, _)| *t == tool.id) {
            let _ = writeln!(o, "\n# {}: left out ({why})", tool.name);
            continue;
        }
        let threads = Some(base.threads as i64);
        let (code, inputs) = crate::python::tool_calls(info, tool, &step.params, &base.roles, threads)?;
        let other = input.coarse.as_ref().filter(|_| tool.id == "improphe").map(|c| &*c.1);
        let from = harmonized.then_some(HARMONIZED_VAR);
        let lines = crate::python::input_lines(
            info,
            tool,
            &step.params,
            &base.roles,
            other,
            base.window,
            &code,
            &inputs,
            from,
        )?;
        let (code, n) = crate::chain::number_results(&code, results, &format!("{:02}_{}", k + 1, tool.id), &mut names);
        let reads = if harmonized { "the harmonized series" } else { "the series" };
        let _ = writeln!(o, "\n# {}. {}, on {reads}\n{lines}{code}", k + 1, tool.name);
        if let Some(line) = harmonized_line(&tool.id, &format!("result{}", results + 1)) {
            let _ = writeln!(o, "{line}");
            harmonized = true;
        }
        results += n;
    }
    if results == 0 {
        return Err("no step can run".into());
    }
    o += &crate::python::save_code(out_dir, &names);
    Ok(o)
}

/// The folder of a preview in the work folder `base`.
fn preview_dir(base: &Path, stamp: &str) -> PathBuf {
    base.join(format!("ard_{stamp}"))
}

// ---------------------------------------------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------------------------------------------

impl State {
    pub(crate) fn ard_menu(&mut self, ui: &mut Ui) {
        if ui
            .add(egui::Button::selectable(self.ard.open, "Analysis-ready data (FORCE)..."))
            .on_hover_text(
                "FORCE's Level 2 chain on the series: NBAR, the spectral adjustment to Sentinel-2A (together the \
                 harmonized series), then Tasseled Cap and ImproPhe on it, as one task",
            )
            .clicked()
        {
            self.ard.open = !self.ard.open;
            ui.close();
        }
    }

    pub(crate) fn ard_window(&mut self, ctx: &egui::Context) {
        self.ard_pump_preview(ctx);
        if !self.ard.open {
            return;
        }
        let mut open = true;
        let width = (ctx.content_rect().width() * 0.4).clamp(420.0, 700.0);
        crate::panels::fitted_window("Analysis-ready data", ctx, width)
            .id(egui::Id::new("ard window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.ard_ui(ui)));
        self.ard.open = open;
        if self.ard_running() || self.ard_previewing() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn ard_running(&self) -> bool {
        self.zeit.jobs.iter().any(|j| j.tool == JOB_TOOL && j.status.lock().unwrap().state == JobState::Running)
    }

    /// The preview's chain is running.
    fn ard_previewing(&self) -> bool {
        self.ard.preview.as_ref().is_some_and(|p| {
            self.zeit.jobs.iter().any(|j| j.output_dir == p.dir && j.status.lock().unwrap().state == JobState::Running)
        })
    }

    /// What to look at now: the steps that are on and the series' metadata (read once per series).
    pub(crate) fn ard_advice(&mut self, info: &Arc<CubeInfo>) -> Vec<Advice> {
        let steps = self.ard_steps();
        let roles = self.active_roles();
        let coarse = self.ard_coarse();
        let facts = date_facts(&steps, info);
        // the band shown, when it is a reflectance band as it is: its stretch's upper end
        let reflectance = SIX.iter().chain(NBAR_ROLES.iter()).any(|r| roles.get(*r) == Some(&info.sel.band));
        let range = &self.display.ranges[janus_gpu::Mode::Value as usize];
        let shown_hi =
            (self.display.mode == janus_gpu::Mode::Value && reflectance && range.is_set()).then_some(range.hi);
        let look = Look { info, roles: &roles, coarse: coarse.as_ref().map(|c| &*c.1), facts: &facts, shown_hi };
        advice(&steps, &look)
    }

    pub(crate) fn ard_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.colored_label(crate::theme::error(), "Open a series first.");
            return;
        };
        // what to look at, once a frame
        let to_look = self.ard_advice(&info);
        let n_look = to_look.iter().filter(|a| a.level > 0).count();
        ui.horizontal(|ui| {
            for (i, name) in ["Input", "Steps", "Output"].into_iter().enumerate() {
                let text = if i == 1 && n_look > 0 { format!("{name} ({n_look})") } else { name.to_string() };
                ui.selectable_value(&mut self.ard.tab, i, RichText::new(text).strong());
            }
        });
        ui.separator();
        match self.ard.tab {
            0 => self.ard_input_tab(ui, &info),
            1 => {
                self.ard_steps_tab(ui, &info);
                crate::panels::section(ui, "To look at");
                crate::tsa::advice_ui(ui, &to_look);
            }
            _ => {
                crate::panels::section(ui, "To look at");
                crate::tsa::advice_ui(ui, &to_look);
                self.ard_output_tab(ui, &info);
            }
        }
    }

    /// The series the chain starts from: its dates, the bands by role, each date's sensor and granule metadata.
    fn ard_input_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let inputs = self.layers.get(self.active).map(|l| l.inputs.clone()).unwrap_or_default();
        let n = info.t();
        let label = |t: usize| info.layers.get(t).map_or(String::new(), |l| l.label.clone());
        ui.label(RichText::new(crate::zeit::series_name(info, &inputs)).strong());
        ui.label(format!("{n} dates, {} to {}", label(0), label(n.saturating_sub(1))));
        if !info.time_is_date {
            ui.colored_label(
                crate::theme::error(),
                "No dates were found: the spectral adjustment and Tasseled Cap need them.",
            );
        }
        crate::panels::section(ui, "Bands");
        let roles = self.active_roles();
        let band = |b: i32| format!("{b}: {}", info.band_names.get(b as usize - 1).map_or("", String::as_str));
        for (title, wanted) in [("Reflectance", &SIX[..]), ("Angles (NBAR)", &ANGLES[..])] {
            for role in wanted {
                ui.horizontal(|ui| {
                    ui.label(format!("{role}:"));
                    match roles.get(*role) {
                        Some(&b) => ui.label(band(b)),
                        None => ui.weak(if title == "Reflectance" {
                            "not mapped"
                        } else {
                            "not mapped (or the Sentinel-2 granules)"
                        }),
                    };
                });
            }
        }
        ui.weak(
            "The roles are mapped in each tool's own window (Tools > Data preparation > NBAR, the spectral \
             adjustment, Tasseled Cap); the chain reads them from there.",
        );
        if info.sel.qa_band > 0 {
            ui.label(format!(
                "Quality band {} (rule: {}): the chain keeps it.",
                info.sel.qa_band,
                info.sel.qa_rule.id()
            ));
        } else {
            ui.weak("No quality band: set one in the series' settings so the steps screen clouds.");
        }
        crate::panels::section(ui, "Dates");
        let sensors = crate::zeit::sensors_for("tasseled_cap", info);
        let mut count: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &sensors {
            *count.entry(if s.is_empty() { "" } else { s.as_str() }).or_default() += 1;
        }
        let known: Vec<String> =
            count.iter().filter(|(s, _)| !s.is_empty()).map(|(s, c)| format!("{s} x {c}")).collect();
        ui.label(format!("Sensors: {}", if known.is_empty() { "none found".to_string() } else { known.join(", ") }));
        let without = count.get("").copied().unwrap_or(0);
        if without > 0 {
            ui.colored_label(
                crate::theme::error(),
                format!("{without} of {n} dates have no sensor (the file name or the STAC platform say none)."),
            );
        }
        let granules = crate::zeit::granules_for(info);
        let with = granules.iter().filter(|g| !g.is_empty()).count();
        ui.label(format!("Sentinel-2 granule metadata (MTD_TL.xml): {with} of {n} dates."));
        if with == 0 && missing(&roles, &ANGLES).len() == ANGLES.len() {
            ui.weak("Without angle bands or granule metadata NBAR cannot run.");
        }
    }

    /// The steps, each with its tool's own parameters (the ones the tool's window edits).
    fn ard_steps_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        let steps = self.ard_steps();
        let roles = self.active_roles();
        let coarse = self.ard_coarse();
        let out = left_out(&steps, info, &roles, coarse.as_ref().map(|c| &*c.1));
        for (i, (id, label, line)) in STEPS.into_iter().enumerate() {
            let Some(tool) = manifest().tool(id) else { continue };
            ui.add_space(2.0);
            ui.checkbox(&mut self.ard.on[i], RichText::new(label).strong()).on_hover_text(&tool.description);
            ui.weak(line);
            if self.ard.on[i]
                && let Some((_, w)) = out.iter().find(|o| o.0 == id)
            {
                ui.colored_label(crate::theme::error(), format!("Left out: {w}"));
            }
            if self.ard.on[i] {
                ui.indent(("ard step", id), |ui| {
                    if self.tool_params_ui(ui, tool) {
                        self.zeit.tools.entry(tool.id.clone()).or_default().version += 1;
                    }
                });
            }
        }
    }

    /// The scope, Run, the progress of the chain under way and where it wrote.
    fn ard_output_tab(&mut self, ui: &mut Ui, info: &Arc<CubeInfo>) {
        if self.ard.scope == 2 && self.series.roi_rect.is_none() {
            self.ard.scope = 0;
        }
        let roi = self.series.roi_rect.is_some();
        ComboBox::from_id_salt("ard scope").width(ui.available_width()).selected_text(SCOPES[self.ard.scope]).show_ui(
            ui,
            |ui| {
                for (i, s) in SCOPES.iter().enumerate() {
                    if ui.add_enabled(i != 2 || roi, egui::Button::selectable(self.ard.scope == i, *s)).clicked() {
                        self.ard.scope = i;
                    }
                }
            },
        );
        let (window, scope_name) = self.run_window(self.ard.scope);
        ui.weak(format!("{scope_name}: {} x {} px", window[2] - window[0], window[3] - window[1]));
        self.output_folder_ui(ui);
        let on: Vec<&str> = (0..STEPS.len()).filter(|&i| self.ard.on[i]).map(|i| STEPS[i].1).collect();
        ui.label(if on.is_empty() { "No step is on.".to_string() } else { format!("Runs: {}.", on.join(", ")) });
        let running = self.ard_running();
        let blocked = if on.is_empty() {
            Some("turn at least one step on (Steps)".to_string())
        } else if running {
            Some("a chain is running".to_string())
        } else {
            self.ard_blocked(info)
        };
        if ui
            .add_enabled(blocked.is_none(), egui::Button::new("Run").min_size(egui::vec2(ui.available_width(), 0.0)))
            .clicked()
        {
            self.ard_run(&ui.ctx().clone());
        }
        if let Some(why) = &blocked
            && !running
        {
            ui.colored_label(crate::theme::error(), format!("Not yet: {why}."));
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(!on.is_empty(), egui::Button::new("Copy Python"))
                .on_hover_text(
                    "One script that does in Zeit what the chain does on this series and scope: the series read once \
                     from its files, each step's call (the steps after NBAR and the adjustment reading the \
                     harmonized series, which in the script is a DataArray of their result with its bands named by \
                     role), every result written as a GeoTIFF. Copied to the clipboard.",
                )
                .clicked()
            {
                match self.ard_python(info) {
                    Ok(code) => {
                        ui.ctx().copy_text(code);
                        self.ard.copied = Some(Instant::now());
                    }
                    Err(e) => self.error_popup = Some(e),
                }
            }
            if self.ard.copied.is_some_and(|t| t.elapsed().as_secs_f32() < 3.0) {
                ui.weak("Copied.");
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
            }
        });
        self.ard_preview_ui(ui, blocked.as_deref(), roi);
        // the last chain: its progress and Cancel, or how it ended and its folder
        let mut open_folder = None;
        let mut use_series = None;
        let harmonized = self
            .ard
            .series_out
            .lock()
            .unwrap()
            .iter()
            .find(|(name, _)| name == HARMONIZED_NAME)
            .map(|(_, path)| path.clone());
        if let Some(j) = self.zeit.jobs.iter().rev().find(|j| j.tool == JOB_TOOL) {
            let s = j.status.lock().unwrap();
            ui.separator();
            ui.label(&j.title);
            match s.state {
                JobState::Running => {
                    ui.add(egui::ProgressBar::new(s.progress as f32).text(format!(
                        "{:.0}%  {:.0} s  {}",
                        s.progress * 100.0,
                        j.started.elapsed().as_secs_f64(),
                        s.message
                    )));
                    if ui.button("Cancel").clicked() {
                        j.cancel.store(true, Ordering::Relaxed);
                    }
                }
                state => {
                    ui.label(match state {
                        JobState::Done => format!("Done in {:.0} s. {}", s.seconds, s.message),
                        JobState::Cancelled => "Cancelled.".to_string(),
                        _ => format!("Failed: {}", s.error),
                    });
                    if ui.button("Folder").on_hover_text(&j.output_dir).clicked() {
                        open_folder = Some(j.output_dir.clone());
                    }
                    if state == JobState::Done
                        && let Some(path) = &harmonized
                        && ui
                            .add(
                                egui::Button::new("Use in Time series analysis")
                                    .min_size(egui::vec2(ui.available_width(), 0.0)),
                            )
                            .on_hover_text(
                                "Makes the harmonized series the active layer (opening it first if it is not) and \
                                 opens the Time series analysis window on it",
                            )
                            .clicked()
                    {
                        use_series = Some(path.clone());
                    }
                }
            }
        }
        if let Some(d) = open_folder {
            crate::platform::open_in_explorer(&d);
        }
        if let Some(path) = use_series {
            self.use_series(&path, &ui.ctx().clone());
            (self.tsa.open, self.tsa.tab) = (true, 0);
        }
    }

    /// Preview on the ROI: the chain on the ROI into a folder of its own, then the curtain; or its progress, and
    /// the button that turns it off.
    fn ard_preview_ui(&mut self, ui: &mut Ui, blocked: Option<&str>, roi: bool) {
        ui.add_space(4.0);
        let Some(p) = &self.ard.preview else {
            let running = self.ard_running();
            let why = if !roi {
                Some("draw an ROI on the map first")
            } else if running {
                Some("a chain is running")
            } else {
                blocked
            };
            if ui
                .add_enabled(
                    why.is_none(),
                    egui::Button::new("Preview on the ROI").min_size(egui::vec2(ui.available_width(), 0.0)),
                )
                .on_hover_text(
                    "The chain on the ROI alone, into a folder that is removed with the preview, then the harmonized \
                     series against the series on the map: the swipe, the series at the left and the harmonized one \
                     at the right, on the date shown",
                )
                .clicked()
            {
                self.ard_preview(&ui.ctx().clone());
            }
            if let Some(why) = why.filter(|_| roi) {
                ui.weak(format!("Preview: {why}."));
            }
            return;
        };
        let dir = p.dir.clone();
        if let Some(j) = self.zeit.jobs.iter().find(|j| j.output_dir == dir) {
            let s = j.status.lock().unwrap();
            match s.state {
                JobState::Running => {
                    ui.add(egui::ProgressBar::new(s.progress as f32).text(format!(
                        "Preview {:.0}%  {}",
                        s.progress * 100.0,
                        s.message
                    )));
                }
                JobState::Done => {
                    ui.weak(
                        "Preview: the series at the left of the divider on the map, the harmonized one at the right \
                         (its dashed window is the ROI).",
                    );
                }
                JobState::Failed => {
                    ui.colored_label(crate::theme::error(), format!("Preview failed: {}", s.error));
                }
                _ => {
                    ui.weak("Preview cancelled.");
                }
            }
        }
        if ui
            .add(egui::Button::new("Turn the preview off").min_size(egui::vec2(ui.available_width(), 0.0)))
            .on_hover_text("Removes the preview's layers, the swipe and its folder")
            .clicked()
        {
            self.ard_preview_stop(&ui.ctx().clone());
        }
    }

    /// The chain on the ROI (its bounding box) into a folder of the preview's own in the work folder; replaces the
    /// last preview of any tool (its maps, layers and folder gone). The curtain follows when it ends
    /// (`ard_pump_preview`).
    pub(crate) fn ard_preview(&mut self, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else { return };
        if self.series.roi_rect.is_none() {
            self.error_popup = Some("draw an ROI on the map first".into());
            return;
        }
        let steps = self.ard_steps();
        if steps.is_empty() {
            self.error_popup = Some("no step of the Analysis-ready data is on".into());
            return;
        }
        self.ard_preview_stop(ctx);
        self.drop_preview();
        let (mut input, _) = self.ard_chain_input(&info, 2);
        input.chain.scope = "preview".into();
        let window = input.chain.window;
        let base = crate::settings::app_data_dir().join("work").join("preview");
        let dir = preview_dir(&base, &format!("{}_{}", crate::zeit::timestamp(), self.frame));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error_popup = Some(format!("could not create {}: {e}", dir.display()));
            return;
        }
        let title =
            format!("Analysis-ready data preview - ROI ({} x {} px)", window[2] - window[0], window[3] - window[1]);
        let log_head = format!(
            "steps: {}\nwindow: {window:?}\nthreads: {}\n",
            steps.iter().map(|s| s.tool.id.as_str()).collect::<Vec<_>>().join(", "),
            input.chain.threads
        );
        let written = Arc::new(Mutex::new(vec![]));
        let start = JobStart {
            title,
            tool: PREVIEW_JOB,
            thread: "zeit-ard-preview",
            cube: info.id,
            root: dir.clone(),
            log_head,
        };
        let root = dir.clone();
        spawn_job(&mut self.zeit.jobs, start, written.clone(), ctx, move |progress, cancel| {
            let mut chain = run_chain(&steps, &input, &root, progress, cancel)?;
            // the series it wrote join the layers hidden: the map keeps the series, the curtain shows them
            for o in chain.result.outputs.iter_mut().filter(|o| o["series"].as_bool() == Some(true)) {
                o["hidden"] = json!(true);
            }
            Ok(chain)
        });
        let dir = dir.to_string_lossy().into_owned();
        self.zeit.preview =
            Some(Preview { tool: JOB_TOOL.into(), dir: dir.clone(), cube: info.id, window, version: 0 });
        self.ard.preview = Some(ArdPreview { dir, written, curtain: false });
    }

    /// Each frame: the preview still the zeit's (another tool's preview replaces it), and once its chain ended and
    /// its harmonized series is a layer, the swipe on it at the date shown.
    pub(crate) fn ard_pump_preview(&mut self, ctx: &egui::Context) {
        self.ard_drop_orphans(ctx);
        let Some(p) = &self.ard.preview else { return };
        if self.zeit.preview.as_ref().is_none_or(|z| z.dir != p.dir) {
            // replaced by another preview (which removed the folder): its layers and the swipe go
            self.ard_preview_stop(ctx);
            return;
        }
        if p.curtain
            || self.opening.is_some()
            || !self.zeit.series_to_open.is_empty()
            || self.zeit.keep_active.is_some()
        {
            return;
        }
        let Some(path) = p.written.lock().unwrap().iter().find(|(n, _)| n == HARMONIZED_NAME).map(|(_, f)| f.clone())
        else {
            return;
        };
        let Some(i) = self.series_layer(&path) else { return };
        let Some((cube, n)) = self.layer_session(i).map(|s| (s.info.id, s.info.t())) else { return };
        if self.swipe.is_none() {
            self.toggle_swipe();
        }
        let t = self.display.t;
        if let Some(sw) = self.swipe.as_mut() {
            // the swipe's right side: the harmonized series, on the date the map shows
            sw.view.cube = cube;
            (sw.view.own_date, sw.view.t, sw.view.own_mode, sw.view.basemap_only) = (true, t.min(n - 1), false, false);
            sw.view.dirty = true;
            sw.x = 0.5;
        }
        if let Some(p) = self.ard.preview.as_mut() {
            p.curtain = true;
        }
    }

    /// The preview gone: the swipe if it shows its series, its layers, its task and its folder.
    pub(crate) fn ard_preview_stop(&mut self, ctx: &egui::Context) {
        let Some(p) = self.ard.preview.take() else { return };
        let dir = std::path::PathBuf::from(&p.dir);
        // not opened yet
        self.zeit.series_to_open.retain(|(q, _)| !std::path::Path::new(q).starts_with(&dir));
        self.ard_remove_layers_under(&dir, ctx);
        // being opened now: its layer is removed when it arrives
        if self.ard_opening_under(&dir) {
            self.ard.orphans.push(p.dir.clone());
        }
        if self.zeit.preview.as_ref().is_some_and(|z| z.dir == p.dir) {
            self.drop_preview();
        } else {
            crate::zeit::remove_dir_soon(p.dir);
        }
    }

    /// The chain's script for the steps on and the scope now, even before it runs.
    pub(crate) fn ard_python(&mut self, info: &Arc<CubeInfo>) -> Result<String, String> {
        let steps = self.ard_steps();
        let (input, name) = self.ard_chain_input(info, self.ard.scope);
        let out = self.results_dir().join(format!("{name}_ard_python"));
        let out_of = left_out(&steps, info, &input.chain.roles, input.coarse.as_ref().map(|c| &*c.1));
        chain_code(&steps, &out_of, &input, &out.to_string_lossy())
    }

    /// Run again from a chain's provenance: its steps on with their parameters and its scope, in the window's Output
    /// tab (on the active series).
    pub(crate) fn ard_again(&mut self, p: &Provenance) {
        let ids: Vec<&str> = p
            .extra
            .get("steps")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|s| s.as_str()).map(|s| s.split_once('_').map_or(s, |x| x.1)).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            self.error_popup = Some("the chain's steps are not in its provenance".into());
            return;
        }
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            self.ard.on[i] = ids.contains(&id);
        }
        for (id, v) in &p.params {
            if let (Some(tool), Some(m)) = (manifest().tool(id), v.as_object()) {
                *self.zeit.params(tool) = m.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                self.zeit.tools.entry(id.clone()).or_default().version += 1;
            }
        }
        self.ard.scope = if p.scope_index == 2 && self.series.roi_rect.is_none() { 0 } else { p.scope_index.min(2) };
        (self.ard.open, self.ard.tab) = (true, 2);
    }

    /// The layers that opened a file under `dir` (a preview's series) removed, and the swipe if it shows one.
    pub(crate) fn ard_remove_layers_under(&mut self, dir: &Path, ctx: &egui::Context) {
        while let Some(i) =
            self.layers.iter().position(|l| l.inputs.len() == 1 && Path::new(&l.inputs[0]).starts_with(dir))
        {
            let cube = self.layer_session(i).map(|s| s.info.id);
            if self.swipe.as_ref().is_some_and(|sw| Some(sw.view.cube) == cube) {
                self.toggle_swipe();
            }
            self.remove_layer(i, ctx);
        }
    }

    /// A series under `dir` is being opened (its layer is not there yet).
    fn ard_opening_under(&self, dir: &Path) -> bool {
        self.opening.as_ref().is_some_and(|o| o.inputs.iter().any(|q| Path::new(q).starts_with(dir)))
    }

    /// The layers of previews turned off while their series was being opened, removed once they are there.
    fn ard_drop_orphans(&mut self, ctx: &egui::Context) {
        for dir in std::mem::take(&mut self.ard.orphans) {
            let path = PathBuf::from(&dir);
            self.ard_remove_layers_under(&path, ctx);
            if self.ard_opening_under(&path) {
                self.ard.orphans.push(dir);
            }
        }
    }

    /// Why the chain cannot run on the series, if no step of it can.
    pub(crate) fn ard_blocked(&mut self, info: &CubeInfo) -> Option<String> {
        let steps = self.ard_steps();
        let roles = self.active_roles();
        let coarse = self.ard_coarse();
        let out = left_out(&steps, info, &roles, coarse.as_ref().map(|c| &*c.1));
        // a step that can run: the chain can
        (out.len() == steps.len()).then(|| {
            out.iter()
                .map(|(id, why)| format!("{}: {why}", STEPS.iter().find(|s| s.0 == id).map_or(id.as_str(), |s| s.1)))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    /// The chain's steps that are on, with the parameters their windows hold.
    pub(crate) fn ard_steps(&mut self) -> Vec<ChainStep> {
        let mut steps = vec![];
        for (i, (id, _, _)) in STEPS.into_iter().enumerate() {
            if let Some(tool) = manifest().tool(id).filter(|_| self.ard.on[i]) {
                steps.push(ChainStep { tool, params: self.zeit.params(tool).clone() });
            }
        }
        steps
    }

    /// The coarse series ImproPhe's form picks (its name and series), if that step is on and one is open.
    fn ard_coarse(&mut self) -> Option<(String, Arc<CubeInfo>)> {
        let tool = manifest().tool("improphe").filter(|_| self.ard.on[3])?;
        let params = self.zeit.params(tool).clone();
        self.picked_series(tool, &params).ok().flatten()
    }

    /// What the chain starts from now: the active layer's series, the scope and the roles of its bands. And the
    /// series' name for the folders.
    fn ard_chain_input(&mut self, info: &Arc<CubeInfo>, scope_index: usize) -> (ArdInput, String) {
        let layer = self.layers.get(self.active).map(|l| (l.name.clone(), l.inputs.clone()));
        let inputs = layer.as_ref().map(|l| l.1.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(info, &inputs);
        let (window, scope) = self.run_window(scope_index);
        let chain = ChainInput {
            info: info.clone(),
            name: layer.map_or(name.clone(), |l| l.0),
            inputs,
            roles: self.active_roles(),
            window,
            scope: scope.to_string(),
            scope_index,
            map_t: self.display.t.min(info.t().saturating_sub(1)),
            threads: self.processing_threads(),
        };
        (ArdInput { chain, coarse: self.ard_coarse() }, name)
    }

    /// Starts the chain as one task (Tools > Tasks): the steps on, on the scope, into a folder of its own.
    pub(crate) fn ard_run(&mut self, ctx: &egui::Context) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            self.error_popup = Some("no series is open".into());
            return;
        };
        let steps = self.ard_steps();
        if steps.is_empty() {
            self.error_popup = Some("no step of the Analysis-ready data is on".into());
            return;
        }
        self.ard_preview_stop(ctx);
        let (input, name) = self.ard_chain_input(&info, self.ard.scope);
        let (window, scope) = (input.chain.window, input.chain.scope.clone());
        let root = self.results_dir().join(format!("{name}_ard_{}", crate::zeit::timestamp()));
        if let Err(e) = std::fs::create_dir_all(&root) {
            self.error_popup = Some(format!("could not create {}: {e}", root.display()));
            return;
        }
        // the chain's provenance at its folder (where its result layers point): the steps with their parameters and
        // folders, the series and the scope; `pump_provenance` completes it from the task's result
        let mut prov = Provenance::start("ard", JOB_TOOL, "Analysis-ready data");
        prov.params = steps.iter().map(|s| (s.tool.id.clone(), json!(s.params))).collect();
        prov.scope = scope.clone();
        prov.scope_index = input.chain.scope_index;
        prov.window = Some(window);
        prov.threads = Some(input.chain.threads as i64);
        prov.series = Some(SeriesRecord::of(&info, &input.chain.name, &input.chain.inputs));
        prov.extra.insert(
            "steps".into(),
            json!(steps.iter().enumerate().map(|(k, s)| format!("{:02}_{}", k + 1, s.tool.id)).collect::<Vec<_>>()),
        );
        // the steps the series cannot take are left out of the run (their folders are not made)
        let out_of = left_out(&steps, &info, &input.chain.roles, input.coarse.as_ref().map(|c| &*c.1));
        prov.extra.insert(
            "left_out".into(),
            json!(out_of.iter().map(|(id, why)| json!({"step": id, "why": why})).collect::<Vec<_>>()),
        );
        // its Python: the script Copy Python gives, writing the results beside the chain's
        let python = root.join("python").to_string_lossy().into_owned();
        match chain_code(&steps, &out_of, &input, &python) {
            Ok(code) if std::fs::write(root.join(SCRIPT), &code).is_ok() => prov.python = SCRIPT.into(),
            Ok(_) => prov.python_note = format!("{SCRIPT} could not be written"),
            Err(e) => prov.python_note = e,
        }
        self.begin_provenance(&root, prov);
        let title = format!("Analysis-ready data - {scope} ({} x {} px)", window[2] - window[0], window[3] - window[1]);
        let log_head = format!(
            "steps: {}\nwindow: {window:?}\nthreads: {}\n",
            steps.iter().map(|s| s.tool.id.as_str()).collect::<Vec<_>>().join(", "),
            input.chain.threads
        );
        let written = Arc::new(Mutex::new(vec![]));
        self.ard.series_out = written.clone();
        let start = JobStart { title, tool: JOB_TOOL, thread: "zeit-ard", cube: info.id, root: root.clone(), log_head };
        spawn_job(&mut self.zeit.jobs, start, written, ctx, move |progress, cancel| {
            run_chain(&steps, &input, &root, progress, cancel)
        });
        self.zeit.show_tasks = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::series_outputs;
    use crate::zeit::{base_spec, series_inputs};
    use janus_cube::open_cube;
    use janus_zeit::job::{JobSpec, run_job};

    const W: usize = 8;
    const H: usize = 6;
    const GT: [f64; 6] = [500000.0, 30.0, 0.0, 7500000.0, 0.0, -30.0];
    const NAMES: [&str; 11] = ["Blue", "Green", "Red", "NIR", "SWIR1", "SWIR2", "SZA", "SAA", "VZA", "VAA", "QA_PIXEL"];
    const GRANULES: &str = "https://example.com/a/MTD_TL.xml https://example.com/b/MTD_TL.xml";

    /// Eight dates, a file each, every 16 days from 2020-01-05: six reflectance bands (x 10000), the four angle
    /// bands (degrees) and a QA_PIXEL with some cloud. Landsat 8, 9 and 7 by the file names, but for the fourth
    /// date, named for no sensor and carrying a STAC platform, and the sixth, which records its granule metadata;
    /// a MTD_TL.xml beside the files stands for the others'.
    fn series(dir: &Path) -> Arc<CubeInfo> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("MTD_TL.xml"), "x").unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap();
        let sensors = ["LC08", "LC09", "LE07", "", "LC09", "LC08", "LE07", "LC09"];
        let day0 = janus_cube::dates::days_from_civil(2020, 1, 5);
        for (k, sensor) in sensors.iter().enumerate() {
            let day = day0 + 16 * k as i64;
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let ymd = format!("{y:04}{m:02}{d:02}");
            let name = if sensor.is_empty() {
                format!("scene_{ymd}.tif")
            } else {
                format!("{sensor}_L2SP_222082_{ymd}_20210101_02_T1_SR.tif")
            };
            let mut ds = drv.create_with_band_type::<f32, _>(dir.join(name), W, H, 11).unwrap();
            ds.set_geo_transform(&GT).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            if sensor.is_empty() {
                ds.set_metadata_item("STAC_PLATFORM", "landsat-7", "").unwrap();
            }
            if k == 5 {
                ds.set_metadata_item("STAC_GRANULE_METADATA", GRANULES, "").unwrap();
            }
            for (b, name) in NAMES.iter().enumerate() {
                let data: Vec<f32> = (0..W * H)
                    .map(|p| {
                        let wave = ((k * 3 + p) as f32 * 0.37 + b as f32).sin();
                        match b {
                            0..=5 => (900.0 + 700.0 * b as f32 + 300.0 * wave).round(),
                            6 => 32.0 + 4.0 * wave,
                            7 => 120.0 + p as f32 * 0.5 + 3.0 * wave,
                            8 => 4.0 + 2.0 * wave,
                            9 => 80.0 + 10.0 * wave,
                            _ => {
                                if (p + k) % 7 == 0 {
                                    8.0
                                } else {
                                    0.0
                                }
                            }
                        }
                    })
                    .collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.set_description(name).unwrap();
                band.write((0, 0), (W, H), &mut gdal::raster::Buffer::new((W, H), data)).unwrap();
            }
        }
        Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap())
    }

    /// The roles of the raw series as the tools' windows would hold them: its bands' and the four angles'.
    fn roles(info: &CubeInfo) -> BTreeMap<String, i32> {
        let mut roles = roles_of(info);
        for (k, a) in ANGLES.iter().enumerate() {
            roles.insert(a.to_string(), 7 + k as i32);
        }
        roles
    }

    fn input(info: &Arc<CubeInfo>, roles: BTreeMap<String, i32>, window: [usize; 4]) -> ArdInput {
        let chain = ChainInput {
            info: info.clone(),
            name: "s".into(),
            inputs: vec![],
            roles,
            window,
            scope: "test".into(),
            scope_index: 0,
            map_t: info.t() - 1,
            threads: 2,
        };
        ArdInput { chain, coarse: None }
    }

    fn step(id: &str) -> ChainStep {
        ChainStep { tool: manifest().tool(id).unwrap(), params: BTreeMap::new() }
    }

    /// A tool on its own: its spec built as the chain builds it, over `window` of `info`.
    fn alone(
        id: &str,
        info: &CubeInfo,
        roles: &BTreeMap<String, i32>,
        window: Option<[usize; 4]>,
        out: &Path,
    ) -> JobResult {
        let tool = manifest().tool(id).unwrap();
        std::fs::create_dir_all(out).unwrap();
        let mut spec: JobSpec = base_spec(info, tool, &BTreeMap::new(), info.t() - 1, 2);
        series_inputs(info, tool, roles, &out.join("in"), &mut spec).unwrap();
        spec.window = window;
        spec.output_dir = out.join("out").to_string_lossy().into_owned();
        run_job(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap()
    }

    /// A raster's bands (NoData as NaN).
    fn read(path: &str) -> Vec<Vec<f32>> {
        let ds = gdal::Dataset::open(path).unwrap();
        let (w, h) = ds.raster_size();
        (1..=ds.raster_count())
            .map(|b| ds.rasterband(b).unwrap().read_as::<f32>((0, 0), (w, h), (w, h), None).unwrap().data().to_vec())
            .collect()
    }

    fn same(a: &[f32], b: &[f32]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.to_bits() == y.to_bits() || x.is_nan() && y.is_nan())
    }

    fn tifs(outputs: &[Value]) -> Vec<&Value> {
        outputs.iter().filter(|o| o["path"].as_str().is_some_and(|p| p.ends_with(".tif"))).collect()
    }

    #[test]
    fn the_chain_is_the_tools_run_one_by_one_on_the_series_between_them() {
        let base = std::env::temp_dir().join("janus-ard-test-chain");
        let raw = series(&base.join("series"));
        assert_eq!(raw.t(), 8);
        assert_eq!((raw.sel.qa_band, raw.sel.qa_rule), (11, QaRule::LandsatC2));
        let roles = roles(&raw);
        let window = [1, 1, W - 1, H - 1];
        let steps = [step("nbar"), step("spectral_adjust"), step("tasseled_cap")];
        let chain = run_chain(
            &steps,
            &input(&raw, roles.clone(), window),
            &base.join("chain"),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(chain.skipped.is_empty(), "{:?}", chain.skipped);
        assert_eq!(chain.result.window, window);
        // by hand: NBAR on the raw series, the spectral adjustment on its bands assembled, Tasseled Cap on those
        let by_hand = base.join("by hand");
        let nbar = alone("nbar", &raw, &roles, Some(window), &by_hand.join("nbar"));
        let a1 = assemble(&raw, window, &band_series("nbar", &nbar.outputs), &by_hand.join("series1")).unwrap();
        let adjust = alone("spectral_adjust", &a1.info, &a1.roles, None, &by_hand.join("spectral_adjust"));
        let b2 = band_series("spectral_adjust", &adjust.outputs);
        let a2 = assemble(&raw, window, &b2, &by_hand.join("series2")).unwrap();
        let tc = alone("tasseled_cap", &a2.info, &a2.roles, None, &by_hand.join("tasseled_cap"));
        let want: Vec<Value> = [nbar.outputs, adjust.outputs, tc.outputs].concat();
        let got = tifs(&chain.result.outputs);
        assert_eq!(got.len(), want.len());
        assert_eq!(got.len(), 6 + 10 + 3, "{} files", got.len());
        let mut compared = 0;
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g["id"], w["id"]);
            let (gb, wb) = (read(g["path"].as_str().unwrap()), read(w["path"].as_str().unwrap()));
            assert_eq!(gb.len(), 8, "{}", g["id"]);
            assert_eq!(gb.len(), wb.len());
            for (b, (x, y)) in gb.iter().zip(&wb).enumerate() {
                assert!(same(x, y), "{} date {b}", g["id"]);
                assert!(x.iter().any(|v| v.is_finite()), "{} date {b} has no value", g["id"]);
                compared += x.len();
            }
        }
        assert!(compared > 19 * 8 * 20, "{compared} values compared");
        // the per-band series are written and kept from the layers; the harmonized series (the adjustment's) is the
        // one that is a layer, and the series the Classification can use are it and Tasseled Cap's
        let layer = |o: &Value| o["no_layer"].as_bool() != Some(true);
        let kept = chain.result.outputs.iter().filter(|o| o["series"] == json!(true) && !layer(o)).count();
        assert_eq!(kept, 6 + 10 + 1, "the bands' series and the harmonized series after NBAR");
        let harmonized: Vec<&Value> = chain.result.outputs.iter().filter(|o| o["id"] == "harmonized").collect();
        assert_eq!(harmonized.len(), 2);
        assert!(!layer(harmonized[0]) && layer(harmonized[1]));
        assert_eq!(harmonized[1]["name"], "Harmonized series");
        assert!(harmonized[1]["path"].as_str().unwrap().replace('\\', "/").ends_with("02_spectral_adjust/series"));
        let series = series_outputs(&chain.result);
        assert_eq!(series.len(), 1 + 3, "{series:?}");
        assert_eq!(series[0].0, "Harmonized series");
        // a step's folder: its spec, log, provenance (the harmonized series it read, for Tasseled Cap), Python
        let folder = base.join("chain").join("03_tasseled_cap");
        for f in ["job.json", "job.log", "provenance.json", "rerun.py"] {
            assert!(folder.join(f).is_file(), "{f}");
        }
        let p = Provenance::read(&folder).unwrap();
        assert_eq!((p.state.as_str(), p.tool.as_str()), ("done", "tasseled_cap"));
        let read_series = p.series.unwrap();
        assert_eq!(read_series.dates, 8);
        let read_from = read_series.inputs[0].replace('\\', "/");
        assert!(read_from.ends_with("02_spectral_adjust/series"), "{read_from}");
        // the adjustment's provenance lists the series it made
        let p = Provenance::read(&base.join("chain").join("02_spectral_adjust")).unwrap();
        assert!(p.outputs.iter().any(|o| o.name == "Harmonized series"));
    }

    /// A coarse series for ImproPhe: three scenes of 4 x 3 pixels of 60 m over the raw series' extent, in two bands.
    fn coarse_series(dir: &Path) -> Arc<CubeInfo> {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let srs = gdal::spatial_ref::SpatialRef::from_epsg(32722).unwrap();
        let day0 = janus_cube::dates::days_from_civil(2020, 1, 5);
        for (k, day) in [day0 + 10, day0 + 50, day0 + 90].into_iter().enumerate() {
            let (y, m, d) = janus_zeit::numeric::civil(day);
            let name = format!("coarse_{y:04}-{m:02}-{d:02}.tif");
            let mut ds = drv.create_with_band_type::<f32, _>(dir.join(name), 4, 3, 2).unwrap();
            ds.set_geo_transform(&[GT[0], 60.0, 0.0, GT[3], 0.0, -60.0]).unwrap();
            ds.set_spatial_ref(&srs).unwrap();
            for b in 0..2 {
                let data: Vec<f32> = (0..12).map(|p| 0.1 + 0.05 * ((p + 3 * k + b) as f32 * 0.9).sin().abs()).collect();
                let mut band = ds.rasterband(b + 1).unwrap();
                band.write((0, 0), (4, 3), &mut gdal::raster::Buffer::new((4, 3), data)).unwrap();
            }
        }
        Arc::new(open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap())
    }

    /// The Python of the chain has each tool's call once, NBAR's on the series' files and the others' on the harmonized
    /// series (a DataArray of NBAR's result, then of the adjustment's with its bands named by role), the results
    /// numbered along and written under their step's name; a step left out is a comment.
    #[test]
    fn the_chain_script_has_each_call_once_on_the_series_the_step_reads() {
        let base = std::env::temp_dir().join("janus-ard-test-code");
        let raw = series(&base.join("series"));
        let coarse = coarse_series(&base.join("coarse"));
        let window = [1, 1, W - 1, H - 1];
        let mut inp = input(&raw, roles(&raw), window);
        let steps = [step("nbar"), step("spectral_adjust"), step("tasseled_cap")];
        let code = chain_code(&steps, &[], &inp, "OUT").unwrap();
        for call in
            ["zeit.nbar(nbar_cube, angles,", "zeit.spectral_adjust(clear_cube,", "zeit.tasseled_cap(clear_cube,"]
        {
            assert_eq!(code.matches(call).count(), 1, "{call}\n{code}");
        }
        let at = |s: &str| code.find(s).unwrap_or_else(|| panic!("{s}\n{code}"));
        let order = [
            at("result1 = zeit.nbar(nbar_cube"),
            at("harmonized = result1  #"),
            at("result2 = zeit.spectral_adjust(clear_cube"),
            at("harmonized = result2.sel(band=[\"blue\", \"green\", \"red\", \"rededge1\""),
            at("result3 = zeit.tasseled_cap(clear_cube"),
            at("os.makedirs(out_dir"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
        // the two steps after NBAR read the bands from the harmonized series, NBAR's own from the files
        assert_eq!(code.matches("raw = {r: harmonized.sel(band=r).values for r in order}").count(), 2, "{code}");
        assert!(!code.contains("roles[r], window) for layer in layers]) for r in order}"), "{code}");
        assert!(code.contains("\"nir\", \"nir broad\", \"swir1\", \"swir2\"])"), "{code}");
        for name in ["01_nbar", "02_spectral_adjust", "03_tasseled_cap"] {
            assert!(code.contains(name), "{name}");
        }
        // a step left out: a comment, and the one after reads what the ones before left
        let part = chain_code(&steps, &[("spectral_adjust".into(), "why".into())], &inp, "OUT").unwrap();
        assert!(
            part.contains("# Spectral adjustment to Sentinel-2A: left out (why)") || part.contains("left out (why)")
        );
        assert!(!part.contains("zeit.spectral_adjust(") && part.contains("result2 = zeit.tasseled_cap(clear_cube"));
        assert!(part.contains("harmonized.sel(band=r)"), "{part}");
        // Tasseled Cap on the files when no step has harmonized the series
        let tc = chain_code(&[step("tasseled_cap")], &[], &inp, "OUT").unwrap();
        assert!(tc.contains("read(layer[0], roles[r], window)") && !tc.contains("harmonized"), "{tc}");
        let all: Vec<(String, String)> = steps.iter().map(|s| (s.tool.id.clone(), "why".into())).collect();
        assert!(chain_code(&steps, &all, &inp, "OUT").is_err());
        // ImproPhe: the harmonized series is its fine one, the coarse one is read from its files
        inp.coarse = Some(("coarse".into(), coarse));
        let steps = [step("nbar"), step("spectral_adjust"), step("improphe")];
        let code = chain_code(&steps, &[], &inp, "OUT").unwrap();
        assert!(code.contains("fine_raw = np.asarray(harmonized.values)"), "{code}");
        assert!(code.contains("[str(b) for b in harmonized.band.values]"), "{code}");
        assert!(code.contains("usable(q, qa_rule)") && code.contains("coarse_files = [("), "{code}");
        assert_eq!(code.matches("zeit.improphe(coarse_cube, fine_cube").count(), 1);
        assert!(code.contains("result3 = zeit.improphe("), "{code}");
        // without a coarse series ImproPhe cannot be in the script (the chain leaves it out)
        inp.coarse = None;
        assert!(chain_code(&steps, &[], &inp, "OUT").is_err());
    }

    /// The chain's script run in Zeit (the Python of JANUS_PYTHON, with zeit, rasterio, rioxarray and xarray; the test
    /// is left out without it) gives the values `run_chain` writes: NBAR, the adjustment, Tasseled Cap and ImproPhe,
    /// the bands of the steps after NBAR read from its result.
    #[test]
    fn the_chain_script_runs_in_zeit_and_gives_the_chains_values() {
        let Ok(python) = std::env::var("JANUS_PYTHON") else {
            println!("skipped: JANUS_PYTHON names no Python with zeit, rasterio, rioxarray and xarray");
            return;
        };
        let base = std::env::temp_dir().join("janus-ard-test-zeit");
        let raw = series(&base.join("series"));
        let coarse = coarse_series(&base.join("coarse"));
        let window = [1, 1, W - 1, H - 1];
        let mut inp = input(&raw, roles(&raw), window);
        inp.coarse = Some(("coarse".into(), coarse));
        let mut improphe = step("improphe");
        improphe.params = serde_json::from_value(
            json!({ "by": "scenes", "kernel_size": 90.0, "texture_size": 30.0, "pca": "principal" }),
        )
        .unwrap();
        let steps = [step("nbar"), step("spectral_adjust"), step("tasseled_cap"), improphe];
        let chain = run_chain(&steps, &inp, &base.join("chain"), &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert!(chain.skipped.is_empty(), "{:?}", chain.skipped);
        let out = base.join("python");
        let _ = std::fs::remove_dir_all(&out);
        let code = chain_code(&steps, &[], &inp, &out.to_string_lossy()).unwrap();
        let script = base.join("chain.py");
        std::fs::write(&script, code).unwrap();
        // the PATH as an activated conda environment has it and Windows' own: the other DLLs on the test's PATH (the
        // GDAL builds, another environment's libraries) shadow the environment's (pyexpat fails to load)
        let root = Path::new(&python).parent().unwrap().to_path_buf();
        let system = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        let mut path: Vec<String> = ["", "Library\\mingw-w64\\bin", "Library\\usr\\bin", "Library\\bin", "Scripts"]
            .iter()
            .map(|d| root.join(d).to_string_lossy().into_owned())
            .collect();
        path.extend([format!("{system}\\system32"), system.clone()]);
        let run = std::process::Command::new(python)
            .arg(&script)
            .env_clear()
            .envs(["SystemRoot", "TEMP", "TMP"].iter().filter_map(|k| Some((*k, std::env::var(k).ok()?))))
            .env("PATH", path.join(";"))
            .env("PYTHONNOUSERSITE", "1")
            .env("OMP_NUM_THREADS", "2")
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        // the script's bands by step (its files start with the step's folder name)
        let files: Vec<(String, Vec<Vec<f32>>)> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "tif"))
            .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), read(&p.to_string_lossy())))
            .collect();
        // the values are reflectance x 10000 (or sums of it): float32 steps of 1e-3 at the largest, so the difference is
        // judged against the value or 1000, whichever is larger
        let close = |a: f32, b: f32| (a.is_nan() && b.is_nan()) || (a - b).abs() <= 1e-6 * b.abs().max(1000.0);
        let (mut bands, mut values, mut worst) = (0, 0, 0f32);
        let mut per_step: BTreeMap<String, usize> = BTreeMap::new();
        for o in tifs(&chain.result.outputs) {
            let path = o["path"].as_str().unwrap();
            let step = Path::new(path).parent().unwrap().file_name().unwrap().to_string_lossy().into_owned();
            for w in &read(path) {
                // a band of the script's files of the step with the chain's values
                let hit = files
                    .iter()
                    .filter(|f| f.0.starts_with(&format!("{step}_")))
                    .flat_map(|f| &f.1)
                    .find(|g| g.len() == w.len() && g.iter().zip(w).all(|(&a, &b)| close(a, b)));
                let Some(g) = hit else {
                    // the closest band of the step in the script, to see how far it is
                    let gap = |g: &Vec<f32>| {
                        g.iter()
                            .zip(w)
                            .filter(|(a, b)| a.is_finite() && b.is_finite())
                            .fold(0f32, |m, (a, b)| m.max((a - b).abs()))
                    };
                    let nearest = files
                        .iter()
                        .filter(|f| f.0.starts_with(&format!("{step}_")))
                        .flat_map(|f| &f.1)
                        .filter(|g| g.len() == w.len())
                        .map(|g| (gap(g), g.iter().filter(|v| v.is_finite()).count()))
                        .fold((f32::MAX, 0), |m, x| if x.0 < m.0 { x } else { m });
                    panic!(
                        "no band of {step} in the script has the values of {} ({path}); the nearest differs by {} over {} values (the band has {})",
                        o["name"],
                        nearest.0,
                        nearest.1,
                        w.iter().filter(|v| v.is_finite()).count()
                    )
                };
                worst = g
                    .iter()
                    .zip(w)
                    .filter(|(a, b)| a.is_finite() && b.is_finite())
                    .fold(worst, |m, (a, b)| m.max((a - b).abs() / b.abs().max(1000.0)));
                values += w.iter().filter(|v| v.is_finite()).count();
                bands += 1;
                *per_step.entry(step.clone()).or_default() += 1;
            }
        }
        println!(
            "{bands} bands, {values} finite values compared ({per_step:?}), the largest relative difference {worst:e}"
        );
        assert_eq!(per_step.len(), 4, "{per_step:?}");
        assert!(bands >= 19 * 8 && values > 3000);
    }

    #[test]
    fn the_assembled_series_has_the_dates_roles_quality_band_and_per_date_metadata_of_the_raw_one() {
        let base = std::env::temp_dir().join("janus-ard-test-assemble");
        let raw = series(&base.join("series"));
        let roles = roles(&raw);
        // the raw series' answers, which the per-date lookups must give again
        let sensors = janus_zeit::sensors::of_series(&raw);
        assert_eq!(sensors, ["LND08", "LND09", "LND07", "LND07", "LND09", "LND08", "LND07", "LND09"]);
        let granules = janus_zeit::granules::of_series(&raw);
        assert_eq!(granules[5], GRANULES.split(' ').collect::<Vec<_>>());
        assert_eq!(granules[0].len(), 1, "{granules:?}");
        for window in [[0, 0, W, H], [2, 1, W, H - 1]] {
            let out = alone("nbar", &raw, &roles, Some(window), &base.join(format!("nbar{}", window[0])));
            // NBAR's c-factors, if asked, are not bands of the series
            let bands = band_series("nbar", &out.outputs);
            let described: Vec<&str> = bands.iter().map(|b| b.description.as_str()).collect();
            assert_eq!(described, ["blue", "green", "red", "nir", "swir1", "swir2"]);
            let dir = base.join(format!("series{}", window[0]));
            let a = assemble(&raw, window, &bands, &dir).unwrap();
            let info = &a.info;
            // the dates: the raw series', and the VRTs named as the raw files are
            assert!(info.time_is_date);
            assert_eq!(info.t(), raw.t());
            let times = |i: &CubeInfo| i.layers.iter().map(|l| l.time).collect::<Vec<_>>();
            assert_eq!(times(info), times(&raw));
            let stems = |i: &CubeInfo| i.layers.iter().map(|l| janus_cube::display_name(&l.path)).collect::<Vec<_>>();
            assert_eq!(stems(info), stems(&raw));
            assert_eq!((info.width, info.height), (window[2] - window[0], window[3] - window[1]));
            // the bands by role, the angle bands not carried, the quality band and its rule as the raw series has them
            assert_eq!(info.band_names, ["blue", "green", "red", "nir", "swir1", "swir2", "QA_PIXEL"]);
            for (k, r) in SIX.iter().enumerate() {
                assert_eq!(a.roles[*r], k as i32 + 1, "{r}");
                assert_eq!(a.roles[*r], roles[*r], "{r}: the same bands as the raw series' roles");
            }
            assert_eq!(a.roles.len(), 6);
            assert_eq!((info.sel.qa_band, info.sel.qa_rule), (7, QaRule::LandsatC2));
            // the values: each band the NBAR series' band of the date, the quality band the raw codes over the window
            let (w, h) = (window[2] - window[0], window[3] - window[1]);
            let files: Vec<Vec<Vec<f32>>> = bands.iter().map(|b| read(&b.file)).collect();
            for (t, layer) in info.layers.iter().enumerate() {
                let vrt = read(&layer.path);
                assert_eq!(vrt.len(), 7);
                for (k, b) in bands.iter().enumerate() {
                    assert!(same(&vrt[k], &files[k][t]), "{} date {t}", b.description);
                }
                let codes = &read(&raw.layers[t].path)[10];
                let cut: Vec<f32> = (window[1]..window[3])
                    .flat_map(|r| (window[0]..window[2]).map(move |c| codes[r * W + c]))
                    .collect();
                assert_eq!(cut.len(), w * h);
                assert!(same(&vrt[6], &cut), "the quality band of date {t}");
            }
            // the per-date lookups give the raw series' answers
            assert_eq!(janus_zeit::sensors::of_series(info), sensors);
            assert_eq!(janus_zeit::granules::of_series(info), granules);
        }
    }

    #[test]
    fn a_step_the_series_cannot_take_is_left_out_with_why() {
        let base = std::env::temp_dir().join("janus-ard-test-left-out");
        let raw = series(&base.join("series"));
        // no angle bands mapped: NBAR cannot run; no coarse series: ImproPhe cannot; Tasseled Cap runs on the raw series
        let mut no_angles = roles(&raw);
        for a in ANGLES {
            no_angles.remove(a);
        }
        let steps = [step("nbar"), step("tasseled_cap"), step("improphe")];
        let predicted = left_out(&steps, &raw, &no_angles, None);
        assert_eq!(predicted.iter().map(|p| p.0.as_str()).collect::<Vec<_>>(), ["nbar", "improphe"]);
        assert!(predicted[0].1.contains("map the angle bands first: sza, saa, vza, vaa"), "{predicted:?}");
        assert!(predicted[1].1.contains("coarse series"), "{predicted:?}");
        let chain = run_chain(
            &steps,
            &input(&raw, no_angles.clone(), [0, 0, W, H]),
            &base.join("chain"),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(chain.skipped, predicted);
        assert_eq!(tifs(&chain.result.outputs).len(), 3);
        assert!(chain.result.outputs.iter().all(|o| o["path"].as_str().unwrap().contains("tasseled_cap")));
        assert!(!base.join("chain").join("01_nbar").exists() && !base.join("chain").join("03_improphe").exists());
        // with the angles from Sentinel-2's metadata they are not needed (the metadata is another matter)
        let s2 = ChainStep {
            tool: manifest().tool("nbar").unwrap(),
            params: BTreeMap::from([("angles".to_string(), json!("sentinel2"))]),
        };
        assert!(left_out(&[s2], &raw, &no_angles, None).is_empty());
        // judged on the series the steps before would leave: the adjustment needs six reflectance bands NBAR made,
        // and a series without a blue band cannot give them
        let mut no_blue = roles(&raw);
        no_blue.remove("blue");
        let steps = [step("nbar"), step("spectral_adjust")];
        let predicted = left_out(&steps, &raw, &no_blue, None);
        assert_eq!(predicted.len(), 1, "{predicted:?}");
        assert_eq!(predicted[0].0, "spectral_adjust");
        assert!(predicted[0].1.contains("choose the band of: blue"), "{predicted:?}");
        // only steps it cannot take: no chain
        let e = run_chain(
            &[step("nbar")],
            &input(&raw, no_angles, [0, 0, W, H]),
            &base.join("none"),
            &|_, _| {},
            &AtomicBool::new(false),
        )
        .err()
        .expect("an error");
        assert!(e.contains("no step could run") && e.contains("nbar"), "{e}");
    }

    #[test]
    fn a_cancelled_chain_stops() {
        let base = std::env::temp_dir().join("janus-ard-test-cancel");
        let raw = series(&base.join("series"));
        let roles = roles(&raw);
        let e = run_chain(
            &[step("nbar")],
            &input(&raw, roles.clone(), [0, 0, W, H]),
            &base.join("chain"),
            &|_, _| {},
            &AtomicBool::new(true),
        )
        .err()
        .expect("an error");
        assert_eq!(e, "cancelled");
        // asked while a step runs: the step stops and so does the chain
        let cancel = AtomicBool::new(false);
        let e = run_chain(
            &[step("nbar"), step("spectral_adjust")],
            &input(&raw, roles, [0, 0, W, H]),
            &base.join("during"),
            &|_, _| cancel.store(true, Ordering::Relaxed),
            &cancel,
        )
        .err()
        .expect("an error");
        assert_eq!(e, "cancelled");
        assert!(!base.join("during").join("02_spectral_adjust").exists());
    }

    // ---- what to look at ----

    /// A series in memory (no file): `n` dates every 16 days from 2020-01-05, 11 bands a date and a quality band.
    fn mem(n: usize) -> CubeInfo {
        let day0 = janus_cube::dates::days_from_civil(2020, 1, 5);
        let layers = (0..n)
            .map(|k| janus_cube::Layer { time: (day0 + 16 * k as i64) as f64 * 86400.0, band: 1, ..Default::default() })
            .collect();
        CubeInfo {
            layers,
            width: 4,
            height: 4,
            time_is_date: true,
            bands_per_date: 11,
            band_names: NAMES.iter().map(|s| s.to_string()).collect(),
            sel: BandSelection { band: 3, qa_band: 11, qa_rule: QaRule::LandsatC2, chosen: true, ..Default::default() },
            ..Default::default()
        }
    }

    fn with(id: &str, params: &[(&str, Value)]) -> ChainStep {
        ChainStep {
            tool: manifest().tool(id).unwrap(),
            params: params.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        }
    }

    fn six_and_angles() -> BTreeMap<String, i32> {
        roles(&mem(1))
    }

    fn has(a: &[Advice], level: u8, part: &str) -> bool {
        a.iter().any(|x| x.level == level && x.text.contains(part))
    }

    fn look<'a>(info: &'a CubeInfo, roles: &'a BTreeMap<String, i32>, facts: &'a DateFacts) -> Look<'a> {
        Look { info, roles, coarse: None, facts, shown_hi: None }
    }

    #[test]
    fn nothing_on_is_level_two_and_a_clean_chain_has_nothing_to_look_at() {
        let info = mem(8);
        let r = six_and_angles();
        let none = DateFacts::default();
        assert_eq!(advice(&[], &look(&info, &r, &none)), vec![Advice { level: 2, text: "No step is on.".into() }]);
        let facts = DateFacts { sensors: vec!["LND08".into(); 8], hls: vec![false; 8], ..Default::default() };
        let a = advice(&[step("nbar"), step("spectral_adjust")], &look(&info, &r, &facts));
        assert!(a.iter().all(|x| x.level == 0), "{a:?}");
    }

    #[test]
    fn dates_without_a_sensor_stop_the_steps_that_read_it_and_up_to_five_are_named() {
        let info = mem(8);
        let r = six_and_angles();
        let mut sensors = vec!["LND08".to_string(); 8];
        sensors[1].clear();
        sensors[3].clear();
        let facts = DateFacts { sensors: sensors.clone(), ..Default::default() };
        let adjust = [step("spectral_adjust")];
        let a = advice(&adjust, &look(&info, &r, &facts));
        assert!(has(&a, 2, "2 of 8 dates have no sensor (2020-01-21, 2020-02-22)"), "{a:?}");
        assert!(has(&a, 2, "Spectral adjustment to Sentinel-2A (sensor: auto)"), "{a:?}");
        // the sensor named in the form: nothing to read
        let named = [with("spectral_adjust", &[("sensor", json!("LND08"))])];
        assert!(advice(&named, &look(&info, &r, &facts)).iter().all(|x| x.level == 0));
        // Tasseled Cap with the coefficients of each sensor reads them too; with Crist's it does not
        let tc = [with("tasseled_cap", &[("coefficients", json!("sensor"))])];
        assert!(has(&advice(&tc, &look(&info, &r, &facts)), 2, "Tasseled Cap (coefficients by sensor)"));
        let crist = [with("tasseled_cap", &[("coefficients", json!("crist1985"))])];
        assert!(advice(&crist, &look(&info, &r, &facts)).iter().all(|x| x.level == 0));
        // seven dates without: five named and the count of the rest
        let facts = DateFacts { sensors: vec![String::new(); 8], ..Default::default() };
        let a = advice(&adjust, &look(&info, &r, &facts));
        assert!(
            has(
                &a,
                2,
                "8 of 8 dates have no sensor (2020-01-05, 2020-01-21, 2020-02-06, 2020-02-22, 2020-03-09 and 3 more)"
            ),
            "{a:?}"
        );
    }

    #[test]
    fn sentinel_2_dates_stop_the_adjustment_unless_its_sensor_is_named() {
        let info = mem(4);
        let r = six_and_angles();
        let facts =
            DateFacts { sensors: ["LND08", "SEN2", "LND09", "SEN2"].map(String::from).to_vec(), ..Default::default() };
        let a = advice(&[step("spectral_adjust")], &look(&info, &r, &facts));
        assert!(has(&a, 2, "2 of 4 dates are Sentinel-2 (2020-01-21, 2020-02-22)"), "{a:?}");
        assert!(has(&a, 2, "Turn it off for a Sentinel-2 series"), "{a:?}");
        let named = [with("spectral_adjust", &[("sensor", json!("LND08"))])];
        assert!(advice(&named, &look(&info, &r, &facts)).iter().all(|x| x.level == 0));
        // Tasseled Cap on Sentinel-2 dates is not refused
        assert!(advice(&[step("tasseled_cap")], &look(&info, &r, &facts)).iter().all(|x| x.level == 0));
    }

    #[test]
    fn nbar_needs_its_angles_from_bands_or_from_granule_metadata() {
        let info = mem(4);
        let r = six_and_angles();
        // from Sentinel-2's metadata: the dates that have none
        let facts = DateFacts { granules: vec![true, false, true, false], ..Default::default() };
        let s2 = [with("nbar", &[("angles", json!("sentinel2"))])];
        let a = advice(&s2, &look(&info, &r, &facts));
        assert!(has(&a, 2, "which 2 of 4 dates lack (2020-01-21, 2020-02-22)"), "{a:?}");
        let facts = DateFacts { granules: vec![true; 4], ..Default::default() };
        assert!(advice(&s2, &look(&info, &r, &facts)).iter().all(|x| x.level == 0));
        // from the bands: the four roles mapped, or the step is left out
        let mut no_angles = r.clone();
        for a in ANGLES {
            no_angles.remove(a);
        }
        let a = advice(&[step("nbar")], &look(&info, &no_angles, &DateFacts::default()));
        assert!(has(&a, 2, "NBAR (c-factor): map the angle bands first: sza, saa, vza, vaa"), "{a:?}");
        // the metadata stands for the bands: no angle band is needed
        assert!(!advice(&s2, &look(&info, &no_angles, &facts)).iter().any(|x| x.text.contains("angle bands")));
    }

    #[test]
    fn hls_dates_under_nbar_are_level_one() {
        let info = mem(4);
        let r = six_and_angles();
        let facts = DateFacts { hls: vec![true, true, false, true], ..Default::default() };
        let a = advice(&[step("nbar")], &look(&info, &r, &facts));
        assert!(has(&a, 1, "3 of 4 dates are HLS (2020-01-05, 2020-01-21, 2020-02-22), already normalised"), "{a:?}");
        // the facts are only read under NBAR
        let a = advice(&[step("tasseled_cap")], &look(&info, &r, &facts));
        assert!(!a.iter().any(|x| x.text.contains("HLS")), "{a:?}");
    }

    #[test]
    fn a_band_shown_beyond_the_units_a_step_is_told_looks_unscaled() {
        let info = mem(4);
        let r = six_and_angles();
        let facts = DateFacts { sensors: vec!["LND08".into(); 4], ..Default::default() };
        let at = |hi: f32| Look { shown_hi: Some(hi), ..look(&info, &r, &facts) };
        let unit = [with("spectral_adjust", &[("units", json!("unit"))])];
        assert!(has(&advice(&unit, &at(8000.0)), 1, "reaches 8000, beyond reflectance 0-1"));
        assert!(!has(&advice(&unit, &at(1.2)), 1, "reaches"));
        let scaled = [with("tasseled_cap", &[("units", json!("x10000"))])];
        assert!(has(&advice(&scaled, &at(30000.0)), 1, "beyond reflectance x 10000, which the units of Tasseled Cap"));
        assert!(!has(&advice(&scaled, &at(8000.0)), 1, "reaches"));
        // auto tells digital numbers by itself; no stretch to look at: nothing to say
        assert!(!has(&advice(&[step("spectral_adjust")], &at(30000.0)), 1, "reaches"));
        assert!(!has(&advice(&unit, &look(&info, &r, &facts)), 1, "reaches"));
    }

    #[test]
    fn improphe_needs_a_coarse_series_in_the_same_crs_and_a_series_without_a_quality_band_is_noted() {
        let mut info = mem(4);
        info.crs_authority = "EPSG:32722".into();
        let r = six_and_angles();
        let facts = DateFacts::default();
        let a = advice(&[step("improphe")], &look(&info, &r, &facts));
        assert!(has(&a, 2, "ImproPhe: choose the coarse series"), "{a:?}");
        let mut other = mem(4);
        other.crs_authority = "EPSG:4326".into();
        let l = Look { coarse: Some(&other), ..look(&info, &r, &facts) };
        assert!(has(&advice(&[step("improphe")], &l), 2, "another CRS"));
        let same = info.clone();
        let l = Look { coarse: Some(&same), ..look(&info, &r, &facts) };
        assert!(advice(&[step("improphe")], &l).iter().all(|x| x.level == 0));
        info.sel.qa_band = 0;
        assert!(has(
            &advice(&[step("improphe")], &Look { coarse: Some(&same), ..look(&info, &r, &facts) }),
            0,
            "no quality band"
        ));
    }

    /// A series of `names.len()` dates, one band each, with the file names and STAC collection of each.
    fn named_series(dir: &Path, files: &[(&str, &str)]) -> CubeInfo {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        for (name, collection) in files {
            let mut ds = drv.create_with_band_type::<f32, _>(dir.join(name), 4, 4, 1).unwrap();
            ds.set_geo_transform(&GT).unwrap();
            if !collection.is_empty() {
                ds.set_metadata_item("STAC_COLLECTION", collection, "").unwrap();
            }
        }
        open_cube(&[dir.to_string_lossy().into_owned()], &BandSelection::default()).unwrap()
    }

    #[test]
    fn the_dates_facts_are_read_from_the_files() {
        let base = std::env::temp_dir().join("janus-ard-test-facts");
        let raw = series(&base.join("series"));
        let steps = [step("nbar"), step("spectral_adjust")];
        let facts = date_facts(&steps, &raw);
        assert_eq!(facts.sensors.len(), 8);
        // the fourth date is named for no sensor but its STAC platform says Landsat 7
        assert_eq!(facts.sensors[3], "LND07");
        assert_eq!(facts.sensors[0], "LND08");
        assert!(facts.granules.is_empty(), "NBAR reads the angle bands: no granule is looked for");
        assert_eq!(facts.hls, vec![false; 8]);
        // from Sentinel-2's metadata: a MTD_TL.xml beside the files stands for every date
        let s2 = [with("nbar", &[("angles", json!("sentinel2"))])];
        assert_eq!(date_facts(&s2, &raw).granules, vec![true; 8]);
        // nothing that reads them on: nothing read
        let none = date_facts(&[step("tasseled_cap")], &raw);
        assert!(none.sensors.is_empty() && none.granules.is_empty() && none.hls.is_empty());
        // HLS by the file name or by the STAC collection
        let hls = named_series(
            &base.join("hls"),
            &[("HLS.L30.T22KDV.2020005.v2.0.tif", ""), ("scene_20200121.tif", "hls2-l30"), ("scene_20200206.tif", "")],
        );
        assert_eq!(hls.t(), 3);
        let facts = date_facts(&steps, &hls);
        assert_eq!(facts.hls.iter().filter(|h| **h).count(), 2, "{:?}", facts.hls);
        assert!(!facts.hls[2]);
    }

    // ---- the preview ----

    #[test]
    fn the_preview_runs_the_chain_on_the_roi_alone_into_its_folder_and_ending_it_removes_the_folder() {
        let base = std::env::temp_dir().join("janus-ard-test-preview");
        let raw = series(&base.join("series"));
        let roles = roles(&raw);
        let roi = [2, 1, 6, 5];
        let dir = preview_dir(&base.join("work").join("preview"), "t0_1");
        assert!(dir.ends_with("ard_t0_1"));
        std::fs::create_dir_all(&dir).unwrap();
        let steps = [step("nbar"), step("spectral_adjust")];
        let chain = run_chain(&steps, &input(&raw, roles, roi), &dir, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        assert_eq!(chain.result.window, roi);
        assert!(chain.skipped.is_empty(), "{:?}", chain.skipped);
        // everything it wrote is in the preview's folder and the size of the ROI, the harmonized series included
        let tifs = tifs(&chain.result.outputs);
        assert_eq!(tifs.len(), 6 + 10);
        for o in &tifs {
            let path = o["path"].as_str().unwrap();
            assert!(Path::new(path).starts_with(&dir), "{path}");
            assert_eq!(gdal::Dataset::open(path).unwrap().raster_size(), (4, 4), "{}", o["id"]);
        }
        let harmonized = chain.result.outputs.iter().find(|o| o["id"] == "harmonized").unwrap();
        let series_dir = Path::new(harmonized["path"].as_str().unwrap());
        assert!(series_dir.starts_with(&dir));
        let first = std::fs::read_dir(series_dir).unwrap().next().unwrap().unwrap().path();
        let ds = gdal::Dataset::open(&first).unwrap();
        assert_eq!(ds.raster_size(), (4, 4));
        let gt = ds.geo_transform().unwrap();
        assert_eq!([gt[0], gt[3]], [GT[0] + 2.0 * 30.0, GT[3] - 30.0]);
        drop(ds);
        // the input series is untouched and outside the folder
        assert!(base.join("series").is_dir() && !Path::new(&raw.first_path).starts_with(&dir));
        // ending the preview removes its folder
        crate::zeit::remove_dir_soon(dir.to_string_lossy().into_owned()).join().unwrap();
        assert!(!dir.exists());
        assert!(base.join("series").is_dir());
    }
}
