//! File > Climate data... (and the MCP tool climate_data): the rainfall of CHIRPS v3 (Climate Hazards Center,
//! 0.05 degrees, 60S-60N, from 1981) read in place through GDAL's /vsicurl/, only the box asked for out of each of the
//! files (monthly, pentad or daily cloud-optimized GeoTIFFs), written as one small GeoTIFF a date and opened as a
//! series layer on its coarse grid, to be drawn beside a vegetation series (the chart's right axis). The download is a
//! job of Tools > Tasks like the embeddings'; a file already written is kept, so downloading again retries only the
//! dates that failed.
//!
//! The data are in the public domain (the Climate Hazards Center's copyright waiver, CC BY 4.0 as the repository says);
//! the window, the files' metadata and `ATTRIBUTION.txt` beside them carry the citation. ERA5 (Planetary Computer's
//! `era5-pds`, hourly Zarr v2 with blosc/zstd chunks, 1979-01 to 2020-12 only) is not offered: see IDEIAS.md,
//! phase 61.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gdal::Metadata;
use gdal::raster::{Buffer, RasterCreationOptions};
use gdal::spatial_ref::SpatialRef;
use serde_json::json;

use crate::app::State;
use crate::emb_download::EmbDownload;
use crate::theme;
use crate::zeit::{Job, JobLog, JobState, JobStatus};

/// Where CHIRPS v3 is published (the environment variable JANUS_CHIRPS_URL replaces it: a mirror, or the folder of
/// the self-test's fake months, a local path as well as an address).
pub const CHIRPS_ROOT: &str = "https://data.chc.ucsb.edu/products/CHIRPS/v3.0";

/// The citation and the repository the data must be credited with.
pub const CITATION: &str = "Funk, C. et al. CHIRPS3. Scientific Data (2026), doi:10.1038/s41597-026-07096-4";
pub const CITATION_DOI: &str = "10.1038/s41597-026-07096-4";
pub const DATA_DOI: &str = "10.15780/G2JQ0P";
pub const LICENSE: &str = "Public domain (Climate Hazards Center copyright waiver); CC BY 4.0";

/// The first month of CHIRPS (January 1981).
const FIRST: (i32, u32) = (1981, 1);
/// A box of the whole product at most: files a download may write.
const MAX_FILES: usize = 4000;
/// Files read at once (the server throttles many requests: the downloads' line limits the jobs, this the files).
const WORKERS: usize = 2;
/// The product's grid: 0.05 degrees, 180W-180E, 60S-60N.
pub const GRID_GT: [f64; 6] = [-180.0, 0.05, 0.0, 60.0, 0.0, -0.05];
pub const GRID_SIZE: (usize, usize) = (7200, 2400);

/// The CHIRPS series the window and the tool offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Monthly,
    /// six a month: days 1-5, 6-10, 11-15, 16-20, 21-25 and 26 to the month's end
    Pentad,
    /// the pentads spread over the days by ERA5 (the "rnl" flavour: from 1981)
    Daily,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Monthly, Source::Pentad, Source::Daily];

    pub fn id(self) -> &'static str {
        match self {
            Source::Monthly => "chirps-monthly",
            Source::Pentad => "chirps-pentad",
            Source::Daily => "chirps-daily",
        }
    }

    pub fn from_id(s: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|v| v.id() == s || v.id().strip_prefix("chirps-") == Some(s))
    }

    pub fn label(self) -> &'static str {
        match self {
            Source::Monthly => "CHIRPS v3 rainfall, monthly",
            Source::Pentad => "CHIRPS v3 rainfall, pentads (5-6 days)",
            Source::Daily => "CHIRPS v3 rainfall, daily (ERA5 timing)",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Source::Monthly => {
                "Rain gauges blended with satellite estimates, mm per month, 0.05 degrees (about 5 km), 60S-60N,\n\
                 from January 1981. The product's own time step: use it wherever the science allows."
            }
            Source::Pentad => {
                "The pentads CHIRPS is computed at (six a month: days 1-5, 6-10, 11-15, 16-20, 21-25, 26 to the\n\
                 month's end), mm per pentad, from 1981. Six files a month."
            }
            Source::Daily => {
                "Not an independent daily product: the pentads' totals spread over the days by ERA5's timing\n\
                 (CHC's \"rnl\" files), mm per day, from 1981. A file a day: a year is 365 reads."
            }
        }
    }

    /// The unit of a value.
    pub fn unit(self) -> &'static str {
        match self {
            Source::Monthly => "mm/month",
            Source::Pentad => "mm/pentad",
            Source::Daily => "mm/day",
        }
    }

    /// The layer's name.
    pub fn layer_name(self) -> String {
        format!("CHIRPS rainfall ({})", self.unit())
    }
}

/// One date of a download: its day (days since 1970-01-01), the file under the root and the file written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub day: i64,
    pub remote: String,
    pub file: String,
}

