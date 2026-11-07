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

use egui::{ComboBox, RichText, Ui};
use gdal::Metadata;
use janus_cube::{BandSelection, CubeInfo, QaRule};
use janus_zeit::job::JobResult;
use janus_zeit::manifest::{Tool, manifest};
use serde_json::{Value, json};

use crate::app::State;
use crate::chain::{Chain, ChainInput, ChainStep, JobStart, StepRun, cancelled, run_step, spawn_job};
use crate::provenance::{OutputRecord, Provenance, SeriesRecord};
use crate::zeit::{JobState, applicability};

/// The id of the chain's task in Tools > Tasks (not a tool of the manifest).
pub(crate) const JOB_TOOL: &str = "ard";

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
}

impl Default for ArdUi {
    fn default() -> ArdUi {
        // NBAR and the spectral adjustment: the harmonized series
        ArdUi { open: false, tab: 0, on: [true, true, false, false], scope: 0, series_out: Arc::default() }
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
                p.outputs.push(OutputRecord { name: "Harmonized series".into(), path: dir.to_string_lossy().into() });
                p.write(folder);
            }
            if let Some(prev) = harmonized {
                outputs[prev]["no_layer"] = json!(true);
            }
            harmonized = Some(outputs.len());
            outputs.push(json!({"id": "harmonized", "name": "Harmonized series", "path": dir.to_string_lossy(),
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
        if self.ard_running() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    fn ard_running(&self) -> bool {
        self.zeit.jobs.iter().any(|j| j.tool == JOB_TOOL && j.status.lock().unwrap().state == JobState::Running)
    }

    pub(crate) fn ard_ui(&mut self, ui: &mut Ui) {
        let Some(info) = self.session.as_ref().map(|s| s.info.clone()) else {
            ui.colored_label(crate::theme::error(), "Open a series first.");
            return;
        };
        ui.horizontal(|ui| {
            for (i, name) in ["Input", "Steps", "Output"].into_iter().enumerate() {
                ui.selectable_value(&mut self.ard.tab, i, RichText::new(name).strong());
            }
        });
        ui.separator();
        match self.ard.tab {
            0 => self.ard_input_tab(ui, &info),
            1 => self.ard_steps_tab(ui, &info),
            _ => self.ard_output_tab(ui, &info),
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
        // the last chain: its progress and Cancel, or how it ended and its folder
        let mut open_folder = None;
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
                }
            }
        }
        if let Some(d) = open_folder {
            crate::platform::open_in_explorer(&d);
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
    fn ard_chain_input(&mut self, info: &Arc<CubeInfo>) -> (ArdInput, String) {
        let layer = self.layers.get(self.active).map(|l| (l.name.clone(), l.inputs.clone()));
        let inputs = layer.as_ref().map(|l| l.1.clone()).unwrap_or_default();
        let name = crate::zeit::series_name(info, &inputs);
        let (window, scope) = self.run_window(self.ard.scope);
        let chain = ChainInput {
            info: info.clone(),
            name: layer.map_or(name.clone(), |l| l.0),
            inputs,
            roles: self.active_roles(),
            window,
            scope: scope.to_string(),
            scope_index: self.ard.scope,
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
        let (input, name) = self.ard_chain_input(&info);
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
}
