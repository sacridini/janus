//! The way in: the Welcome window (Help > Welcome only: never shown by itself at the start), the sample data it
//! downloads, the "?" button of the tools' windows that opens their guide on the site, and the parameters' hover text
//! with its default.
//!
//! The samples are listed by a manifest (JSON, `SAMPLES_URL`, or the URL or path in JANUS_SAMPLES_URL): for each the
//! files to download (with their SHA-256), and what to open once they are there. They are downloaded on request, in
//! the background (a task of Tools > Tasks, in the line of downloads), into `<Janus data folder>/samples/<id>/`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::State;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

/// The site's address (the guides are pages of it).
pub const SITE: &str = "https://sacridini.github.io/janus";
/// The manifest of the sample data, an asset of a release of the repository.
pub const SAMPLES_URL: &str = "https://github.com/sacridini/janus/releases/download/sample-data-v1/samples.json";
/// The marker a finished download leaves in its folder.
const COMPLETE: &str = ".complete";

// ---------------------------------------------------------------------------------------------------------------
// The guides: which page of the site goes with which tool.

/// The site's list of methods (`id`, `guide` of each), the page of a method.
const METHODS_JSON: &str = include_str!("../../../site/data/methods.json");

/// The method of each Zeit tool (the id of site/data/methods.json), for the tools whose id is not the method's.
const TOOL_METHODS: &[(&str, &str)] = &[
    ("agreement", "agreement"),
    ("bfast", "bfast"),
    ("bfast_lite", "bfast-lite"),
    ("bfast_monitor", "bfast-monitor"),
    ("beast", "beast"),
    ("ccdc", "ccdc"),
    ("ccdc_synthetic", "ccdc-synthetic-image"),
    ("sccd", "s-ccd"),
    ("coded", "coded"),
    ("landtrendr", "landtrendr"),
    ("mann_kendall", "mann-kendall"),
    ("ndfi", "ndfi"),
    ("phenology", "phenology"),
    ("smooth", "smoothing"),
    ("som", "som"),
    ("kmeans", "k-means"),
    ("tmask", "tmask"),
    ("twdtw", "twdtw"),
    ("postproc", "post-processing"),
    ("snic", "snic"),
    ("texture", "texture"),
    ("ar_trend", "ar-1-trend"),
    ("recovery", "recovery"),
    ("ewmacd", "ewmacd"),
    ("unmix", "spectral-unmixing"),
    ("unmix_spectrum", "spectral-unmixing"),
    ("hotspots", "emerging-hot-spots"),
    ("interpolate", "interpolate"),
    ("temporal_metrics", "temporal-metrics"),
    ("fold", "fold-by-period"),
    ("standardize", "standardize"),
    ("linear_trend", "linear-trend-of-a-fold"),
    ("cat", "cat"),
    ("clear_observations", "clear-sky-observations"),
    ("polar_metrics", "polar-phenology"),
];

/// The page (and the section) of the tools of Janus itself, and of the Zeit tools whose guide is not the one of their
/// method (the data-preparation tools: their sections of the Data preparation guide).
pub const NATIVE_PAGES: &[(&str, &str)] = &[
    ("calculator", "calculator.html"),
    ("validation", "validation.html"),
    ("tune", "tune.html"),
    ("eof", "eof.html"),
    ("lagged_correlation", "eof.html"),
    ("mnf", "envi.html#mnf"),
    ("cva", "envi.html#cva"),
    ("imad", "envi.html#imad"),
    ("radcal", "envi.html#radcal"),
    ("omnibus", "envi.html#omnibus"),
    ("topocorr", "data-preparation.html#topographic-correction"),
    ("coregister", "data-preparation.html#co-registration"),
    ("alerts", "docs-usage.html#official-alerts-against-a-map-of-change"),
    ("stac", "stac.html"),
    ("classification", "deep-learning.html"),
    ("fields", "fields.html"),
    ("unmixing", "unmixing.html"),
    ("tsa", "docs-usage.html#time-series-analysis-force"),
    ("ard", "docs-usage.html#analysis-ready-data-force"),
    ("level2", "data-preparation.html#level2"),
    ("tmask", "data-preparation.html#tmask"),
    ("clear_observations", "data-preparation.html#clear-observations"),
    ("nbar", "data-preparation.html#nbar"),
    ("spectral_adjust", "data-preparation.html#spectral-adjust"),
    ("harmonize", "data-preparation.html#harmonize"),
    ("improphe", "data-preparation.html#improphe"),
    ("interpolate", "data-preparation.html#interpolate"),
    ("smooth", "data-preparation.html#smoothing"),
    ("standardize", "data-preparation.html#standardize"),
    ("regularize", "data-preparation.html#regularize"),
    ("speckle", "data-preparation.html#speckle"),
    ("warp", "data-preparation.html#warp"),
    ("build", "series.html"),
    ("save", "series.html"),
    ("rasterize", "maps-to-numbers.html"),
    ("polygonize", "maps-to-numbers.html"),
    ("zonal", "maps-to-numbers.html"),
    ("profiles", "maps-to-numbers.html"),
    ("tasseled_cap", "docs-usage.html#analysis-ready-data-force"),
];

#[derive(Deserialize)]
struct MethodEntry {
    id: String,
    #[serde(default)]
    guide: Option<String>,
}