fn month_days(y: i32, m: u32) -> u32 {
    match m {
        2 if janus_cube::dates::is_leap(y) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// The month after (year, month).
fn next_month((y, m): (i32, u32)) -> (i32, u32) {
    if m == 12 { (y + 1, 1) } else { (y, m + 1) }
}

/// The dates of a source from the month `start` to the month `end` (both included), in order.
pub fn steps(source: Source, start: (i32, u32), end: (i32, u32)) -> Vec<Step> {
    let mut out = Vec::new();
    let mut ym = start;
    while ym <= end {
        let (y, m) = ym;
        let day = |d: u32| janus_cube::dates::days_from_civil(y, m, d);
        match source {
            Source::Monthly => out.push(Step {
                day: day(1),
                remote: format!("monthly/global/cogs/chirps-v3.0.{y}.{m:02}.cog"),
                file: format!("chirps_{y}-{m:02}.tif"),
            }),
            Source::Pentad => {
                for p in 1..=6u32 {
                    let d = 5 * (p - 1) + 1;
                    out.push(Step {
                        day: day(d),
                        remote: format!("pentads/global/cogs/chirps-v3.0.{y}.{m:02}.{p}.cog"),
                        file: format!("chirps_{y}-{m:02}-{d:02}.tif"),
                    });
                }
            }
            Source::Daily => {
                for d in 1..=month_days(y, m) {
                    out.push(Step {
                        day: day(d),
                        remote: format!("daily/final/rnl/cogs/{y}/chirps-v3.0.rnl.{y}.{m:02}.{d:02}.cog"),
                        file: format!("chirps_{y}-{m:02}-{d:02}.tif"),
                    });
                }
            }
        }
        ym = next_month(ym);
    }
    out
}

/// The latest month a download may ask for: the one before today's (CHIRPS is published after the month ends).
pub fn latest_month() -> (i32, u32) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let (y, m, _) = janus_cube::dates::civil_from_days((now / 86400.0).floor() as i64);
    if m == 1 { (y - 1, 12) } else { (y, m - 1) }
}

/// The month of Unix seconds.
pub fn month_of(t: f64) -> (i32, u32) {
    let (y, m, _) = janus_cube::dates::civil_from_days((t / 86400.0).floor() as i64);
    (y, m)
}

/// The pixels (col0, row0, col1, row1; the last excluded) of a grid of `size` pixels, north up, whose cells cover
/// the box (west, south, east, north in the grid's units, here degrees). Err when the box is not one, or lies
/// outside the grid.
pub fn window(gt: [f64; 6], size: (usize, usize), bbox: [f64; 4]) -> Result<[usize; 4], String> {
    let [w, s, e, n] = bbox;
    if !bbox.iter().all(|v| v.is_finite()) || e <= w || n <= s {
        return Err("the box is empty: west < east and south < north are needed".into());
    }
    if gt[2] != 0.0 || gt[4] != 0.0 || gt[1] <= 0.0 || gt[5] >= 0.0 {
        return Err("the product's grid is not north up".into());
    }
    let (x1, y1) = (gt[0] + size.0 as f64 * gt[1], gt[3] + size.1 as f64 * gt[5]);
    if e <= gt[0] || w >= x1 || n <= y1 || s >= gt[3] {
        return Err(format!(
            "the box lies outside the product's coverage ({:.0} to {:.0} longitude, {:.0} to {:.0} latitude)",
            gt[0], x1, y1, gt[3]
        ));
    }
    // a small tolerance keeps a box that ends on a cell's edge from taking the next cell too
    let eps = 1e-9;
    let c0 = ((w - gt[0]) / gt[1] + eps).floor().max(0.0) as usize;
    let c1 = (((e - gt[0]) / gt[1] - eps).ceil().max(0.0) as usize).min(size.0);
    let r0 = ((n - gt[3]) / gt[5] + eps).floor().max(0.0) as usize;
    let r1 = (((s - gt[3]) / gt[5] - eps).ceil().max(0.0) as usize).min(size.1);
    if c1 <= c0 || r1 <= r0 {
        return Err("the box lies outside the product's coverage".into());
    }
    Ok([c0, r0, c1, r1])
}

/// The window of a box on CHIRPS's own grid.
pub fn chirps_window(bbox: [f64; 4]) -> Result<[usize; 4], String> {
    window(GRID_GT, GRID_SIZE, bbox)
}

/// What a download asks for.
#[derive(Clone, Debug)]
pub struct Spec {
    pub source: Source,
    /// west, south, east, north in longitude and latitude
    pub bbox: [f64; 4],
    pub start: (i32, u32),
    pub end: (i32, u32),
    pub out_dir: PathBuf,
}

/// The size of a download before it starts.
#[derive(Clone, Debug)]
pub struct Estimate {
    pub files: usize,
    pub cols: usize,
    pub rows: usize,
    /// the pixels written, as float32 and uncompressed
    pub bytes: f64,
}

/// The checks every download goes through: the period within the product's, the box within its coverage.
pub fn estimate(spec: &Spec) -> Result<Estimate, String> {
    if spec.start < FIRST {
        return Err("CHIRPS starts in January 1981".into());
    }
    if spec.end < spec.start {
        return Err("the period ends before it starts".into());
    }
    if spec.end > latest_month() {
        let (y, m) = latest_month();
        return Err(format!("the latest month published is {y}-{m:02}"));
    }
    let [c0, r0, c1, r1] = chirps_window(spec.bbox)?;
    let files = steps(spec.source, spec.start, spec.end).len();
    let (cols, rows) = (c1 - c0, r1 - r0);
    Ok(Estimate { files, cols, rows, bytes: files as f64 * cols as f64 * rows as f64 * 4.0 })
}

/// A root set by the self-test and the tests (before the environment's).
static ROOT_OVERRIDE: Mutex<Option<String>> = Mutex::new(None);

/// Reads the files from here instead of CHC's (None: back to the default).
pub(crate) fn set_root_override(root: Option<String>) {
    *ROOT_OVERRIDE.lock().unwrap() = root;
}

/// The root of the files: the test's, else the environment's, else CHC's.
fn root() -> String {
    let set = ROOT_OVERRIDE.lock().unwrap().clone();
    set.or_else(|| std::env::var("JANUS_CHIRPS_URL").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| CHIRPS_ROOT.into())
}

/// What GDAL opens for a file under the root: /vsicurl/ for an address, the path for a folder.
pub fn source_path(root: &str, rel: &str) -> String {
    let root = root.trim_end_matches(['/', '\\']);
    if root.starts_with("http://") || root.starts_with("https://") {
        format!("/vsicurl/{root}/{rel}")
    } else {
        Path::new(root).join(rel).to_string_lossy().into_owned()
    }
}

/// What a download did.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// the files of every date that has one now, in date order
    pub files: Vec<PathBuf>,
    /// read in this run
    pub fetched: usize,
    /// already there (an earlier run)
    pub kept: usize,
    /// the dates that could not be read, and why
    pub failed: Vec<(String, String)>,
    /// the window read, in the product's pixels
    pub window: [usize; 4],
}

/// The thread's GDAL settings for the server: no looking for side-car files (a listing a file, and the server
/// answers 429 to many), retries, the program's User-Agent and certificates.
fn http_settings() {
    janus_geo::ogc::http_config();
    let set = |k: &str, v: &str| {
        let _ = gdal::config::set_thread_local_config_option(k, v);
    };
    set("GDAL_DISABLE_READDIR_ON_OPEN", "EMPTY_DIR");
    set("CPL_VSIL_CURL_ALLOWED_EXTENSIONS", ".cog,.tif,.tiff");
    set("GDAL_HTTP_MAX_RETRY", "4");
    set("GDAL_HTTP_RETRY_DELAY", "2");
}

/// One date: the box read out of the remote file and written, through a temporary file, as a GeoTIFF.
fn fetch_one(src: &str, bbox: [f64; 4], out: &Path, step: &Step, source: Source) -> Result<[usize; 4], String> {
    let ds = gdal::Dataset::open(src).map_err(|e| format!("could not open the file: {e}"))?;
    let gt = ds.geo_transform().map_err(|e| e.to_string())?;
    let win = window(gt, ds.raster_size(), bbox)?;
    let (w, h) = (win[2] - win[0], win[3] - win[1]);
    let band = ds.rasterband(1).map_err(|e| e.to_string())?;
    let nodata = band.no_data_value();
    let buf = band
        .read_as::<f32>((win[0] as isize, win[1] as isize), (w, h), (w, h), None)
        .map_err(|e| format!("could not read the box: {e}"))?;
    let tmp = out.with_extension("tif.part");
    let wkt = ds.projection();
    let write = || -> Result<(), String> {
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").map_err(|e| e.to_string())?;
        let opts = RasterCreationOptions::from_iter(["COMPRESS=DEFLATE", "PREDICTOR=3"]);
        let mut o =
            drv.create_with_band_type_with_options::<f32, _>(&tmp, w, h, 1, &opts).map_err(|e| e.to_string())?;
        o.set_geo_transform(&[gt[0] + win[0] as f64 * gt[1], gt[1], 0.0, gt[3] + win[1] as f64 * gt[5], 0.0, gt[5]])
            .map_err(|e| e.to_string())?;
        if wkt.is_empty() {
            let srs = SpatialRef::from_epsg(4326).map_err(|e| e.to_string())?;
            o.set_spatial_ref(&srs).map_err(|e| e.to_string())?;
        } else {
            o.set_projection(&wkt).map_err(|e| e.to_string())?;
        }
        let date = janus_cube::dates::format_date(step.day as f64 * 86400.0, janus_cube::TimeGranularity::Day);
        for (k, v) in [
            ("SOURCE", format!("CHIRPS v3.0, Climate Hazards Center, UC Santa Barbara: {}", step.remote)),
            ("DATE", date),
            ("UNITS", source.unit().to_string()),
            ("LICENSE", LICENSE.to_string()),
            ("CITATION", CITATION.to_string()),
            ("DATA_DOI", DATA_DOI.to_string()),
        ] {
            o.set_metadata_item(k, &v, "").map_err(|e| e.to_string())?;
        }
        {
            let mut b = o.rasterband(1).map_err(|e| e.to_string())?;
            if let Some(nd) = nodata {
                b.set_no_data_value(Some(nd)).map_err(|e| e.to_string())?;
            }
            let _ = b.set_description(&source.layer_name());
            let (cols, rows) = (w, h);
            b.write((0, 0), (cols, rows), &mut Buffer::new((cols, rows), buf.data().to_vec()))
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    let r = write();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r?;
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())?;
    Ok(win)
}

/// Reads the box of every date of the period into `spec.out_dir` (a file a date; a file already there is kept), with
/// a few tries a date. Err only when nothing could be done (a wrong box or period, a folder that cannot be made, a
/// cancel); the dates that failed are in the outcome.
pub fn download(spec: &Spec, progress: &(dyn Fn(f64, &str) + Sync), cancel: &AtomicBool) -> Result<Outcome, String> {
    estimate(spec)?;
    std::fs::create_dir_all(&spec.out_dir).map_err(|e| format!("{}: {e}", spec.out_dir.display()))?;
    let todo = steps(spec.source, spec.start, spec.end);
    if todo.len() > MAX_FILES {
        return Err(format!("{} files is too many (at most {MAX_FILES}): ask for a shorter period", todo.len()));
    }
    let root = root();
    let next = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let state = Mutex::new(Outcome { window: chirps_window(spec.bbox)?, ..Default::default() });
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(todo.len()) {
            scope.spawn(|| {
                http_settings();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= todo.len() || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let step = &todo[i];
                    let out = spec.out_dir.join(&step.file);
                    let exists = std::fs::metadata(&out).is_ok_and(|m| m.len() > 0);
                    let mut result = Ok(None);
                    if !exists {
                        let src = source_path(&root, &step.remote);
                        for attempt in 0..3 {
                            result = fetch_one(&src, spec.bbox, &out, step, spec.source).map(Some);
                            // a box outside the grid or a missing file is not going to change
                            if result.is_ok() || cancel.load(Ordering::Relaxed) {
                                break;
                            }
                            if attempt < 2 {
                                std::thread::sleep(std::time::Duration::from_millis(1000 * (attempt + 1)));
                            }
                        }
                    }
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    {
                        let mut s = state.lock().unwrap();
                        match result {
                            Ok(Some(_)) => s.fetched += 1,
                            Ok(None) => s.kept += 1,
                            Err(e) => s.failed.push((step.file.clone(), e)),
                        }
                    }
                    progress(n as f64 / todo.len() as f64, &format!("{n} of {} dates", todo.len()));
                }
            });
        }
    });
    if cancel.load(Ordering::Relaxed) {
        return Err("cancelled".into());
    }
    let mut out = state.into_inner().unwrap();
    out.failed.sort();
    out.files = todo
        .iter()
        .map(|s| spec.out_dir.join(&s.file))
        .filter(|p| std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
        .collect();
    write_notes(spec, &out)?;
    Ok(out)
}