/// site/data/methods.json: (id, guide page without ".html", None when the method has no guide of its own).
fn methods() -> &'static [(String, Option<String>)] {
    static M: LazyLock<Vec<(String, Option<String>)>> = LazyLock::new(|| {
        serde_json::from_str::<Vec<MethodEntry>>(METHODS_JSON)
            .unwrap_or_default()
            .into_iter()
            .map(|m| (m.id, m.guide.filter(|g| !g.is_empty())))
            .collect()
    });
    &M
}

/// The page of the site (relative: "landtrendr.html", "methods.html#tmask", "docs-usage.html#section") that goes with
/// a tool: the id of a Zeit tool, or the name of a tool of Janus (`NATIVE_PAGES`).
pub fn guide_page(key: &str) -> Option<String> {
    if let Some((_, page)) = NATIVE_PAGES.iter().find(|(k, _)| *k == key) {
        return Some((*page).to_string());
    }
    let (_, method) = TOOL_METHODS.iter().find(|(k, _)| *k == key)?;
    let (id, guide) = methods().iter().find(|(id, _)| id == method)?;
    // a method without a guide of its own: its place in the list of methods
    Some(match guide {
        Some(g) => format!("{g}.html"),
        None => format!("methods.html#{id}"),
    })
}

/// The address of a page of the site.
pub fn page_url(page: &str) -> String {
    format!("{SITE}/{}", page.trim_start_matches('/'))
}

/// The address of the guide that goes with a tool.
#[cfg(test)]
pub fn guide_url(key: &str) -> Option<String> {
    guide_page(key).map(|p| page_url(&p))
}

// ---------------------------------------------------------------------------------------------------------------
// What the interface does for a test: no browser, buttons that can be asked about and pressed.

#[derive(Default)]
struct Probe {
    on: bool,
    /// the buttons drawn (their keys) since the probe was turned on
    drawn: Vec<String>,
    /// the buttons to be taken as pressed the next time they are drawn
    press: Vec<String>,
    /// the addresses that would have opened in the browser
    urls: Vec<String>,
}

static PROBE: Mutex<Probe> = Mutex::new(Probe { on: false, drawn: Vec::new(), press: Vec::new(), urls: Vec::new() });

fn probe() -> std::sync::MutexGuard<'static, Probe> {
    PROBE.lock().unwrap_or_else(|e| e.into_inner())
}

/// The self-test's hooks: no browser (the addresses are kept), the buttons drawn are listed and one can be pressed.
pub mod test_hooks {
    use super::probe;

    pub fn start() {
        let mut p = probe();
        *p = Default::default();
        p.on = true;
    }

    pub fn stop() {
        *probe() = Default::default();
    }

    /// Whether the button with this key was drawn since `start` (or since the keys were cleared).
    pub fn drawn(key: &str) -> bool {
        probe().drawn.iter().any(|k| k == key)
    }

    pub fn clear_drawn() {
        probe().drawn.clear();
    }

    /// The button with this key is taken as pressed the next time it is drawn.
    pub fn press(key: &str) {
        probe().press.push(key.to_string());
    }

    /// The addresses opened so far.
    pub fn urls() -> Vec<String> {
        probe().urls.clone()
    }
}

/// A button's click, as the probe may say it: noted as drawn, and true when the test pressed it.
fn clicked(r: &egui::Response, key: &str) -> bool {
    let mut p = probe();
    if p.on {
        if !p.drawn.iter().any(|k| k == key) {
            p.drawn.push(key.to_string());
        }
        if let Some(i) = p.press.iter().position(|k| k == key) {
            p.press.remove(i);
            return true;
        }
    }
    r.clicked()
}

/// An address opened in the browser (kept instead, for the tests).
pub fn open_url(ctx: &egui::Context, url: &str) {
    {
        let mut p = probe();
        if p.on {
            p.urls.push(url.to_string());
            return;
        }
    }
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

/// A "?" at the right of a tool's window, one line: its guide on the site in the browser. Nothing for a tool without
/// a page.
pub fn guide_button(ui: &mut Ui, key: &str) {
    let Some(page) = guide_page(key) else { return };
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        let r = ui.small_button("?").on_hover_text(format!("Guide: {page}"));
        if clicked(&r, &format!("guide:{key}")) {
            open_url(ui.ctx(), &page_url(&page));
        }
    });
}

// ---------------------------------------------------------------------------------------------------------------
// The parameters' hover text.

/// A parameter's default as text, None when there is nothing to say (a list, an empty text).
fn default_text(p: &janus_zeit::Param) -> Option<String> {
    let unit = p.unit.as_deref().filter(|u| !u.is_empty()).map_or(String::new(), |u| format!(" {u}"));
    let label_of = |o: &str| {
        let options = p.options.as_deref().unwrap_or_default();
        let labels = p.labels.as_deref().unwrap_or_default();
        options.iter().position(|x| x == o).and_then(|i| labels.get(i)).map_or_else(|| o.to_string(), Clone::clone)
    };
    match p.kind.as_str() {
        "int" => p.default.as_i64().map(|v| format!("{v}{unit}")),
        "float" => p.default.as_f64().map(|v| format!("{v}{unit}")),
        "bool" => p.default.as_bool().map(|v| (if v { "on" } else { "off" }).to_string()),
        "enum" => p.default.as_str().filter(|s| !s.is_empty()).map(label_of),
        "text" | "file" | "folder" => p.default.as_str().filter(|s| !s.is_empty()).map(str::to_string),
        "multi" => {
            let v: Vec<String> =
                p.default.as_array()?.iter().filter_map(|o| o.as_str()).map(label_of).collect::<Vec<_>>();
            (!v.is_empty()).then(|| v.join(", "))
        }
        _ => None,
    }
}