/// ATTRIBUTION.txt and climate.json beside the files: what they are, where from, the credit that is due.
fn write_notes(spec: &Spec, out: &Outcome) -> Result<(), String> {
    let ym = |(y, m): (i32, u32)| format!("{y}-{m:02}");
    let text = format!(
        "{}\n\nCHIRPS v3.0 rainfall, {} ({}), {} to {}, the box {:?} (west, south, east, north).\n\
         Source: {CHIRPS_ROOT} (Climate Hazards Center, University of California, Santa Barbara).\n\
         Licence: {LICENSE}.\n\nPlease cite:\n  {CITATION}\n  Data repository: doi:{DATA_DOI}\n",
        spec.source.layer_name(),
        spec.source.label(),
        spec.source.unit(),
        ym(spec.start),
        ym(spec.end),
        spec.bbox
    );
    std::fs::write(spec.out_dir.join("ATTRIBUTION.txt"), text).map_err(|e| e.to_string())?;
    let meta = json!({
        "source": spec.source.id(), "unit": spec.source.unit(), "bbox": spec.bbox,
        "start": ym(spec.start), "end": ym(spec.end), "window": out.window,
        "license": LICENSE, "citation": CITATION, "data_doi": DATA_DOI, "paper_doi": CITATION_DOI,
        "files": out.files.len(),
    });
    std::fs::write(spec.out_dir.join("climate.json"), serde_json::to_string_pretty(&meta).unwrap_or_default())
        .map_err(|e| e.to_string())
}

/// The folder a download goes to by default: one per source, box and period, so the same download again finds its
/// files (and reads only what failed or is new).
pub fn default_folder(base: &Path, spec: &Spec) -> PathBuf {
    let b = spec.bbox;
    base.join("climate").join(format!(
        "{}_{:.3}_{:.3}_{:.3}_{:.3}_{}-{:02}_{}-{:02}",
        spec.source.id(),
        b[0],
        b[1],
        b[2],
        b[3],
        spec.start.0,
        spec.start.1,
        spec.end.0,
        spec.end.1
    ))
}

// ---------------------------------------------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------------------------------------------

/// The window's choices.
pub struct ClimateUi {
    pub show: bool,
    pub source: Source,
    /// 0 the visible area, 1 the ROI, 2 a box of longitudes and latitudes
    pub area: usize,
    /// west, south, east, north
    pub bbox: [f64; 4],
    pub start: (i32, u32),
    pub end: (i32, u32),
    /// the output folder ("": the default under the results folder)
    pub folder: String,
    /// the download last started: what it says now
    pub last: Option<Arc<Mutex<JobStatus>>>,
}

impl Default for ClimateUi {
    fn default() -> Self {
        let end = latest_month();
        ClimateUi {
            show: false,
            source: Source::Monthly,
            area: 2,
            bbox: [-63.0, -10.0, -62.0, -9.0],
            start: (end.0 - 10, end.1),
            end,
            folder: String::new(),
            last: None,
        }
    }
}

/// The two month fields: a year and a month.
fn month_picker(ui: &mut egui::Ui, ym: &mut (i32, u32)) {
    ui.add(egui::DragValue::new(&mut ym.0).range(1981..=latest_month().0).speed(0.2));
    ui.add(egui::DragValue::new(&mut ym.1).range(1..=12).custom_formatter(|v, _| format!("{:02}", v as u32)));
}

impl State {
    /// File > Climate data...: the window, its period the active series' dates when it has some, else the last ten
    /// years, its area the visible area when a series is open.
    pub(crate) fn open_climate(&mut self) {
        if self.climate.show {
            return;
        }
        let end = latest_month();
        let dates = self.session.as_ref().filter(|s| s.info.time_is_date && s.info.t() > 1).map(|s| {
            let ts: Vec<f64> = s.info.layers.iter().map(|l| l.time).collect();
            let lo = ts.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = ts.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            (month_of(lo), month_of(hi))
        });
        let c = &mut self.climate;
        (c.start, c.end) = match dates {
            Some((a, b)) => (a.max(FIRST), b.min(end)),
            None => ((end.0 - 10, end.1), end),
        };
        if c.end < c.start {
            (c.start, c.end) = ((end.0 - 10, end.1), end);
        }
        c.area = if self.session.is_some() { 0 } else { 2 };
        self.climate.show = true;
    }

    pub(crate) fn climate_window(&mut self, ctx: &egui::Context) {
        if !self.climate.show {
            return;
        }
        let mut open = true;
        crate::panels::fitted_window("Climate data", ctx, 460.0)
            .id(egui::Id::new("climate window"))
            .open(&mut open)
            .show(ctx, |ui| crate::panels::fitted_body(ui, |ui| self.climate_ui(ui)));
        self.climate.show = open;
    }

    /// The box and where it comes from (said in the window), for the area chosen.
    fn climate_bbox(&self) -> Result<([f64; 4], String), String> {
        match self.climate.area {
            2 => Ok((self.climate.bbox, "the box typed".into())),
            a => {
                let (ll, _, _) = self.embedding_bounds(a)?;
                Ok((ll, if a == 1 { "the ROI".into() } else { "the map's visible area".into() }))
            }
        }
    }

    fn climate_spec(&self) -> Result<Spec, String> {
        let (bbox, _) = self.climate_bbox()?;
        let c = &self.climate;
        let mut spec = Spec { source: c.source, bbox, start: c.start, end: c.end, out_dir: PathBuf::new() };
        spec.out_dir = if c.folder.trim().is_empty() {
            default_folder(&self.results_dir(), &spec)
        } else {
            PathBuf::from(c.folder.trim())
        };
        Ok(spec)
    }