/// The hover text of a parameter: its help, and "Default: ..." when it has a default and the help does not say it.
pub fn param_help(p: &janus_zeit::Param) -> String {
    let Some(d) = default_text(p) else { return p.help.clone() };
    if p.help.to_lowercase().contains("default") {
        return p.help.clone();
    }
    if p.help.is_empty() { format!("Default: {d}") } else { format!("{}\nDefault: {d}", p.help) }
}

// ---------------------------------------------------------------------------------------------------------------
// The samples' manifest.

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct SampleFile {
    pub url: String,
    pub name: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct Sample {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub size_mb: f64,
    /// the guide's page on the site, without ".html"
    #[serde(default)]
    pub guide: String,
    pub files: Vec<SampleFile>,
    /// what to open: a file, a folder or a `.janus` project, relative to the sample's folder ("" for the folder)
    #[serde(default)]
    pub open: String,
}

/// A path inside the sample's folder: relative, without "..", no drive or root.
fn safe_relative(p: &str) -> bool {
    let path = Path::new(p);
    !p.contains(':')
        && !path.is_absolute()
        && !p.starts_with(['/', '\\'])
        && path.components().all(|c| matches!(c, std::path::Component::Normal(_) | std::path::Component::CurDir))
}

/// The samples a manifest lists. An entry that cannot be trusted (an id or a path that leaves the sample's folder, a
/// checksum that is not one) makes the whole manifest refused: it is the program's own download, not a place for
/// surprises.
pub fn parse_manifest(bytes: &[u8]) -> Result<Vec<Sample>, String> {
    let mut list: Vec<Sample> =
        serde_json::from_slice(bytes).map_err(|e| format!("the list of sample data could not be read: {e}"))?;
    for s in &mut list {
        let ok_id = !s.id.is_empty()
            && !s.id.starts_with('.')
            && s.id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !ok_id {
            return Err(format!("the list of sample data has an entry with a bad id: \"{}\"", s.id));
        }
        if s.files.is_empty() {
            return Err(format!("the sample {} has no files", s.id));
        }
        for f in &s.files {
            if f.name.is_empty() || !safe_relative(&f.name) || f.url.is_empty() {
                return Err(format!("the sample {} has a file with a bad name or address: \"{}\"", s.id, f.name));
            }
            if f.sha256.len() != 64 || !f.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(format!("the sample {}: {} has no SHA-256", s.id, f.name));
            }
        }
        if !safe_relative(&s.open) {
            return Err(format!("the sample {} opens a path outside its folder: \"{}\"", s.id, s.open));
        }
        if s.open.is_empty() && s.files.len() == 1 {
            s.open = s.files[0].name.clone();
        }
        if !s.guide.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            s.guide.clear();
        }
    }
    Ok(list)
}

/// Where the manifest is: JANUS_SAMPLES_URL (a URL or a path) or `SAMPLES_URL`.
pub fn manifest_source() -> String {
    if let Some(s) = probe_override().0 {
        return s;
    }
    std::env::var("JANUS_SAMPLES_URL").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| SAMPLES_URL.into())
}

/// The folder of the samples: JANUS_SAMPLES_DIR, else `samples` in the Janus data folder.
pub fn samples_dir() -> PathBuf {
    if let Some(d) = probe_override().1 {
        return d;
    }
    std::env::var_os("JANUS_SAMPLES_DIR")
        .filter(|s| !s.is_empty())
        .map_or_else(|| crate::settings::app_data_dir().join("samples"), PathBuf::from)
}

/// The self-test's own manifest and folder (it must not set the process' environment while threads run).
static OVERRIDE: Mutex<(Option<String>, Option<PathBuf>)> = Mutex::new((None, None));

fn probe_override() -> (Option<String>, Option<PathBuf>) {
    OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// A manifest and a folder for the samples other than the program's (the self-test).
pub fn set_override(source: Option<String>, dir: Option<PathBuf>) {
    *OVERRIDE.lock().unwrap_or_else(|e| e.into_inner()) = (source, dir);
}

fn is_http(s: &str) -> bool {
    let l = s.trim_start().to_ascii_lowercase();
    l.starts_with("http://") || l.starts_with("https://")
}

/// A file's address as the manifest at `base` means it: as it is when it is a URL or an absolute path, else beside the
/// manifest.
pub fn resolve(base: &str, url: &str) -> String {
    if url.contains("://") || Path::new(url).is_absolute() {
        return url.to_string();
    }
    let dir = base.rfind(['/', '\\']).map_or("", |i| &base[..=i]);
    format!("{dir}{url}")
}

/// The manifest read from `source` (and kept in the samples' folder: offline, the last list is still there, and the
/// samples already downloaded open from it). The second value says when the list is the kept one.
pub fn load_manifest(source: &str, dir: &Path) -> Result<(Vec<Sample>, String), String> {
    let cache = dir.join("samples.json");
    let got = if is_http(source) {
        let agent = format!("USERAGENT=Janus/{} (https://github.com/sacridini/janus)", crate::update::VERSION);
        let options = [agent.as_str(), "TIMEOUT=20", "CONNECTTIMEOUT=15"];
        janus_gdal::http_fetch(source, None, &options)
    } else {
        std::fs::read(source).map_err(|e| format!("{source}: {e}"))
    };
    let parsed = got.and_then(|b| {
        let mut list = parse_manifest(&b)?;
        for s in &mut list {
            for f in &mut s.files {
                f.url = resolve(source, &f.url);
            }
        }
        Ok(list)
    });
    match parsed {
        Ok(list) => {
            if std::fs::create_dir_all(dir).is_ok()
                && let Ok(text) = serde_json::to_vec_pretty(&list)
            {
                let _ = std::fs::write(&cache, text);
            }
            Ok((list, String::new()))
        }
        Err(e) => {
            let kept = std::fs::read(&cache).ok().and_then(|b| parse_manifest(&b).ok());
            match kept {
                Some(list) => Ok((list, format!("No network: the list from the last time ({e})"))),
                None => {
                    Err(format!("The list of sample data could not be fetched: {e}. Check the network and try again."))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// The download.

/// The file's SHA-256 against `expected` (hexadecimal, any case).
pub fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let got = janus_ml::fetch::sha256_hex(&bytes);
    if got.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(format!(
            "{} does not match its checksum (the file changed, or the download was damaged); it was deleted",
            path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned())
        ))
    }
}

/// A local file copied to `dest`, through a temporary file; `progress` gets the bytes copied so far.
fn copy_local(src: &str, dest: &Path, progress: &dyn Fn(f64), cancel: &AtomicBool) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut input = std::fs::File::open(src).map_err(|e| format!("{src}: {e}"))?;
    if let Some(d) = dest.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let tmp = dest.with_extension("part");
    let mut out = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0f64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(&tmp);
            return Err("cancelled".into());
        }
        let n = input.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        got += n as f64;
        progress(got);
    }
    drop(out);
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", dest.display()))
}

/// The files of a sample into `dir`, each checked against its SHA-256; a folder that is complete says so (`COMPLETE`).
/// `progress` gets the fraction done and a message. A file already there that passes its check is kept.
pub fn download_sample(
    sample: &Sample,
    dir: &Path,
    progress: &dyn Fn(f64, &str),
    cancel: &AtomicBool,
) -> Result<(), String> {
    let total = (sample.size_mb * 1048576.0).max(1.0);
    let mut done = 0f64;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let _ = std::fs::remove_file(dir.join(COMPLETE));
    for f in &sample.files {
        let dest = dir.join(&f.name);
        if dest.is_file() && verify_sha256(&dest, &f.sha256).is_ok() {
            done += std::fs::metadata(&dest).map_or(0.0, |m| m.len() as f64);
            continue;
        }
        progress((done / total).min(0.99), &format!("{} ({:.0} MB)", f.name, sample.size_mb));
        let base = done;
        let part =
            |got: f64| progress(((base + got) / total).min(0.99), &format!("{} ({:.0} MB)", f.name, sample.size_mb));
        if is_http(&f.url) {
            janus_ml::fetch::fetch(&f.url, &dest, &part, cancel)?;
        } else {
            copy_local(&f.url, &dest, &part, cancel)?;
        }
        if let Err(e) = verify_sha256(&dest, &f.sha256) {
            let _ = std::fs::remove_file(&dest);
            return Err(e);
        }
        done += std::fs::metadata(&dest).map_or(0.0, |m| m.len() as f64);
    }
    std::fs::write(dir.join(COMPLETE), &sample.id).map_err(|e| format!("{}: {e}", dir.display()))?;
    progress(1.0, "done");
    Ok(())
}

/// What a sample opens: its `open` path in its folder.
fn open_path(sample: &Sample, dir: &Path) -> PathBuf {
    if sample.open.is_empty() || sample.open == "." { dir.to_path_buf() } else { dir.join(&sample.open) }
}

/// A sample downloaded (and complete) in the samples' folder.
fn is_downloaded(sample: &Sample, root: &Path) -> bool {
    let dir = root.join(&sample.id);
    dir.join(COMPLETE).is_file() && open_path(sample, &dir).exists()
}

// ---------------------------------------------------------------------------------------------------------------
// The window.

/// Where the manifest is.
#[derive(Clone, Default)]
enum ManifestState {
    #[default]
    Idle,
    Loading,
    /// the samples, and a note (the list is the kept one)
    Ready(Vec<Sample>, String),
    Failed(String),
}

/// A sample being downloaded, to be opened when it is there.
struct SampleRun {
    id: String,
    status: Arc<Mutex<JobStatus>>,
    open: PathBuf,
    opened: bool,
}

/// The Welcome window's state.
#[derive(Default)]
pub struct WelcomeUi {
    pub show: bool,
    /// the sample data's list shown instead of the start
    pub samples_view: bool,
    manifest: Arc<Mutex<ManifestState>>,
    runs: Vec<SampleRun>,
}

/// What the window asks of the program.
enum Act {
    Samples,
    Back,
    Folder,
    Stac,
    Project(String),
    Guide(String),
    Download(Sample),
    OpenSample(Sample),
    Retry,
}

impl State {
    /// Help > Welcome: the window shown at its start page.
    pub fn open_welcome(&mut self) {
        self.welcome.show = true;
        self.welcome.samples_view = false;
    }