    fn climate_ui(&mut self, ui: &mut egui::Ui) {
        let w = ui.available_width();
        let cur = self.climate.source;
        egui::ComboBox::from_id_salt("climate source")
            .width(w)
            .selected_text(cur.label())
            .show_ui(ui, |ui| {
                for s in Source::ALL {
                    ui.selectable_value(&mut self.climate.source, s, s.label()).on_hover_text(s.help());
                }
            })
            .response
            .on_hover_text(cur.help());
        ui.add(egui::Label::new(egui::RichText::new(cur.help()).weak()).wrap());
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.climate.area, 0, "Visible area");
            ui.radio_value(&mut self.climate.area, 1, "ROI");
            ui.radio_value(&mut self.climate.area, 2, "Box");
        });
        if self.climate.area == 2 {
            let b = &mut self.climate.bbox;
            ui.horizontal(|ui| {
                for (label, i) in [("W", 0), ("S", 1), ("E", 2), ("N", 3)] {
                    ui.label(label);
                    ui.add(egui::DragValue::new(&mut b[i]).speed(0.05).max_decimals(4));
                }
            });
        }
        ui.horizontal(|ui| {
            ui.label("from");
            month_picker(ui, &mut self.climate.start);
            ui.label("to");
            month_picker(ui, &mut self.climate.end);
        });
        let latest = latest_month();
        let c = &mut self.climate;
        c.start = c.start.max(FIRST).min(latest);
        c.end = c.end.max(c.start).min(latest);
        ui.horizontal(|ui| {
            ui.label("Folder");
            ui.add(egui::TextEdit::singleline(&mut self.climate.folder).hint_text("default: under the results folder"));
            if ui.small_button("Browse...").clicked()
                && let Some(d) = rfd::FileDialog::new().pick_folder()
            {
                self.climate.folder = d.to_string_lossy().into_owned();
            }
        });
        // the estimate, or why there is nothing to download
        let mut block = true;
        match (self.climate_bbox(), self.climate_spec()) {
            (Ok((bbox, from)), Ok(spec)) => match estimate(&spec) {
                Ok(e) => {
                    block = e.files > MAX_FILES;
                    ui.weak(format!(
                        "{} file{} x {} x {} px of 0.05 degrees (~{:.1} MB written) from {from}",
                        e.files,
                        if e.files == 1 { "" } else { "s" },
                        e.cols,
                        e.rows,
                        e.bytes / 1048576.0
                    ))
                    .on_hover_text(format!(
                        "Only the box is read from each remote file. {:.3}, {:.3} to {:.3}, {:.3} (longitude, \
                         latitude).\nSaved under {}",
                        bbox[0],
                        bbox[1],
                        bbox[2],
                        bbox[3],
                        spec.out_dir.display()
                    ));
                    if e.files > MAX_FILES {
                        ui.colored_label(
                            theme::warning(),
                            format!("too many files (at most {MAX_FILES}): shorten the period"),
                        );
                    }
                }
                Err(why) => {
                    ui.colored_label(theme::warning(), why);
                }
            },
            (Err(why), _) | (_, Err(why)) => {
                ui.colored_label(theme::warning(), why);
            }
        }
        let running = self.climate.last.as_ref().is_some_and(|s| s.lock().unwrap().state == JobState::Running);
        let go = ui
            .add_enabled(
                !block && !running,
                egui::Button::new("Download").min_size(egui::vec2(ui.available_width(), 0.0)),
            )
            .on_hover_text(
                "Downloads in the background (progress on the map and in Tools > Tasks); the series is added on\n\
                 top when it is ready, the one being studied stays active. Download again to retry the dates\n\
                 that failed: the ones already written are kept.",
            )
            .clicked();
        if let Some(s) = &self.climate.last {
            let s = s.lock().unwrap();
            match s.state {
                JobState::Failed => {
                    ui.colored_label(theme::warning(), format!("failed: {}", s.error));
                }
                JobState::Running => {
                    ui.weak(&s.message);
                }
                _ if !s.message.is_empty() => {
                    ui.weak(&s.message);
                }
                _ => {}
            }
        }
        crate::panels::section(ui, "Credit");
        ui.add(
            egui::Label::new(
                "CHIRPS v3: Climate Hazards Center, UC Santa Barbara. Public domain (CC BY 4.0). Please cite:\n\
                 Funk, C. et al. CHIRPS3. Scientific Data (2026).",
            )
            .wrap(),
        );
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to("doi:10.1038/s41597-026-07096-4", format!("https://doi.org/{CITATION_DOI}"));
            ui.hyperlink_to("data: doi:10.15780/G2JQ0P", format!("https://doi.org/{DATA_DOI}"));
        });
        if go {
            self.start_climate_download(ui.ctx());
        }
    }

    /// Starts the window's download: a job of Tools > Tasks; the folder opened as a series when it is done.
    pub(crate) fn start_climate_download(&mut self, ctx: &egui::Context) {
        let spec = match self.climate_spec().and_then(|s| estimate(&s).map(|_| s)) {
            Ok(s) => s,
            Err(e) => {
                self.error_popup = Some(e);
                return;
            }
        };
        if let Err(e) = std::fs::create_dir_all(&spec.out_dir) {
            self.error_popup = Some(format!("could not create {}: {e}", spec.out_dir.display()));
            return;
        }
        let name = spec.source.layer_name();
        let title = format!(
            "Climate data: {} {}-{:02} to {}-{:02}",
            spec.source.id(),
            spec.start.0,
            spec.start.1,
            spec.end.0,
            spec.end.1
        );
        let log = JobLog { path: spec.out_dir.join("job.log"), t0: Instant::now() };
        log.write(&format!(
            "=== {} job: {title}\nsource: {}\nbox: {:?}\noutput: {}\n{CITATION}\nlicence: {LICENSE}\n",
            crate::zeit::timestamp_text(),
            root(),
            spec.bbox,
            spec.out_dir.display()
        ));
        let log_path = log.path.to_string_lossy().into_owned();
        let status = Arc::new(Mutex::new(JobStatus {
            state: JobState::Running,
            progress: 0.0,
            message: "starting".into(),
            error: String::new(),
            result: None,
            seconds: 0.0,
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let (st, cn, c, sp) = (status.clone(), cancel.clone(), ctx.clone(), spec.clone());
        std::thread::Builder::new()
            .name("climate download".into())
            .spawn(move || {
                let t0 = Instant::now();
                let logged = Mutex::new((-1.0, -1.0));
                let progress = |f: f64, m: &str| {
                    {
                        let mut s = st.lock().unwrap();
                        (s.progress, s.message) = (f, m.to_string());
                    }
                    let now = t0.elapsed().as_secs_f64();
                    let mut l = logged.lock().unwrap();
                    if l.0 < 0.0 || f >= 1.0 || f - l.0 >= 0.1 || now - l.1 >= 5.0 {
                        log.line(&format!("{:3.0}%  {m}", f * 100.0));
                        *l = (f, now);
                    }
                    c.request_repaint();
                };
                // its turn in the line of downloads first, kept while it transfers
                let r = crate::downloads::turn(&st, &cn).and_then(|_turn| download(&sp, &progress, &cn));
                let mut s = st.lock().unwrap();
                s.seconds = t0.elapsed().as_secs_f64();
                match r {
                    Ok(o) if o.files.is_empty() => {
                        let why = o.failed.first().map_or("nothing was read".into(), |f| format!("{}: {}", f.0, f.1));
                        log.line(&format!("error: {why}"));
                        (s.state, s.error) = (JobState::Failed, why);
                    }
                    Ok(o) => {
                        for f in &o.failed {
                            log.line(&format!("failed: {}: {}", f.0, f.1));
                        }
                        log.line(&format!("{} read, {} kept, {} failed; done", o.fetched, o.kept, o.failed.len()));
                        s.message = if o.failed.is_empty() {
                            format!("{} dates in {:.0} s", o.files.len(), t0.elapsed().as_secs_f64())
                        } else {
                            format!(
                                "{} dates; {} could not be read (first: {}: {}). Download again to retry them.",
                                o.files.len(),
                                o.failed.len(),
                                o.failed[0].0,
                                o.failed[0].1
                            )
                        };
                        (s.state, s.progress) = (JobState::Done, 1.0);
                    }
                    Err(e) if e == "cancelled" => {
                        log.line("cancelled");
                        s.state = JobState::Cancelled;
                    }
                    Err(e) => {
                        log.line(&format!("error: {e}"));
                        (s.state, s.error) = (JobState::Failed, e);
                    }
                }
                c.request_repaint();
            })
            .ok();
        let folder = spec.out_dir.to_string_lossy().into_owned();
        self.zeit.jobs.push(Job {
            title: title.clone(),
            tool: "climate".into(),
            cube: 0,
            output_dir: folder.clone(),
            log_path,
            started: Instant::now(),
            status: status.clone(),
            cancel,
            loaded: true, // nothing for the Zeit results to load
        });
        self.climate.last = Some(status.clone());
        // rain is drawn as bars when it is in the chart (the Series axes menu changes it)
        self.chart.looks.entry(name.clone()).or_default().bars = true;
        self.emb_downloads.push(EmbDownload {
            title: "Climate data".into(),
            status,
            started: Instant::now(),
            folder,
            name: Some(name),
            keep_active: true,
            sel: None,
            opened: false,
            ended: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_file_names_per_source() {
        let m = steps(Source::Monthly, (2019, 11), (2020, 2));
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].remote, "monthly/global/cogs/chirps-v3.0.2019.11.cog");
        assert_eq!(m[3].file, "chirps_2020-02.tif");
        assert_eq!(m[3].day, janus_cube::dates::days_from_civil(2020, 2, 1));
        let p = steps(Source::Pentad, (2020, 2), (2020, 2));
        assert_eq!(p.len(), 6);
        assert_eq!(p[5].remote, "pentads/global/cogs/chirps-v3.0.2020.02.6.cog");
        assert_eq!(p[5].file, "chirps_2020-02-26.tif");
        let d = steps(Source::Daily, (2020, 2), (2020, 3));
        assert_eq!(d.len(), 29 + 31);
        assert_eq!(d[28].remote, "daily/final/rnl/cogs/2020/chirps-v3.0.rnl.2020.02.29.cog");
        assert_eq!(steps(Source::Daily, (2021, 2), (2021, 2)).len(), 28);
        // days increase by one through the daily steps, by five through a pentad's
        assert!(d.windows(2).all(|w| w[1].day == w[0].day + 1));
    }

    #[test]
    fn file_names_carry_dates_the_series_reader_finds() {
        for (s, y, m) in [(Source::Monthly, 2020, 3), (Source::Pentad, 2020, 3), (Source::Daily, 2020, 3)] {
            for st in steps(s, (y, m), (y, m)) {
                let (t, _) = janus_cube::dates::parse_date(&st.file).expect(&st.file);
                assert_eq!((t / 86400.0) as i64, st.day, "{}", st.file);
            }
        }
        let (_, g) = janus_cube::dates::parse_date("chirps_2020-03.tif").unwrap();
        assert_eq!(g, janus_cube::TimeGranularity::Month);
    }

    #[test]
    fn month_ranges() {
        assert_eq!(steps(Source::Monthly, (2020, 5), (2020, 4)).len(), 0);
        assert_eq!(steps(Source::Monthly, (1981, 1), (2020, 12)).len(), 480);
        assert_eq!(next_month((2020, 12)), (2021, 1));
        assert_eq!(month_of(janus_cube::dates::days_from_civil(2020, 7, 31) as f64 * 86400.0), (2020, 7));
    }

    #[test]
    fn a_box_becomes_a_window_on_the_grid() {
        // the first cell is the north-west corner
        assert_eq!(chirps_window([-180.0, 59.95, -179.95, 60.0]).unwrap(), [0, 0, 1, 1]);
        // a box ending on a cell's edge does not take the next cell
        assert_eq!(chirps_window([-63.0, -10.0, -62.0, -9.0]).unwrap(), [2340, 1380, 2360, 1400]);
        // one inside a cell takes the cell; one across an edge, both
        assert_eq!(chirps_window([-62.99, -9.99, -62.98, -9.98]).unwrap(), [2340, 1399, 2341, 1400]);
        let w = chirps_window([-62.98, -9.98, -62.94, -9.94]).unwrap();
        assert_eq!((w[2] - w[0], w[3] - w[1]), (2, 2));
        // the limits: the box is cut at 60N and 60S, 180W and 180E
        assert_eq!(chirps_window([170.0, -70.0, 190.0, -50.0]).unwrap(), [7000, 2200, 7200, 2400]);
        assert_eq!(chirps_window([-200.0, 55.0, -170.0, 80.0]).unwrap(), [0, 0, 200, 100]);
        assert_eq!(chirps_window([-180.0, -60.0, 180.0, 60.0]).unwrap(), [0, 0, 7200, 2400]);
    }

    #[test]
    fn a_box_outside_the_coverage_says_so() {
        for b in [[10.0, 61.0, 20.0, 70.0], [10.0, -80.0, 20.0, -60.5], [-200.0, 0.0, -181.0, 10.0]] {
            let e = chirps_window(b).unwrap_err();
            assert!(e.contains("outside the product's coverage"), "{e}");
        }
        assert!(chirps_window([10.0, 10.0, 10.0, 20.0]).unwrap_err().contains("empty"));
        assert!(chirps_window([f64::NAN, 10.0, 11.0, 20.0]).is_err());
    }

    #[test]
    fn the_estimate_checks_the_period_and_the_box() {
        let mut s = Spec {
            source: Source::Monthly,
            bbox: [-63.0, -10.0, -62.0, -9.0],
            start: (2020, 1),
            end: (2020, 12),
            out_dir: PathBuf::new(),
        };
        let e = estimate(&s).unwrap();
        assert_eq!((e.files, e.cols, e.rows), (12, 20, 20));
        assert!((e.bytes - 12.0 * 400.0 * 4.0).abs() < 1e-6);
        s.start = (1980, 12);
        assert!(estimate(&s).unwrap_err().contains("1981"));
        s.start = (2020, 6);
        s.end = (2020, 5);
        assert!(estimate(&s).is_err());
        s.end = (2999, 1);
        assert!(estimate(&s).unwrap_err().contains("latest month"));
        s.end = (2020, 12);
        s.bbox = [0.0, 70.0, 1.0, 71.0];
        assert!(estimate(&s).unwrap_err().contains("outside"));
    }

    #[test]
    fn sources_are_found_by_their_ids() {
        for s in Source::ALL {
            assert_eq!(Source::from_id(s.id()), Some(s));
        }
        assert_eq!(Source::from_id("monthly"), Some(Source::Monthly));
        assert_eq!(Source::from_id("era5"), None);
        assert_eq!(source_path("https://h/x/", "a/b.cog"), "/vsicurl/https://h/x/a/b.cog");
        assert!(source_path("C:\\data\\fake", "a/b.cog").ends_with("b.cog"));
    }

    /// A fake month: a 40 x 40 grid of 0.05 degrees whose north-west corner is (-63.5, -8.5); a value says its pixel.
    fn write_fake(path: &Path, month: u32) {
        let drv = gdal::DriverManager::get_driver_by_name("GTiff").unwrap();
        let (w, h) = (40usize, 40usize);
        let mut ds = drv.create_with_band_type::<f32, _>(path, w, h, 1).unwrap();
        ds.set_geo_transform(&[-63.5, 0.05, 0.0, -8.5, 0.0, -0.05]).unwrap();
        ds.set_spatial_ref(&SpatialRef::from_epsg(4326).unwrap()).unwrap();
        let data: Vec<f32> = (0..w * h).map(|p| ((p % w) + 100 * (p / w)) as f32 + month as f32 * 10000.0).collect();
        let mut b = ds.rasterband(1).unwrap();
        b.set_no_data_value(Some(-9999.0)).unwrap();
        b.write((0, 0), (w, h), &mut Buffer::new((w, h), data)).unwrap();
    }

    #[test]
    fn a_download_from_a_local_folder_writes_the_box_of_every_month_and_keeps_them() {
        let dir = std::env::temp_dir().join(format!("janus-climate-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (src, out) = (dir.join("src"), dir.join("out"));
        std::fs::create_dir_all(src.join("monthly/global/cogs")).unwrap();
        // the months 1 and 3 exist, the 2nd does not
        for m in [1u32, 3] {
            write_fake(&src.join(format!("monthly/global/cogs/chirps-v3.0.2020.{m:02}.cog")), m);
        }
        set_root_override(Some(src.to_string_lossy().into_owned()));
        let spec = Spec {
            source: Source::Monthly,
            bbox: [-63.0, -10.0, -62.0, -9.0],
            start: (2020, 1),
            end: (2020, 3),
            out_dir: out.clone(),
        };
        // the estimate and the window follow the real grid, the fake files have their own: the box is read from them
        let cancel = AtomicBool::new(false);
        let r = download(&spec, &|_, _| {}, &cancel);
        // the fake grid is not the product's, so the box (20 x 20 on the real grid) is read at the fake one's pixels
        let r = r.unwrap();
        assert_eq!(r.fetched, 2, "{:?}", r.failed);
        assert_eq!(r.failed.len(), 1);
        assert!(r.failed[0].0.contains("2020-02"));
        assert_eq!(r.files.len(), 2);
        let ds = gdal::Dataset::open(&r.files[1]).unwrap();
        // west -63 is the fake grid's column 10, north -9 its row 10; 20 columns, 20 rows
        assert_eq!(ds.raster_size(), (20, 20));
        let gt = ds.geo_transform().unwrap();
        assert!((gt[0] + 63.0).abs() < 1e-9 && (gt[3] + 9.0).abs() < 1e-9 && (gt[1] - 0.05).abs() < 1e-12);
        let v = ds.rasterband(1).unwrap().read_as::<f32>((0, 0), (20, 20), (20, 20), None).unwrap();
        assert_eq!(v.data()[0], 10.0 + 100.0 * 10.0 + 30000.0);
        assert_eq!(v.data()[21], 11.0 + 100.0 * 11.0 + 30000.0);
        assert_eq!(ds.metadata_item("DATA_DOI", "").as_deref(), Some(DATA_DOI));
        assert!(out.join("ATTRIBUTION.txt").exists() && out.join("climate.json").exists());
        // the second month appears: only it is read, the others are kept
        write_fake(&src.join("monthly/global/cogs/chirps-v3.0.2020.02.cog"), 2);
        let r = download(&spec, &|_, _| {}, &cancel).unwrap();
        assert_eq!((r.fetched, r.kept, r.failed.len(), r.files.len()), (1, 2, 0, 3));
        set_root_override(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Against the real server, run by hand (`--ignored --nocapture`): one month of a small box into the folder in
    /// JANUS_CLIMATE_TEST_DIR, timed; a script compares it with what rasterio reads from the same file.
    #[test]
    #[ignore = "reads the Climate Hazards Center's server"]
    fn real_chirps_month_for_a_small_box() {
        let out = PathBuf::from(std::env::var("JANUS_CLIMATE_TEST_DIR").expect("JANUS_CLIMATE_TEST_DIR"));
        let _ = std::fs::remove_dir_all(&out);
        let spec = Spec {
            source: Source::Monthly,
            bbox: [-63.0, -10.0, -62.0, -9.0],
            start: (2020, 1),
            end: (2020, 1),
            out_dir: out,
        };
        let t = Instant::now();
        let r = download(&spec, &|_, _| {}, &AtomicBool::new(false)).unwrap();
        println!(
            "read {} month(s) in {:.2} s, window {:?}, failed {:?}",
            r.fetched,
            t.elapsed().as_secs_f64(),
            r.window,
            r.failed
        );
        assert_eq!(r.files.len(), 1);
    }

    /// ERA5 on Planetary Computer through Janus's own GDAL (hourly Zarr v2, blosc/zstd), run by hand.
    #[test]
    #[ignore = "reads Planetary Computer"]
    fn era5_zarr_opens_in_janus_gdal() {
        janus_cube::stac::sign_planetary("era5-pds", "cpdataeuwest", "era5").unwrap();
        let path = "ZARR:\"/vsiaz/era5/ERA5/2020/06/precipitation_amount_1hour_Accumulation.zarr\":/precipitation_amount_1hour_Accumulation";
        let t = Instant::now();
        let ds = gdal::Dataset::open(path).unwrap();
        println!("size {:?}, bands {}", ds.raster_size(), ds.raster_count());
        let b = ds.rasterband(1).unwrap();
        let v = b.read_as::<f32>((0, 0), (4, 4), (4, 4), None).unwrap();
        println!("first hour, 4 x 4 corner: {:?} in {:.2} s", v.data(), t.elapsed().as_secs_f64());
    }
}