    /// Each frame: the list of samples asked for when the window shows, the samples downloaded opened. The window is
    /// never shown by itself at the start (Janus opens empty, ready to use): only Help > Welcome opens it.
    pub(crate) fn pump_welcome(&mut self, ctx: &egui::Context) {
        if self.welcome.show && matches!(*self.welcome.manifest.lock().unwrap(), ManifestState::Idle) {
            self.start_manifest(ctx);
        }
        // a finished download opens (one at a time, when nothing else is opening)
        let mut open = None;
        for r in &mut self.welcome.runs {
            if !r.opened
                && open.is_none()
                && r.status.lock().unwrap().state == JobState::Done
                && self.opening.is_none()
                && self.project.restore.is_none()
            {
                r.opened = true;
                open = Some(r.open.clone());
            }
        }
        if let Some(p) = open {
            self.welcome.show = false;
            if !p.exists() {
                self.error_popup = Some(format!("{} is not in the sample's folder", p.display()));
            } else if let Some(s) = p.to_str() {
                self.open_inputs(vec![s.to_string()], true, ctx);
            }
        }
        self.welcome.runs.retain(|r| !r.opened);
        if self.welcome.runs.iter().any(|r| r.status.lock().unwrap().state == JobState::Running) {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
    }

    /// The manifest fetched in the background.
    fn start_manifest(&mut self, ctx: &egui::Context) {
        *self.welcome.manifest.lock().unwrap() = ManifestState::Loading;
        let (state, ctx) = (self.welcome.manifest.clone(), ctx.clone());
        let (source, dir) = (manifest_source(), samples_dir());
        std::thread::Builder::new()
            .name("samples list".into())
            .spawn(move || {
                let r = load_manifest(&source, &dir);
                *state.lock().unwrap() = match r {
                    Ok((list, note)) => ManifestState::Ready(list, note),
                    Err(e) => ManifestState::Failed(e),
                };
                ctx.request_repaint();
            })
            .ok();
    }

    /// The sample's download started as a task (Tools > Tasks): in the line of downloads, with its log and Cancel.
    fn start_sample(&mut self, sample: &Sample, ctx: &egui::Context) {
        let dir = samples_dir().join(&sample.id);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.error_popup = Some(format!("could not create {}: {e}", dir.display()));
            return;
        }
        self.welcome.runs.retain(|r| r.id != sample.id);
        let title = format!("Sample data: {}", sample.title);
        let log = JobLog { path: dir.join("job.log"), t0: Instant::now() };
        log.write(&format!("=== {} job: {title}\noutput: {}\n", crate::zeit::timestamp_text(), dir.display()));
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, s, d) = (status.clone(), cancel.clone(), ctx.clone(), sample.clone(), dir.clone());
        let log_path = log.path.to_string_lossy().into_owned();
        std::thread::Builder::new()
            .name("sample download".into())
            .spawn(move || {
                let t0 = Instant::now();
                let progress = |f: f64, m: &str| {
                    {
                        let mut g = st.lock().unwrap();
                        (g.progress, g.message) = (f, m.to_string());
                    }
                    c.request_repaint();
                };
                let r = crate::downloads::turn(&st, &cn).and_then(|_turn| download_sample(&s, &d, &progress, &cn));
                let mut g = st.lock().unwrap();
                g.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(()) => {
                        log.line("done");
                        (g.state, g.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        g.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        (g.state, g.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        self.zeit.jobs.push(Job {
            title,
            tool: "sample".into(),
            cube: 0,
            output_dir: dir.to_string_lossy().into_owned(),
            log_path,
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true, // nothing for the Zeit results to load
        });
        self.welcome.runs.push(SampleRun {
            id: sample.id.clone(),
            status,
            open: open_path(sample, &dir),
            opened: false,
        });
    }

    /// The Welcome window.
    pub(crate) fn welcome_window(&mut self, ctx: &egui::Context) {
        if !self.welcome.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Welcome to Janus", ctx, 520.0)
            .id(egui::Id::new("welcome window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.welcome_ui(ui)));
        if !open {
            self.welcome.show = false;
        }
    }

    fn welcome_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut act = None;
        if self.welcome.samples_view {
            self.samples_ui(ui, &mut act);
        } else {
            self.start_ui(ui, &mut act);
        }
        match act {
            None => {}
            Some(Act::Samples) => self.welcome.samples_view = true,
            Some(Act::Back) => self.welcome.samples_view = false,
            Some(Act::Folder) => {
                if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                    self.open_inputs(vec![dir.to_string_lossy().into_owned()], true, &ctx);
                    self.welcome.show = false;
                }
            }
            Some(Act::Stac) => {
                self.stac.show = true;
                self.welcome.show = false;
            }
            Some(Act::Project(p)) => {
                self.welcome.show = false;
                self.open_project(Path::new(&p), &ctx);
            }
            Some(Act::Guide(page)) => open_url(&ctx, &page_url(&page)),
            Some(Act::Download(s)) => self.start_sample(&s, &ctx),
            Some(Act::OpenSample(s)) => {
                let p = open_path(&s, &samples_dir().join(&s.id));
                self.welcome.show = false;
                if let Some(p) = p.to_str() {
                    self.open_inputs(vec![p.to_string()], true, &ctx);
                }
            }
            Some(Act::Retry) => *self.welcome.manifest.lock().unwrap() = ManifestState::Idle,
        }
    }

    /// The start page: what to open, the recent projects, the guides.
    fn start_ui(&mut self, ui: &mut Ui, act: &mut Option<Act>) {
        ui.label(
            "Janus reads raster time series. It finds where and when the land changed, and it checks how right \
             the result is.",
        );
        crate::panels::section(ui, "Start");
        ui.horizontal_wrapped(|ui| {
            let r = ui.button("Open sample data...").on_hover_text(
                "Small series from the guides, downloaded once to the Janus data folder. They open with the \
                 guide's settings.",
            );
            if clicked(&r, "Open sample data...") {
                *act = Some(Act::Samples);
            }
            let r = ui.button("Open folder...").on_hover_text("A folder of images, one for each date, as a series");
            if clicked(&r, "Open folder...") {
                *act = Some(Act::Folder);
            }
            let r = ui
                .button("Search STAC...")
                .on_hover_text("Satellite scenes from a catalog (Earth Search, Planetary Computer...) over your area");
            if clicked(&r, "Search STAC...") {
                *act = Some(Act::Stac);
            }
        });
        ui.label(RichText::new("File > Open file(s) and dropping files on the window work too.").weak());

        crate::panels::section(ui, "Recent projects");
        let recent: Vec<String> = self.settings.s.recent_projects.iter().take(6).cloned().collect();
        if recent.is_empty() {
            ui.weak("None yet. File > Save project keeps one here.");
        }
        for r in &recent {
            let name = Path::new(r).file_name().map_or(r.clone(), |n| n.to_string_lossy().into_owned());
            let resp = ui.link(&name).on_hover_text(r);
            if clicked(&resp, &format!("recent:{r}")) {
                *act = Some(Act::Project(r.clone()));
            }
        }

        crate::panels::section(ui, "Guides");
        ui.label("Not sure which method fits your data? Answer a few questions and see the methods that do.");
        let b = egui::Button::new(RichText::new("Which method?").strong().color(theme::accent()));
        let r = ui.add(b).on_hover_text("methods.html: every method, what it needs and what it gives");
        if clicked(&r, "Which method?") {
            *act = Some(Act::Guide("methods.html".into()));
        }
        ui.horizontal_wrapped(|ui| {
            for (label, page) in
                [("All the guides", "guides.html"), ("Using Janus", "docs-usage.html"), ("What is new", "news.html")]
            {
                let r = ui.link(label).on_hover_text(page);
                if clicked(&r, &format!("link:{page}")) {
                    *act = Some(Act::Guide(page.into()));
                }
            }
        });
        ui.label(RichText::new("Every tool's window has a ? button that opens its guide.").weak());
    }

    /// The sample data: each with its size, its guide and its button.
    fn samples_ui(&mut self, ui: &mut Ui, act: &mut Option<Act>) {
        ui.horizontal(|ui| {
            let r = ui.button("< Back");
            if clicked(&r, "Back") {
                *act = Some(Act::Back);
            }
            ui.strong("Sample data");
        });
        ui.label(
            "Small series from the guides, with their settings. They are downloaded once, to the Janus data \
             folder, and open from there.",
        );
        let root = samples_dir();
        let state = self.welcome.manifest.lock().unwrap().clone();
        match &state {
            ManifestState::Idle | ManifestState::Loading => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Fetching the list...");
                });
            }
            ManifestState::Failed(e) => {
                ui.add(egui::Label::new(RichText::new(e).color(theme::warning())).wrap());
                let r = ui.button("Try again");
                if clicked(&r, "Try again") {
                    *act = Some(Act::Retry);
                }
            }
            ManifestState::Ready(list, note) => {
                if !note.is_empty() {
                    ui.add(egui::Label::new(RichText::new(note).color(theme::warning())).wrap());
                }
                if list.is_empty() {
                    ui.weak("The list is empty.");
                }
                for s in list {
                    self.sample_row(ui, s, &root, act);
                }
            }
        }
    }

    fn sample_row(&self, ui: &mut Ui, s: &Sample, root: &Path, act: &mut Option<Act>) {
        ui.separator();
        ui.add(egui::Label::new(RichText::new(&s.title).strong()).wrap());
        if !s.description.is_empty() {
            ui.add(egui::Label::new(RichText::new(&s.description).weak()).wrap());
        }
        ui.horizontal_wrapped(|ui| {
            if s.size_mb > 0.0 {
                ui.weak(format!("{:.0} MB", s.size_mb));
            }
            if !s.guide.is_empty() {
                let page = format!("{}.html", s.guide);
                let r = ui.link(format!("Guide: {page}")).on_hover_text(page_url(&page));
                if clicked(&r, &format!("guide:{}", s.id)) {
                    *act = Some(Act::Guide(page));
                }
            }
        });
        let run = self.welcome.runs.iter().rfind(|r| r.id == s.id);
        let (state, progress, message, error) = run.map_or((None, 0.0, String::new(), String::new()), |r| {
            let g = r.status.lock().unwrap();
            (Some(g.state), g.progress, g.message.clone(), g.error.clone())
        });
        match state {
            Some(JobState::Running) => {
                let waiting = run.is_some_and(|r| crate::downloads::place(&r.status).is_some());
                let bar = egui::ProgressBar::new(if waiting { 0.0 } else { progress as f32 }).text(message);
                crate::panels::progress_row(ui, bar, |ui| {
                    if let Some(r) = run {
                        let b = ui.button("Cancel");
                        if clicked(&b, &format!("cancel:{}", s.id))
                            && let Some(j) = self.zeit.jobs.iter().find(|j| Arc::ptr_eq(&j.status, &r.status))
                        {
                            j.cancel.store(true, Ordering::Relaxed);
                        }
                    }
                });
            }
            Some(JobState::Failed) => {
                ui.add(egui::Label::new(RichText::new(format!("Failed: {error}")).color(theme::warning())).wrap());
                let r = ui.button("Download again");
                if clicked(&r, &format!("download:{}", s.id)) {
                    *act = Some(Act::Download(s.clone()));
                }
            }
            _ if is_downloaded(s, root) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Downloaded").color(theme::accent()));
                    let r = ui.button("Open");
                    if clicked(&r, &format!("open:{}", s.id)) {
                        *act = Some(Act::OpenSample(s.clone()));
                    }
                });
            }
            _ => {
                let r = ui.button("Download and open").on_hover_text(format!("Into {}", root.join(&s.id).display()));
                if clicked(&r, &format!("download:{}", s.id)) {
                    *act = Some(Act::Download(s.clone()));
                }
            }
        }
    }

    /// For the self-test: the samples the window knows, or what went wrong (None while the list is being fetched).
    pub(crate) fn welcome_samples(&self) -> Option<Result<Vec<Sample>, String>> {
        match &*self.welcome.manifest.lock().unwrap() {
            ManifestState::Ready(l, _) => Some(Ok(l.clone())),
            ManifestState::Failed(e) => Some(Err(e.clone())),
            _ => None,
        }
    }

    /// For the self-test: the state of the download of a sample (None: none asked, or already opened).
    pub(crate) fn welcome_run(&self, id: &str) -> Option<(JobState, String)> {
        let r = self.welcome.runs.iter().rfind(|r| r.id == id)?;
        let g = r.status.lock().unwrap();
        Some((g.state, g.error.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(page: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../site").join(page)
    }

    /// "page.html#anchor": the page is a file of site/, and the anchor an id in it.
    fn exists_on_site(page: &str) -> Result<(), String> {
        let (file, anchor) = page.split_once('#').map_or((page, None), |(f, a)| (f, Some(a)));
        let text = std::fs::read_to_string(site(file)).map_err(|e| format!("{file}: {e}"))?;
        match anchor {
            Some(a) if !text.contains(&format!("id=\"{a}\"")) => Err(format!("{file} has no #{a}")),
            _ => Ok(()),
        }
    }

    #[test]
    fn every_guide_page_exists_on_the_site() {
        // the methods' own pages
        assert!(methods().len() >= 30, "site/data/methods.json was not read");
        for (id, guide) in methods() {
            if let Some(g) = guide {
                exists_on_site(&format!("{g}.html")).unwrap_or_else(|e| panic!("method {id}: {e}"));
            }
        }
        // the table of the windows of Janus itself
        for (key, page) in NATIVE_PAGES.iter().copied() {
            exists_on_site(page).unwrap_or_else(|e| panic!("{key}: {e}"));
        }
        // each Zeit tool has a page, and a method without a guide of its own is found in methods.html by its id
        for t in &janus_zeit::manifest().tools {
            let page = guide_page(&t.id).unwrap_or_else(|| panic!("the tool {} has no guide page", t.id));
            exists_on_site(&page).unwrap_or_else(|e| panic!("tool {}: {e}", t.id));
        }
        // the pages the Welcome window links
        for page in ["methods.html", "guides.html", "docs-usage.html", "news.html"] {
            exists_on_site(page).unwrap();
        }
    }

    #[test]
    fn the_tools_table_names_methods_that_exist() {
        for (tool, method) in TOOL_METHODS.iter().copied() {
            assert!(methods().iter().any(|(id, _)| id == method), "{tool}: no method {method} in methods.json");
        }
    }

    #[test]
    fn the_address_of_a_guide() {
        assert_eq!(guide_url("landtrendr").unwrap(), "https://sacridini.github.io/janus/landtrendr.html");
        // BFAST Lite shares the BFAST page
        assert_eq!(guide_url("bfast_lite").unwrap(), "https://sacridini.github.io/janus/bfast.html");
        // a method with no page of its own: its place in the list of methods
        assert_eq!(guide_url("texture").unwrap(), "https://sacridini.github.io/janus/methods.html#texture");
        // the data-preparation tools: their sections of the Data preparation guide
        for (tool, section) in [
            ("level2", "level2"),
            ("tmask", "tmask"),
            ("clear_observations", "clear-observations"),
            ("nbar", "nbar"),
            ("spectral_adjust", "spectral-adjust"),
            ("harmonize", "harmonize"),
            ("topocorr", "topographic-correction"),
            ("coregister", "co-registration"),
            ("improphe", "improphe"),
            ("interpolate", "interpolate"),
            ("smooth", "smoothing"),
            ("standardize", "standardize"),
            ("regularize", "regularize"),
            ("speckle", "speckle"),
            ("warp", "warp"),
        ] {
            assert_eq!(
                guide_url(tool).unwrap(),
                format!("https://sacridini.github.io/janus/data-preparation.html#{section}"),
                "{tool}"
            );
        }
        assert_eq!(guide_url("calculator").unwrap(), "https://sacridini.github.io/janus/calculator.html");
        assert_eq!(
            guide_url("ard").unwrap(),
            "https://sacridini.github.io/janus/docs-usage.html#analysis-ready-data-force"
        );
        assert!(guide_url("no such tool").is_none());
        assert_eq!(page_url("/x.html"), "https://sacridini.github.io/janus/x.html");
    }

    fn param(kind: &str, default: serde_json::Value, help: &str) -> janus_zeit::Param {
        janus_zeit::Param {
            id: "p".into(),
            label: "P".into(),
            kind: kind.into(),
            default,
            help: help.into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_parameters_hover_has_its_default() {
        let p = param("float", serde_json::json!(0.15), "Minimum segment length.");
        assert_eq!(param_help(&p), "Minimum segment length.\nDefault: 0.15");
        let p = param("int", serde_json::json!(5), "");
        assert_eq!(param_help(&p), "Default: 5");
        assert_eq!(param_help(&param("bool", serde_json::json!(true), "Smooth it.")), "Smooth it.\nDefault: on");
        // already said
        let p = param("int", serde_json::json!(5), "The default is 5.");
        assert_eq!(param_help(&p), "The default is 5.");
        // an option by its label
        let mut p = param("enum", serde_json::json!("b"), "Which.");
        p.options = Some(vec!["a".into(), "b".into()]);
        p.labels = Some(vec!["First".into(), "Second".into()]);
        assert_eq!(param_help(&p), "Which.\nDefault: Second");
        // nothing to say of a list or an empty text
        assert_eq!(param_help(&param("layers", serde_json::json!([]), "Maps.")), "Maps.");
        assert_eq!(param_help(&param("file", serde_json::json!(""), "A file.")), "A file.");
        let mut p = param("float", serde_json::json!(2.0), "x");
        p.unit = Some("years".into());
        assert_eq!(param_help(&p), "x\nDefault: 2 years");
    }

    const GOOD: &str = r#"[{"id": "a", "title": "A", "description": "d", "size_mb": 1.5, "guide": "landtrendr",
        "files": [{"url": "a.tif", "name": "a.tif",
        "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"}], "open": "a.tif"}]"#;

    #[test]
    fn the_manifest_is_parsed_and_the_unsafe_refused() {
        let list = parse_manifest(GOOD.as_bytes()).unwrap();
        assert_eq!((list[0].id.as_str(), list[0].open.as_str(), list[0].size_mb), ("a", "a.tif", 1.5));
        // one file and no "open": that file
        let no_open = GOOD.replace(r#", "open": "a.tif""#, "");
        assert_eq!(parse_manifest(no_open.as_bytes()).unwrap()[0].open, "a.tif");
        for bad in [
            GOOD.replace(r#""name": "a.tif""#, r#""name": "../a.tif""#),
            GOOD.replace(r#""name": "a.tif""#, r#""name": "C:\\a.tif""#),
            GOOD.replace(r#""open": "a.tif""#, r#""open": "/etc""#),
            GOOD.replace(r#""id": "a""#, r#""id": "../a""#),
            GOOD.replace("e3b0c442", "e3b0"),
            GOOD.replace(r#""files": ["#, r#""files": [], "x": ["#),
            "not json".to_string(),
        ] {
            assert!(parse_manifest(bad.as_bytes()).is_err(), "{bad}");
        }
        // the example the repository keeps
        let ex = parse_manifest(include_bytes!("../../../tools/samples.example.json")).unwrap();
        assert_eq!(ex[0].id, "rondonia-ndvi");
        exists_on_site(&format!("{}.html", ex[0].guide)).unwrap();
    }

    #[test]
    fn a_manifest_resolves_its_files_beside_itself() {
        assert_eq!(resolve("https://h/x/samples.json", "a.tif"), "https://h/x/a.tif");
        assert_eq!(resolve("https://h/x/samples.json", "https://o/a.tif"), "https://o/a.tif");
        assert_eq!(resolve("C:\\d\\samples.json", "a.tif"), "C:\\d\\a.tif");
        assert_eq!(resolve("samples.json", "a.tif"), "a.tif");
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("janus-welcome-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_checksum_is_checked() {
        let d = temp("sha");
        let f = d.join("abc.txt");
        std::fs::write(&f, b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        verify_sha256(&f, abc).unwrap();
        verify_sha256(&f, &abc.to_uppercase()).unwrap();
        let e = verify_sha256(&f, &"0".repeat(64)).unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_sample_is_downloaded_checked_and_marked() {
        let d = temp("download");
        let src = d.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("a.bin"), b"abc").unwrap();
        let abc = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let manifest = src.join("samples.json");
        let text = GOOD
            .replace("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", abc)
            .replace("a.tif", "a.bin");
        std::fs::write(&manifest, text).unwrap();
        let root = d.join("samples");
        let (list, note) = load_manifest(manifest.to_str().unwrap(), &root).unwrap();
        assert!(note.is_empty());
        assert!(!is_downloaded(&list[0], &root));
        let seen = Mutex::new(vec![]);
        let progress = |f: f64, m: &str| seen.lock().unwrap().push((f, m.to_string()));
        let cancel = AtomicBool::new(false);
        let dir = root.join("a");
        download_sample(&list[0], &dir, &progress, &cancel).unwrap();
        assert_eq!(std::fs::read(dir.join("a.bin")).unwrap(), b"abc");
        assert!(is_downloaded(&list[0], &root));
        assert_eq!(seen.lock().unwrap().last().unwrap().0, 1.0);
        // a file that changed: refused and deleted
        let mut bad = list[0].clone();
        bad.files[0].sha256 = "0".repeat(64);
        let dir2 = root.join("b");
        let e = download_sample(&bad, &dir2, &progress, &cancel).unwrap_err();
        assert!(e.contains("checksum"), "{e}");
        assert!(!dir2.join("a.bin").exists() && !dir2.join(COMPLETE).exists());
        // cancelled
        cancel.store(true, Ordering::Relaxed);
        let dir3 = root.join("c");
        assert_eq!(download_sample(&list[0], &dir3, &progress, &cancel).unwrap_err(), "cancelled");
        // offline (the manifest is not there): a clear message, and the last list when there is one
        let gone = src.join("gone.json");
        let (kept, note) = load_manifest(gone.to_str().unwrap(), &root).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(note.contains("last time"), "{note}");
        let e = load_manifest(gone.to_str().unwrap(), &d.join("no-cache")).unwrap_err();
        assert!(e.contains("could not be fetched"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
